//! Persist-then-broadcast transcript storage.
//!
//! The database is the only source of truth. The broadcast channel is a hint
//! for live viewers: a lagging subscriber re-syncs from the database instead
//! of trusting the channel.

use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Clone, Debug, FromRow)]
pub struct StoredEvent {
    pub id: Uuid,
    pub session_id: Uuid,
    pub event_id: Uuid,
    pub sequence_number: i64,
    pub speaker: String,
    pub content: String,
    pub started_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct NewEvent {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub speaker: String,
    pub content: String,
    pub started_at: DateTime<Utc>,
}

pub struct PublishOutcome {
    pub event: StoredEvent,
    pub duplicate: bool,
}

const SELECT_ENTRY: &str = r#"
SELECT id, session_id, event_id, sequence_number, speaker, content,
       started_at, received_at
FROM transcript_entries
"#;

/// Insert one event, assigning the session's next sequence number.
///
/// The session row is locked (`FOR UPDATE`) so concurrent publishers get
/// distinct sequence numbers. A repeated `event_id` returns the stored row
/// unchanged — that is the whole idempotency story.
pub async fn publish(pool: &PgPool, event: NewEvent) -> Result<PublishOutcome> {
    let mut tx = pool.begin().await?;

    // Locking the session also validates it exists and has not ended.
    let session: Option<(String,)> =
        sqlx::query_as("SELECT status FROM sessions WHERE id = $1 FOR UPDATE")
            .bind(event.session_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((status,)) = session else {
        return Err(Error::NotFound);
    };
    if status == "completed" {
        return Err(Error::SessionEnded);
    }

    if let Some(stored) = fetch_by_event_id_tx(&mut tx, event.event_id).await? {
        tx.commit().await?;
        return Ok(PublishOutcome {
            event: stored,
            duplicate: true,
        });
    }

    let next: (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM transcript_entries WHERE session_id = $1",
    )
    .bind(event.session_id)
    .fetch_one(&mut *tx)
    .await?;

    let stored = sqlx::query_as::<_, StoredEvent>(
        "INSERT INTO transcript_entries
            (id, session_id, event_id, speaker, content, started_at, received_at, sequence_number)
         VALUES ($1, $2, $3, $4, $5, $6, now(), $7)
         RETURNING id, session_id, event_id, sequence_number, speaker, content, started_at, received_at",
    )
    .bind(Uuid::new_v4())
    .bind(event.session_id)
    .bind(event.event_id)
    .bind(&event.speaker)
    .bind(&event.content)
    .bind(event.started_at)
    .bind(next.0)
    .fetch_one(&mut *tx)
    .await?;

    // First committed line starts the meeting without a separate device call.
    sqlx::query(
        "UPDATE sessions
         SET status = CASE WHEN status = 'created' THEN 'active' ELSE status END,
             started_at = COALESCE(started_at, now())
         WHERE id = $1",
    )
    .bind(event.session_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(PublishOutcome {
        event: stored,
        duplicate: false,
    })
}

async fn fetch_by_event_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
) -> Result<Option<StoredEvent>> {
    let sql = format!("{SELECT_ENTRY} WHERE event_id = $1");
    Ok(sqlx::query_as::<_, StoredEvent>(&sql)
        .bind(event_id)
        .fetch_optional(&mut **tx)
        .await?)
}

pub async fn by_event_id(pool: &PgPool, event_id: Uuid) -> Result<Option<StoredEvent>> {
    let sql = format!("{SELECT_ENTRY} WHERE event_id = $1");
    Ok(sqlx::query_as::<_, StoredEvent>(&sql)
        .bind(event_id)
        .fetch_optional(pool)
        .await?)
}

/// Entries after `from_sequence`, oldest first.
pub async fn after(
    pool: &PgPool,
    session_id: Uuid,
    from_sequence: i64,
    limit: i64,
) -> Result<Vec<StoredEvent>> {
    let sql = format!(
        "{SELECT_ENTRY} WHERE session_id = $1 AND sequence_number > $2
         ORDER BY sequence_number ASC LIMIT $3"
    );
    Ok(sqlx::query_as::<_, StoredEvent>(&sql)
        .bind(session_id)
        .bind(from_sequence.max(0))
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

pub async fn latest_sequence(pool: &PgPool, session_id: Uuid) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(sequence_number), 0) FROM transcript_entries WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// The newest `limit` entries, returned oldest first (prompt order).
pub async fn recent(pool: &PgPool, session_id: Uuid, limit: i64) -> Result<Vec<StoredEvent>> {
    let sql = format!(
        "SELECT * FROM ({SELECT_ENTRY} WHERE session_id = $1
         ORDER BY sequence_number DESC LIMIT $2) newest
         ORDER BY newest.sequence_number ASC"
    );
    Ok(sqlx::query_as::<_, StoredEvent>(&sql)
        .bind(session_id)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

/// Full-text matches for a question, excluding the recent window already in
/// the prompt. Cheap stand-in for embeddings at MVP scale.
pub async fn search(
    pool: &PgPool,
    session_id: Uuid,
    query: &str,
    before_sequence: i64,
    limit: i64,
) -> Result<Vec<StoredEvent>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "{SELECT_ENTRY} WHERE session_id = $1 AND sequence_number < $2
           AND to_tsvector('english', content) @@ plainto_tsquery('english', $3)
         ORDER BY sequence_number DESC LIMIT $4"
    );
    let mut rows = sqlx::query_as::<_, StoredEvent>(&sql)
        .bind(session_id)
        .bind(before_sequence)
        .bind(query)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.reverse();
    Ok(rows)
}

pub async fn count(pool: &PgPool, session_id: Uuid) -> Result<i64> {
    let row: (i64,) =
        sqlx::query_as("SELECT count(*) FROM transcript_entries WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

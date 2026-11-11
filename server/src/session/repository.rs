//! SQL for sessions. Every query is runtime-checked so the crate builds
//! without a live database (CI and laptops have different ones).

use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Clone, Debug, FromRow)]
pub struct SessionRow {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub transcript_count: i64,
    pub ai_help: bool,
    /// What the session is about, as prompt context (may be empty).
    pub ai_context: String,
    /// Extra answer guidance (may be empty).
    pub ai_instructions: String,
}

const SELECT_SESSION: &str = r#"
SELECT s.id, s.title, s.status, s.started_at, s.ended_at, s.created_at,
       (SELECT count(*) FROM transcript_entries t WHERE t.session_id = s.id) AS transcript_count,
       s.ai_help, s.ai_context, s.ai_instructions
FROM sessions s
"#;

pub async fn create(
    pool: &PgPool,
    title: &str,
    token_hash: &str,
    ai_help: bool,
    ai_context: &str,
    ai_instructions: &str,
) -> Result<SessionRow> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, title, status, token_hash, ai_help, ai_context, ai_instructions)
         VALUES ($1, $2, 'created', $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(title)
    .bind(token_hash)
    .bind(ai_help)
    .bind(ai_context)
    .bind(ai_instructions)
    .execute(pool)
    .await?;
    get(pool, id).await?.ok_or(Error::NotFound)
}

/// Replace the session's brief. Both fields are replaced, so an empty string
/// clears it; the phone sends what the user sees in the box.
pub async fn update_guidance(
    pool: &PgPool,
    id: Uuid,
    ai_context: &str,
    ai_instructions: &str,
) -> Result<SessionRow> {
    let result = sqlx::query(
        "UPDATE sessions SET ai_context = $2, ai_instructions = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(ai_context)
    .bind(ai_instructions)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    get(pool, id).await?.ok_or(Error::NotFound)
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<SessionRow>> {
    let sql = format!("{SELECT_SESSION} WHERE s.id = $1");
    Ok(sqlx::query_as::<_, SessionRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn list(pool: &PgPool, limit: i64, active_only: bool) -> Result<Vec<SessionRow>> {
    let sql = format!(
        "{SELECT_SESSION} {} ORDER BY s.created_at DESC LIMIT $1",
        if active_only {
            "WHERE s.status IN ('created', 'active')"
        } else {
            ""
        }
    );
    Ok(sqlx::query_as::<_, SessionRow>(&sql)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

/// Mark a session active. Idempotent; `started_at` is set once.
pub async fn start(pool: &PgPool, id: Uuid) -> Result<SessionRow> {
    sqlx::query(
        "UPDATE sessions
         SET status = CASE WHEN status = 'created' THEN 'active' ELSE status END,
             started_at = COALESCE(started_at, now())
         WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    get(pool, id).await?.ok_or(Error::NotFound)
}

pub async fn end(pool: &PgPool, id: Uuid) -> Result<SessionRow> {
    let affected = sqlx::query(
        "UPDATE sessions
         SET status = 'completed', ended_at = COALESCE(ended_at, now())
         WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(Error::NotFound);
    }
    get(pool, id).await?.ok_or(Error::NotFound)
}

pub async fn delete_completed_before(pool: &PgPool, days: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM sessions
         WHERE status = 'completed'
           AND COALESCE(ended_at, created_at) < now() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

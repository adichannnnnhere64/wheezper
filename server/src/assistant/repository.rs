//! Assistant conversation storage (optional audit trail; never transcript
//! mutations).
//!
//! Answers are anchored to the transcript row they answer (`answered_sequence`),
//! which is what lets the phone show them under that row again after a restart.

use crate::error::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Clone, Debug, FromRow)]
pub struct AssistantMessageRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub question: String,
    pub answer: String,
    pub provider: String,
    pub model: String,
    pub answered_sequence: Option<i64>,
    pub created_at: DateTime<Utc>,
}

pub async fn save(
    pool: &PgPool,
    session_id: Uuid,
    question: &str,
    answer: &str,
    provider: &str,
    model: &str,
    answered_sequence: Option<i64>,
) -> Result<AssistantMessageRow> {
    Ok(sqlx::query_as::<_, AssistantMessageRow>(
        "INSERT INTO assistant_messages
             (id, session_id, question, answer, provider, model, answered_sequence)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id, session_id, question, answer, provider, model, answered_sequence, created_at",
    )
    .bind(Uuid::new_v4())
    .bind(session_id)
    .bind(question)
    .bind(answer)
    .bind(provider)
    .bind(model)
    .bind(answered_sequence)
    .fetch_one(pool)
    .await?)
}

pub async fn count(pool: &PgPool, session_id: Uuid) -> Result<i64> {
    let row: (i64,) =
        sqlx::query_as("SELECT count(*) FROM assistant_messages WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

/// The answers anchored to transcript rows, one per row (the latest wins).
///
/// The client re-renders these under their rows on open, so a reconnected phone
/// shows the answers the server already has instead of an empty transcript.
pub async fn answers_for_rows(pool: &PgPool, session_id: Uuid) -> Result<Vec<AssistantMessageRow>> {
    Ok(sqlx::query_as::<_, AssistantMessageRow>(
        "SELECT DISTINCT ON (answered_sequence)
             id, session_id, question, answer, provider, model, answered_sequence, created_at
         FROM assistant_messages
         WHERE session_id = $1 AND answered_sequence IS NOT NULL
         ORDER BY answered_sequence, created_at DESC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?)
}

/// Earlier answers of this meeting, oldest first, for the prompt.
///
/// The assistant is expected to stay consistent with what it already said and
/// not repeat it, so the previous question/answer pairs are part of its context.
pub async fn previous_answers(
    pool: &PgPool,
    session_id: Uuid,
    limit: i64,
) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT question, answer FROM (
             SELECT question, answer, created_at
             FROM assistant_messages
             WHERE session_id = $1 AND answer <> ''
             ORDER BY created_at DESC
             LIMIT $2
         ) recent
         ORDER BY created_at",
    )
    .bind(session_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

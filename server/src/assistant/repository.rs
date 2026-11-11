//! Assistant conversation storage (optional audit trail; never transcript
//! mutations).

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
    pub created_at: DateTime<Utc>,
}

pub async fn save(
    pool: &PgPool,
    session_id: Uuid,
    question: &str,
    answer: &str,
    provider: &str,
    model: &str,
) -> Result<AssistantMessageRow> {
    Ok(sqlx::query_as::<_, AssistantMessageRow>(
        "INSERT INTO assistant_messages (id, session_id, question, answer, provider, model)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, session_id, question, answer, provider, model, created_at",
    )
    .bind(Uuid::new_v4())
    .bind(session_id)
    .bind(question)
    .bind(answer)
    .bind(provider)
    .bind(model)
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

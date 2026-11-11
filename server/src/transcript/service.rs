//! Transcript ingestion + read use-cases.

use super::broadcaster::Broadcaster;
use super::repository::{self, NewEvent, PublishOutcome, StoredEvent};
use crate::error::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// PostgreSQL unique-violation SQLSTATE.
const UNIQUE_VIOLATION: &str = "23505";

#[derive(Clone)]
pub struct TranscriptService {
    pool: PgPool,
    broadcaster: Broadcaster,
}

pub struct SyncPage {
    pub events: Vec<StoredEvent>,
    pub latest_sequence: i64,
    pub has_more: bool,
}

impl TranscriptService {
    pub fn new(pool: PgPool, broadcaster: Broadcaster) -> Self {
        Self { pool, broadcaster }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn broadcaster(&self) -> &Broadcaster {
        &self.broadcaster
    }

    /// Persist before acknowledging, then broadcast. Duplicate `event_id`s are
    /// acknowledged with the original sequence number.
    pub async fn publish(&self, event: NewEvent) -> Result<PublishOutcome> {
        let session_id = event.session_id;
        let mut attempt = 0;
        let outcome = loop {
            attempt += 1;
            match repository::publish(&self.pool, event.clone()).await {
                Ok(outcome) => break outcome,
                Err(crate::error::Error::Database(err))
                    if is_unique_violation(&err) && attempt < 5 =>
                {
                    // Two publishers raced for the same sequence number, or
                    // the same event_id was inserted concurrently. Re-read the
                    // idempotency key first, then retry the transaction.
                    if let Some(stored) =
                        repository::by_event_id(&self.pool, event.event_id).await?
                    {
                        break PublishOutcome {
                            event: stored,
                            duplicate: true,
                        };
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10 * attempt)).await;
                }
                Err(err) => return Err(err),
            }
        };
        if !outcome.duplicate {
            self.broadcaster.publish(outcome.event.clone());
        }
        tracing::debug!(
            session = %session_id,
            sequence = outcome.event.sequence_number,
            duplicate = outcome.duplicate,
            "transcript persisted"
        );
        Ok(outcome)
    }

    pub async fn sync(&self, session_id: Uuid, from_sequence: i64, limit: i64) -> Result<SyncPage> {
        let limit = if limit <= 0 { 500 } else { limit.min(2000) };
        // +1 row tells us whether another page exists without a second query.
        let mut events =
            repository::after(&self.pool, session_id, from_sequence, limit + 1).await?;
        let has_more = events.len() as i64 > limit;
        if has_more {
            events.truncate(limit as usize);
        }
        let latest_sequence = repository::latest_sequence(&self.pool, session_id).await?;
        Ok(SyncPage {
            events,
            latest_sequence,
            has_more,
        })
    }

    pub async fn recent(&self, session_id: Uuid, limit: i64) -> Result<Vec<StoredEvent>> {
        repository::recent(&self.pool, session_id, limit).await
    }

    pub async fn search(
        &self,
        session_id: Uuid,
        query: &str,
        before_sequence: i64,
        limit: i64,
    ) -> Result<Vec<StoredEvent>> {
        repository::search(&self.pool, session_id, query, before_sequence, limit).await
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some(UNIQUE_VIOLATION))
}

//! Session use-cases: create, list, start, end, retention.

use super::repository::{self, SessionRow};
use crate::auth::{hash_token, new_token};
use crate::error::{Error, Result};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct SessionService {
    pool: PgPool,
}

impl SessionService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a session and mint its scoped publish token.
    pub async fn create(&self, title: &str, ai_help: bool) -> Result<(SessionRow, String)> {
        let title = title.trim();
        let title = if title.is_empty() {
            "Untitled meeting"
        } else {
            title
        };
        let token = new_token();
        let row = repository::create(&self.pool, title, &hash_token(&token), ai_help).await?;
        Ok((row, token))
    }

    pub async fn get(&self, id: Uuid) -> Result<SessionRow> {
        repository::get(&self.pool, id)
            .await?
            .ok_or(Error::NotFound)
    }

    pub async fn list(&self, limit: i64, active_only: bool) -> Result<Vec<SessionRow>> {
        let limit = if limit <= 0 { 50 } else { limit.min(200) };
        repository::list(&self.pool, limit, active_only).await
    }

    pub async fn start(&self, id: Uuid) -> Result<SessionRow> {
        repository::start(&self.pool, id).await
    }

    pub async fn end(&self, id: Uuid) -> Result<SessionRow> {
        // Dropping the publish credential is part of ending the session: a
        // stale laptop must not keep writing into a completed meeting.
        repository::end(&self.pool, id).await?;
        sqlx::query("UPDATE sessions SET token_hash = $2 WHERE id = $1")
            .bind(id)
            .bind(hash_token(&new_token()))
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }

    pub async fn purge_old_completed(&self, days: i64) -> Result<u64> {
        if days <= 0 {
            return Ok(0);
        }
        repository::delete_completed_before(&self.pool, days).await
    }
}

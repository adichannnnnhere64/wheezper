//! Device pairing and request authentication.
//!
//! Two kinds of bearer credential exist:
//!   * **device tokens** — short-lived, issued by `PairDevice` in exchange for
//!     the operator's pairing code. They grant the paired device access to
//!     sessions (create/list/read) but not to a session's publish channel.
//!   * **session tokens** — returned once by `CreateSession`. They can publish
//!     committed transcript events for exactly one session.
//!
//! Only sha256 hashes are stored, so a database dump does not hand out live
//! credentials.

use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::time::Duration;
use tonic::metadata::MetadataMap;
use uuid::Uuid;

/// A 64-hex-character random token (two v4 UUIDs = 244 random bits).
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.trim().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compare two secrets without leaking the matching-prefix length.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Device { id: Uuid, name: String },
    Session { session_id: Uuid },
}

impl Principal {
    pub fn device_or_err(&self) -> Result<(&Uuid, &str)> {
        match self {
            Principal::Device { id, name } => Ok((id, name.as_str())),
            Principal::Session { .. } => Err(Error::Unauthorized),
        }
    }
}

#[derive(Clone)]
pub struct Authenticator {
    pool: PgPool,
}

impl Authenticator {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Exchange the operator's pairing code for a short-lived device token.
    pub async fn pair_device(
        &self,
        pairing_code: &str,
        expected_code: &str,
        device_name: &str,
        ttl: Duration,
    ) -> Result<(String, DateTime<Utc>)> {
        if !constant_time_eq(pairing_code.trim(), expected_code) {
            return Err(Error::Unauthorized);
        }
        let name = device_name.trim();
        let name = if name.is_empty() {
            "device"
        } else {
            &name[..name.len().min(64)]
        };
        let token = new_token();
        let expires_at = Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_default();
        sqlx::query(
            "INSERT INTO devices (id, name, token_hash, expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(hash_token(&token))
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok((token, expires_at))
    }

    pub async fn authenticate(&self, metadata: &MetadataMap) -> Result<Principal> {
        let token = bearer(metadata)?;
        let hash = hash_token(token);

        let device: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, name FROM devices WHERE token_hash = $1 AND expires_at > now()",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((id, name)) = device {
            return Ok(Principal::Device { id, name });
        }

        let session: Option<(Uuid, String)> =
            sqlx::query_as("SELECT id, status FROM sessions WHERE token_hash = $1")
                .bind(&hash)
                .fetch_optional(&self.pool)
                .await?;
        if let Some((session_id, status)) = session {
            if status == "completed" {
                return Err(Error::SessionEnded);
            }
            return Ok(Principal::Session { session_id });
        }

        Err(Error::Unauthorized)
    }

    /// Device-only RPCs (session listing, viewing, the assistant).
    pub async fn authenticate_device(&self, metadata: &MetadataMap) -> Result<(Uuid, String)> {
        let principal = self.authenticate(metadata).await?;
        let (id, name) = principal.device_or_err()?;
        Ok((*id, name.to_string()))
    }

    /// Publishing needs either a device token or the session's own token; a
    /// session token may only publish into its own session.
    pub async fn authorize_publish(
        &self,
        metadata: &MetadataMap,
        session_id: Uuid,
    ) -> Result<Principal> {
        let principal = self.authenticate(metadata).await?;
        if let Principal::Session { session_id: scoped } = &principal
            && *scoped != session_id
        {
            return Err(Error::Unauthorized);
        }
        Ok(principal)
    }

    pub async fn purge_expired_devices(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM devices WHERE expires_at <= now()")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

fn bearer(metadata: &MetadataMap) -> Result<&str> {
    let raw = metadata
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(Error::Unauthorized)?;
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(Error::Unauthorized)?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable_and_tokens_unique() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        assert_eq!(hash_token(&token), hash_token(&format!("  {token} ")));
        assert_eq!(hash_token(&token).len(), 64);
        assert_ne!(hash_token(&token), hash_token(&new_token()));
    }

    #[test]
    fn constant_time_compare_is_correct() {
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
    }
}

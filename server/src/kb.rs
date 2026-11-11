//! Personal knowledge base (`family-kb` on hermes) as assistant context, and
//! the place a finished meeting is pushed to.
//!
//! Off unless `WHEEZPER_KB_URL` and `WHEEZPER_KB_TOKEN` are set, for two
//! reasons: a transcript is private, and the KB's own README says it must stay
//! loopback-only. Only the configured collections are searched — the KB also
//! holds `family` and `personal`, and those must not be prefilled into a model
//! prompt just because a question happens to match.
//!
//! Failures are never fatal: a KB outage means an answer without the user's
//! background, and an unsent note, not a failed meeting.

use crate::error::{Error, Result};
use std::time::Duration;

#[derive(Clone)]
pub struct Kb {
    url: String,
    token: String,
    /// Collections searched for answers; `WHEEZPER_KB_COLLECTIONS`.
    collections: Vec<String>,
    /// Collection a finished meeting is written to.
    meetings: String,
    client: reqwest::Client,
}

impl Kb {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("WHEEZPER_KB_URL").ok()?.trim_end_matches('/').to_string();
        let token = std::env::var("WHEEZPER_KB_TOKEN").ok()?;
        if url.is_empty() || token.is_empty() {
            return None;
        }
        let collections = std::env::var("WHEEZPER_KB_COLLECTIONS")
            .unwrap_or_else(|_| "interviews,learning".into())
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let meetings = std::env::var("WHEEZPER_KB_MEETINGS")
            .unwrap_or_else(|_| "meetings".into())
            .trim()
            .to_ascii_lowercase();
        Some(Self {
            url,
            token,
            collections,
            meetings,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(15))
                .build()
                .ok()?,
        })
    }

    pub fn meetings_collection(&self) -> &str {
        &self.meetings
    }

    /// Retrieval only (no KB-side answer generation: the meeting assistant does
    /// its own, with the transcript as the primary source).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        let mut hits = Vec::new();
        for collection in &self.collections {
            let body = serde_json::json!({
                "query": query,
                "limit": limit,
                "collection": collection,
            });
            let response = self
                .client
                .post(format!("{}/api/search", self.url))
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::provider(format!("knowledge base unreachable: {e}")))?;
            if !response.status().is_success() {
                return Err(Error::provider(format!(
                    "knowledge base search returned {}",
                    response.status()
                )));
            }
            let rows: Vec<serde_json::Value> = response
                .json()
                .await
                .map_err(|e| Error::provider(format!("bad knowledge base response: {e}")))?;
            for row in rows {
                let text = row.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
                if text.is_empty() {
                    continue;
                }
                let title = row.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let collection = row.get("collection").and_then(|v| v.as_str()).unwrap_or("");
                hits.push(format!("[{collection}] {title}: {text}"));
            }
        }
        Ok(hits)
    }

    /// Push one finished meeting as a KB document.
    pub async fn add_note(&self, title: &str, content: &str, source: &str) -> Result<()> {
        let body = serde_json::json!({
            "title": title,
            "content": content,
            "collection": self.meetings,
            "source": source,
        });
        let response = self
            .client
            .post(format!("{}/api/documents", self.url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::provider(format!("knowledge base unreachable: {e}")))?;
        if !response.status().is_success() {
            return Err(Error::provider(format!(
                "knowledge base rejected the note: {}",
                response.status()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_off_without_url_and_token() {
        // The default deployment must not talk to any KB.
        assert!(Kb::from_env().is_none() || std::env::var("WHEEZPER_KB_TOKEN").is_ok());
    }
}

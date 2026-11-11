//! LLM providers for the assistant.
//!
//! The backend owns model access so the Android app never holds an API key.
//! MVP ships Ollama (local or `ollama.com`); an OpenAI-compatible provider can
//! be added behind the same `generate` / `stream` shape.

use crate::error::{Error, Result};
use futures_util::StreamExt;
use std::time::Duration;

/// The only remote endpoint a client may select. A client-supplied URL would
/// turn the assistant RPC into an SSRF probe, so it is deliberately fixed.
pub const OLLAMA_CLOUD_URL: &str = "https://ollama.com/api";

/// Both URL shapes are accepted: a bare base (`http://localhost:11434`) and the
/// `ollama.com/api` form users copy from the cloud docs. Keeping `/api` would
/// make every request `…/api/api/chat`, which the server answers with 404
/// (`path "/api/api/chat" not found`).
pub fn api_base(url: &str) -> String {
    let url = url.trim_end_matches('/');
    url.strip_suffix("/api").unwrap_or(url).to_string()
}

#[derive(Clone)]
pub struct Ollama {
    /// Base URL without the `/api/...` suffix.
    pub url: String,
    pub model: String,
    /// Used for this request only; never logged or persisted.
    pub api_key: Option<String>,
    client: reqwest::Client,
}

pub struct Prompt {
    pub system: String,
    pub user: String,
    /// Ollama `think`: `false` unless the user picked a reasoning level. Cloud
    /// reasoning models otherwise spend the whole budget in `message.thinking`.
    pub think: Think,
}

/// Reasoning budget. Values map 1:1 onto Ollama's `think` field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Think {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl Think {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "off" | "false" | "none" => Ok(Self::Off),
            "low" => Ok(Self::Low),
            "medium" | "mid" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(Error::invalid(format!(
                "thinking must be off, low, medium or high (got {other})"
            ))),
        }
    }

    fn json(self) -> serde_json::Value {
        match self {
            Self::Off => serde_json::Value::Bool(false),
            Self::Low => "low".into(),
            Self::Medium => "medium".into(),
            Self::High => "high".into(),
        }
    }
}

/// Token usage reported by the provider on the last stream message.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub output_tokens: i64,
}

impl Ollama {
    pub fn new(url: &str, model: &str, api_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // meetings are long; generation may take a while on a local box
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            url: api_base(url),
            model: model.to_string(),
            api_key,
            client,
        }
    }

    /// Full `/api/chat` endpoint, for error messages and tests.
    pub fn chat_url(&self) -> String {
        format!("{}/api/chat", self.url)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn request(&self, prompt: &Prompt, stream: bool) -> reqwest::RequestBuilder {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": prompt.system},
                {"role": "user", "content": prompt.user},
            ],
            "stream": stream,
            "think": prompt.think.json(),
            "options": {"temperature": 0.3, "num_predict": 320},
        });
        let mut req = self
            .client
            .post(self.chat_url())
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    pub async fn generate(&self, prompt: &Prompt) -> Result<(String, Usage)> {
        let response = self
            .request(prompt, false)
            .send()
            .await
            .map_err(|e| Error::provider(format!("ollama at {} unreachable: {e}", self.url)))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(Error::provider(format!(
                "ollama returned {status}: {}",
                detail.chars().take(300).collect::<String>()
            )));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::provider(format!("bad ollama response: {e}")))?;
        if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
            return Err(Error::provider(err));
        }
        let content = body
            .pointer("/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(Error::provider(
                "ollama returned an empty answer (is `think` supported by the model?)",
            ));
        }
        Ok((content, usage(&body)))
    }

    /// NDJSON streaming (`stream: true`) chunked into content deltas, ending
    /// with one `Usage` part (empty text) when the provider reports it.
    pub fn stream(
        &self,
        prompt: Prompt,
    ) -> impl futures_util::Stream<Item = Result<StreamPart>> + Send + 'static {
        let this = self.clone();
        async_stream::try_stream! {
            let response = this
                .request(&prompt, true)
                .send()
                .await
                .map_err(|e| Error::provider(format!("ollama at {} unreachable: {e}", this.url)))?;
            let status = response.status();
            if !status.is_success() {
                let detail = response.text().await.unwrap_or_default();
                Err(Error::provider(format!(
                    "ollama returned {status}: {}",
                    detail.chars().take(300).collect::<String>()
                )))?;
                return;
            }

            let mut body = response.bytes_stream();
            let mut buffer: Vec<u8> = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|e| Error::provider(format!("ollama stream: {e}")))?;
                buffer.extend_from_slice(&chunk);
                while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=newline).collect();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let value: serde_json::Value = serde_json::from_str(line)
                        .map_err(|e| Error::provider(format!("bad ollama stream line: {e}")))?;
                    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                        Err(Error::provider(err))?;
                    }
                    if let Some(delta) = value.pointer("/message/content").and_then(|v| v.as_str())
                        && !delta.is_empty()
                    {
                        yield StreamPart { text: delta.to_string(), usage: None };
                    }
                    if value.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                        yield StreamPart { text: String::new(), usage: Some(usage(&value)) };
                        return;
                    }
                }
            }
        }
    }
}

/// One streamed piece: a content delta, or the final usage report.
pub struct StreamPart {
    pub text: String,
    pub usage: Option<Usage>,
}

fn usage(value: &serde_json::Value) -> Usage {
    let get = |key: &str| value.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    Usage {
        prompt_tokens: get("prompt_eval_count"),
        output_tokens: get("eval_count"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_api_suffix_is_not_doubled() {
        let cloud = Ollama::new(OLLAMA_CLOUD_URL, "gpt-oss:20b", Some("k".into()));
        assert_eq!(cloud.chat_url(), "https://ollama.com/api/chat");
    }

    #[test]
    fn local_base_url_is_kept() {
        let local = Ollama::new("http://localhost:11434/", "gemma3:1b", None);
        assert_eq!(local.chat_url(), "http://localhost:11434/api/chat");
    }

    #[test]
    fn thinking_levels_parse_and_reject_typos() {
        assert_eq!(Think::parse("").unwrap(), Think::Off);
        assert_eq!(Think::parse("off").unwrap(), Think::Off);
        assert_eq!(Think::parse("MEDIUM").unwrap(), Think::Medium);
        assert_eq!(Think::parse("high").unwrap(), Think::High);
        assert!(Think::parse("maximum").is_err());
        assert_eq!(Think::High.json(), serde_json::json!("high"));
        assert_eq!(Think::Off.json(), serde_json::json!(false));
    }

    #[test]
    fn operator_cloud_url_is_normalised() {
        // WHEEZPER_ASSISTANT_URL=https://ollama.com/api is a documented value.
        assert_eq!(api_base("https://ollama.com/api/"), "https://ollama.com");
        assert_eq!(api_base("https://ollama.com"), "https://ollama.com");
    }
}

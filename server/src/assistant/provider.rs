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
pub const OLLAMA_CLOUD_URL: &str = "https://ollama.com";

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
    /// Sampling temperature for this call.
    pub temperature: f32,
    /// Output cap. Interview answers are short; meeting answers may be longer.
    pub num_predict: i32,
}

impl Prompt {
    pub fn new(system: String, user: String) -> Self {
        Self {
            system,
            user,
            temperature: 0.3,
            num_predict: 320,
        }
    }

    /// Interview answers: near-deterministic and short, so the model stops
    /// riffing and cannot pad the answer with invented experience.
    pub fn interview(system: String, user: String) -> Self {
        Self {
            system,
            user,
            temperature: 0.1,
            num_predict: 220,
        }
    }
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
            url: url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            client,
        }
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
            // cloud reasoning models otherwise spend the whole budget in
            // `message.thinking` and return empty content
            "think": false,
            "options": {
                "temperature": prompt.temperature,
                "num_predict": prompt.num_predict
            },
        });
        let mut req = self
            .client
            .post(format!("{}/api/chat", self.url))
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    pub async fn generate(&self, prompt: &Prompt) -> Result<String> {
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
        Ok(content)
    }

    /// NDJSON streaming (`stream: true`) chunked into content deltas.
    pub fn stream(
        &self,
        prompt: Prompt,
    ) -> impl futures_util::Stream<Item = Result<String>> + Send + 'static {
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
                        yield delta.to_string();
                    }
                    if value.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                        return;
                    }
                }
            }
        }
    }
}

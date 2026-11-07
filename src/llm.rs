//! Ollama integration: post-process the session transcript with a local LLM
//! (any model the Ollama server hosts — gemma3, llama3.2, qwen, ...).
//!
//! Never fatal: if the server is down or the model is missing, wheezper
//! warns on stderr and the raw transcript stays untouched.

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::time::Duration;

pub struct Ollama {
    pub url: String,
    pub model: String,
    agent: ureq::Agent,
}

impl Ollama {
    pub fn new(url: &str, model: &str) -> Self {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(300)))
            .build();
        Self {
            url: url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Is the server reachable and the model available?
    pub fn check(&self) -> Result<()> {
        let resp = self
            .agent
            .get(format!("{}/api/tags", self.url))
            .call()
            .map_err(|e| anyhow!("Ollama server unreachable at {}: {e}", self.url))?;
        let mut body = String::new();
        resp.into_parts()
            .1
            .into_reader()
            .read_to_string(&mut body)
            .context("reading Ollama response")?;
        // /api/tags: {"models":[{"name":"gemma3:1b",...}]}
        let names: Vec<String> = body
            .split("\"name\":\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap_or_default().to_string())
            .collect();
        let wanted = self.model.split(':').next().unwrap_or_default();
        if !names.iter().any(|n| n.starts_with(wanted)) {
            return Err(anyhow!(
                "model '{}' not found in Ollama (available: {}). Try: ollama pull {}",
                self.model,
                if names.is_empty() { "none".to_string() } else { names.join(", ") },
                self.model
            ));
        }
        Ok(())
    }

    /// One-shot generation via /api/chat. Returns the model's reply text.
    pub fn generate(&self, system: &str, prompt: &str, max_tokens: u32) -> Result<String> {
        let body = format!(
            "{{\"model\":{}, \"messages\":[{{\"role\":\"system\",\"content\":{}}},{{\"role\":\"user\",\"content\":{}}}], \"stream\":false, \"options\":{{\"num_predict\":{}, \"temperature\":0.3}}}}",
            json_str(&self.model),
            json_str(system),
            json_str(prompt),
            max_tokens,
        );
        let resp = self
            .agent
            .post(format!("{}/api/chat", self.url))
            .header("Content-Type", "application/json")
            .send(body.as_str())
            .map_err(|e| anyhow!("Ollama generate failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("Ollama generate: HTTP {}", resp.status()));
        }
        let mut body = String::new();
        resp.into_parts()
            .1
            .into_reader()
            .read_to_string(&mut body)
            .context("reading Ollama response")?;
        // {"message":{"role":"assistant","content":"..."},"done":true}
        extract_json_string(&body, "content")
            .ok_or_else(|| anyhow!("could not parse Ollama response: {}", body.chars().take(200).collect::<String>()))
    }

    /// Summarize a meeting transcript (one-paragraph + key points + actions).
    pub fn summarize(&self, transcript: &str) -> Result<String> {
        let system = "You summarize meeting transcripts. Reply in markdown with: a one-paragraph summary, then a '## Key points' bullet list, then a '## Action items' bullet list with owners if mentioned. Be concise and factual; never invent facts.";
        let prompt = format!("Transcript:\n{transcript}");
        let out = self.generate(system, &prompt, 1024)?;
        Ok(out.trim().to_string())
    }

    /// Polish one transcribed utterance: punctuation, casing, filler removal.
    /// Keeps the wording otherwise intact; falls back to the raw text if the
    /// model goes off-script.
    pub fn polish(&self, text: &str) -> Result<String> {
        let system = "You fix raw speech-to-text output: add punctuation and capitalization, remove filler words (um, uh) and stutters. Keep the wording otherwise EXACTLY the same. Reply with the corrected sentence only, nothing else.";
        let prompt = format!("Raw text: {text}");
        let out = self.generate(system, &prompt, 256)?;
        let out = out.trim().trim_matches('"').to_string();
        if out.is_empty() || out.len() > text.len() * 3 {
            return Ok(text.to_string());
        }
        Ok(out)
    }
}

/// Minimal JSON string value extractor for flat Ollama responses
/// (no serde dependency needed for this).
fn extract_json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = body.find(&needle)? + needle.len();
    let mut out = String::new();
    let mut chars = body[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => out.push(other), // e.g. \" -> "
            },
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::extract_json_string;

    #[test]
    fn extracts_response_field() {
        assert_eq!(
            extract_json_string(r#"{"message":{"role":"assistant","content":"hello \"world\"\nline"},"done":true}"#, "content"),
            Some("hello \"world\"\nline".to_string())
        );
        assert_eq!(extract_json_string(r#"{"error":"x"}"#, "content"), None);
    }
}

//! Optional webhook sink: POST each transcribed utterance as JSON.
//! Non-fatal by design — a webhook outage must never stop transcription.

use std::time::Duration;

pub struct Webhook {
    url: String,
    agent: ureq::Agent,
}

impl Webhook {
    pub fn new(url: &str) -> Self {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build();
        Self { url: url.to_string(), agent: ureq::Agent::new_with_config(config) }
    }

    /// Fire and (mostly) forget. Errors go to stderr.
    pub fn post(&self, channel: &str, text: &str) {
        let body = format!(
            "{{\"channel\":{}, \"text\":{}, \"time\":\"{}\"}}",
            json_str(channel),
            json_str(text),
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z")
        );
        if let Err(e) = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .send(body.as_str())
        {
            eprintln!("wheezper: webhook failed: {e}");
        }
    }
}

/// Minimal JSON string escaping (no serde dependency needed for this shape).
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
    use super::json_str;

    #[test]
    fn escapes_json() {
        assert_eq!(json_str("hi"), "\"hi\"");
        assert_eq!(json_str("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_str("line\n"), "\"line\\n\"");
    }
}

//! LLM post-processing of the transcript. Two interchangeable backends:
//!
//! - [`Ollama`]: talk to an Ollama server (localhost or remote) with any
//!   model it hosts.
//! - [`Builtin`]: llama.cpp in an embedded helper process — no server, no
//!   installs. whisper.cpp and llama.cpp both bundle their own ggml, so they
//!   cannot live in one executable; wheezper.exe carries `wheezper-llm`
//!   inside itself and extracts it to the cache dir on first use. The GGUF
//!   model (default Google gemma3-1b) auto-downloads like the whisper models.
//!
//! Both implement the same [`Llm`] interface (summary + polish) and are
//! never fatal: on failure the raw transcript stays untouched.

use crate::models;
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

// ---------------------------------------------------------------------------
// unified interface
// ---------------------------------------------------------------------------

pub enum Llm {
    Server(Ollama),
    Builtin(Builtin),
}

impl Llm {
    pub fn describe(&self) -> String {
        match self {
            Llm::Server(o) => format!("{} @ {}", o.model, o.url),
            Llm::Builtin(b) => format!("{} (built-in llama.cpp)", b.model_name),
        }
    }

    pub fn generate(&self, system: &str, prompt: &str, max_tokens: u32) -> Result<String> {
        match self {
            Llm::Server(o) => o.generate(system, prompt, max_tokens),
            Llm::Builtin(b) => b.generate(system, prompt, max_tokens),
        }
    }

    /// Summarize a meeting transcript (one-paragraph + key points + actions).
    pub fn summarize(&self, transcript: &str) -> Result<String> {
        let system = "You summarize meeting transcripts. Reply in markdown with: a one-paragraph summary, then a '## Key points' bullet list, then a '## Action items' bullet list with owners if mentioned. Be concise and factual; never invent facts.";
        let prompt = format!("Transcript:\n{transcript}");
        let out = self.generate(system, &prompt, 512)?;
        Ok(out.trim().to_string())
    }

    /// Polish one transcribed utterance: punctuation, casing, filler removal.
    /// Keeps the wording otherwise intact; falls back to raw text if the
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

// ---------------------------------------------------------------------------
// Ollama server backend
// ---------------------------------------------------------------------------

pub struct Ollama {
    pub url: String,
    pub model: String,
    /// Set when OLLAMA_API_KEY is present: cloud inference (ollama.com)
    /// requires "Authorization: Bearer <key>" on every request.
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl Ollama {
    pub fn new(url: &str, model: &str) -> Self {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(300)))
            .build();
        // accept both a plain base ("http://localhost:11434", used for
        // /api/chat etc.) and the cloud form ending in "/api"
        let mut url = url.trim_end_matches('/').to_string();
        if url.ends_with("/api") {
            url.truncate(url.len() - "/api".len());
        }
        Self {
            url,
            model: model.to_string(),
            api_key: std::env::var("OLLAMA_API_KEY").ok().filter(|k| !k.is_empty()),
            agent: ureq::Agent::new_with_config(config),
        }
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.api_key {
            Some(key) => req.header("Authorization", &format!("Bearer {key}")),
            None => req,
        }
    }

    /// Is the server reachable and the model available?
    pub fn check(&self) -> Result<()> {
        let resp = self
            .auth(self.agent.get(format!("{}/api/tags", self.url)))
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
            .auth(
                self.agent
                    .post(format!("{}/api/chat", self.url))
                    .header("Content-Type", "application/json"),
            )
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
}

// ---------------------------------------------------------------------------
// built-in backend: llama.cpp in an embedded helper process
// ---------------------------------------------------------------------------

pub struct Builtin {
    model_name: String,
    model_path: PathBuf,
    threads: i32,
    worker: std::cell::RefCell<Worker>,
}

impl Builtin {
    /// `name_or_path`: a known model name (gemma3-1b), an Ollama-style tag
    /// (gemma3:1b), or a path to an existing .gguf. The model is downloaded
    /// on first use; the worker process is embedded in wheezper.exe.
    pub fn new(name_or_path: &str, threads: i32) -> Result<Self> {
        let model_path = models::resolve_llm(Some(name_or_path))
            .with_context(|| format!("resolving built-in LLM model '{name_or_path}'"))?;
        let mut worker = Worker::spawn()?;
        worker.warm_up(&model_path, threads)?; // load the model once, fail early
        Ok(Self { model_name: name_or_path.to_string(), model_path, threads, worker: std::cell::RefCell::new(worker) })
    }

    pub fn generate(&self, system: &str, prompt: &str, max_tokens: u32) -> Result<String> {
        let req = format!(
            "{{\"model\":{}, \"system\":{}, \"prompt\":{}, \"max_tokens\":{}, \"threads\":{}}}",
            json_str(&self.model_path.display().to_string()),
            json_str(system),
            json_str(prompt),
            max_tokens,
            self.threads,
        );
        self.worker.borrow_mut().request(&req)
    }
}

/// A running `wheezper-llm` helper: JSON lines in, JSON lines out.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn() -> Result<Self> {
        let path = worker_binary_path()?;
        let mut child = Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning {}", path.display()))?;
        let stdin = child.stdin.take().context("worker stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("worker stdout")?);
        Ok(Self { child, stdin, stdout })
    }

    /// Send one request line, read one response line.
    fn request(&mut self, req_json: &str) -> Result<String> {
        writeln!(self.stdin, "{req_json}").context("writing to LLM worker")?;
        self.stdin.flush().ok();
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .context("reading from LLM worker")?;
        if line.trim().is_empty() {
            return Err(anyhow!("LLM worker closed unexpectedly"));
        }
        let text = extract_json_string(line.trim(), "text")
            .ok_or_else(|| anyhow!("bad LLM worker response: {}", line.chars().take(200).collect::<String>()))?;
        if let Some(err) = extract_json_string(line.trim(), "error") {
            return Err(anyhow!("LLM worker: {err}"));
        }
        Ok(text)
    }

    /// Load the model (empty prompt probe) so failures surface at startup.
    fn warm_up(&mut self, model_path: &std::path::Path, threads: i32) -> Result<()> {
        let req = format!(
            "{{\"model\":{}, \"system\":\"\", \"prompt\":\"\", \"max_tokens\":1, \"threads\":{}}}",
            json_str(&model_path.display().to_string()),
            threads,
        );
        self.request(&req)?;
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where to find the wheezper-llm worker binary:
/// 1. $WHEEZPER_LLM_WORKER (dev builds)
/// 2. next to the wheezper executable (same target dir / same zip)
/// 3. embedded copy, extracted once into the model cache dir
fn worker_binary_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("WHEEZPER_LLM_WORKER")
        && PathBuf::from(&p).exists()
    {
        return Ok(PathBuf::from(p));
    }

    let exe = std::env::current_exe().context("locating wheezper executable")?;
    let exe_dir = exe.parent().context("executable has no parent dir")?.to_path_buf();
    let sibling = if cfg!(windows) { "wheezper-llm.exe" } else { "wheezper-llm" };
    let sibling_path = exe_dir.join(sibling);
    if sibling_path.exists() {
        return Ok(sibling_path);
    }

    // embedded copy (build.rs puts it in OUT_DIR; empty when not provided)
    const EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wheezper-llm.bin"));
    if EMBEDDED.is_empty() {
        return Err(anyhow!(
            "wheezper-llm worker not found (not embedded, not next to {}). \
             Use an Ollama server instead: --ollama http://<server>:11434",
            exe_dir.display()
        ));
    }
    let dir = models::cache_dir()?.join("bin");
    std::fs::create_dir_all(&dir).ok();
    let dest = dir.join(sibling);
    if !dest.exists() || std::fs::metadata(&dest).map(|m| m.len() as usize).ok() != Some(EMBEDDED.len()) {
        std::fs::write(&dest, EMBEDDED).with_context(|| format!("extracting {}", dest.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).ok();
        }
    }
    Ok(dest)
}

// ---------------------------------------------------------------------------

/// Minimal JSON string value extractor for flat JSON responses
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

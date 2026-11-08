//! Model resolution + first-run download.
//!
//! The only thing wheezper needs besides its own binary is a whisper ggml
//! model. `--model` accepts either a path to an existing .bin or a model name
//! (`base`, `tiny`, ...); named models are downloaded once from
//! huggingface.co into a cache dir using rustls (no system TLS/CA deps).

use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct ModelSpec {
    pub name: &'static str,
    pub file: &'static str,
    pub approx_mb: u64,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec { name: "tiny", file: "ggml-tiny-q5_1.bin", approx_mb: 32 },
    ModelSpec { name: "base", file: "ggml-base-q5_1.bin", approx_mb: 57 },
    ModelSpec { name: "small", file: "ggml-small-q5_1.bin", approx_mb: 181 },
    ModelSpec { name: "medium", file: "ggml-medium-q5_0.bin", approx_mb: 539 },
    ModelSpec { name: "turbo", file: "ggml-large-v3-turbo-q5_0.bin", approx_mb: 574 },
];

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// The Silero VAD model used to gate segments before whisper (see vad.rs).
const VAD_MODEL_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";
const VAD_MODEL_FILE: &str = "ggml-silero-v5.1.2.bin";

/// Resolve the Silero VAD model (~0.9 MB, one-time download).
pub fn resolve_vad() -> Result<PathBuf> {
    let dest = cache_dir()?.join(VAD_MODEL_FILE);
    if !dest.exists() {
        download_to(VAD_MODEL_URL, &dest, 1)?;
    }
    Ok(dest)
}

/// Built-in LLM models (GGUF, run via hardwired llama.cpp — no Ollama needed).
pub struct LlmModelSpec {
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub approx_mb: u64,
}

pub const LLM_MODELS: &[LlmModelSpec] = &[LlmModelSpec {
    name: "gemma3-1b",
    file: "gemma-3-1b-it-Q4_K_M.gguf",
    url: "https://huggingface.co/ggml-org/gemma-3-1b-it-GGUF/resolve/main/gemma-3-1b-it-Q4_K_M.gguf",
    approx_mb: 779,
}];

/// Resolve a built-in LLM model name/path into a local GGUF, downloading on
/// first use. Accepts an existing path, a known name (gemma3-1b), or an
/// Ollama-style tag (gemma3:1b -> gemma3-1b).
pub fn resolve_llm(arg: Option<&str>) -> Result<PathBuf> {
    let raw = arg.unwrap_or("gemma3-1b");
    let arg = raw.replace(':', "-");
    let path = Path::new(&arg);
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    let spec = LLM_MODELS.iter().find(|m| m.name == arg).ok_or_else(|| {
        let names: Vec<&str> = LLM_MODELS.iter().map(|m| m.name).collect();
        anyhow!(
            "LLM model '{raw}' is neither an existing file nor a known model name ({})",
            names.join(", ")
        )
    })?;
    let dest = cache_dir()?.join(spec.file);
    if !dest.exists() {
        download_to(spec.url, &dest, spec.approx_mb)?;
    }
    Ok(dest)
}

/// Resolve `--model` (or default `base`) into a local model path,
/// downloading on first use. Accepts:
///   - a path to an existing .bin file (used as-is)
///   - a model name from MODELS
///   - an exact ggml file name from the huggingface repo
pub fn resolve(arg: Option<&str>) -> Result<PathBuf> {
    let arg = arg.unwrap_or("base");
    let path = Path::new(arg);
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    let spec = lookup(arg).ok_or_else(|| {
        let names: Vec<&str> = MODELS.iter().map(|m| m.name).collect();
        anyhow!(
            "model '{arg}' is neither an existing file nor a known model name ({}). \
             Or point --model at a .bin file.",
            names.join(", ")
        )
    })?;
    let dest = cache_dir()?.join(&spec.file);
    if !dest.exists() {
        download_to(&format!("{HF_BASE}/{}", spec.file), &dest, spec.approx_mb)?;
    }
    Ok(dest)
}

/// Owned download spec (a repo file name + size hint).
struct DownloadSpec {
    file: String,
    approx_mb: u64,
}

fn lookup(name: &str) -> Option<DownloadSpec> {
    let n = name.to_ascii_lowercase();
    if let Some(m) = MODELS.iter().find(|m| m.name == n) {
        return Some(DownloadSpec { file: m.file.to_string(), approx_mb: m.approx_mb });
    }
    if n.starts_with("ggml-") && n.ends_with(".bin") {
        return Some(DownloadSpec { file: n, approx_mb: 0 });
    }
    None
}

/// Platform-appropriate cache dir (never needs installing anything):
/// Windows: %APPDATA%\wheezper | Unix: $XDG_DATA_HOME/wheezper or ~/.local/share/wheezper
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("WHEEZPER_MODEL_DIR") {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return Ok(PathBuf::from(appdata).join("wheezper"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(home).join(".wheezper"));
        }
        bail!("could not determine model cache dir (set WHEEZPER_MODEL_DIR)");
    }
    #[cfg(not(target_os = "windows"))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| anyhow!("could not determine model cache dir (set WHEEZPER_MODEL_DIR)"))?;
        Ok(data.join("wheezper"))
    }
}

/// Download `url` to `dest` (temp file + rename) with progress on stderr.
fn download_to(url: &str, dest: &Path, approx_mb: u64) -> Result<()> {
    eprintln!("wheezper: downloading (~{} MB)…", approx_mb);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating model dir {}", parent.display()))?;
    }
    let tmp = dest.with_extension("part");

    let resp = ureq::get(url).call().context("starting model download")?;
    if !resp.status().is_success() {
        bail!("download failed: HTTP {}", resp.status());
    }
    let total: u64 = resp
        .headers()
        .iter()
        .find(|(name, _)| name.as_str() == "content-length")
        .and_then(|(_, v)| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(approx_mb * 1_000_000);

    let mut reader = resp.into_parts().1.into_reader();
    let mut file = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let start = Instant::now();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        let total_mb = total / 1_000_000;
        let done_mb = done / 1_000_000;
        let speed = done as f64 / start.elapsed().as_secs_f64().max(0.1) / 1_000_000.0;
        eprint!("\rwheezper: {done_mb}/{total_mb} MB ({speed:.1} MB/s)…  ");
    }
    eprintln!();
    file.sync_all().ok();
    drop(file);
    std::fs::rename(&tmp, dest).with_context(|| format!("finalising {}", dest.display()))?;
    eprintln!("wheezper: model saved to {}", dest.display());
    Ok(())
}

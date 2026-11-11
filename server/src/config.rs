//! Server configuration from the environment (12-factor; no config file).

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind: SocketAddr,
    /// Shared secret exchanged for a short-lived device token (PairDevice).
    pub pairing_code: String,
    /// Where a generated pairing code is kept, when it did not come from the
    /// environment. `None` = `WHEEZPER_PAIRING_CODE` was set.
    pub pairing_code_file: Option<PathBuf>,
    pub device_ttl: Duration,
    /// Completed sessions older than this are deleted. 0 keeps them forever.
    pub retention_days: i64,
    pub assistant_url: String,
    pub assistant_model: String,
    pub assistant_api_key: Option<String>,
    /// Interview context policy: `auto` (short for a local model, whole meeting
    /// for a cloud one), `full` (always the whole meeting) or `short`.
    /// Prefill, not decoding, is the latency on a small local model.
    pub assistant_context: String,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let database_url = env("WHEEZPER_DATABASE_URL")
            .or_else(|| env("DATABASE_URL"))
            .context("WHEEZPER_DATABASE_URL is required (postgres://user:pass@host/db)")?;
        let bind = env("WHEEZPER_BIND")
            .unwrap_or_else(|| "0.0.0.0:50051".into())
            .parse()
            .context("WHEEZPER_BIND must be host:port, e.g. 0.0.0.0:50051")?;
        // The pairing code is a shared secret. It may be given, or generated once
        // and kept next to the server's state: a code that changed on every
        // restart would pair nothing, and forcing every operator to invent one
        // is how weak codes get chosen.
        let (pairing_code, pairing_code_file) = resolve_pairing_code()?;
        let device_ttl_hours = parse_env("WHEEZPER_DEVICE_TTL_HOURS", 24 * 7_u64)?;
        let retention_days = parse_env("WHEEZPER_RETENTION_DAYS", 0_i64)?;
        let assistant_url = env("WHEEZPER_ASSISTANT_URL")
            .unwrap_or_else(|| "http://localhost:11434".into())
            .trim_end_matches('/')
            .to_string();
        let assistant_model = env("WHEEZPER_ASSISTANT_MODEL").unwrap_or_else(|| "gemma3:1b".into());
        let assistant_api_key = env("OLLAMA_API_KEY").or_else(|| env("WHEEZPER_ASSISTANT_API_KEY"));
        let assistant_context = env("WHEEZPER_ASSISTANT_CONTEXT")
            .unwrap_or_else(|| "auto".into())
            .to_ascii_lowercase();
        if !matches!(assistant_context.as_str(), "auto" | "full" | "short") {
            bail!("WHEEZPER_ASSISTANT_CONTEXT must be auto, full or short");
        }
        let (tls_cert, tls_key) = match (env("WHEEZPER_TLS_CERT"), env("WHEEZPER_TLS_KEY")) {
            (Some(cert), Some(key)) => (Some(PathBuf::from(cert)), Some(PathBuf::from(key))),
            (None, None) => (None, None),
            _ => bail!("WHEEZPER_TLS_CERT and WHEEZPER_TLS_KEY must be set together"),
        };
        Ok(Self {
            database_url,
            bind,
            pairing_code,
            pairing_code_file,
            device_ttl: Duration::from_secs(device_ttl_hours * 3600),
            retention_days,
            assistant_url,
            assistant_model,
            assistant_api_key,
            assistant_context,
            tls_cert,
            tls_key,
        })
    }

    /// True when the assistant would send transcript text off this machine.
    pub fn assistant_is_remote(&self) -> bool {
        let host = self.assistant_url.to_ascii_lowercase();
        !(host.contains("localhost") || host.contains("127.0.0.1") || host.contains("[::1]"))
    }

    pub fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some() && self.tls_key.is_some()
    }
}

/// `WHEEZPER_PAIRING_CODE`, else a generated code persisted in a 0600 file.
fn resolve_pairing_code() -> Result<(String, Option<PathBuf>)> {
    if let Some(code) = env("WHEEZPER_PAIRING_CODE") {
        validate_pairing_code(&code)?;
        return Ok((code, None));
    }
    let path = env("WHEEZPER_PAIRING_CODE_FILE")
        .map(PathBuf::from)
        .or_else(default_pairing_code_path)
        .context("no WHEEZPER_PAIRING_CODE and no state dir for a generated one")?;
    let code = load_or_generate_pairing_code(&path)?;
    Ok((code, Some(path)))
}

fn default_pairing_code_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|dir| PathBuf::from(dir).join("wheezper").join("pairing-code"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .map(|dir| dir.join("wheezper").join("pairing-code"))
    }
}

/// Read the code, or make one and store it (`0600`). The file is the only copy:
/// it is a secret, so it is never logged.
pub(crate) fn load_or_generate_pairing_code(path: &Path) -> Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            validate_pairing_code(&existing)?;
            return Ok(existing);
        }
    }
    let code = generate_pairing_code();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file =
        options.open(path).with_context(|| format!("writing {}", path.display()))?;
    file.write_all(format!("{code}\n").as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all().ok();
    Ok(code)
}

/// XXXX-XXXX-XXXX-XXXX from an unambiguous alphabet (no 0/O, 1/I/l): the code is
/// typed on a phone keyboard.
pub(crate) fn generate_pairing_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    // uuid v4 bytes: already a dependency, uniformly random (122 bits), no extra
    // crate for a 16-character code.
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let code: String = bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect();
    code.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

fn validate_pairing_code(code: &str) -> Result<()> {
    if code.trim().len() < 12 {
        bail!(
            "WHEEZPER_PAIRING_CODE must be at least 12 characters (try `openssl rand -base64 24`)"
        );
    }
    Ok(())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn parse_env<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match env(name) {
        Some(raw) => match raw.parse() {
            Ok(value) => Ok(value),
            Err(_) => bail!("{name} is not a valid number: {raw}"),
        },
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_code_is_phone_typable() {
        let code = generate_pairing_code();
        assert_eq!(code.len(), 19, "XXXX-XXXX-XXXX-XXXX: {code}");
        assert!(
            code.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-'),
            "{code}"
        );
        // 0/O and 1/I/l are the classic misreads when typing a code on a phone.
        for confusing in ['0', 'O', '1', 'I', 'L'] {
            assert!(!code.contains(confusing), "{code} contains {confusing}");
        }
        assert_ne!(generate_pairing_code(), generate_pairing_code());
        validate_pairing_code(&code).unwrap();
    }

    #[test]
    fn a_generated_code_survives_restarts() {
        let dir = std::env::temp_dir().join(format!("wheezper-pairing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state").join("pairing-code");

        let first = load_or_generate_pairing_code(&path).expect("generate");
        let second = load_or_generate_pairing_code(&path).expect("reread");
        assert_eq!(
            first, second,
            "a regenerated code would unpair every restart"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            first,
            "the file is what the next start reads"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the pairing code is a secret");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_short_configured_code_is_rejected() {
        assert!(validate_pairing_code("short").is_err());
        assert!(validate_pairing_code("").is_err());
        assert!(validate_pairing_code("a-perfectly-fine-code").is_ok());
    }
}

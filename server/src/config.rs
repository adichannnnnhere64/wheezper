//! Server configuration from the environment (12-factor; no config file).

use anyhow::{Context, Result, bail};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind: SocketAddr,
    /// Shared secret exchanged for a short-lived device token (PairDevice).
    pub pairing_code: String,
    pub device_ttl: Duration,
    /// Completed sessions older than this are deleted. 0 keeps them forever.
    pub retention_days: i64,
    pub assistant_url: String,
    pub assistant_model: String,
    pub assistant_api_key: Option<String>,
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
        let pairing_code = env("WHEEZPER_PAIRING_CODE")
            .context("WHEEZPER_PAIRING_CODE is required (shared secret used to pair devices)")?;
        if pairing_code.len() < 12 {
            bail!(
                "WHEEZPER_PAIRING_CODE must be at least 12 characters (try `openssl rand -base64 24`)"
            );
        }
        let device_ttl_hours = parse_env("WHEEZPER_DEVICE_TTL_HOURS", 24 * 7_u64)?;
        let retention_days = parse_env("WHEEZPER_RETENTION_DAYS", 0_i64)?;
        let assistant_url = env("WHEEZPER_ASSISTANT_URL")
            .unwrap_or_else(|| "http://localhost:11434".into())
            .trim_end_matches('/')
            .to_string();
        let assistant_model = env("WHEEZPER_ASSISTANT_MODEL").unwrap_or_else(|| "gemma3:1b".into());
        let assistant_api_key = env("OLLAMA_API_KEY").or_else(|| env("WHEEZPER_ASSISTANT_API_KEY"));
        let (tls_cert, tls_key) = match (env("WHEEZPER_TLS_CERT"), env("WHEEZPER_TLS_KEY")) {
            (Some(cert), Some(key)) => (Some(PathBuf::from(cert)), Some(PathBuf::from(key))),
            (None, None) => (None, None),
            _ => bail!("WHEEZPER_TLS_CERT and WHEEZPER_TLS_KEY must be set together"),
        };
        Ok(Self {
            database_url,
            bind,
            pairing_code,
            device_ttl: Duration::from_secs(device_ttl_hours * 3600),
            retention_days,
            assistant_url,
            assistant_model,
            assistant_api_key,
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

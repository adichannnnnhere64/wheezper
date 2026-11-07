//! Config file loading: `--config PATH`, else `./wheezper.toml`, else
//! `~/.config/wheezper/config.toml` (Linux) / `%APPDATA%\wheezper\config.toml`
//! (Windows). Keys mirror the CLI flags; command-line flags always win.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml::Table;

pub struct Config {
    pub path: PathBuf,
    pub table: Table,
}

pub fn load(explicit: Option<&Path>) -> Result<Option<Config>> {
    if let Some(p) = explicit {
        return Ok(Some(Config { path: p.to_path_buf(), table: parse(p)? }));
    }
    for candidate in candidates() {
        if candidate.exists() {
            let table = parse(&candidate)?;
            return Ok(Some(Config { path: candidate, table }));
        }
    }
    Ok(None)
}

fn candidates() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("wheezper.toml")];
    if let Some(dir) = config_dir() {
        v.push(dir.join("config.toml"));
    }
    v
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("wheezper"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("wheezper"))
    }
}

fn parse(path: &Path) -> Result<Table> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    raw.parse::<Table>().with_context(|| format!("parsing {} (TOML)", path.display()))
}

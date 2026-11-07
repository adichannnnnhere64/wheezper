//! Notepad sink: timestamped markdown file, appended per line and flushed
//! immediately (safe to tail, safe against Ctrl-C).

use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Notepad {
    file: File,
    path: PathBuf,
}

impl Notepad {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating notes dir {}", parent.display()))?;
        }
        let existed = path.exists();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening notepad {}", path.display()))?;
        let mut np = Self { file, path: path.to_path_buf() };
        if !existed {
            let now = Local::now();
            let header = format!("# Meeting — {}\n\n", now.format("%Y-%m-%d %H:%M"));
            np.file.write_all(header.as_bytes())?;
            np.file.flush()?;
        }
        Ok(np)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one utterance, optionally attributed to a channel ("me"/"them").
    /// Empty text is ignored.
    pub fn append(&mut self, channel: Option<&str>, text: &str) -> Result<bool> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(false);
        }

        let ts = Local::now().format("%H:%M:%S");
        match channel {
            Some(ch) => writeln!(self.file, "- [{ts}] **{ch}:** {text}")?,
            None => writeln!(self.file, "- [{ts}] {text}")?,
        }
        self.file.flush()?;
        Ok(true)
    }
}

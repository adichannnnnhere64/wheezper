//! Notepad sink: timestamped markdown file, appended per line and flushed
//! immediately (safe to tail, safe against Ctrl-C).

use anyhow::{Context, Result, bail};
use chrono::Local;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub struct Notepad {
    file: File,
    path: PathBuf,
    line_counts: HashMap<String, usize>,
}

#[derive(Clone)]
pub struct AnnotationKey {
    line: String,
    occurrence: usize,
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
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening notepad {}", path.display()))?;
        let mut old_content = String::new();
        file.read_to_string(&mut old_content)
            .with_context(|| format!("reading notepad {}", path.display()))?;
        let mut line_counts = HashMap::new();
        for line in old_content.lines().filter(|line| line.starts_with("- [")) {
            *line_counts.entry(line.to_string()).or_insert(0) += 1;
        }
        let mut np = Self {
            file,
            path: path.to_path_buf(),
            line_counts,
        };
        if !existed {
            let now = Local::now();
            let header = format!("# Meeting — {}\n\n", now.format("%Y-%m-%d %H:%M"));
            np.file.write_all(header.as_bytes())?;
            np.file.flush()?;
        }
        Ok(np)
    }

    /// Append a multi-line block (e.g. an LLM summary) under a heading.
    pub fn append_block(&mut self, heading: &str, text: &str) -> Result<()> {
        let ts = Local::now().format("%Y-%m-%d %H:%M");
        self.file.seek(SeekFrom::End(0))?;
        writeln!(self.file, "\n## {heading} — {ts}\n\n{text}\n")?;
        self.file.flush()?;
        Ok(())
    }

    /// Full notepad content so far (for LLM summarization).
    pub fn read_content(&self) -> Result<String> {
        std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))
    }

    /// Append an utterance and return a stable locator for its later annotation.
    pub fn append_with_key(
        &mut self,
        channel: Option<&str>,
        text: &str,
    ) -> Result<Option<AnnotationKey>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }

        let ts = Local::now().format("%H:%M:%S");
        let line = match channel {
            Some(ch) => format!("- [{ts}] **{ch}:** {text}"),
            None => format!("- [{ts}] {text}"),
        };
        let occurrence = *self.line_counts.get(&line).unwrap_or(&0);
        self.file.seek(SeekFrom::End(0))?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        self.line_counts.insert(line.clone(), occurrence + 1);
        Ok(Some(AnnotationKey { line, occurrence }))
    }

    /// Insert a completed async annotation directly below its transcript line.
    pub fn append_annotation(&mut self, target: &AnnotationKey, annotation: &str) -> Result<()> {
        let annotation = annotation.replace(['\r', '\n'], " ");
        let mut content = String::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_string(&mut content)?;

        let mut updated = String::with_capacity(content.len() + annotation.len() + 1);
        let mut seen = 0usize;
        let mut offset = 0usize;
        let mut inserted = false;
        for chunk in content.split_inclusive('\n') {
            offset += chunk.len();
            let line = chunk
                .strip_suffix('\n')
                .unwrap_or(chunk)
                .strip_suffix('\r')
                .unwrap_or_else(|| chunk.strip_suffix('\n').unwrap_or(chunk));
            if line == target.line {
                if seen == target.occurrence {
                    updated.push_str(chunk);
                    if !chunk.ends_with('\n') {
                        updated.push('\n');
                    }
                    updated.push_str(&annotation);
                    updated.push('\n');
                    updated.push_str(&content[offset..]);
                    inserted = true;
                    break;
                }
                seen += 1;
            }
            updated.push_str(chunk);
        }
        if !inserted {
            bail!(
                "transcript line for annotation is no longer in {}",
                self.path.display()
            );
        }
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(updated.as_bytes())?;
        self.file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Notepad;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn async_annotations_are_inserted_under_their_exact_duplicate_line() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("wheezper-notepad-{id}.md"));
        let mut notes = Notepad::open(&path).unwrap();
        let first = notes
            .append_with_key(Some("them"), "we need approval tomorrow")
            .unwrap()
            .unwrap();
        let second = notes
            .append_with_key(Some("them"), "we need approval tomorrow")
            .unwrap()
            .unwrap();
        notes
            .append_with_key(Some("me"), "I will send the updated draft")
            .unwrap();
        notes
            .append_annotation(&second, "  ↳ 🤖 them / test: ask for an owner")
            .unwrap();
        notes
            .append_annotation(&first, "  ↳ 🤖 them / test: confirm the date")
            .unwrap();
        let content = notes.read_content().unwrap();
        let lines: Vec<_> = content.lines().collect();
        let transcript: Vec<_> = lines
            .iter()
            .filter(|line| line.starts_with("- ["))
            .copied()
            .collect();
        assert_eq!(transcript.len(), 3);
        let first_idx = lines.iter().position(|line| *line == first.line).unwrap();
        let second_idx = lines.iter().rposition(|line| *line == second.line).unwrap();
        assert!(lines[first_idx + 1].contains("confirm the date"));
        assert!(lines[second_idx + 1].contains("ask for an owner"));
        drop(notes);
        std::fs::remove_file(path).unwrap();
    }
}

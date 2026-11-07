//! Incremental streaming transcription.
//!
//! The segment-then-transcribe pipeline waits for silence (or a 10 s
//! force-cut) before committing anything, and its live previews re-guess the
//! same audio again and again. This module replaces both for live capture:
//! a sliding window of recent audio is re-transcribed back-to-back (self-
//! paced by inference speed), and words are COMMITTED exactly once — the
//! moment the same leading words show up in two consecutive hypotheses.
//!
//! Committed text is append-only: a word is never revised or re-guessed.
//! Line breaks happen on sentence punctuation, on a word cap, or after a
//! couple of seconds of silence on the channel.

use crate::capture::TARGET_RATE;
use crate::dsp::Resampler;
use crate::transcribe::Transcriber;
use crate::vad::Vad;
use anyhow::Result;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Hypothesis context: audio the decoder sees each pass.
const WINDOW_S: f32 = 8.0;
/// New audio required between inference passes (self-pacing floor).
const NEW_AUDIO_S: f32 = 0.7;
/// Don't bother running whisper below this much audio.
const MIN_WINDOW_S: f32 = 1.0;
/// Hard cap on the buffer; oldest audio slides out first.
const BUF_CAP_S: f32 = 10.0;
/// Flush a line after this many words even without punctuation.
const LINE_FLUSH_WORDS: usize = 24;
/// Flush a line this long after its channel went quiet.
const IDLE_FLUSH: Duration = Duration::from_millis(2000);

pub struct StreamJob {
    pub chan: usize,
    pub samples: Vec<f32>,
    pub prompt: Option<String>,
}

pub struct StreamResult {
    pub chan: usize,
    pub text: String,
}

/// Spawn the shared inference worker (owns the whisper model + VAD).
/// Windows in, hypotheses out; one window per channel in flight at a time.
pub fn spawn_worker(
    model: &Path,
    language: &str,
    threads: i32,
    use_vad: bool,
) -> Result<(mpsc::Sender<StreamJob>, mpsc::Receiver<StreamResult>)> {
    let (tx, rx) = mpsc::channel::<StreamJob>();
    let (out_tx, out_rx) = mpsc::channel::<StreamResult>();
    let (model, language) = (model.to_path_buf(), language.to_string());
    std::thread::Builder::new()
        .name("stream-worker".into())
        .spawn(move || {
            let mut tr = match Transcriber::new(&model, &language, threads) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("wheezper: stream worker failed to start: {e:#}");
                    return;
                }
            };
            let mut vader = if use_vad { Vad::new(1).ok() } else { None };
            while let Ok(job) = rx.recv() {
                let vad_blocked = match &mut vader {
                    Some(v) => !v.has_speech(&job.samples, 0.05),
                    None => false,
                };
                let text = if vad_blocked {
                    String::new()
                } else {
                    tr.transcribe_greedy(&job.samples, job.prompt.as_deref()).unwrap_or_default()
                };
                if out_tx.send(StreamResult { chan: job.chan, text }).is_err() {
                    return;
                }
            }
        })?;
    Ok((tx, out_rx))
}

/// Per-channel streaming state (main-loop side).
pub struct StreamChannel {
    pub label: Option<&'static str>,
    resampler: Resampler,
    scratch: Vec<f32>,
    buf: Vec<f32>,
    /// Committed text of the currently open line (append-only).
    line: String,
    /// Previous hypothesis, for the two-pass stability rule.
    prev_hyp: Option<String>,
    unprocessed: usize,
    in_flight: bool,
    last_audio: Instant,
    /// true once the utterance tail has been confirmed by an idle re-pass
    confirmed_idle: bool,
    line_opened: Option<Instant>,
    closed: bool,
}

impl StreamChannel {
    pub fn new(label: Option<&'static str>, in_rate: u32) -> Self {
        Self {
            label,
            resampler: Resampler::new(in_rate, TARGET_RATE),
            scratch: Vec::new(),
            buf: Vec::new(),
            line: String::new(),
            prev_hyp: None,
            unprocessed: 0,
            in_flight: false,
            last_audio: Instant::now(),
            confirmed_idle: true,
            line_opened: None,
            closed: false,
        }
    }

    /// Feed a mono chunk at the capture rate.
    pub fn push(&mut self, chunk: &[f32]) {
        self.scratch.clear();
        self.resampler.push_slice(chunk, &mut self.scratch);
        self.buf.extend_from_slice(&self.scratch);
        self.unprocessed += self.scratch.len();
        // silence from an idle monitor must not look like fresh speech: only
        // audible chunks count as activity (≈ -45 dBFS energy floor)
        let energy = self.scratch.iter().map(|x| x * x).sum::<f32>() / self.scratch.len().max(1) as f32;
        if energy > 0.000_005_6 {
            self.last_audio = Instant::now();
            self.confirmed_idle = false;
        }
        let cap = (BUF_CAP_S * TARGET_RATE as f32) as usize;
        if self.buf.len() > cap {
            let drop_n = self.buf.len() - cap;
            self.buf.drain(..drop_n);
        }
    }

    /// Send a window to the worker when there is enough new audio. When the
    /// channel goes quiet mid-line, one extra re-pass confirms the utterance
    /// tail (its last words lack the right-context a later pass would give).
    pub fn maybe_send(&mut self, idx: usize, tx: &mpsc::Sender<StreamJob>, line_prompt: &str) {
        if self.in_flight || self.closed {
            return;
        }
        let need = (NEW_AUDIO_S * TARGET_RATE as f32) as usize;
        let min = (MIN_WINDOW_S * TARGET_RATE as f32) as usize;
        let idle_confirm = self.line_open()
            && self.last_audio.elapsed() > Duration::from_millis(500)
            && !self.confirmed_idle;
        if (self.unprocessed < need && !idle_confirm) || self.buf.len() < min {
            return;
        }
        let win_len = ((WINDOW_S * TARGET_RATE as f32) as usize).min(self.buf.len());
        let start = self.buf.len() - win_len;
        let samples = self.buf[start..].to_vec();
        let prompt = (!line_prompt.is_empty()).then(|| line_prompt.to_string());
        let _ = tx.send(StreamJob { chan: idx, samples, prompt });
        self.in_flight = true;
        self.unprocessed = 0;
        if idle_confirm {
            self.confirmed_idle = true;
        }
    }

    /// A hypothesis arrived: commit newly stable words (exactly once).
    /// Returns the delta text appended to the line.
    pub fn on_result(&mut self, text: &str) -> String {
        self.in_flight = false;
        let hyp: Vec<&str> = text.split_whitespace().collect();
        if hyp.is_empty() {
            return String::new();
        }
        let line_words = self.line.split_whitespace().collect::<Vec<_>>();
        // longest suffix of the line matching a prefix of hyp
        let suffix = longest_suffix_prefix(&line_words, &hyp);
        let delta_new: Vec<&str> = hyp[suffix.min(hyp.len())..].to_vec();
        // stability: the same new words must appear in the previous hypothesis
        let stable = match &self.prev_hyp {
            Some(prev) => {
                let prev_words: Vec<&str> = prev.split_whitespace().collect();
                let prev_suffix = longest_suffix_prefix(&line_words, &prev_words);
                let delta_prev: Vec<&str> =
                    prev_words[prev_suffix.min(prev_words.len())..].to_vec();
                common_prefix_len(&delta_prev, &delta_new)
            }
            None => 0,
        };
        self.prev_hyp = Some(text.to_string());
        if stable == 0 {
            return String::new();
        }
        let delta: Vec<&str> = delta_new[..stable].to_vec();
        for w in &delta {
            self.line.push_str(w);
            self.line.push(' ');
        }
        if self.line_opened.is_none() {
            self.line_opened = Some(Instant::now());
        }
        delta.join(" ")
    }

    /// Final pass at shutdown: commit the whole current hypothesis unverified.
    pub fn finish(&mut self) -> String {
        if self.closed {
            return String::new();
        }
        self.closed = true;
        let mut delta = String::new();
        if let Some(hyp) = self.prev_hyp.take() {
            let hyp_words: Vec<&str> = hyp.split_whitespace().collect();
            let line_words = self.line.split_whitespace().collect::<Vec<_>>();
            let suffix = longest_suffix_prefix(&line_words, &hyp_words);
            let fresh = hyp_words[suffix.min(hyp_words.len())..].to_vec();
            for w in fresh {
                self.line.push_str(w);
                self.line.push_str(" ");
                delta.push_str(w);
                delta.push(' ');
            }
        }
        delta
    }

    /// Is the line complete (sentence end / word cap / channel idle)?
    pub fn line_ready(&mut self) -> bool {
        let line = self.line.trim();
        if line.is_empty() {
            return false;
        }
        if line.ends_with(['.', '!', '?']) || line.ends_with('…') {
            return true;
        }
        if line.split_whitespace().count() >= LINE_FLUSH_WORDS {
            return true;
        }
        self.last_audio.elapsed() > IDLE_FLUSH && !self.in_flight
    }

    /// Take the completed line, clearing state for the next one.
    pub fn take_line(&mut self) -> Option<String> {
        let line = self.line.trim().to_string();
        if line.is_empty() {
            return None;
        }
        self.line.clear();
        self.prev_hyp = None;
        self.line_opened = None;
        Some(line)
    }

    /// Continuity prompt for the decoder: tail of the committed line.
    pub fn prompt_tail(&self) -> String {
        let words: Vec<&str> = self.line.split_whitespace().collect();
        let n = words.len().saturating_sub(12);
        words[n..].join(" ")
    }

    pub fn line_open(&self) -> bool {
        !self.line.trim().is_empty()
    }

    pub fn mark_closed(&mut self) {
        self.closed = true;
    }

    pub fn closed(&self) -> bool {
        self.closed
    }
}

fn norm(w: &str) -> String {
    w.trim_matches(|c: char| c.is_ascii_punctuation())
        .to_ascii_lowercase()
}

/// Longest suffix of `a` that matches a prefix of `b` (word-aligned,
/// punctuation/case-insensitive). Returns the word count.
fn longest_suffix_prefix(a: &[&str], b: &[&str]) -> usize {
    let max = a.len().min(b.len());
    for len in (1..=max).rev() {
        let a_tail = &a[a.len() - len..];
        let b_head = &b[..len];
        if a_tail.iter().zip(b_head).all(|(x, y)| norm(x) == norm(y)) {
            return len;
        }
    }
    0
}

fn common_prefix_len(a: &[&str], b: &[&str]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| norm(x) == norm(y)).count()
}

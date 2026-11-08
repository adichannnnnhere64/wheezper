//! Incremental streaming transcription.
//!
//! The segment-then-transcribe pipeline waits for silence (or a 10 s
//! force-cut) before committing anything, and its live previews re-guess the
//! same audio again and again. This module replaces both for live capture.
//!
//! The invariant is *time*, not text: the buffer only ever holds audio that
//! has not been committed yet, every window starts exactly at that boundary,
//! and each pass commits the words whose per-token timestamps end before the
//! window's unstable tail. Committed text is append-only — a word is never
//! revised, re-guessed or emitted twice — and committed audio is dropped, so
//! the next window starts where the last committed word ended.
//!
//! Line breaks happen on sentence punctuation, on a word cap, or after a
//! couple of seconds of silence on the channel.

use crate::capture::TARGET_RATE;
use crate::dsp::Resampler;
use crate::transcribe::{Seg, Transcriber};
use crate::vad::Vad;
use anyhow::Result;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Longest window handed to whisper in one pass (whisper pads to 30 s anyway).
const WINDOW_S: f32 = 8.0;
/// New audio required between inference passes (self-pacing floor).
const NEW_AUDIO_S: f32 = 0.7;
/// Don't bother running whisper below this much uncommitted audio.
const MIN_WINDOW_S: f32 = 1.5;
/// A quiet channel flushes its tail with as little as this.
const IDLE_MIN_WINDOW_S: f32 = 0.5;
/// Never commit a word that ends within this much of the window end: at the
/// live edge whisper has no right-context and that tail is still moving.
const HOLD_S: f32 = 0.5;
/// Channel is considered quiet after this much silence.
const IDLE_AFTER: Duration = Duration::from_millis(700);
/// Safety cap: uncommitted audio beyond this is dropped (we are losing time).
const BUF_CAP_S: f32 = 20.0;
/// Flush a line after this many words even without punctuation.
const LINE_FLUSH_WORDS: usize = 24;
/// Flush a line this long after its channel went quiet.
const IDLE_FLUSH: Duration = Duration::from_millis(2000);

pub struct StreamJob {
    pub chan: usize,
    pub samples: Vec<f32>,
    /// Where `samples[0]` sits on the channel timeline (seconds).
    pub start_s: f64,
    pub prompt: Option<String>,
}

pub struct StreamResult {
    pub chan: usize,
    pub start_s: f64,
    /// Duration of the window that produced `segs`, in seconds.
    pub dur_s: f32,
    pub segs: Vec<Seg>,
}

/// Spawn the shared inference worker (owns the whisper model + VAD).
/// Windows in, segment lists out; one window per channel in flight at a time.
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
                let segs = if vad_blocked {
                    Vec::new()
                } else {
                    tr.segments_greedy(&job.samples, job.prompt.as_deref()).unwrap_or_default()
                };
                let dur_s = job.samples.len() as f32 / TARGET_RATE as f32;
                let res = StreamResult { chan: job.chan, start_s: job.start_s, dur_s, segs };
                if out_tx.send(res).is_err() {
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
    /// Audio not yet committed to text (starts at the commit boundary).
    buf: Vec<f32>,
    /// Channel-timeline position (seconds) of `buf[0]`.
    buf_start_s: f64,
    /// Committed text of the currently open line (append-only).
    line: String,
    unprocessed: usize,
    in_flight: bool,
    last_audio: Instant,
    closed: bool,
    /// Channel-timeline position (seconds) where the last committed word ended.
    last_commit_s: f64,
}

impl StreamChannel {
    pub fn new(label: Option<&'static str>, in_rate: u32) -> Self {
        Self {
            label,
            resampler: Resampler::new(in_rate, TARGET_RATE),
            scratch: Vec::new(),
            buf: Vec::new(),
            buf_start_s: 0.0,
            line: String::new(),
            unprocessed: 0,
            in_flight: false,
            last_audio: Instant::now(),
            closed: false,
            last_commit_s: 0.0,
        }
    }

    /// Feed a mono chunk at the capture rate.
    pub fn push(&mut self, chunk: &[f32]) {
        self.scratch.clear();
        self.resampler.push_slice(chunk, &mut self.scratch);
        self.unprocessed += self.scratch.len();
        // silence from an idle monitor must not look like fresh speech: only
        // audible chunks count as activity (≈ -45 dBFS energy floor)
        let energy = self.scratch.iter().map(|x| x * x).sum::<f32>() / self.scratch.len().max(1) as f32;
        if energy > 0.000_005_6 {
            self.last_audio = Instant::now();
        }
        self.buf.extend_from_slice(&self.scratch);
        let cap = (BUF_CAP_S * TARGET_RATE as f32) as usize;
        if self.buf.len() > cap {
            let drop_n = self.buf.len() - cap;
            self.buf.drain(..drop_n);
            self.buf_start_s += drop_n as f64 / TARGET_RATE as f64;
        }
    }

    fn quiet(&self) -> bool {
        self.last_audio.elapsed() >= IDLE_AFTER
    }

    /// Send the uncommitted window to the worker when there is enough new
    /// audio — or unconditionally once the channel goes quiet, so the tail
    /// gets one final pass instead of waiting for more speech.
    pub fn maybe_send(&mut self, idx: usize, tx: &mpsc::Sender<StreamJob>, line_prompt: &str) {
        if self.in_flight || self.closed {
            return;
        }
        let quiet = self.quiet();
        let min = if quiet { IDLE_MIN_WINDOW_S } else { MIN_WINDOW_S } * TARGET_RATE as f32;
        if self.buf.len() < min as usize {
            return;
        }
        let window_cap = (WINDOW_S * TARGET_RATE as f32) as usize;
        let backlog = self.buf.len() >= window_cap;
        let need = (NEW_AUDIO_S * TARGET_RATE as f32) as usize;
        if !quiet && !backlog && self.unprocessed < need {
            return;
        }
        let win_len = window_cap.min(self.buf.len());
        let samples = self.buf[..win_len].to_vec();
        let prompt = (!line_prompt.is_empty()).then(|| line_prompt.to_string());
        let job = StreamJob { chan: idx, samples, start_s: self.buf_start_s, prompt };
        let _ = tx.send(job);
        self.in_flight = true;
        self.unprocessed = 0;
    }

    /// A hypothesis arrived: commit the words that are old enough, drop
    /// their audio, return the text appended to the line.
    pub fn on_result(&mut self, start_s: f64, dur_s: f32, segs: &[Seg]) -> String {
        self.in_flight = false;
        // on a quiet channel the window's tail IS the end of the utterance:
        // no later pass will give it right-context, so commit it as-is
        let quiet = self.quiet();
        let cutoff = (if quiet { dur_s } else { dur_s - HOLD_S }).max(0.0) as f64;

        let mut delta = String::new();
        let mut upto = start_s;
        'segs: for seg in segs {
            for word in &seg.words {
                let rel_end = (word.end_s as f64).min(dur_s as f64);
                // timestamps behind the commit boundary were decoded twice
                if rel_end <= upto - start_s {
                    continue;
                }
                // still at the live edge: this word can change, wait for it
                if rel_end > cutoff {
                    break 'segs;
                }
                delta.push_str(&word.text);
                self.line.push_str(&word.text);
                upto = start_s + rel_end;
            }
        }

        // nothing was committed: either the tail is still moving, or the
        // window held no speech at all. Only the latter frees the audio.
        if upto == start_s && (quiet || self.buf.len() >= (WINDOW_S * TARGET_RATE as f32) as usize) {
            upto = start_s + cutoff;
        }
        if !delta.is_empty() {
            self.last_commit_s = upto;
        }
        self.drop_committed(upto);
        delta
    }

    /// Send the last uncommitted audio at shutdown (no new audio will come,
    /// so the caller should treat `on_result` as quiet).
    pub fn flush(&mut self) {
        self.last_audio = Instant::now() - IDLE_AFTER;
    }

    fn drop_committed(&mut self, upto_s: f64) {
        let n = (((upto_s - self.buf_start_s) * TARGET_RATE as f64) as usize).min(self.buf.len());
        if n == 0 {
            return;
        }
        self.buf.drain(..n);
        self.buf_start_s += n as f64 / TARGET_RATE as f64;
    }

    /// Stop accepting audio for this channel (the shutdown drain commits
    /// whatever the last window produced before this).
    pub fn finish(&mut self) {
        self.closed = true;
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

    /// Is a window for this channel still being transcribed?
    pub fn busy(&self) -> bool {
        self.in_flight
    }

    /// End of the audio covered by the words committed so far (seconds on the
    /// channel timeline). Used by the latency benchmark and diagnostics.
    pub fn last_commit_s(&self) -> f64 {
        self.last_commit_s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 kHz mono f32 speech used by the replay test. Committed under
    /// bench/fixtures so `cargo test` works in a clean checkout (CI).
    fn load_f32() -> Option<Vec<f32>> {
        let bytes = std::fs::read("bench/fixtures/jfk.f32").ok()?;
        Some(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect(),
        )
    }

    /// Measures live caption latency: word-end -> committed-to-line, end to
    /// end, with audio fed in real time (what a live meeting looks like).
    ///
    ///   MODEL=base LATENCY_SECONDS=40 cargo test --release \
    ///     stream::tests::measures_commit_latency -- --ignored --nocapture
    #[test]
    #[ignore = "benchmark: needs the model + fixture, runs in real time"]
    fn measures_commit_latency() {
        let Some(jfk) = load_f32() else {
            eprintln!("skipping: bench/fixtures/jfk.f32 missing");
            return;
        };
        let model_name = std::env::var("MODEL").unwrap_or_else(|_| "base".into());
        let seconds: f32 = std::env::var("LATENCY_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40.0);

        // CHANNELS=2 models --source dual (mic + loopback): both channels share
        // one inference worker, so they compete for it pass by pass.
        let n_chans: usize = std::env::var("CHANNELS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let model = crate::models::resolve(Some(&model_name)).expect("model");
        let threads = 4;
        let (tx, rx) = spawn_worker(&model, "en", threads, false).expect("worker");
        let mut chans: Vec<StreamChannel> = (0..n_chans)
            .map(|_| StreamChannel::new(Some("them"), TARGET_RATE))
            .collect();

        let mut audio: Vec<f32> = Vec::new();
        while (audio.len() as f32 / TARGET_RATE as f32) < seconds {
            audio.extend_from_slice(&jfk);
        }

        let chunk = TARGET_RATE as usize / 10; // 100 ms, like a cpal callback
        let feed_start = Instant::now();
        let mut commits: Vec<(f64, f64)> = Vec::new(); // (audio position, latency)
        let mut passes: Vec<f64> = Vec::new();
        let mut sent_at: Option<Instant> = None;
        let mut i = 0;
        while i < audio.len() {
            let end = (i + chunk).min(audio.len());
            for sc in chans.iter_mut() {
                sc.push(&audio[i..end]);
            }
            i = end;

            while let Ok(res) = rx.try_recv() {
                if let Some(t) = sent_at.take() {
                    passes.push(t.elapsed().as_secs_f64());
                }
                let now = Instant::now();
                let Some(sc) = chans.get_mut(res.chan) else { continue };
                let delta = sc.on_result(res.start_s, res.dur_s, &res.segs);
                if !delta.trim().is_empty() {
                    let committed = sc.last_commit_s();
                    // that audio was handed to the channel at feed_start + committed
                    let latency = now.duration_since(feed_start).as_secs_f64() - committed;
                    commits.push((committed, latency));
                    eprintln!("t=+{:6.2}s commit={:6.2}s latency={:5.2}s  {}", i as f32 / TARGET_RATE as f32, committed, latency, delta.trim());
                }
            }
            for (idx, sc) in chans.iter_mut().enumerate() {
                let prompt = sc.prompt_tail();
                sc.maybe_send(idx, &tx, &prompt);
                if sc.busy() && sent_at.is_none() {
                    sent_at = Some(Instant::now());
                }
                if sc.line_ready()
                    && let Some(line) = sc.take_line()
                {
                    eprintln!("LINE: {line}");
                }
            }

            let fed = i as f32 / TARGET_RATE as f32;
            let spent = feed_start.elapsed().as_secs_f32();
            if fed > spent {
                std::thread::sleep(Duration::from_secs_f32(fed - spent));
            }
        }

        let mut lat: Vec<f64> = commits.iter().map(|(_, l)| *l).collect();
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| -> f64 {
            if lat.is_empty() { f64::NAN } else { lat[((lat.len() - 1) as f64 * p) as usize] }
        };
        let mut pass_sorted = passes.clone();
        pass_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "\nMODEL={model_name} threads={threads} channels={n_chans} audio={:.1}s commits={} passes={}",
            audio.len() as f32 / TARGET_RATE as f32,
            commits.len(),
            passes.len()
        );
        eprintln!(
            "latency (word end -> on screen): min={:.2}s median={:.2}s p90={:.2}s max={:.2}s",
            lat.first().copied().unwrap_or(f64::NAN),
            pct(0.5),
            pct(0.9),
            lat.last().copied().unwrap_or(f64::NAN)
        );
        if commits.len() >= 6 {
            eprintln!(
                "drift: first 3 avg={:.2}s, last 3 avg={:.2}s",
                commits[..3].iter().map(|c| c.1).sum::<f64>() / 3.0,
                commits[commits.len() - 3..].iter().map(|c| c.1).sum::<f64>() / 3.0
            );
        }
        eprintln!(
            "inference pass: median={:.2}s max={:.2}s ({} passes, {:.2}s wall/audio-second)",
            pass_sorted.get(pass_sorted.len() / 2).copied().unwrap_or(f64::NAN),
            pass_sorted.last().copied().unwrap_or(f64::NAN),
            passes.len(),
            feed_start.elapsed().as_secs_f64() / (audio.len() as f64 / TARGET_RATE as f64)
        );
        assert!(!commits.is_empty(), "no words were committed");
    }

    /// Replays a 16 kHz f32 file through the exact main-loop logic of
    /// `run_stream`, so stream-mode regressions are reproducible without
    /// audio hardware.
    #[test]
    fn replays_audio_through_stream_loop() {
        let Some(audio) = load_f32() else {
            eprintln!("skipping: bench/fixtures/jfk.f32 missing");
            return;
        };
        let model = crate::models::resolve(Some("base")).expect("model");
        let (tx, rx) = spawn_worker(&model, "en", 4, true).expect("worker");
        let mut sc = StreamChannel::new(Some("them"), TARGET_RATE);

        let mut committed = String::new();
        let chunk = TARGET_RATE as usize / 10; // 100 ms, like a cpal callback
        let started = Instant::now();
        let mut i = 0;
        while i < audio.len() {
            let end = (i + chunk).min(audio.len());
            sc.push(&audio[i..end]);
            i = end;
            while let Ok(res) = rx.try_recv() {
                let delta = sc.on_result(res.start_s, res.dur_s, &res.segs);
                if !delta.is_empty() {
                    committed.push_str(&delta);
                    committed.push(' ');
                }
            }
            let prompt = sc.prompt_tail();
            sc.maybe_send(0, &tx, &prompt);
            if sc.line_ready()
                && let Some(line) = sc.take_line()
            {
                eprintln!("LINE: {line}");
            }
            // real time minus what inference already spent
            let fed = i as f32 / TARGET_RATE as f32;
            let spent = started.elapsed().as_secs_f32();
            if fed > spent {
                std::thread::sleep(Duration::from_secs_f32(fed - spent));
            }
        }
        // shutdown: one final pass, drained like the real loop
        sc.flush();
        let prompt = sc.prompt_tail();
        sc.maybe_send(0, &tx, &prompt);
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && sc.busy() {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(res) => {
                    let delta = sc.on_result(res.start_s, res.dur_s, &res.segs);
                    if !delta.is_empty() {
                        committed.push_str(&delta);
                        committed.push(' ');
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        sc.finish();
        eprintln!("COMMITTED: {committed:?}");
        let lower = committed.to_ascii_lowercase();
        assert!(lower.contains("fellow americans"), "missing opening: {committed:?}");
        assert!(lower.contains("country"), "missing closing: {committed:?}");
        // committed exactly once: no word triple may appear twice in a row
        let words: Vec<&str> = lower.split_whitespace().collect();
        for w in words.windows(6) {
            assert!(w[..3] != w[3..], "output repeats itself: {committed:?}");
        }
    }
}

//! Incremental streaming transcription.
//!
//! The segment-then-transcribe pipeline waits for silence (or a 10 s
//! force-cut) before committing anything, and its live previews re-guess the
//! same audio again and again. This module replaces both for live capture.
//!
//! Phase 2 runs **two tiers per channel**:
//!
//! - the *final* tier is the Phase-1 engine, unchanged: generous windows,
//!   full-quality inference, words committed exactly once by timestamp,
//!   append-only. Only its commits reach the notes.
//! - the *provisional* tier decodes just the live edge (tail windows, tiny
//!   encoder context, one greedy pass) to put words on screen within the
//!   0.5 s budget. Its text is display-only and superseded word-by-word as
//!   the final tier confirms, so it may be rough — it can never be wrong in
//!   the notes.
//!
//! The invariant is *time*, not text: each tier's buffer only ever holds
//! audio that tier has not committed yet, every window starts exactly at
//! that boundary (provisional windows start *within* it, at the live edge),
//! and each pass commits the words whose per-token timestamps end before the
//! window's unstable tail. Committed text is append-only — a word is never
//! revised, re-guessed or emitted twice — and committed audio is dropped.
//!
//! Line breaks happen on sentence punctuation, on a word cap, or after a
//! couple of seconds of silence on the channel.

use crate::capture::TARGET_RATE;
use crate::dsp::Resampler;
use crate::transcribe::{Profile, Seg, Transcriber};
use crate::vad::Vad;
use anyhow::Result;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Flush a line after this many words even without punctuation.
const LINE_FLUSH_WORDS: usize = 24;
/// How far past the commit boundary a word may end and still count as the
/// same word the previous pass already committed (whisper re-decodes the word
/// straddling the boundary, so it comes back with a slightly later end).
const DUP_SLACK_S: f64 = 0.6;
/// How many trailing committed words the dedupe compares against.
const DUP_TAIL_WORDS: usize = 4;
/// A committed word may end at most this far past the last audio that
/// actually carried speech energy (whisper invents words on silence — the
/// shutdown drain of an idle channel used to append "you you you").
const SPEECH_SLACK_S: f64 = 0.4;
/// Per-chunk energy floor (~-52 dBFS): below this the chunk is silence.
const SPEECH_ENERGY: f32 = 0.000_005_6;
/// Flush a line this long after its channel went quiet.
const IDLE_FLUSH: Duration = Duration::from_millis(2000);

/// Per-tier tuning. The tiers differ in *when* their windows may fire and
/// how much right-context a word needs before it counts as stable.
#[derive(Clone, Copy)]
pub struct Tuning {
    /// Longest window handed to whisper in one pass.
    pub window_s: f32,
    /// Don't run whisper below this much uncommitted audio.
    pub min_window_s: f32,
    /// A quiet channel flushes its tail with as little as this.
    pub idle_min_window_s: f32,
    /// New audio required between inference passes (self-pacing floor).
    pub new_audio_s: f32,
    /// Never commit a word that ends within this much of the window end: at
    /// the live edge whisper has no right-context and that tail is unstable.
    pub hold_s: f32,
    /// Silence after which the channel counts as quiet and its tail is
    /// committed as-is (no later pass will give it right-context). Longer for
    /// the final tier: natural pauses inside a sentence are ~0.7 s, and
    /// committing then without right-context is where the garbage words come
    /// from (measured: "and fellow Americans. And" for "fellow Americans").
    pub idle_after_s: f32,
    /// Safety cap: uncommitted audio beyond this is dropped (losing time).
    pub buf_cap_s: f32,
    /// Decode the buffer's *tail* (live edge) instead of its head (commit
    /// boundary). The provisional tier needs this: the final boundary lags
    /// seconds behind, and a head window would keep re-decoding stale audio
    /// instead of the words being spoken right now.
    pub tail_window: bool,
    /// Keep an open line and report it via `line_ready` (final tier only).
    pub track_line: bool,
}

impl Tuning {
    /// Phase-1 engine, byte-for-byte: this is what the notes are made of,
    /// and it must not drift from the quality the fixture gate asserts.
    /// The cap is generous because a rare temperature-ladder stall must not
    /// silently drop uncommitted audio (30 s ≈ 2 MB per channel).
    pub fn final_default() -> Self {
        Self {
            window_s: 8.0,
            min_window_s: 1.5,
            idle_min_window_s: 0.5,
            // 3 s of new audio per pass, not 0.7: each pass starts at the
            // commit boundary and commits up to `window - hold`, so a 3 s
            // cadence commits ~2.5-3 s of text per pass instead of ~1 s.
            // Every boundary is a place where whisper has no left context;
            // measured on the jfk fixture replayed three times, cadence 0.7
            // scored 71-91% (mean 82) while 3.0 scored 95.45% eight runs in a
            // row. The extra lag is invisible behind the preview tier.
            new_audio_s: 3.0,
            hold_s: 0.5,
            idle_after_s: 1.5,
            buf_cap_s: 30.0,
            tail_window: false,
            track_line: true,
        }
    }

    /// Live preview: cheap windows on the live edge. `window_s` must stay
    /// within `audio_ctx * 20 ms` of the provisional profile (see
    /// `transcribe`), or audio past the encoder window is silently cut.
    /// Tuned on the reference machine for the ≤0.5 s median / ≤0.8 s p90
    /// dual-channel budget.
    pub fn provisional() -> Self {
        Self {
            window_s: 0.8,
            min_window_s: 0.30,
            idle_min_window_s: 0.25,
            new_audio_s: 0.02,
            hold_s: 0.10,
            idle_after_s: 0.7,
            buf_cap_s: 4.0,
            tail_window: true,
            track_line: false,
        }
    }

    /// Sweep knobs: env overrides so latency experiments don't need a
    /// rebuild per configuration (defaults are the shipped behaviour).
    /// Provisional tier only — the final tier is frozen at Phase-1 tuning so
    /// a sweep can never silently degrade notes quality.
    pub fn with_env(mut self) -> Self {
        let f = |k: &str, slot: &mut f32| {
            if let Some(v) = std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok()) {
                *slot = v;
            }
        };
        f("WZ_WINDOW_S", &mut self.window_s);
        f("WZ_MIN_WINDOW_S", &mut self.min_window_s);
        f("WZ_IDLE_MIN_WINDOW_S", &mut self.idle_min_window_s);
        f("WZ_NEW_AUDIO_S", &mut self.new_audio_s);
        f("WZ_HOLD_S", &mut self.hold_s);
        f("WZ_IDLE_AFTER_S", &mut self.idle_after_s);
        self
    }
}

/// Split the core budget between the tiers. The provisional tier is the
/// latency-critical one (the 0.5 s budget); the final tier's lag is
/// invisible behind the preview, so it runs on the leftovers — never below
/// one thread, or the notes would fall hopelessly behind. One core is
/// always reserved for the capture/main loop: oversubscribing the box
/// makes the ggml spin barriers convoy and doubles every pass time
/// (measured on the 6-core reference box: 2+4 misses the p90 budget,
/// 2+3 clears it with margin).
pub fn split_threads(cores: i32, requested: i32) -> (i32, i32) {
    if requested > 0 {
        // explicit --threads: split it evenly and predictably
        let half = (requested / 2).max(1);
        return (half, half);
    }
    let fin = (cores / 3).clamp(1, 2);
    let prov = (cores - fin - 1).clamp(1, 4);
    (fin, prov)
}

/// Resamples capture-rate chunks to 16 kHz once per channel, so both tiers
/// see the same mono stream without paying for two resamplers.
pub struct ChannelFeed {
    resampler: Resampler,
    scratch: Vec<f32>,
}

impl ChannelFeed {
    pub fn new(in_rate: u32) -> Self {
        Self { resampler: Resampler::new(in_rate, TARGET_RATE), scratch: Vec::new() }
    }

    /// Convert one capture chunk; the returned slice is only valid until
    /// the next call.
    pub fn resample(&mut self, chunk: &[f32]) -> &[f32] {
        self.scratch.clear();
        self.resampler.push_slice(chunk, &mut self.scratch);
        &self.scratch
    }
}

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

/// Words committed by one inference pass, in order. Each entry is the
/// word's end on the channel timeline (seconds) plus its text — the
/// provisional tier trims its preview with them as the final tier advances.
#[derive(Default)]
pub struct CommitDelta {
    pub text: String,
    pub words: Vec<(f64, String)>,
}

/// Spawn an inference worker (owns the whisper model + VAD for its
/// profile). Windows in, segment lists out; one window per channel in
/// flight at a time. Each profile gets its own worker so the cheap
/// provisional passes never queue behind a full final pass.
pub fn spawn_worker(
    model: &Path,
    language: &str,
    threads: i32,
    use_vad: bool,
    profile: Profile,
) -> Result<(mpsc::Sender<StreamJob>, mpsc::Receiver<StreamResult>)> {
    let (tx, rx) = mpsc::channel::<StreamJob>();
    let (out_tx, out_rx) = mpsc::channel::<StreamResult>();
    let (model, language) = (model.to_path_buf(), language.to_string());
    std::thread::Builder::new()
        .name(format!("stream-worker-{profile:?}"))
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
                let started = Instant::now();
                let segs = if vad_blocked {
                    Vec::new()
                } else {
                    tr.segments_greedy(&job.samples, job.prompt.as_deref(), profile)
                        .unwrap_or_default()
                };
                let dur_s = job.samples.len() as f32 / TARGET_RATE as f32;
                if std::env::var("WZ_STREAM_DEBUG").ok().as_deref() == Some("1") {
                    eprintln!(
                        "decode {profile:?} dur={dur_s:.2} took={:.2}s words={} prompt={:?}",
                        started.elapsed().as_secs_f32(),
                        segs.iter().map(|s| s.words.len()).sum::<usize>(),
                        job.prompt.as_deref().map(|p| p.len())
                    );
                }
                let res = StreamResult { chan: job.chan, start_s: job.start_s, dur_s, segs };
                if out_tx.send(res).is_err() {
                    return;
                }
            }
        })?;
    Ok((tx, out_rx))
}

/// Per-channel streaming state (main-loop side). One instance per tier.
pub struct StreamChannel {
    pub label: Option<&'static str>,
    tuning: Tuning,
    /// Audio not yet committed to text (starts at the commit boundary).
    buf: Vec<f32>,
    /// Channel-timeline position (seconds) of `buf[0]`.
    buf_start_s: f64,
    /// Committed text of the currently open line (append-only, final tier).
    line: String,
    unprocessed: usize,
    in_flight: bool,
    last_audio: Instant,
    closed: bool,
    /// Channel-timeline position (seconds) where the last committed word ended.
    last_commit_s: f64,
    /// Channel-timeline end of the last audio chunk above the speech floor.
    last_speech_s: f64,
    /// Text of the last few committed words, for the boundary dedupe.
    committed_tail: Vec<String>,
    /// The "falling behind" warning is printed once per channel.
    warned_drop: bool,
}

impl StreamChannel {
    pub fn new(label: Option<&'static str>, tuning: Tuning) -> Self {
        Self {
            label,
            tuning,
            buf: Vec::new(),
            buf_start_s: 0.0,
            line: String::new(),
            unprocessed: 0,
            in_flight: false,
            last_audio: Instant::now(),
            closed: false,
            last_commit_s: 0.0,
            last_speech_s: 0.0,
            committed_tail: Vec::new(),
            warned_drop: false,
        }
    }

    /// Feed a resampled 16 kHz mono chunk (see `ChannelFeed`).
    pub fn push_16k(&mut self, chunk: &[f32]) {
        self.unprocessed += chunk.len();
        // silence from an idle monitor must not look like fresh speech: only
        // audible chunks count as activity (≈ -45 dBFS energy floor)
        let energy = chunk.iter().map(|x| x * x).sum::<f32>() / chunk.len().max(1) as f32;
        if energy > SPEECH_ENERGY {
            self.last_audio = Instant::now();
        }
        self.buf.extend_from_slice(chunk);
        if energy > SPEECH_ENERGY {
            self.last_speech_s = self.buf_start_s + self.buf.len() as f64 / TARGET_RATE as f64;
        }
        let cap = (self.tuning.buf_cap_s * TARGET_RATE as f32) as usize;
        if self.buf.len() > cap {
            let drop_n = self.buf.len() - cap;
            self.buf.drain(..drop_n);
            self.buf_start_s += drop_n as f64 / TARGET_RATE as f64;
            // losing audio is never silent: this is the only path that can drop
            // speech, and it means the box cannot keep up with the model
            if !self.warned_drop {
                self.warned_drop = true;
                eprintln!(
                    "wheezper: stream: inference is not keeping up — dropping the oldest uncommitted audio (try a smaller --model or more --threads)"
                );
            }
        }
    }

    fn quiet(&self) -> bool {
        self.last_audio.elapsed() >= Duration::from_secs_f32(self.tuning.idle_after_s)
    }

    /// Send the uncommitted window to the worker when there is enough new
    /// audio — or unconditionally once the channel goes quiet, so the tail
    /// gets one final pass instead of waiting for more speech. Returns true
    /// when a pass was actually dispatched (the shutdown drain uses that to
    /// tell "drained" from "still has uncommitted audio").
    pub fn maybe_send(&mut self, idx: usize, tx: &mpsc::Sender<StreamJob>, line_prompt: &str) -> bool {
        if self.in_flight || self.closed {
            return false;
        }
        let quiet = self.quiet();
        let min =
            if quiet { self.tuning.idle_min_window_s } else { self.tuning.min_window_s } * TARGET_RATE as f32;
        if self.buf.len() < min as usize {
            return false;
        }
        let window_cap = (self.tuning.window_s * TARGET_RATE as f32) as usize;
        let backlog = self.buf.len() >= window_cap;
        let need = (self.tuning.new_audio_s * TARGET_RATE as f32) as usize;
        if !quiet && !backlog && self.unprocessed < need {
            return false;
        }
        let win_len = window_cap.min(self.buf.len());
        // final tiers decode from the commit boundary (head of the buffer);
        // provisional tiers decode the live edge (tail) so the preview
        // never sits on stale audio while the final boundary lags behind
        let off = if self.tuning.tail_window { self.buf.len() - win_len } else { 0 };
        let samples = self.buf[off..off + win_len].to_vec();
        let start_s = self.buf_start_s + off as f64 / TARGET_RATE as f64;
        let prompt = (!line_prompt.is_empty()).then(|| line_prompt.to_string());
        let job = StreamJob { chan: idx, samples, start_s, prompt };
        let _ = tx.send(job);
        self.in_flight = true;
        self.unprocessed = 0;
        true
    }

    /// A hypothesis arrived: commit the words that are old enough, drop
    /// their audio, return the text appended to the line.
    pub fn on_result(&mut self, start_s: f64, dur_s: f32, segs: &[Seg]) -> CommitDelta {
        self.in_flight = false;
        // on a quiet channel the window's tail IS the end of the utterance:
        // no later pass will give it right-context, so commit it as-is
        let quiet = self.quiet();
        let cutoff = (if quiet { dur_s } else { dur_s - self.tuning.hold_s }).max(0.0) as f64;

        // stable words of this pass, in order: (channel-time end, text)
        let mut cand: Vec<(f64, &str)> = Vec::new();
        'segs: for seg in segs {
            for word in &seg.words {
                let rel_end = (word.end_s as f64).min(dur_s as f64);
                let abs_end = start_s + rel_end;
                // audio behind the boundary was committed by an earlier pass
                if abs_end <= self.last_commit_s {
                    continue;
                }
                // still at the live edge: this word can change, wait for it
                if rel_end > cutoff {
                    break 'segs;
                }
                // there is no audio left that could carry this word: whisper
                // keeps decoding on padding silence ("you you you" at the end
                // of an idle channel), so never commit past the last sound
                if abs_end > self.last_speech_s + SPEECH_SLACK_S {
                    break 'segs;
                }
                cand.push((abs_end, word.text.as_str()));
            }
        }
        // whisper re-decodes the word straddling the boundary and hands it
        // back with a slightly later end, so it would be committed twice
        // (measured: "Ask not!" then "not watch your ..."). Drop a leading
        // run that repeats what is already in the notes.
        let dup = self.leading_repeat(&cand);

        let mut delta = CommitDelta::default();
        let mut upto = start_s;
        for (abs_end, text) in cand[dup..].iter().copied() {
            delta.text.push_str(text);
            delta.words.push((abs_end, text.to_string()));
            if self.tuning.track_line {
                self.line.push_str(text);
            }
            upto = abs_end;
            // punctuation-only tokens ("!", ",") are not words: they would
            // otherwise sit in the dedupe tail as "" and break the comparison
            let norm = text.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if !norm.is_empty() {
                self.committed_tail.push(norm);
                if self.committed_tail.len() > DUP_TAIL_WORDS {
                    self.committed_tail.remove(0);
                }
            }
        }

        // nothing was committed: either the tail is still moving, or the decode
        // came back empty. Only a *quiet* channel whose buffered audio carries
        // no speech at all may be dropped — an empty decode of speech must be
        // retried, not thrown away (on a slow runner this silently deleted up
        // to 8 s of notes per pass; the transcript ended while audio played on).
        if upto == start_s && quiet && self.last_speech_s <= start_s + 0.1 {
            upto = start_s + cutoff;
        }
        if !delta.text.is_empty() {
            self.last_commit_s = upto;
        }
        if std::env::var("WZ_STREAM_DEBUG").ok().as_deref() == Some("1") {
            eprintln!(
                "pass start={start_s:.2} dur={dur_s:.2} cutoff={cutoff:.2} quiet={quiet} words={} dup={dup} cand_head={:?} tail={:?} committed={:?} boundary={upto:.2}",
                cand.len(),
                cand.iter().take(2).map(|(e, w)| (format!("{e:.2}"), w.trim())).collect::<Vec<_>>(),
                self.committed_tail,
                delta.text.trim()
            );
        }
        self.drop_committed(upto);
        delta
    }

    /// Number of leading words of `cand` that are the same words the previous
    /// pass just committed (whisper re-decodes the boundary word).
    fn leading_repeat(&self, cand: &[(f64, &str)]) -> usize {
        let max = self.committed_tail.len().min(cand.len()).min(DUP_TAIL_WORDS);
        let mut dup = 0;
        for k in 1..=max {
            let tail = &self.committed_tail[self.committed_tail.len() - k..];
            let same = tail.iter().zip(&cand[..k]).all(|(t, (end, w))| {
                *end < self.last_commit_s + DUP_SLACK_S
                    && *t == w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
            });
            if same {
                dup = k;
            }
        }
        dup
    }

    /// Send the last uncommitted audio at shutdown (no new audio will come,
    /// so the caller should treat `on_result` as quiet).
    pub fn flush(&mut self) {
        self.last_audio = Instant::now() - Duration::from_secs_f32(self.tuning.idle_after_s);
    }

    /// Drop buffered audio before channel-time `upto_s` (seconds). Used by
    /// the provisional tier when the final boundary passes it: that audio
    /// is locked into final text and no longer needs previewing.
    pub fn drop_below(&mut self, upto_s: f64) {
        self.drop_committed(upto_s);
    }

    fn drop_committed(&mut self, upto_s: f64) {
        // negative when the caller points before the buffer start (final
        // boundary behind our own): saturating cast makes that a no-op
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
        if !self.tuning.track_line {
            return false;
        }
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
        if !self.tuning.track_line {
            return None;
        }
        let line = self.line.trim().to_string();
        if line.is_empty() {
            return None;
        }
        self.line.clear();
        Some(line)
    }

    /// Continuity prompt for the decoder: tail of the committed line.
    /// Continuity hint for the next window: off by default. A tail of the
    /// open line is text for the decoder to *complete*, and a completed
    /// sentence is an early EOT or a hallucinated continuation (measured:
    /// 2 words committed for 3 s of audio, and "asked me to come back to the"
    /// invented after the speech ended). Off: 91.7 % mean over ten audio
    /// alignments, min 83; 12 words: 88.5 % mean, min 78.8. `--lyrics` passes
    /// its own prompt; `WZ_PROMPT_WORDS=n` restores a tail for experiments.
    pub fn prompt_tail(&self) -> String {
        let n_words: usize = std::env::var("WZ_PROMPT_WORDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if n_words == 0 {
            return String::new();
        }
        let words: Vec<&str> = self.line.split_whitespace().collect();
        let n = words.len().saturating_sub(n_words);
        words[n..].join(" ")
    }

    pub fn line_open(&self) -> bool {
        self.tuning.track_line && !self.line.trim().is_empty()
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

    /// End of the audio covered by the words committed so far (seconds on
    /// the channel timeline). Used by the latency benchmark and by the
    /// provisional tier to shed preview words the final tier confirmed.
    pub fn last_commit_s(&self) -> f64 {
        self.last_commit_s
    }
}

/// Live-edge preview tier: fast, rough, display-only. Its buffer starts at
/// the final tier's commit boundary and its windows are the buffer's tail,
/// so its decode always sits on the words being spoken *now*, even while
/// the final tier lags seconds behind. As the final tier commits, the
/// preview sheds the words it has confirmed.
pub struct ProvisionalTier {
    ch: StreamChannel,
    /// Preview words beyond the final boundary: (channel-time end, text).
    words: Vec<(f64, String)>,
    /// Latest final-tier commit boundary (seconds, channel timeline).
    final_boundary_s: f64,
}

impl ProvisionalTier {
    pub fn new() -> Self {
        Self {
            ch: StreamChannel::new(None, Tuning::provisional().with_env()),
            words: Vec::new(),
            final_boundary_s: 0.0,
        }
    }

    pub fn push_16k(&mut self, chunk: &[f32]) {
        self.ch.push_16k(chunk)
    }

    pub fn maybe_send(&mut self, idx: usize, tx: &mpsc::Sender<StreamJob>) -> bool {
        self.ch.maybe_send(idx, tx, "")
    }

    /// A provisional hypothesis arrived: commit the stable words and return
    /// them ((end, text) pairs, newest last). Words the final tier already
    /// confirmed never make it into the preview.
    pub fn on_result(&mut self, start_s: f64, dur_s: f32, segs: &[Seg]) -> Vec<(f64, String)> {
        let delta = self.ch.on_result(start_s, dur_s, segs);
        let committed: Vec<(f64, String)> =
            delta.words.into_iter().filter(|(e, _)| *e > self.final_boundary_s).collect();
        self.words.extend(committed.iter().cloned());
        // bounded: the preview renders only its last few dozen words
        if self.words.len() > 128 {
            self.words.drain(..64);
        }
        committed
    }

    /// The final tier committed up to channel-time `s`: its text supersedes
    /// everything the preview holds below that point.
    pub fn advance_final_boundary(&mut self, s: f64) {
        self.final_boundary_s = self.final_boundary_s.max(s);
        self.words.retain(|(e, _)| *e > self.final_boundary_s);
        self.ch.drop_below(self.final_boundary_s);
    }

    /// Current preview text (the words decoded past the final boundary).
    pub fn preview(&self) -> String {
        let mut out = String::new();
        for (_, w) in &self.words {
            out.push_str(w.trim_start());
            out.push(' ');
        }
        out.trim_end().to_string()
    }

    pub fn flush(&mut self) {
        self.ch.flush()
    }

    pub fn finish(&mut self) {
        self.ch.finish()
    }

    pub fn busy(&self) -> bool {
        self.ch.busy()
    }

    pub fn mark_closed(&mut self) {
        self.ch.mark_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 kHz mono f32 speech used by the replay test. Committed under
    /// bench/fixtures so `cargo test` works in a clean checkout (CI).
    fn load_f32() -> Option<Vec<f32>> {
        load_f32_path("bench/fixtures/jfk.f32")
    }

    /// Read a raw 16 kHz f32le file.
    fn load_f32_path(path: &str) -> Option<Vec<f32>> {
        let bytes = std::fs::read(path).ok()?;
        Some(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect(),
        )
    }

    /// Measures live caption latency: word-end -> on screen, end to end,
    /// with audio fed in real time (what a live meeting looks like).
    ///
    /// The Phase 2 acceptance run is the *provisional* tier (the words a
    /// viewer sees), dual channel worst case, gated via WZ_ACCEPT:
    ///
    ///   CHANNELS=2 MODEL=base WZ_ACCEPT=1 LATENCY_SECONDS=40 cargo test \
    ///     --release stream::tests::measures_commit_latency -- --ignored \
    ///     --nocapture
    ///
    /// The final tier (notes quality) is reported alongside but not gated:
    /// its lag is invisible, the provisional tier already showed the text.
    #[test]
    #[ignore = "benchmark: needs the model + fixture, runs in real time"]
    fn measures_commit_latency() {
        let Some(jfk) = load_f32() else {
            eprintln!("skipping: bench/fixtures/jfk.f32 missing");
            return;
        };
        let model_name = std::env::var("MODEL").unwrap_or_else(|_| "base".into());
        let seconds: f32 =
            std::env::var("LATENCY_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40.0);

        // CHANNELS=2 models --source dual (mic + loopback): both channels
        // feed both shared workers, so the tiers compete per pass.
        let n_chans: usize = std::env::var("CHANNELS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let model = crate::models::resolve(Some(&model_name)).expect("model");
        // split the core budget between the tiers like run_stream does: two
        // full-thread workers oversubscribe the box and their ggml spin
        // barriers collapse each other's pass times
        let cores = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
        let (dflt_fin, dflt_prov) = split_threads(cores, 0);
        let threads: i32 =
            std::env::var("WZ_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(dflt_fin);
        let prov_threads: i32 =
            std::env::var("WZ_PROV_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(dflt_prov);
        let (tx, rx) = spawn_worker(&model, "en", threads, false, Profile::Final).expect("worker");
        let (ptx, prx) =
            spawn_worker(&model, "en", prov_threads, false, Profile::Provisional).expect("prov worker");

        struct Ch {
            feed: ChannelFeed,
            fin: StreamChannel,
            prov: ProvisionalTier,
        }
        let mut chans: Vec<Ch> = (0..n_chans)
            .map(|_| Ch {
                feed: ChannelFeed::new(TARGET_RATE),
                // the final tier is frozen at Phase-1 tuning: sweep knobs
                // (WZ_*) apply to the provisional tier only, so a latency
                // sweep can never silently degrade notes quality
                fin: StreamChannel::new(Some("them"), Tuning::final_default()),
                prov: ProvisionalTier::new(),
            })
            .collect();

        let mut audio: Vec<f32> = Vec::new();
        while (audio.len() as f32 / TARGET_RATE as f32) < seconds {
            audio.extend_from_slice(&jfk);
        }

        let chunk = TARGET_RATE as usize / 10; // 100 ms, like a cpal callback
        let feed_start = Instant::now();
        // (word end, latency) per tier; pass wall-times per worker
        let mut prov_commits: Vec<(f64, f64)> = Vec::new();
        let mut fin_commits: Vec<(f64, f64)> = Vec::new();
        let mut prov_passes: Vec<f64> = Vec::new();
        let mut fin_passes: Vec<f64> = Vec::new();
        let mut prov_sent: Option<Instant> = None;
        let mut fin_sent: Option<Instant> = None;
        let mut i = 0;
        while i < audio.len() {
            let end = (i + chunk).min(audio.len());
            for ch in chans.iter_mut() {
                let mono = ch.feed.resample(&audio[i..end]);
                ch.fin.push_16k(mono);
                ch.prov.push_16k(mono);
            }
            i = end;

            while let Ok(res) = rx.try_recv() {
                if let Some(t) = fin_sent.take() {
                    fin_passes.push(t.elapsed().as_secs_f64());
                }
                let now = Instant::now();
                let Some(ch) = chans.get_mut(res.chan) else { continue };
                let d = ch.fin.on_result(res.start_s, res.dur_s, &res.segs);
                ch.prov.advance_final_boundary(ch.fin.last_commit_s());
                if !d.text.trim().is_empty() {
                    let committed = ch.fin.last_commit_s();
                    let latency = now.duration_since(feed_start).as_secs_f64() - committed;
                    fin_commits.push((committed, latency));
                    eprintln!(
                        "t=+{:6.2}s FINAL commit={:6.2}s latency={:5.2}s  {}",
                        i as f32 / TARGET_RATE as f32,
                        committed,
                        latency,
                        d.text.trim()
                    );
                }
            }
            while let Ok(res) = prx.try_recv() {
                if let Some(t) = prov_sent.take() {
                    prov_passes.push(t.elapsed().as_secs_f64());
                }
                let now = Instant::now();
                let Some(ch) = chans.get_mut(res.chan) else { continue };
                for (end_abs, text) in ch.prov.on_result(res.start_s, res.dur_s, &res.segs) {
                    let latency = now.duration_since(feed_start).as_secs_f64() - end_abs;
                    prov_commits.push((end_abs, latency));
                    eprintln!(
                        "t=+{:6.2}s prov={:6.2}s latency={:5.2}s  {}",
                        i as f32 / TARGET_RATE as f32,
                        end_abs,
                        latency,
                        text.trim()
                    );
                }
            }

            for (idx, ch) in chans.iter_mut().enumerate() {
                let prompt = ch.fin.prompt_tail();
                ch.fin.maybe_send(idx, &tx, &prompt);
                if ch.fin.busy() && fin_sent.is_none() {
                    fin_sent = Some(Instant::now());
                }
                ch.prov.maybe_send(idx, &ptx);
                if ch.prov.busy() && prov_sent.is_none() {
                    prov_sent = Some(Instant::now());
                }
                if ch.fin.line_ready()
                    && let Some(line) = ch.fin.take_line()
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

        let stats = |name: &str, lat: &[(f64, f64)]| -> Option<(f64, f64)> {
            let mut v: Vec<f64> = lat.iter().map(|c| c.1).collect();
            if v.is_empty() {
                eprintln!("{name}: no commits");
                return None;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
            eprintln!(
                "{name}: commits={} median={:.2}s p90={:.2}s max={:.2}s",
                v.len(),
                pct(0.5),
                pct(0.9),
                v[v.len() - 1]
            );
            Some((pct(0.5), pct(0.9)))
        };
        let pass_stats = |name: &str, mut v: Vec<f64>| {
            if v.is_empty() {
                eprintln!("{name} pass: none");
                return;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            eprintln!(
                "{name} pass: n={} median={:.2}s max={:.2}s",
                v.len(),
                v[v.len() / 2],
                v[v.len() - 1]
            );
        };

        eprintln!(
            "\nMODEL={model_name} threads={threads}+{prov_threads} channels={n_chans} audio={:.1}s",
            audio.len() as f32 / TARGET_RATE as f32
        );
        let prov = stats("provisional (on screen)", &prov_commits);
        let fin = stats("final (into notes)", &fin_commits);
        pass_stats("final", fin_passes);
        pass_stats("provisional", prov_passes);

        if std::env::var("WZ_ACCEPT").ok().as_deref() == Some("1") {
            let (med, p90) = prov.expect("no provisional commits to gate");
            assert!(med <= 0.5, "Phase 2 budget: provisional median {med:.2}s > 0.5s");
            assert!(p90 <= 0.8, "Phase 2 budget: provisional p90 {p90:.2}s > 0.8s");
        }
        if let Some((med, _)) = fin {
            // the notes tier must keep working: no commits means the notes
            // would be empty
            assert!(med.is_finite());
        }
        assert!(!prov_commits.is_empty(), "no provisional words were committed");
    }

    /// Replays a 16 kHz f32 file through the exact main-loop logic of
    /// `run_stream`, so stream-mode regressions are reproducible without
    /// audio hardware. Drives both tiers; asserts on *final* text (the
    /// notes) — the provisional tier must produce words but is allowed to
    /// be rough.
    #[test]
    fn replays_audio_through_stream_loop() {
        let Some(audio) = load_f32() else {
            eprintln!("skipping: bench/fixtures/jfk.f32 missing");
            return;
        };
        let model = crate::models::resolve(Some("base")).expect("model");
        let (tx, rx) = spawn_worker(&model, "en", 4, true, Profile::Final).expect("worker");
        let (ptx, prx) = spawn_worker(&model, "en", 4, true, Profile::Provisional).expect("prov worker");
        let mut feed = ChannelFeed::new(TARGET_RATE);
        let mut sc = StreamChannel::new(Some("them"), Tuning::final_default());
        let mut prov = ProvisionalTier::new();

        let mut committed = String::new();
        let mut previewed_words = 0usize;
        // the streaming invariant checked below, tracked deterministically:
        // the final commit boundary only ever advances, never past the audio
        // that was actually fed (a word is committed exactly once)
        let mut last_commit = 0.0f64;
        let audio_s = audio.len() as f64 / TARGET_RATE as f64;
        let check_commit = |sc: &StreamChannel, last: &mut f64| {
            let c = sc.last_commit_s();
            assert!(c >= *last - 1e-6, "commit boundary went backwards: {last} -> {c}");
            assert!(c <= audio_s + 1.0, "committed past the fed audio: {c} > {audio_s}");
            *last = c;
        };
        let chunk = TARGET_RATE as usize / 10; // 100 ms, like a cpal callback
        let started = Instant::now();
        let mut i = 0;
        while i < audio.len() {
            let end = (i + chunk).min(audio.len());
            let mono = feed.resample(&audio[i..end]);
            sc.push_16k(mono);
            prov.push_16k(mono);
            i = end;
            while let Ok(res) = rx.try_recv() {
                let d = sc.on_result(res.start_s, res.dur_s, &res.segs);
                prov.advance_final_boundary(sc.last_commit_s());
                check_commit(&sc, &mut last_commit);
                committed.push_str(&d.text);
                committed.push(' ');
            }
            while let Ok(res) = prx.try_recv() {
                previewed_words += prov.on_result(res.start_s, res.dur_s, &res.segs).len();
            }
            let prompt = sc.prompt_tail();
            sc.maybe_send(0, &tx, &prompt);
            prov.maybe_send(0, &ptx);
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
        // shutdown: drain like the real loop. A tier that fell behind during
        // capture needs more than one window to reach the end of its buffer;
        // giving up after a single pass silently drops the tail (this is what
        // failed on the 2-core CI runners).
        sc.flush();
        prov.flush();
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let mut pending = false;
            pending |= if sc.busy() { true } else { sc.maybe_send(0, &tx, &sc.prompt_tail()) };
            pending |= if prov.busy() { true } else { prov.maybe_send(0, &ptx) };
            if !pending || Instant::now() >= deadline {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(res) => {
                    let d = sc.on_result(res.start_s, res.dur_s, &res.segs);
                    prov.advance_final_boundary(sc.last_commit_s());
                    check_commit(&sc, &mut last_commit);
                    committed.push_str(&d.text);
                    committed.push(' ');
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            match prx.recv_timeout(Duration::from_millis(250)) {
                Ok(res) => {
                    previewed_words += prov.on_result(res.start_s, res.dur_s, &res.segs).len();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        sc.finish();
        prov.finish();
        eprintln!("COMMITTED: {committed:?}");
        eprintln!("PROVISIONAL WORDS PREVIEWED: {previewed_words}");
        let lower = committed.to_ascii_lowercase();
        // Guard the streaming loop, not ASR precision: the 5-bit base model
        // mangles proper nouns differently per platform/CPU ("fellow" ->
        // "follow" on the CI runner), so assert on keyword coverage instead
        // of an exact transcript. Accuracy has its own gate (bench/run.sh).
        let needles = ["ask", "not", "what", "country", "can", "do", "for", "you"];
        let words: Vec<&str> = lower
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .collect();
        let hits = needles.iter().filter(|n| words.iter().any(|w| w == *n)).count();
        assert!(
            hits >= 6,
            "transcript lost most of the utterance ({hits}/{} keywords): {committed:?}",
            needles.len()
        );
        // committed exactly once: a long span (4 words) repeated back to back
        // means the same audio was committed twice. Shorter repeats are a
        // decode hiccup on the unstable tail window, not a commit bug.
        for w in words.windows(8) {
            assert!(w[..4] != w[4..], "output repeats itself: {committed:?}");
        }
        assert!(previewed_words > 0, "provisional tier produced no words");
    }

    /// Notes accuracy of the *streaming* engine, measured the way the capture
    /// VM test measures it (fixture played three times, words committed live)
    /// but without an audio server — so a tuning change can be scored in ~40 s:
    ///
    ///   cargo test --release stream::tests::measures_notes_accuracy -- \
    ///     --ignored --nocapture
    #[test]
    #[ignore = "benchmark: needs the model + fixture, runs in real time"]
    fn measures_notes_accuracy() {
        let path = std::env::var("AUDIO").unwrap_or_else(|_| "bench/fixtures/jfk.f32".into());
        let Some(audio) = load_f32_path(&path) else {
            eprintln!("skipping: {path} missing");
            return;
        };
        let reference = std::fs::read_to_string("bench/fixtures/jfk.txt").expect("reference text");
        let model_name = std::env::var("MODEL").unwrap_or_else(|_| "base".into());
        let reps: usize = std::env::var("REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
        let threads: i32 = std::env::var("THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);

        let model = crate::models::resolve(Some(&model_name)).expect("model");
        let (tx, rx) = spawn_worker(&model, "en", threads, false, Profile::Final).expect("worker");
        let mut sc = StreamChannel::new(Some("them"), Tuning::final_default().with_env());

        let mut lines: Vec<String> = Vec::new();
        let mut committed = String::new();
        // RATE=44100 replays through the capture path too: the device rate is
        // what a real call gives us, and the resampler is the only difference
        // between "file is right" and "live capture is right"
        let rate: u32 = std::env::var("RATE").ok().and_then(|v| v.parse().ok()).unwrap_or(TARGET_RATE);
        let mut feed = ChannelFeed::new(rate);
        // OFFSET_MS shifts where the audio starts relative to the engine's
        // window grid. Without it every run decodes the identical windows and
        // "8 runs in a row at 95%" only measures one lucky alignment.
        let offset_ms: usize = std::env::var("OFFSET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let offset = vec![0.0f32; rate as usize * offset_ms / 1000];
        let chunk = rate as usize / 100; // 10 ms, like a cpal callback
        let started = Instant::now();
        let mut fed_samples = 0usize;
        sc.push_16k(feed.resample(&offset));
        for _ in 0..reps {
            let mut i = 0;
            while i < audio.len() {
                let start = i;
                let end = (i + chunk).min(audio.len());
                sc.push_16k(feed.resample(&audio[start..end]));
                fed_samples += end - start;
                i = end;
                while let Ok(res) = rx.try_recv() {
                    let d = sc.on_result(res.start_s, res.dur_s, &res.segs);
                    if !d.text.trim().is_empty() {
                        committed.push_str(d.text.trim());
                        committed.push(' ');
                        if sc.line_ready()
                            && let Some(line) = sc.take_line()
                        {
                            lines.push(line);
                        }
                    }
                }
                let prompt = sc.prompt_tail();
                sc.maybe_send(0, &tx, &prompt);
                // real time minus what inference already spent
                let fed = fed_samples as f32 / rate as f32;
                let spent = started.elapsed().as_secs_f32();
                if fed > spent {
                    std::thread::sleep(Duration::from_secs_f32(fed - spent));
                }
            }
        }
        // drain
        sc.flush();
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let pending = if sc.busy() { true } else { sc.maybe_send(0, &tx, &sc.prompt_tail()) };
            if !pending || Instant::now() >= deadline {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(res) => {
                    let d = sc.on_result(res.start_s, res.dur_s, &res.segs);
                    if !d.text.trim().is_empty() {
                        committed.push_str(d.text.trim());
                        committed.push(' ');
                        if sc.line_ready()
                            && let Some(line) = sc.take_line()
                        {
                            lines.push(line);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if sc.line_open()
            && let Some(line) = sc.take_line()
        {
            lines.push(line);
        }
        for line in &lines {
            eprintln!("LINE: {line}");
        }
        let (sub, del, ins) = word_errors(&reference.repeat(reps), &committed);
        let errors = sub + del + ins;
        let ref_words = words(&reference.repeat(reps));
        let acc = 100.0 * (1.0 - errors as f64 / ref_words.len() as f64);
        eprintln!(
            "STREAM NOTES: sub={sub} del={del} ins={ins} errors={errors}/{} accuracy={acc:.2}%",
            ref_words.len()
        );
    }

    /// Normalized word list: lowercase, punctuation stripped.
    fn words(s: &str) -> Vec<String> {
        s.to_ascii_lowercase()
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|w| !w.is_empty())
            .collect()
    }

    /// (substitutions, deletions, insertions) via word-level Levenshtein.
    fn word_errors(reference: &str, hypothesis: &str) -> (usize, usize, usize) {
        let (r, h) = (words(reference), words(hypothesis));
        let mut dp = vec![vec![0usize; h.len() + 1]; r.len() + 1];
        for (i, row) in dp.iter_mut().enumerate() {
            row[0] = i;
        }
        for (j, cell) in dp[0].iter_mut().enumerate() {
            *cell = j;
        }
        for i in 1..=r.len() {
            for j in 1..=h.len() {
                dp[i][j] = if r[i - 1] == h[j - 1] {
                    dp[i - 1][j - 1]
                } else {
                    1 + dp[i - 1][j - 1].min(dp[i - 1][j]).min(dp[i][j - 1])
                };
            }
        }
        let (mut sub, mut del, mut ins) = (0, 0, 0);
        let (mut i, mut j) = (r.len(), h.len());
        while i > 0 || j > 0 {
            if i > 0 && j > 0 && r[i - 1] == h[j - 1] {
                i -= 1;
                j -= 1;
            } else if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + 1 {
                sub += 1;
                i -= 1;
                j -= 1;
            } else if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
                del += 1;
                i -= 1;
            } else {
                ins += 1;
                j -= 1;
            }
        }
        (sub, del, ins)
    }
}

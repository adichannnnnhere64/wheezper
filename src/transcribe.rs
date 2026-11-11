//! whisper.cpp transcription engine (local, via whisper-rs).

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Transcriber {
    // Shared (Arc) so live-caption workers can create extra states against
    // the same loaded model. State holds a raw pointer into the context;
    // keeping the context alive here keeps that valid for all states.
    ctx: Arc<WhisperContext>,
    state: WhisperState,
    language: Option<String>,
    threads: i32,
}

impl Transcriber {
    pub fn new(model: &Path, language: &str, threads: i32) -> Result<Self> {
        let language = if language.eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(language.to_string())
        };
        let ctx = WhisperContext::new_with_params(
            model.to_str().context("model path is not valid UTF-8")?,
            WhisperContextParameters::default(),
        )
        .context("failed to load whisper model (is the file a valid ggml .bin?)")?;
        let state = ctx.create_state().context("failed to create whisper state")?;
        Ok(Self { ctx: Arc::new(ctx), state, language, threads })
    }

    /// Create an additional whisper state against the same loaded model,
    /// for running live-caption previews on a background thread.
    pub fn new_state(&self) -> Result<WhisperState> {
        self.ctx.create_state().context("failed to create whisper state")
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn threads(&self) -> i32 {
        self.threads
    }

    /// Transcribe one segment (16 kHz mono f32). Returns the joined text.
    /// Filters no-speech segments (whisper's main hallucination path).
    /// Finals use beam search (better word choice); previews stay greedy.
    pub fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String> {
        transcribe_state(
            &mut self.state,
            self.language.as_deref(),
            self.threads,
            samples,
            prompt,
            true,
        )
    }

    /// Cheap greedy variant for live-caption previews.
    pub fn transcribe_greedy(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String> {
        transcribe_state(
            &mut self.state,
            self.language.as_deref(),
            self.threads,
            samples,
            prompt,
            false,
        )
    }

    /// Greedy variant that keeps per-word end times, so the streaming
    /// pipeline can commit each word exactly once, by time (see `stream`).
    /// `profile` selects final (notes quality) vs provisional (live edge).
    pub fn segments_greedy(
        &mut self,
        samples: &[f32],
        prompt: Option<&str>,
        profile: Profile,
    ) -> Result<Vec<Seg>> {
        segments_state(
            &mut self.state,
            self.language.as_deref(),
            self.threads,
            samples,
            prompt,
            false,
            profile,
        )
    }
}

/// Byte-level BPE tokens can carry a fragment of a multi-byte character
/// (whisper tokenises “...” as raw bytes), which shows up as U+FFFD. Collapse
/// such a run between letters back to a plain apostrophe ("doesn’t").
fn clean_token(raw: &str) -> String {
    if !raw.contains('\u{FFFD}') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut pending = 0usize;
    for c in raw.chars() {
        if c == '\u{FFFD}' {
            pending += 1;
            continue;
        }
        if pending > 0 {
            let prev_alnum = out.chars().last().is_some_and(|p| p.is_alphanumeric());
            if prev_alnum && c.is_alphanumeric() {
                out.push('\'');
            }
            pending = 0;
        }
        out.push(c);
    }
    out
}

/// One decoded word: text plus where it ends in the source audio.
#[derive(Clone, Debug)]
pub struct Word {
    /// End of the word, seconds from `samples[0]`.
    pub end_s: f32,
    /// Token text as whisper emits it (leading space included when present).
    pub text: String,
}

/// One whisper segment: its words plus the joined text.
#[derive(Clone, Debug)]
pub struct Seg {
    pub words: Vec<Word>,
    pub text: String,
}

/// Core inference, usable from any state (main engine or caption workers).
/// Note: whisper.cpp pads every call to 30 s internally (the provisional
/// profile caps its encoder window instead — see `Profile`). Encoding
/// dominates that fixed cost, so beam-search decoding on finals is nearly
/// free and measurably better at word choice.
pub fn transcribe_state(
    state: &mut WhisperState,
    language: Option<&str>,
    threads: i32,
    samples: &[f32],
    prompt: Option<&str>,
    beam: bool,
) -> Result<String> {
    let segs = segments_state(state, language, threads, samples, prompt, beam, Profile::Final)?;
    Ok(segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "))
}

/// Decode profile: the final tier is byte-identical to the Phase-1 pipeline
/// (quality first); the provisional tier trades accuracy for speed to hit
/// the 0.5 s live-latency budget. Provisional text is display-only — the
/// final tier supersedes it before anything reaches the notes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    /// Phase-1 parameters, unchanged: full 30 s padded mel, temperature
    /// fallback ladder, rolling decode context, continuity prompt.
    Final,
    /// Fast preview pass on the live edge: tiny encoder window (audio_ctx),
    /// one greedy decode (no fallback ladder — a failed first decode would
    /// retry at up to 21x the cost), bounded token count, and no rolling
    /// context. An explicit domain hint is still applied; the default remains
    /// prompt-free to preserve Phase 2 latency.
    Provisional,
}

/// Core inference keeping per-word end timestamps.
#[allow(clippy::too_many_arguments)]
pub fn segments_state(
    state: &mut WhisperState,
    language: Option<&str>,
    threads: i32,
    samples: &[f32],
    prompt: Option<&str>,
    beam: bool,
    profile: Profile,
) -> Result<Vec<Seg>> {
    let strategy = match (beam, profile) {
        (true, _) => SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 },
        (false, Profile::Provisional) => SamplingStrategy::Greedy { best_of: 1 },
        (false, _) => SamplingStrategy::Greedy { best_of: 5 }
    };
    let mut params = FullParams::new(strategy);
    params.set_language(language);
    params.set_n_threads(threads);
    params.set_translate(false);
    // keep whisper from printing anything itself
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    // per-word timestamps: the streaming pipeline commits words by time, so
    // audio is never re-transcribed and never silently skipped
    params.set_token_timestamps(true);
    // TUNING KNOBS (temporary, for the song benchmark sweep) — final profile
    // only; the provisional profile is a fixed, latency-budgeted decode.
    let envf = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
    if profile == Profile::Provisional {
        let envi = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<i32>().ok());
        params.set_no_context(true);
        // one decode, no temperature ladder (a "failed" first decode would
        // otherwise re-run at up to 21x the cost of the pass itself)
        params.set_temperature_inc(0.0);
        // bound runaway decoding on fragmented audio: hitting the cap still
        // emits the words decoded so far. A 6-token cap cut words mid-decode,
        // which read as fragments ("(", "H") in the preview; 20 tokens covers
        // a 1.2 s window without changing the pass cost (the window is the
        // bound — measured max 0.20 s).
        params.set_max_tokens(envi("WZ_MAXTOK").unwrap_or(20));
        // encoder window in 20 ms frames; the provisional streaming window
        // must stay within audio_ctx*20 ms or audio past it is silently cut.
        // 60 frames = 1.2 s, matching Tuning::provisional().window_s.
        params.set_audio_ctx(envi("WZ_AUDIOCTX").unwrap_or(60));
        if let Some(p) = prompt {
            params.set_initial_prompt(p);
        }
    } else {
        if std::env::var("WZ_NOCTX").ok().as_deref() == Some("1") {
            params.set_no_context(true);
        }
        if let Some(v) = envf("WZ_TEMP") { params.set_temperature_inc(v); }
        if let Some(v) = envf("WZ_ENT") { params.set_entropy_thold(v); }
        if let Some(v) = envf("WZ_LOG") { params.set_logprob_thold(v); }
        if let Some(v) = envf("WZ_NOSP") { params.set_no_speech_thold(v); }
        if let Some(v) = envf("WZ_MAXINIT") { params.set_max_initial_ts(v); }
        if std::env::var("WZ_NOPROMPT").ok().as_deref() == Some("1") {
            // skip prompt entirely (sweep only)
        } else if let Some(p) = prompt {
            // continuity: after a force-cut mid-utterance, seed the decoder
            // with the tail of the previous segment so it doesn't restart a
            // sentence
            params.set_initial_prompt(p);
        }
    }
    if std::env::var("WZ_DEBUG").ok().as_deref() == Some("1") {
        params.set_debug_mode(true);
    }

    state
        .full(params, samples)
        .context("whisper inference failed")?;

    let mut out = Vec::new();
    for i in 0..state.full_n_segments() {
        let Some(seg) = state.get_segment(i) else { continue };
        // High no-speech probability on silence/music => hallucinated text
        if seg.no_speech_probability() > 0.6 {
            continue;
        }
        let mut words = Vec::new();
        for j in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(j) else { continue };
            let Ok(raw) = tok.to_str_lossy() else { continue };
            // special + timestamp tokens ("[_BEG_]", "[_TT_120]") and
            // whisper's own markers ("[MUSIC]", "[INAUDIBLE]") are not text
            let raw = clean_token(&raw);
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('[') || trimmed.ends_with(']') {
                continue;
            }
            let t1 = tok.token_data().t1;
            if t1 < 0 {
                continue;
            }
            words.push(Word { end_s: t1 as f32 / 100.0, text: raw.to_string() });
        }
        let text = words.iter().map(|w| w.text.as_str()).collect::<String>();
        let text = text.trim();
        if text.is_empty() || (text.starts_with('[') && text.ends_with(']')) {
            continue;
        }
        // whisper's non-speech markers, with or without their brackets
        if matches!(
            text.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_uppercase().as_str(),
            "MUSIC" | "BLANK_AUDIO" | "INAUDIBLE" | "APPLAUSE" | "LAUGHTER" | "SILENCE"
        ) {
            continue;
        }
        // whisper's silence placeholder — never a real utterance
        if text == "[BLANK_AUDIO]" {
            continue;
        }
        out.push(Seg { words, text: text.to_string() });
    }
    Ok(out)
}

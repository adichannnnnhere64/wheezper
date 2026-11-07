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
}

/// Core inference, usable from any state (main engine or caption workers).
/// Note: whisper.cpp pads every call to 30 s internally, so each call costs
/// roughly the same wall time regardless of sample count — callers should
/// batch rather than ping. Encoding dominates that fixed cost, so beam-search
/// decoding on finals is nearly free and measurably better at word choice.
pub fn transcribe_state(
    state: &mut WhisperState,
    language: Option<&str>,
    threads: i32,
    samples: &[f32],
    prompt: Option<&str>,
    beam: bool,
) -> Result<String> {
    let strategy = if beam {
        SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 }
    } else {
        SamplingStrategy::Greedy { best_of: 5 }
    };
    let mut params = FullParams::new(strategy);
    params.set_language(language);
    params.set_n_threads(threads);
    params.set_translate(false);
    // keep whisper from printing anything itself
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    // continuity: after a force-cut mid-utterance, seed the decoder with
    // the tail of the previous segment so it doesn't restart a sentence
    if let Some(p) = prompt {
        params.set_initial_prompt(p);
    }

    state
        .full(params, samples)
        .context("whisper inference failed")?;

    let mut out = String::new();
    for i in 0..state.full_n_segments() {
        let Some(seg) = state.get_segment(i) else { continue };
        // High no-speech probability on silence/music => hallucinated text
        if seg.no_speech_probability() > 0.6 {
            continue;
        }
        let text = seg
            .to_str_lossy()
            .context("segment text was not valid UTF-8")?
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        // whisper's silence placeholder — never a real utterance
        if text == "[BLANK_AUDIO]" {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&text);
    }
    Ok(out)
}

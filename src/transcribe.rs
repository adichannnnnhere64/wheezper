//! whisper.cpp transcription engine (local, via whisper-rs).

use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Transcriber {
    // State holds a raw pointer into the context; keep it alive for as long
    // as the state exists (hence the field, despite looking unused).
    _ctx: WhisperContext,
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
        Ok(Self { _ctx: ctx, state, language, threads })
    }

    /// Transcribe one segment (16 kHz mono f32). Returns the joined text.
    /// Filters no-speech segments (whisper's main hallucination path).
    pub fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String> {
        // best_of=5 greedy: noticeably better than greedy-1 at negligible cost
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
        params.set_language(self.language.as_deref());
        params.set_n_threads(self.threads);
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

        self.state
            .full(params, samples)
            .context("whisper inference failed")?;

        let mut out = String::new();
        for i in 0..self.state.full_n_segments() {
            let Some(seg) = self.state.get_segment(i) else { continue };
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
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&text);
        }
        Ok(out)
    }
}

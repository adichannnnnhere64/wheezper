//! Neural voice-activity gate (Silero VAD via whisper.cpp).
//!
//! The RMS gate in `segmenter` is cheap and low-latency but dumb: call
//! comfort noise, loopback dither, music tails and fan hum all pass it —
//! and whisper, fed that near-silence, hallucinates ("The animals are not
//! moving in", "Thank you."). A one-pass Silero check on each *closed*
//! segment blocks those from ever reaching whisper — better transcripts
//! AND less CPU (silence never pays the 30 s-window inference tax).
//!
//! Fails open: if the VAD model can't load or a call errors, segments pass
//! through and whisper's own no-speech filter remains the only guard.

use crate::models;
use anyhow::{anyhow, Context, Result};
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

/// Minimum plausible speech duration (in samples) worth running VAD on.
const MIN_INPUT: usize = 1600; // 100 ms @ 16 kHz

pub struct Vad {
    ctx: WhisperVadContext,
    params: WhisperVadParams,
}

impl Vad {
    /// Load the Silero VAD model, downloading it (~0.9 MB) on first use.
    pub fn new(threads: i32) -> Result<Self> {
        let model = models::resolve_vad().context("resolving VAD model")?;
        let mut ctx_params = WhisperVadContextParams::new();
        ctx_params.set_n_threads(threads.max(1) as i32);
        let ctx = WhisperVadContext::new(
            model.to_str().context("VAD model path is not valid UTF-8")?,
            ctx_params,
        )
        .map_err(|e| anyhow!("failed to load VAD model (is the file a valid ggml VAD model?): {e:?}"))?;
        let mut params = WhisperVadParams::new();
        // close segments on 100 ms silences; pad speech lightly for context
        params.set_min_silence_duration(100);
        params.set_speech_pad(30);
        Ok(Self { ctx, params })
    }

    /// True when `samples` contains at least `min_speech_s` seconds of
    /// actual human speech. VAD errors fail open (returns true).
    pub fn has_speech(&mut self, samples: &[f32], min_speech_s: f32) -> bool {
        if samples.len() < MIN_INPUT {
            return false;
        }
        let Ok(segments) = self.ctx.segments_from_samples(self.params, samples) else {
            return true; // fail open: let whisper decide
        };
        let n = segments.num_segments().max(0) as usize;
        let mut total = 0.0f32;
        for i in 0..n {
            if let (Some(t0), Some(t1)) = (
                segments.get_segment_start_timestamp(i as i32),
                segments.get_segment_end_timestamp(i as i32),
            ) {
                total += (t1 - t0).max(0.0);
            }
        }
        total >= min_speech_s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_short_is_never_speech() {
        // needs the Silero model (downloaded on first use); skip offline
        let Ok(mut v) = Vad::new(1) else {
            eprintln!("skipping: VAD model unavailable");
            return;
        };
        assert!(!v.has_speech(&[0.0; 800], 0.0)); // 50 ms: below MIN_INPUT
    }
}

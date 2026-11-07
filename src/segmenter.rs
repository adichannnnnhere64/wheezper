//! Utterance segmenter: energy-gated chunking of the raw sample stream.
//!
//! Splits audio into segments that whisper can transcribe well:
//! - close a segment after `min_silence` seconds of (near) silence
//! - drop blips shorter than `min_segment`
//! - force-close segments longer than `max_segment`
//! - keep a small lead/trailing overlap so words aren't clipped at boundaries

pub struct Segmenter {
    frame_len: usize,          // samples per 30 ms frame
    gate_rms: f32,             // RMS at or above this counts as speech
    min_silence_frames: usize, // trailing silence to close a segment
    min_samples: usize,        // minimum segment length (speech content)
    max_samples: usize,        // force-close threshold
    keep_frames: usize,        // trailing silence kept on emit / lead into next

    buf: Vec<f32>,
    silent_run: usize,   // consecutive silent frames currently in buf
    speech_seen: bool,   // any speech in current buf since last reset
}

impl Segmenter {
    pub fn new(rate: u32, threshold_db: f32, min_silence: f32, min_segment: f32, max_segment: f32) -> Self {
        let frame_len = ((rate as f32 * 0.03).round() as usize).max(1); // 30 ms
        // dBFS -> linear RMS
        let gate_rms = 10f32.powf(threshold_db / 20.0);
        let f = |secs: f32| (secs * rate as f32).round() as usize;
        let keep_frames = 8; // ~0.24 s
        Self {
            frame_len,
            gate_rms,
            min_silence_frames: ((min_silence / 0.03).ceil() as usize).max(1),
            min_samples: f(min_segment),
            max_samples: f(max_segment).max(f(min_segment)),
            keep_frames,
            buf: Vec::new(),
            silent_run: 0,
            speech_seen: false,
        }
    }

    /// Samples per 30 ms frame (exposed so callers size their read buffers).
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// Feed one 30 ms frame. Returns a completed segment when one closes.
    pub fn feed(&mut self, frame: &[f32]) -> Option<Vec<f32>> {
        debug_assert_eq!(frame.len(), self.frame_len);

        let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();
        let speech = rms >= self.gate_rms;
        if speech {
            self.speech_seen = true;
            self.silent_run = 0;
        } else {
            self.silent_run += 1;
        }
        self.buf.extend_from_slice(frame);

        if self.buf.len() >= self.max_samples {
            return Some(self.emit_mid_speech());
        }

        if self.silent_run >= self.min_silence_frames {
            let speech_len = self.buf.len() - self.silent_run * self.frame_len;
            if speech_len >= self.min_samples {
                return Some(self.emit_on_silence());
            }
            // Too short: isolated noise blip — discard, keep a lead context.
            self.reset_to_lead();
        }
        None
    }

    /// EOF: emit whatever is pending, but only if it ever got loud. A buffer
    /// of marginal noise (system loopback dither, a decork pop) can hold a
    /// few above-gate frames — flushing it just feeds whisper garbage.
    pub fn flush(&mut self) -> Option<Vec<f32>> {
        let pending = std::mem::take(&mut self.buf);
        let had_speech = self.speech_seen;
        self.silent_run = 0;
        self.speech_seen = false;
        let loud_enough = pending.iter().fold(0f32, |p, &x| p.max(x.abs())) >= self.gate_rms * 4.0;
        if had_speech && loud_enough && pending.len() >= self.min_samples {
            return Some(pending);
        }
        None
    }

    /// Cut on trailing silence: keep a little trailing silence in the segment
    /// (whisper likes some padding) and a little lead for the next one.
    fn emit_on_silence(&mut self) -> Vec<f32> {
        let cut_silence = self.silent_run.saturating_sub(self.keep_frames) * self.frame_len;
        let lead = self.keep_frames * self.frame_len;
        let boundary = self.buf.len() - cut_silence;
        let seg = self.buf.drain(..boundary).collect::<Vec<f32>>();
        let keep_from = seg.len().saturating_sub(lead);
        let lead_samples: Vec<f32> = seg[keep_from..].to_vec();
        self.buf = lead_samples;
        self.silent_run = 0;
        // silence still trailing inside buf counts toward the next silent_run
        self.silent_run = self.keep_frames.min(self.min_silence_frames.saturating_sub(1));
        self.speech_seen = false;
        seg
    }

    /// Force-cut at max length, mid-utterance. Keep lead overlap.
    fn emit_mid_speech(&mut self) -> Vec<f32> {
        let lead = self.keep_frames * self.frame_len;
        let boundary = self.buf.len() - lead;
        let seg = self.buf.drain(..boundary).collect::<Vec<f32>>();
        self.silent_run = 0;
        self.speech_seen = true; // still mid-utterance: force-cut, not silence
        seg
    }

    fn reset_to_lead(&mut self) {
        let lead = self.keep_frames * self.frame_len;
        let from = self.buf.len().saturating_sub(lead);
        self.buf.drain(..from);
        self.silent_run = self.keep_frames.min(self.min_silence_frames.saturating_sub(1));
        self.speech_seen = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(rate: u32) -> Segmenter {
        Segmenter::new(rate, -40.0, 0.6, 0.5, 12.0)
    }

    fn frame(s: &mut Segmenter, amplitude: f32) -> Option<Vec<f32>> {
        let v = vec![amplitude; s.frame_len()];
        s.feed(&v)
    }

    #[test]
    fn silence_is_dropped() {
        let mut s = seg(16000);
        for _ in 0..100 {
            assert!(frame(&mut s, 0.0).is_none());
        }
        assert!(s.flush().is_none());
    }

    #[test]
    fn speech_then_silence_emits_once() {
        let mut s = seg(16000);
        for _ in 0..100 {
            // ~3 s speech
            assert!(frame(&mut s, 0.2).is_none());
        }
        let mut emitted = Vec::new();
        for _ in 0..30 {
            // ~0.9 s silence > min_silence
            if let Some(seg) = frame(&mut s, 0.0) {
                emitted.push(seg);
            }
        }
        assert_eq!(emitted.len(), 1);
        assert!(emitted[0].len() > 0);
        // trailing silence was trimmed but not entirely
        assert!(s.flush().is_none());
    }

    #[test]
    fn short_blip_is_discarded() {
        let mut s = seg(16000);
        for _ in 0..5 {
            // 150 ms of speech < min_segment
            assert!(frame(&mut s, 0.5).is_none());
        }
        for _ in 0..30 {
            assert!(frame(&mut s, 0.0).is_none());
        }
        assert!(s.flush().is_none(), "isolated blip must not be emitted");
    }

    #[test]
    fn max_length_forces_cut() {
        let mut s = seg(16000);
        let mut cuts = 0;
        // 15 s of continuous speech; max is 12 s -> exactly one force cut
        for _ in 0..(15 * 100 / 3) {
            if frame(&mut s, 0.2).is_some() {
                cuts += 1;
            }
        }
        assert_eq!(cuts, 1);
        assert!(s.flush().is_some(), "remaining speech must flush at EOF");
    }

    #[test]
    fn flush_drops_marginal_noise_buffer() {
        // system loopback dither/decork noise: a few frames barely above the
        // gate accumulate for seconds; flushing them must not feed whisper.
        let mut s = seg(16000);
        for _ in 0..300 {
            // 9 s of dither straddling the gate (peak 0.015 < 4x gate)
            let _ = frame(&mut s, 0.012);
        }
        assert!(s.flush().is_none(), "marginal-noise buffer must not flush as speech");
    }

    #[test]
    fn flush_keeps_real_speech() {
        let mut s = seg(16000);
        for _ in 0..60 {
            let _ = frame(&mut s, 0.2);
        }
        assert!(s.flush().is_some(), "real speech pending at EOF must flush");
    }

    #[test]
    fn flush_emits_pending_speech() {
        let mut s = seg(16000);
        for _ in 0..60 {
            // 1.8 s speech, never followed by silence
            assert!(frame(&mut s, 0.2).is_none());
        }
        let pending = s.flush().expect("speech pending at EOF must be emitted");
        assert!(pending.len() >= s.min_samples);
        assert!(s.flush().is_none(), "double flush must not re-emit");
    }
}

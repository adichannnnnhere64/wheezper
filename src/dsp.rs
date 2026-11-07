//! DSP: channel mixdown, streaming resampling and framing.
//!
//! Device streams come in at 44.1/48 kHz stereo; whisper wants 16 kHz mono.
//! A streaming linear resampler is plenty for speech features and keeps this
//! dependency-free and incremental.

/// Average interleaved multi-channel f32 samples down to mono.
pub fn mix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 | 1 => samples.to_vec(),
        ch => samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect(),
    }
}

/// Incremental linear resampler (mono, arbitrary in/out rates).
///
/// Emits output samples at fractional positions between input samples,
/// interpolating linearly. The first input sample only primes the state,
/// so output count is `(n - 1) * out_rate / in_rate` (± 1).
pub struct Resampler {
    step: f64, // input samples per output sample
    pos: f64,   // next output position, in input-sample units
    prev: f32,  // last input sample
    idx: u64,   // index of the last input sample
    started: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 1.0,
            prev: 0.0,
            idx: 0,
            started: false,
        }
    }

    /// Push one input sample; appends any due output samples to `out`.
    pub fn push(&mut self, s: f32, out: &mut Vec<f32>) {
        if !self.started {
            self.prev = s;
            self.started = true;
            return;
        }
        self.idx += 1; // s is sample number `idx` (1-based after priming)
        while self.pos <= self.idx as f64 {
            let frac = (self.pos - (self.idx as f64 - 1.0)) as f32;
            out.push(self.prev + (s - self.prev) * frac);
            self.pos += self.step;
        }
        self.prev = s;
    }

    /// Convenience: push a slice of mono input samples.
    pub fn push_slice(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        for &s in samples {
            self.push(s, out);
        }
    }
}

/// Accumulates a sample stream and hands out fixed-size frames (30 ms).
pub struct Framer {
    frame_len: usize,
    buf: Vec<f32>,
}

impl Framer {
    pub fn new(frame_len: usize) -> Self {
        Self { frame_len, buf: Vec::new() }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    /// Next complete frame, if one is ready.
    pub fn next_frame(&mut self) -> Option<Vec<f32>> {
        if self.buf.len() >= self.frame_len {
            Some(self.buf.drain(..self.frame_len).collect())
        } else {
            None
        }
    }

    /// Remaining partial frame, zero-padded to frame length (for EOF).
    pub fn finish(&mut self) -> Option<Vec<f32>> {
        if self.buf.is_empty() {
            return None;
        }
        self.buf.resize(self.frame_len, 0.0);
        Some(std::mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixdown_averages_channels() {
        let stereo = [0.0, 1.0, 0.5, -0.5, 1.0, 1.0];
        assert_eq!(mix_to_mono(&stereo, 2), vec![0.5, 0.0, 1.0]);
        assert_eq!(mix_to_mono(&stereo[..3], 1), stereo[..3].to_vec());
    }

    #[test]
    fn identity_rate_passes_samples_through() {
        let mut r = Resampler::new(16000, 16000);
        let mut out = Vec::new();
        for i in 0..1000 {
            r.push(i as f32 * 0.01, &mut out);
        }
        assert_eq!(out.len(), 999);
        for k in 0..999 {
            assert!((out[k] - (k + 1) as f32 * 0.01).abs() < 1e-6);
        }
    }

    #[test]
    fn downsample_count_is_proportional() {
        let mut r = Resampler::new(48000, 16000);
        let mut out = Vec::new();
        for i in 0..48000 {
            r.push((i as f32 * 0.001).sin(), &mut out);
        }
        // 48000 in, first primes -> 15999 ± 1 out
        assert!((out.len() as i64 - 15999).abs() <= 1, "len={}", out.len());
    }

    #[test]
    fn upsample_count_is_proportional() {
        let mut r = Resampler::new(8000, 16000);
        let mut out = Vec::new();
        for i in 0..8000 {
            r.push(1.0, &mut out);
        }
        // 8000 in, first primes -> 15997 out (± interpolation drift)
        assert!((out.len() as i64 - 15997).abs() <= 2, "len={}", out.len());
        // constant input -> constant output
        assert!(out.iter().all(|&x| (x - 1.0).abs() < 1e-6));
    }

    #[test]
    fn framer_emits_fixed_frames_and_pads_tail() {
        let mut f = Framer::new(4);
        f.push(&[1.0, 2.0, 3.0]);
        assert!(f.next_frame().is_none());
        f.push(&[4.0, 5.0, 6.0, 7.0, 8.0]);
        assert_eq!(f.next_frame().unwrap(), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(f.next_frame().unwrap(), vec![5.0, 6.0, 7.0, 8.0]);
        assert!(f.next_frame().is_none());
        f.push(&[9.0]);
        assert_eq!(f.finish().unwrap(), vec![9.0, 0.0, 0.0, 0.0]);
        assert!(f.finish().is_none());
    }
}

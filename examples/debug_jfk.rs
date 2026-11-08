// debug: run whisper directly on the jfk sample, dump segment details
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn main() -> anyhow::Result<()> {
    let bytes = std::fs::read("bench/fixtures/jfk.f32")?;
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let ctx = WhisperContext::new_with_params(
        "models/ggml-base-q5_1.bin",
        WhisperContextParameters::default(),
    )?;
    let mut state = ctx.create_state()?;
    let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    p.set_language(Some("en"));

    state.full(p, &samples)?;
    println!("n_segments = {}", state.full_n_segments());
    for i in 0..state.full_n_segments() {
        let s = state.get_segment(i).unwrap();
        println!(
            "seg {i}: no_speech={:.3} text={:?}",
            s.no_speech_probability(),
            s.to_str_lossy()?
        );
    }
    Ok(())
}

// Embeds the wheezper-llm worker binary (built by the llm-worker workspace
// member) into wheezper itself, so distribution stays a single file.
//
// whisper.cpp and llama.cpp both bundle their own ggml and cannot be linked
// into one executable, so the LLM runs in a helper process. For the embed to
// happen, point WHEEZPER_LLM_WORKER at the built worker binary:
//
//   cargo build --release -p llm-worker
//   WHEEZPER_LLM_WORKER=target/release/wheezper-llm cargo build --release
//
// Without the env var (plain `cargo build`), wheezper falls back to looking
// for `wheezper-llm` next to its own executable (same target dir).

use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=WHEEZPER_LLM_WORKER");
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("wheezper-llm.bin");
    match env::var("WHEEZPER_LLM_WORKER") {
        Ok(src) if Path::new(&src).exists() => {
            let bytes = std::fs::read(&src)
                .unwrap_or_else(|e| panic!("reading {src}: {e}"));
            std::fs::write(&dest, &bytes)
                .unwrap_or_else(|e| panic!("writing {}: {e}", dest.display()));
            println!("cargo:warning=embedded llm worker: {src} ({} KB)", bytes.len() / 1024);
        }
        _ => {
            // empty placeholder: not embedded; runtime falls back to a sibling binary
            std::fs::write(&dest, b"").unwrap();
        }
    }
}

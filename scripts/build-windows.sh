#!/usr/bin/env bash
# Cross-compile wheezper.exe for Windows 10/11 (x86_64, static CRT).
#
# Two steps, because whisper.cpp and llama.cpp both bundle their own ggml and
# cannot be linked into one executable: the LLM runs in a helper binary
# (wheezper-llm) which is then EMBEDDED into wheezper.exe — the result is a
# single portable file with zero DLL dependencies beyond what ships with
# Windows itself (no VC++ redistributable, no Ollama install).
#
# Cross builds use their own CARGO_TARGET_DIR (target-xwin) to avoid mixing
# artifacts from two rustc versions on NixOS (rustup toolchain vs nix rustc).
set -euo pipefail
cd "$(dirname "$0")/.."

export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="target-xwin"
export LLAMA_STATIC_CRT=1

rustup target add x86_64-pc-windows-msvc

# 1) the LLM worker (llama.cpp only — no whisper conflict)
RUSTFLAGS="-C target-feature=+crt-static" \
    cargo xwin build --release -p llm-worker --target x86_64-pc-windows-msvc

# 2) wheezper with the worker embedded
WHEEZPER_LLM_WORKER="target-xwin/x86_64-pc-windows-msvc/release/wheezper-llm.exe" \
RUSTFLAGS="-C target-feature=+crt-static" \
    cargo xwin build --release --features sync --target x86_64-pc-windows-msvc

echo
echo "=> target-xwin/x86_64-pc-windows-msvc/release/wheezper.exe (worker embedded)"
llvm-objdump -p target-xwin/x86_64-pc-windows-msvc/release/wheezper.exe | grep "DLL Name"

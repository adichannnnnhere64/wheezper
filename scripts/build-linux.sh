#!/usr/bin/env bash
# Build a portable x86_64 Linux binary of wheezper.
#
# Strategy: build inside an Ubuntu 22.04 LTS container (glibc 2.35) so the
# resulting binary runs on every glibc Linux released since ~2022 (Ubuntu 22.04+,
# Debian 12+, Fedora 37+, Arch, ...). In practice the *required* glibc symbols
# are usually much older than 2.35 (see the check at the end), so coverage is
# even wider. The only runtime dependency left is libasound.so.2 (ALSA), which
# every desktop Linux ships.
#
# (debian:bullseye would give glibc 2.31, but it is EOL and its apt repos 404.)
#
# The LLM worker is embedded (same as build-windows.sh), so the result is a
# single file. Uses its own CARGO_TARGET_DIR so it never clobbers the native
# NixOS build (target/) or the Windows cross build (target-xwin/).
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="wheezper-linux-builder:ubuntu2204"
TARGET_DIR="target-linux-portable"

docker build -t "$IMAGE" - <<'DOCKERFILE'
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config cmake clang libclang-dev \
        libasound2-dev libpulse-dev ninja-build git ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Rust 1.85+ (the crate uses edition 2024)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app
DOCKERFILE

docker run --rm \
    -v "$PWD:/app" \
    -v wheezper-cargo-registry:/root/.cargo/registry \
    -v wheezper-cargo-git:/root/.cargo/git \
    "$IMAGE" bash -c '
        set -euo pipefail
        export CARGO_TARGET_DIR=/app/'"$TARGET_DIR"'
        export LIBCLANG_PATH="$(dirname "$(find /usr/lib -name "libclang.so*" 2>/dev/null | head -1)")"
        export GGML_NATIVE=OFF   # portable: no -march=native in whisper.cpp/ggml
        echo "LIBCLANG_PATH=$LIBCLANG_PATH"

        echo "==> building llm-worker"
        cargo build --release -p llm-worker

        echo "==> building wheezper (worker embedded)"
        WHEEZPER_LLM_WORKER=/app/'"$TARGET_DIR"'/release/wheezper-llm \
            cargo build --release --features sync
    '

BIN="$TARGET_DIR/release/wheezper"
echo
echo "=> $BIN"
echo
echo "runtime dependencies (DT_NEEDED):"
readelf -d "$BIN" | grep NEEDED || true
echo
echo "highest glibc symbol version required: $(objdump -T "$BIN" | grep -oE 'GLIBC_[0-9.]+' | sort -V | tail -1)"
echo "highest libstdc++ symbol version required: $(objdump -T "$BIN" | grep -oE 'GLIBCXX_[0-9.]+' | sort -V | tail -1)"

tar -C "$TARGET_DIR/release" -czf "wheezper-linux-amd64.tar.gz" wheezper
echo
echo "packed: wheezper-linux-amd64.tar.gz"

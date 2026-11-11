#!/usr/bin/env bash
# Build and smoke-test wheezper inside a Termux userspace (Android).
#
#   scripts/termux-build.sh            # run *inside* Termux
#   docker run --rm -v "$PWD:/src:ro" -v "$PWD/scripts/termux-build.sh:/build.sh:ro" \
#     termux/termux-docker:x86_64 bash /build.sh
#
# The built-in LLM is disabled (`--no-default-features`): llama.cpp needs the
# Android NDK, and on a phone you would use `--ollama <server>` anyway.
#
# Note on audio: Termux userspaces (the docker image, or a chroot) have no
# Android system libraries, so `libaaudio.so` is missing and linking fails.
# That library exists on every real Android >= 8 device. When it is missing we
# link against a generated stub (the AAudio entry points are never called on
# the stdin path) so the *build* is still validated; capture has to be tested
# on a device.
set -euo pipefail

src="${SRC:-/src}"
work="${HOME}/wheezper"

echo "== copying sources to $work"
mkdir -p "$work"
cd "$src"
# proto + server are workspace members: cargo resolves the whole manifest even
# when only the CLI is built.
tar cf - Cargo.toml Cargo.lock build.rs src vendor llm-worker proto server bench/fixtures | tar xf - -C "$work"
cd "$work"

echo "== packages"
pkg update -y > /dev/null
pkg install -y rust binutils cmake clang > /dev/null
rustc --version
clang --version | head -1

export LIBCLANG_PATH="$PREFIX/lib"
export GGML_NATIVE=OFF # portable: no -march=native for ggml

stub="$PREFIX/lib/libaaudio.so"
if [ ! -e "$stub" ]; then
  echo
  echo "== no Android system audio libraries in this userspace; linking a stub"
  # the entry points come from the ndk-sys crate that cpal pulls in for the
  # Android target, so make sure the sources are present before grepping
  cargo fetch > /dev/null 2>&1 || true
  syms=$(grep -rhoE 'pub fn AAudio[A-Za-z0-9_]+' "$HOME"/.cargo/registry/src/*/ndk-sys-*/src/*.rs \
           | awk '{print $3}' | sort -u || true)
  [ -n "$syms" ] || { echo "could not determine the AAudio entry points"; exit 1; }
  {
    echo '/* generated stub: AAudio only exists on a real Android device */'
    for s in $syms; do echo "void ${s}(void) {}"; done
  } > "$TMPDIR/aaudio-stub.c"
  cc -shared -fPIC -o "$stub" "$TMPDIR/aaudio-stub.c"
  echo "$(echo "$syms" | wc -l) AAudio entry points stubbed at $stub"
fi

echo
echo "== build"
# -p wheezper: the workspace also holds the gRPC server, which is not a Termux
# build target (and would drag tokio/sqlx into a phone build).
cargo build --release --no-default-features -p wheezper
ls -la target/release/wheezper
./target/release/wheezper --version

echo
echo "== transcribe the committed speech fixture (stdin path, no audio hardware)"
notes="$TMPDIR/termux-notes.md"
log="$TMPDIR/termux-run.log"
rm -f "$notes"
./target/release/wheezper --source stdin --model tiny --language en --no-vad \
    --notes "$notes" < bench/fixtures/jfk.f32 > "$log" 2>&1 \
  || { echo "wheezper run failed:"; tail -20 "$log"; exit 1; }
cat "$notes"
grep -qiE 'fellow americans|ask not' "$notes" \
  || { echo "transcript does not match the fixture"; exit 1; }

echo
echo "PASS: wheezper builds and transcribes under Termux ($(rustc --version | cut -d' ' -f2))"

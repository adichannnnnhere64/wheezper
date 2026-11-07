#!/usr/bin/env bash
# Cross-compile wheezper.exe for Windows 10/11 (x86_64, static CRT).
#
# Requires (all in shell.nix): rustup + stable toolchain with the
# x86_64-pc-windows-msvc target, cargo-xwin, clang, cmake, ninja, llvm.
# The result is a single portable exe with zero DLL dependencies beyond
# what ships with Windows itself (no VC++ redistributable needed).
set -euo pipefail
cd "$(dirname "$0")/.."

export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"

rustup target add x86_64-pc-windows-msvc
RUSTFLAGS="-C target-feature=+crt-static" \
    cargo xwin build --target x86_64-pc-windows-msvc --release

echo
echo "=> target/x86_64-pc-windows-msvc/release/wheezper.exe"
llvm-objdump -p target/x86_64-pc-windows-msvc/release/wheezper.exe | grep "DLL Name"

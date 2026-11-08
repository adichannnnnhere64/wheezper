#!/usr/bin/env bash
# End-to-end test of a *shipped* Linux artifact, from a scratch directory.
#
#   scripts/vmtest-linux.sh [wheezper-linux-amd64.tar.gz]
#
# Unpacks the tarball into a temp dir, checks the binary runs (no nix-store
# paths, only the documented runtime library), then transcribes the committed
# speech fixture and gates on accuracy. Nothing from the build tree is used
# except the fixture and the scorer, so this is what a user actually gets.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

tarball="${1:-wheezper-linux-amd64.tar.gz}"
model="${MODEL:-base}"
min_accuracy="${MIN_ACCURACY:-90}"

[ -f "$tarball" ] || { echo "missing artifact: $tarball" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tar -C "$work" -xzf "$tarball"
bin="$work/wheezper"
[ -x "$bin" ] || { echo "tarball does not contain an executable 'wheezper'" >&2; exit 1; }

echo "==> running $("$bin" --version 2>&1 | head -1) from $work"

echo "==> runtime dependencies"
readelf -d "$bin" | grep NEEDED
# Developer machines building with Nix legitimately carry nix-store RPATHs;
# release artifacts must not (set ALLOW_NIX_RPATH=1 only for local runs).
if [ "${ALLOW_NIX_RPATH:-0}" != "1" ] && readelf -d "$bin" | grep -q '/nix/store'; then
  echo "ERROR: nix-store path leaked into the shipped binary" >&2
  exit 1
fi
if [ "${ALLOW_NIX_RPATH:-0}" != "1" ] && ! readelf -l "$bin" | grep -q 'interpreter: /lib64/ld-linux-x86-64.so.2'; then
  echo "ERROR: unexpected ELF interpreter (portability)" >&2
  readelf -l "$bin" | grep -i interpreter
  exit 1
fi
missing=0
while read -r lib; do
  case "$lib" in
    libc.so.6|libm.so.6|libgcc_s.so.1|libstdc++.so.6|libasound.so.2|ld-linux-x86-64.so.2) ;;
    *) echo "ERROR: unexpected runtime dependency: $lib" >&2; missing=1 ;;
  esac
done < <(readelf -d "$bin" | grep NEEDED | sed -E 's/.*\[(.*)\]/\1/')
[ "$missing" -eq 0 ] || exit 1

echo "==> CLI"
"$bin" --help > /dev/null
"$bin" --list-devices > /dev/null 2>&1 || echo "note: --list-devices unavailable in this VM (no audio server)"

echo "==> transcription + accuracy gate (from the unpacked artifact)"
mkdir -p "$work/bench/fixtures"
cp "$root/bench/run.sh" "$root/bench/score.py" "$work/bench/"
cp "$root/bench/fixtures/jfk.f32" "$root/bench/fixtures/jfk.txt" "$work/bench/fixtures/"
cd "$work"
MIN_ACCURACY="$min_accuracy" \
  ./bench/run.sh bench/fixtures/jfk.f32 bench/fixtures/jfk.txt \
  --model "$model" --language en --no-vad

echo "PASS: shipped Linux artifact transcribes and scores correctly"

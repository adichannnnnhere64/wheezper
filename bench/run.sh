#!/usr/bin/env bash
# Run wheezper over an audio file and score the transcript against a reference.
#
#   bench/run.sh <audio> <reference> [extra wheezper args...]
#
# <audio>: any format ffmpeg can read, or a raw 16 kHz mono f32le file (*.f32).
# <reference>: plain-text reference (lyrics, or a sentence for the CI fixture).
#
# Environment:
#   MIN_ACCURACY=90   fail (exit 1) if word accuracy is below this percentage
#   TAG=name          prefix the output files, so runs do not overwrite each other
#   WHEEZPER=path     binary to run (default: ./wheezper, else target/release/)
#   PYTHON=python3    interpreter for the scorer (CI Windows uses "python")
#
# Outputs (git-ignored): bench/out/<tag.><audio>.md   transcript/notepad
#                        bench/out/<tag.><audio>.log  wheezper stderr+stdout
set -euo pipefail
cd "$(dirname "$0")/.."

audio="$1"; shift
reference="$1"; shift
[ -f "$audio" ]     || { echo "missing audio: $audio" >&2; exit 2; }
[ -f "$reference" ] || { echo "missing reference text: $reference" >&2; exit 2; }

tag="$(basename "${audio%.*}")"
tag="${TAG:+$TAG.}${tag:0:40}"
out_dir="bench/out"
mkdir -p "$out_dir"
notes="$out_dir/$tag.md"
log="$out_dir/$tag.log"

case "$audio" in
  *.f32)
    # already 16 kHz mono f32le (whisper's input format)
    pcm="$audio"
    ;;
  *)
    pcm="$out_dir/$tag.f32"
    ffmpeg -hide_banner -loglevel error -y -i "$audio" -ac 1 -ar 16000 -f f32le "$pcm"
    ;;
esac

if [ -n "${WHEEZPER:-}" ]; then
  bin="$WHEEZPER"
elif [ -x ./wheezper ]; then
  bin=./wheezper
elif [ -x target/release/wheezper ]; then
  bin=target/release/wheezper
elif [ -x target/release/wheezper.exe ]; then
  bin=target/release/wheezper.exe
else
  echo "no wheezper binary: build it, or set WHEEZPER=/path/to/wheezper" >&2
  exit 2
fi
python="${PYTHON:-python3}"

rm -f "$notes"
start=$SECONDS
"$bin" --source stdin --notes "$notes" "$@" < "$pcm" > "$log" 2>&1 || {
  echo "wheezper failed:"; tail -20 "$log"; exit 1
}
echo "binary=$bin"
echo "wall=$((SECONDS - start))s"
grep -E "^wheezper: (model|lang|vad|source|lyrics)" "$log" || true

score_args=()
if [ -n "${MIN_ACCURACY:-}" ]; then
  score_args+=(--min-accuracy "$MIN_ACCURACY")
fi
"$python" bench/score.py "$reference" "$notes" "${score_args[@]}"

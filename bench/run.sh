#!/usr/bin/env bash
# Run wheezper over a song/wav/opus and score it against reference lyrics.
#
#   bench/run.sh <audio> <lyrics> [extra wheezper args...]
#
# Decodes to 16 kHz mono f32 (what whisper wants), runs the file through the
# stdin path with a temp notepad, prints the score. All wheezper output is
# kept next to the transcript for eyeballing.
set -euo pipefail
cd "$(dirname "$0")/.."

audio="$1"; shift
lyrics="$1"; shift

tag="$(basename "${audio%.*}")"
tag="${TAG:+$TAG.}${tag:0:40}"
out_dir="bench/out"
mkdir -p "$out_dir"
wav="$out_dir/$tag.f32"
notes="$out_dir/$tag.md"
log="$out_dir/$tag.log"

ffmpeg -hide_banner -loglevel error -y -i "$audio" -ac 1 -ar 16000 -f f32le "$wav"
rm -f "$notes"

start=$SECONDS
./wheezper --source stdin --notes "$notes" "$@" < "$wav" > "$log" 2>&1 || true
echo "wall=$((SECONDS - start))s"
grep -E "^wheezper: (model|lang|vad|source)" "$log" || true

python3 bench/score.py "$lyrics" "$notes"

#!/usr/bin/env bash
# parameter sweep on Backburner
set -u
cd "$(dirname "$0")/.."
S="NIKI -  Backburner (Official Lyric Video) [qYwsPx-ryxk].opus"
L=backburner.lyrics
run() {
  local tag="$1"; shift
  echo "=== $tag :: noctx=${WZ_NOCTX:-0} args: $*"
  TAG="$tag" ./bench/run.sh "$S" "$L" --language en "$@" 2>&1 | grep -E "^(wall|sub=|WER|accuracy)"
  echo
}
unset WZ_NOCTX
run b_base_ctx --model base --no-vad
export WZ_NOCTX=1
run b_base_noctx --model base --no-vad
run b_base_noctx_seg25 --model base --no-vad --max-segment 25 --min-silence 0.4
run b_small_noctx_seg25 --model small --no-vad --max-segment 25 --min-silence 0.4
unset WZ_NOCTX
run b_small_seg25_vad --model small --max-segment 25 --min-silence 0.4

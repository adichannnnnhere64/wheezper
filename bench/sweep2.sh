#!/usr/bin/env bash
set -u
cd "$(dirname "$0")/.."
S="NIKI -  Backburner (Official Lyric Video) [qYwsPx-ryxk].opus"
L=backburner.lyrics
run() {
  local tag="$1"; shift
  echo "=== $tag : prompt=$([ "${WZ_NOPROMPT:-0}" = 1 ] && echo off || echo on) nosp=${WZ_NOSP:-d} $*"
  TAG="$tag" ./bench/run.sh "$S" "$L" --language en "$@" 2>&1 | grep -E "^(wall|sub=|WER|accuracy)"
  echo
}
run b_small_seg30 --model small --no-vad --max-segment 30
WZ_NOPROMPT=1 run b_small_seg30_noprompt --model small --no-vad --max-segment 30
WZ_NOSP=0.9 run b_small_seg30_nosp90 --model small --no-vad --max-segment 30

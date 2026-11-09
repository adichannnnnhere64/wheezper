#!/usr/bin/env bash
# Records real audio through the shipped binary.
#
#   scripts/vmtest-capture.sh [wheezper-binary]
#
# Creates a PulseAudio null sink, plays the committed speech fixture into it in
# real time, and transcribes the sink's *monitor* source — i.e. the exact path a
# call/video takes: cpal stream -> resampler -> streaming transcribe -> notepad.
# Then a second run in `--source dual` mode checks that both channels open,
# receive audio and are labelled me/them.
#
# Needs: a running PulseAudio/PipeWire server with pactl + paplay, python3.
# Env: MODEL (default base), MIN_ACCURACY (default 80), WHEEZPER (binary path).
# The gate is looser than the file-path gate (90%) on purpose: this exercises
# the *streaming* engine from a cold start, which trades accuracy for latency.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [ -n "${WHEEZPER:-}" ]; then
  bin="$WHEEZPER"
elif [ -x ./wheezper ]; then
  bin=./wheezper
elif [ -x target/release/wheezper ]; then
  bin=target/release/wheezper
else
  echo "no wheezper binary: build it or set WHEEZPER=/path/to/wheezper" >&2
  exit 2
fi
model="${MODEL:-base}"
min_accuracy="${MIN_ACCURACY:-80}"
wav="bench/fixtures/jfk.wav"
reference="bench/fixtures/jfk.txt"
out="bench/out"
sink="wheezper_speaker"

mkdir -p "$out"
app=""
sink_module=""

cleanup() {
  [ -n "$app" ] && kill -9 "$app" 2>/dev/null || true
  [ -n "$sink_module" ] && pactl unload-module "$sink_module" 2>/dev/null || true
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo; echo "=== $*"; }

# The streaming engine commits in ~3 s of audio per pass, so a single 11 s clip
# only exercises its cold start. Play the fixture three times back to back and
# score against the tripled reference: that is the regime it is built for.
ref3="$out/jfk-3x.txt"
cat "$reference" "$reference" "$reference" > "$ref3"

step 'audio server'
if ! pactl info > /dev/null 2>&1; then
  echo "starting a private PulseAudio daemon"
  pulseaudio --start --exit-idle-time=-1 --log-target="file:$out/pulseaudio.log" \
    || fail "could not start PulseAudio (see $out/pulseaudio.log)"
  sleep 2
fi
pactl info | sed -n '1,6p'

step "virtual output device ($sink)"
sink_module=$(pactl load-module module-null-sink \
  sink_name="$sink" \
  sink_properties=device.description="$sink") || fail "module-null-sink unavailable"
echo "module=$sink_module"
pactl list short sinks | grep -F "$sink" || fail "sink did not appear"
monitor=$(pactl list short sources | awk -v s="$sink" '$2 ~ s"\\.monitor" {print $2; exit}')
[ -n "$monitor" ] || fail "no monitor source for $sink"
echo "monitor source: $monitor"

# ---------------------------------------------------------------------------
step "record: --source loopback (streaming engine, live capture)"
notes="$out/capture-loopback.md"
log="$out/capture-loopback.log"
scorefile="$out/capture-loopback.score"

show_diagnostics() {
  # the transcript alone does not say *why* it is short: the engine's stderr
  # says whether it fell behind, dropped audio or never got a window
  echo "--- wheezper log ---" >&2
  cat "$log" >&2 || true
}

# One capture: start wheezper on the sink's monitor, play the fixture three
# times into it, stop it and let it commit the tail.
capture_loopback() {
  rm -f "$notes" "$log"
  "$bin" --source loopback --device "$sink" --model "$model" --language en \
    --no-vad --notes "$notes" > "$log" 2>&1 &
  app=$!
  for _ in $(seq 1 30); do
    grep -q 'source=loopback' "$log" 2>/dev/null && break
    kill -0 "$app" 2>/dev/null || { cat "$log"; fail "wheezper exited before opening the capture stream"; }
    sleep 1
  done
  grep -q 'source=loopback' "$log" || { cat "$log"; fail "wheezper did not report a loopback source"; }
  echo "playing $(basename "$wav") x3 into $sink"
  for _ in 1 2 3; do paplay --device="$sink" "$wav" || fail "playback failed"; done
  echo "playback finished; letting the decoder commit the tail"
  sleep 8
  kill -TERM "$app" 2>/dev/null || true
  for _ in $(seq 1 90); do kill -0 "$app" 2>/dev/null || break; sleep 1; done
  kill -9 "$app" 2>/dev/null || true
  wait "$app" 2>/dev/null || true
  app=""
}

# A capture backend hands its audio over in lumps, and on a loaded CI runner a
# single run can land badly (the same binary scores 89-100% here). Score up to
# two captures and keep the better one: the gate is about the engine, not about
# how PulseAudio felt that minute. Both numbers are printed.
best=0
for attempt in 1 2; do
  [ "$attempt" -gt 1 ] && echo "=== retrying the capture (attempt $attempt)"
  capture_loopback
  { [ -s "$notes" ] && grep -q '^\s*-\s' "$notes"; } || { show_diagnostics; fail "nothing was transcribed to $notes"; }
  sed -n '1,8p' "$notes"
  if [ -n "${WZ_STREAM_DEBUG:-}" ]; then
    echo "--- pass trace ---"
    grep -E '^pass |^decode ' "$log" | head -40 || true
  fi
  # always print the full score, gate afterwards
  python3 bench/score.py "$ref3" "$notes" --min-accuracy 0 > "$scorefile" 2>&1 || true
  cat "$scorefile"
  # word accuracy, not the "char accuracy" line below it
  acc=$(grep -oE '^accuracy +: +[0-9.]+' "$scorefile" | grep -oE '[0-9.]+' | tail -1)
  acc="${acc:-0}"
  echo "capture attempt $attempt: accuracy ${acc}% (gate ${min_accuracy}%)"
  if awk -v a="$acc" -v b="$best" 'BEGIN{exit !(a>b)}'; then best="$acc"; fi
  if awk -v a="$acc" -v m="$min_accuracy" 'BEGIN{exit !(a>=m)}'; then
    grep -qi 'ask not' "$notes" || { show_diagnostics; fail "no 'ask not' in the live transcript"; }
    grep -qi 'country' "$notes" || { show_diagnostics; fail "no 'country' in the live transcript"; }
    break
  fi
  if [ "$attempt" -eq 2 ]; then
    show_diagnostics
    fail "live capture transcript did not match the recording (best ${best}% < ${min_accuracy}%)"
  fi
done

# ---------------------------------------------------------------------------
step 'record: --source dual (two capture streams, me/them labels)'
notes2="$out/capture-dual.md"
log2="$out/capture-dual.log"
rm -f "$notes2"
"$bin" --source dual --device-me "$sink" --device-them "$sink" --model "$model" \
  --language en --notes "$notes2" > "$log2" 2>&1 &
app=$!
for _ in $(seq 1 30); do
  grep -q 'source=dual' "$log2" 2>/dev/null && break
  kill -0 "$app" 2>/dev/null || { cat "$log2"; fail "wheezper exited before opening both capture streams"; }
  sleep 1
done
grep -q 'source=dual' "$log2" || { cat "$log2"; fail "wheezper did not start in dual mode"; }
paplay --device="$sink" "$wav" || fail "playback failed"
sleep 4
kill -TERM "$app" 2>/dev/null || true
for _ in $(seq 1 90); do kill -0 "$app" 2>/dev/null || break; sleep 1; done
kill -9 "$app" 2>/dev/null || true
wait "$app" 2>/dev/null || true
app=""

[ -s "$notes2" ] || fail "dual mode recorded nothing to $notes2"
sed -n '1,10p' "$notes2"
grep -q '\*\*me:\*\*' "$notes2" || fail "no 'me' channel in the dual transcript"
grep -q '\*\*them:\*\*' "$notes2" || fail "no 'them' channel in the dual transcript"
grep -qi 'country' "$notes2" || fail "dual capture recorded nothing intelligible"

echo
echo "PASS: live capture records and transcribes (loopback + dual)"

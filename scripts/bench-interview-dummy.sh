#!/usr/bin/env bash
# Transcribe every WebM in interview-dummy and gate word accuracy against its VTT.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE_DIR="${1:?usage: scripts/bench-interview-dummy.sh FIXTURE_DIR [MODEL]}"
MODEL="${2:-${MODEL:-turbo}}"
MIN_ACCURACY="${MIN_ACCURACY:-95}"
MIN_SILENCE="${MIN_SILENCE:-1.2}"
MAX_SEGMENT="${MAX_SEGMENT:-25}"
THRESHOLD_DB="${THRESHOLD_DB:--40}"
THREADS="${THREADS:-0}"
BIN="${WHEEZPER_BIN:-$ROOT/target/release/wheezper}"
RUN_ID="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="${OUT_DIR:-$ROOT/bench/out/interview-dummy-$RUN_ID}"
FAILED=0

if [[ ! -x "$BIN" ]]; then
    echo "wheezper: build the release binary first (expected $BIN)" >&2
    exit 2
fi
mkdir -p "$OUT_DIR"

shopt -s nullglob
videos=("$FIXTURE_DIR"/*.webm)
if [[ ${#videos[@]} -ne 2 ]]; then
    echo "wheezper: expected exactly two .webm videos in $FIXTURE_DIR, found ${#videos[@]}" >&2
    exit 2
fi

for video in "${videos[@]}"; do
    stem="${video%.webm}"
    reference="${stem}.en.vtt"
    [[ -f "$reference" ]] || reference="${stem}.en-orig.vtt"
    if [[ ! -f "$reference" ]]; then
        echo "wheezper: no .en.vtt or .en-orig.vtt reference for $video" >&2
        exit 2
    fi
    name="$(basename "$stem")"
    notes="$OUT_DIR/$name.md"
    case "$name" in
        *Engineering*) prompt="Software engineering, product development, databases, cloud services, and technical project planning." ;;
        *) prompt="Data science, growth analytics, staff meeting, business metrics, and marketing performance." ;;
    esac
    echo "wheezper: transcribing $name with $MODEL; reference=$reference"
    if ! ffmpeg -nostdin -v error -i "$video" -vn -ac 1 -ar 16000 -f f32le - \
        | "$BIN" --source stdin --model "$MODEL" --language en --prompt "$prompt" \
            --threshold-db "$THRESHOLD_DB" --threads "$THREADS" \
            --min-silence "$MIN_SILENCE" --max-segment "$MAX_SEGMENT" \
            --notes "$notes" > "$OUT_DIR/$name.log" \
            2> "$OUT_DIR/$name.stderr.log"; then
        echo "wheezper: transcription failed for $name" >&2
        tail -n 20 "$OUT_DIR/$name.stderr.log" >&2 || true
        FAILED=1
        continue
    fi
    if ! python3 "$ROOT/bench/score.py" "$reference" "$notes" --min-accuracy "$MIN_ACCURACY" \
        | tee "$OUT_DIR/$name.score.txt"; then
        FAILED=1
    fi
done
echo "wheezper: interview results -> $OUT_DIR"
exit "$FAILED"

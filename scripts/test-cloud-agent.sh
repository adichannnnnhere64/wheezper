#!/usr/bin/env bash
# Smoke-test the Ollama Cloud annotation agent on a real recording.
#
#   scripts/test-cloud-agent.sh AUDIO [MODEL]
#
# Uses the normal transcription path (ffmpeg -> 16 kHz mono -> wheezper) and
# checks that at least one committed line gets a non-empty cloud answer.
# OLLAMA_API_KEY comes from the environment or ./.env. The audio is only read
# locally; nothing is uploaded except the transcript lines sent to the agent.
# Env: WHISPER_MODEL (default base), WHEEZPER_CLOUD_MODEL,
#      WHEEZPER_CLOUD_BUDGET (default 128), WHEEZPER (binary).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

audio="${1:?usage: scripts/test-cloud-agent.sh AUDIO [MODEL]}"
[ -f "$audio" ] || { echo "no such audio: $audio" >&2; exit 2; }
model="${2:-${WHEEZPER_CLOUD_MODEL:-deepseek-v4.1-flash}}"
budget="${WHEEZPER_CLOUD_BUDGET:-128}"
whisper="${WHISPER_MODEL:-base}"
bin="${WHEEZPER:-target/release/wheezper}"
[ -x "$bin" ] || { echo "build the release binary first ($bin)" >&2; exit 2; }
if ! command -v ffmpeg >/dev/null; then
    echo "ffmpeg is required to decode the sample" >&2
    exit 2
fi
if [ -z "${OLLAMA_API_KEY:-}" ] && [ ! -s .env ]; then
    echo "OLLAMA_API_KEY is not set and ./.env is missing" >&2
    exit 2
fi

out="bench/out/cloud-agent-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
notes="$out/notes.md"
log="$out/run.log"
echo "wheezper: cloud agent test -> $out (model=$model, whisper=$whisper)"
ffmpeg -nostdin -v error -i "$audio" -vn -ac 1 -ar 16000 -f f32le - \
    | "$bin" --source stdin --model "$whisper" --language en \
        --prompt "Java Spring Boot developer interview: Spring Boot, microservices, REST APIs, Kubernetes, AWS, Java." \
        --with-agent "ollama-cloud:$model:$budget" \
        --notes "$notes" > "$log" 2>&1

if ! grep -q 'ANS: ' "$notes"; then
    echo "FAIL: no cloud annotation reached the notes" >&2
    echo "--- app log ---" >&2
    tail -n 30 "$log" >&2 || true
    exit 1
fi

# Print the model answers with a stable label, independent of the notepad
# annotation decoration.
grep 'ANS: ' "$notes" | sed -E 's/^.*ANS: /ANS: /'
echo "PASS: cloud annotation(s) found in $notes"

#!/usr/bin/env bash
# End-to-end session sync: the laptop goes offline, keeps transcribing, and the
# queued events land on the server once it comes back.
#
#   nix-shell --run './scripts/test-session-sync.sh'
#
# Requirements: initdb/pg_ctl (PostgreSQL), a whisper model. Uses the packaged
# fixture and a throwaway database; nothing outside a temp dir is touched.

set -euo pipefail
cd "$(dirname "$0")/.."

port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }

TMP=$(mktemp -d)
SERVER_PID=""
cleanup() {
    [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" >/dev/null 2>&1 || true
    pg_ctl -D "$TMP/pg" -m immediate stop >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT

MODEL="${MODEL:-models/ggml-base-q5_1.bin}"
if [[ ! -f "$MODEL" ]]; then
    echo "wheezper: model $MODEL missing (run a normal transcription once, or set MODEL=/path)" >&2
    exit 1
fi
FIXTURE="${FIXTURE:-bench/fixtures/jfk.f32}"
[[ -f "$FIXTURE" ]] || { echo "wheezper: fixture $FIXTURE missing" >&2; exit 1; }

echo "wheezper: building server, admin CLI and sync-enabled client"
cargo build --release -p wheezper-server --bins
cargo build --release --bin wheezper --features sync

PGPORT=$(port)
mkdir -p "$TMP/sock"
initdb -D "$TMP/pg" -U postgres --auth=trust >/dev/null
pg_ctl -D "$TMP/pg" -o "-k $TMP/sock -c listen_addresses=127.0.0.1 -p $PGPORT" \
    -l "$TMP/postgres.log" start >/dev/null
psql -h 127.0.0.1 -p "$PGPORT" -U postgres -c 'CREATE DATABASE wheezper_sync' >/dev/null

SERVER_PORT=$(port)
SERVER_URL="http://127.0.0.1:$SERVER_PORT"
DB_URL="postgres://postgres@127.0.0.1:$PGPORT/wheezper_sync"
export WHEEZPER_STATE_DIR="$TMP/state"
PAIRING_CODE="sync-e2e-pairing"

start_server() {
    WHEEZPER_DATABASE_URL="$DB_URL" \
    WHEEZPER_BIND="127.0.0.1:$SERVER_PORT" \
    WHEEZPER_PAIRING_CODE="$PAIRING_CODE" \
        ./target/release/wheezper-server >>"$TMP/server.log" 2>&1 &
    SERVER_PID=$!
    for _ in $(seq 1 100); do
        if ./target/release/wheezper-admin --server "$SERVER_URL" pair \
            --code "$PAIRING_CODE" --name probe >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    echo "wheezper: server did not start" >&2
    cat "$TMP/server.log" >&2
    exit 1
}

stop_server() {
    if [[ -n "$SERVER_PID" ]]; then
        kill "$SERVER_PID" >/dev/null 2>&1 || true
        wait "$SERVER_PID" 2>/dev/null || true
        SERVER_PID=""
    fi
}

admin() { ./target/release/wheezper-admin --server "$SERVER_URL" "$@"; }

echo "wheezper: starting server"
start_server
DEVICE=$(admin pair --code "$PAIRING_CODE" --name laptop)
CREATE_OUT=$(admin --token "$DEVICE" create --title "sync e2e")
SESSION=$(sed -n 's/^session=//p' <<<"$CREATE_OUT")
SESSION_TOKEN=$(sed -n 's/^token=//p' <<<"$CREATE_OUT")
[[ -n "$SESSION" && -n "$SESSION_TOKEN" ]] || { echo "wheezper: create failed: $CREATE_OUT" >&2; exit 1; }
echo "wheezper: session $SESSION"

run_client() {
    local notes="$1" log="$2"
    ./target/release/wheezper \
        --source stdin \
        --model "$MODEL" \
        --language en \
        --no-vad \
        --notes "$notes" \
        --session "$SESSION" \
        --server "$SERVER_URL" \
        --session-token "$SESSION_TOKEN" \
        <"$FIXTURE" >"$log.stdout" 2>"$log.stderr" || {
            echo "wheezper: client failed" >&2
            cat "$log.stderr" >&2
            exit 1
        }
}

echo "wheezper: transcription #1 with the server DOWN (events must buffer)"
stop_server
run_client "$TMP/offline.md" "$TMP/offline"
grep -q "session sync offline" "$TMP/offline.stderr" || {
    echo "wheezper: expected an offline warning, got:" >&2
    cat "$TMP/offline.stderr" >&2
    exit 1
}
grep -q '^- \[' "$TMP/offline.md" || { echo "wheezper: local notes were not written" >&2; exit 1; }
OFFLINE_QUEUED=$(cat "$WHEEZPER_STATE_DIR"/pending/"$SESSION".jsonl | grep -c '"event_id"')
[[ "$OFFLINE_QUEUED" -ge 1 ]] || { echo "wheezper: no events were queued on disk" >&2; exit 1; }
echo "wheezper: $OFFLINE_QUEUED event(s) buffered on disk"

echo "wheezper: transcription #2 with the server UP (drains the backlog)"
start_server
run_client "$TMP/online.md" "$TMP/online"

SYNC_OUT=$(admin --token "$DEVICE" sync --session "$SESSION")
echo "$SYNC_OUT"
LINES=$(grep -c '^\[' <<<"$SYNC_OUT" || true)
[[ "$LINES" -ge 2 ]] || { echo "wheezper: expected buffered + live lines on the server, got $LINES" >&2; exit 1; }
grep -qi "country" <<<"$SYNC_OUT" || { echo "wheezper: server transcript has no fixture words" >&2; exit 1; }

# The pending queue must be empty now (all events acknowledged).
PENDING_FILE="$WHEEZPER_STATE_DIR/pending/$SESSION.jsonl"
if [[ -f "$PENDING_FILE" ]]; then
    REMAINING=$(grep -c '"event_id"' "$PENDING_FILE" || true)
    [[ "$REMAINING" -eq 0 ]] || {
        echo "wheezper: $REMAINING event(s) still queued after a clean drain" >&2
        cat "$PENDING_FILE" >&2
        exit 1
    }
fi

# Replaying the same fixture must not duplicate: sequence numbers are unique.
UNIQUE=$(sed -n 's/^\[ *\([0-9]*\)\].*/\1/p' <<<"$SYNC_OUT" | sort -u | wc -l)
[[ "$UNIQUE" -eq "$LINES" ]] || { echo "wheezper: duplicate sequence numbers" >&2; exit 1; }

echo "wheezper: session sync OK — $LINES committed events, no duplicates, backlog drained"

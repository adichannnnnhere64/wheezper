#!/usr/bin/env bash
# Run wheezper-server's integration tests against a throwaway PostgreSQL.
#
#   nix-shell --run './scripts/test-server.sh'
#   WHEEZPER_TEST_DATABASE_URL=postgres://... cargo test -p wheezper-server
#
# Without initdb/pg_ctl on PATH the script still builds and runs the unit tests
# (the integration tests then print their skip line).

set -euo pipefail
cd "$(dirname "$0")/.."

port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }

TMP=""
cleanup() {
    if [[ -n "${PGDATA:-}" && -n "${TMP:-}" ]]; then
        pg_ctl -D "$PGDATA" -m immediate stop >/dev/null 2>&1 || true
    fi
    [[ -n "${TMP:-}" ]] && rm -rf "$TMP"
}
trap cleanup EXIT

if [[ -z "${WHEEZPER_TEST_DATABASE_URL:-}" ]] && command -v initdb >/dev/null && command -v pg_ctl >/dev/null; then
    TMP=$(mktemp -d)
    PGDATA="$TMP/pg"
    mkdir -p "$TMP/sock"
    PGPORT=$(port)
    initdb -D "$PGDATA" -U postgres --auth=trust >/dev/null
    pg_ctl -D "$PGDATA" -o "-k $TMP/sock -c listen_addresses=127.0.0.1 -p $PGPORT" \
        -l "$TMP/postgres.log" start >/dev/null
    psql -h 127.0.0.1 -p "$PGPORT" -U postgres -c 'CREATE DATABASE wheezper_test' >/dev/null
    export WHEEZPER_TEST_DATABASE_URL="postgres://postgres@127.0.0.1:$PGPORT/wheezper_test"
    echo "wheezper: test database $WHEEZPER_TEST_DATABASE_URL"
else
    echo "wheezper: no throwaway PostgreSQL started (set WHEEZPER_TEST_DATABASE_URL to run integration tests)"
fi

cargo test -p wheezper-server "$@"

# wheezper-server

The durable session + intelligence layer for [wheezper](../README.md). It is
optional: the CLI works exactly as before without a server.

```
laptop (whisper.cpp)  --gRPC PublishTranscript-->  wheezper-server  --StreamTranscripts-->  Android
        |                                               |
   notes/YYYY-MM-DD.md                            PostgreSQL (durable) + Ollama (answers)
   pending queue (offline)
```

Only **committed** transcript lines are published. Provisional preview text is
display-only on the laptop and never leaves it; raw audio never leaves the
laptop at all.

## Quick start (Docker)

```bash
cp server/.env.example server/.env
# edit WHEEZPER_PAIRING_CODE, POSTGRES_PASSWORD
docker compose -f server/compose.yml up --build
```

The port is plaintext gRPC. Put a TLS terminator in front, or mount certs and
set `WHEEZPER_TLS_CERT` / `WHEEZPER_TLS_KEY`. See [TLS](#tls).

Deploying to a small server: build the image on your laptop and ship it rather
than building on the box (a Rust release build wants ~4 GB of builder cache):

```bash
scripts/deploy-image.sh HOST /path/to/deploy/dir
# docker save | ssh | docker load, then `docker compose up -d --no-build server`
```

Plain `cargo build -p wheezper-server` plus a `COPY` Dockerfile works too if the
target already has the same glibc.

## Quick start (native)

```bash
export WHEEZPER_DATABASE_URL=postgres://wheezper:wheezper@localhost/wheezper
export WHEEZPER_PAIRING_CODE="$(openssl rand -base64 24)"
cargo run -p wheezper-server
# migrations run automatically at startup
```

Operate it with the bundled CLI:

```bash
# pair a device (returns a short-lived device token)
wheezper-admin --server http://127.0.0.1:50051 pair --code "$PAIRING_CODE" --name laptop

# create a meeting; prints session=<uuid> and token=<one-meeting token>
wheezper-admin --server http://127.0.0.1:50051 --token "$DEVICE" create --title "Review"

# interview-assist session (the Android app shows an "AI" badge and auto-answers)
wheezper-admin --server http://127.0.0.1:50051 --token "$DEVICE" create \
    --title "Interview" --ai-help

# watch the stored transcript, or ask the assistant about it
wheezper-admin --server http://127.0.0.1:50051 --token "$DEVICE" sync --session "$SESSION"
wheezper-admin --server http://127.0.0.1:50051 --token "$DEVICE" ask --session "$SESSION" \
    --question "What did they say about the deadline?"

# per-request model / token / backend (what the phone sends)
wheezper-admin --server http://127.0.0.1:50051 --token "$DEVICE" ask --session "$SESSION" \
    --question "How would you handle concurrency?" \
    --model deepseek-v4.1-flash --assistant-token "$OLLAMA_CLOUD_TOKEN" --cloud
```

Point wheezper at the session:

```bash
wheezper --source dual \
  --session SESSION_ID \
  --server https://meeting.example.com:50051 \
  --session-token TOKEN
```

## Configuration

All configuration is environment variables (12-factor; no config file).

| Variable | Default | Meaning |
|---|---|---|
| `WHEEZPER_DATABASE_URL` | **required** | PostgreSQL URL (`DATABASE_URL` also accepted) |
| `WHEEZPER_PAIRING_CODE` | **required** | shared secret exchanged for a device token; min 12 chars |
| `WHEEZPER_BIND` | `0.0.0.0:50051` | gRPC listen address |
| `WHEEZPER_DEVICE_TTL_HOURS` | `168` | device-token lifetime |
| `WHEEZPER_RETENTION_DAYS` | `0` | delete completed sessions older than N days (0 = forever) |
| `WHEEZPER_ASSISTANT_URL` | `http://localhost:11434` | Ollama base URL (local or `ollama.com/api`) |
| `WHEEZPER_ASSISTANT_MODEL` | `gemma3:1b` | Ollama model name |
| `OLLAMA_API_KEY` / `WHEEZPER_ASSISTANT_API_KEY` | unset | bearer key for a cloud provider |
| `WHEEZPER_ASSISTANT_CONTEXT` | `auto` | interview prompt size: `auto` (short for a local model, whole meeting for a cloud one), `full`, `short` |
| `WHEEZPER_TLS_CERT` / `WHEEZPER_TLS_KEY` | unset | PEM files; both or neither |
| `RUST_LOG` | `info` | tracing filter |

If the assistant URL is not localhost the server logs a warning at startup:
transcript text is then sent to that provider.

## Data model

- `sessions` — durable meeting rows. States: `created → active → completed`,
  plus `ai_help` (the viewer should auto-answer the far side's questions).
  A laptop going offline never changes the state; a disconnect is not an end.
  Ending a session rotates (revokes) its publish token.
- `transcript_entries` — append-only. `event_id` is the idempotency key (unique
  across the table); `(session_id, sequence_number)` is unique and ordered.
  Sequence numbers are assigned by the server under a `FOR UPDATE` lock on the
  session row, so concurrent publishers cannot collide.
- `assistant_messages` — question/answer audit trail. Never inserted into the
  transcript.
- `devices` — token hashes + expiry for paired phones/laptops.

## Delivery semantics

1. The laptop writes the line to its notepad, appends the event to a local
   pending queue (fsynced), then tries to publish.
2. The server persists inside a transaction, then acknowledges with the
   assigned sequence number.
3. The client removes an event from the queue only after the ack. Retries reuse
   the same `event_id`; the server returns the stored row with
   `duplicate = true`.
4. Viewers (`StreamTranscripts`) get the backlog from PostgreSQL first, then
   live events. If the fan-out channel lags or a sequence gap appears, the
   handler re-reads the missing range from PostgreSQL — the in-memory channel is
   never the source of truth.

This is application-level delivery, not "gRPC guarantees it".

## AI help (per session)

`CreateSession(ai_help: true)` only records the intent. The Android app drives
the feature: it watches the committed stream and, for far-side lines that
qualify (questions by default, or every far-side line when selected), calls
`StreamAnswer` with the model/token chosen on the phone. The answer is shown in
a panel clearly marked as an AI suggestion and is persisted in
`assistant_messages`, never in `transcript_entries`.

Consequences worth knowing:

- only **committed** lines trigger an answer — provisional preview text is not
  published, so the answer follows the firm transcript,
- one answer is in flight at a time and only the newest queued line is kept,
- the phone's token/model are per request; rotate them by re-pairing or editing
  the settings,
- if the phone goes away, auto-answers stop (the server keeps only the flag).

## Authentication

- `PairDevice(pairing_code, device_name)` → short-lived **device token**.
  Only SHA-256 hashes are stored.
- `CreateSession` → also returns a **session token**, scoped to that one
  session. It can publish to its session and read that session; nothing else.
- Device tokens can create/list/read/end sessions and use the assistant.
- All credentials travel in `authorization: Bearer <token>` metadata.
  The assistant's per-request `api_key` field carries an optional provider
  token; it is used for that call only, never persisted and never logged.
- There is no account system and no public transcript endpoint. Use TLS: a
  bearer token in plaintext is as good as published.

## Assistant

The backend owns model access; the Android app never holds a model. A request
may carry a **model name**, a **provider token** and a **backend** choice
(`DEFAULT` = the configured Ollama URL, `OLLAMA_CLOUD` = the fixed
`https://ollama.com/api`). The endpoint is never client-supplied (no SSRF), the
token is used for that call only and is never stored or logged, and the token
falls back to the server's `OLLAMA_API_KEY` when the request has none.

`AskQuestion` / `StreamAnswer`:

1. load the session,
2. take the most recent transcript entries plus up to 20 older full-text
   matches for the question (`to_tsvector` / `plainto_tsquery`; the MVP stand-in
   for embeddings) plus the answers already given in this meeting,
3. apply the context budget (`assistant::context::Budget`): a local interview
   keeps the last 12 lines and 12k chars because a small model must prefill them
   before it answers; a cloud request (`OLLAMA_CLOUD`) gets the whole meeting up
   to 48k chars,
4. stream the answer from Ollama (NDJSON),
5. persist the question/answer pair with its `answered_sequence` (best-effort).

`ListAnswers` returns those pairs anchored to their transcript row
(`DISTINCT ON (answered_sequence)`, newest wins), so a phone that reopens the
meeting renders each answer under the line it answers instead of showing an
interview with no answers. The app sends `answered_sequence` on
`AskQuestion`/`StreamAnswer`; `0` means "not about one transcript row" (the
free-form Ask-AI sheet).

The system prompt forbids inventing meeting content, and answers are stored
outside the transcript. `think: false` is set because cloud reasoning models
otherwise spend the token budget in `message.thinking` and return empty
content.

Embeddings/vector search can replace `assistant/context.rs` later; the interface
(`MeetingContext`) stays.

## TLS

Either terminate TLS in a proxy and keep the gRPC listener on a private
network, or:

```bash
WHEEZPER_TLS_CERT=/certs/fullchain.pem \
WHEEZPER_TLS_KEY=/certs/privkey.pem \
WHEEZPER_BIND=0.0.0.0:50051 wheezper-server
```

The CLI and Android app accept `https://` endpoints (rustls / platform TLS).
Debug Android builds also allow plaintext for a laptop on the LAN; release
builds do not.

## Tests

```bash
# throwaway PostgreSQL + integration tests (pairing, idempotency, restart,
# stream resume, assistant with a mock Ollama)
./scripts/test-server.sh

# or against an existing disposable database
WHEEZPER_TEST_DATABASE_URL=postgres://... cargo test -p wheezper-server
```

Client-side end-to-end (offline buffering then drain):

```bash
nix-shell --run './scripts/test-session-sync.sh'
```

## Operations

- Back up PostgreSQL; it is the only durable state.
- The broadcaster is in-process. Run **one** server instance per database;
  horizontal scale-out needs a shared fan-out (e.g. Postgres `LISTEN/NOTIFY`)
  before it is safe.
- Expired device tokens are purged daily and at startup. Sessions are deleted
  after `WHEEZPER_RETENTION_DAYS`, if set.
- `SIGTERM`/Ctrl-C stop accepting connections and shut down gracefully.

## Contract

`proto/wheezper/v1/*.proto` is versioned in the package name. Breaking changes
bump to `wheezper.v2` and keep `v1` served until clients catch up. Never reuse
a field number. Regenerate Rust (`proto/`) and Android (`android/`) stubs from
the same files.

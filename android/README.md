# wheezper companion (Android)

Kotlin + Jetpack Compose viewer for meetings transcribed by the
[wheezper CLI](../README.md) and stored by
[wheezper-server](../server/README.md).

It is a **viewer**, not another transcription engine:

- no microphone, no whisper, no model on the phone — the laptop transcribes,
- no permanent credential in the APK — pairing exchanges the server's pairing
  code for a short-lived device token,
- only committed transcript text ever reaches the phone. Provisional preview
  text and raw audio stay on the laptop.

```
laptop --PublishTranscript--> server --StreamTranscripts--> this app
                                   |
                              AskQuestion / StreamAnswer -> Ollama
```

## Screens

| Screen | What it does |
|---|---|
| **Meetings** | server + pairing status, active meetings, recent meetings, "New meeting" |
| **Session** | live transcript (speaker dot, timestamp, text), search, end meeting, AI assistant, AI-help panel |
| **History** | completed meetings, title search, reopen a meeting |
| **Settings** | server address, device pairing, reload, privacy notes |

Live updates arrive over `StreamTranscripts`. A dropped connection is retried
with exponential backoff; on reconnect (or when a sequence gap is detected) the
app calls `SyncTranscripts` and back-fills from PostgreSQL, so the displayed
transcript stays complete and ordered. Sequence numbers are the dedupe key.

The assistant opens in a bottom sheet: answers stream in, are labelled
`AI SUGGESTION — NOT PART OF THE TRANSCRIPT`, and never touch the transcript.

## AI help

Tick **Enable AI help** in the new-meeting dialog:

- **Questions only** (default) answers a far-side committed line when it ends
  with `?` or starts with a question/interview cue (`what`, `explain`, `walk me
  through`, `can you`, …).
- **Every far-side line** answers every committed line from the other side.
  The candidate's own (`me`) lines are never answered.

Only committed lines trigger an answer: the provisional preview is not
published, so answers follow the firm transcript. One request runs at a time;
while it streams, only the newest queued line is kept, so a slow model cannot
build a backlog. The answer appears in the AI-help panel above the transcript.

The dialog (and Settings) hold the **model** and an optional **Ollama token**;
toggle **Use Ollama Cloud** to send requests to `https://ollama.com/api` (the
token is required there). The token stays on the phone (DataStore) and travels
with each request; the server never stores or logs it. Choosing Ollama Cloud
sends transcript text to ollama.com — the dialog says so explicitly.

## Build

Needs JDK 17 and Android SDK platform 36 + build-tools 36.

```bash
# from the repo root
ANDROID_HOME=/path/to/android-sdk ./scripts/build-android.sh
# or
cd android
echo "sdk.dir=/path/to/android-sdk" > local.properties
./gradlew assembleDebug
# -> app/build/outputs/apk/debug/app-debug.apk
```

The protobuf contract is the repo-root `proto/wheezper/v1/*.proto`. A Gradle
`Sync` task mirrors it into `app/src/main/proto` (gitignored) before codegen;
edit the repo-root files only.

## Pairing workflow

1. Run `wheezper-server` with `WHEEZPER_PAIRING_CODE` set.
2. On the phone: enter the server address + pairing code → **Pair**.
3. **New meeting** → the server creates the session and shows the command to
   paste on the laptop:

   ```bash
   wheezper --source dual --session <id> --server <url> --session-token <token>
   ```

4. Start that command on the laptop; lines appear on the phone as they are
   committed.

## TLS

Release builds refuse plaintext gRPC. Debug builds allow it so a laptop on the
LAN can be used without certificates (`src/debug/AndroidManifest.xml`); use
`https://` with a real deployment (the server can terminate TLS itself or sit
behind a proxy — see `server/README.md`).

## Local data

`DataStore` holds the server URL, the device token and the session token shown
after creating a meeting. Transcript rows are kept in memory for the open
meeting; there is **no offline transcript cache** (Room) yet, and no
notifications/export yet — those are PLAN2 phase 5 items.

## Client test (no emulator)

The gRPC client is pure JVM, so a live server can be exercised from a unit
test:

```bash
WHEEZPER_TEST_SERVER=http://127.0.0.1:50051 \
WHEEZPER_TEST_PAIRING_CODE=the-server-code \
  ./gradlew testDebugUnitTest
```

Without those variables the test is skipped. `assembleDebug` is the CI gate.

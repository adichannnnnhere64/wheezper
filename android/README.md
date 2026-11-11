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
build a backlog. Each answer is rendered **under the transcript row it
answers**, and it is stored by the server anchored to that row: reopening the
meeting (or restarting the app) calls `ListAnswers` and puts every earlier
answer back under its line. Tapping **Ask AI** opens the free-form sheet, which
is not anchored to a row. Opening the app in the middle of a live interview
answers the far side's pending questions too (the queue keeps the newest few);
a finished meeting is never re-answered.

The dialog (and Settings) hold the **model** and an optional **Ollama token**;
toggle **Use Ollama Cloud** to send requests to `https://ollama.com/api` (the
token is required there). The token stays on the phone (DataStore) and travels
with each request; the server never stores or logs it. Choosing Ollama Cloud
sends transcript text to ollama.com — the dialog says so explicitly.

## Updating the app

Release builds are distributed through Google Play (custom/private app), so Play
delivers the updates. **Settings → App → Check for updates** uses Play's in-app
update API (`com.google.android.play:app-update`): it asks Play whether a newer
build exists and, if so, opens Play's own full-screen update flow.

An app cannot update itself from a downloaded APK here: Play App Signing means
the installed app is signed with Google's key, so an APK signed with your upload
key is a different signer and the installer rejects it
(`INSTALL_FAILED_UPDATE_INCOMPATIBLE`). That is why there is no APK download
path in the app. A debug/sideloaded build has no Play record either, so the
screen reports "this build did not come from Google Play" instead of offering a
button that cannot work.

For the Play upload the app must be signed with a **stable upload key**:

```bash
keytool -genkeypair -v -keystore wheezper-release.jks -alias wheezper \
  -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=wheezper"
base64 -w0 wheezper-release.jks   # -> ANDROID_KEYSTORE_B64
# repository secrets: ANDROID_KEYSTORE_B64, ANDROID_KEYSTORE_PASSWORD,
#                     ANDROID_KEY_ALIAS, ANDROID_KEY_PASSWORD
```

CI then builds and uploads `wheezper-android.apk` + `wheezper-android.aab` as
the `wheezper-android-release` artifact for your Play pipeline (skipped when the
secrets are absent). Locally the same variables work:

```bash
WHEEZPER_ANDROID_KEYSTORE=wheezper-release.jks \
WHEEZPER_ANDROID_KEYSTORE_PASSWORD=... WHEEZPER_ANDROID_KEY_ALIAS=wheezper \
WHEEZPER_ANDROID_KEY_PASSWORD=... ./gradlew assembleRelease bundleRelease
```

Bump `versionCode`/`versionName` in `app/build.gradle.kts` for every upload —
Play refuses a repeated `versionCode`.

## Publishing to the APK store

The companion also ships through the APK store, and **every Android change ends
with a publish there** (see `AGENTS.md`). One command does the whole thing:

```bash
# bump versionCode/versionName first
scripts/publish-android.sh              # signed release APK -> store -> verify listing
scripts/publish-android.sh --build-only # just the signed APK
```

It refuses to publish a debuggable build or to run without the release key, and
prints the versionCode, sha256 and the store's own confirmation. The store
tooling defaults to the sibling `google-play-temu` checkout
(`WHEEZPER_STORE_PUBLISH` to override), the listing check uses
https://wheezper-dl.pumpkinforge.de/store (override with `WHEEZPER_STORE_URL`).

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

1. Get the pairing code from the server:
   `wheezper-server --print-pairing-code` (it is generated on first start and
   kept in `$XDG_STATE_HOME/wheezper/pairing-code`; setting
   `WHEEZPER_PAIRING_CODE` overrides it).
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

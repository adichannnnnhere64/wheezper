# wheezper on Android — install and use

APK: **https://wheezper-dl.pumpkinforge.de/app-debug.apk**
(16.6 MB, sha256 `2352c85a88c6a85aa08f36e5df57fd50e9ba7581e190eabf765b61f2962e58b9`)
Same file, other paths:

```
hermes:/home/ubuntu/migration/wheezper/app-debug.apk
hermes:/var/www/html/wheezper/app-debug.apk          (what the URL serves)
laptop:<repo>/android/app/build/outputs/apk/debug/app-debug.apk
```

Requirements: Android 8.0+ (minSdk 26). Debug-signed build: Android shows
"unknown app" / Play Protect warnings, that is expected — allow install for the
browser (or `adb install -r app-debug.apk`).

## 1. Point the app at the server

Open **wheezper → Settings**:

| field | value |
|---|---|
| Server address | `https://wheezper.pumpkinforge.de` |
| Pairing code | in `hermes:/home/ubuntu/migration/wheezper/.env` (`WHEEZPER_PAIRING_CODE`) |

Tap **Pair**. The device token lasts 7 days; pairing again is one click.

## 2. Start a meeting with AI help

**New meeting** → title → tick **Enable AI help** → create.
The app shows the laptop command to copy:

```bash
wheezper --source dual \
  --session <SESSION-ID> \
  --session-token <TOKEN> \
  --server https://wheezper.pumpkinforge.de
```

`--source dual` matters: the mic is labelled `me`, the far side `them`, and the
phone ignores `me` lines when deciding what to answer.

## 3. Read it on the phone

* Transcript rows appear as they are **committed** (not the laptop's preview
  text), with speaker dot + timestamp. Search box filters the open meeting.
* With AI help on, a far-side line that looks like a question (`?`, or starts
  with what/why/explain/how/walk me through/can you…) is answered automatically
  and streams into the AI panel; the answer is one short sentence phrased as the
  candidate would say it (~6-14 s on this box). Toggle **every far-side line** in
  Settings if you want answers for statements too.
* Answers are labelled as suggestions and never enter the transcript.
* **End meeting** marks it completed; completed meetings stay under **History**.

## Assistant settings (Settings)

| field | leave as |
|---|---|
| model | `gemma3:1b` (the model the server hosts) |
| Ollama token | empty |
| Use Ollama Cloud | **off** — the server answers locally; turning this on sends transcript text to ollama.com and needs a token |

## If something looks wrong

* **"AI help is on, but Ollama Cloud needs a token"** → Cloud toggle is on; turn
  it off, the server has a local model.
* **No transcript arriving** → the laptop command must be running and publishing;
  the app retries and re-syncs from PostgreSQL by sequence number on its own.
* **Answers refuse to use general knowledge** → the meeting was created without
  AI help. Only AI-help sessions use the interview prompt; others answer strictly
  from the transcript.
* **Server unreachable** → `https://wheezper.pumpkinforge.de` must be
  reachable from the phone's network (nothing else on the host is exposed).

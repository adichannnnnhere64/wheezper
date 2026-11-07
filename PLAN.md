# wheezper — live meeting/call transcription to text

A portable CLI service that captures microphone (+ system audio where possible),
transcribes it in near-real-time, and appends the text to a local notepad file.

Runs as a single binary on **Linux, Windows, and Android (Termux)**. Rust core.

---

## 1. Concept

```
┌────────────┐   ┌─────────┐   ┌───────────────┐   ┌────────────┐   ┌─────────┐
│ audio src  │──▶│ ring buf│──▶│ VAD + segment │──▶│ transcriber│──▶│  sink(s)│
│ mic/loop   │   │ 16kHz   │   │ utterances    │   │ whisper.cpp│   │ notepad │
└────────────┘   └─────────┘   └───────────────┘   └────────────┘   └─────────┘
   pluggable          resample/scale         chunking          local or cloud   file,
   per-OS                                                      engine           clip, webhook
```

Unix-philosophy escape hatch: if fed raw PCM on **stdin**, the app is just
`transcribe → append`. This solves Termux capture, testing, and exotic setups
in one move.

## 2. Platform capture matrix (the honest constraints)

| Platform | Mic | System/loopback audio | How |
|---|---|---|---|
| Linux (PipeWire) | ✅ | ✅ monitor source | CPAL `pulseaudio` host, or `pw-cat` piped in |
| Windows 10/11 | ✅ | ✅ WASAPI loopback | CPAL `wasapi` host (loopback supported) |
| Android/Termux | ✅ via `termux-microphone-record` | ❌ needs root/MediaProjection — **out of scope** | pipe raw file/PCM into wheezper |
| Any (fallback) | via stdin | via stdin | `ffmpeg -f ... -` \| `wheezper` |

Two-source mode (mic **and** loopback for calls): both streams are resampled to
16 kHz mono and mixed into one ring buffer; a `--dual` flag on desktop.

## 3. Architecture (Rust crates)

```
wheezper/
├── src/
│   ├── main.rs          # CLI (clap)
│   ├── source/
│   │   ├── mod.rs       # trait AudioSource { fn stream(&mut) -> ... }
│   │   ├── cpal.rs      # Linux/Windows: mic + loopback
│   │   └── stdin.rs     # universal: raw PCM from stdin/pipe/file
│   ├── dsp.rs           # resample (rubato), mix dual sources, ring buffer
│   ├── vad.rs           # webrtc-vad or Silero ONNX: speech/no-speech gates
│   ├── segmenter.rs     # utterance boundaries: split on ≥300ms silence,
│   │                    #   min 0.5s / max ~12s segments w/ 1s lookback overlap
│   ├── engine/
│   │   ├── mod.rs       # trait Transcriber
│   │   ├── whisper.rs   # whisper-rs (whisper.cpp): local, quantized models
│   │   └── cloud.rs     # optional: Groq/OpenAI whisper API (async, batched)
│   └── sink/
│       ├── mod.rs       # trait Sink
│       ├── notepad.rs   # append markdown w/ [HH:MM:SS] prefix, flush per line
│       ├── clipboard.rs # arboard (desktop only)
│       └── webhook.rs   # POST JSON segment (for phone → PC relay, bots, etc.)
└── config.toml          # model path, language, sinks, hotkeys off
```

**Pipeline flow:**
1. `AudioSource` produces f32 16 kHz mono frames (CPAL gives 48 kHz stereo → resample).
2. VAD tags frames; segmenter accumulates a segment until silence or max length.
3. Segment (with `--partial` flag: also transcribe rolling 2s windows for live captions).
4. Transcriber returns text → sinks.
5. Notepad sink appends `- [14:32:05] utterance text…` and flushes immediately
   (safe for Obsidian/vim tailing, and never corrupts on Ctrl-C).

### Key crates
- `cpal` — capture (Linux pulse/pipewire host, Windows WASAPI incl. loopback)
- `rubato` — resampling
- `webrtc-vad` (or `silero-vad` ONNX) — silence detection, cuts hallucination
- `whisper-rs` — whisper.cpp bindings; `small` or `base` int8 model, `ggml`
- `clap` + `directories` — CLI + config/model storage
- `tokio` only for the cloud engine path; local path is fully sync (no runtime)

## 4. Transcription engine plan

**Default: local whisper.cpp via `whisper-rs`**
- Models: `ggml-base-q5_1` (~60 MB, fast) for meetings; `small-q5_1` if CPU allows.
- Real-time factor on a modern laptop CPU with base-int8: comfortably < 1×.
- `--language en` auto-lock (prevents language flapping mid-call).
- Suppress whisper hallucination: VAD-gated segments + confidence filter +
  drop repeats (whisper.cpp loves echoing "Thank you." on silence).

**Optional: `--engine cloud`**
- Groq `whisper-large-v3-turbo` API: fastest cheap option, ~few ¢/hour.
- Good for Termux phones where local whisper is too slow — Termux records mic,
  phone Wi-Fi uploads, PC or phone writes the notepad.

**Termux note:** whisper.cpp compiles under Termux (`pkg install rust`), but
`small` is slow on many phones → recommend cloud engine or stdin-pipe mode
where the phone captures and a PC transcribes.

## 5. Output sinks

- **notepad** (always): `notes/2025-11-07_meeting.md`
  ```markdown
  # Meeting — 2025-11-07 14:31

  - [14:32:05] Alright, let's kick off with the roadmap items.
  - [14:32:41] First, the Q3 migration is on track…
  ```
- **clipboard** (`--clip`): each utterance replaces clipboard (paste anywhere).
- **webhook** (`--push URL`): JSON POST → enables Termux → PC relay, Discord
  bots, browser tabs, etc.
- **speaker labels**: not in v1. Two-mic diarization is research-grade;
  instead use dual-source separation: mic source = "me", loopback = "them"
  (only works when the other side is VoIP output, not same-room).

## 6. Distribution

| Platform | How |
|---|---|
| Linux | static musl binary, single file, no deps |
| Windows | `wheezper.exe`, no installer, loopback needs nothing extra |
| Termux | `cargo build --release` in Termux (rust is in termux-pkgs); optional `.deb` later |
| Models | auto-download on first run to `~/.local/share/wheezper/` (or `--model path`) |

Config resolution: `--config` flag → `./wheezper.toml` → `~/.config/wheezper/config.toml`.

## 7. Milestones

1. **M1 — skeleton (2–3 days):** stdin PCM → whisper-rs → append to file. Proves
   the whole pipeline with zero capture code. Test with
   `ffmpeg -f pulse ... -` or any wav.
2. **M2 — capture:** CPAL mic on Linux/Windows, WASAPI loopback, resampling.
3. **M3 — segmentation quality:** VAD + overlap + hallucination filters,
   `--partial` live captions. This is where most tuning time goes.
4. **M4 — portability:** musl build, Windows CI, Termux instructions,
   webhook/clipboard sinks, cloud engine.
5. **M5 — polish:** config file, model auto-download, Ctrl-C safety (partial
   flush), README with per-OS quickstart.

## 8. MVP CLI shape

```bash
# desktop: mic + system audio → today's note
wheezper --dual

# mic only
wheezper --source mic

# Termux: pipe from termux-api, transcribe on phone (cloud engine)
termux-microphone-record -r 16000 -c 1 -f wheezper_in.wav & # (or pw/sox pipe)
wheezper --engine cloud --language en

# any OS, no capture code: raw PCM on stdin
ffmpeg -f pulse -i default -ar 16000 -ac 1 -f f32le - | wheezper --stdin
```

## 9. Risks / open questions

- **Windows WASAPI loopback via CPAL**: works, but exclusive-mode apps and
  some Bluetooth mics misbehave — need a `--device` picker and `list` command.
- **whisper hallucinations on silence/music** — VAD-gating mitigates; keep
  beam size ≥2 for the quality path.
- **Android system audio**: blocked without MediaProjection; accepted
  limitation (mic-only on Termux).
- **Privacy**: local default keeps meeting audio on-device; cloud engine is
  opt-in only.

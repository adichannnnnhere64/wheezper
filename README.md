# wheezper

Live meeting/call transcription into a notepad. Single portable binary,
local whisper.cpp inference — audio never leaves your machine.

```
mic / loopback / stdin ─▶ VAD + segmenter ─▶ whisper.cpp ─▶ timestamped markdown
```

**Zero external dependencies at runtime**: the model auto-downloads on first
run; on Windows the exe imports only OS DLLs (statically linked CRT — no
VC++ redistributable needed).

## Status

- **M1 — stdin pipeline**: works, unit tested.
- **M2 — native capture**: mic + system loopback via CPAL (PulseAudio host on
  Linux, WASAPI on Windows), verified live on PipeWire.
- **M3 — segmentation quality**: 0.6 s silence split, 25 s max segments,
  best-of-5 decoding, initial-prompt continuity across force-cuts.
- **M4 (Windows)**: cross-builds to a portable `wheezper.exe` (static CRT,
  system DLLs only), model auto-download built in.
- **Calls**: `--source dual` transcribes mic ("me") + system loopback
  ("them") into speaker-labeled notes; `--webhook` pushes each utterance
  as JSON.

## Usage

```bash
# just run it — mic input, model auto-downloads on first use (~57 MB, once)
wheezper

# system/loopback audio — the other side of a call, videos, etc.
wheezper --source loopback

# calls: mic + loopback at once, labeled me/them
wheezper --source dual

# push every utterance somewhere (phone->PC relay, bots, ...)
wheezper --source dual --webhook https://example.com/hook

# rolling live captions while you speak (stdout only; pairs well with --model tiny)
wheezper --partial --model tiny

# pick a model: tiny, base (default), small, medium, turbo
wheezper --model small

# pick a specific device
wheezper --list-devices
wheezper --source loopback --device hdmi

# any recording or exotic source: pipe mono f32le PCM
ffmpeg -i recording.wav -ar 16000 -ac 1 -f f32le - | wheezper --source stdin
```

Output (default `notes/YYYY-MM-DD.md`, flushed per line) is also echoed to
stdout, so you can watch it live:

```markdown
# Meeting — 2026-11-07 19:06

- [19:06:13] And so my fellow Americans ask not what your country can do for you…
```

Loopback notes: on Linux it captures the default sink's PulseAudio monitor
source (works under PipeWire). On Windows, WASAPI loopback opens the default
output device as an input stream. On hosts without loopback, pipe via ffmpeg.

## Models

`--model` accepts a path to an existing ggml `.bin`, a model name, or an exact
file from the huggingface repo. Named models are downloaded once (rustls HTTPS,
no system TLS/CA dependencies) into the cache dir — `%APPDATA%\wheezper`
(Windows), `~/.local/share/wheezper` (Linux), or `WHEEZPER_MODEL_DIR`:

| name | size | notes |
|------|------|-------|
| tiny | ~32 MB | fastest, weakest |
| base | ~57 MB | default, good meeting quality |
| small | ~181 MB | better, ~3× slower |
| medium | ~513 MB | much slower |
| turbo | ~574 MB | large-v3-turbo, needs a strong CPU |

To pre-seed the cache manually:

```bash
curl -L -o ~/.local/share/wheezper/ggml-base-q5_1.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin
```

## Install / build

```bash
# NixOS (shell.nix included)
nix-shell --run 'cargo build --release'

# Windows (native: rustup + Visual Studio Build Tools)
cargo build --release        # -> wheezper.exe, no installer, no extra DLLs

# or cross-build the Windows exe from Linux
nix-shell --run 'scripts/build-windows.sh'

# Termux
pkg install rust binutils cmake clang
cargo build --release
```

## Config file

Avoid typing flags: keys mirror CLI flags, command-line flags always win.
Loaded from `--config PATH`, else `./wheezper.toml`, else
`~/.config/wheezper/config.toml` (Linux) or `%APPDATA%\wheezper\config.toml`
(Windows). See [`wheezper.example.toml`](wheezper.example.toml):

```toml
# daily meeting setup
source = "dual"      # mic | loopback | dual | stdin
model = "base"       # tiny | base | small | medium | turbo | /path/to/model.bin
language = "en"     # or "auto"
partial = true       # rolling live captions (stdout only)
webhook = "http://192.168.1.20:8399/hook"
notes = "notes/work.md"
```

## Ollama: summary + polish (local LLM, no data leaves your machine)

If you run [Ollama](https://ollama.com), wheezper can post-process the
transcript with any model it hosts — Google's Gemma by default:

```bash
ollama pull gemma3:1b   # Google's smallest, ~800 MB — good summaries

wheezper --ollama                       # summary appended to notepad on stop
wheezper --ollama --polish              # also clean each utterance (latency!)
wheezper --ollama --ollama-model llama3.2   # any hosted model works
wheezper --ollama http://server:11434        # remote Ollama server
```

On stop (Ctrl-C), the whole session transcript is sent to the model and a
summary block (`one-paragraph, Key points, Action items`) is appended:

```markdown
## Summary — 2026-11-07 21:58

The meeting focused on the Q3 migration timeline... The speakers agreed on ... 
## Key points:
*   ...
## Action items:
*   ...
```

`--polish` additionally cleans each utterance (punctuation, filler words)
before writing — adds a per-line LLM round-trip, so use it when latency
doesn't matter. If the Ollama server is down or the model missing, wheezper
warns and keeps the raw transcript untouched.

## Termux

CPAL has no Android host, so use the stdin path:

```bash
termux-microphone-record -r 16000 -e m4a -f /tmp/meeting.m4a   # stop: termux-microphone-record -q
ffmpeg -i /tmp/meeting.m4a -ar 16000 -ac 1 -f f32le - | wheezper --source stdin
```

## Options

```
-m, --model <PATH|NAME>  model path or name: tiny, base, small, medium, turbo
                          [default: base] (or env WHEEZPER_MODEL)
-n, --notes <PATH>     notepad file [default: notes/YYYY-MM-DD.md]
-l, --language <LANG>  language code, or "auto" [default: auto]
    --source <SRC>     mic | loopback | dual | stdin [default: stdin if piped, else mic]
    --device <NAME>    pick device by substring (see --list-devices)
    --host <NAME>     audio host override (pulseaudio, wasapi, alsa, ...)
    --webhook <URL>   POST each utterance to this URL as JSON
    --partial         rolling live captions of the utterance in progress
    --config <PATH>   config file [default: ./wheezper.toml, then config dir]
    --list-devices    list audio devices and exit
    --rate <HZ>       stdin sample rate [default: 16000, mono f32le]
    --threshold-db    silence gate in dBFS (adaptive noise floor on top) [default: -40]
    --min-silence     seconds of silence that close a segment [default: 0.6]
    --min-segment     minimum segment seconds [default: 0.5]
    --max-segment     maximum segment seconds [default: 25]
    --threads         CPU threads for whisper [default: all]
```

## Design

See [PLAN.md](PLAN.md) for the roadmap. `vendor/whisper-rs-sys` pins
whisper.cpp and forces a static CRT (/MT) on Windows builds.

# wheezper

Live meeting/call transcription into a notepad. Portable CLI, Rust core,
local whisper.cpp inference — audio never leaves your machine.

```
stdin (f32le mono PCM) ─▶ energy VAD + segmenter ─▶ whisper.cpp ─▶ timestamped markdown
```

## Status

**M1 — stdin pipeline works.** Capture (mic/loopback via CPAL) is next; until
then, feed audio from any source that can pipe raw PCM.

## Usage

```bash
# any wav/flac/mp4 recording
ffmpeg -i recording.wav -ar 16000 -ac 1 -f f32le - | wheezper -m models/ggml-base-q5_1.bin

# Linux desktop: default microphone (PipeWire/Pulse)
ffmpeg -f pulse -i default -ar 16000 -ac 1 -f f32le - | wheezper -m models/ggml-base-q5_1.bin

# Linux desktop: system/loopback audio (the other side of a call)
ffmpeg -f pulse -i <monitor-source> -ar 16000 -ac 1 -f f32le - | wheezper -m models/ggml-base-q5_1.bin

# Termux: record mic, then transcribe the file
termux-microphone-record -r 16000 -e m4a -f /tmp/meeting.m4a  # stop with: termux-microphone-record -q
ffmpeg -i /tmp/meeting.m4a -ar 16000 -ac 1 -f f32le - | wheezper -m models/ggml-base-q5_1.bin
```

Output (default `notes/YYYY-MM-DD.md`, flushed per line):

```markdown
# Meeting — 2026-11-07 19:06

- [19:06:13] And so my fellow Americans ask not what your country can do for you…
```

Each line is also echoed to stdout, so you can watch it live.

## Install / build

Rust + a C++ toolchain + cmake + libclang (whisper.cpp build requirements).

```bash
# NixOS (shell.nix included)
nix-shell --run 'cargo build --release'

# Linux / Windows
cargo build --release

# Termux
pkg install rust binutils cmake clang
cargo build --release
```

## Models

Quantized whisper models go to `models/` (download once):

```bash
curl -L -o models/ggml-base-q5_1.bin  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin   # ~57 MB, recommended
curl -L -o models/ggml-tiny-q5_1.bin  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny-q5_1.bin   # ~32 MB, faster/weaker
```

## Options

```
-m, --model <PATH>     ggml model (or env WHEEZPER_MODEL)
-n, --notes <PATH>     notepad file [default: notes/YYYY-MM-DD.md]
-l, --language <LANG>  language code, or "auto" [default: auto]
    --rate <HZ>        input sample rate [default: 16000, mono f32le]
    --threshold-db     silence gate in dBFS [default: -40]
    --min-silence      seconds of silence that close a segment [default: 0.4]
    --min-segment      minimum segment seconds [default: 0.5]
    --max-segment      maximum segment seconds [default: 12]
    --threads          CPU threads for whisper [default: all]
```

## Design

See [PLAN.md](PLAN.md) for the roadmap: M2 adds native capture (CPAL mic +
WASAPI/PipeWire loopback), M3 tuning, M4 portability/sinks.

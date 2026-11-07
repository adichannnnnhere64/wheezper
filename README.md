# wheezper

Live meeting/call transcription into a notepad. Portable CLI, Rust core,
local whisper.cpp inference — audio never leaves your machine.

```
stdin (f32le mono PCM) ─▶ energy VAD + segmenter ─▶ whisper.cpp ─▶ timestamped markdown
```

## Status

- **M1 — stdin pipeline**: works, unit tested.
- **M2 — native capture**: works (verified live on PipeWire). Mic + system
  loopback via CPAL; stdin fallback everywhere.
- Next: M3 segmentation quality, M4 portability (Windows build, Termux notes).

## Usage

```bash
# microphone (default when nothing is piped in)
wheezper -m models/ggml-base-q5_1.bin

# system/loopback audio — the other side of a call, videos, etc.
wheezper --source loopback -m models/ggml-base-q5_1.bin

# pick a specific device
wheezper --list-devices
wheezper --source loopback --device hdmi -m models/ggml-base-q5_1.bin

# any recording or exotic source: pipe mono f32le PCM
ffmpeg -i recording.wav -ar 16000 -ac 1 -f f32le - | wheezper --source stdin -m models/ggml-base-q5_1.bin

# stdin at another rate (resampled automatically)
ffmpeg -i old.wav -ar 44100 -ac 1 -f f32le - | wheezper --source stdin --rate 44100 -m models/ggml-base-q5_1.bin
```

Loopback notes: on Linux it uses the default sink's PulseAudio monitor source
(works under PipeWire). On Windows, WASAPI loopback opens the default output
device as an input stream. On hosts without loopback, pipe via ffmpeg.

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
    --source <SRC>     mic | loopback | stdin [default: stdin if piped, else mic]
    --device <NAME>    pick device by substring (see --list-devices)
    --host <NAME>     audio host override (pulseaudio, wasapi, alsa, ...)
    --list-devices    list audio devices and exit
    --rate <HZ>       stdin sample rate [default: 16000, mono f32le]
    --threshold-db    silence gate in dBFS [default: -40]
    --min-silence     seconds of silence that close a segment [default: 0.4]
    --min-segment     minimum segment seconds [default: 0.5]
    --max-segment     maximum segment seconds [default: 12]
    --threads         CPU threads for whisper [default: all]
```

## Termux

CPAL has no Android host, so use the stdin path:

```bash
termux-microphone-record -r 16000 -e m4a -f /tmp/meeting.m4a   # stop: termux-microphone-record -q
ffmpeg -i /tmp/meeting.m4a -ar 16000 -ac 1 -f f32le - | wheezper --source stdin -m model.bin
```

## Design

See [PLAN.md](PLAN.md) for the roadmap: M2 adds native capture (CPAL mic +
WASAPI/PipeWire loopback), M3 tuning, M4 portability/sinks.

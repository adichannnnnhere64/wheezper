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
- **Built-in LLM**: `--ollama` summarizes the session via an Ollama server
  if you run one, else via **llama.cpp compiled into wheezper itself**
  (worker embedded in the exe, Gemma model auto-downloads) — no installs.
- **Phase 3 context and annotations**: `--prompt` biases whisper with a short
  domain hint; `--with-agent` adds optional asynchronous notes under committed
  transcript lines without delaying transcription.

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

# domain-specific terms, products, or meeting topic (opt-in whisper bias)
wheezper --prompt "Engineering planning for the Atlas migration"

# annotate committed lines asynchronously (requires a model already in Ollama)
wheezper --source dual --prompt "Engineering planning for Atlas" \
  --with-agent "ollama:gemma3:1b:256"

# cloud annotations require OLLAMA_API_KEY; the final number is output tokens
wheezper --with-agent "ollama-cloud:deepseek-v4.1-flash:1017"

# pick a model: tiny, base (default), small, medium, turbo
wheezper --model small

# pick a specific device
wheezper --list-devices
wheezper --source loopback --device hdmi

# any recording or exotic source: pipe mono f32le PCM
ffmpeg -i recording.wav -ar 16000 -ac 1 -f f32le - | wheezper --source stdin

# MUSIC (songs, videos with sung vocals): the VAD gate deletes singing and
# short segments cut mid-phrase. Turn both off and use a bigger model
wheezper --source loopback --no-vad --max-segment 25 --model turbo

# known song: follow a reference lyric sheet (see "Songs and lyrics" below)
wheezper --source loopback --no-vad --max-segment 40 --model turbo \
         --lyrics song.lyrics --lyric-sync
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

## Live latency (measured, not estimated)

Live capture runs **two tiers per channel** (Phase 2):

- a **provisional** tier decodes just the live edge (≤0.8 s windows, capped
  encoder context, one greedy pass) and puts words on screen as a revisable
  `▍` preview line — **~0.2–0.35 s** after they are spoken;
- the **final** tier is the quality engine (8 s windows, full decode with the
  temperature-fallback ladder): words commit exactly once, append-only, and
  only this text reaches the notes file. It runs a few seconds behind — you
  already read the words in the preview — so the notes stay clean.

When stdout is piped (not a terminal) the preview tier is skipped entirely and
wheezper behaves like the single-tier Phase-1 engine.

`stream::tests::measures_commit_latency` feeds real audio in real time and
stamps every committed word, so "word finished being spoken → word on screen"
is measured end to end (6-core desktop, models local):

| setup | median | p90 | one pass |
|---|---|---|---|
| preview tier, `base`, 1 ch | **0.22 s** | 0.46 s | 0.10 s |
| preview tier, `base`, dual (both sides talking) | **0.22–0.33 s** | 0.46–0.62 s | 0.10 s |
| final tier (into notes), `base`, 1 ch | 4.11 s | 5.50 s | 3.1 s (2 threads) |
| final tier (into notes), `base`, dual | 6.6–7.4 s | 7.6 s | 3.3 s |
| Phase-1 single tier (piped stdout), `base`, 1 ch | 2.60 s | 3.00 s | 1.80 s (4 threads) |
| Phase-1 single tier (piped stdout), `base`, dual | 4.23 s | 5.13 s | 1.70 s |
| Phase-1 single tier (piped stdout), `small`, 1 ch | 6.86 s | 7.70 s | 6.40 s |

Reproduce (add `WZ_ACCEPT=1` to gate the 0.5 s median / 0.8 s p90 budget on
the preview tier):

```bash
CHANNELS=2 MODEL=base WZ_ACCEPT=1 LATENCY_SECONDS=40 nix-shell --run \
  'cargo test --release stream::tests::measures_commit_latency -- --ignored --nocapture'
```

Reading it: preview latency ≈ 0.1 s (hold) + one cheap pass + scheduling. The
final tier only stays bounded while **one pass is faster than the audio it
covers** — `base` has headroom on this box, `small` is at its limit
(6.4 s per pass at 4 threads, ~7 s window), and `turbo` is 3.8× *slower* than
real time, so it is a batch/file model, not a live one. On a slower laptop,
watch the transcript: if text drifts further and further behind the speaker,
drop to `base` or lower `--threads` so other apps still get CPU.

For a meeting, `--source dual` also means the mic hears the speakers unless you
use headphones — that transcribes the clients twice (once as `them`, once as
`me`).

## Songs and lyrics

Speech-tuned defaults fight music: the Silero VAD gate classifies singing as
non-speech (it silently drops whole lines) and 10 s segments cut through
phrases. For anything sung:

```bash
wheezper --no-vad --max-segment 25 --model turbo
```

Measured on two NIKI tracks (see `bench/RESULTS.md` for the full table,
scripts and raw output):

| setup | word accuracy | character accuracy |
|---|---|---|
| `base` (default) | 39% | 63% |
| `small` | 79–81% | 87% |
| `turbo` | 89–93% | 94–95% |
| `turbo --lyrics --lyric-sync` (known song) | 99–100% | 99–100% |

95% word-level accuracy is not reachable from acoustics alone on sung, layered
vocals at this model scale — the remaining errors are homophones and dropped function
words. If you *have* the lyrics:

- `--lyrics FILE` — the decoder is prompted with the lyrics around a moving
  cursor (bias only; measured gain is small).
- `--lyrics FILE --lyric-sync` — output the reference wording for every
  segment the (unbiased) transcription aligns with, keeping ASR text where it
  does not. This is karaoke-style lyric sync, not transcription: 99–100% word
  accuracy on the tracks tested. Longer segments help it (see below).

## Interview reference scoring

To compare the two local WebM interviews against their matching YouTube VTTs,
build the release binary and run the 95% word-accuracy gate:

```bash
nix-shell --run 'cargo build --release'
scripts/bench-interview-dummy.sh /path/to/interview-dummy turbo
```

The harness handles rolling VTT cues and scores only timestamped transcript
rows. It writes transcripts, logs, and per-video score summaries under
`bench/out/`; override `MIN_ACCURACY`, `MIN_SILENCE`, `MAX_SEGMENT`,
`THRESHOLD_DB`, `THREADS`, or `OUT_DIR` when tuning a run.

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

# Termux / Android (no llama.cpp: that needs the Android NDK)
pkg install rust binutils cmake clang
cargo build --release --no-default-features

# anywhere, if you do not want the embedded LLM (≈4 MB smaller binary; a
# --ollama <server> still works, only the bundled model runner goes away)
cargo build --release --no-default-features
```

## CI / CD: tested artifacts only

`.github/workflows/ci.yml` builds, tests and then tests *the packaged artifact*
again, on Linux and on real Windows VMs. `.github/workflows/release.yml` calls
that workflow and publishes only what passed, with SHA256 checksums.

What every run gates on:

| gate | where | fails the build when |
|---|---|---|
| unit tests | Linux + Windows | `cargo test --release --workspace` fails (incl. the streaming replay test) |
| accuracy gate | Linux + Windows | the built binary scores < 90% word accuracy on `bench/fixtures/jfk.f32` |
| Linux artifact | fresh Ubuntu 22.04 *and* 24.04 VMs | the tarball, unpacked in a scratch dir, does not run or score 90% (no compilers, no `target/` — only `libasound2`, the documented runtime dep) |
| Linux **capture** | fresh Ubuntu 22.04 *and* 24.04 VMs | the shipped binary cannot *record*: a PulseAudio null sink + `paplay` feed real audio into the monitor source, the transcript must match (≥70%) and a `--source dual` run must produce both `me` and `them` lines |
| Windows artifact | fresh `windows-2022` and `windows-2025` VMs, as a **standard user** | the zip needs admin rights, DLLs, an installer, PATH changes, or fails to transcribe |
| song benchmark (optional) | Ubuntu, when `BENCH_FIXTURES_URL` is set | pure-ASR < 85% or lyric-sync < 95% word accuracy (`bench/RESULTS.md`) |

### The Windows VM test

GitHub-hosted `windows-*` runners are Hyper-V VMs booted per job, so the job
only ever sees the uploaded zip. It then:

1. creates a local **standard user** (`New-LocalUser`, not in Administrators),
2. unpacks the zip into `C:\Users\Public\wheezper-vmtest` (user-writable,
   nothing installed, PATH untouched),
3. launches `scripts/vmtest-windows.ps1` **as that user**
   (`Start-Process -Credential` → `CreateProcessWithLogonW`: own token, own
   `%APPDATA%`, no admin group membership),
4. which asserts it is unelevated, runs `--version`/`--help`, enumerates audio
   devices on a deviceless VM, downloads the whisper model into `%APPDATA%`,
   converts the committed WAV to the f32le stdin stream and transcribes it,
   checks the transcript words, checks nothing was written to `Program Files`
   or `ProgramData`, then repeats with the cached model.

The same script runs in Windows Sandbox or any Hyper-V/VMware VM:

```powershell
# as a standard user, next to wheezper.exe
powershell -ExecutionPolicy Bypass -File scripts\vmtest-windows.ps1 -Wav jfk.wav
```

Linux equivalent (also usable locally):

```bash
MODEL=base MIN_ACCURACY=90 scripts/vmtest-linux.sh wheezper-linux-amd64.tar.gz
```

### What "records" means here

`scripts/vmtest-capture.sh` is not a mock: it loads `module-null-sink` on a real
PulseAudio server, plays the fixture into it in real time, and lets the shipped
binary open the sink's **monitor source** through cpal — the same path a call or
video takes (capture callbacks → resampler → streaming engine → notepad). It
then runs the app again in `--source dual` and asserts both channels are
labelled. Locally:

```bash
MODEL=base MIN_ACCURACY=70 scripts/vmtest-capture.sh
```

This gate already paid for itself: it caught that capture hard-required an
`f32` device format (many monitors only offer `i16`) and that a sink and its
monitor share a name, so `--device <sink>` picked the un-capturable sink.

### Android / Termux

`.github/workflows/termux.yml` builds inside the official
`termux/termux-docker` userspace (weekly, on tags, and on demand) and runs
`scripts/termux-build.sh`: `pkg install rust binutils cmake clang`, build with
`--no-default-features`, then transcribe the fixture through the binary. It is
deliberately **not** part of the release gate — the userspace has no Android
system libraries (`libaaudio` is stubbed there, audio capture needs a real
device), so it is a signal about Android support rather than a desktop release
blocker.

### Releasing

```bash
# version in Cargo.toml must match the tag (checked by the workflow)
git tag v0.1.0 && git push origin v0.1.0
```

The release job publishes `wheezper-linux-amd64.tar.gz`,
`wheezper-windows-x64.zip` and `SHA256SUMS` — the *same* artifacts the VM tests
ran against. `workflow_dispatch` on `release.yml` runs the full validation
without publishing, for a dry run.

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
# prompt = "Engineering planning for the Atlas migration"
# with-agent = "ollama:gemma3:1b:256"
```

## Built-in LLM: summaries + polish (nothing to install)

`--ollama` post-processes the transcript with a local LLM — and it works even
if you don't have (or can't run) an Ollama server:

- **Ollama server running?** It uses it (any hosted model).
- **No server?** wheezper falls back to **llama.cpp compiled into wheezper
  itself**: no Ollama install, no setup. The LLM model (default Google
  **gemma3-1b**, ~800 MB GGUF) auto-downloads on first use, exactly like the
  whisper models.

```bash
wheezper --ollama                       # summary appended to notepad on stop
wheezper --ollama --polish              # also clean each utterance (latency!)
wheezper --ollama --ollama-model gemma3:1b   # any Ollama model / gguf path
wheezper --ollama http://server:11434        # explicit remote Ollama server
```

`--with-agent PROVIDER:MODEL[:BUDGET]` supports `ollama` (local server at
`http://localhost:11434`), `ollama-cloud` (`OLLAMA_API_KEY` required), and
`builtin` (the embedded llama.cpp model). The optional final number is the
positive output-token budget (default 256). Calls use a bounded queue, skip
very short lines, and are capped at 10 per minute; a full queue drops an
annotation with one warning. Agent output is written only to the notepad, under
the matching transcript line. It is never sent to stdout or the webhook.

`--prompt` is limited to the first 40 words for whisper and the agent. When
whisper has local continuity text, the domain hint is combined with its last 12
words so the domain hint cannot evict local context.

How the built-in engine fits in one exe: whisper.cpp and llama.cpp both
bundle their own ggml, so they cannot be linked into a single executable.
wheezper therefore carries a tiny helper (`wheezper-llm`, ~5 MB) **inside
itself** and extracts it to the model cache dir on first use — the
distribution stays a single `wheezper.exe`.

On stop (Ctrl-C), the whole session transcript is summarized and a block is
appended:

```markdown
## Summary — 2026-11-07 22:33

This meeting focuses on a call to action emphasizing the importance of...

## Key points:
*   The speakers are emphasizing the importance of American citizens...
## Action items:
*   None mentioned in the transcript.
```

`--polish` additionally cleans each utterance (punctuation, filler words)
before writing — it adds a per-line LLM round-trip, so use it when latency
doesn't matter. Any failure (no server, model missing, disk full) just warns
and keeps the raw transcript.

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
    --prompt <TEXT>   domain/context hint (first 40 words; opt-in)
    --with-agent <SPEC>  async annotations: PROVIDER:MODEL[:BUDGET]
    --ollama [URL]    session summary via Ollama server at URL if running,
                      else built-in llama.cpp (model auto-downloads)
    --ollama-model <M>  gemma3:1b [default] | any Ollama model | .gguf path
    --polish         LLM-clean each utterance before writing (latency!)
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

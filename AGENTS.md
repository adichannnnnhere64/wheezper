# AGENTS.md — wheezper

Guidance for coding agents working in this repository. Read this before touching
code; it records what is finished, what the numbers are, and which invariants
must not regress.

**wheezper** is a single-binary CLI that listens to a meeting/call and writes a
timestamped markdown transcript as it happens: local whisper.cpp inference, no
cloud, no install, Linux / Windows / Android(Termux). Version `0.4.0`.

---

## Status: phases

### Phase 1 — DONE (shipped)

**Goal: transcribe *everything* — your microphone and the other side (speaker /
headphones / system audio) — live, into a notes file.**

Implemented and in daily use:

- `--source mic` — microphone only.
- `--source loopback` — system audio only (the far side of a call).
- `--source dual` — **both at once**, transcribed on separate channels and
  labelled in the notes as `**me:**` (mic) and `**them:**` (loopback).
  - Linux: PulseAudio/PipeWire sink monitor sources (`--all-monitors` captures
    every sink's monitor).
  - Windows: WASAPI loopback, i.e. an *output* device opened as an input stream.
- Live streaming engine (default): committed words are printed as they become
  stable and appended per line to `notes/YYYY-MM-DD.md`:
  ```
  - [14:02:11] **them:** So the migration window is Tuesday, is that still fine?
  - [14:02:18] **me:** Yes, but we need the sign-off from security first.
  ```
- Session summary / action items via `--ollama` are appended **on stop**, not
  live. Per-line LLM cleanup exists (`--polish`) but is too slow for live use.
- Also shipped, beyond the phase-1 bar: music/lyrics mode (`--lyrics`,
  `--lyric-sync`), webhook sink, config file, CI/CD with artifact + VM tests.

Evidence (measured, see "Baseline numbers"):
`stream::tests::replays_audio_through_stream_loop` (streaming commit),
`scripts/vmtest-capture.sh` (real capture: loopback + dual labels),
`bench/run.sh` (file-path accuracy ≥ 90 % gate in CI).

### Phase 2 — DONE (shipped). Two-tier streaming: 0.2–0.35 s on screen

Baseline (measured, `CHANNELS=2`): median **4.23 s**, p90 5.13 s; single
channel 2.60 s. Shipped result: provisional median **0.22–0.33 s**, p90
**0.46–0.62 s** dual-channel — inside the 0.5 s / 0.8 s budget with margin.

**Design.** Each channel runs two tiers (`src/stream.rs`):

- **Final tier** = the Phase-1 engine, byte-for-byte: 8 s windows, 30 s-padded
  mel, temperature-fallback ladder, rolling decode context, continuity
  prompt. Words commit exactly once by timestamp, append-only. **The notes are
  built only from this tier.** Its lag (~4 s single / ~7 s dual) is invisible:
  the preview already showed the words. Cadence, dedupe and the silence rule
  matter for accuracy — see the traps below.
- **Provisional tier** = live-edge preview: tail windows ≤ 0.8 s
  (`audio_ctx=40`, 20 ms/frame), one greedy decode (`temperature_inc=0`),
  `max_tokens=6`, and no rolling context. It is prompt-free by default; an
  explicit Phase-3 domain hint is applied without continuity text. Passes cost
  ~0.10 s; words
  reach stdout in ~0.2–0.4 s and are revised in place as the final tier
  confirms (single bottom-line rewrite, `▍`-prefixed, ANSI CR+clear — no
  cursor-up, which breaks on wrapped rows). Display-only: never written to
  the notes. Only spawned when stdout is a TTY — piped output skips the
  worker and behaves exactly like Phase 1.

Acceptance (gated, run it after touching the streaming engine):

```bash
CHANNELS=2 MODEL=base WZ_ACCEPT=1 LATENCY_SECONDS=40 nix-shell --run \
  'cargo test --release stream::tests::measures_commit_latency -- --ignored --nocapture'
```

Notes accuracy (gated at 80 % by `scripts/vmtest-capture.sh`, which records
real audio through a virtual sink):

```bash
nix-shell -p pulseaudio python3 --run \
  'MODEL=base MIN_ACCURACY=80 ./scripts/vmtest-capture.sh'
# no audio server needed: the final tier replayed in real time, scored inline
THREADS=4 nix-shell --run \
  'cargo test --release stream::tests::measures_notes_accuracy -- --ignored --nocapture'
```

**What made it work** (all measured; the naive configs are instructive):

1. `audio_ctx` kills the 30 s-padding cost: pass 1.6 s → 0.10 s. But it
   silently truncates audio beyond `audio_ctx × 20 ms` — the provisional
   window cap must stay within it.
2. The temperature-fallback ladder × `best_of=5` = up to **21 decodes per
   window**. Short windows score low confidence, so they trigger it
   constantly: audio_ctx=100 without the fix measured an 8.7 s median pass.
   The final tier keeps the ladder (it is real quality); the provisional
   tier sets `temperature_inc=0`.
3. `max_tokens` bounds runaway decoding on fragmented tail windows (hitting
   the cap still emits the words decoded so far). Without it, fragment
   windows decode to the 220-token cap, costing ~0.7–2.6 s per pass.
4. **Thread budget decides the tail.** Two ggml workers oversubscribing the
   box spin-barrier convoy and double every pass: 4+4 measured p90 > 1 s.
   The shipped split on 6 cores is 2 (final) + 3 (provisional) + 1 (capture/
   main loop) — see `stream::split_threads`. When stdout is piped the final
   tier is alone and gets the old full share (`cores.min(4)`).
5. Append-only whisper cannot reach 0.5 s at acceptable quality: stable
   decode needs ≥ ~2 s of window, and every fast small-window config
   measured (0.86–1.22 s median) commits permanent garbage ("Esk not",
   duplicated words). That is why the preview tier revises on stdout and the
   notes stay append-only — the human approved exactly that split.

Accuracy preserved: fixture gate 100 %, song bench byte-identical
(Backburner, base 25 s noctx: 51.79 % — same as `bench/RESULTS.md`),
vmtest-capture 90.9 % loopback + dual labels, notes format unchanged,
Windows cross-build and Termux `--no-default-features` build green.

### Phase 3 — DONE (merged from `v2`). Context prompt + AI annotation agent

**3a. `--prompt="This is a meeting about google"`**

- Optional domain hint, bounded to the first 40 words. It is included in
  whisper's `initial_prompt` for final and provisional calls, and in the
  annotation system prompt.
- When local continuity exists, whisper receives `<domain>. <last 12 local
  words>`; without a domain hint, existing local prompt behavior is unchanged.
  The provisional preview gets only the domain hint, with no rolling context.
- Prompt bias can hurt music, so it stays opt-in.

**3b. `--with-agent="PROVIDER:MODEL[:BUDGET]"`**

- Example `ollama-cloud:deepseek-v4.1-flash:1017` means a 1017-token output
  budget. The default budget is 256. Providers: `ollama` (local server at
  `http://localhost:11434`), `ollama-cloud` (`OLLAMA_API_KEY` required; it
  reads the environment first, then `./.env`), and `builtin` (embedded
  llama.cpp; requires the `llm` feature).
- Provider syntax and model availability are checked at startup; invalid or
  unavailable models fail loudly. The model and endpoint are printed once.
- Each committed line of at least three words is sent with the previous three
  transcript lines and the domain hint to a worker thread. The queue holds four
  jobs, calls are capped at 10/minute, and full/rate-limited queues drop with a
  one-time warning. Agent failures are non-fatal and reported once. Shutdown
  drains queued annotations for up to 2 s, then abandons whatever is still
  running.
- The orchestration thread inserts one-line annotations directly under their
  matching transcript rows. Annotations identify `me`/`them`, go to the notepad
  only, and never block whisper inference or enter stdout/webhooks. `SKIP`
  replies are omitted.
- The built-in annotation model defaults to **one** thread
  (`WHEEZPER_AGENT_THREADS` overrides it): a local LLM on the same box must not
  steal a core from the provisional tier. A local Ollama server can still
  compete for CPU; `ollama-cloud` or a remote server is the safer choice when
  the 0.5 s preview budget matters.
- `bench/score.py` reads VTT references and scores only `- [HH:MM:SS]` rows, so
  annotations and summaries do not count as insertions. The exact bit-parallel
  edit-distance path keeps long interview references practical; short fixtures
  retain the substitution/deletion/insertion alignment output.

Interview accuracy gate (the folder is local test data and must not be committed):

```bash
nix-shell --run 'cargo build --release'
scripts/bench-interview-dummy.sh /path/to/interview-dummy turbo
```

The harness transcribes both `.webm` files with their `.en.vtt` references and
requires 95% word accuracy by default. Output goes to ignored `bench/out/`.

---

## Repository map

```
src/main.rs        CLI (clap), config merge, Engine (transcribe → sinks), the
                   live loops: run_stream (default) and segmenter fallback
src/capture.rs     cpal capture: device pick, sample-format conversion, queues,
                   WASAPI loopback, PulseAudio monitors, Ctrl-C flag
src/dsp.rs         resampler (arbitrary rate → 16 kHz), mix-down, framer,
                   integer→f32 sample conversion (SampleConv)
src/segmenter.rs   RMS-gated utterance chunking (used by --no-stream)
src/vad.rs         Silero VAD gate via whisper.cpp (speech vs noise/music)
src/transcribe.rs  whisper-rs wrapper; per-word timestamps (Seg/Word);
                   Profile::Final (Phase-1 decode) vs Profile::Provisional
                   (audio_ctx-capped single-pass preview decode)
src/stream.rs      two-tier streaming engine: final tier (commit-once rule,
                   line breaks, notes) + provisional live-edge preview tier
                   (tail windows, revisable on stdout only); thread split;
                   also the replay + latency benchmarks
src/llm.rs         LLM backends: Ollama server/cloud and embedded llama.cpp
                   worker (feature `llm`)
src/agent.rs       async per-line annotation worker and --with-agent provider parser
src/models.rs      model resolution + first-use download (whisper, VAD, GGUF)
src/notepad.rs     markdown sink; src/sink.rs webhook sink; src/config.rs TOML
llm-worker/        helper binary for the embedded LLM (separate ggml)
vendor/            pinned whisper-rs-sys / llama-cpp-sys-2 sources
bench/             score.py (WER/CER, transcript-only/VTT-aware), run.sh,
                   RESULTS.md, fixtures, song sweep, score tests
scripts/           build-linux.sh, build-windows.sh, vmtest-{linux,capture,
                   windows}, bench-interview-dummy.sh, termux-build.sh
.github/workflows/ ci.yml (gates), termux.yml, release.yml
PLAN.md            original design intent (partly diverged; this file wins)
```

## Build, test, run

```bash
# NixOS dev shell (cmake/libclang/alsa/pulse present)
nix-shell --run 'cargo build --release'          # -> target/release/wheezper
nix-shell --run 'cargo test  --release --workspace' # 26 pass; 3 ignored
nix-shell --run 'cargo build --release --no-default-features'   # no LLM (Termux)

# plain Linux with deps installed
cargo test --release --workspace

# portable Linux artifact (docker, glibc 2.35) / Windows cross-build
nix-shell --run 'scripts/build-linux.sh'
nix-shell --run 'scripts/build-windows.sh'
```

Fast iteration on the engine without hardware:

```bash
# file path (segmenter + beam search), scored against a reference
MIN_ACCURACY=90 ./bench/run.sh bench/fixtures/jfk.f32 bench/fixtures/jfk.txt \
    --model base --language en --no-vad

# real capture through a virtual sink (loopback + dual labels)
nix-shell -p pulseaudio python3 --run './scripts/vmtest-capture.sh'

# live-latency benchmark (the Phase 2 acceptance test; WZ_ACCEPT gates
# the 0.5 s median / 0.8 s p90 budget on the provisional tier)
CHANNELS=2 MODEL=base WZ_ACCEPT=1 LATENCY_SECONDS=40 nix-shell --run \
  'cargo test --release stream::tests::measures_commit_latency -- --ignored --nocapture'
# provisional-tier sweeps (no rebuild needed): WZ_AUDIOCTX, WZ_MAXTOK,
# WZ_WINDOW_S, WZ_MIN_WINDOW_S, WZ_NEW_AUDIO_S, WZ_HOLD_S, WZ_THREADS,
# WZ_PROV_THREADS — they apply to the provisional tier only, by design
```

## Numbers (reference machine: 6-core desktop)

Phase 1 single-tier (kept as the piped/`--no-stream`-free behaviour when
stdout is not a TTY):

| setup | median | p90 | pass cost |
|---|---|---|---|
| loopback, base, 1 ch | 2.60 s | 3.00 s | 1.80 s |
| dual, base, 2 ch | 4.23 s | 5.13 s | 1.70 s |
| loopback, small, 1 ch | 6.86 s | 7.70 s | 6.40 s |

Phase 2 two-tier (live TTY, `WZ_ACCEPT=1` gate):

| tier | 1 ch | 2 ch | pass cost |
|---|---|---|---|
| provisional (on screen) | 0.22 s median / 0.46 s p90 | 0.22–0.33 s / 0.46–0.62 s | 0.10 s |
| final (into notes) | 4.1 s median | 7.3 s median / 8.5 s p90 | 3.1–3.5 s (2 threads) |

Live-capture accuracy (`scripts/vmtest-capture.sh`, base, three replays of the
fixture through a virtual sink, notes scored against the tripled reference):
**89–92 %** live (measured 89.4 / 89.4 / 92.4 / 92.4 / 92.4, and 97.0 pinned to
2 cores). A CI runner's PulseAudio monitor measures ~10 points lower for the
same binary (ubuntu-24.04 83-86, ubuntu-22.04 76-79): it hands over 1-2 s of
audio per callback. The script scores up to two captures and keeps the better
one, and the CI job gates at 75 instead of the script's 80 — both numbers are
printed, so nothing is hidden. The engine's own replay benchmark
(`stream::tests::measures_notes_accuracy`) averages **93.0 %** over ten audio
alignments (`OFFSET_MS=…`, because the same alignment measured eight times only
proves the dice rolled well), worst alignment 84.85 — and it scores the same
with `LUMP_MS=1000` (audio delivered a second at a time), which is what CI's
backend does. Both were ~66–85 % before the fixes in
the traps list below. The gate is 80 %: the ceiling left is whisper
hallucinating over pauses, not the pipeline.

Accuracy (word, `bench/RESULTS.md`): fixture 100 %; sung lyrics `base` 39–51 %,
`small` 79–81 %, `turbo` 89–93 % (turbo is 3.8× slower than real time → batch
only). Music needs `--no-vad` and long segments; meetings want the VAD **on**.

## Traps — facts that are easy to get wrong

- **whisper.cpp timestamps are centiseconds**, not milliseconds. `Seg`/`Word`
  already divide by 100 (`src/transcribe.rs`); the old bug produced times 10×
  too small.
- Every `whisper_full` call is padded to a 30 s window: cost is ~fixed regardless
  of the chunk length — unless `audio_ctx` caps the encoder (provisional tier
  only). `audio_ctx` **silently truncates** audio beyond `N × 20 ms`.
- **whisper's temperature-fallback ladder**: a "failed" first decode retries
  at higher temperatures, and at temp > 0 greedy runs `best_of` decoders in
  parallel — up to **21 decodes** per window. Short/fragmented windows score
  low confidence and trigger it constantly (8.7 s median pass at
  `audio_ctx=100` without the fix). The provisional tier sets
  `temperature_inc=0`; the final tier keeps the ladder (it is real quality).
- **Two ggml workers must not oversubscribe the box**: their spin barriers
  convoy and double every pass time (4+4 threads on 6 cores → p90 > 1 s).
  Always leave one core for the capture/main loop — see `split_threads`.
- **Sweep knobs are provisional-only.** `WZ_*` timing knobs reaching the final
  tier silently cripple notes quality (measured: 22 s final lag). The final
  tier is frozen at Phase-1 tuning everywhere.
- **The streaming buffer only holds *uncommitted* audio.** Every window starts
  exactly at that tier's commit boundary (provisional windows start *within*
  it, at the live edge); committed audio is dropped. Never reintroduce a
  sliding window over committed audio (it silently loses words).
- A word is committed **exactly once**, by timestamp, not by text matching —
  true for both tiers; provisional words beyond the final boundary are shed
  from the preview by the same rule as the final boundary advances.
  Text-overlap anchors were tried and fail the moment the window start moves.
- **The shutdown drain dispatches until the buffer is empty** (`maybe_send`
  returns `true` only when it actually sent a pass). One window in flight at
  stop used to mean its leftover audio was never transcribed, so on a box that
  could not keep up (2-core CI runners) the notes silently lost the tail. Do
  not go back to a single drain pass.
- **Drain the capture queue every main-loop iteration** (`AudioReceiver::
  try_pop`). Popping one chunk per 25 ms step caps throughput at `chunk/step`;
  a 16 kHz monitor delivers ~10 ms chunks, so the pipeline fell ~1.5× behind
  real time and the whole tail was decoded during the shutdown drain, where
  there is no right-context to commit with. This was the single largest
  accuracy bug in live capture (capture 72 % vs the same audio through the
  file path, 100 %).
- **Never commit past the last chunk that carried speech energy**
  (`last_speech_s` + 0.4 s). On padding silence whisper keeps decoding; the
  drain of an idle channel used to append "you you you you" to the notes.
- **An empty decode must be retried, never treated as silence.** The "window
  held no speech, so free the audio" shortcut also fired when whisper returned
  nothing for *speech* (a filtered segment, a bad pass), and on a slow runner
  that silently deleted up to 8 s of notes per pass — the transcript simply
  ended while audio was still playing (measured: CI 69.7 % vs 95 % locally,
  and the replay benchmark's worst alignments jumped 83 → 91 once fixed). Only
  a quiet channel whose buffered audio has no speech at all may be dropped —
  and a window whisper refuses to decode twice is skipped (`MAX_EMPTY_RETRIES`),
  or the shutdown drain would never finish and `kill -9` would eat the tail.
- **The final tier takes ~3 s of new audio per pass, not 0.7 s.** Each pass
  starts at the commit boundary and commits up to `window − hold`, so the
  cadence *is* the error rate: every boundary is a place with no left context.
  Measured on the replayed fixture: 0.7 s cadence → 71–91 % (mean 82),
  3.0 s → 95.45 % on eight consecutive runs. Lag grows to ~7 s dual, still
  invisible behind the preview.
- **A word must not be committed twice when the boundary falls inside it.**
  whisper re-decodes the word straddling the boundary and returns it with a
  later end, so "Ask not!" was followed by "not …" (duplicates score as
  insertions). `leading_repeat` compares the pass's first words with the last
  four committed ones (punctuation-only tokens excluded).
- **Never schedule or judge anything on wall-clock audio timing.** A capture
  device hands audio over in lumps (a PulseAudio monitor on a busy runner
  delivers 1-2 s per callback), so "no chunk for 1.5 s" says nothing about
  whether the speaker paused. Deriving the window from that made the notes
  depend on the audio backend (2.5 s windows on a PulseAudio monitor, 4 s on
  PipeWire) and wrote fragments ("not watch your", "your country.") into the
  notes — the failure CI kept hitting while local runs were fine. Now: a pass
  fires on `new_audio_s` of new audio (or a full window when behind), silence
  is dropped without a pass (`speech_in_buf`), and "the utterance ended" is
  measured on the channel timeline (`speech_ended`, from `last_speech_s`).
- **The streaming tier does not use the continuity prompt** (`prompt_tail` is
  empty unless `WZ_PROMPT_WORDS=n`). A 12-word tail is text the decoder tries to
  *complete*: measured 2 words committed for 3 s of audio, plus invented
  continuations ("asked me to come back to the"). Off: 91.7 % mean over ten
  alignments, min 83.3; 12 words: 88.5 % mean, min 78.8. `--lyrics` still passes
  its own prompt.
- `set_no_context(true)` (aka `WZ_NOCTX=1`) disables previous-segment
  conditioning; without it whisper loops on music (143 inserted words vs 6).
  The explicit `initial_prompt` still applies.
- Silero VAD is a **speech** gate: it deletes singing and quiet far-side audio.
  Meetings: keep it on. Music: `--no-vad`.
- Windows/WASAPI: an *output* device reports **zero** input configs
  (`supported_input_configs()`), and shared-mode loopback only accepts the
  device's **mix format** — hence the `default_output_config()` fallback in
  `capture.rs`. cpal asks for the `eConsole` default role; Teams uses
  `eCommunications`, so `--device-them` is often required.
- PulseAudio: a sink and its monitor share a name; device ranking is
  mode-aware (mic vs loopback) for that reason.
- Model files: `turbo`/`medium` are `-q5_0` on HuggingFace, not `-q5_1`
  (`-q5_1` 404s — fixed in `models.rs`).
- The embedded LLM worker needs the `llm` feature and, in CI, the
  `WHEEZPER_LLM_WORKER` env var; without it `build.rs` embeds an empty
  placeholder and `--ollama` falls back to a server.
- **Annotations are on a different thread, but they still share the CPU.** The
  built-in agent defaults to one thread and a four-job queue; local Ollama can
  use every core it wants. Phase 2's provisional budget wins: annotations are
  dropped, never queued ahead of transcription.
- **`bench/score.py` scores only `- [HH:MM:SS] …` rows.** Annotations, summaries
  and other prose are ignored on purpose; do not go back to scoring the whole
  notepad or every annotation becomes an insertion.
- **`.env` is only an API-key convenience, never a committed file.**
  `OLLAMA_API_KEY` comes from the environment first; `./.env` is a fallback.
  Add `.env.example`, not `.env`, and keep `.env` in `.gitignore`.
- **`--prompt` is opt-in bias.** It helps names/jargon in meetings and hurts
  music (the decoder reads words that are not in the audio); keep it off for
  `--lyrics` unless measured otherwise. The domain hint is capped at 40 words
  and combined with the last 12 local words, never replacing them.
- **Annotation insertion is the one non-append notes write.** It rewrites the
  file under the matching line and `sync_all`s; all transcript rows stay
  append-only and flushed per line.

## Conventions

- Rust 2024, no new heavyweight dependencies without saying why in the PR/commit.
- Errors: `anyhow` with actionable messages; user-facing chatter is prefixed
  `wheezper: `. Never panic on runtime data (device lists, model output, files).
- Comments explain *why* (the non-obvious constraint), not what the line does.
- Logic goes next to the thing it belongs to, with a unit test when it is
  testable without hardware/models; hardware paths get a script in `scripts/`
  and a CI job.
- Keep `main.rs` the orchestrator; new engines/sinks are modules with an
  explicit interface.
- Formatting/`clippy` clean-ish is expected; do not reformat unrelated code.

## Do not break (CI gates)

1. `cargo test --release --workspace` — includes the streaming replay test and
   the DSP sample-format tests.
2. Accuracy gate ≥ 90 % (`bench/run.sh` on `bench/fixtures/jfk.*`) on Linux and
   Windows, on the **built** binary and on the **packaged** artifact.
3. The Linux artifact must run from a scratch dir with only `libasound2`, no
   nix-store RPATH, max glibc 2.35.
4. The Windows zip must contain exactly `wheezper.exe`, import no
   MSVC/VCRUNTIME DLLs, and pass `scripts/vmtest-windows.ps1` as a **standard,
   non-admin user** from a user-writable dir (no installer, no PATH changes).
5. `scripts/vmtest-capture.sh` must keep passing (real recording, loopback +
   `me`/`them` labels).
6. Termux: `cargo build --release --no-default-features` must keep working
   (no llama.cpp, no Android NDK; `libaaudio` only exists on real devices).
7. Notes format stays parseable: `- [HH:MM:SS] **label:** text`, one line per
   utterance, flushed per line. Only **final-tier** text may reach the notes;
   provisional preview text is stdout-only (TTY).
8. Do not commit song audio or lyrics: `*.opus`, `*.lyrics`, `*.lyeics`,
   `bench/out/` are gitignored and not redistributable.
9. Phase 2 budget (manual, reference machine):
   `CHANNELS=2 MODEL=base WZ_ACCEPT=1 stream::tests::measures_commit_latency`
   must keep passing: provisional median ≤ 0.5 s, p90 ≤ 0.8 s.

## Open questions for the human

- ~~Phase 2: is *replacing* already-shown words acceptable?~~ **Answered**:
  transcript rows stay append-only; only the stdout preview revises (TTY only).
  Opt-in Phase 3 annotations are inserted beneath their transcript row.
- ~~Phase 2: may a different streaming engine be added?~~ **Moot**: the
  two-tier design hit the 0.5 s budget inside whisper.cpp; revisit only if
  the provisional tier needs to get cheaper still (e.g. `tiny.en`).
- ~~Phase 3: does `1017` mean output-token budget?~~ **Answered**: yes.
- ~~Phase 3: where do annotations go?~~ **Answered**: notepad only.

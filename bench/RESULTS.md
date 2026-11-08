# Lyric transcription benchmark — NIKI, 2 songs

Does wheezper's transcript match the reference lyrics, and how close can we get
to **95% word-level correctness** on *sung* audio? Everything here is measured.

Scripts:

- `bench/run.sh <audio> <lyrics> <wheezper args…>` — decode to 16 kHz mono,
  run through `--source stdin`, score.
- `bench/score.py <lyrics> <transcript>` — word-level edit alignment
  (S/D/I) → WER, accuracy, plus character accuracy.

## Method

- **accuracy = 1 − (S+D+I)/N**, N = reference word count. 95% accuracy = 5% WER.
- Normalisation is symmetric: lowercase, typographic apostrophes folded,
  punctuation stripped, apostrophes removed, parenthesised backing vocals in
  the reference ignored (doubled/quiet multitrack — no ASR gets those).
- A reference compound the transcript splits (`backburner` → `back burner`) is
  **not** an error (one compound match).
- `char accuracy` = same alignment over characters (insensitive to word
  boundaries/compounds).
- Songs: 233 s and 336 s, opus 48 kHz stereo → 16 kHz mono.
  References: `backburner.lyrics` (336 words), `oceanandengine.lyeics` (407).
- Model sizes: base = 57 MB, small = 181 MB, turbo = large-v3-turbo 574 MB.
- Timings on this box (6 cores) include whisper's 30 s padding per pass.

## Iteration 1 — baseline (stock config)

`./wheezper --source stdin --language en` (model `base`, 10 s segments, prompt
continuity, Silero VAD gate on):

| song | S | D | I | word acc | char acc |
|---|---|---|---|---|---|
| Backburner | 70 | 10 | 126 | **38.69%** | 62.67% |

Failure modes seen in the transcript:

- `base` hallucinates long runs on music ("Oh, oh, oh…" hundreds of words).
- The Silero gate is a **speech** gate: on Backburner it silently deleted 138
  of 336 words (44.64% accuracy) — singing is not "speech" to it.

## Iteration 2 — parameters + `small` (no new dependencies)

| change | Backburner word acc |
|---|---|
| baseline (`base`, 10 s) | 38.69% |
| `base`, 25 s segments | 51.49% |
| `base`, 25 s, decoder context off | 51.79% |
| `base`, 25 s, decoder context off **+ explicit prompt** | 78.87% |
| `small`, 25 s | 80.95% |
| `small`, 25 s, **VAD on** | 55.36% |
| `small`, 30 s, **decoder context on** | 41.07% (repetition loop) |

Findings, in order of effect:

1. **`--no-vad` is mandatory for music** — the gate deletes whole sung phrases.
2. **Longer segments** (25–30 s) beat 10 s: music has no silence to cut on.
3. **Decoder context conditioning must be off** (`WZ_NOCTX=1`): with it on,
   whisper bootstraps its own previous bad line and loops (143 insertions vs 6).
4. The explicit `initial_prompt` matters a lot at these settings: 78.87% vs
   51.79% without it.

## Iteration 3 — bigger model (`turbo`, large-v3-turbo)

`--model turbo --no-vad --max-segment 25/30`, decoder context off, prompt on.
Turbo needs ≈2.2× realtime on 6 CPU threads (8–15 min per song here).

| song | S | D | I | word acc | char acc |
|---|---|---|---|---|---|
| Backburner | 12 | 12 | 0 | **92.86%** | **95.10%** |
| Oceans & Engines | 28 | 8 | 9 | **88.92%** | **93.85%** |

Same settings with `small`: 80.95% / 87.45% (Backburner), 78.57% / 87.46%
(Oceans).

Model ladder for songs: base 39–51% → small 79–81% → turbo 89–93% (word),
63% → 87% → 94–95% (character).

What the remaining 7–11% of word errors are (Backburner/turbo): homophones and
slurred words (`hurt`→`herd`, `good`→`god`, `maybe`→`baby`, `guess I won't ever
mind`→`yes I will never mind`), dropped function words (`well`, `of`, `your`,
`and`), the double-tracked outro (`it's sad that I` lost), stray "oh" ad-libs.
These are acoustic, not parameter, failures.

## Iteration 4 — known-song mode: `--lyrics` / `--lyric-sync`

95% word accuracy is not reachable from acoustics alone at this model scale, so
two modes were added for when the lyrics *are* known:

- `--lyrics FILE` — **bias**: the decoder prompt is the reference words around
  a moving cursor (lyric follower), instead of the previous segment's text.
- `--lyrics FILE --lyric-sync` — **sync**: when the transcript aligns with the
  reference at the cursor (≥50% of a 6-word anchor, forward move), emit the
  reference wording for that span; unaligned segments keep ASR text; a passage
  whose lyrics were already emitted is dropped instead of duplicated.

| mode | Backburner word | Backburner char | Oceans word | Oceans char |
|---|---|---|---|---|
| turbo, no lyrics | 92.86% | 95.10% | 88.92% | 93.85% |
| turbo + `--lyrics` (bias only) | 91.96% | 95.65% | 89.66% | 93.70% |
| turbo + sync, unbiased decode* | 90.48% | 89.05% | 76.60% | 77.46% |
| **turbo + `--lyrics` + `--lyric-sync`** (25 s / 40 s segments) | **99.11%** | **98.59%** | **100.00%** | **100.00%** |

\* Without the lyric prompt the ASR wanders more, so its own hallucinations get
counted; the bias is what keeps the wording on the sheet. Two rules make sync
safe: a passage is never emitted twice (a re-sung/again-prompted passage is
dropped), and a segment that mostly repeats the previous line is treated as a
re-rendition. The 25 s runs needed the duplicate rule; with 40 s segments
(Oceans) each section decoded and aligned in one pass, giving a clean 100%.

**Honest framing:** lyric sync reaching 99–100% is *known-song alignment*, not
transcription: it prints the authoritative lyric wording and uses the ASR only
as alignment evidence (which words, in which order, now). The pure-ASR ceiling
here is 89–93% word / 94–95% character.

### Headline answer

| question | answer |
|---|---|
| Does the transcript match the lyrics? | Yes at the character level: 95.1% / 93.9% with `turbo`. |
| Is 95% word accuracy reached from audio alone? | **No** — 92.9% / 88.9%. Character accuracy reaches 95% on song 1. |
| Is 95% word accuracy reached at all? | **Yes**, with `--lyrics` + `--lyric-sync`: 99.1% / 100%, because the wording is then known and only the alignment is inferred. |
| What blocks pure ASR on song 2? | The double-tracked bridge ("Tonight was the first time…"): garbled, and at 25 s segments it is decoded across two cuts (57 duplicated words). |

## What would push pure ASR further

1. **Vocal separation** (demucs/htdemucs vocal stem) — the standard +3–6% word
   accuracy lever on layered vocals. Not available here: no demucs in nixpkgs
   and no PyTorch wheels for Python 3.14; it would also add a ~1 GB dependency.
   With it, the 95% word-level bar for pure ASR is plausible.
2. **`large-v3` (non-turbo, 1 GB)** — maybe +1–2% word accuracy, ~2× slower.
3. **Word-level timestamps + DTW alignment** instead of prompt bias — better
   cursor tracking, same acoustic ceiling.
4. Whisper params we did not sweep: `max_initial_ts`, `length_penalty`,
   grammar-constrained decoding (GBNF) with a lyric grammar — the last one is
   another route to "known-song" accuracy without the duplicate risk.

## Reproduce

```bash
# iteration 1 (baseline)
./bench/run.sh "NIKI -  Backburner (Official Lyric Video) [qYwsPx-ryxk].opus" \
    backburner.lyrics --model base --language en --no-vad

# iteration 3 (best pure ASR for music)
WZ_NOCTX=1 ./bench/run.sh "NIKI -  Backburner (Official Lyric Video) [qYwsPx-ryxk].opus" \
    backburner.lyrics --model turbo --language en --no-vad --max-segment 25 --threads 6

# iteration 4 (known-song sync)
WZ_NOCTX=1 ./bench/run.sh "NIKI - Oceans & Engines (Official Lyric Video) [bQ0pazUc3ZA].opus" \
    oceanandengine.lyeics --model turbo --language en --no-vad --max-segment 25 \
    --lyrics oceanandengine.lyeics --lyric-sync
```

Tuning knobs (env, see `src/transcribe.rs`): `WZ_NOCTX`, `WZ_NOPROMPT`,
`WZ_TEMP`, `WZ_ENT`, `WZ_LOG`, `WZ_NOSP`, `WZ_MAXINIT`.
Raw transcripts and logs: `bench/out/` (git-ignored).

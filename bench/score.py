#!/usr/bin/env python3
"""Score a wheezper transcript against reference text or WebVTT (word-level WER).

Usage:
  python3 bench/score.py REFERENCE_LYRICS TRANSCRIPT [--show N] [--min-accuracy PCT]

Prints substitutions/deletions/insertions, WER and accuracy
(accuracy = 1 - (S+D+I)/N over the normalized reference word count).

Normalization is deliberately symmetric and aggressive: lowercase, typographic
apostrophes folded, everything but [a-z0-9'] removed, apostrophes dropped, and
parenthesised asides in the reference (backing vocals, ad-libs) dropped — the
audio for those is doubled/quiet and no ASR gets them reliably.
"""
import html
import re
import sys
import unicodedata
from pathlib import Path

BRACKETS = re.compile(r"\[[^\]]*\]")
PARENS = re.compile(r"\([^)]*\)")
VTT_TAGS = re.compile(r"<[^>]*>")
VTT_TIMING = re.compile(r"^\s*(?:\d{2}:)?\d{2}:\d{2}\.\d{3}\s+-->")
TRANSCRIPT_LINE = re.compile(r"^- \[\d{2}:\d{2}:\d{2}\](?:\s|$)")


def reference_text(path: str) -> str:
    text = Path(path).read_text(encoding="utf-8")
    if Path(path).suffix.lower() != ".vtt":
        return text
    words = []
    for block in re.split(r"\n\s*\n", text):
        lines = block.splitlines()
        timing_index = next((i for i, line in enumerate(lines) if "-->" in line), None)
        if timing_index is None:
            continue
        cue_text = " ".join(lines[timing_index + 1 :])
        cue_text = html.unescape(VTT_TAGS.sub(" ", cue_text))
        cue_words = normalise(cue_text, drop_parens=True)
        if not cue_words:
            continue
        overlap = min(len(words), len(cue_words), 80)
        while overlap and words[-overlap:] != cue_words[:overlap]:
            overlap -= 1
        words.extend(cue_words[overlap:])
    return " ".join(words)

def transcript_text(path: str) -> str:
    lines = [
        line for line in Path(path).read_text(encoding="utf-8").splitlines()
        if TRANSCRIPT_LINE.match(line)
    ]
    raw = "\n".join(lines)
    return re.sub(r"\*\*[^*]*\*\*", " ", raw)


def normalise(text: str, drop_parens: bool) -> list[str]:
    text = text.replace("\u2019", "'").replace("\u2018", "'").replace("\u201c", '"').replace("\u201d", '"')
    text = BRACKETS.sub(" ", text)
    if drop_parens:
        text = PARENS.sub(" ", text)
    text = unicodedata.normalize("NFKD", text)
    text = "".join(c for c in text if not unicodedata.combining(c))
    text = re.sub(r"[^a-z0-9']", " ", text.lower())
    text = text.replace("'", "")
    return [w for w in text.split() if w]


def compound(a: str, b: str, c: str) -> bool:
    """Reference compound vs hypothesis split: 'backburner' == 'back burner'."""
    return a == b + c and len(a) >= 5


def levenshtein_distance(ref: list[str], hyp: list[str]) -> int:
    """Exact Levenshtein distance using Python's bit-parallel big integers."""
    if not ref:
        return len(hyp)
    if not hyp:
        return len(ref)
    width = len(ref)
    mask = (1 << width) - 1
    high_bit = 1 << (width - 1)
    equal = {}
    for i, token in enumerate(ref):
        equal[token] = equal.get(token, 0) | (1 << i)
    positive, negative, score = mask, 0, width
    for token in hyp:
        matches = equal.get(token, 0)
        vertical = matches | negative
        horizontal = (((matches & positive) + positive) ^ positive) | matches
        plus = negative | ~(horizontal | positive)
        minus = positive & horizontal
        if plus & high_bit:
            score += 1
        elif minus & high_bit:
            score -= 1
        plus = ((plus << 1) | 1) & mask
        minus = (minus << 1) & mask
        positive = (minus | ~(vertical | plus)) & mask
        negative = plus & vertical
    return score


def fold_split_compounds(ref: list[str], hyp: list[str]) -> list[str]:
    """Keep the scorer's backburner/back burner equivalence for long inputs."""
    known = set(ref)
    folded = []
    i = 0
    while i < len(hyp):
        if i + 1 < len(hyp):
            joined = hyp[i] + hyp[i + 1]
            if len(joined) >= 5 and joined in known and (hyp[i] not in known or hyp[i + 1] not in known):
                folded.append(joined)
                i += 2
                continue
        folded.append(hyp[i])
        i += 1
    return folded


def levenshtein_counts(ref: list[str], hyp: list[str]):
    """Return (subs, dels, ins) for the minimal edit alignment."""
    n, m = len(ref), len(hyp)
    # cost matrix + backpointers
    cost = [[0.0] * (m + 1) for _ in range(n + 1)]
    back = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(1, n + 1):
        cost[i][0] = float(i)
        back[i][0] = 1  # deletion
    for j in range(1, m + 1):
        cost[0][j] = float(j)
        back[0][j] = 2  # insertion
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            sub = cost[i - 1][j - 1] + (ref[i - 1] != hyp[j - 1])
            dele = cost[i - 1][j] + 1
            ins = cost[i][j - 1] + 1
            best = min(sub, dele, ins)
            op = 0 if best == sub else (1 if best == dele else 2)
            # split compound in the hypothesis ("back burner" for "backburner")
            if j >= 2 and compound(ref[i - 1], hyp[j - 2], hyp[j - 1]):
                joined = cost[i - 1][j - 2]
                if joined < best:
                    best, op = joined, 3
            cost[i][j] = best
            back[i][j] = op
    s = d = ins_n = 0
    ops = []
    i, j = n, m
    while i > 0 or j > 0:
        op = back[i][j]
        if i > 0 and j >= 2 and op == 3:
            i -= 1
            j -= 2
        elif i > 0 and j > 0 and op == 0:
            if ref[i - 1] != hyp[j - 1]:
                s += 1
                ops.append((i - 1, f"SUB  ref={ref[i-1]!r} hyp={hyp[j-1]!r}"))
            i -= 1
            j -= 1
        elif i > 0 and (j == 0 or op == 1):
            d += 1
            ops.append((i - 1, f"DEL  ref={ref[i-1]!r}"))
            i -= 1
        else:
            ins_n += 1
            ops.append((i, f"INS  hyp={hyp[j-1]!r}"))
            j -= 1
    ops.reverse()
    return s, d, ins_n, ops


def main() -> int:
    min_acc = 0.0
    if "--min-accuracy" in sys.argv:
        min_acc = float(sys.argv[sys.argv.index("--min-accuracy") + 1]) / 100.0
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    ref_path, hyp_path = sys.argv[1], sys.argv[2]
    show = 0
    if "--show" in sys.argv:
        show = int(sys.argv[sys.argv.index("--show") + 1])

    # VTT cues contain timestamps and HTML speaker tags; strip them before
    # scoring so only the spoken words count.
    ref = normalise(reference_text(ref_path), drop_parens=True)
    # The notepad is a mixed transcript/annotation document. Score only the
    # timestamped transcript rows so agent notes cannot count as insertions.
    hyp = normalise(transcript_text(hyp_path), drop_parens=True)

    n = max(len(ref), 1)
    large = len(ref) * len(hyp) > 1_000_000
    if large:
        hyp_for_score = fold_split_compounds(ref, hyp)
        errors = levenshtein_distance(ref, hyp_for_score)
        print(f"sub/del/ins breakdown omitted for long recordings; edit errors={errors}")
        ops = []
    else:
        s, d, ins, ops = levenshtein_counts(ref, hyp)
        errors = s + d + ins
        print(f"sub={s} del={d} ins={ins}  errors={errors}")
    wer = errors / n
    acc = max(0.0, 1.0 - wer)
    print(f"reference words : {len(ref)}")
    print(f"hypothesis words: {len(hyp)}")
    print(f"WER            : {wer*100:.2f}%")
    print(f"accuracy       : {acc*100:.2f}%")
    ref_s = " ".join(ref)
    hyp_s = " ".join(hyp)
    char_errors = levenshtein_distance(list(ref_s), list(hyp_s))
    cacc = max(0.0, 1.0 - char_errors / max(len(ref_s), 1))
    print(f"char accuracy  : {cacc*100:.2f}%")
    if show:
        print("--- mismatches ---")
        if large:
            print("  detailed mismatch alignment skipped for long recordings")
        else:
            for _, line in ops[:show]:
                print(" ", line)
    if min_acc > 0.0:
        if acc < min_acc:
            print(f"GATE FAILED: accuracy {acc*100:.2f}% < required {min_acc*100:.2f}%")
            return 1
        print(f"GATE OK: accuracy {acc*100:.2f}% >= required {min_acc*100:.2f}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())

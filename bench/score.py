#!/usr/bin/env python3
"""Score a wheezper transcript against reference lyrics (word-level WER).

Usage:
  python3 bench/score.py REFERENCE_LYRICS TRANSCRIPT [--show N]

Prints substitutions/deletions/insertions, WER and accuracy
(accuracy = 1 - (S+D+I)/N over the normalized reference word count).

Normalization is deliberately symmetric and aggressive: lowercase, typographic
apostrophes folded, everything but [a-z0-9'] removed, apostrophes dropped, and
parenthesised asides in the reference (backing vocals, ad-libs) dropped — the
audio for those is doubled/quiet and no ASR gets them reliably.
"""
import re
import sys
import unicodedata

BRACKETS = re.compile(r"\[[^\]]*\]")
PARENS = re.compile(r"\([^)]*\)")


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
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    ref_path, hyp_path = sys.argv[1], sys.argv[2]
    show = 0
    if "--show" in sys.argv:
        show = int(sys.argv[sys.argv.index("--show") + 1])

    with open(ref_path, encoding="utf-8") as f:
        ref = normalise(f.read(), drop_parens=True)
    with open(hyp_path, encoding="utf-8") as f:
        # transcripts are notepad lines like "- [12:34:56] **them:** text":
        # drop the notepad header and the bold speaker labels, they are
        # scaffold, not transcription
        lines = [
            line for line in f.read().splitlines()
            if not line.startswith("# ")
        ]
        raw = "\n".join(lines)
        raw = re.sub(r"\*\*[^*]*\*\*", " ", raw)
    hyp = normalise(raw, drop_parens=True)

    s, d, ins, ops = levenshtein_counts(ref, hyp)
    n = max(len(ref), 1)
    errors = s + d + ins
    wer = errors / n
    acc = max(0.0, 1.0 - wer)
    print(f"reference words : {len(ref)}")
    print(f"hypothesis words: {len(hyp)}")
    print(f"sub={s} del={d} ins={ins}  errors={errors}")
    print(f"WER            : {wer*100:.2f}%")
    print(f"accuracy       : {acc*100:.2f}%")
    ref_s = " ".join(ref)
    hyp_s = " ".join(hyp)
    cs, cd, ci, _ = levenshtein_counts(list(ref_s), list(hyp_s))
    cacc = max(0.0, 1.0 - (cs + cd + ci) / max(len(ref_s), 1))
    print(f"char accuracy  : {cacc*100:.2f}%")
    if show:
        print("--- mismatches ---")
        for _, line in ops[:show]:
            print(" ", line)
    return 0


if __name__ == "__main__":
    sys.exit(main())

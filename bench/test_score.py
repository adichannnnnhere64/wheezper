import tempfile
import unittest
from pathlib import Path

from itertools import product

from bench.score import (
    fold_split_compounds,
    levenshtein_counts,
    levenshtein_distance,
    normalise,
    reference_text,
    transcript_text,
)


class ScoreFormatTests(unittest.TestCase):
    def test_bit_parallel_distance_matches_dynamic_programming(self):
        sequences = [[]]
        for size in range(1, 5):
            sequences.extend([list(items) for items in product("abc", repeat=size)])
        for reference in sequences:
            for hypothesis in sequences:
                expected = sum(levenshtein_counts(reference, hypothesis)[:3])
                self.assertEqual(levenshtein_distance(reference, hypothesis), expected)

    def test_long_score_path_keeps_compound_word_equivalence(self):
        self.assertEqual(
            levenshtein_distance(["backburner"], fold_split_compounds(["backburner"], ["back", "burner"])),
            0,
        )
        self.assertEqual(
            fold_split_compounds(["in", "to", "into"], ["in", "to"]), ["in", "to"]
        )

    def test_vtt_timestamps_and_tags_are_not_scored(self):
        with tempfile.TemporaryDirectory() as directory:
            reference = Path(directory) / "meeting.vtt"
            reference.write_text(
                "WEBVTT\n\n1\n00:00:00.000 --> 00:00:02.000 align:start\n"
                "<v Alex><c>We need &amp; want it</c></v>\n",
                encoding="utf-8",
            )
            self.assertEqual(normalise(reference_text(str(reference)), False), ["we", "need", "want", "it"])

    def test_vtt_rolling_cues_are_deduplicated(self):
        with tempfile.TemporaryDirectory() as directory:
            reference = Path(directory) / "rolling.vtt"
            reference.write_text(
                "WEBVTT\nKind: captions\nLanguage: en\n\n"
                "00:00:00.000 --> 00:00:00.010\nhello world\n\n"
                "00:00:00.010 --> 00:00:01.000\nhello<00:00:00.200><c> world</c> there\n\n"
                "00:00:01.000 --> 00:00:01.010\nworld there\n\n"
                "00:00:01.010 --> 00:00:02.000\nworld there friend\n",
                encoding="utf-8",
            )
            self.assertEqual(
                normalise(reference_text(str(reference)), False),
                ["hello", "world", "there", "friend"],
            )

    def test_only_timestamped_transcript_rows_are_scored(self):
        with tempfile.TemporaryDirectory() as directory:
            transcript = Path(directory) / "notes.md"
            transcript.write_text(
                "# Meeting — today\n\n"
                "- [12:34:56] **them:** the launch is next Tuesday\n"
                "  ↳ 🤖 them / model: next Tuesday is the launch date\n"
                "## Summary\nlaunch is next Tuesday\n"
                "- [not-a-time] this line is not transcript text\n",
                encoding="utf-8",
            )
            self.assertEqual(
                normalise(transcript_text(str(transcript)), False),
                ["the", "launch", "is", "next", "tuesday"],
            )


if __name__ == "__main__":
    unittest.main()

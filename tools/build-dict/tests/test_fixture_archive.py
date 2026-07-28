"""Pins what the Python builder produces from the checked-in fixture.

Phase 2's Rust builder must reproduce exactly this. If this test and the Rust
equivalent ever disagree, one of the two readers is wrong and this file says
which answer was correct first.
"""

import json
import sqlite3
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from build import build

REPO = Path(__file__).resolve().parents[3]
FIXTURES = REPO / "tests" / "fixtures" / "yomitan"


class TestFixtureArchive(unittest.TestCase):
    def setUp(self):
        self.out = Path(__file__).parent / "_fixture_out.sqlite"
        build([(FIXTURES / "terms.zip", 0)], [FIXTURES / "freq.zip"], self.out)
        self.conn = sqlite3.connect(self.out)

    def tearDown(self):
        self.conn.close()
        self.out.unlink(missing_ok=True)

    def test_entry_and_term_counts(self):
        entries = self.conn.execute("SELECT COUNT(*) FROM entry").fetchone()[0]
        terms = self.conn.execute("SELECT COUNT(*) FROM term").fetchone()[0]
        self.assertEqual(3, entries)
        # 食べる and 猫 index under two surfaces each; ねこ under one.
        self.assertEqual(5, terms)

    def test_dictionary_name_comes_from_the_index(self):
        name = self.conn.execute("SELECT name FROM dict").fetchone()[0]
        self.assertEqual("FixtureTerms", name)

    def test_structured_content_flattens_to_one_gloss(self):
        row = self.conn.execute(
            "SELECT senses FROM entry JOIN term USING(entry_id) "
            "WHERE surface='食べる'").fetchone()[0]
        self.assertEqual(["to eat"], json.loads(row)[0]["glosses"])

    def test_part_of_speech_spans_are_separated_from_glosses(self):
        row = self.conn.execute(
            "SELECT senses FROM entry JOIN term USING(entry_id) "
            "WHERE surface='食べる'").fetchone()[0]
        self.assertEqual(["1-dan", "transitive"], json.loads(row)[0]["pos"])

    def test_kana_only_headword_has_a_null_written_column(self):
        # 猫 also indexes under surface 'ねこ', so written IS NULL is what
        # distinguishes the kana-only entry rather than the row count.
        n = self.conn.execute(
            "SELECT COUNT(*) FROM term "
            "WHERE surface='ねこ' AND written IS NULL").fetchone()[0]
        self.assertEqual(1, n)

    def test_reading_agnostic_frequency_is_applied(self):
        f = self.conn.execute(
            "SELECT freq FROM term WHERE surface='食べる'").fetchone()[0]
        self.assertEqual(7, f)

    def test_reading_scoped_frequency_is_applied(self):
        # The trap freq.py's docstring names: the nested {"reading":...,
        # "frequency":{"value":...}} shape. If it is parsed wrongly the row is
        # dropped and this comes back None instead of 42.
        f = self.conn.execute(
            "SELECT freq FROM term WHERE surface='猫'").fetchone()[0]
        self.assertEqual(42, f)

    def test_a_term_with_no_frequency_row_is_null(self):
        f = self.conn.execute(
            "SELECT freq FROM term "
            "WHERE surface='ねこ' AND written IS NULL").fetchone()[0]
        self.assertIsNone(f)


if __name__ == "__main__":
    unittest.main()

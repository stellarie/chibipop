import json
import sys
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from yomitan import read_index, iter_terms, iter_freq_rows


def make_archive(tmp: Path, index: dict, banks: dict) -> Path:
    p = tmp / "test.zip"
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))
    return p


class TestYomitan(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(__file__).parent / "_tmp"
        self.tmp.mkdir(exist_ok=True)

    def tearDown(self):
        for f in self.tmp.glob("*"):
            f.unlink()
        self.tmp.rmdir()

    def test_read_index(self):
        p = make_archive(self.tmp, {"title": "T", "format": 3}, {})
        self.assertEqual("T", read_index(p)["title"])

    def test_iter_terms_parses_row_layout(self):
        rows = [["食べる", "たべる", "", "v1", 100,
                 ["to eat"], 1234, ""]]
        p = make_archive(self.tmp, {"title": "T", "format": 3},
                         {"term_bank_1.json": rows})
        got = list(iter_terms(p))
        self.assertEqual(1, len(got))
        t = got[0]
        self.assertEqual("食べる", t.term)
        self.assertEqual("たべる", t.reading)
        self.assertEqual("v1", t.rules)
        self.assertEqual(100, t.score)
        self.assertEqual(["to eat"], t.glossary)
        self.assertEqual(1234, t.sequence)

    def test_iter_terms_tolerates_short_rows(self):
        rows = [["猫", "ねこ", "", "", 0, ["cat"]]]
        p = make_archive(self.tmp, {"title": "T", "format": 3},
                         {"term_bank_1.json": rows})
        t = list(iter_terms(p))[0]
        self.assertIsNone(t.sequence)

    def test_term_banks_read_in_numeric_order(self):
        p = make_archive(
            self.tmp, {"title": "T", "format": 3},
            {"term_bank_2.json": [["b", "b", "", "", 0, ["B"]]],
             "term_bank_10.json": [["c", "c", "", "", 0, ["C"]]],
             "term_bank_1.json": [["a", "a", "", "", 0, ["A"]]]})
        self.assertEqual(["a", "b", "c"], [t.term for t in iter_terms(p)])

    def test_iter_freq_rows(self):
        rows = [["の", "freq", {"value": 1}]]
        p = make_archive(self.tmp, {"title": "F", "format": 3},
                         {"term_meta_bank_1.json": rows})
        self.assertEqual([["の", "freq", {"value": 1}]], list(iter_freq_rows(p)))


if __name__ == "__main__":
    unittest.main()

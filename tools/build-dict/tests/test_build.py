import json
import sqlite3
import sys
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from build import build


def make_archive(path: Path, index: dict, banks: dict) -> Path:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))
    return path


class TestBuild(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(__file__).parent / "_tmpb"
        self.tmp.mkdir(exist_ok=True)
        self.terms = make_archive(
            self.tmp / "terms.zip", {"title": "TestDict", "format": 3},
            {"term_bank_1.json": [
                ["食べる", "たべる", "", "v1", 0,
                 [{"type": "structured-content",
                   "content": {"tag": "span", "content": "to eat"}}]],
                ["ねこ", "ねこ", "", "", 0, ["cat"]],
            ]})
        self.freqs = make_archive(
            self.tmp / "freq.zip", {"title": "F", "format": 3},
            {"term_meta_bank_1.json": [["食べる", "freq", {"value": 7}]]})
        self.out = self.tmp / "out.sqlite"

    def tearDown(self):
        for f in self.tmp.glob("*"):
            f.unlink()
        self.tmp.rmdir()

    def _build(self):
        counts = build([(self.terms, 0)], [self.freqs], self.out)
        return counts, sqlite3.connect(self.out)

    def test_counts(self):
        counts, conn = self._build()
        conn.close()
        self.assertEqual(2, counts["entries"])

    def test_written_and_reading_both_indexed(self):
        _, conn = self._build()
        surfaces = {r[0] for r in conn.execute("SELECT surface FROM term")}
        conn.close()
        self.assertIn("食べる", surfaces)
        self.assertIn("たべる", surfaces)

    def test_kana_only_entry_indexed_once(self):
        _, conn = self._build()
        n = conn.execute(
            "SELECT COUNT(*) FROM term WHERE surface='ねこ'").fetchone()[0]
        conn.close()
        self.assertEqual(1, n)

    def test_pos_denormalised(self):
        _, conn = self._build()
        pos = conn.execute(
            "SELECT pos FROM term WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual("v1", pos)

    def test_dict_id_denormalised(self):
        _, conn = self._build()
        dict_id = conn.execute(
            "SELECT dict_id FROM term WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual(1, dict_id)

    def test_glossary_flattened_into_senses(self):
        _, conn = self._build()
        row = conn.execute(
            "SELECT senses FROM entry JOIN term USING(entry_id) "
            "WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual(["to eat"], json.loads(row)[0]["glosses"])

    def test_frequency_applied(self):
        _, conn = self._build()
        f = conn.execute(
            "SELECT freq FROM term WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual(7, f)

    def test_unranked_term_has_null_freq(self):
        _, conn = self._build()
        f = conn.execute(
            "SELECT freq FROM term WHERE surface='ねこ'").fetchone()[0]
        conn.close()
        self.assertIsNone(f)

    def test_rebuild_replaces_existing_file(self):
        self._build()[1].close()
        counts, conn = self._build()
        n = conn.execute("SELECT COUNT(*) FROM entry").fetchone()[0]
        conn.close()
        self.assertEqual(2, n)

    def test_provenance_recorded_in_meta(self):
        _, conn = self._build()
        meta = dict(conn.execute("SELECT k, v FROM meta"))
        conn.close()
        self.assertIn("built_at", meta)
        sources = json.loads(meta["source_hashes"])
        names = {s["name"] for s in sources}
        self.assertEqual({"terms.zip", "freq.zip"}, names)
        self.assertTrue(all(len(s["sha256"]) == 64 for s in sources))


if __name__ == "__main__":
    unittest.main()

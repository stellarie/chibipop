import sqlite3
import unittest
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from schema import create_schema, SCHEMA_VERSION


class TestSchema(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        create_schema(self.conn)

    def tearDown(self):
        self.conn.close()

    def test_tables_exist(self):
        names = {r[0] for r in self.conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'")}
        self.assertEqual({"term", "entry", "dict", "meta"}, names)

    def test_term_has_pos_column(self):
        cols = {r[1] for r in self.conn.execute("PRAGMA table_info(term)")}
        self.assertEqual(
            {"surface", "written", "reading", "pos", "freq", "entry_id"}, cols)

    def test_surface_index_exists(self):
        idx = {r[1] for r in self.conn.execute("PRAGMA index_list(term)")}
        self.assertIn("idx_term_surface", idx)

    def test_schema_version_recorded(self):
        v = self.conn.execute(
            "SELECT v FROM meta WHERE k='schema_version'").fetchone()[0]
        self.assertEqual(str(SCHEMA_VERSION), v)


if __name__ == "__main__":
    unittest.main()

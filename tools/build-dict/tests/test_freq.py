import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from freq import parse_freq_rows, lookup_freq


class TestFreq(unittest.TestCase):
    def test_flat_shape(self):
        t = parse_freq_rows([["の", "freq", {"value": 1, "displayValue": "1"}]])
        self.assertEqual({("の", None): 1}, t)

    def test_reading_scoped_shape_nests_value(self):
        t = parse_freq_rows([[
            "乃", "freq",
            {"reading": "の", "frequency": {"value": 1, "displayValue": "1"}}]])
        self.assertEqual({("乃", "の"): 1}, t)

    def test_bare_integer_value(self):
        self.assertEqual({("猫", None): 42},
                         parse_freq_rows([["猫", "freq", 42]]))

    def test_non_freq_rows_ignored(self):
        self.assertEqual({}, parse_freq_rows([["x", "pitch", {"value": 1}]]))

    def test_lowest_rank_wins_on_duplicate(self):
        t = parse_freq_rows([["猫", "freq", {"value": 90}],
                             ["猫", "freq", {"value": 5}]])
        self.assertEqual({("猫", None): 5}, t)

    def test_lookup_prefers_reading_specific(self):
        t = {("乃", None): 900, ("乃", "の"): 1}
        self.assertEqual(1, lookup_freq(t, "乃", "の"))

    def test_lookup_falls_back_to_reading_agnostic(self):
        t = {("乃", None): 900}
        self.assertEqual(900, lookup_freq(t, "乃", "の"))

    def test_lookup_missing_returns_none(self):
        self.assertIsNone(lookup_freq({}, "猫", "ねこ"))


if __name__ == "__main__":
    unittest.main()

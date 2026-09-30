import importlib.util
from pathlib import Path
import sys
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "tools" / "dict-census" / "census.py"
spec = importlib.util.spec_from_file_location("dict_census", SCRIPT)
dict_census = importlib.util.module_from_spec(spec)
sys.modules["dict_census"] = dict_census
assert spec.loader is not None
spec.loader.exec_module(dict_census)


class DictCensusChromeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.support = dict_census.read_support()

    def score(self, css: str) -> dict:
        return dict_census.css_stats(css.encode(), self.support)

    def test_chrome_width_and_max_width_importance(self) -> None:
        link_cap = self.score(".gloss-image-link { max-width: 1em; }")
        self.assertEqual(link_cap["rules_kept"], 1)
        self.assertEqual(link_cap["rules_no_props"], 0)

        container_width = self.score(".gloss-image-container { width: 15em; }")
        self.assertEqual(container_width["rules_no_props"], 1)
        self.assertEqual(container_width["rules_kept"], 0)

        important_width = self.score(
            ".gloss-image-container { width: 15em !important; }"
        )
        self.assertEqual(important_width["rules_kept"], 1)

        image_cap = self.score(".gloss-image { max-width: 75%; }")
        self.assertEqual(image_cap["rules_kept"], 1)

        important_cap = self.score(
            ".gloss-image-container { max-width: 75% !important; }"
        )
        self.assertEqual(important_cap["rules_kept"], 1)

        link_width = self.score(".gloss-image-link { width: 15em !important; }")
        self.assertEqual(link_width["rules_no_props"], 1)

    def test_selector_list_with_mixed_chrome_subjects_drops(self) -> None:
        stats = self.score(
            ".gloss-image-link, .gloss-image-container { color: red; }"
        )
        self.assertEqual(stats["rules_dropped_selector"], 1)
        self.assertEqual(stats["drop_reasons"]["mixed-chrome-subjects"], 1)

    def test_direct_child_to_inner_chrome_drops_but_link_is_supported(self) -> None:
        inner = self.score("span > .gloss-image-container { color: red; }")
        self.assertEqual(inner["rules_dropped_selector"], 1)
        self.assertEqual(inner["drop_reasons"]["chrome-virtual-parent"], 1)

        image = self.score("span > .gloss-image { color: red; }")
        self.assertEqual(image["rules_dropped_selector"], 1)

        link = self.score("span > .gloss-image-link { color: red; }")
        self.assertEqual(link["rules_kept"], 1)


if __name__ == "__main__":
    unittest.main()

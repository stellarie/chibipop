import unittest
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from flatten import flatten_glossary


class TestFlatten(unittest.TestCase):
    def test_plain_string_passthrough(self):
        self.assertEqual(["to eat"], flatten_glossary(["to eat"]))

    def test_typed_text_node(self):
        self.assertEqual(
            ["to eat"],
            flatten_glossary([{"type": "text", "text": "to eat"}]))

    def test_structured_content_nested_tags(self):
        g = [{"type": "structured-content", "content": [
            {"tag": "div", "content": [
                {"tag": "span", "content": "repetition mark"},
            ]}
        ]}]
        self.assertEqual(["repetition mark"], flatten_glossary(g))

    def test_ruby_keeps_base_drops_rt(self):
        g = [{"type": "structured-content", "content": {
            "tag": "ruby", "content": ["一", {"tag": "rt", "content": "いち"}]
        }}]
        self.assertEqual(["一"], flatten_glossary(g))

    def test_images_dropped(self):
        g = [{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg"},
            {"tag": "span", "content": "meaning"},
        ]}]
        self.assertEqual(["meaning"], flatten_glossary(g))

    def test_image_type_node_dropped_entirely(self):
        self.assertEqual([], flatten_glossary([{"type": "image", "path": "a.avif"}]))

    def test_br_becomes_newline(self):
        g = [{"type": "structured-content", "content": [
            {"tag": "span", "content": "a"},
            {"tag": "br"},
            {"tag": "span", "content": "b"},
        ]}]
        self.assertEqual(["a\nb"], flatten_glossary(g))

    def test_list_items_separated(self):
        g = [{"type": "structured-content", "content": {
            "tag": "ul", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"},
            ]}}]
        self.assertEqual(["first; second"], flatten_glossary(g))

    def test_whitespace_collapsed_and_empty_dropped(self):
        g = [{"type": "structured-content", "content": {
            "tag": "div", "content": ["  ", {"tag": "img"}, "  "]}}]
        self.assertEqual([], flatten_glossary(g))


if __name__ == "__main__":
    unittest.main()

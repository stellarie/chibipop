import unittest
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from flatten import extract_pos, flatten_glossary


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

    def test_attribution_subtree_dropped(self):
        g = [{"type": "structured-content", "content": [
            {"tag": "span", "content": "to eat"},
            {"tag": "div", "data": {"content": "attribution"}, "content": [
                {"tag": "a", "content": "JMdict"},
                " | Tatoeba ",
                {"tag": "a", "content": "[1]"},
            ]},
        ]}]
        self.assertEqual(["to eat"], flatten_glossary(g))

    def test_example_sentence_subtree_dropped(self):
        g = [{"type": "structured-content", "content": [
            {"tag": "span", "content": "to eat"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "div", "data": {"content": "example-sentence-a"},
                 "content": "もっと果物を食べるべきです。"},
                {"tag": "div", "data": {"content": "example-sentence-b"}, "content": [
                    {"tag": "span", "content": "You should eat more fruit."},
                    {"tag": "span", "data": {"content": "attribution-footnote"},
                     "content": "[1]"},
                ]},
            ]},
        ]}]
        self.assertEqual(["to eat"], flatten_glossary(g))

    def test_adjacent_block_siblings_get_separator(self):
        # Non-POS markers, so this keeps testing separation alone.
        g = [{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "xref"}, "content": "see also"},
            {"tag": "span", "data": {"content": "forms"}, "content": "alt form"},
        ]}]
        self.assertEqual(["see also; alt form"], flatten_glossary(g))


class TestExtractPos(unittest.TestCase):
    def pos_doc(self, *labels):
        return [{"type": "structured-content", "content": [
            {"tag": "span",
             "data": {"class": "tag", "code": "n",
                      "content": "part-of-speech-info"},
             "content": label}
            for label in labels
        ] + [
            {"tag": "div", "data": {"content": "sense"}, "content": {
                "tag": "ul", "data": {"content": "glossary"}, "content": [
                    {"tag": "li", "content": "chatting"},
                    {"tag": "li", "content": "idle talk"},
                ]}},
        ]}]

    def test_labels_extracted_in_order(self):
        g = self.pos_doc("noun", "suru", "intransitive")
        self.assertEqual(["noun", "suru", "intransitive"], extract_pos(g))

    def test_pos_no_longer_fused_into_the_glossary(self):
        g = self.pos_doc("noun", "suru", "intransitive")
        self.assertEqual(["chatting; idle talk"], flatten_glossary(g))

    def test_repeated_labels_deduped(self):
        g = self.pos_doc("noun", "noun", "suru")
        self.assertEqual(["noun", "suru"], extract_pos(g))

    def test_no_pos_markers_yields_empty(self):
        g = [{"type": "structured-content", "content": {
            "tag": "span", "content": "今日の一日前の日。"}}]
        self.assertEqual([], extract_pos(g))

    def test_plain_string_glossary_has_no_pos(self):
        self.assertEqual([], extract_pos(["to eat"]))

    def test_empty_glossary_is_safe(self):
        self.assertEqual([], extract_pos(None))
        self.assertEqual([], extract_pos([]))

    def test_pos_inside_a_dropped_subtree_is_not_collected(self):
        # An example sentence is not a definition; nor is its POS.
        g = [{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"},
             "content": "noun"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "data": {"content": "part-of-speech-info"},
                 "content": "ghost"},
            ]},
        ]}]
        self.assertEqual(["noun"], extract_pos(g))


if __name__ == "__main__":
    unittest.main()

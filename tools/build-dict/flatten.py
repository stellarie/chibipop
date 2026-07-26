"""Flatten Yomitan structured-content glossaries to plain text.

v1 keeps gloss text and ruby base text; it drops furigana (rt), images,
gaiji glyph references, and all styling. See spec section 5.

Jitendex additionally marks semantic block boundaries (part-of-speech
labels, senses, glossary items, cross-references, alternate forms, ...)
on each node's data.content field, and wraps every sense in an inlined
Tatoeba example sentence plus a trailing dictionary/source attribution
stamp. The example sentences and attribution do not belong in a
definition and are dropped outright; every other data.content-marked
node is treated as a block boundary, same as a list item, so adjacent
blocks are joined with a separator instead of fusing into one run-on
word (e.g. "1-dan" + "transitive" -> "1-dantransitive").
"""

import re

# Tags whose entire subtree is discarded.
_DROP_TAGS = {"rt", "rp", "img"}

# Jitendex data.content markers whose entire subtree is discarded: inlined
# Tatoeba example sentences (source sentence, translation, and the
# footnote number tagging them) and the trailing "JMdict | Tatoeba [1][2]"
# attribution stamp. Neither belongs in a definition.
_DROP_CONTENT = {
    "attribution", "attribution-footnote",
    "example-keyword", "example-sentence",
    "example-sentence-a", "example-sentence-b",
}

# Sentinel marking a block boundary: list items, and any Jitendex node
# carrying a data.content marker (part-of-speech-info, sense, glossary,
# xref, forms, ...). _tidy() turns runs of these into "; "-separated
# text, the same separator already used to join list items.
_BLOCK_MARK = "\u0000LI\u0000"

_WS = re.compile(r"[ \t\u3000]+")


def _render(node):
    """Render one structured-content node to a string."""
    if node is None:
        return ""
    if isinstance(node, str):
        return node
    if isinstance(node, list):
        return "".join(_render(c) for c in node)
    if not isinstance(node, dict):
        return ""

    content_marker = (node.get("data") or {}).get("content")
    if content_marker in _DROP_CONTENT:
        return ""

    tag = node.get("tag")
    if tag in _DROP_TAGS:
        return ""
    if tag == "br":
        return "\n"
    if tag == "li" or content_marker is not None:
        return _BLOCK_MARK + _render(node.get("content"))

    return _render(node.get("content"))


def _tidy(text):
    """Collapse whitespace and turn block markers into separators."""
    parts = [p.strip() for p in text.split(_BLOCK_MARK)]
    parts = [p for p in parts if p]
    text = "; ".join(parts)
    text = _WS.sub(" ", text)
    text = "\n".join(line.strip() for line in text.split("\n"))
    return text.strip()


def flatten_glossary(glossary):
    """Flatten a Yomitan glossary array to a list of plain-text strings.

    Empty results are dropped, so an image-only sense yields [].
    """
    out = []
    for item in glossary or []:
        if isinstance(item, str):
            text = _tidy(item)
        elif isinstance(item, dict):
            kind = item.get("type")
            if kind == "text":
                text = _tidy(item.get("text", ""))
            elif kind == "structured-content":
                text = _tidy(_render(item.get("content")))
            elif kind == "image":
                text = ""
            else:
                text = _tidy(_render(item))
        else:
            text = ""
        if text:
            out.append(text)
    return out

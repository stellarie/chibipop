"""Flatten Yomitan structured-content glossaries to plain text.

v1 keeps gloss text and ruby base text; it drops furigana (rt), images,
gaiji glyph references, and all styling. See spec section 5.
"""

import re

# Tags whose entire subtree is discarded.
_DROP_TAGS = {"rt", "rp", "img"}

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

    tag = node.get("tag")
    if tag in _DROP_TAGS:
        return ""
    if tag == "br":
        return "\n"
    if tag == "li":
        return "\u0000LI\u0000" + _render(node.get("content"))

    return _render(node.get("content"))


def _tidy(text):
    """Collapse whitespace and turn list-item markers into separators."""
    parts = [p.strip() for p in text.split("\u0000LI\u0000")]
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

#!/usr/bin/env python3
"""Structured-content shape census over a corpus of Yomitan dictionaries.

Walks every `term_bank_*.json` glossary tree in every archive and counts what
the dictionaries actually emit: tags, style properties, `data` hooks, media
node shapes, and nesting depth. Answers "which parts of the Yomitan schema do
real dictionaries use, and how many dictionaries would notice if we skipped
one" - the question a two-dictionary extrapolation cannot answer.

It also scans each archive's own `styles.css`, the second way a dictionary
draws a box: Yomitan scopes that stylesheet to the dictionary's own entries,
so a border declared there is invisible to a renderer that reads only the
inline `style` of a structured-content node.

Support columns are read out of `src/dict/glossary.rs` rather than duplicated
here, so the report re-scores itself as chibipop's allow-lists grow.

Stdlib only. No setup step.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys
import time
import zipfile
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
# Support columns come out of these three, never out of a copy here, so the
# report always scores against the current build. Ticket 02 moved the tag and
# style tables out of the old `glossary.rs` into the arena parser, and ticket
# 17 added the stylesheet grammar and property table beside them.
PARSE_RS = REPO / "src" / "dict" / "gloss" / "parse.rs"
GLOSS_RS = REPO / "src" / "dict" / "gloss" / "mod.rs"
SHEET_RS = REPO / "src" / "dict" / "sheet" / "mod.rs"

# Yomitan attributes worth counting separately from `style`; see
# dictionary-term-bank-v3-schema.json.
TRACKED_ATTRS = ("href", "colSpan", "rowSpan", "lang", "title", "open")
IMG_FIELDS = (
    "path", "width", "height", "sizeUnits", "appearance", "background",
    "collapsed", "collapsible", "verticalAlign", "imageRendering",
    "pixelated", "border", "borderRadius", "alt", "description", "title",
)


# ---- chibipop's current support, parsed from the Rust source ----


def _rust_str_array(src: str, name: str, where_: Path) -> set[str]:
    """The string literals in a `const NAME: [&str; N] = [...]` array."""
    m = re.search(rf"const {name}\s*:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];", src, re.S)
    if not m:
        raise SystemExit(f"census: could not parse {name} out of {where_}")
    return set(re.findall(r'"([^"]+)"', m.group(1)))


def _rust_match_keys(src: str, fn: str, where_: Path) -> set[str]:
    """The string patterns of a `fn NAME(...) -> Option<T> { Some(match s {`
    table: one `"key" => Variant,` arm per line."""
    m = re.search(rf"fn {fn}\b.*?\{{(.*?)\n\}}", src, re.S)
    if not m:
        raise SystemExit(f"census: could not find {fn} in {where_}")
    keys = set(re.findall(r'"([^"]+)"\s*=>', m.group(1)))
    if not keys:
        raise SystemExit(f"census: {fn} in {where_} parsed to no keys")
    return keys


def _rust_needles(src: str, where_: Path) -> list[tuple[str, str]]:
    """`const NEEDLES: [(&str, Role); N]` as `[(needle, role), ...]`, in the
    table's own order."""
    m = re.search(
        r"const NEEDLES\s*:\s*\[\(&str,\s*Role\);\s*\d+\]\s*=\s*\[(.*?)\n\];", src, re.S
    )
    if not m:
        raise SystemExit(f"census: could not parse NEEDLES out of {where_}")
    rows = re.findall(r'\("([^"]+)",\s*Role::(\w+)\)', m.group(1))
    if not rows:
        raise SystemExit(f"census: NEEDLES in {where_} parsed to no rows")
    return rows


def _rust_role_order(src: str, where_: Path) -> list[str]:
    """The `Role` variants in declaration order, which *is* the
    classification precedence - lowest wins. Parsed rather than copied so a
    reordered enum reorders this too."""
    m = re.search(r"pub enum Role \{(.*?)\n\}", src, re.S)
    if not m:
        raise SystemExit(f"census: could not parse enum Role out of {where_}")
    order = re.findall(r"^\s{4}(\w+),", m.group(1), re.M)
    if not order:
        raise SystemExit(f"census: enum Role in {where_} parsed to no variants")
    return order


def read_support() -> dict[str, object]:
    parse_src = PARSE_RS.read_text(encoding="utf-8")
    gloss_src = GLOSS_RS.read_text(encoding="utf-8")
    sheet_src = SHEET_RS.read_text(encoding="utf-8")
    return {
        # The tags the arena parser resolves to its own `Tag` enum. Anything
        # else parses as `Tag::Other`, keeping its name as an attribute.
        "tags": _rust_match_keys(parse_src, "tag_for", PARSE_RS),
        # Inline `style` keys, camelCase as the schema spells them.
        "styles": _rust_match_keys(parse_src, "style_key_for", PARSE_RS),
        # The editorial-role classifier: the needle table, the three keys
        # whose value carries the role, and the precedence the `Role` enum's
        # declaration order defines.
        "role_needles": _rust_needles(parse_src, PARSE_RS),
        "role_value_keys": _rust_str_array(parse_src, "VALUE_KEYS", PARSE_RS),
        "role_order": _rust_role_order(gloss_src, GLOSS_RS),
        # The `styles.css` half: the CSS spelling of the same properties, and
        # the selector grammar the matcher compiles.
        "css_props": _rust_match_keys(sheet_src, "css_key", SHEET_RS),
        "css_kinds": _rust_str_array(
            sheet_src, "SUPPORTED_SELECTOR_KINDS", SHEET_RS
        ),
        "css_pseudos": _rust_str_array(
            sheet_src, "SUPPORTED_PSEUDO_CLASSES", SHEET_RS
        ),
    }


# ---- the editorial-role classifier, mirroring src/dict/gloss/parse.rs ----


def fold(text: str) -> str:
    """`parse::fold`, one string at a time: ASCII case, full-width ASCII
    letters and digits, and the ideographic space."""
    out = []
    for ch in text:
        o = ord(ch)
        if 0x41 <= o <= 0x5A:
            out.append(chr(o + 32))
        elif 0xFF21 <= o <= 0xFF3A:
            out.append(chr(o - 0xFF21 + 0x61))
        elif 0xFF41 <= o <= 0xFF5A:
            out.append(chr(o - 0xFF41 + 0x61))
        elif 0xFF10 <= o <= 0xFF19:
            out.append(chr(o - 0xFF10 + 0x30))
        elif o == 0x3000:
            out.append(" ")
        else:
            out.append(ch)
    return "".join(out)


class Roles:
    """The classifier, resolved once against the Rust tables."""

    def __init__(self, support: dict) -> None:
        self.order = support["role_order"]
        self.rank = {name: i for i, name in enumerate(self.order)}
        self.content = self.rank["Content"]
        self.needles = [
            (fold(needle), self.rank[role]) for needle, role in support["role_needles"]
        ]
        self.value_keys = {fold(k) for k in support["role_value_keys"]}

    def of_text(self, text: str) -> int:
        folded = fold(text)
        best = self.content
        for needle, rank in self.needles:
            if rank < best and needle in folded:
                best = rank
        return best

    def of_entry(self, key: str, value) -> int:
        """One `data` entry's role. The value side is an *exact* key match on
        the three conventions, and only a string value names a role."""
        best = self.of_text(key)
        if fold(key) in self.value_keys and isinstance(value, str):
            best = min(best, self.of_text(value))
        return best

    def name(self, rank: int) -> str:
        return self.order[rank]


# ---- the walk ----


def new_stats(path: str) -> dict:
    return {
        "path": path,
        "title": None,
        "revision": None,
        "format": None,
        "rows": 0,
        "rows_capped": False,
        "banks": 0,
        "kinds": collections.Counter(),
        "tags": collections.Counter(),
        "style": collections.Counter(),
        "data": collections.Counter(),
        "attrs": collections.Counter(),
        "img_fields": collections.Counter(),
        "img_exts": collections.Counter(),
        "img_nodes": 0,
        "img_sized": 0,
        "img_unsized": 0,
        "gaiji_nodes": 0,
        "maxdepth": 0,
        "error": None,
        "styles_css": None,
    }


def walk(node, st: dict, depth: int) -> None:
    if depth > st["maxdepth"]:
        st["maxdepth"] = depth
    if isinstance(node, str) or node is None:
        return
    if isinstance(node, list):
        for child in node:
            walk(child, st, depth)
        return
    if not isinstance(node, dict):
        return

    tag = node.get("tag")
    if isinstance(tag, str):
        st["tags"][tag] += 1

    data = node.get("data")
    if isinstance(data, dict):
        for key, value in data.items():
            if isinstance(value, str):
                st["data"][f"{key}={value}"] += 1
            if key == "gaiji":
                st["gaiji_nodes"] += 1

    style = node.get("style")
    if isinstance(style, dict):
        for key in style:
            st["style"][key] += 1

    for attr in TRACKED_ATTRS:
        if attr in node:
            st["attrs"][attr] += 1

    if tag == "img":
        count_image(node, st)

    walk(node.get("content"), st, depth + 1)


def count_image(node: dict, st: dict) -> None:
    """One `img` node, or a top-level `type:image` glossary item."""
    st["img_nodes"] += 1
    for field in IMG_FIELDS:
        if field in node:
            st["img_fields"][field] += 1
    path = node.get("path")
    if isinstance(path, str):
        st["img_exts"][os.path.splitext(path)[1].lower() or "(none)"] += 1
    if "width" in node or "height" in node:
        st["img_sized"] += 1
    else:
        st["img_unsized"] += 1


# ---- a dictionary's own styles.css ----

# Yomitan lets a dictionary ship a `styles.css` and scopes it to that
# dictionary's own entries. Structured content has no `class` attribute, so
# such a stylesheet can only reach content three ways: bare tag selectors, the
# `data-*` attributes Yomitan derives from a node's `data` map, and structural
# pseudo-selectors. The classification below follows that reality rather than
# the categories of general web CSS.
STYLES_CSS = "styles.css"

# Properties a text-only renderer cannot reproduce: these are what draws a
# pill, a rule, or a box.
BOX_PREFIXES = ("border", "margin", "padding", "outline")
BOX_PROPS = frozenset({
    "background", "background-color", "display", "border-radius", "box-shadow",
    "width", "height", "float", "vertical-align", "content", "list-style",
    "list-style-type",
})

# `:before` and its three siblings are pseudo-elements even with one colon.
LEGACY_PSEUDO_ELEMENTS = frozenset({"before", "after", "first-line", "first-letter"})
# Pseudo-classes whose argument is itself a selector list, so descending into
# it is worth the trouble: `td:has([data-sc親字])` keys on content exactly the
# way a bare attribute selector does. Everything else - `:nth-child(2n+1)`,
# `:lang(ja)` - is skipped, so `2n` is never misread as a tag.
SELECTOR_LIST_PSEUDOS = frozenset({
    "not", "is", "where", "has", "matches", "any", "-moz-any", "-webkit-any",
})
# Blocks inside these are `from`/`to`/`50%` steps, never selectors.
KEYFRAMES_AT_RULES = frozenset({"@keyframes", "@-webkit-keyframes", "@-moz-keyframes"})

AT_NAME_RE = re.compile(r"@([-\w]+)")
PROP_NAME_RE = re.compile(r"-{0,2}[a-z][-a-z0-9]*")
ATTR_RE = re.compile(
    r"""\s*([^\s=~|^$*\[\]]+)\s*                  # attribute name
        (?:([~|^$*]?=)\s*                         # optional match operator
           (?:"([^"]*)"|'([^']*)'|([^\s\]]*)))?   # quoted or bare value
    """,
    re.X,
)


def new_css_stats(parse_error: str | None = None) -> dict:
    return {
        "bytes": 0,
        "rules": 0,
        "declarations": 0,
        "selectors": 0,
        "selector_kinds": collections.Counter(),
        "data_attrs": collections.Counter(),
        "box_props": collections.Counter(),
        "box_rules": 0,
        "at_rules": collections.Counter(),
        # ---- scored against the live grammar (ticket 17) ----
        # These four partition `rules`, exactly as `dict::sheet::SheetCounts`
        # does, so the census and the matcher report the same arithmetic.
        "rules_kept": 0,
        "rules_dropped_selector": 0,
        "rules_dropped_at_rule": 0,
        "rules_no_props": 0,
        # Of `box_rules`: how many the matcher would actually draw.
        "box_rules_kept": 0,
        # Why a rule was dropped, so the gap is diagnosable and not just
        # countable.
        "drop_reasons": collections.Counter(),
        "parse_error": parse_error,
    }


def is_box_prop(name: str) -> bool:
    return name.startswith(BOX_PREFIXES) or name in BOX_PROPS


def find_styles_css(names: list[str]) -> str | None:
    """`styles.css` at the archive root, or one directory deep: some archives
    are zipped with a wrapper folder. The shallowest wins."""
    best: tuple[int, str] | None = None
    for name in names:
        flat = name.replace("\\", "/")
        if os.path.basename(flat) != STYLES_CSS:
            continue
        depth = flat.strip("/").count("/")
        if depth > 1:
            continue
        if best is None or depth < best[0]:
            best = (depth, name)
    return best[1] if best else None


def _skip_string(css: str, i: int) -> tuple[int, bool]:
    """Index just past the closing quote of the string opening at `i`."""
    quote, n = css[i], len(css)
    i += 1
    while i < n:
        ch = css[i]
        if ch == "\\":
            i += 2
        elif ch == quote:
            return i + 1, True
        elif ch == "\n":  # a CSS string never spans a raw newline
            return i, False
        else:
            i += 1
    return n, False


def _skip_nested(css: str, i: int, opener: str, closer: str) -> int:
    """Index just past the `closer` matching the `opener` at `i`, honouring
    strings and nesting. Runs to the end on unbalanced input."""
    depth, n = 0, len(css)
    while i < n:
        ch = css[i]
        if ch in "\"'":
            i, _ = _skip_string(css, i)
            continue
        if ch == opener:
            depth += 1
        elif ch == closer:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def _at_name(prelude: str) -> str:
    m = AT_NAME_RE.match(prelude)
    return "@" + m.group(1).lower() if m else "@"


def _block_kind(prelude: str, open_blocks: list[dict], at_rules: collections.Counter) -> str:
    if prelude.startswith("@"):
        at_rules[_at_name(prelude)] += 1
        return "at"
    for prev in open_blocks:
        if prev["kind"] == "at" and _at_name(prev["prelude"]) in KEYFRAMES_AT_RULES:
            return "keyframe"
    return "rule"


def _flush_statement(
    buf: list[str], decls: list[tuple[str, str]] | None, at_rules: collections.Counter
) -> None:
    """One `;`-terminated statement: a declaration, or an at-statement such as
    `@import url(...)`."""
    text = "".join(buf).strip()
    if not text:
        return
    if text.startswith("@"):
        at_rules[_at_name(text)] += 1
        return
    head, sep, value = text.partition(":")
    if not sep or decls is None:
        return
    name = head.strip().lower()
    if PROP_NAME_RE.fullmatch(name):
        # The value comes along because support is not a property question
        # alone: the matcher drops a `var()` value it cannot substitute,
        # whatever property carries it.
        decls.append((name, value.strip()))


def _resolve_nesting(prelude: str, parent: list[str] | None) -> list[str]:
    """A nested prelude flattened against the rule it sits in.

    Native CSS nesting, which real files use: Jitendex writes its
    marker-suppression rule as `li[...] { & ul[...] { ... } }`, and a scan
    that only reported the inner prelude would score `& ul[...]` as an
    unreadable selector. Mirrors `Sheet::compile`'s own resolution so that
    the two agree on what is supported."""
    own = split_selectors(prelude)
    if parent is None:
        return own
    return [
        s.replace("&", p) if "&" in s else f"{p} {s}" for p in parent for s in own
    ]


def scan_css(text: str) -> tuple[list[dict], collections.Counter, str | None]:
    """Character-level scan into style-rule blocks.

    Hand-rolled because no regex sees block structure: nested at-rules, native
    `&` nesting, and braces inside strings or `url(...)` all defeat a pattern
    that counts braces. Malformed input records an error and keeps whatever was
    scanned up to that point.

    Each block is `{prelude, kind, decls, sels, in_at}`. `kind` is `rule` for
    a style rule, `at` for an at-rule block, `keyframe` for an animation step
    inside `@keyframes`. `sels` is the selector list with `&` resolved, and
    `in_at` says the rule sits inside an at-rule body.
    """
    blocks: list[dict] = []
    at_rules: collections.Counter = collections.Counter()
    open_blocks: list[dict] = []
    buf: list[str] = []
    error: str | None = None
    parens = 0
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch == "/" and text.startswith("/*", i):
            end = text.find("*/", i + 2)
            if end < 0:
                error = error or "unterminated comment"
                break
            buf.append(" ")
            i = end + 2
        elif ch in "\"'":
            j, ok = _skip_string(text, i)
            if not ok:
                error = error or "unterminated string"
            buf.append(text[i:j])
            i = j
        elif ch == "(":
            parens += 1
            buf.append(ch)
            i += 1
        elif ch == ")":
            parens = max(0, parens - 1)
            buf.append(ch)
            i += 1
        elif parens or ch not in "{};":
            buf.append(ch)
            i += 1
        elif ch == "{":
            prelude = "".join(buf).strip()
            buf = []
            kind = _block_kind(prelude, open_blocks, at_rules)
            parent = next(
                (b["sels"] for b in reversed(open_blocks) if b["kind"] == "rule"), None
            )
            open_blocks.append({
                "prelude": prelude,
                "kind": kind,
                "decls": [],
                "sels": [] if kind == "at" else _resolve_nesting(prelude, parent),
                "in_at": kind == "at" or any(b["in_at"] for b in open_blocks),
            })
            i += 1
        elif ch == "}":
            if open_blocks:
                _flush_statement(buf, open_blocks[-1]["decls"], at_rules)
                blocks.append(open_blocks.pop())
            else:
                error = error or "unbalanced '}'"
            buf = []
            i += 1
        else:  # ';'
            _flush_statement(
                buf, open_blocks[-1]["decls"] if open_blocks else None, at_rules
            )
            buf = []
            i += 1
    if open_blocks:
        error = error or f"{len(open_blocks)} unclosed block(s)"
        _flush_statement(buf, open_blocks[-1]["decls"], at_rules)
        while open_blocks:
            blocks.append(open_blocks.pop())
    return blocks, at_rules, error


def split_selectors(prelude: str) -> list[str]:
    """A rule prelude split on its top-level commas."""
    out: list[str] = []
    start, i, n = 0, 0, len(prelude)
    while i < n:
        ch = prelude[i]
        if ch in "\"'":
            i, _ = _skip_string(prelude, i)
        elif ch == "(":
            i = _skip_nested(prelude, i, "(", ")")
        elif ch == "[":
            i = _skip_nested(prelude, i, "[", "]")
        elif ch == ",":
            out.append(prelude[start:i])
            i += 1
            start = i
        else:
            i += 1
    out.append(prelude[start:])
    return [sel.strip() for sel in out if sel.strip()]


def _is_ident_start(ch: str) -> bool:
    return ch.isalpha() or ch == "_" or ch == "\\" or ord(ch) > 0x7F


def _skip_ident(sel: str, i: int) -> int:
    n = len(sel)
    while i < n:
        ch = sel[i]
        if ch == "\\":
            i += 2
        elif ch.isalnum() or ch in "-_" or ord(ch) > 0x7F:
            i += 1
        else:
            break
    return i


def _classify_attr(inner: str, seen: set[str], data_attrs: collections.Counter) -> None:
    """One attribute selector, its brackets already stripped."""
    m = ATTR_RE.match(inner)
    name = m.group(1) if m else ""
    if not name:
        seen.add("unknown")
        return
    if not name.lower().startswith("data-"):
        seen.add("other-attr")
        return
    seen.add("data-attr")
    op = m.group(2)
    value = next((g for g in m.group(3, 4, 5) if g is not None), "")
    # `data-sc-content="example"` is the bridge to the term-bank `data`
    # counter. A prefix or substring operator is kept verbatim, because it
    # matches a family of values rather than one.
    data_attrs[f'{name}{op}"{value}"' if op else name] += 1


def _classify_pseudo(
    sel: str, i: int, seen: set[str], data_attrs: collections.Counter
) -> int:
    double = sel.startswith("::", i)
    head = i + (2 if double else 1)
    end = _skip_ident(sel, head)
    name = sel[head:end].lower()
    if not name:
        seen.add("unknown")
        return i + 1
    seen.add(
        "pseudo-element"
        if double or name in LEGACY_PSEUDO_ELEMENTS
        else "pseudo-class"
    )
    if end < len(sel) and sel[end] == "(":
        close = _skip_nested(sel, end, "(", ")")
        if name in SELECTOR_LIST_PSEUDOS:
            _classify(sel[end + 1 : max(end + 1, close - 1)], seen, data_attrs)
        return close
    return end


def _classify(sel: str, seen: set[str], data_attrs: collections.Counter) -> None:
    i, n = 0, len(sel)
    while i < n:
        ch = sel[i]
        if ch in "\"'":
            i, _ = _skip_string(sel, i)
        elif ch == "[":
            close = _skip_nested(sel, i, "[", "]")
            _classify_attr(sel[i + 1 : max(i + 1, close - 1)], seen, data_attrs)
            i = close
        elif ch == ":":
            i = _classify_pseudo(sel, i, seen, data_attrs)
        elif ch in ".#":
            end = _skip_ident(sel, i + 1)
            if end > i + 1:
                seen.add("class" if ch == "." else "id")
                i = end
            else:
                seen.add("unknown")
                i += 1
        elif ch == "*":
            seen.add("universal")
            i += 1
        elif ch in " \t\r\n>+~&|":  # combinators and the nesting marker
            i += 1
        elif _is_ident_start(ch):
            seen.add("tag")
            i = _skip_ident(sel, i)
        else:
            seen.add("unknown")
            i += 1


def classify_selector(
    sel: str, kinds: collections.Counter, data_attrs: collections.Counter
) -> None:
    """Score one complex selector. Each kind counts once per selector, so a
    selector contributing `tag` and `data-attr` lands in both buckets."""
    seen: set[str] = set()
    _classify(sel, seen, data_attrs)
    for kind in seen or {"unknown"}:
        kinds[kind] += 1


# An attribute operator other than a bare `=`: the matcher reads only `=`.
ATTR_OP_RE = re.compile(r"\[[^\]]*?([~|^$*])=")


def _outside_brackets(sel: str) -> str:
    """The selector with every `[...]` and `(...)` blanked, so a scan for a
    combinator cannot trip over one inside an attribute value."""
    out, depth = [], 0
    for ch in sel:
        if ch in "[(":
            depth += 1
        elif ch in "])":
            depth = max(0, depth - 1)
        elif depth == 0:
            out.append(ch)
    return "".join(out)


def _tag_names(sel: str) -> set[str]:
    """Every tag name in one selector, pseudo arguments included."""
    out: set[str] = set()
    i, n = 0, len(sel)
    while i < n:
        ch = sel[i]
        if ch in "\"'":
            i, _ = _skip_string(sel, i)
        elif ch == "[":
            i = _skip_nested(sel, i, "[", "]")
        elif ch == ":":
            head = i + (2 if sel.startswith("::", i) else 1)
            end = _skip_ident(sel, head)
            name = sel[head:end].lower()
            if end < len(sel) and sel[end] == "(":
                close = _skip_nested(sel, end, "(", ")")
                if name in SELECTOR_LIST_PSEUDOS:
                    out |= _tag_names(sel[end + 1 : max(end + 1, close - 1)])
                i = close
            else:
                i = end
        elif ch in ".#":
            i = max(_skip_ident(sel, i + 1), i + 1)
        elif _is_ident_start(ch):
            end = _skip_ident(sel, i)
            out.add(sel[i:end].lower())
            i = end
        else:
            i += 1
    return out


def _pseudo_names(sel: str) -> set[str]:
    """Every pseudo-class name in one selector. A pseudo-*element* is already
    an unsupported kind, so it is not repeated here."""
    out: set[str] = set()
    i, n = 0, len(sel)
    while i < n:
        ch = sel[i]
        if ch in "\"'":
            i, _ = _skip_string(sel, i)
        elif ch == "[":
            i = _skip_nested(sel, i, "[", "]")
        elif ch == ":":
            double = sel.startswith("::", i)
            head = i + (2 if double else 1)
            end = _skip_ident(sel, head)
            name = sel[head:end].lower()
            if not double and name not in LEGACY_PSEUDO_ELEMENTS and name:
                out.add(name)
            if end < len(sel) and sel[end] == "(":
                close = _skip_nested(sel, end, "(", ")")
                if name in SELECTOR_LIST_PSEUDOS:
                    out |= _pseudo_names(sel[end + 1 : max(end + 1, close - 1)])
                i = close
            else:
                i = end
        else:
            i += 1
    return out


def selector_support(sel: str, support: dict[str, set[str]]) -> set[str]:
    """Why one complex selector leaves the matcher's grammar, or an empty set
    when it does not.

    A deliberately literal reading of `src/dict/sheet/select.rs`: the kinds
    and the pseudo-classes come out of that source, so this scores the build
    rather than a copy of it. Reasons are named rather than counted alone,
    because "70 dropped" and "70 dropped, all chrome classes" are different
    findings."""
    bad: set[str] = set()
    kinds: set[str] = set()
    _classify(sel, kinds, collections.Counter())
    for kind in kinds:
        if kind not in support["css_kinds"]:
            bad.add(kind)
    # `_classify` reports `tag` and `pseudo-class` as kinds without saying
    # which; both need their own names checked.
    for name in _tag_names(sel):
        if name not in support["tags"]:
            bad.add(f"tag:{name}")
    for name in _pseudo_names(sel):
        if name not in support["css_pseudos"]:
            bad.add(f"pseudo:{name}")
    if re.search(r"[+~]", _outside_brackets(sel)):
        bad.add("sibling-combinator")
    for inner in ATTR_OP_RE.findall(sel):
        bad.add(f"attr-op:{inner}")
    return bad


def css_stats(raw: bytes, support: dict[str, set[str]]) -> dict:
    st = new_css_stats()
    st["bytes"] = len(raw)
    try:
        text = raw.decode("utf-8-sig")
    except UnicodeDecodeError as exc:
        st["parse_error"] = f"UnicodeDecodeError: {exc}"
        text = raw.decode("utf-8", "replace")
    try:
        blocks, at_rules, error = scan_css(text)
    except Exception as exc:  # a scanner bug must not lose the archive
        st["parse_error"] = st["parse_error"] or f"{type(exc).__name__}: {exc}"
        return st
    st["parse_error"] = st["parse_error"] or error
    st["at_rules"] = at_rules
    for block in blocks:
        decls = block["decls"]
        st["declarations"] += len(decls)
        if block["kind"] != "rule":
            continue  # @font-face descriptors and animation steps draw no pill
        st["rules"] += 1
        box = [prop for prop, _ in decls if is_box_prop(prop)]
        for prop in box:
            st["box_props"][prop] += 1
        if box:
            st["box_rules"] += 1
        # The published counters read the prelude as written; the support
        # scoring reads it with `&` resolved, because that is what the
        # matcher compiles.
        for sel in split_selectors(block["prelude"]):
            st["selectors"] += 1
            classify_selector(sel, st["selector_kinds"], st["data_attrs"])
        score_rule(st, block, decls, bool(box), support)
    return st


def score_rule(
    st: dict,
    block: dict,
    decls: list[tuple[str, str]],
    is_box: bool,
    support: dict[str, set[str]],
) -> None:
    """One rule into exactly one of the four support buckets.

    The order matters and mirrors `Sheet::compile`: an at-rule body never
    reaches selector compilation, a selector list is atomic so one unreadable
    member drops the whole rule, and a rule whose every property is unmapped
    is its own bucket rather than a grammar gap."""
    if block["in_at"]:
        st["rules_dropped_at_rule"] += 1
        st["drop_reasons"]["at-rule body"] += 1
        return
    sels = block["sels"]
    reasons: set[str] = set()
    for sel in sels:
        reasons |= selector_support(sel, support)
    if not sels or reasons:
        st["rules_dropped_selector"] += 1
        for reason in reasons or {"empty selector"}:
            st["drop_reasons"][reason] += 1
        return
    if not any(
        prop in support["css_props"] and "var(" not in value for prop, value in decls
    ):
        st["rules_no_props"] += 1
        return
    st["rules_kept"] += 1
    if is_box:
        st["box_rules_kept"] += 1


def read_styles_css(
    z: zipfile.ZipFile, names: list[str], support: dict[str, set[str]]
) -> dict | None:
    """The archive's own stylesheet, or `None` when it ships none. Independent
    of `--rows`: a stylesheet is read whole or not at all."""
    name = find_styles_css(names)
    if name is None:
        return None
    try:
        raw = z.read(name)
    except Exception as exc:  # a bad member must not lose the term-bank walk
        return new_css_stats(f"{type(exc).__name__}: {exc}")
    return css_stats(raw, support)


def census(job: tuple[str, int, dict[str, object]]) -> dict:
    path, row_cap, support = job
    st = new_stats(path)
    try:
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
            if "index.json" in names:
                index = json.loads(z.read("index.json"))
                st["title"] = index.get("title")
                st["revision"] = index.get("revision")
                st["format"] = index.get("format", index.get("version"))
            st["styles_css"] = read_styles_css(z, names, support)
            banks = sorted(
                n for n in names
                if re.fullmatch(r"term_bank_\d+\.json", os.path.basename(n))
            )
            st["banks"] = len(banks)
            for bank in banks:
                if row_cap and st["rows"] >= row_cap:
                    st["rows_capped"] = True
                    break
                for row in json.loads(z.read(bank)):
                    if row_cap and st["rows"] >= row_cap:
                        st["rows_capped"] = True
                        break
                    st["rows"] += 1
                    glossary = row[5] if isinstance(row, list) and len(row) > 5 else None
                    if not isinstance(glossary, list):
                        continue
                    for item in glossary:
                        if isinstance(item, str):
                            st["kinds"]["plain-string"] += 1
                        elif isinstance(item, list):
                            st["kinds"]["deinflection"] += 1
                        elif isinstance(item, dict):
                            kind = item.get("type", "(untyped)")
                            st["kinds"][kind] += 1
                            if kind == "structured-content":
                                walk(item.get("content"), st, 1)
                            elif kind == "image":
                                count_image(item, st)
                        else:
                            st["kinds"]["other"] += 1
    except Exception as exc:  # one bad archive must not lose the run
        st["error"] = f"{type(exc).__name__}: {exc}"
    for key in ("kinds", "tags", "style", "data", "attrs", "img_fields", "img_exts"):
        st[key] = dict(st[key])
    if st["styles_css"] is not None:
        for key in (
            "selector_kinds", "data_attrs", "box_props", "at_rules", "drop_reasons",
        ):
            st["styles_css"][key] = dict(st["styles_css"][key])
    return st


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("corpus", type=Path, help="directory of Yomitan .zip archives")
    ap.add_argument(
        "--rows", type=int, default=30000,
        help="term rows sampled per dictionary; 0 reads every row (default 30000)",
    )
    ap.add_argument(
        "--out", type=Path, default=Path(__file__).parent / "results" / "census.json",
    )
    ap.add_argument("--jobs", type=int, default=min(12, (os.cpu_count() or 4)))
    args = ap.parse_args()

    archives = sorted(str(p) for p in args.corpus.glob("*.zip"))
    if not archives:
        print(f"census: no .zip archives under {args.corpus}", file=sys.stderr)
        return 1

    # `read_support` already hands back JSON-serialisable shapes, and two of
    # them are *ordered*: the needle table and the `Role` precedence. Sorting
    # them here, as an earlier version did to every column alike, would
    # alphabetise the precedence and silently change what wins.
    support = read_support()
    jobs = [(path, args.rows, support) for path in archives]

    started = time.time()
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        results = list(pool.map(census, jobs))
    elapsed = time.time() - started

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(
            {
                "corpus": str(args.corpus),
                "row_cap": args.rows,
                "elapsed_s": round(elapsed, 1),
                "support": {
                    k: sorted(v) if isinstance(v, set) else v
                    for k, v in support.items()
                },
                "dictionaries": results,
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )

    failed = [r for r in results if r["error"]]
    print(f"{len(results)} archives in {elapsed:.0f}s -> {args.out}")
    if failed:
        print(f"{len(failed)} failed:", file=sys.stderr)
        for r in failed:
            print(f"  {os.path.basename(r['path'])}: {r['error']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

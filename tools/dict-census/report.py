#!/usr/bin/env python3
"""Aggregates a census run into the markdown tables embedded in
docs/research/dict-shapes.md.

Ranks every feature by **how many dictionaries use it**, not by node count.
Node counts say which dictionary is chatty; dictionary counts say how many
users notice if we skip the feature - the only ranking that sizes work.
"""

from __future__ import annotations

import argparse
import collections
import json
from pathlib import Path

DEFAULT_IN = Path(__file__).parent / "results" / "census.json"
DEFAULT_OUT = Path(__file__).parent / "results" / "tables.md"


def rank(dicts: list[dict], field: str) -> list[tuple[str, int, int]]:
    """(key, dictionaries using it, total nodes), most dictionaries first."""
    per_dict: collections.Counter = collections.Counter()
    per_node: collections.Counter = collections.Counter()
    for d in dicts:
        for key, count in d[field].items():
            per_dict[key] += 1
            per_node[key] += count
    return [(k, c, per_node[k]) for k, c in per_dict.most_common()]


def table(rows: list[str], header: str, sep: str) -> list[str]:
    return [header, sep, *rows, ""]


# ---- a dictionary's own styles.css ----

# The inline `style` keys that draw a box rather than style text. These are
# Yomitan's camelCase spellings; census.py holds the CSS-side equivalents.
INLINE_BOX_PREFIXES = ("margin", "padding", "border", "background")
INLINE_BOX_KEYS = frozenset({
    "display", "verticalAlign", "width", "height", "listStyleType",
})


def inline_box_keys(d: dict) -> list[str]:
    return sorted(
        k for k in d["style"]
        if k.startswith(INLINE_BOX_PREFIXES) or k in INLINE_BOX_KEYS
    )


def data_attr_name(key: str) -> str:
    """The attribute Yomitan derives from a structured-content `data` key.

    Yomitan writes the key into `dataset`, and the DOM turns every ASCII
    capital there into `-x`. So `partOfSpeech` becomes `data-sc-part-of-speech`,
    while a CJK key gets no separating hyphen at all: `data-sc常用外マーク`.
    """
    head = key[:1]
    stem = head.upper() + key[1:] if head.isascii() and head.isalpha() else key
    return "data-" + "".join(
        f"-{c.lower()}" if "A" <= c <= "Z" else c for c in "sc" + stem
    )


def css_attr_parts(entry: str) -> tuple[str, str | None]:
    """A census `data_attrs` key back into `(name, value)`. A prefix or
    substring operator matches a family of values, so it degrades to a
    name-only match rather than claiming an exact one."""
    head, sep, rest = entry.partition("=")
    name = head.rstrip("~|^$*")
    if not sep or name != head:
        return name, None
    return name, rest.strip('"')


def rank_css(shipped: list[dict], field: str) -> list[tuple[str, int, int]]:
    """`rank`, for a counter nested under `styles_css`."""
    per_dict: collections.Counter = collections.Counter()
    per_hit: collections.Counter = collections.Counter()
    for d in shipped:
        for key, count in d["styles_css"][field].items():
            per_dict[key] += 1
            per_hit[key] += count
    return [(k, c, per_hit[k]) for k, c in per_dict.most_common()]


def styles_css_section(with_terms: list[dict], sc: list[dict]) -> list[str]:
    shipped = [d for d in with_terms if d.get("styles_css")]
    drawing = [d for d in shipped if d["styles_css"]["box_rules"]]
    broken = [d for d in shipped if d["styles_css"]["parse_error"]]

    out = ["## A dictionary's own `styles.css`", ""]
    out.append(f"- {len(shipped)} of {len(with_terms)} dictionaries with term rows ship a "
               "`styles.css`. Yomitan scopes it to that dictionary's own entries.")
    out.append(f"- {len(drawing)} of those {len(shipped)} declare box-model properties "
               "there: borders, padding, background, `border-radius`.")
    if broken:
        for d in broken:
            out.append(f"- **{d['title'] or '?'}** did not scan cleanly: "
                       f"`{d['styles_css']['parse_error']}`.")
    else:
        out.append("- Every stylesheet scanned clean. No `parse_error` in the corpus.")
    out.append("")

    rows = []
    for d in sorted(shipped, key=lambda d: -d["styles_css"]["box_rules"]):
        css = d["styles_css"]
        top = ", ".join(
            f"`{k}`" for k, _ in
            sorted(css["data_attrs"].items(), key=lambda kv: -kv[1])[:3]
        )
        ats = ", ".join(f"`{k}`" for k in sorted(css["at_rules"]))
        rows.append(f"| {d['title'] or '?'} | {css['bytes']:,} | {css['rules']} | "
                    f"{css['box_rules']} | {top or '-'} | {ats or '-'} |")
    out += table(
        rows,
        "| dictionary | bytes | rules | box rules | top `data-*` keys | at-rules |",
        "|---|---:|---:|---:|---|---|",
    )

    out.append("### How those selectors reach content")
    out.append("")
    out.append("Structured content carries no `class` attribute. A stylesheet can only "
               "reach a node by tag, by a `data-*` attribute Yomitan derives from the "
               "node's `data` map, or by position.")
    out.append("")
    rows = [f"| `{k}` | {n} | {v:,} |" for k, n, v in rank_css(shipped, "selector_kinds")]
    out += table(rows, "| selector kind | #dicts | #selectors |", "|---|---:|---:|")

    out.append("### What the matcher compiles, and what it drops")
    out.append("")
    out.append("Scored against the live grammar in `src/dict/sheet/select.rs` and the "
               "property table in `src/dict/sheet/mod.rs`, so these columns move when "
               "the matcher does. A rule with an unreadable selector is dropped whole, "
               "as CSS itself drops one; `no props` is a rule whose selector compiles "
               "and whose every declaration names a property this build does not map.")
    out.append("")
    rows = []
    tot = collections.Counter()
    for d in sorted(shipped, key=lambda d: -d["styles_css"]["box_rules"]):
        css = d["styles_css"]
        rows.append(
            f"| {d['title'] or '?'} | {css['rules']} | {css['rules_kept']} | "
            f"{css['rules_dropped_selector'] + css['rules_dropped_at_rule']} | "
            f"{css['rules_no_props']} | {css['box_rules']} | {css['box_rules_kept']} |"
        )
        for k in ("rules", "rules_kept", "rules_dropped_selector",
                  "rules_dropped_at_rule", "rules_no_props", "box_rules",
                  "box_rules_kept"):
            tot[k] += css[k]
    rows.append(
        f"| **all {len(shipped)}** | **{tot['rules']}** | **{tot['rules_kept']}** | "
        f"**{tot['rules_dropped_selector'] + tot['rules_dropped_at_rule']}** | "
        f"**{tot['rules_no_props']}** | **{tot['box_rules']}** | "
        f"**{tot['box_rules_kept']}** |"
    )
    out += table(
        rows,
        "| dictionary | rules | kept | dropped | no props | box rules | box rules kept |",
        "|---|---:|---:|---:|---:|---:|---:|",
    )
    out.append("Why a rule is dropped:")
    out.append("")
    rows = [f"| `{k}` | {n} | {v:,} |" for k, n, v in rank_css(shipped, "drop_reasons")]
    out += table(rows, "| reason | #dicts | #rules |", "|---|---:|---:|")

    out.append("### Box-model properties declared there")
    out.append("")
    rows = [f"| `{k}` | {n} | {v:,} |" for k, n, v in rank_css(shipped, "box_props")]
    out += table(rows, "| property | #dicts | #rules |", "|---|---:|---:|")

    out.append("### The `data-*` keys those selectors match")
    out.append("")
    out.append("`in term banks` cross-references the `data` counter of the same "
               "dictionary: the selector matches content this census has seen.")
    out.append("")
    per_dict: collections.Counter = collections.Counter()
    per_hit: collections.Counter = collections.Counter()
    matched: collections.Counter = collections.Counter()
    for d in shipped:
        keys = [k.partition("=") for k in d["data"]]
        names = {data_attr_name(k) for k, _, _ in keys}
        pairs = {(data_attr_name(k), v) for k, _, v in keys}
        for entry, count in d["styles_css"]["data_attrs"].items():
            per_dict[entry] += 1
            per_hit[entry] += count
            name, value = css_attr_parts(entry)
            hit = name in names if value is None else (name, value) in pairs
            if hit:
                matched[entry] += 1
    out.append(f"{sum(matched.values()):,} of {sum(per_dict.values()):,} "
               "(dictionary, `data-*` key) pairs match a key the term-bank walk saw.")
    out.append("")
    ordered = sorted(per_dict, key=lambda k: (-per_dict[k], -per_hit[k], k))
    rows = [
        f"| `{k}` | {per_dict[k]} | {per_hit[k]:,} | "
        f"{'yes' if matched[k] else 'no'} |"
        for k in ordered[:20]
    ]
    out += table(rows, "| `data-*` key | #dicts | #selectors | in term banks |",
                 "|---|---:|---:|---|")

    out.append("### Parity: where the pills actually live")
    out.append("")
    out.append("A box model reads the inline `style` of a structured-content node. It "
               "never sees `styles.css`. So the question is which dictionaries draw "
               "their boxes where.")
    out.append("")
    buckets: dict[str, list[dict]] = {"inline-only": [], "css-only": [], "both": []}
    rows = []
    for d in sc:
        css = d.get("styles_css") or {}
        box_rules = css.get("box_rules", 0)
        inline = inline_box_keys(d)
        if box_rules and inline:
            bucket = "both"
        elif box_rules:
            bucket = "css-only"
        elif inline:
            bucket = "inline-only"
        else:
            continue
        buckets[bucket].append(d)
        if box_rules:
            keys = ", ".join(f"`{k}`" for k in inline)
            rows.append(f"| {d['title'] or '?'} | {box_rules} | {keys or '(none)'} | "
                        f"**{bucket}** |")
    out += table(
        rows,
        "| dictionary | `styles.css` box rules | inline box `style` keys | bucket |",
        "|---|---:|---|---|",
    )

    bucketed = sum(len(v) for v in buckets.values())
    out.append(f"- **inline-only: {len(buckets['inline-only'])}** - the box model reaches "
               "these pills; `styles.css` is absent or draws no box.")
    out.append(f"- **css-only: {len(buckets['css-only'])}** - the pills exist only in "
               "`styles.css`, so a box model with no CSS support draws nothing for them.")
    out.append(f"- **both: {len(buckets['both'])}**.")
    out.append(f"- {len(sc) - bucketed} structured-content dictionaries declare no box "
               "model at all, inline or in CSS.")
    out.append("")
    out.append(f"**{len(buckets['css-only'])} of {len(sc)} structured-content dictionaries "
               "fall in the `css-only` bucket:** ignoring `styles.css` costs them every "
               "box they draw.")
    out.append("")
    return out


def render(run: dict) -> str:
    dicts = run["dictionaries"]
    support = run["support"]
    ok_tags = set(support["tags"])
    ok_styles = set(support["styles"])

    with_terms = [d for d in dicts if d["rows"] > 0]
    no_terms = [d for d in dicts if d["rows"] == 0 and not d["error"]]
    sc = [d for d in with_terms if d["kinds"].get("structured-content", 0) > 0]
    plain = [d for d in with_terms if d["kinds"].get("structured-content", 0) == 0]

    out: list[str] = []
    out.append(f"Corpus: `{run['corpus']}` - {len(dicts)} archives, "
               f"{run['row_cap'] or 'all'} rows sampled per dictionary, "
               f"{run['elapsed_s']}s.")
    out.append("")
    out.append(f"- {len(with_terms)} archives carry term banks; {len(no_terms)} carry "
               "none (frequency and pitch archives).")
    out.append(f"- {len(sc)} of {len(with_terms)} use structured content.")
    out.append(f"- {len(plain)} of {len(with_terms)} emit plain strings only, and reach "
               "full parity from the sense-splitting fix alone.")
    out.append("")

    out.append("## Tags")
    out.append("")
    rows = []
    for tag, ndicts, nnodes in rank(sc, "tags"):
        state = "kept" if tag in ok_tags else "**unsupported**"
        rows.append(f"| `{tag}` | {ndicts} | {nnodes:,} | {state} |")
    out += table(rows, "| tag | #dicts | #nodes | chibipop |", "|---|---:|---:|---|")

    out.append("## Style properties")
    out.append("")
    rows = []
    for key, ndicts, nnodes in rank(sc, "style"):
        state = "mapped" if key in ok_styles else "**unsupported**"
        rows.append(f"| `{key}` | {ndicts} | {nnodes:,} | {state} |")
    out += table(rows, "| style property | #dicts | #nodes | chibipop |", "|---|---:|---:|---|")

    out += styles_css_section(with_terms, sc)

    out.append("## Attributes")
    out.append("")
    rows = [f"| `{k}` | {n} | {v:,} |" for k, n, v in rank(sc, "attrs")]
    out += table(rows, "| attribute | #dicts | #nodes |", "|---|---:|---:|")

    out.append("## Media")
    out.append("")
    img_dicts = [d for d in sc if d["img_nodes"] > 0]
    total_img = sum(d["img_nodes"] for d in sc)
    gaiji = sum(d["gaiji_nodes"] for d in sc)
    sized = sum(d["img_sized"] for d in sc)
    unsized = sum(d["img_unsized"] for d in sc)
    out.append(f"- {len(img_dicts)} of {len(sc)} structured-content dictionaries emit "
               f"image nodes; {total_img:,} nodes in the sample.")
    out.append(f"- {gaiji:,} nodes carry a `data.gaiji` marker: these are characters the "
               "dictionary font lacks, not illustrations.")
    out.append(f"- {sized:,} declare `width` or `height`; {unsized:,} declare neither and "
               "need an intrinsic size recorded at build time.")
    out.append("")
    rows = [f"| `{k}` | {n} | {v:,} |" for k, n, v in rank(sc, "img_exts")]
    out += table(rows, "| media extension | #dicts | #nodes |", "|---|---:|---:|")
    rows = [f"| `{k}` | {n} | {v:,} |" for k, n, v in rank(sc, "img_fields")]
    out += table(rows, "| image field | #dicts | #nodes |", "|---|---:|---:|")

    out.append("### Dictionaries by image volume")
    out.append("")
    rows = [
        f"| {d['title'] or '?'} | {d['img_nodes']:,} | {d['gaiji_nodes']:,} |"
        for d in sorted(img_dicts, key=lambda d: -d["img_nodes"])[:15]
    ]
    out += table(rows, "| dictionary | image nodes | gaiji-marked |", "|---|---:|---:|")

    out.append("## Editorial drops")
    out.append("")
    owners = [(d["title"], d["dropped_by_content"]) for d in sc if d["dropped_by_content"]]
    if owners:
        for title, n in sorted(owners, key=lambda t: -t[1]):
            out.append(f"- `DROP_CONTENT` removes {n:,} nodes from **{title}**.")
    else:
        out.append("- `DROP_CONTENT` removes nothing from this corpus.")
    out.append("")
    out.append("Example content carried under other keys, which chibipop renders today:")
    out.append("")
    rows = []
    for d in sc:
        for key, n in sorted(d["data"].items(), key=lambda kv: -kv[1]):
            if "example" in key.lower() or "attribution" in key.lower():
                rows.append(f"| {d['title'] or '?'} | `{key}` | {n:,} |")
    out += table(rows, "| dictionary | data hook | #nodes |", "|---|---|---:|")

    out.append("## Nesting depth")
    out.append("")
    depths = collections.Counter(d["maxdepth"] for d in sc)
    rows = [f"| {depth} | {n} |" for depth, n in sorted(depths.items())]
    out += table(rows, "| max depth | #dicts |", "|---:|---:|")

    out.append("## Plain-string-only dictionaries")
    out.append("")
    for d in sorted(plain, key=lambda d: d["title"] or ""):
        out.append(f"- {d['title'] or Path(d['path']).name}")
    out.append("")

    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--in", dest="src", type=Path, default=DEFAULT_IN)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args()

    run = json.loads(args.src.read_text(encoding="utf-8"))
    text = render(run)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

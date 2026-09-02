# Dictionary shape census

Self-contained harness behind
[docs/research/dict-shapes.md](../../docs/research/dict-shapes.md). It walks the
structured-content tree of every `term_bank_*.json` row in a corpus of Yomitan
archives and counts what real dictionaries emit: tags, `style` properties,
`data` hooks, media node shapes, nesting depth, and the dictionary's own
`styles.css`.

It scans that `styles.css` because it is the second place a dictionary draws a
box. Yomitan scopes the stylesheet to the dictionary's own entries, and
structured content has no `class` attribute, so those rules reach content only
through tag selectors and the `data-*` attributes Yomitan derives from a node's
`data` map. The census counts the rules, the box-model declarations, the
selector kinds, the `data-*` keys the selectors match, and the at-rules. A
stylesheet that cannot be decoded or scanned records a `parse_error` and the
run continues.

It exists because the Yomitan schema is much larger than any dictionary uses.
Ranking the schema by what it *permits* sizes the work wrong. This ranks every
feature by **how many dictionaries use it**, which is the number that decides
whether skipping a feature is free or visible.

Stdlib Python only. No setup step, no downloads, nothing installed on the host.

## Run

```sh
python3 tools/dict-census/census.py ~/Downloads/dict/Japanese
python3 tools/dict-census/report.py
```

`census.py` writes `results/census.json`. `report.py` aggregates it into
`results/tables.md`, the markdown embedded in the results doc. Both paths are
overridable with `--out` / `--in`.

By default each dictionary contributes its first 30 000 term rows. Shape
coverage converges long before that, and the cap keeps a multi-gigabyte corpus
to about 15 seconds. Pass `--rows 0` to read every row when a rare tag matters.

The cap never touches `styles.css`. A stylesheet is read whole or not at all.

The corpus is **not** committed. Point the tool at any directory of `.zip`
archives.

## The support columns re-score themselves

`census.py` parses its support columns out of the Rust source instead of
duplicating them:

- `tag_for`, `style_key_for`, `NEEDLES` and `VALUE_KEYS` from
  `src/dict/gloss/parse.rs` — the tags and inline `style` keys the arena parser
  resolves, and the editorial-role classifier's needle table and convention
  keys.
- `enum Role` from `src/dict/gloss/mod.rs` — its declaration order *is* the
  classification precedence, so the report's role columns run in the same order
  the parser resolves them.
- `css_key`, `SUPPORTED_SELECTOR_KINDS` and `SUPPORTED_PSEUDO_CLASSES` from
  `src/dict/sheet/mod.rs` — the `styles.css` property table and the selector
  grammar the matcher compiles.

So the `chibipop` column in the report is always measured against the current
build, and two counts are live progress gauges that shrink as the renderer
grows: the `**unsupported**` rows, and the stylesheet rules the matcher drops.
A parse failure is a hard error rather than a silent fallback, because a stale
column would quietly overstate support.

## Layout

- `census.py` — the walk. One process per archive, `ProcessPoolExecutor`.
  A failing archive records its error and never fails the run.
- `report.py` — aggregation into markdown tables.
- `results/` — generated, gitignored.

## What it deliberately does not do

It counts shapes; it does not render them. It cannot tell you that a table
looks wrong, only that 16 dictionaries ship tables. Visual correctness is the
job of the layout fixtures and the geometry goldens.

It reads a stylesheet; it does not cascade one. It can say that a rule carries
a border, not that the border is visually a pill. There is no specificity
resolution, no `var()` substitution, and no proof that the rule wins over
another one. Cross-referencing a selector's `data-*` key against the term-bank
`data` counter shows that the key exists in the content, not that the result
looks like a pill.

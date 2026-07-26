# chibipop M0 + M1 — Lookup Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the OCR-availability probe (M0) and the complete pure lookup core (M1), ending with `chibipop lookup 食べさせられた` printing correct definitions from a SQLite database built from real Yomitan dictionary archives.

**Architecture:** An offline Python builder reads Yomitan `format: 3` archives, flattens their structured-content glossaries to plain text, merges rank-based frequency data, and emits a single SQLite file. A Rust binary loads public-domain deconjugation rules, scans every prefix of the input longest-first, deconjugates each prefix, queries the database, filters by part-of-speech, ranks, and prints. No Windows APIs are used anywhere in M1 — the entire milestone compiles and tests on any platform.

**Tech Stack:** Rust (stable-x86_64-pc-windows-msvc) with `rusqlite` (bundled SQLite), `serde`/`serde_json`, `anyhow`, `clap`. Python 3.13 standard library only (`zipfile`, `json`, `sqlite3`) — no third-party packages.

## Global Constraints

- **Design spec is authoritative:** `docs/superpowers/specs/2026-07-26-chibipop-design.md` (rev 2). Where this plan and the spec disagree, stop and reconcile before coding.
- **No `windows` crate in M1.** `src/lookup/` must compile and test without any Windows API. This is a hard architectural rule from spec §4 — a task that adds a Windows dependency to the lookup core is wrong.
- **Rust edition 2021**, toolchain `stable-x86_64-pc-windows-msvc`.
- **Python 3.13, standard library only.** The builder must run with no `pip install`.
- **Dictionary source directory:** `C:\Users\Stella\Documents\dicts`. Filenames contain `[`, `]`, and a full-width space (U+3000). Always glob the directory; never hand-type these paths. In PowerShell always use `-LiteralPath`.
- **Never use .NET Framework's `ZipArchive`** (i.e. Windows PowerShell 5.1 `[System.IO.Compression.ZipFile]`) to read these archives — it silently reports 0 entries for the Daijirin file. Python's `zipfile` is correct.
- **Frequency semantics:** `rank-based`; lower `freq` = more common; `NULL` = unranked.
- **Commit after every task.** Repo `C:\Users\Stella\chibipop`, branch `main`. Git identity is already configured repo-locally as `0x41 <76443517+stellarie@users.noreply.github.com>` — do not change it.

---

## File Structure

| Path | Responsibility |
|---|---|
| `Cargo.toml` | Crate manifest, dependencies |
| `data/deconjugator.json` | Public-domain deconjugation rules (copied from weikipop) |
| `src/main.rs` | CLI entry point; `lookup` subcommand |
| `src/lib.rs` | Crate root; `pub mod lookup;` |
| `src/lookup/mod.rs` | Module re-exports; `Dictionary` trait |
| `src/lookup/model.rs` | `TermRow`, `Sense`, `Entry`, `Hit` |
| `src/lookup/rules.rs` | Rule types, scalar-or-list handling, rule loading |
| `src/lookup/deconj.rs` | `Form`, `Deconjugator` (BFS fixpoint) |
| `src/lookup/engine.rs` | `LookupEngine`: prefix scan, POS filter, ranking |
| `src/lookup/sqlite.rs` | `SqliteDictionary` — read-only, mmap'd |
| `tests/golden.rs` | Golden corpus: sentence + offset → expected headword |
| `tools/build-dict/yomitan.py` | Yomitan archive reader |
| `tools/build-dict/flatten.py` | Structured-content → plain text |
| `tools/build-dict/freq.py` | Frequency bank parsing (both row shapes) |
| `tools/build-dict/schema.py` | SQLite DDL and writer |
| `tools/build-dict/build.py` | Builder CLI |
| `tools/build-dict/tests/` | Builder unit tests (`unittest`) |
| `docs/superpowers/findings/` | Probe findings notes |

### Schema refinement vs spec §5 — surfaced, not smuggled

The spec's `term` table has no part-of-speech column. Without one, the POS filter in spec §4.2 needs the entry's senses *during* the prefix scan, which means an entry fetch per candidate form — an N+1 query on the hot path.

**This plan adds `term.pos TEXT NOT NULL DEFAULT ''`**, holding the Yomitan `rules` field (space-separated keys such as `v1 v5k`). The POS filter then reads a column already in hand, and entries are fetched only for the final ranked results. Task 2 updates the spec's §5 schema block in the same commit so the two never diverge.

---

## Task 0: Toolchain and crate skeleton

**Files:**
- Create: `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `.gitignore`
- Create: `data/deconjugator.json` (copied)

**Interfaces:**
- Consumes: nothing
- Produces: a compiling crate named `chibipop` with a binary of the same name

**Environment facts already verified (do not re-probe):** Rust is NOT installed. Python 3.13.14 is available via mise shims. MSVC linker `14.51.36231` and Windows SDK `10.0.26100.0` ARE present at `C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools`, so the `msvc` target will link. `scoop` is at `C:\Users\Stella\scoop\shims\scoop.ps1`.

- [ ] **Step 1: Install Rust**

```bash
scoop install rustup
```

- [ ] **Step 2: Set the default toolchain and verify**

```bash
rustup default stable-x86_64-pc-windows-msvc
```

Then:

```bash
cargo --version
```

Expected: a version line such as `cargo 1.8x.x (...)`. If `rustup` is not found after install, open a new shell so scoop's shim directory is on `PATH`.

- [ ] **Step 3: Create the crate manifest**

Create `Cargo.toml`:

```toml
[package]
name = "chibipop"
version = "0.1.0"
edition = "2021"

[lib]
name = "chibipop"
path = "src/lib.rs"

[[bin]]
name = "chibipop"
path = "src/main.rs"
```

- [ ] **Step 4: Add dependencies**

Run each; `cargo add` resolves current versions so none are hard-coded here:

```bash
cargo add rusqlite --features bundled
```

```bash
cargo add serde --features derive
```

```bash
cargo add serde_json anyhow clap --features clap/derive
```

- [ ] **Step 5: Create the crate root and a placeholder binary**

Create `src/lib.rs`:

```rust
pub mod lookup;
```

Create `src/lookup/mod.rs`:

```rust
pub mod deconj;
pub mod engine;
pub mod model;
pub mod rules;
pub mod sqlite;
```

Create empty placeholder files so the module tree compiles — `src/lookup/deconj.rs`, `src/lookup/engine.rs`, `src/lookup/model.rs`, `src/lookup/rules.rs`, `src/lookup/sqlite.rs`, each containing only:

```rust
// implemented in a later task
```

Create `src/main.rs`:

```rust
fn main() {
    println!("chibipop");
}
```

- [ ] **Step 6: Copy the deconjugation rules**

```bash
mkdir -p data && cp /c/Users/Stella/weikipop/data/deconjugator.json data/deconjugator.json
```

The file's first element is the string `"This file is public domain data."` — it is public domain, so vendoring it is fine. Do not modify it.

- [ ] **Step 7: Create `.gitignore`**

```gitignore
/target
__pycache__/
*.pyc
*.sqlite
*.sqlite-journal
```

`*.sqlite` is ignored deliberately: the built dictionary is ~100MB of derived data and must never be committed.

- [ ] **Step 8: Verify the whole thing builds and links**

```bash
cargo build
```

Expected: `Finished dev profile` with no errors. This proves the MSVC linker is reachable and that `rusqlite`'s bundled SQLite compiled.

**If linking fails** with a missing `link.exe`, fall back to the GNU toolchain — `windows-rs` supports it and nothing in this plan depends on MSVC:

```bash
rustup default stable-x86_64-pc-windows-gnu
```

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "chore: rust crate skeleton, deps, vendored deconjugation rules"
```

---

## Task 1 (M0): OCR availability probe

**Files:**
- Create: `docs/superpowers/findings/2026-07-26-m0-ocr-availability.md`

**Interfaces:**
- Consumes: nothing
- Produces: a recorded yes/no answer that gates the *next* plan (M2, the OCR tier). Nothing in M1 depends on it.

This is spec §9's M0. It is a read-only probe, not production code — no Rust, no `windows-rs`. WinRT is reachable from PowerShell directly, which makes this five minutes instead of an afternoon.

- [ ] **Step 1: Run the probe**

```bash
powershell -NoProfile -Command "[Windows.Media.Ocr.OcrEngine,Windows.Media,ContentType=WindowsRuntime] | Out-Null; [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | ForEach-Object { $_.LanguageTag }"
```

Expected: a list of BCP-47 tags. **Look for `ja` or `ja-JP`.**

- [ ] **Step 2: Record the finding**

Create `docs/superpowers/findings/2026-07-26-m0-ocr-availability.md` containing: the exact command run, its complete output verbatim, and a one-line verdict — either `VERDICT: ja available — OCR tier viable as designed` or `VERDICT: ja NOT available — M2 blocked, see below`.

If `ja` is absent, also record the output of:

```bash
powershell -NoProfile -Command "Get-WindowsCapability -Online -Name 'Language.OCR~~~ja-JP*' | Select-Object Name, State"
```

This says whether the OCR language pack can simply be installed. Do not attempt to install it — that is the user's call, and it may require elevation.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs: M0 findings - Windows.Media.Ocr Japanese availability"
```

- [ ] **Step 4: Report to the user**

State the verdict plainly. If `ja` is unavailable, say so and stop treating spec §2's memory budget as settled — but **continue with M1 regardless**, since the lookup core does not depend on OCR.

---

## Task 2: SQLite schema module

**Files:**
- Create: `tools/build-dict/schema.py`
- Test: `tools/build-dict/tests/test_schema.py`
- Modify: `docs/superpowers/specs/2026-07-26-chibipop-design.md` (§5 `term` table — add the `pos` column)

**Interfaces:**
- Consumes: nothing
- Produces: `create_schema(conn) -> None`, `SCHEMA_VERSION: int`

- [ ] **Step 1: Write the failing test**

Create `tools/build-dict/tests/test_schema.py`:

```python
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
```

- [ ] **Step 2: Run it to verify it fails**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: FAIL with `ModuleNotFoundError: No module named 'schema'`.

- [ ] **Step 3: Write the implementation**

Create `tools/build-dict/schema.py`:

```python
"""SQLite schema for the chibipop dictionary."""

SCHEMA_VERSION = 1

DDL = """
CREATE TABLE dict (
    dict_id  INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE entry (
    entry_id INTEGER PRIMARY KEY,
    dict_id  INTEGER NOT NULL REFERENCES dict(dict_id),
    senses   TEXT NOT NULL
);

CREATE TABLE term (
    surface  TEXT NOT NULL,
    written  TEXT,
    reading  TEXT,
    pos      TEXT NOT NULL DEFAULT '',
    freq     INTEGER,
    entry_id INTEGER NOT NULL REFERENCES entry(entry_id)
);

CREATE TABLE meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
"""

INDEXES = """
CREATE INDEX idx_term_surface ON term(surface);
"""


def create_schema(conn):
    """Create all tables, the surface index, and record the schema version."""
    conn.executescript(DDL)
    conn.executescript(INDEXES)
    conn.execute(
        "INSERT INTO meta (k, v) VALUES ('schema_version', ?)",
        (str(SCHEMA_VERSION),),
    )
    conn.commit()
```

- [ ] **Step 4: Run the tests**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: 4 tests, all PASS.

- [ ] **Step 5: Update the spec's schema block**

In `docs/superpowers/specs/2026-07-26-chibipop-design.md` §5, replace the `CREATE TABLE term` block with the one above (the version including `pos`), and add this sentence immediately after the SQL block:

> `term.pos` holds the Yomitan `rules` field verbatim (space-separated keys such as `v1 v5k`). It is denormalised onto the term row so the part-of-speech filter in §4.2 costs no extra query on the hot path.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(builder): sqlite schema with denormalised term.pos"
```

---

## Task 3: Structured-content flattener

**Files:**
- Create: `tools/build-dict/flatten.py`
- Test: `tools/build-dict/tests/test_flatten.py`

**Interfaces:**
- Consumes: nothing
- Produces: `flatten_glossary(glossary: list) -> list[str]` — one plain-text string per glossary element

Both jitendex and 大辞林 store glossaries as Yomitan `structured-content` trees. Rendering nothing leaves every entry blank, so this is a v1 requirement (spec §5). Flattening rules: keep gloss text, keep `ruby` base text, **drop `rt` furigana**, drop images and `gaiji`, drop styling.

- [ ] **Step 1: Write the failing test**

Create `tools/build-dict/tests/test_flatten.py`:

```python
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
```

- [ ] **Step 2: Run it to verify it fails**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: FAIL with `ModuleNotFoundError: No module named 'flatten'`.

- [ ] **Step 3: Write the implementation**

Create `tools/build-dict/flatten.py`:

```python
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
```

- [ ] **Step 4: Run the tests**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: all tests PASS (4 from Task 2 plus 9 here = 13).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(builder): flatten structured-content glossaries to plain text"
```

---

## Task 4: Yomitan archive reader

**Files:**
- Create: `tools/build-dict/yomitan.py`
- Test: `tools/build-dict/tests/test_yomitan.py`

**Interfaces:**
- Consumes: `flatten_glossary` from Task 3
- Produces:
  - `read_index(zip_path: Path) -> dict`
  - `iter_terms(zip_path: Path) -> Iterator[TermEntry]` where `TermEntry` is a `NamedTuple` with fields `term: str`, `reading: str`, `definition_tags: str`, `rules: str`, `score: int`, `glossary: list`, `sequence: int | None`
  - `iter_freq_rows(zip_path: Path) -> Iterator[tuple]` — raw `term_meta_bank` rows

Term bank row layout, verified against both archives:

```
[ term, reading, definitionTags, rules, score, glossary[], sequence, termTags ]
    0      1           2           3      4        5           6         7
```

Field 3 (`rules`) is the part-of-speech key the deconjugation filter needs. Field 4 (`score`) is a sort hint, **not** a frequency.

- [ ] **Step 1: Write the failing test**

Create `tools/build-dict/tests/test_yomitan.py`:

```python
import io
import json
import sys
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from yomitan import read_index, iter_terms, iter_freq_rows


def make_archive(tmp: Path, index: dict, banks: dict) -> Path:
    p = tmp / "test.zip"
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))
    return p


class TestYomitan(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(__file__).parent / "_tmp"
        self.tmp.mkdir(exist_ok=True)

    def tearDown(self):
        for f in self.tmp.glob("*"):
            f.unlink()
        self.tmp.rmdir()

    def test_read_index(self):
        p = make_archive(self.tmp, {"title": "T", "format": 3}, {})
        self.assertEqual("T", read_index(p)["title"])

    def test_iter_terms_parses_row_layout(self):
        rows = [["食べる", "たべる", "", "v1", 100,
                 ["to eat"], 1234, ""]]
        p = make_archive(self.tmp, {"title": "T", "format": 3},
                         {"term_bank_1.json": rows})
        got = list(iter_terms(p))
        self.assertEqual(1, len(got))
        t = got[0]
        self.assertEqual("食べる", t.term)
        self.assertEqual("たべる", t.reading)
        self.assertEqual("v1", t.rules)
        self.assertEqual(100, t.score)
        self.assertEqual(["to eat"], t.glossary)
        self.assertEqual(1234, t.sequence)

    def test_iter_terms_tolerates_short_rows(self):
        rows = [["猫", "ねこ", "", "", 0, ["cat"]]]
        p = make_archive(self.tmp, {"title": "T", "format": 3},
                         {"term_bank_1.json": rows})
        t = list(iter_terms(p))[0]
        self.assertIsNone(t.sequence)

    def test_term_banks_read_in_numeric_order(self):
        p = make_archive(
            self.tmp, {"title": "T", "format": 3},
            {"term_bank_2.json": [["b", "b", "", "", 0, ["B"]]],
             "term_bank_10.json": [["c", "c", "", "", 0, ["C"]]],
             "term_bank_1.json": [["a", "a", "", "", 0, ["A"]]]})
        self.assertEqual(["a", "b", "c"], [t.term for t in iter_terms(p)])

    def test_iter_freq_rows(self):
        rows = [["の", "freq", {"value": 1}]]
        p = make_archive(self.tmp, {"title": "F", "format": 3},
                         {"term_meta_bank_1.json": rows})
        self.assertEqual([["の", "freq", {"value": 1}]], list(iter_freq_rows(p)))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run it to verify it fails**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: FAIL with `ModuleNotFoundError: No module named 'yomitan'`.

- [ ] **Step 3: Write the implementation**

Create `tools/build-dict/yomitan.py`:

```python
"""Reader for Yomitan format-3 dictionary archives.

Uses Python's zipfile deliberately: .NET Framework's ZipArchive silently
reports zero entries for some of these archives.
"""

import json
import re
import zipfile
from pathlib import Path
from typing import Iterator, NamedTuple

_NUM = re.compile(r"(\d+)")


class TermEntry(NamedTuple):
    term: str
    reading: str
    definition_tags: str
    rules: str
    score: int
    glossary: list
    sequence: "int | None"


def _sorted_banks(names, prefix):
    """Bank files sorted numerically, so bank_10 follows bank_9, not bank_1."""
    picked = [n for n in names if n.startswith(prefix) and n.endswith(".json")]

    def key(n):
        m = _NUM.search(n[len(prefix):])
        return int(m.group(1)) if m else 0

    return sorted(picked, key=key)


def read_index(zip_path: Path) -> dict:
    with zipfile.ZipFile(zip_path) as z:
        return json.loads(z.read("index.json").decode("utf-8"))


def iter_terms(zip_path: Path) -> Iterator[TermEntry]:
    with zipfile.ZipFile(zip_path) as z:
        for bank in _sorted_banks(z.namelist(), "term_bank_"):
            for row in json.loads(z.read(bank).decode("utf-8")):
                yield TermEntry(
                    term=row[0],
                    reading=row[1] if len(row) > 1 else "",
                    definition_tags=row[2] if len(row) > 2 else "",
                    rules=row[3] if len(row) > 3 else "",
                    score=row[4] if len(row) > 4 else 0,
                    glossary=row[5] if len(row) > 5 else [],
                    sequence=row[6] if len(row) > 6 else None,
                )


def iter_freq_rows(zip_path: Path) -> Iterator[list]:
    with zipfile.ZipFile(zip_path) as z:
        for bank in _sorted_banks(z.namelist(), "term_meta_bank_"):
            for row in json.loads(z.read(bank).decode("utf-8")):
                yield row
```

- [ ] **Step 4: Run the tests**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: 18 tests, all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(builder): yomitan format-3 archive reader"
```

---

## Task 5: Frequency bank parsing

**Files:**
- Create: `tools/build-dict/freq.py`
- Test: `tools/build-dict/tests/test_freq.py`

**Interfaces:**
- Consumes: `iter_freq_rows` from Task 4
- Produces: `parse_freq_rows(rows: Iterable[list]) -> dict[tuple[str, str | None], int]` — keys are `(term, reading_or_None)`, values are ranks
- Produces: `lookup_freq(table, term, reading) -> int | None` — reading-specific entry wins, falls back to the reading-agnostic entry

**The critical detail.** The frequency bank contains **two row shapes in the same file**, both verified present:

```json
["の","freq",{"value":1,"displayValue":"1㋕"}]
["乃","freq",{"reading":"の","frequency":{"value":1,"displayValue":"1㋕"}}]
```

The second is **reading-scoped** *and* nests `value` one level deeper under `frequency`. Handling only the first shape makes a rare kanji spelling silently inherit its common homophone's rank.

- [ ] **Step 1: Write the failing test**

Create `tools/build-dict/tests/test_freq.py`:

```python
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
```

- [ ] **Step 2: Run it to verify it fails**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: FAIL with `ModuleNotFoundError: No module named 'freq'`.

- [ ] **Step 3: Write the implementation**

Create `tools/build-dict/freq.py`:

```python
"""Parse Yomitan rank-based frequency banks.

Two row shapes occur in the same file:
    ["の", "freq", {"value": 1}]                                  reading-agnostic
    ["乃", "freq", {"reading": "の", "frequency": {"value": 1}}]   reading-scoped
The second nests `value` one level deeper. Missing that makes a rare kanji
spelling inherit its common homophone's rank.
"""


def _extract(payload):
    """Return (reading_or_None, rank_or_None) from a freq row's payload."""
    if isinstance(payload, int):
        return None, payload
    if not isinstance(payload, dict):
        return None, None

    reading = payload.get("reading")

    inner = payload.get("frequency")
    if isinstance(inner, int):
        return reading, inner
    if isinstance(inner, dict):
        v = inner.get("value")
        return reading, v if isinstance(v, int) else None

    v = payload.get("value")
    return reading, v if isinstance(v, int) else None


def parse_freq_rows(rows):
    """Build {(term, reading_or_None): rank}. Lower rank = more common."""
    table = {}
    for row in rows:
        if len(row) < 3 or row[1] != "freq":
            continue
        reading, rank = _extract(row[2])
        if rank is None:
            continue
        key = (row[0], reading)
        prev = table.get(key)
        if prev is None or rank < prev:
            table[key] = rank
    return table


def lookup_freq(table, term, reading):
    """Reading-specific rank if present, else the reading-agnostic one."""
    if reading:
        hit = table.get((term, reading))
        if hit is not None:
            return hit
    return table.get((term, None))
```

- [ ] **Step 4: Run the tests**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: 26 tests, all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(builder): rank-based frequency parsing, both row shapes"
```

---

## Task 6: Builder CLI and real dictionary build

**Files:**
- Create: `tools/build-dict/build.py`
- Test: `tools/build-dict/tests/test_build.py`

**Interfaces:**
- Consumes: `create_schema` (Task 2), `flatten_glossary` (Task 3), `iter_terms`/`read_index`/`iter_freq_rows` (Task 4), `parse_freq_rows`/`lookup_freq` (Task 5)
- Produces: `build(term_archives: list[tuple[Path, int]], freq_archives: list[Path], out_path: Path) -> dict` returning counts; and a CLI

Each term archive becomes one `dict` row. Entries get globally sequential `entry_id`s. Each term entry produces **one `term` row for the reading** and, when the written form differs from the reading, **a second row for the written form** — so both spellings are findable by the prefix scan.

- [ ] **Step 1: Write the failing test**

Create `tools/build-dict/tests/test_build.py`:

```python
import json
import sqlite3
import sys
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from build import build


def make_archive(path: Path, index: dict, banks: dict) -> Path:
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))
    return path


class TestBuild(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(__file__).parent / "_tmpb"
        self.tmp.mkdir(exist_ok=True)
        self.terms = make_archive(
            self.tmp / "terms.zip", {"title": "TestDict", "format": 3},
            {"term_bank_1.json": [
                ["食べる", "たべる", "", "v1", 0,
                 [{"type": "structured-content",
                   "content": {"tag": "span", "content": "to eat"}}]],
                ["ねこ", "ねこ", "", "", 0, ["cat"]],
            ]})
        self.freqs = make_archive(
            self.tmp / "freq.zip", {"title": "F", "format": 3},
            {"term_meta_bank_1.json": [["食べる", "freq", {"value": 7}]]})
        self.out = self.tmp / "out.sqlite"

    def tearDown(self):
        for f in self.tmp.glob("*"):
            f.unlink()
        self.tmp.rmdir()

    def _build(self):
        counts = build([(self.terms, 0)], [self.freqs], self.out)
        return counts, sqlite3.connect(self.out)

    def test_counts(self):
        counts, conn = self._build()
        conn.close()
        self.assertEqual(2, counts["entries"])

    def test_written_and_reading_both_indexed(self):
        _, conn = self._build()
        surfaces = {r[0] for r in conn.execute("SELECT surface FROM term")}
        conn.close()
        self.assertIn("食べる", surfaces)
        self.assertIn("たべる", surfaces)

    def test_kana_only_entry_indexed_once(self):
        _, conn = self._build()
        n = conn.execute(
            "SELECT COUNT(*) FROM term WHERE surface='ねこ'").fetchone()[0]
        conn.close()
        self.assertEqual(1, n)

    def test_pos_denormalised(self):
        _, conn = self._build()
        pos = conn.execute(
            "SELECT pos FROM term WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual("v1", pos)

    def test_glossary_flattened_into_senses(self):
        _, conn = self._build()
        row = conn.execute(
            "SELECT senses FROM entry JOIN term USING(entry_id) "
            "WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual(["to eat"], json.loads(row)[0]["glosses"])

    def test_frequency_applied(self):
        _, conn = self._build()
        f = conn.execute(
            "SELECT freq FROM term WHERE surface='食べる'").fetchone()[0]
        conn.close()
        self.assertEqual(7, f)

    def test_unranked_term_has_null_freq(self):
        _, conn = self._build()
        f = conn.execute(
            "SELECT freq FROM term WHERE surface='ねこ'").fetchone()[0]
        conn.close()
        self.assertIsNone(f)

    def test_rebuild_replaces_existing_file(self):
        self._build()[1].close()
        counts, conn = self._build()
        n = conn.execute("SELECT COUNT(*) FROM entry").fetchone()[0]
        conn.close()
        self.assertEqual(2, n)

    def test_provenance_recorded_in_meta(self):
        _, conn = self._build()
        meta = dict(conn.execute("SELECT k, v FROM meta"))
        conn.close()
        self.assertIn("built_at", meta)
        sources = json.loads(meta["source_hashes"])
        names = {s["name"] for s in sources}
        self.assertEqual({"terms.zip", "freq.zip"}, names)
        self.assertTrue(all(len(s["sha256"]) == 64 for s in sources))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run it to verify it fails**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: FAIL with `ModuleNotFoundError: No module named 'build'`.

- [ ] **Step 3: Write the implementation**

Create `tools/build-dict/build.py`:

```python
"""Build chibipop.sqlite from Yomitan format-3 archives."""

import argparse
import json
import sqlite3
import sys
from pathlib import Path

from flatten import flatten_glossary
from freq import lookup_freq, parse_freq_rows
from schema import create_schema
from yomitan import iter_freq_rows, iter_terms, read_index


def build(term_archives, freq_archives, out_path):
    """term_archives: [(Path, priority)]. Returns {'entries':n,'terms':n}."""
    out_path = Path(out_path)
    if out_path.exists():
        out_path.unlink()

    freq_table = {}
    for fa in freq_archives:
        freq_table.update(parse_freq_rows(iter_freq_rows(Path(fa))))

    conn = sqlite3.connect(out_path)
    create_schema(conn)

    entry_id = 0
    term_rows = 0

    for dict_id, (archive, priority) in enumerate(term_archives, start=1):
        archive = Path(archive)
        title = read_index(archive).get("title", archive.stem)
        conn.execute(
            "INSERT INTO dict (dict_id, name, priority) VALUES (?, ?, ?)",
            (dict_id, title, priority))

        entries, terms = [], []
        for t in iter_terms(archive):
            glosses = flatten_glossary(t.glossary)
            if not glosses:
                continue
            entry_id += 1
            senses = [{"glosses": glosses,
                       "pos": t.rules.split() if t.rules else [],
                       "misc": []}]
            entries.append(
                (entry_id, dict_id, json.dumps(senses, ensure_ascii=False)))

            written = t.term
            reading = t.reading or t.term
            rank = lookup_freq(freq_table, written, reading)

            # Reading row. `written` is NULL when the headword is kana-only.
            terms.append((reading, None if written == reading else written,
                          reading, t.rules, rank, entry_id))
            # Written row, only when it differs.
            if written != reading:
                terms.append(
                    (written, written, reading, t.rules, rank, entry_id))

            if len(entries) >= 5000:
                _flush(conn, entries, terms)
                term_rows += len(terms)
                entries, terms = [], []

        _flush(conn, entries, terms)
        term_rows += len(terms)

    _write_meta(conn, term_archives, freq_archives)
    conn.commit()
    conn.execute("ANALYZE")
    conn.commit()
    conn.close()
    return {"entries": entry_id, "terms": term_rows}


def _write_meta(conn, term_archives, freq_archives):
    """Record provenance, per spec section 5: built_at and source hashes."""
    import datetime
    import hashlib

    conn.execute(
        "INSERT OR REPLACE INTO meta (k, v) VALUES ('built_at', ?)",
        (datetime.datetime.now(datetime.timezone.utc)
         .replace(microsecond=0).isoformat(),))

    sources = []
    for path in [a for a, _ in term_archives] + list(freq_archives):
        path = Path(path)
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        sources.append({"name": path.name,
                        "bytes": path.stat().st_size,
                        "sha256": h.hexdigest()})
    conn.execute(
        "INSERT OR REPLACE INTO meta (k, v) VALUES ('source_hashes', ?)",
        (json.dumps(sources, ensure_ascii=False),))


def _flush(conn, entries, terms):
    if entries:
        conn.executemany(
            "INSERT INTO entry (entry_id, dict_id, senses) VALUES (?, ?, ?)",
            entries)
    if terms:
        conn.executemany(
            "INSERT INTO term (surface, written, reading, pos, freq, entry_id) "
            "VALUES (?, ?, ?, ?, ?, ?)", terms)


def main(argv=None):
    ap = argparse.ArgumentParser(description="Build chibipop.sqlite")
    ap.add_argument("--dicts-dir", type=Path, required=True,
                    help="directory containing Yomitan .zip archives")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args(argv)

    archives = sorted(args.dicts_dir.glob("*.zip"))
    if not archives:
        print(f"no .zip archives in {args.dicts_dir}", file=sys.stderr)
        return 1

    terms, freqs = [], []
    for a in archives:
        idx = read_index(a)
        if idx.get("frequencyMode") or "Freq" in a.name:
            freqs.append(a)
        else:
            terms.append(a)

    # Lower priority number sorts first in ranking ties.
    ranked = [(a, i) for i, a in enumerate(terms)]
    for a, p in ranked:
        print(f"term dict  [{p}] {a.name}")
    for a in freqs:
        print(f"freq dict      {a.name}")

    counts = build(ranked, freqs, args.out)
    print(f"wrote {args.out}: {counts['entries']} entries, "
          f"{counts['terms']} term rows")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 4: Run the tests**

```bash
python -m unittest discover -s tools/build-dict/tests -v
```

Expected: 35 tests, all PASS.

- [ ] **Step 5: Build the real dictionary**

```bash
python tools/build-dict/build.py --dicts-dir "C:/Users/Stella/Documents/dicts" --out data/chibipop.sqlite
```

Expected: three lines naming the archives (jitendex and 大辞林 as term dicts, jiten_freq as a freq dict), then a count line. This will take a few minutes and produce roughly 100MB.

- [ ] **Step 6: Verify the real database by hand**

```bash
python -c "import sqlite3;c=sqlite3.connect('data/chibipop.sqlite');print(c.execute(\"SELECT surface,written,reading,pos,freq FROM term WHERE surface='食べる' LIMIT 5\").fetchall())"
```

Expected: at least one row with `pos` containing `v1` and a small integer `freq`. **If `pos` is empty for 食べる, stop** — the `rules` field is not being carried through, and the POS filter in Task 10 will reject everything.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(builder): dictionary builder CLI, verified against real archives"
```

---

## Task 7: Deconjugation rule loading

**Files:**
- Create: `src/lookup/rules.rs`
- Test: inline `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `data/deconjugator.json`
- Produces:
  - `pub enum StrOrList { One(String), Many(Vec<String>) }` with `pub fn as_slice(&self) -> &[String]`
  - `pub struct Rule { pub rule_type: String, pub dec_end: StrOrList, pub con_end: StrOrList, pub dec_tag: Option<StrOrList>, pub con_tag: Option<StrOrList>, pub detail: String }`
  - `pub fn load_rules(path: &Path) -> anyhow::Result<Vec<Rule>>`

**Verified facts about the file:** 149 top-level elements — **43 comment strings and 106 rule objects**. Rule types: `stdrule` 58, `onlyfinalrule` 28, `neverfinalrule` 9, `rewriterule` 7, `contextrule` 2, `substitution` 2. All 106 have `type`, `dec_end`, `con_end`, `detail`; only 104 have `dec_tag`/`con_tag`. Any of `dec_end`/`con_end`/`dec_tag`/`con_tag` may independently be a string or a list of strings.

`load_rules` drops the two `substitution` rules, matching the reference implementation, which skips them unconditionally at apply time.

- [ ] **Step 1: Write the failing test**

Add to `src/lookup/rules.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rules_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/deconjugator.json")
    }

    #[test]
    fn loads_real_rule_file_skipping_comments_and_substitutions() {
        let rules = load_rules(&rules_path()).unwrap();
        // 106 objects in the file, minus 2 `substitution` rules.
        assert_eq!(104, rules.len());
        assert!(rules.iter().all(|r| r.rule_type != "substitution"));
    }

    #[test]
    fn parses_scalar_and_list_fields() {
        let one: StrOrList = serde_json::from_str(r#""る""#).unwrap();
        assert_eq!(&["る".to_string()], one.as_slice());
        let many: StrOrList = serde_json::from_str(r#"["く","す"]"#).unwrap();
        assert_eq!(2, many.as_slice().len());
    }

    #[test]
    fn optional_tags_absent_parse_as_none() {
        let r: Rule = serde_json::from_str(
            r#"{"type":"stdrule","dec_end":"る","con_end":"た","detail":"x"}"#,
        )
        .unwrap();
        assert!(r.dec_tag.is_none());
        assert!(r.con_tag.is_none());
    }

    #[test]
    fn every_rule_type_present_in_real_file() {
        let rules = load_rules(&rules_path()).unwrap();
        for t in ["stdrule", "onlyfinalrule", "neverfinalrule", "rewriterule",
                  "contextrule"] {
            assert!(rules.iter().any(|r| r.rule_type == t), "missing {t}");
        }
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test --lib rules
```

Expected: compile error — `load_rules`, `Rule`, and `StrOrList` do not exist.

- [ ] **Step 3: Write the implementation**

Replace the contents of `src/lookup/rules.rs` (keeping the test module at the bottom):

```rust
//! Deconjugation rule types and loading.
//!
//! `data/deconjugator.json` is a list mixing comment strings with rule
//! objects. Any of `dec_end` / `con_end` / `dec_tag` / `con_tag` may be a
//! single string or a list of strings.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StrOrList {
    One(String),
    Many(Vec<String>),
}

impl StrOrList {
    pub fn as_slice(&self) -> &[String] {
        match self {
            StrOrList::One(s) => std::slice::from_ref(s),
            StrOrList::Many(v) => v.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(rename = "type")]
    pub rule_type: String,
    pub dec_end: StrOrList,
    pub con_end: StrOrList,
    #[serde(default)]
    pub dec_tag: Option<StrOrList>,
    #[serde(default)]
    pub con_tag: Option<StrOrList>,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Element {
    Rule(Box<Rule>),
    Comment(String),
}

/// Load rules, discarding comment strings and `substitution` rules.
pub fn load_rules(path: &Path) -> Result<Vec<Rule>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading rules from {}", path.display()))?;
    let elements: Vec<Element> =
        serde_json::from_str(&text).context("parsing deconjugator.json")?;

    Ok(elements
        .into_iter()
        .filter_map(|e| match e {
            Element::Rule(r) => Some(*r),
            Element::Comment(_) => None,
        })
        .filter(|r| r.rule_type != "substitution")
        .collect())
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --lib rules
```

Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lookup): deconjugation rule types and loader"
```

---

## Task 8: Deconjugator

**Files:**
- Create: `src/lookup/deconj.rs`
- Test: inline `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `Rule`, `StrOrList` from Task 7
- Produces:
  - `pub struct Form { pub text: String, pub process: Vec<String>, pub tags: Vec<String> }` — derives `Clone, Debug, PartialEq, Eq, Hash`
  - `pub struct Deconjugator` with `pub fn new(rules: Vec<Rule>) -> Self` and `pub fn deconjugate(&self, text: &str) -> HashSet<Form>`

This is a **faithful port** of weikipop's `src/dictionary/deconjugator.py`. Two inherited behaviours are preserved deliberately and pinned by tests — do not "fix" either:

1. **`max_len` is driven by `dec_end` alone.** In the reference, `dec_end` is always a list after normalisation, so it always determines the iteration count. Exactly one rule (the mizenkei stem rule, `con_tag: "stem-mizenkei"`) has a `dec_tag` list one element longer than its `dec_end` list, so its sixth tag (`vs-i`) is unreachable and the mizenkei path tags `する` as `vk`. That rule requires a form already tagged `stem-mizenkei` (reached via e.g. しない), so bare `し`/`する` never touches it; a parallel rule reaches `vs-i` down its own path regardless, which is why the quirk has no practical effect. The upstream intent is unknown; changing it would alter deconjugation for many verbs at once.
2. **`contextrule` predicates are ignored.** Both `contextrule` rules carry a `contextrule` key (`saspecial`, `v1inftrap`) that the reference never reads, so they over-match. Spurious forms simply fail to match the dictionary, so the cost is extra queries, not wrong answers.

- [ ] **Step 1: Write the failing test**

Add to `src/lookup/deconj.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::rules::load_rules;
    use std::path::PathBuf;

    fn deconjugator() -> Deconjugator {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/deconjugator.json");
        Deconjugator::new(load_rules(&p).unwrap())
    }

    fn reaches(d: &Deconjugator, input: &str, want: &str) -> bool {
        d.deconjugate(input).iter().any(|f| f.text == want)
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(deconjugator().deconjugate("   ").is_empty());
    }

    #[test]
    fn seed_form_always_present() {
        assert!(reaches(&deconjugator(), "学校", "学校"));
    }

    #[test]
    fn past_plain_ichidan() {
        assert!(reaches(&deconjugator(), "食べた", "食べる"));
    }

    #[test]
    fn causative_passive_past() {
        assert!(reaches(&deconjugator(), "食べさせられた", "食べる"));
    }

    #[test]
    fn negative_past_godan() {
        assert!(reaches(&deconjugator(), "行かなかった", "行く"));
    }

    #[test]
    fn i_adjective_negative() {
        assert!(reaches(&deconjugator(), "面白くない", "面白い"));
    }

    #[test]
    fn te_iru_progressive() {
        assert!(reaches(&deconjugator(), "見ている", "見る"));
    }

    #[test]
    fn terminal_tag_recorded_for_filtering() {
        let d = deconjugator();
        let forms = d.deconjugate("食べた");
        let f = forms.iter().find(|f| f.text == "食べる").unwrap();
        assert_eq!(Some(&"v1".to_string()), f.tags.last());
    }

    /// Pins inherited quirk #1: `max_len` is driven by `dec_end` alone, so the
    /// mizenkei rule's sixth `dec_tag` (`vs-i`) is unreachable and する arrives
    /// tagged `vk` down that path.
    ///
    /// Verified against the unmodified Python reference: deconjugating しない
    /// yields する twice — once via ('negative', '(mizenkei)') tagged `vk`, and
    /// once via ('negative',) tagged `vs-i`. That parallel correct tagging is
    /// why the quirk has no practical effect. Bare し never reaches this rule,
    /// which needs a form already tagged `stem-mizenkei`.
    ///
    /// If this test ever needs changing, that is a deliberate semantic change
    /// to deconjugation - not a refactor.
    #[test]
    fn known_quirk_mizenkei_path_tags_suru_as_vk() {
        let d = deconjugator();
        let forms = d.deconjugate("しない");

        let via_mizenkei: Vec<&Form> = forms
            .iter()
            .filter(|f| f.text == "する" && f.process.iter().any(|p| p == "(mizenkei)"))
            .collect();
        assert!(
            !via_mizenkei.is_empty(),
            "the mizenkei path to する disappeared - deconjugation changed"
        );
        assert!(
            via_mizenkei
                .iter()
                .all(|f| f.tags.last().map(String::as_str) == Some("vk")),
            "quirk resolved upstream - update this test and the note in the module docs"
        );

        // The correct vs-i tagging is reachable in parallel; this is why the
        // quirk is harmless in practice.
        assert!(forms
            .iter()
            .any(|f| f.text == "する" && f.tags.last().map(String::as_str) == Some("vs-i")));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test --lib deconj
```

Expected: compile error — `Deconjugator` and `Form` do not exist.

- [ ] **Step 3: Write the implementation**

Replace the contents of `src/lookup/deconj.rs` (keeping the test module at the bottom):

```rust
//! Rule-based deconjugation: a BFS fixpoint over the rule set.
//!
//! Faithful port of weikipop's `src/dictionary/deconjugator.py`. Two inherited
//! behaviours are preserved deliberately:
//!
//! 1. `max_len` is driven by `dec_end` alone, so the mizenkei rule's sixth
//!    `dec_tag` (`vs-i`) is unreachable and that path tags `する` as `vk`.
//!    A parallel rule still reaches `vs-i`, so the quirk is harmless.
//! 2. `contextrule` predicates are ignored, so those two rules over-match.
//!
//! Both are pinned by tests. Changing either is a semantic change, not a
//! refactor.

use crate::lookup::rules::Rule;
use std::collections::HashSet;

pub const MAX_DECONJ_ITERATIONS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Form {
    pub text: String,
    pub process: Vec<String>,
    pub tags: Vec<String>,
}

impl Form {
    pub fn seed(text: &str) -> Self {
        Form { text: text.to_string(), process: Vec::new(), tags: Vec::new() }
    }
}

pub struct Deconjugator {
    rules: Vec<Rule>,
}

impl Deconjugator {
    pub fn new(rules: Vec<Rule>) -> Self {
        Deconjugator { rules }
    }

    pub fn deconjugate(&self, text: &str) -> HashSet<Form> {
        let clean = text.trim();
        if clean.is_empty() {
            return HashSet::new();
        }

        let mut processed: HashSet<Form> = HashSet::new();
        let mut novel: HashSet<Form> = HashSet::new();
        novel.insert(Form::seed(clean));

        let mut iteration = 0usize;
        while !novel.is_empty() {
            iteration += 1;
            if iteration > MAX_DECONJ_ITERATIONS {
                break;
            }

            let mut new_novel: HashSet<Form> = HashSet::new();
            for form in &novel {
                for rule in &self.rules {
                    if rule.rule_type == "onlyfinalrule" && !form.tags.is_empty() {
                        continue;
                    }
                    if rule.rule_type == "neverfinalrule" && form.tags.is_empty() {
                        continue;
                    }
                    for f in self.apply_rule(form, rule) {
                        if !processed.contains(&f)
                            && !novel.contains(&f)
                            && !new_novel.contains(&f)
                        {
                            new_novel.insert(f);
                        }
                    }
                }
            }

            processed.extend(novel.into_iter());
            novel = new_novel;
        }

        // The reference discards the frontier on iteration-cap break and
        // unconditionally re-adds the seed. Preserved.
        processed.insert(Form::seed(clean));
        processed
    }

    fn apply_rule(&self, form: &Form, rule: &Rule) -> Vec<Form> {
        let dec_ends = rule.dec_end.as_slice();
        let con_ends = rule.con_end.as_slice();
        let empty: &[String] = &[];
        let dec_tags = rule.dec_tag.as_ref().map(|t| t.as_slice()).unwrap_or(empty);
        let con_tags = rule.con_tag.as_ref().map(|t| t.as_slice()).unwrap_or(empty);

        // Inherited quirk #1: iteration count comes from dec_end alone.
        let max_len = dec_ends.len().max(1);

        let is_starter = matches!(
            rule.rule_type.as_str(),
            "stdrule" | "rewriterule" | "onlyfinalrule" | "contextrule"
        );

        let mut results = Vec::new();
        for i in 0..max_len {
            let con_end = if con_ends.is_empty() {
                ""
            } else {
                con_ends[i % con_ends.len()].as_str()
            };
            let dec_end = if dec_ends.is_empty() {
                ""
            } else {
                dec_ends[i % dec_ends.len()].as_str()
            };
            let con_tag = if con_tags.is_empty() {
                None
            } else {
                Some(con_tags[i % con_tags.len()].as_str())
            };
            let dec_tag = if dec_tags.is_empty() {
                None
            } else {
                Some(dec_tags[i % dec_tags.len()].as_str())
            };

            if !form.text.ends_with(con_end) {
                continue;
            }

            let tag_match = if form.tags.is_empty() {
                is_starter
            } else {
                form.tags.last().map(String::as_str) == con_tag
            };
            if !tag_match {
                continue;
            }

            if rule.rule_type == "rewriterule" && form.text != con_end {
                continue;
            }

            // `ends_with` guarantees a char boundary, so byte slicing is safe.
            let new_text = if con_end.is_empty() {
                format!("{}{}", form.text, dec_end)
            } else {
                format!(
                    "{}{}",
                    &form.text[..form.text.len() - con_end.len()],
                    dec_end
                )
            };

            let mut process = form.process.clone();
            process.push(rule.detail.clone());

            let tags = if form.tags.is_empty() {
                match dec_tag {
                    Some(t) => vec![t.to_string()],
                    None => Vec::new(),
                }
            } else {
                let mut t = form.tags[..form.tags.len() - 1].to_vec();
                if let Some(d) = dec_tag {
                    t.push(d.to_string());
                }
                t
            };

            results.push(Form { text: new_text, process, tags });
        }
        results
    }
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --lib deconj
```

Expected: 9 tests PASS. If `causative_passive_past` or `te_iru_progressive` fail, the tag-threading logic diverged from the reference — re-read `apply_rule` against `weikipop/src/dictionary/deconjugator.py:54` line by line before changing anything else.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lookup): deconjugator, faithful port with quirks pinned by tests"
```

---

## Task 9: Model types and the Dictionary trait

**Files:**
- Create: `src/lookup/model.rs`
- Modify: `src/lookup/mod.rs`
- Test: inline `#[cfg(test)]` module in `model.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub struct TermRow { pub surface: String, pub written: Option<String>, pub reading: Option<String>, pub pos: String, pub freq: Option<i64>, pub entry_id: i64 }`
  - `pub struct Sense { pub glosses: Vec<String>, pub pos: Vec<String>, pub misc: Vec<String> }` — `Serialize + Deserialize`
  - `pub struct Entry { pub entry_id: i64, pub dict_id: i64, pub senses: Vec<Sense> }`
  - `pub struct Hit { pub written: Option<String>, pub reading: Option<String>, pub match_len: usize, pub freq: Option<i64>, pub score: f64, pub process: Vec<String>, pub entry: Entry }`
  - `pub trait Dictionary { fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>>; fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>>; }`
  - `pub struct FakeDictionary` implementing `Dictionary`, for tests in Task 10

- [ ] **Step 1: Write the failing test**

Add to `src/lookup/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sense_roundtrips_through_json() {
        let json = r#"[{"glosses":["to eat"],"pos":["v1"],"misc":[]}]"#;
        let senses: Vec<Sense> = serde_json::from_str(json).unwrap();
        assert_eq!(vec!["to eat".to_string()], senses[0].glosses);
        assert_eq!(vec!["v1".to_string()], senses[0].pos);
    }

    #[test]
    fn sense_tolerates_missing_optional_fields() {
        let senses: Vec<Sense> =
            serde_json::from_str(r#"[{"glosses":["cat"]}]"#).unwrap();
        assert!(senses[0].pos.is_empty());
        assert!(senses[0].misc.is_empty());
    }

    #[test]
    fn fake_dictionary_returns_seeded_rows() {
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v1", Some(7), 1);
        assert_eq!(1, d.terms_for("食べる").unwrap().len());
        assert!(d.terms_for("猫").unwrap().is_empty());
    }

    #[test]
    fn fake_dictionary_returns_seeded_entries() {
        let mut d = FakeDictionary::new();
        d.add_entry(1, 1, vec![Sense {
            glosses: vec!["to eat".into()],
            pos: vec!["v1".into()],
            misc: vec![],
        }]);
        assert_eq!(1, d.entries(&[1]).unwrap().len());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test --lib model
```

Expected: compile error — the types do not exist.

- [ ] **Step 3: Write the implementation**

Replace the contents of `src/lookup/model.rs` (keeping the test module at the bottom):

```rust
//! Core data types and the `Dictionary` abstraction.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct TermRow {
    pub surface: String,
    pub written: Option<String>,
    pub reading: Option<String>,
    /// Yomitan `rules` field, space-separated (e.g. "v1", "v5k").
    pub pos: String,
    /// Rank; lower is more common. `None` means unranked.
    pub freq: Option<i64>,
    pub entry_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sense {
    pub glosses: Vec<String>,
    #[serde(default)]
    pub pos: Vec<String>,
    #[serde(default)]
    pub misc: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub entry_id: i64,
    pub dict_id: i64,
    pub senses: Vec<Sense>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub written: Option<String>,
    pub reading: Option<String>,
    /// Characters of input consumed by this match.
    pub match_len: usize,
    pub freq: Option<i64>,
    pub score: f64,
    /// Deconjugation trace, outermost step first.
    pub process: Vec<String>,
    pub entry: Entry,
}

pub trait Dictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>>;
    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>>;
}

/// In-memory `Dictionary` for tests.
#[derive(Default)]
pub struct FakeDictionary {
    terms: HashMap<String, Vec<TermRow>>,
    entries: HashMap<i64, Entry>,
}

impl FakeDictionary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_term(
        &mut self,
        surface: &str,
        written: Option<&str>,
        reading: Option<&str>,
        pos: &str,
        freq: Option<i64>,
        entry_id: i64,
    ) {
        self.terms.entry(surface.to_string()).or_default().push(TermRow {
            surface: surface.to_string(),
            written: written.map(str::to_string),
            reading: reading.map(str::to_string),
            pos: pos.to_string(),
            freq,
            entry_id,
        });
    }

    pub fn add_entry(&mut self, entry_id: i64, dict_id: i64, senses: Vec<Sense>) {
        self.entries.insert(entry_id, Entry { entry_id, dict_id, senses });
    }
}

impl Dictionary for FakeDictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        Ok(self.terms.get(surface).cloned().unwrap_or_default())
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        Ok(ids.iter().filter_map(|i| self.entries.get(i).cloned()).collect())
    }
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --lib model
```

Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lookup): model types, Dictionary trait, in-memory fake"
```

---

## Task 10: Lookup engine

**Files:**
- Create: `src/lookup/engine.rs`
- Test: inline `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `Deconjugator`/`Form` (Task 8), `Dictionary`/`TermRow`/`Entry`/`Hit` (Task 9)
- Produces: `pub struct LookupEngine` with `pub fn new(deconjugator: Deconjugator) -> Self` and `pub fn run<D: Dictionary>(&self, dict: &D, text: &str) -> Result<Vec<Hit>>`
- Produces: `pub const MAX_LOOKUP_CHARS: usize = 25`, `pub const MAX_RESULTS: usize = 10`

**Algorithm** (spec §4.2):
1. Clean: trim, truncate to `MAX_LOOKUP_CHARS` characters, cut at the first separator in `、。「」！？…\n`.
2. Prefix scan, longest first, over **character** counts. **No early exit** — the right word is often reachable only by deconjugating a shorter prefix.
3. For each prefix, take every deconjugated `Form` plus the literal form.
4. For each form, `terms_for(&form.text)`. Keep a row only if the form's terminal tag is absent, or appears in the row's `pos` field.
5. Keep the first (longest-prefix) hit per `entry_id`.
6. Score, sort, truncate to `MAX_RESULTS`, then fetch entries.

**Scoring**, ported from weikipop's `_calculate_priority`:

```
score = match_len
      + 10 * (1 - ln(freq) / ln(999_999))
      + 3 if the match is all-kana and unconjugated
      - deconjugation_step_count
```

- [ ] **Step 1: Write the failing test**

Add to `src/lookup/engine.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::{FakeDictionary, Sense};
    use crate::lookup::rules::load_rules;
    use std::path::PathBuf;

    fn engine() -> LookupEngine {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/deconjugator.json");
        LookupEngine::new(Deconjugator::new(load_rules(&p).unwrap()))
    }

    fn sense(gloss: &str, pos: &str) -> Sense {
        Sense {
            glosses: vec![gloss.to_string()],
            pos: vec![pos.to_string()],
            misc: vec![],
        }
    }

    fn taberu_dict() -> FakeDictionary {
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v1", Some(500), 1);
        d.add_entry(1, 1, vec![sense("to eat", "v1")]);
        d
    }

    #[test]
    fn exact_match_found() {
        let hits = engine().run(&taberu_dict(), "食べる").unwrap();
        assert_eq!(1, hits.len());
        assert_eq!(vec!["to eat".to_string()], hits[0].entry.senses[0].glosses);
    }

    #[test]
    fn conjugated_form_deconjugates_to_headword() {
        let hits = engine().run(&taberu_dict(), "食べさせられた").unwrap();
        assert_eq!(1, hits.len());
        assert_eq!(Some("食べる".to_string()), hits[0].written);
    }

    #[test]
    fn trailing_context_is_ignored_by_prefix_scan() {
        let hits = engine().run(&taberu_dict(), "食べるのが好き").unwrap();
        assert!(!hits.is_empty());
        assert_eq!(Some("食べる".to_string()), hits[0].written);
    }

    #[test]
    fn input_cut_at_japanese_separator() {
        assert_eq!("食べる", clean_input("食べる。おいしい"));
    }

    #[test]
    fn input_truncated_by_character_not_byte_count() {
        let long = "あ".repeat(40);
        assert_eq!(MAX_LOOKUP_CHARS, clean_input(&long).chars().count());
    }

    #[test]
    fn empty_input_returns_no_hits() {
        assert!(engine().run(&taberu_dict(), "   ").unwrap().is_empty());
    }

    #[test]
    fn pos_filter_rejects_mismatched_part_of_speech() {
        // A godan verb row cannot satisfy an ichidan deconjugation.
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v5r", Some(500), 1);
        d.add_entry(1, 1, vec![sense("wrong pos", "v5r")]);
        let hits = engine().run(&d, "食べさせられた").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn empty_pos_column_is_never_filtered_out() {
        let mut d = FakeDictionary::new();
        d.add_term("ねこ", None, Some("ねこ"), "", None, 1);
        d.add_entry(1, 1, vec![sense("cat", "")]);
        assert_eq!(1, engine().run(&d, "ねこ").unwrap().len());
    }

    #[test]
    fn more_common_word_ranks_first() {
        let mut d = FakeDictionary::new();
        d.add_term("はし", None, Some("はし"), "", Some(50), 1);
        d.add_entry(1, 1, vec![sense("chopsticks", "")]);
        d.add_term("はし", None, Some("はし"), "", Some(9000), 2);
        d.add_entry(2, 1, vec![sense("bridge", "")]);
        let hits = engine().run(&d, "はし").unwrap();
        assert_eq!(vec!["chopsticks".to_string()], hits[0].entry.senses[0].glosses);
    }

    #[test]
    fn longer_match_outranks_shorter() {
        let mut d = FakeDictionary::new();
        d.add_term("日本", Some("日本"), Some("にほん"), "", Some(100), 1);
        d.add_entry(1, 1, vec![sense("Japan", "")]);
        d.add_term("日本語", Some("日本語"), Some("にほんご"), "", Some(900), 2);
        d.add_entry(2, 1, vec![sense("Japanese language", "")]);
        let hits = engine().run(&d, "日本語").unwrap();
        assert_eq!(Some("日本語".to_string()), hits[0].written);
    }

    #[test]
    fn results_truncated_to_max() {
        let mut d = FakeDictionary::new();
        for i in 1..=25 {
            d.add_term("あ", None, Some("あ"), "", Some(i), i);
            d.add_entry(i, 1, vec![sense("x", "")]);
        }
        assert_eq!(MAX_RESULTS, engine().run(&d, "あ").unwrap().len());
    }

    #[test]
    fn each_entry_appears_at_most_once() {
        let hits = engine().run(&taberu_dict(), "食べさせられた").unwrap();
        let mut ids: Vec<i64> = hits.iter().map(|h| h.entry.entry_id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test --lib engine
```

Expected: compile error — `LookupEngine` and `clean_input` do not exist.

- [ ] **Step 3: Write the implementation**

Replace the contents of `src/lookup/engine.rs` (keeping the test module at the bottom):

```rust
//! The lookup engine: prefix scan, deconjugation, POS filter, ranking.

use crate::lookup::deconj::{Deconjugator, Form};
use crate::lookup::model::{Dictionary, Entry, Hit, TermRow};
use anyhow::Result;
use std::collections::HashMap;

pub const MAX_LOOKUP_CHARS: usize = 25;
pub const MAX_RESULTS: usize = 10;
const DEFAULT_FREQ: f64 = 999_999.0;
const SEPARATORS: &[char] = &['、', '。', '「', '」', '！', '？', '…', '\n'];

/// Trim, cut at the first separator, and truncate to MAX_LOOKUP_CHARS.
pub fn clean_input(text: &str) -> String {
    let trimmed = text.trim();
    let cut = match trimmed.find(SEPARATORS) {
        Some(i) => &trimmed[..i],
        None => trimmed,
    };
    cut.chars().take(MAX_LOOKUP_CHARS).collect()
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{30FC}')
}

/// Ported from weikipop's `_calculate_priority`.
fn score(match_len: usize, freq: Option<i64>, kana_bonus: bool, steps: usize) -> f64 {
    let f = freq.map(|v| v as f64).unwrap_or(DEFAULT_FREQ).max(1.0);
    let mut s = match_len as f64;
    s += 10.0 * (1.0 - f.ln() / DEFAULT_FREQ.ln());
    if kana_bonus {
        s += 3.0;
    }
    s -= steps as f64;
    s
}

struct Candidate {
    row: TermRow,
    match_len: usize,
    steps: usize,
    process: Vec<String>,
}

pub struct LookupEngine {
    deconjugator: Deconjugator,
}

impl LookupEngine {
    pub fn new(deconjugator: Deconjugator) -> Self {
        LookupEngine { deconjugator }
    }

    pub fn run<D: Dictionary>(&self, dict: &D, text: &str) -> Result<Vec<Hit>> {
        let cleaned = clean_input(text);
        if cleaned.is_empty() {
            return Ok(Vec::new());
        }

        let chars: Vec<char> = cleaned.chars().collect();
        // First (longest-prefix) hit per entry wins.
        let mut best: HashMap<i64, Candidate> = HashMap::new();

        // No early exit: shorter prefixes can deconjugate to different words.
        for prefix_len in (1..=chars.len()).rev() {
            let prefix: String = chars[..prefix_len].iter().collect();

            let mut forms: Vec<Form> = self.deconjugator.deconjugate(&prefix)
                .into_iter()
                .collect();
            forms.push(Form::seed(&prefix));

            for form in &forms {
                let required_pos = form.tags.last().map(String::as_str);
                for row in dict.terms_for(&form.text)? {
                    if let Some(need) = required_pos {
                        if !row.pos.is_empty()
                            && !row.pos.split_whitespace().any(|p| p == need)
                        {
                            continue;
                        }
                    }
                    best.entry(row.entry_id).or_insert_with(|| Candidate {
                        row: row.clone(),
                        match_len: prefix_len,
                        steps: form.process.len(),
                        process: form.process.clone(),
                    });
                }
            }
        }

        let mut ranked: Vec<Candidate> = best.into_values().collect();

        // Kana bonus applies to unconjugated all-kana matches only.
        ranked.sort_by(|a, b| {
            let sa = score(
                a.match_len,
                a.row.freq,
                a.steps == 0 && a.row.surface.chars().all(is_kana),
                a.steps,
            );
            let sb = score(
                b.match_len,
                b.row.freq,
                b.steps == 0 && b.row.surface.chars().all(is_kana),
                b.steps,
            );
            b.match_len
                .cmp(&a.match_len)
                .then(sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.row.entry_id.cmp(&b.row.entry_id))
        });
        ranked.truncate(MAX_RESULTS);

        let ids: Vec<i64> = ranked.iter().map(|c| c.row.entry_id).collect();
        let entries: HashMap<i64, Entry> = dict
            .entries(&ids)?
            .into_iter()
            .map(|e| (e.entry_id, e))
            .collect();

        Ok(ranked
            .into_iter()
            .filter_map(|c| {
                let entry = entries.get(&c.row.entry_id)?.clone();
                let kana_bonus =
                    c.steps == 0 && c.row.surface.chars().all(is_kana);
                Some(Hit {
                    score: score(c.match_len, c.row.freq, kana_bonus, c.steps),
                    written: c.row.written.clone(),
                    reading: c.row.reading.clone(),
                    match_len: c.match_len,
                    freq: c.row.freq,
                    process: c.process,
                    entry,
                })
            })
            .collect())
    }
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --lib engine
```

Expected: 12 tests PASS.

If `pos_filter_rejects_mismatched_part_of_speech` fails, check that the filter skips rows whose `pos` is **empty** rather than rejecting them — an empty POS column means "unknown", not "matches nothing", and rejecting it would blank out every kana-only entry.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lookup): prefix-scan engine with POS filter and ranking"
```

---

## Task 11: SQLite dictionary

**Files:**
- Create: `src/lookup/sqlite.rs`
- Test: inline `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `Dictionary`, `TermRow`, `Entry`, `Sense` (Task 9)
- Produces: `pub struct SqliteDictionary` with `pub fn open(path: &Path) -> Result<Self>`, implementing `Dictionary`

Opened **read-only** with a 256MB mmap window, so resident memory stays near zero (spec §3 D3).

- [ ] **Step 1: Write the failing test**

Add to `src/lookup/sqlite.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::Dictionary;

    #[test]
    fn reads_terms_and_entries() {
        let dir = std::env::temp_dir().join("chibipop_sqlite_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.sqlite");
        let _ = std::fs::remove_file(&path);

        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE dict(dict_id INTEGER PRIMARY KEY, name TEXT, priority INTEGER);
                 CREATE TABLE entry(entry_id INTEGER PRIMARY KEY, dict_id INTEGER, senses TEXT);
                 CREATE TABLE term(surface TEXT, written TEXT, reading TEXT, pos TEXT,
                                   freq INTEGER, entry_id INTEGER);
                 CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT);
                 CREATE INDEX idx_term_surface ON term(surface);
                 INSERT INTO dict VALUES (1,'d',0);
                 INSERT INTO entry VALUES (1,1,'[{\"glosses\":[\"to eat\"],\"pos\":[\"v1\"],\"misc\":[]}]');
                 INSERT INTO term VALUES ('食べる','食べる','たべる','v1',500,1);",
            )
            .unwrap();
        }

        let d = SqliteDictionary::open(&path).unwrap();
        let rows = d.terms_for("食べる").unwrap();
        assert_eq!(1, rows.len());
        assert_eq!("v1", rows[0].pos);
        assert_eq!(Some(500), rows[0].freq);

        let entries = d.entries(&[1]).unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(vec!["to eat".to_string()], entries[0].senses[0].glosses);

        assert!(d.terms_for("いぬ").unwrap().is_empty());
        assert!(d.entries(&[]).unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test --lib sqlite
```

Expected: compile error — `SqliteDictionary` does not exist.

- [ ] **Step 3: Write the implementation**

Replace the contents of `src/lookup/sqlite.rs` (keeping the test module at the bottom):

```rust
//! Read-only, memory-mapped SQLite implementation of `Dictionary`.

use crate::lookup::model::{Dictionary, Entry, Sense, TermRow};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub struct SqliteDictionary {
    conn: Connection,
}

impl SqliteDictionary {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening dictionary {}", path.display()))?;
        // 256MB mmap window: the OS pages what is touched, so resident
        // memory stays near zero.
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
        Ok(SqliteDictionary { conn })
    }
}

impl Dictionary for SqliteDictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT surface, written, reading, pos, freq, entry_id \
             FROM term WHERE surface = ?1",
        )?;
        let rows = stmt.query_map([surface], |r| {
            Ok(TermRow {
                surface: r.get(0)?,
                written: r.get(1)?,
                reading: r.get(2)?,
                pos: r.get(3)?,
                freq: r.get(4)?,
                entry_id: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(ids.len());
        let mut stmt = self.conn.prepare_cached(
            "SELECT entry_id, dict_id, senses FROM entry WHERE entry_id = ?1",
        )?;
        for id in ids {
            let mut rows = stmt.query([id])?;
            if let Some(r) = rows.next()? {
                let senses_json: String = r.get(2)?;
                let senses: Vec<Sense> = serde_json::from_str(&senses_json)
                    .with_context(|| format!("parsing senses for entry {id}"))?;
                out.push(Entry {
                    entry_id: r.get(0)?,
                    dict_id: r.get(1)?,
                    senses,
                });
            }
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --lib sqlite
```

Expected: 1 test PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lookup): read-only mmap'd sqlite dictionary"
```

---

## Task 12: CLI and golden corpus

**Files:**
- Modify: `src/main.rs`
- Create: `tests/golden.rs`

**Interfaces:**
- Consumes: everything from Tasks 7–11
- Produces: `chibipop lookup <text> [--dict <path>] [--rules <path>]`

The golden corpus is the highest-value test in the project (spec §8). It runs against the **real** database, so it is skipped automatically when that file is absent — a fresh clone must not fail its test suite for want of a 100MB artifact.

- [ ] **Step 1: Write the failing test**

Create `tests/golden.rs`:

```rust
//! Golden corpus: (input, expected headword) against the real dictionary.
//!
//! Skipped when data/chibipop.sqlite is absent, so a fresh clone still
//! passes `cargo test`. Build it with tools/build-dict/build.py.

use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use std::path::PathBuf;

const CASES: &[(&str, &str)] = &[
    ("食べる", "食べる"),
    ("食べさせられた", "食べる"),
    ("行かなかった", "行く"),
    ("面白くない", "面白い"),
    ("見ている", "見る"),
    ("日本語", "日本語"),
    ("学校に行く", "学校"),
    ("読みました", "読む"),
    ("小さかった", "小さい"),
    ("してしまった", "する"),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn golden_corpus() {
    let db = root().join("data/chibipop.sqlite");
    if !db.exists() {
        eprintln!("SKIP golden_corpus: {} not built", db.display());
        return;
    }

    let dict = SqliteDictionary::open(&db).unwrap();
    let engine = LookupEngine::new(Deconjugator::new(
        load_rules(&root().join("data/deconjugator.json")).unwrap(),
    ));

    let mut failures = Vec::new();
    for (input, expected) in CASES {
        let hits = engine.run(&dict, input).unwrap();
        let found = hits.iter().take(3).any(|h| {
            h.written.as_deref() == Some(*expected)
                || h.reading.as_deref() == Some(*expected)
        });
        if !found {
            let top: Vec<String> = hits
                .iter()
                .take(3)
                .map(|h| {
                    h.written.clone().or(h.reading.clone()).unwrap_or_default()
                })
                .collect();
            failures.push(format!(
                "{input}: expected {expected} in top 3, got {top:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "golden failures:\n{}", failures.join("\n"));
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test --test golden
```

Expected: compile error — `chibipop::lookup::*` paths do not resolve, because `src/lookup/mod.rs` does not re-export them publicly yet.

- [ ] **Step 3: Make the modules public and write the CLI**

Confirm `src/lookup/mod.rs` reads exactly:

```rust
pub mod deconj;
pub mod engine;
pub mod model;
pub mod rules;
pub mod sqlite;
```

Replace `src/main.rs`:

```rust
use anyhow::{Context, Result};
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "chibipop", about = "Japanese lookup engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Look up Japanese text and print ranked results.
    Lookup {
        text: String,
        #[arg(long, default_value = "data/chibipop.sqlite")]
        dict: PathBuf,
        #[arg(long, default_value = "data/deconjugator.json")]
        rules: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Lookup { text, dict, rules } => {
            let dictionary = SqliteDictionary::open(&dict).with_context(|| {
                format!("opening {} - build it with tools/build-dict/build.py",
                        dict.display())
            })?;
            let engine =
                LookupEngine::new(Deconjugator::new(load_rules(&rules)?));

            let hits = engine.run(&dictionary, &text)?;
            if hits.is_empty() {
                println!("no results for {text}");
                return Ok(());
            }
            for (i, h) in hits.iter().enumerate() {
                let head = h.written.clone().or(h.reading.clone())
                    .unwrap_or_default();
                let reading = h.reading.clone().unwrap_or_default();
                let freq = h.freq
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{}. {head} [{reading}]  freq={freq}  match={}  score={:.2}",
                    i + 1, h.match_len, h.score
                );
                if !h.process.is_empty() {
                    println!("     via: {}", h.process.join(" -> "));
                }
                for sense in &h.entry.senses {
                    println!("     {}", sense.glosses.join("; "));
                }
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 4: Run the whole test suite**

```bash
cargo test
```

Expected: every unit test from Tasks 7–11 passes, and `golden_corpus` passes (or prints `SKIP` if the database has not been built).

- [ ] **Step 5: Run the real acceptance check**

```bash
cargo run --release -- lookup 食べさせられた
```

Expected: `食べる` as result 1, with its reading, a frequency rank, a `via:` deconjugation trace, and English glosses from Jitendex.

Then confirm the Japanese-Japanese dictionary is reachable too:

```bash
cargo run --release -- lookup 面白くない
```

Expected: `面白い` in the top three, with both Jitendex and 大辞林 senses present across the results.

- [ ] **Step 6: Measure and report honestly**

```bash
powershell -NoProfile -Command "$p = Start-Process -FilePath .\target\release\chibipop.exe -ArgumentList 'lookup','日本語' -PassThru -NoNewWindow -Wait; Write-Output ('exit ' + $p.ExitCode)"
```

Record in the commit message: the golden corpus pass count, and whether both dictionaries appear in results. **Do not claim a memory figure** — spec §2's budget is measured at M3, when a resident process exists. A CLI that exits immediately cannot measure it.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: chibipop lookup CLI and golden corpus"
```

---

## Done criteria for M0 + M1

- [ ] `cargo test` passes with no failures
- [ ] `chibipop lookup 食べさせられた` prints 食べる with glosses, from the real dictionary
- [ ] The golden corpus runs against the real database and passes all 10 cases
- [ ] M0's finding is recorded in `docs/superpowers/findings/` with a plain verdict
- [ ] `src/lookup/` contains no reference to the `windows` crate
- [ ] Both inherited deconjugation quirks are documented in `deconj.rs` and pinned by tests

**Explicitly NOT done, and not claimed:** no OCR, no UIA, no popup, no window, no memory measurement, no hotkey. Those are M2–M5 and get their own plans.

# The Rust builder and the dictionary library — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the Yomitan dictionary builder from Python into Rust, then let
someone add and remove dictionaries from inside the settings window — so
chibipop needs no toolchain at all and ships no dictionary it has no right to
redistribute.

**Architecture:** Four pure modules port `tools/build-dict`'s logic
(`frequency`, `glossary`, `archive`, `build`), exposed as a `build-dict`
subcommand. A `library/` folder beside the executable holds copies of the
user's archives; the `.sqlite` becomes a cache regenerated from it by a
**child process**, so the memory spike never lands in the process that idles
at 12 MB. The settings window stages Add/Remove and rebuilds once on Apply.

**Tech Stack:** Rust (stable, MSVC), `zip` 8.6 (deflate only), `rusqlite`
(bundled), `serde_json`, hand-rolled Win32.

**Covers:** design §2–§8 phases 2 and 3
([spec](../specs/2026-07-29-dictionary-management-design.md)).

> ### Detail level: all nine tasks are executable
>
> Tasks 1–5 shipped. **Tasks 6–9 were expanded on 2026-07-29 after Task 5
> landed**, which is what the earlier version of this callout required: their
> interfaces are now read off the merged code rather than proposed. Every
> signature, control id, line number and Cargo feature below was verified
> against the tree at `db3546c`, not recalled.
>
> What that re-reading changed, versus the outlines:
>
> - **A data-loss hazard the outline did not see.** If `library/` is empty and
>   `data/chibipop.sqlite` already has dictionaries — every current user,
>   because the shipped database was built by the Python builder — then adding
>   one archive and pressing Apply rebuilds from a library of one and destroys
>   the other three. Task 6 and Task 8 now carry the mitigation.
> - **`Win32_UI_Controls_Dialogs` is not an enabled feature.** `GetOpenFileNameW`
>   does not compile today. Task 8 adds it.
> - **The D9 disarm is two calls, not one** — `Hooks::set_scroll_armed(false)`
>   *and* `drain_capture_guard()` (`app.rs:709-710`). The outline named one.
> - **Control ids 100–116 are taken.** New ones start at 117.

**Where `library/` lives:** beside the executable, via
`paths::beside_exe("library")` — the same rule `chibipop.toml` uses, and for
the same reason. Task 6's functions take the directory as a parameter so they
stay testable against a temporary folder; Task 9 is the only place that
supplies the real one.

## Global Constraints

- **The Python builder is the oracle.** `tools/build-dict/*.py` works, is
  tested, and has produced a 232 MB database from real archives. Every ported
  module is verified by making it agree with its Python counterpart on
  `tests/fixtures/yomitan/`. **Read the Python source as the specification** —
  do not reimplement from the Yomitan format docs.
- **Do not modify anything under `tools/build-dict/`.** It must keep working
  and keep passing its 58 tests; it is the reference.
- **Comments under 30 characters of text**, and only where the code cannot be
  made obvious. `// SAFETY:` blocks are the sole exemption and keep their full
  explanation.
- **`cargo fmt` is never run.** The repo has never been rustfmt-clean.
- **Stage by name.** Never `git add -u` or `git add -A`.
- **Tier 0 must stay green:** all Rust tests pass (**252** before this plan —
  sum all five `test result` lines, never quote the first), clippy
  accepted-error count **exactly 5**, bin-target clippy **0**, release builds,
  Python builder 58 tests.
- **Binary under 100 MB** (3.4 MB today). The `zip` dependency is the first
  real growth in this project — every task that touches `Cargo.toml` reports
  the size after.
- **Never `AllocConsole` or `FreeConsole`.** chibipop is console-subsystem and
  `println!` aborts the process when a write fails. `ui::console` hides and
  shows only.
- Three unsafe lints are enabled at both crate roots; every unsafe operation
  goes inside a visible `unsafe` block.

### Environment

scoop's rustup creates no shims. **Every** cargo command needs:

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"
export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup
```

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `src/dict/mod.rs` | Module root | 1 |
| `src/dict/frequency.rs` | Parse rank rows, both nesting shapes | 1 |
| `src/dict/glossary.rs` | Flatten structured content; extract part of speech | 2 |
| `src/dict/archive.rs` | Read a Yomitan zip | 3 |
| `src/dict/build.rs` | Create schema, write entries and terms | 4 |
| `src/main.rs` | `build-dict` subcommand | 5 |
| `src/library.rs` | The archive folder and its manifest | 6 |
| `src/rebuild.rs` | Spawn the builder, read progress, swap the result | 7 |
| `src/ui/settings_window.rs` | Two dictionary groups, Add/Remove, file picker | 8 |
| `src/settings.rs`, `src/app.rs` | Staged changes wired to Apply | 9 |

---

## Task 1: Frequency parsing

The smallest pure port. Establishes the pattern the next three follow.

**Reference:** `tools/build-dict/freq.py` (53 lines) and its tests at
`tools/build-dict/tests/test_freq.py`. Read both before writing anything.

**Files:**
- Create: `src/dict/mod.rs`, `src/dict/frequency.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub type FreqTable = HashMap<(String, Option<String>), i64>;
  pub fn parse_freq_rows(rows: &[serde_json::Value]) -> FreqTable;
  pub fn lookup_freq(table: &FreqTable, term: &str, reading: Option<&str>) -> Option<i64>;
  ```

**The trap, in the module's own words:** a frequency row has two shapes in one
file — `["の","freq",{"value":1}]` and
`["乃","freq",{"reading":"の","frequency":{"value":1}}]`. The second nests
`value` one level deeper. Missing it makes a rare kanji spelling inherit its
common homophone's rank. `freq.py:11-28` is the exact decision tree.

Two more behaviours that are easy to lose:
- **Lowest rank wins** on a duplicate key (`freq.py:41-43`).
- **Reading-specific beats reading-agnostic** (`freq.py:47-53`) — that is the
  precedence the fixture's 猫 pair exists to pin.

- [ ] **Step 1: Write the failing tests**

Create `src/dict/frequency.rs` with only a test module. Port **every** case in
`tools/build-dict/tests/test_freq.py`, plus these four, which are the ones the
fixture depends on:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows(v: serde_json::Value) -> Vec<serde_json::Value> {
        v.as_array().unwrap().clone()
    }

    #[test]
    fn a_reading_agnostic_row_is_keyed_with_no_reading() {
        let t = parse_freq_rows(&rows(json!([["の", "freq", {"value": 1}]])));
        assert_eq!(Some(1), t.get(&("の".to_string(), None)).copied());
    }

    #[test]
    fn a_reading_scoped_row_nests_value_one_level_deeper() {
        let t = parse_freq_rows(&rows(json!([
            ["乃", "freq", {"reading": "の", "frequency": {"value": 7}}]
        ])));
        assert_eq!(Some(7), t.get(&("乃".to_string(), Some("の".to_string()))).copied());
    }

    #[test]
    fn the_lowest_rank_wins_for_one_key() {
        let t = parse_freq_rows(&rows(json!([
            ["猫", "freq", {"value": 900}],
            ["猫", "freq", {"value": 40}]
        ])));
        assert_eq!(Some(40), t.get(&("猫".to_string(), None)).copied());
    }

    #[test]
    fn a_reading_specific_rank_beats_a_reading_agnostic_one() {
        let t = parse_freq_rows(&rows(json!([
            ["猫", "freq", {"value": 9999}],
            ["猫", "freq", {"reading": "ねこ", "frequency": {"value": 42}}]
        ])));
        assert_eq!(Some(42), lookup_freq(&t, "猫", Some("ねこ")));
        assert_eq!(Some(9999), lookup_freq(&t, "猫", Some("びょう")));
        assert_eq!(None, lookup_freq(&t, "犬", None));
    }

    #[test]
    fn rows_that_are_not_freq_rows_are_skipped() {
        let t = parse_freq_rows(&rows(json!([
            ["x", "pitch", {"value": 1}],
            ["y", "freq"],
            ["z", "freq", {"no_value_here": true}]
        ])));
        assert!(t.is_empty());
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test --lib dict::frequency
```
Expected: FAIL to compile — the functions do not exist.

- [ ] **Step 3: Implement**

Write `parse_freq_rows` and `lookup_freq` above the test module, following
`freq.py`'s decision tree exactly. Create `src/dict/mod.rs` containing
`pub mod frequency;` and add `pub mod dict;` to `src/lib.rs`, keeping the
module list alphabetical.

- [ ] **Step 4: Run them**

```bash
cargo test --lib dict::frequency
cargo test 2>&1 | grep -E "^test result" | awk '/ok\./ {s+=$4} END {print "TOTAL: " s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
```
Expected: all green; clippy still exactly **5**.

- [ ] **Step 5: Commit**

```bash
git add src/dict/mod.rs src/dict/frequency.rs src/lib.rs
git commit -m "feat(dict): port the frequency parser

First of four pure modules replacing tools/build-dict. freq.py stays as the
oracle; this must agree with it.

The shape that matters: a rank row nests `value` one level deeper when it is
scoped to a reading, and a reading-specific rank beats a reading-agnostic one
for the same term. Getting that wrong makes a rare kanji spelling inherit its
common homophone's rank, which is the failure freq.py's docstring names."
```

---

## Task 2: Glossary flattening and part of speech

**The hardest module in this plan.** 141 lines of irregular nested structures,
and the likeliest place for an answer that is wrong and still compiles.

**Reference:** `tools/build-dict/flatten.py` and
`tools/build-dict/tests/test_flatten.py` (152 lines). Read both fully. Port
every test case.

**Files:**
- Create: `src/dict/glossary.rs`
- Modify: `src/dict/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn flatten_glossary(glossary: &serde_json::Value) -> Vec<String>;
  pub fn extract_pos(glossary: &serde_json::Value) -> Vec<String>;
  ```

**The traps, each of which has a line number:**

1. **The block sentinel.** `flatten.py:41` uses `" LI "` to mark a
   boundary, and `_tidy` (`:72-79`) splits on it, strips each part, drops
   empties, and joins with `"; "`. Any mechanism producing identical output is
   fine — a `Vec<String>` of parts is cleaner in Rust — but the **output must
   match character for character**.
2. **Any `data.content` marker is a boundary**, not just `li` (`:66`).
   Without it, "1-dan" and "transitive" fuse into "1-dantransitive".
3. **Whitespace collapsing includes U+3000**, the ideographic space
   (`flatten.py:43`, `[ \t　]+` → one ASCII space). A Rust
   `char::is_whitespace` check is **not** equivalent — it would also eat the
   newlines `<br>` deliberately produces.
4. **`<br>` becomes `"\n"`**, and `_tidy` then strips each line individually
   (`:78`) while keeping the line breaks.
5. **Part-of-speech nodes render as empty** in `_render` (`:58`) but are
   collected separately by `_collect_pos` (`:82-100`), **ordered and
   de-duplicated by exact string** (`:96-97`).
6. **Dropped subtrees are dropped for POS too** (`:92-93`) — a
   part-of-speech marker inside an example sentence is not a definition.
7. `flatten_glossary` handles four item shapes (`:124-138`): a bare string, a
   `{"type":"text"}` object, a `{"type":"structured-content"}` object, a
   `{"type":"image"}` object (empty), and anything else rendered directly.
   **Empty results are dropped**, so an image-only sense yields `[]`.

- [ ] **Step 1: Write the failing tests**

Port every case from `tools/build-dict/tests/test_flatten.py`. In addition,
these five pin the traps above and must be present:

```rust
    #[test]
    fn adjacent_marked_blocks_are_separated_not_fused() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "sense"}, "content": "one"},
            {"tag": "span", "data": {"content": "sense"}, "content": "two"}
        ]}]);
        assert_eq!(vec!["one; two".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn an_ideographic_space_collapses_but_a_line_break_survives() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "あ\u{3000}\u{3000}い"},
            {"tag": "br"},
            {"tag": "span", "content": "  う  "}
        ]}]);
        assert_eq!(vec!["あ い\nう".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn furigana_and_images_are_dropped_but_ruby_base_text_is_kept() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "content": "ねこ"}
            ]},
            {"tag": "img", "content": "ignored"}
        ]}]);
        assert_eq!(vec!["猫".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn part_of_speech_is_collected_in_order_without_duplicates() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "suru"},
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "content": "chatting"}
        ]}]);
        assert_eq!(vec!["noun".to_string(), "suru".to_string()], extract_pos(&g));
        // And it must not also appear in the gloss text.
        assert_eq!(vec!["chatting".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn an_example_sentence_contributes_neither_text_nor_part_of_speech() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "verb"},
                {"tag": "span", "content": "a sentence"}
            ]},
            {"tag": "span", "content": "real gloss"}
        ]}]);
        assert_eq!(vec!["real gloss".to_string()], flatten_glossary(&g));
        assert!(extract_pos(&g).is_empty());
    }

    #[test]
    fn an_image_only_sense_yields_nothing() {
        assert!(flatten_glossary(&json!([{"type": "image", "path": "x.png"}])).is_empty());
    }
```

- [ ] **Step 2: Run and watch them fail**

```bash
cargo test --lib dict::glossary
```

- [ ] **Step 3: Implement, reading `flatten.py` as you go**

- [ ] **Step 4: Cross-check against the oracle on real data**

Unit tests are not enough for this module. Compare both implementations on
the real archives — this is the step that catches a subtly wrong flattener:

```bash
cargo build --release
# Dump the Python builder's first 200 glosses from the fixture.
cd tools/build-dict && python -c "
import json, sys; sys.path.insert(0, '.')
from yomitan import iter_terms
from flatten import flatten_glossary, extract_pos
from pathlib import Path
for t in list(iter_terms(Path('../../tests/fixtures/yomitan/terms.zip')))[:200]:
    print(json.dumps({'g': flatten_glossary(t.glossary), 'p': extract_pos(t.glossary)}, ensure_ascii=False))
" > /tmp/py_gloss.txt
wc -l /tmp/py_gloss.txt
```

Write a temporary Rust test (delete it before committing) that reads the same
fixture and prints the same JSON lines, then `diff` them. **They must be
byte-identical.** If they differ, the Python output is correct by definition.

- [ ] **Step 5: Gate and commit**

```bash
cargo test 2>&1 | grep -E "^test result" | awk '/ok\./ {s+=$4} END {print "TOTAL: " s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
git add src/dict/glossary.rs src/dict/mod.rs
git commit -m "feat(dict): port glossary flattening and part-of-speech extraction

The hardest of the four ports: 141 lines of irregular nested structures where
a wrong answer still compiles.

Three traps carried across deliberately. Any data.content marker is a block
boundary, not just li - without that, 1-dan and transitive fuse into one
word. Whitespace collapsing covers the ideographic space U+3000 but must not
eat the newlines br produces. Part-of-speech nodes render as nothing and are
collected separately, ordered and de-duplicated, and a marker inside a
dropped example sentence is not a definition.

Verified against flatten.py on the fixture, not only by unit test."
```

---

## Task 3: The Yomitan archive reader

**Reference:** `tools/build-dict/yomitan.py` (61 lines),
`tools/build-dict/tests/test_yomitan.py`.

**Files:**
- Create: `src/dict/archive.rs`
- Modify: `src/dict/mod.rs`, `Cargo.toml`

**Interfaces:**
- Produces:
  ```rust
  pub struct TermEntry {
      pub term: String,
      pub reading: String,
      pub rules: String,
      pub glossary: serde_json::Value,
  }
  pub fn read_index(zip: &Path) -> Result<serde_json::Value>;
  pub fn iter_terms(zip: &Path) -> Result<Vec<TermEntry>>;
  pub fn iter_freq_rows(zip: &Path) -> Result<Vec<serde_json::Value>>;
  pub fn is_frequency_archive(zip: &Path) -> bool;
  ```

**Traps:**
- **Bank files sort numerically, not lexically** (`yomitan.py:26-34`) — so
  `term_bank_10.json` follows `term_bank_9.json`, not `term_bank_1.json`.
  Getting this wrong silently reorders entries and changes `entry_id`
  assignment.
- **Rows are ragged.** `yomitan.py:46-54` indexes defensively: a row may be
  shorter than seven elements and every field past the first has a default.
- `is_frequency_archive` mirrors `build.py:139` — `frequencyMode` present in
  `index.json`, **or** `Freq` in the filename.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, one line in the existing dependency list:

```toml
zip = { version = "8.6", default-features = false, features = ["deflate"] }
```

`default-features = false` matters: the defaults pull aes-crypto, bzip2, lzma,
ppmd, zstd and xz, none of which a Yomitan archive uses.

- [ ] **Step 2: Record the size cost**

```bash
cargo build --release
ls -l target/release/chibipop.exe | awk '{printf "%.2f MB\n", $5/1048576}'
```
Baseline before this task is **3.41 MB**; the budget is 100 MB. Report the
new figure — this is the project's first real dependency growth.

- [ ] **Step 3: Write the failing tests, using the checked-in fixture**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    #[test]
    fn the_index_carries_the_dictionary_title() {
        let idx = read_index(&fixture("terms.zip")).unwrap();
        assert_eq!(Some("FixtureTerms"), idx["title"].as_str());
    }

    #[test]
    fn every_term_row_is_read_with_its_rules_field() {
        let terms = iter_terms(&fixture("terms.zip")).unwrap();
        assert_eq!(3, terms.len());
        let taberu = terms.iter().find(|t| t.term == "食べる").expect("食べる present");
        assert_eq!("たべる", taberu.reading);
        assert_eq!("v1", taberu.rules);
    }

    #[test]
    fn a_frequency_archive_is_detected_and_a_term_archive_is_not() {
        assert!(is_frequency_archive(&fixture("freq.zip")));
        assert!(!is_frequency_archive(&fixture("terms.zip")));
    }

    #[test]
    fn frequency_rows_come_back_raw_for_the_parser() {
        let rows = iter_freq_rows(&fixture("freq.zip")).unwrap();
        assert_eq!(3, rows.len());
    }

    #[test]
    fn banks_sort_numerically_so_bank_10_follows_bank_9() {
        let mut names = vec![
            "term_bank_10.json".to_string(),
            "term_bank_2.json".to_string(),
            "term_bank_1.json".to_string(),
        ];
        sort_banks(&mut names, "term_bank_");
        assert_eq!(
            vec!["term_bank_1.json", "term_bank_2.json", "term_bank_10.json"],
            names
        );
    }
}
```

`sort_banks` is a private helper you will write; the test exercises it
directly because numeric ordering is invisible in a one-bank fixture and is
exactly the thing that breaks on a real dictionary.

- [ ] **Step 4: Implement, then gate and commit**

```bash
cargo test 2>&1 | grep -E "^test result" | awk '/ok\./ {s+=$4} END {print "TOTAL: " s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
git add Cargo.toml Cargo.lock src/dict/archive.rs src/dict/mod.rs
git commit -m "feat(dict): port the Yomitan archive reader

Adds the first real dependency this project has taken: zip 8.6, deflate only.
The default features pull aes-crypto, bzip2, lzma, ppmd, zstd and xz, none of
which a Yomitan archive uses.

Bank files sort numerically rather than lexically, so term_bank_10 follows
term_bank_9. Lexical order silently reorders entries and shifts every
entry_id, which no unit test on a single-bank fixture would notice."
```

---

## Task 4: The schema and the writer

**Reference:** `tools/build-dict/schema.py` (47 lines) and
`tools/build-dict/build.py` (158 lines).

**Files:**
- Create: `src/dict/build.rs`
- Modify: `src/dict/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct BuildCounts { pub entries: i64, pub terms: i64 }
  pub fn build(terms: &[PathBuf], freqs: &[PathBuf], out: &Path) -> Result<BuildCounts>;
  ```

**What must match `build.py` exactly:**
- The DDL and the `idx_term_surface` index (`schema.py:5-36`), and
  `meta.schema_version = 2` — `sqlite.rs:19` hard-fails on any other value.
- `dict_id` assigned by enumeration order from 1, `priority` from 0
  (`build.py:31,36`).
- `entry_id` a running counter across all dictionaries (`build.py:28,43`).
- **Two term rows when written differs from reading, one when it does not**
  (`build.py:56-63`). `written` is NULL for a kana-only headword.
- Senses stored as JSON with `glosses`, `pos`, `misc` (`build.py:46-48`).
- `meta.built_at` and `meta.source_hashes` with a SHA-256 per source
  (`build.py:81-103`).
- `ANALYZE` before closing (`build.py:75`).
- Entries whose glossary flattens to nothing are **skipped** (`build.py:41-42`).

- [ ] **Step 1: Write the failing tests**

Mirror `tools/build-dict/tests/test_fixture_archive.py` — the same eight
assertions, against a database this builder produces from the same fixture.
That file is the oracle's contract and yours must satisfy it identically:
3 entries, 5 term rows, `FixtureTerms` as the name, `食べる` with
`pos = "v1"` and `freq = 7`, `猫` with `freq = 42`, the kana-only `ねこ` with
`written IS NULL` and `freq IS NULL`, and `senses[0].pos == ["1-dan","transitive"]`.

- [ ] **Step 2: Implement and gate**

- [ ] **Step 3: Commit**

```bash
git add src/dict/build.rs src/dict/mod.rs
git commit -m "feat(dict): port the schema and the database writer

Completes the four pure modules. Deliberately identical to build.py where it
matters: schema_version 2 (sqlite.rs hard-fails on anything else), dict_id
and entry_id assignment, two term rows when written differs from reading and
one when it does not, and provenance in meta.

Verified against the same eight assertions the Python oracle satisfies on the
same fixture."
```

---

## Task 5: The `build-dict` subcommand, and the oracle diff

The step that proves the port. Everything before this is unit-tested; this
compares whole databases built from **the real archives**.

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `chibipop build-dict --library <dir> --out <file>`, printing one
  progress line per archive to stdout.

- [ ] **Step 1: Add the subcommand**

```rust
    /// Rebuild the dictionary database from a folder of Yomitan archives.
    BuildDict {
        /// Folder holding the .zip archives.
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
```

Classify each `.zip` with `archive::is_frequency_archive`, term archives in
filename order, then call `dict::build::build`. Print one line per archive as
it starts, and a final count — the parent process in Task 7 reads these.

- [ ] **Step 2: Build both databases from the fixture and compare**

```bash
cargo build --release
mkdir -p /tmp/oracle && cp tests/fixtures/yomitan/*.zip /tmp/oracle/
./target/release/chibipop.exe build-dict --library /tmp/oracle --out /tmp/rust.sqlite
(cd tools/build-dict && python build.py --dicts-dir /tmp/oracle --out /tmp/py.sqlite)

for t in dict entry term; do
  echo "--- $t ---"
  sqlite3 /tmp/py.sqlite   "SELECT * FROM $t ORDER BY rowid" > /tmp/py_$t.txt
  sqlite3 /tmp/rust.sqlite "SELECT * FROM $t ORDER BY rowid" > /tmp/rust_$t.txt
  diff /tmp/py_$t.txt /tmp/rust_$t.txt && echo "IDENTICAL"
done
```

All three tables must be **identical**. `meta` will differ — `built_at` is a
timestamp — so compare only `schema_version` there.

If `sqlite3` is unavailable, use `python -c` with the `sqlite3` module to dump
instead; do not skip the comparison.

- [ ] **Step 3: The real archives — they are on disk, so this is not optional**

`C:\Users\Stella\Documents\dicts\` holds the three archives the shipped
database was built from:

```
01 [JA-EN] jitendex-yomitan (2026-07-09).zip
[JA Freq] jiten_freq_global (2026-06-14).zip
[JA-JA] 大辞林　第四版.zip
```

The fixture is three entries; this is over a million term rows and it is where
a subtly wrong flattener or a mis-sorted bank actually shows. Build both and
compare the same three tables:

```bash
./target/release/chibipop.exe build-dict --library "/c/Users/Stella/Documents/dicts" --out /tmp/rust_full.sqlite
(cd tools/build-dict && python build.py --dicts-dir "C:\Users\Stella\Documents\dicts" --out /tmp/py_full.sqlite)
```

Report row counts from both, and diff `dict`, `entry` and `term`. On a set this
size, dump sorted and compare hashes rather than whole files.

**There is no `sqlite3` CLI on this machine.** Dump with Python's `sqlite3`
module instead, and set `PYTHONIOENCODING=utf-8` — the archive names contain
Japanese and a stock Windows console is cp1252, which raises
`UnicodeEncodeError` before anything useful prints.

- [ ] **Step 4: The golden corpus against a Rust-built database**

```bash
./target/release/chibipop.exe build-dict --library <real archives> --out /tmp/rust_full.sqlite
cp /tmp/rust_full.sqlite data/chibipop.sqlite   # back up the original first
cargo test --test golden
```
`tests/golden.rs`'s ten cases must pass. **Restore the original database
afterwards** and confirm its size.

- [ ] **Step 5: Commit**

---

## Task 6: The archive library

**Files:**
- Create: `src/library.rs`
- Modify: `src/lib.rs` (add `pub mod library;` — the list is alphabetical, so
  it goes **between `input` and `lookup`**: `libr` < `look`)

**Interfaces:**
```rust
pub enum Kind { Term, Frequency }
pub struct Entry { pub file: String, pub name: String, pub kind: Kind }
pub struct Library { pub entries: Vec<Entry> }
impl Library {
    pub fn load(dir: &Path) -> Result<Library>;
    pub fn save(&self, dir: &Path) -> Result<()>;
    pub fn import(&mut self, dir: &Path, source: &Path) -> Result<Entry>;
    pub fn remove(&mut self, dir: &Path, file: &str) -> Result<()>;
    pub fn term_paths(&self, dir: &Path) -> Vec<PathBuf>;
    pub fn freq_paths(&self, dir: &Path) -> Vec<PathBuf>;
}
```

`import` returns the `Entry` it created — Task 8 needs the resolved name and
kind to put a row in the right listbox without re-reading the archive.

**These are real functions now, not proposals.** Use them as they exist:
- `crate::dict::archive::read_index(zip) -> Result<Value>` — the title is
  `index["title"].as_str()`, verified at `archive.rs:19`.
- `crate::dict::archive::is_frequency_archive(zip) -> bool` — `archive.rs:56`.
  Note it takes a **path** and returns a plain `bool`, swallowing IO errors, so
  an unreadable file classifies as a term archive. Import must therefore call
  `read_index` **first** and fail there on a corrupt zip, before classifying.

**Behaviour that must hold:**
- `import` copies into `dir`, then classifies. **A name collision must not
  overwrite** — suffix the stem (`foo.zip` → `foo (2).zip`) rather than
  clobbering a dictionary the user already has.
- `save` writes `library.json` with the **write-then-rename** discipline at
  `config.rs:234-238`: write `library.json.tmp`, then `std::fs::rename`. Copy
  that shape exactly, including creating `dir` if absent.
- `load` on a missing folder or missing manifest returns an **empty library,
  not an error** — that is first run.
- `remove` deletes the file **and** the manifest entry. A missing file is not
  an error: the entry still goes.

> #### ⚠ The hazard the outline missed — read before implementing
>
> `data/chibipop.sqlite` on every existing install was built by the **Python**
> builder from archives in `~/Documents/dicts`, and `library/` does not exist.
> A rebuild is **from the library only**. So: empty library + populated
> database + user adds one archive + Apply ⇒ a database with **one**
> dictionary and three destroyed.
>
> The library layer cannot fix this alone; it just must not pretend the
> situation is normal. Provide:
>
> ```rust
> pub fn is_empty(&self) -> bool;
> ```
>
> and let Task 8 render the warning and Task 9 refuse the destructive path.
> **Do not silently seed the library from the database** — the archives may no
> longer be on disk, and inventing entries that point at missing files is
> worse than saying so.

- [ ] **Step 1: Write the failing tests**

Against a `tempfile`-style scratch directory (the crate already builds
temporary paths in `dict::build`'s tests — follow whatever that does rather
than adding a dev-dependency):

```rust
#[test] fn a_missing_directory_loads_as_an_empty_library() { … }
#[test] fn import_classifies_a_frequency_archive_by_its_index() { … }   // freq.zip → Kind::Frequency
#[test] fn import_classifies_a_term_archive() { … }                     // terms.zip → Kind::Term
#[test] fn import_reads_the_title_from_index_json() { … }               // terms.zip → "FixtureTerms"
#[test] fn importing_the_same_filename_twice_keeps_both() { … }         // second becomes "terms (2).zip"
#[test] fn a_corrupt_archive_fails_import_and_changes_nothing() { … }   // not a zip → Err, dir unchanged
#[test] fn remove_deletes_the_file_and_the_entry() { … }
#[test] fn remove_of_an_entry_whose_file_is_gone_still_removes_it() { … }
#[test] fn save_then_load_round_trips_order_and_kind() { … }
#[test] fn term_and_freq_paths_split_by_kind_in_manifest_order() { … }
```

The last one matters: `term_paths` feeds `dict::build::build`'s first argument
and **its order assigns `dict_id`**, so a wrong order silently renumbers every
dictionary.

- [ ] **Step 2: Implement, gate, commit**

```bash
cargo test 2>&1 | awk '/^test result: ok\./ {s+=$4} END {print "TOTAL:", s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
git add src/library.rs src/lib.rs
git commit -m "feat(library): the archive folder and its manifest"
```

---

## Task 7: Rebuild as a child process

**Files:**
- Create: `src/rebuild.rs`
- Modify: `src/lib.rs` (`pub mod rebuild;` sits **between `present` and
  `settings`**)

**Interfaces:**
```rust
pub enum Progress { Line(String), Done(PathBuf), Failed(String) }
pub fn spawn(library: &Path, out: &Path) -> Result<Receiver<Progress>>;
```

**Why a child process** (spec §5): the builder holds the whole frequency table
in memory, plausibly 50–80 MB, and that spike must not land in a process that
idles at 12 MB. It also means the parent never holds the database open while
it is rewritten, and a builder crash cannot take the popup down.

**The command line is fixed and already shipped** (`main.rs:353`):

```
<current_exe> build-dict --library <dir> --out <file>
```

It prints one line per archive as it starts and a final count. Forward those
verbatim as `Progress::Line`; do not parse them into a percentage, because the
subcommand emits no total.

**Three traps, each with its reason:**

1. **`CREATE_NO_WINDOW` (`0x0800_0000`) is required.** chibipop is
   console-subsystem. When it was double-clicked, `ui::console::hide()` hid
   *our* console — but a child spawned without that flag gets a **brand new
   visible console window**, so a rebuild would flash a black box at someone
   who has never seen a terminal. Set it via
   `std::os::windows::process::CommandExt::creation_flags`. Piping stdout is
   not sufficient on its own.
2. **Write to `<out>.tmp`, rename on success only.** A failed, crashed or
   killed build must leave the existing database byte-identical. Rename after
   the exit status is checked, never before.
3. **Drain stdout on a reader thread.** A pipe has a finite buffer; if the
   parent waits on exit before reading, a chatty child blocks writing and both
   deadlock. Read to end first, then `wait`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_fixture_library_builds_and_reports_done() { … }
    // Copy tests/fixtures/yomitan/*.zip into a temp dir, spawn, collect the
    // channel to exhaustion, assert the last message is Done and the file at
    // that path opens as a database with 3 entries.
#[test] fn progress_lines_arrive_before_done() { … }
#[test] fn a_failed_build_leaves_an_existing_output_byte_identical() { … }
    // Put known bytes at `out`, point --library at a dir holding a file named
    // *.zip that is not a zip, assert Failed and that `out` still hashes the
    // same and no `.tmp` is left behind.
#[test] fn an_empty_library_directory_is_a_failure_not_an_empty_database() { … }
```

That last case is the one that decides whether "Remove everything, Apply"
wipes the user's dictionary or refuses. **Refuse.**

- [ ] **Step 2: Implement, gate, commit**

---

## Task 8: The settings window's two dictionary groups

**Files:**
- Modify: `src/ui/settings_window.rs`, `src/settings.rs`, `Cargo.toml`

### The Cargo change (this does not compile without it)

`GetOpenFileNameW` lives in `windows::Win32::UI::Controls::Dialogs`, and
**`Win32_UI_Controls_Dialogs` is not in the feature list** (`Cargo.toml:21-41`).
Add it, keeping the list's existing order. Report the binary size after —
3,928,064 bytes (3.75 MB) is the figure to beat, budget 100 MB.

`GetOpenFileNameW` rather than `IFileOpenDialog` deliberately: the shell dialog
is COM and wants an STA, while this process establishes
`RoInitialize(RO_INIT_MULTITHREADED)` apartments per thread for OCR
(`app.rs:8-12`). The old dialog takes no COM at all and cannot conflict.

### The D9 trap, stated correctly

A file picker is **modal and pumps its own message loop**. `WM_TIMER` stops
arriving while it is open while the low-level hook keeps firing, so
`SCROLL_ARMED` latches and the wheel is captured **for every application on the
machine** until chibipop is killed.

`app.rs:703-711` is the existing solution and the exact thing to copy. It is
**two calls, not one**:

```rust
Hooks::set_scroll_armed(false);
drain_capture_guard();
```

`Hooks::set_scroll_armed` is `pub` at `hooks.rs:305`. `drain_capture_guard` is
private to `app.rs`, so the picker must either be invoked through a callback
supplied by `app.rs` (mirroring `tray.rs:237`'s `before_blocking: impl FnOnce()`)
or that helper made `pub(crate)`. **Prefer the callback** — it keeps the
window module free of app state, exactly as the tray does.

### The layout, read from the current file

Existing ids run **100–116** (`settings_window.rs:51-67`). New ones start at
117. Geometry constants: `WIN_W = 470`, `PAD = 14`, `ROW_H = 24`,
`ROW_GAP = 6` (`:71-77`).

The **Dictionaries** group today is `settings_window.rs:681-716`:
`dict_h = 5 * ROW_H + 34` = **154px**, a listbox `4 * ROW_H` tall with Move
up / Move down at `bx = WIN_W - PAD - 100`, then a static hint, then the
optional stale-entry warning.

Replace with two groups:

| Group | Controls | New ids |
|---|---|---|
| **Dictionaries** — topmost is shown first | existing listbox + Move up/down, **plus Add… and Remove** | `ID_DICT_ADD = 117`, `ID_DICT_REMOVE = 118` |
| **Frequency data** — how common each word is | listbox + Add… and Remove (no ordering; rank is a lookup, not a display order) | `ID_FREQS = 119`, `ID_FREQ_ADD = 120`, `ID_FREQ_REMOVE = 121` |

Four buttons will not fit beside a `4 * ROW_H` listbox at 92px each in the
`WIN_W - 2*PAD - 110` column that remains. **Shorten the Dictionaries listbox
to `3 * ROW_H` and stack four buttons at `ROW_H + 4` pitch**, or widen the
button column — either is fine, but recompute `dict_h` to match instead of
leaving the old constant, which would clip the last button.

Use `group_start` (not `group`) for **Frequency data**: `WS_GROUP` terminates
the preceding control group, the same reason `:605` uses it for Popup. Without
it, arrow keys walk out of one listbox into the next.

### The staged model

Extend `SettingsForm` (`settings.rs:23-35`, 11 fields today). Add:

```rust
pub freq_names: Vec<String>,
pub staged_adds: Vec<PathBuf>,
pub staged_removes: Vec<String>,
```

`dict_names` stays as-is so `from_config` and `apply_to` keep working
unchanged. Nothing touches `library/` until Apply.

### The data-loss warning belongs here

When `Library::is_empty()` but the loaded database has dictionaries, render a
static line in the Dictionaries group, in the style of the existing stale-entry
warning at `:707-715`:

> chibipop is using a dictionary built outside the app. Adding or removing
> here rebuilds from this list only — import your original .zip files first.

- [ ] **Step 1** Extend `SettingsForm` and test the **pure model only** (no
  window): staging an add then a remove of the same file is a no-op; a remove
  preserves the order of the rest; a staged add of an already-present filename
  is rejected rather than duplicated.
- [ ] **Step 2** Build the controls, and add `Win32_UI_Controls_Dialogs`.
  Multi-select via `OFN_ALLOWMULTISELECT | OFN_EXPLORER`, filter `*.zip`.
  **The multi-select return format is a trap**: with `OFN_EXPLORER` the buffer
  is a directory, then a NUL, then each filename, then a double NUL — and when
  exactly **one** file is picked it is instead a single full path. Handle both;
  a naive split yields a path that does not exist.
- [ ] **Step 3** **UNVERIFIABLE BY AGENT.** `build` returns the layout's `y`
  and `fit_to` sizes the window to it. Report the **measured** returned height
  before and after. The window is already tall; a settings window that does not
  fit a 1080px screen is a real failure, not cosmetic. Leave the visual check
  to a human and say plainly that it is unverified.
- [ ] **Step 4** Gate and commit, reporting the binary size.

---

## Task 9: Apply performs the rebuild

**Files:**
- Modify: `src/app.rs`, `src/settings.rs`, `README.md`, `docs/REFERENCE.md`

The existing Apply is `app.rs:635-651`: `apply_to` → `Config::save` →
`restart_self` → `PostQuitMessage`. Its comment at `:637-641` already states
the rule this task must not break — *a settings round-trip that half applies is
the one outcome worth refusing outright.*

New sequence, inserted **before** the config save:

1. If nothing is staged, behave exactly as today. A settings-only Apply must
   not trigger a multi-minute rebuild.
2. Apply staged adds/removes to the `Library`, `save` the manifest.
3. `rebuild::spawn` and pump `Progress` **on the main thread's existing message
   loop** — do not block it, or the window stops repainting and Windows paints
   the ghost-white "not responding" overlay over a build that is fine.
4. On `Done`: save the config, then `restart_self`.
5. On `Failed`: **leave the previous database in place**, keep the window open,
   report the failure. Do not save the config and do not restart — restarting
   into a half-applied state is precisely what `:637-641` forbids.

**Refuse the destructive case** identified in Task 6: if applying the staged
removals would leave the library empty while a database exists, refuse with a
message rather than building an empty dictionary.

- [ ] **Step 1** Wire it, with a progress line in the settings window.
- [ ] **Step 2** Gate. **UNVERIFIABLE BY AGENT** — the full flow needs a human
  with real archives; say so rather than implying it was exercised.
- [ ] **Step 3** Docs. [`README.md`](../../README.md) §"Setting it up" loses
  its Python step — importing dictionaries becomes something you do in the
  settings window. `docs/REFERENCE.md` gains `build-dict` in the subcommand
  table.
- [ ] **Step 4** Commit.

---

## Acceptance

**Tier 0** throughout, unchanged.

**Phase 2 is accepted when** the Rust builder produces byte-identical `dict`,
`entry` and `term` tables to the Python builder on the fixture, and
`tests/golden.rs` passes against a Rust-built database made from the real
archives.

**Phase 3 needs a human:** import two term archives and one frequency archive
into a clean library, Apply, confirm the rebuild completes and lookups reflect
all three; remove one, Apply, confirm it is gone and the others still rank
correctly.

**Budget:** binary under 100 MB — report it after Task 3, which is where the
only dependency growth happens.

## What this plan does not do

It does not delete `tools/build-dict/`. The Python builder stays as the oracle
until the Rust one has been run against real archives by a human, and removing
it is a separate decision.

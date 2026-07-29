# Double-click and the oracle harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `chibipop.exe` something a person can double-click, and check in a
shared Yomitan fixture both the Python builder and the future Rust builder can
read.

**Architecture:** Two independent slices from
[the design](../specs/2026-07-29-dictionary-management-design.md). Phase 0 is a
checked-in fixture archive plus a test pinning what the Python builder makes of
it — the oracle the Phase 2 port will be diffed against. Phase 1 switches the
binary to the windows subsystem, re-attaches to a parent console so the
diagnostic subcommands still print, makes no-arguments mean `run`, stops a
missing dictionary from being a fatal invisible error, and adds a live lookup
log.

**Tech Stack:** Rust (stable, MSVC), `windows` crate, clap, Python 3 (fixture
generation only).

**Covers:** design §6 and §8 phases 0–1. Phases 2 (`archive`/`glossary`/
`frequency`/`build`) and 3 (library store and settings UI) get their own plans.

## Global Constraints

- **Binary must stay under 100 MB.** Baseline 3.5 MB.
- **Idle memory and CPU must not regress.** Baseline 12 MB working set / 2.6 MB
  private / 0.000% CPU.
- **Windows only.** `windows-latest`; the crate does not build elsewhere.
- **Comments are under 30 characters** and only where the code cannot be made
  obvious. Exempt: `// SAFETY:` blocks. This is a house rule, not upstream Rust
  style.
- **`cargo fmt` is never run.** The repo has never been rustfmt-clean; see
  `docs/REGRESSION.md`.
- **Tier 0 must stay green:** all tests pass, clippy accepted-error count is
  **exactly 5**, bin-target clippy is **0**, release build succeeds.
- **Stage by name. Never `git add -u` or `git add -A`.**
- Three unsafe lints are enabled at both crate roots. Every unsafe operation
  goes inside a visible `unsafe` block.

> ### ⚠️ `Cargo.toml` carries an uncommitted reformat
>
> The working tree has had an unstaged whitespace reformat of the `windows`
> feature list for some time, deliberately. **Task 3 must edit `Cargo.toml`**,
> and `git add Cargo.toml` will sweep that reformat in.
>
> **Before starting Task 3, commit the reformat on its own:**
>
> ```bash
> git add Cargo.toml
> git commit -m "style(cargo): one windows feature per line"
> ```
>
> Do not skip this. Mixing it into a feature commit makes that commit
> unreviewable.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `tests/fixtures/yomitan/make_fixtures.py` | Regenerates the fixture archives | 1 |
| `tests/fixtures/yomitan/terms.zip` | Two-entry term archive, checked in | 1 |
| `tests/fixtures/yomitan/freq.zip` | One-row frequency archive, checked in | 1 |
| `tools/build-dict/tests/test_fixture_archive.py` | Pins the Python builder's output for the fixture | 1 |
| `src/paths.rs` | Resolve data files beside the executable | 2 |
| `src/main.rs` | Subsystem attribute, console attach, default subcommand, path defaults | 2,3,4 |
| `src/app.rs` | Missing-dictionary path opens settings instead of failing | 5 |
| `src/ui/console.rs` | `AllocConsole`, control handler, live lookup log | 6 |
| `src/config.rs` | `show_lookup_log` setting | 6 |
| `Cargo.toml` | `Win32_System_Console` feature | 3 |

---

## Task 1: Shared fixture archive and its oracle test

The Python builder is already tested against archives built in memory
(`tools/build-dict/tests/test_build.py`). What does not exist is a **checked-in**
archive that a second implementation can read. Phase 2 needs one to diff
against.

**Files:**
- Create: `tests/fixtures/yomitan/make_fixtures.py`
- Create: `tests/fixtures/yomitan/terms.zip` (generated, committed)
- Create: `tests/fixtures/yomitan/freq.zip` (generated, committed)
- Create: `tools/build-dict/tests/test_fixture_archive.py`

**Interfaces:**
- Produces: two archives at fixed paths, and a pinned expectation of the
  database built from them — `3` entries, `5` term rows, `食べる` with
  `pos = "v1"` and `freq = 7`, `猫` with `freq = 42` via the reading-scoped
  row, and the kana-only `ねこ` with `freq IS NULL`.

- [ ] **Step 1: Write the fixture generator**

Create `tests/fixtures/yomitan/make_fixtures.py`:

```python
"""Regenerate the checked-in Yomitan fixture archives.

Deliberately tiny and deliberately committed: Phase 2's Rust builder is
verified by producing the same database from these exact bytes that the
Python builder does. Run from the repository root:

    python tests/fixtures/yomitan/make_fixtures.py
"""

import json
import zipfile
from pathlib import Path

HERE = Path(__file__).parent

TERMS_INDEX = {"title": "FixtureTerms", "format": 3, "revision": "1"}

# Covers the shapes that matter: structured content carrying part-of-speech
# spans, a plain string glossary, a kana-only headword, and a kanji spelling
# sharing its reading with that headword - which is what makes the
# reading-scoped frequency row below meaningful rather than decorative.
TERM_BANK = [
    ["食べる", "たべる", "", "v1", 0,
     [{"type": "structured-content", "content": [
         {"tag": "span", "data": {"content": "part-of-speech-info"},
          "content": "1-dan"},
         {"tag": "span", "data": {"content": "part-of-speech-info"},
          "content": "transitive"},
         {"tag": "span", "content": "to eat"},
     ]}]],
    ["ねこ", "ねこ", "", "", 0, ["cat"]],
    ["猫", "ねこ", "", "", 0, ["cat (kanji)"]],
]

FREQ_INDEX = {"title": "FixtureFreq", "format": 3, "frequencyMode": "rank-based"}

# Both row shapes: reading-agnostic, and reading-scoped with the extra nesting.
FREQ_BANK = [
    ["食べる", "freq", {"value": 7}],
    ["猫", "freq", {"reading": "ねこ", "frequency": {"value": 42}}],
]


def write(path, index, banks):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))


if __name__ == "__main__":
    write(HERE / "terms.zip", TERMS_INDEX, {"term_bank_1.json": TERM_BANK})
    write(HERE / "freq.zip", FREQ_INDEX, {"term_meta_bank_1.json": FREQ_BANK})
    print(f"wrote {HERE / 'terms.zip'} and {HERE / 'freq.zip'}")
```

- [ ] **Step 2: Generate the archives**

Run:
```bash
python tests/fixtures/yomitan/make_fixtures.py
```
Expected: `wrote .../terms.zip and .../freq.zip`, and both files exist.

- [ ] **Step 3: Write the failing oracle test**

Create `tools/build-dict/tests/test_fixture_archive.py`:

```python
"""Pins what the Python builder produces from the checked-in fixture.

Phase 2's Rust builder must reproduce exactly this. If this test and the Rust
equivalent ever disagree, one of the two readers is wrong and this file says
which answer was correct first.
"""

import json
import sqlite3
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from build import build

REPO = Path(__file__).resolve().parents[3]
FIXTURES = REPO / "tests" / "fixtures" / "yomitan"


class TestFixtureArchive(unittest.TestCase):
    def setUp(self):
        self.out = Path(__file__).parent / "_fixture_out.sqlite"
        build([(FIXTURES / "terms.zip", 0)], [FIXTURES / "freq.zip"], self.out)
        self.conn = sqlite3.connect(self.out)

    def tearDown(self):
        self.conn.close()
        self.out.unlink(missing_ok=True)

    def test_entry_and_term_counts(self):
        entries = self.conn.execute("SELECT COUNT(*) FROM entry").fetchone()[0]
        terms = self.conn.execute("SELECT COUNT(*) FROM term").fetchone()[0]
        self.assertEqual(3, entries)
        # 食べる and 猫 index under two surfaces each; ねこ under one.
        self.assertEqual(5, terms)

    def test_dictionary_name_comes_from_the_index(self):
        name = self.conn.execute("SELECT name FROM dict").fetchone()[0]
        self.assertEqual("FixtureTerms", name)

    def test_structured_content_flattens_to_one_gloss(self):
        row = self.conn.execute(
            "SELECT senses FROM entry JOIN term USING(entry_id) "
            "WHERE surface='食べる'").fetchone()[0]
        self.assertEqual(["to eat"], json.loads(row)[0]["glosses"])

    def test_part_of_speech_spans_are_separated_from_glosses(self):
        row = self.conn.execute(
            "SELECT senses FROM entry JOIN term USING(entry_id) "
            "WHERE surface='食べる'").fetchone()[0]
        self.assertEqual(["1-dan", "transitive"], json.loads(row)[0]["pos"])

    def test_kana_only_headword_has_a_null_written_column(self):
        # 猫 also indexes under surface 'ねこ', so written IS NULL is what
        # distinguishes the kana-only entry rather than the row count.
        n = self.conn.execute(
            "SELECT COUNT(*) FROM term "
            "WHERE surface='ねこ' AND written IS NULL").fetchone()[0]
        self.assertEqual(1, n)

    def test_reading_agnostic_frequency_is_applied(self):
        f = self.conn.execute(
            "SELECT freq FROM term WHERE surface='食べる'").fetchone()[0]
        self.assertEqual(7, f)

    def test_reading_scoped_frequency_is_applied(self):
        # The trap freq.py's docstring names: the nested {"reading":...,
        # "frequency":{"value":...}} shape. If it is parsed wrongly the row is
        # dropped and this comes back None instead of 42.
        f = self.conn.execute(
            "SELECT freq FROM term WHERE surface='猫'").fetchone()[0]
        self.assertEqual(42, f)

    def test_a_term_with_no_frequency_row_is_null(self):
        f = self.conn.execute(
            "SELECT freq FROM term "
            "WHERE surface='ねこ' AND written IS NULL").fetchone()[0]
        self.assertIsNone(f)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 4: Run it and watch it pass**

Run:
```bash
cd tools/build-dict && python -m unittest discover -s tests
```
Expected: `OK`, and the count rises from 48 to **56**.

If `test_entry_and_term_counts` fails on the term count, read the actual value
before changing the assertion — `食べる` and `猫` produce two rows each (written
and reading) and `ねこ` produces one, so 5 is the intended answer.

- [ ] **Step 5: Update the documented Python test count**

In `docs/REFERENCE.md`, change `**48 tests.**` to `**56 tests.**`.
In `docs/RELEASING.md`, change `48 Python tests` to `56 Python tests`.

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures/yomitan/make_fixtures.py tests/fixtures/yomitan/terms.zip tests/fixtures/yomitan/freq.zip tools/build-dict/tests/test_fixture_archive.py docs/REFERENCE.md docs/RELEASING.md
git commit -m "test: a checked-in Yomitan fixture, and what the builder makes of it

Phase 2 replaces build.py with Rust. The safest way to verify a rewrite of
working code is to diff it against the code that works, which needs an input
both can read - test_build.py builds its archives in memory, so there was
nothing on disk to share.

Two archives, a few hundred bytes each, covering the shapes that actually
differ: structured content with part-of-speech spans, a plain string glossary,
a kana-only headword, and both frequency row nestings."
```

---

## Task 2: Resolve data files beside the executable

`--config` already defaults beside the executable, and its comment gives the
reason — *"so a shortcut-launched chibipop.exe still finds its settings"*.
`--dict` and `--rules` default relative to the working directory, so that
reasoning is currently not honoured by either. Double-click is what turns this
from a quirk into a defect.

**Files:**
- Create: `src/paths.rs`
- Modify: `src/lib.rs` (add `pub mod paths;`)
- Modify: `src/main.rs` (the three `default_value` attributes and `main`)

**Interfaces:**
- Produces: `chibipop::paths::beside_exe(relative: &str) -> PathBuf`, used by
  Task 5 and by Phase 3's library store.

- [ ] **Step 1: Write the failing test**

Create `src/paths.rs`:

```rust
//! Where chibipop's own files live.

use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_name_lands_beside_the_executable() {
        let got = beside_exe("data/chibipop.sqlite");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(exe.parent().unwrap(), got.parent().unwrap().parent().unwrap());
        assert!(got.ends_with("data/chibipop.sqlite"));
    }

    #[test]
    fn a_bare_name_lands_directly_beside_it() {
        let got = beside_exe("chibipop.toml");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(exe.parent().unwrap(), got.parent().unwrap());
    }
}
```

- [ ] **Step 2: Run it and verify it fails**

Run:
```bash
cargo test --lib paths
```
Expected: FAIL — `cannot find function 'beside_exe'`.

- [ ] **Step 3: Write the implementation**

Add above the test module in `src/paths.rs`:

```rust
/// Resolved beside the exe.
pub fn beside_exe(relative: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(relative)))
        .unwrap_or_else(|| PathBuf::from(relative))
}
```

Add to `src/lib.rs`, keeping the modules alphabetical:

```rust
pub mod paths;
```

- [ ] **Step 4: Run the tests**

Run:
```bash
cargo test --lib paths
```
Expected: PASS, 2 tests.

- [ ] **Step 5: Use it for the CLI defaults**

In `src/main.rs`, every `dict` and `rules` argument currently reads:

```rust
        #[arg(long, default_value = "data/chibipop.sqlite")]
        dict: PathBuf,
        #[arg(long, default_value = "data/deconjugator.json")]
        rules: PathBuf,
```

Change **each of the four occurrences** (`Lookup`, `Probe`, `Settings` has only
`dict`, `Watch`, `Run`) to:

```rust
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
```

Then add these helpers to `src/main.rs`, beside `default_config_path`:

```rust
/// `--dict`, or the default beside the exe.
fn dict_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| chibipop::paths::beside_exe("data/chibipop.sqlite"))
}

/// `--rules`, or the default beside the exe.
fn rules_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| chibipop::paths::beside_exe("data/deconjugator.json"))
}
```

And replace `default_config_path`'s body so all three share one rule:

```rust
fn default_config_path() -> PathBuf {
    chibipop::paths::beside_exe("chibipop.toml")
}
```

In each match arm, bind the resolved values first. For example in
`Command::Lookup`:

```rust
        Command::Lookup { text, dict, rules } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
```

Do the same in `Probe`, `Watch`, `Run`, and `Settings` (which takes `dict`
only).

- [ ] **Step 6: Verify the gate**

Run:
```bash
cargo test 2>&1 | grep -E "^test result"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
```
Expected: all green, and the clippy count is still **5**.

- [ ] **Step 7: Verify it against reality**

Run from a directory that is **not** the repository root:
```bash
cd /c && ~/chibipop/target/release/chibipop.exe lookup 食べた
```

Build first if needed (`cargo build --release`). Expected: a real result. Before
this task it fails, because `data/chibipop.sqlite` does not exist relative to
`C:\`.

- [ ] **Step 8: Commit**

```bash
git add src/paths.rs src/lib.rs src/main.rs
git commit -m "fix(paths): resolve the dictionary and rules beside the exe

--config already did this, and its comment gives the reason: 'so a
shortcut-launched chibipop.exe still finds its settings'. That argument
applies verbatim to --dict and --rules, and neither honoured it - they
resolved against the working directory, so chibipop only worked when launched
from its own folder.

The README papered over it with 'run from this folder'. A double-clicked
shortcut has no such guarantee, and its working directory is frequently
C:\\Windows\\System32."
```

---

## Task 3: Hide the console when we own it alone

> **This task replaces an earlier version that was tried and reverted.**
> That version set `#![windows_subsystem = "windows"]` and called
> `AttachConsole(ATTACH_PARENT_PROCESS)`. Attaching a console does **not**
> rebind the standard handles, so a process launched without redirection had
> an unusable stdout. Measured on this machine: console subsystem gave a
> correct 647-byte redirect; GUI + `AttachConsole` gave 0 bytes; GUI +
> conditional `SetStdHandle` gave 0 bytes **and a panic**, in PowerShell and
> cmd alike, because `println!` aborts the process when a write fails.
> Commits `79b9c61` and `9948b0d`, reverted by `9d5a582`.

Stay on the console subsystem — everything that works today keeps working —
and hide the console window when it belongs to us alone.

**Hide, never free.** `FreeConsole` would invalidate stdout, and `app::run`
prints on startup, so freeing reintroduces exactly the panic the reverted
approach died of. A hidden console keeps every handle valid: output goes into
a window nobody sees, and redirection is untouched.

**Files:**
- Modify: `Cargo.toml` (one feature)
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `hide_own_console()`, called first in `main`.

- [ ] **Step 1: Add the console feature**

In `Cargo.toml`, add `"Win32_System_Console",` to the `windows` feature list,
one feature per line as the rest of the list already is.

- [ ] **Step 2: Write the function**

Add to `src/main.rs`:

```rust
/// Hides a console only we hold.
///
/// A double-click gets a console with this process alone attached. Launched
/// from a shell, the shell is attached too and the window is not ours to
/// touch. Hidden rather than freed: freeing invalidates stdout, and
/// `println!` aborts the process when a write fails.
fn hide_own_console() {
    use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    // SAFETY: GetConsoleProcessList writes at most `pids.len()` entries into
    // the buffer and returns the true count, which may exceed it - we only
    // compare against 1, so a truncated write cannot mislead us.
    // GetConsoleWindow returns null when no console exists, which the
    // is_invalid check covers.
    unsafe {
        let mut pids = [0u32; 4];
        if GetConsoleProcessList(&mut pids) != 1 {
            return;
        }
        let hwnd = GetConsoleWindow();
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}
```

- [ ] **Step 3: Call it first**

```rust
fn main() -> Result<()> {
    hide_own_console();
    let cli = Cli::parse();
```

- [ ] **Step 4: Verify redirection is untouched in every shell**

All four must produce real output. This is the regression the reverted
approach caused, so it is the point of the task.

```bash
./target/release/chibipop.exe lookup 食べた | head -3
./target/release/chibipop.exe lookup 食べた > b.txt && wc -c < b.txt && rm b.txt
powershell -NoProfile -Command "& '.\target\release\chibipop.exe' lookup 食べた"
cmd //c "target\release\chibipop.exe lookup 食べた > c.txt" && wc -c < c.txt && rm c.txt
```

- [ ] **Step 5: Gate**

```bash
cargo test 2>&1 | grep -E "^test result" | awk '/ok\./ {s+=$4} END {print "TOTAL: " s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
```
Expect TOTAL 248 and exactly 5.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat(cli): hide the console when it is ours alone

A console-subsystem binary flashes a console on double-click. Switching to
the windows subsystem removes it and takes stdout with it - tried, measured,
reverted.

This keeps the subsystem and hides the window instead, only when
GetConsoleProcessList reports this process as the sole attachment. A shell
launch has the shell attached too and is left alone, so redirection and every
diagnostic behave exactly as before.

Hidden rather than freed on purpose: FreeConsole invalidates stdout, app::run
prints on startup, and println! aborts the process when a write fails."
```

---

## Task 3 (SUPERSEDED): Windows subsystem, with the console re-attached

**Files:**
- Modify: `Cargo.toml` (add one feature)
- Modify: `src/main.rs` (crate attribute and `main`)

**Read the Cargo.toml warning in Global Constraints before this task.**

**Interfaces:**
- Produces: no console on double-click; `println!` still reaches a terminal that
  launched the process.

- [ ] **Step 1: Commit the pending Cargo.toml reformat on its own**

```bash
git add Cargo.toml
git commit -m "style(cargo): one windows feature per line"
```

- [ ] **Step 2: Add the console feature**

In `Cargo.toml`, add `"Win32_System_Console",` to the `windows` feature list,
keeping one feature per line.

- [ ] **Step 3: Add the crate attribute and the attach**

At the very top of `src/main.rs`, **above** the existing `#![warn(...)]` lines:

```rust
// No console on double-click.
#![windows_subsystem = "windows"]
```

Then add this function to `src/main.rs`:

```rust
/// Reattach a parent console.
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: FFI call with no preconditions. It fails harmlessly when the
    // parent has no console, which is every double-click.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
```

Call it as the first line of `main`:

```rust
fn main() -> Result<()> {
    attach_parent_console();
    let cli = Cli::parse();
```

- [ ] **Step 4: Verify the gate**

Run:
```bash
cargo build --release 2>&1 | grep -E "^error|Finished"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
```
Expected: `Finished`, clippy count still **5**.

- [ ] **Step 5: Verify output still reaches a terminal**

Run:
```bash
./target/release/chibipop.exe lookup 食べた
```
Expected: the ranked results still print. If nothing prints, `AttachConsole` is
not being reached — check it runs before `Cli::parse()`.

- [ ] **Step 6: Check the binary budget**

Run:
```bash
ls -l target/release/chibipop.exe | awk '{printf "%.2f MB\n", $5/1048576}'
```
Expected: still around 3.5 MB, far under the 100 MB ceiling.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat(cli): windows subsystem, with the parent console reattached

A console-subsystem binary flashes a console window on every double-click,
which is the opposite of what someone unfamiliar with a terminal should see.
Switching subsystems fixes that and costs the diagnostics their output -
unless the process reattaches to the console that launched it, which is what
ATTACH_PARENT_PROCESS is for.

Known wrinkle, not a defect: the shell returns its prompt before the output
arrives, so output appears after the prompt."
```

---

## Task 4: No arguments means `run`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `dict_path`/`rules_path` from Task 2.

- [ ] **Step 1: Make the subcommand optional**

In `src/main.rs`, change the `Cli` struct:

```rust
#[derive(Parser)]
#[command(name = "chibipop", about = "Japanese lookup engine")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
```

- [ ] **Step 2: Default to `Run` when absent**

In `main`, replace `match cli.command {` with:

```rust
    // A double-click passes none.
    let command = cli.command.unwrap_or(Command::Run {
        dict: None,
        rules: None,
        config: None,
    });
    match command {
```

- [ ] **Step 3: Verify**

Run:
```bash
cargo build --release && ./target/release/chibipop.exe --help
```
Expected: help still lists every subcommand, and the usage line now shows the
subcommand as optional.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): no arguments means run

A double-click passes no arguments. Before this it printed a usage error into
a console that, since the subsystem change, nobody can see."
```

---

## Task 5: A missing dictionary opens settings instead of failing

Under the windows subsystem, `run`'s current hard failure is an error message
nobody can read. First run is exactly when there is no dictionary.

**Files:**
- Modify: `src/app.rs` (`run`)

- [ ] **Step 1: Check before spawning the worker**

In `src/app.rs`, at the top of `run`, before the worker thread is spawned:

```rust
    // No dictionary on first run.
    if !dict_path.exists() {
        return settings_only(cfg, &[], config_path);
    }
```

`settings_only` already exists, takes an empty dictionary list without
complaint, and returns `Ok(())` on Cancel — so this opens the settings window
with no popup, no hooks and no OCR, and exits cleanly when dismissed.

- [ ] **Step 2: Verify**

Run:
```bash
cargo build --release
mv data/chibipop.sqlite data/chibipop.sqlite.bak
./target/release/chibipop.exe run
```
Expected: the settings window opens; no crash, no error. Close it, then:
```bash
mv data/chibipop.sqlite.bak data/chibipop.sqlite
```

**This step needs a human — it opens a window.** Put it on the portrait
secondary (x ≥ 2560).

- [ ] **Step 3: Commit**

```bash
git add src/app.rs
git commit -m "fix(app): a missing dictionary opens settings, it does not kill startup

Under the windows subsystem the previous hard failure was an error message
with nowhere to appear. First run is precisely when there is no dictionary,
so the first thing a new user would have seen was nothing at all.

Phase 3 upgrades this window to offer an import. Until then it is the honest
place to land: it is where the dictionary settings already are."
```

---

## Task 6: The live lookup log

A console the user can open from settings, printing each resolved hover. It
answers *"why isn't it reading this?"* without a command line.

**Files:**
- Create: `src/ui/console.rs`
- Modify: `src/ui/mod.rs`
- Modify: `src/config.rs` (one setting)

**Interfaces:**
- Produces: `chibipop::ui::console::show()` and `hide()`.

- [ ] **Step 1: Write the module**

Create `src/ui/console.rs`:

```rust
//! The live lookup log console.

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Console::{
    AllocConsole, FreeConsole, GetConsoleWindow, SetConsoleCtrlHandler,
};
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOW};

static ALLOCATED: AtomicBool = AtomicBool::new(false);

/// Close must hide, not exit.
unsafe extern "system" fn ctrl_handler(_event: u32) -> BOOL {
    hide();
    BOOL(1)
}

/// Shows it, allocating once.
pub fn show() {
    // SAFETY: all four calls are console FFI with no preconditions beyond
    // being called from a process that may allocate a console, which any
    // windows-subsystem process may.
    unsafe {
        if !ALLOCATED.swap(true, Ordering::SeqCst) {
            if AllocConsole().is_err() {
                ALLOCATED.store(false, Ordering::SeqCst);
                return;
            }
            let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
        }
        let hwnd = GetConsoleWindow();
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
}

/// Hides without freeing.
pub fn hide() {
    // SAFETY: GetConsoleWindow returns null when no console exists, which the
    // invalid check covers.
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Frees it at shutdown.
pub fn release() {
    // SAFETY: harmless when no console was ever allocated.
    unsafe {
        if ALLOCATED.swap(false, Ordering::SeqCst) {
            let _ = FreeConsole();
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `src/ui/mod.rs`, add `pub mod console;` keeping the list alphabetical.

- [ ] **Step 3: Add the setting**

In `src/config.rs`, add to `DebugConfig`:

```rust
    /// A console of each hover.
    #[serde(default)]
    pub show_lookup_log: bool,
```

`DebugConfig` derives `Default`, and the field-level `#[serde(default)]` means
a config written before this field existed still loads.

- [ ] **Step 4: Write the failing test**

Add to `src/config.rs`'s test module:

```rust
    /// A pre-existing config must still load.
    #[test]
    fn a_config_written_before_the_lookup_log_still_loads() {
        let p = tmp("no_lookup_log");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[debug]\nshow_scan_region = false\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-lookup-log config must load");
        assert!(!c.debug.show_lookup_log);
        let _ = std::fs::remove_file(&p);
    }
```

- [ ] **Step 5: Run the tests**

Run:
```bash
cargo test 2>&1 | grep -E "^test result"
```
Expected: all green. Total passing rises from 245 to **248** (two from Task 2,
one here).

- [ ] **Step 6: Wire it into startup**

In `src/app.rs`'s `run`, after the theme is built:

```rust
    if cfg.debug.show_lookup_log {
        crate::ui::console::show();
    }
```

And in the shutdown sequence, after `drop(tray);`:

```rust
    crate::ui::console::release();
```

In `handle_worker_outcome`'s `WorkerOutcome::Ready` arm, print the resolved
hover. **Placement matters:** it goes immediately *after* the `same_content`
early return, not before it. Before it, the log repeats the same line every
tick while the cursor holds still on one word — which is the case the
`same_content` check exists to suppress.

```rust
            if shown.as_ref().is_some_and(|prev| same_content(prev, &presentation, anchor)) {
                return; // Already on screen, unchanged.
            }
            // Only changed popups.
            if let Some(card) = &presentation.top {
                let head = card.written.clone()
                    .or_else(|| card.reading.clone())
                    .unwrap_or_default();
                println!("{head}  match={}", card.match_len);
            }
```

- [ ] **Step 7: Verify the gate, then reality**

Run:
```bash
cargo test 2>&1 | grep -E "^test result"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
cargo clippy --all-targets --all-features -- -D warnings -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -cE "^(error|warning)"
cargo build --release 2>&1 | grep -E "^error|Finished"
```
Expected: green, **5**, **0**, `Finished`.

Then, **with a human present**, set `show_lookup_log = true` in
`chibipop.toml`, run `chibipop.exe run`, hover some Japanese, and confirm lines
appear in the console. **Then close the console window and confirm chibipop is
still running** — that is the whole point of `ctrl_handler`, and nothing
automated can check it.

- [ ] **Step 8: Update the documented test count**

In `docs/REGRESSION.md` and `docs/REFERENCE.md`, change 245 to **248**.

- [ ] **Step 9: Document the setting**

In `docs/REFERENCE.md`'s `[debug]` block, add:

```toml
show_lookup_log = false    # a console printing each resolved hover
```

In `README.md`'s settings table, add a row:

| **Show the lookup log** | Opens a small text window listing each word chibipop reads, so you can see what it's doing. |

- [ ] **Step 10: Commit**

```bash
git add src/ui/console.rs src/ui/mod.rs src/config.rs src/app.rs docs/REGRESSION.md docs/REFERENCE.md README.md
git commit -m "feat(ui): an optional console carrying the live lookup log

Answers 'why isn't it reading this?' without a command line - which matters
more now that the windows subsystem means there is no console by default.

The trap this is written around: closing an allocated console terminates the
process. Windows sends CTRL_CLOSE_EVENT and the default handler exits, so
closing the log window would have taken chibipop with it. The handler claims
the event and hides the window instead."
```

---

## Acceptance

**Tier 0**, unchanged: all tests pass (248), clippy accepted-error count is
exactly 5, bin-target clippy is 0, release build succeeds, Python builder tests
pass (56).

**Tier 2 — needs a human**, and none of it can be automated:

1. Create a desktop shortcut to `chibipop.exe`, launch it from there, and
   confirm **no console window appears** and hovering works.
2. Run `chibipop.exe probe --at <x>,<y>` from a terminal and confirm output
   still prints.
3. Rename the dictionary away, run `chibipop.exe` with no arguments, and
   confirm the **settings window opens** rather than nothing happening.
4. Enable `show_lookup_log`, hover, confirm lines appear — then **close the
   console and confirm chibipop survives**.

**Budget:** binary under 100 MB (expect ~3.5 MB), idle memory and CPU unchanged.

---

## What this plan does not cover

Phases 2 and 3 of the design — the Rust builder port and the library store with
its settings interface. Each gets its own plan, written once this one has
landed and the codebase state is known rather than predicted.

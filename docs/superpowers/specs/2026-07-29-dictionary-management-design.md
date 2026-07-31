# Dictionary management, and a chibipop you can double-click — design

**Date:** 2026-07-29
**Status:** approved, not built
**Supersedes nothing.** Extends the settings-GUI round (2026-07-28).

---

## 1. Goal

One executable a person can remember, double-click, and use — with their own
dictionaries added and removed from inside the application, so chibipop never
has to ship a dictionary it has no right to redistribute.

Today the path from "downloaded chibipop" to "hovering Japanese" runs through
installing Python, sourcing Yomitan archives, and a command line. This round
removes everything except sourcing the archives, which is a choice only the
user can make.

### Constraints, with today's measured baselines

| Constraint | Baseline | Budget |
|---|---|---|
| Binary size | 3.5 MB | **< 100 MB** |
| Idle memory | 12 MB working set / 2.6 MB private | no regression |
| Idle CPU | 0.000% | no regression |
| Hover latency | 91 ms per `probe` | no regression |

Nothing in this round touches the hover path, so the last three are protected
by construction rather than by care. The binary grows by one zip reader —
deflate only, pure Rust — estimated +300 KB.

---

## 2. The decision that removes the hard problems

**The database stops being something you build and becomes something chibipop
derives.**

chibipop keeps a library of archives. The `.sqlite` is a cache generated from
that library, and can be deleted at any time without losing anything. Import
copies an archive in and regenerates. Delete removes it and regenerates.

This one choice dissolves three problems that incremental editing would have
created, all of which come from `build.py` assuming a single full build:

- **`entry_id` is a running counter** (`build.py:28,43`), so appending a
  dictionary needs a watermark instead.
- **`dict_id` doubles as priority.** `engine.rs:210` sorts on it with a
  comment saying so. Importing out of order breaks that. (`dict.priority`
  already exists in the schema and Rust never reads it — so this had an easy
  fix, but now needs no fix at all.)
- **Frequency is joined at build time and discarded.** `build.py:54` bakes
  `lookup_freq(...)` into `term.freq`; the frequency table is never stored.
  Adding a frequency archive would mean rewriting ~1.26 M rows, and **deleting
  one is not possible at all** — the previous ranks are gone and nothing on
  disk can reconstruct them.

Regenerating makes all three moot. The schema does not change. There is no
migration. The builder port never needs incremental logic, which is a
significant reduction in the riskiest part of this round.

**Cost:** roughly 70 s per change, and disk usage roughly doubles (archives
plus the derived database). Both are accepted.

**Rejected:** *storing paths instead of copying archives.* Saves the disk, but
moving or deleting a source file silently drops a dictionary at the next
rebuild — a bad failure for exactly the person this round is for.

---

## 3. Layout

```
chibipop/                     the folder the zip unpacks to
  chibipop.exe                the one file to remember
  chibipop.toml               written with defaults on first run
  library/                    NEW
    <archive>.zip             copied in on import
    library.json              kind, display name, order
  data/
    chibipop.sqlite           derived; deletable; regenerated from library/
```

Everything resolves **beside the executable**. `--config` already does this,
and its own comment gives the reason — *"so a shortcut-launched chibipop.exe
still finds its settings"* — which applies verbatim to `--dict` and `--rules`
and is currently not honoured by either. Double-click is what makes that a
defect rather than a quirk.

`library/` and `data/` are chibipop's bookkeeping. The user is never asked to
open them.

**Rejected:** *`%LOCALAPPDATA%\chibipop`.* Correct for an installed
application; wrong for one that should be portable and removable by deleting
one folder.

---

## 4. Components

Six new modules. The first three are pure — no Windows API, no filesystem
beyond a byte slice — which is what makes the risky part cheap to verify.

| Module | Responsibility | Ports | Pure |
|---|---|---|---|
| `dict/archive.rs` | Read a Yomitan zip: `index.json`, term banks, frequency banks | `yomitan.py` (61) | ✓ |
| `dict/glossary.rs` | Flatten glossary structures; extract part of speech | `flatten.py` (141) | ✓ |
| `dict/frequency.rs` | Parse rank rows in both nesting shapes | `freq.py` (53) | ✓ |
| `dict/build.rs` | Create schema, write entries and terms, `ANALYZE` | `schema.py`, `build.py` (205) | |
| `library.rs` | The archive folder and its manifest | new | |
| `ui/console.rs` | Console ownership, hide and show, the live lookup log | new | |

`glossary.rs` is the one to be careful with: 141 lines of irregular nested
structures, and the likeliest place for an answer that is subtly wrong and
still compiles.

### New subcommand

`chibipop build-dict --library <dir> --out <file>` — regenerates the database
from a library folder, printing progress to stdout. Public, scriptable, and
the thing the settings window invokes.

---

## 5. Rebuild as a child process

The settings window does not rebuild in-process. It spawns
`chibipop.exe build-dict`, reads its stdout for progress, and swaps the result
in on success.

**Why, in priority order:**

1. **Memory.** The builder holds the whole frequency table in memory —
   plausibly 50–80 MB for a global rank list. In-process, that spike lands in
   the process that is supposed to idle at 12 MB. In a child, it is reclaimed
   on exit.
2. **The parent never holds the database open while it is rewritten.** The app
   opens it `SQLITE_OPEN_READ_ONLY` and mmaps 256 MB; a separate writer
   process avoids reasoning about that at all.
3. **A builder crash cannot take the popup down.**
4. `build-dict` is a subcommand worth having regardless.

The child writes to a temporary file and the parent renames it into place, so
an interrupted rebuild never leaves a half-written database — the same
write-then-rename discipline `Config::save` now uses.

**On the child's stdout and its console.** chibipop is a console-subsystem
binary, so the child has a usable stdout regardless. The parent still spawns
it with piped stdio, which is what lets it *read* the progress rather than
letting it scroll past.

No second console window appears. A child inherits its parent's console, so
`own_console()` sees two processes attached and declines to touch it — which
is the same guard that stops chibipop hiding a user's terminal. If the parent
was double-clicked, the console it inherits is the already-hidden one.

**Sequence:** stage changes → Apply → write `library/` → spawn builder →
progress → swap → restart. Apply already restarts chibipop, so this extends
an existing flow rather than inventing one.

---

## 6. Double-click

- **Console subsystem is kept.** The subsystem approach below was tried and
  reverted; this is the design of record.
- **The console window is hidden** when `GetConsoleProcessList` reports this
  process as its sole attachment — that is what distinguishes a double-click
  (a fresh console, ours alone) from a shell launch (the shell's own console,
  shared). `own_console()` in `src/ui/console.rs` is the one place that
  question is answered.
- **Hidden, never freed.** `FreeConsole` would invalidate stdout, and
  `println!` aborts the process on a failed write — so the console stays
  allocated and merely off-screen.
- **No arguments means `run`.**
- **First run with no dictionary (or no rules) opens the settings window**
  on the import panel, with hovering inactive and a plain explanation —
  rather than a hard failure whose message nobody can see with the console
  hidden.

**Rejected: windows subsystem + `AttachConsole`.** Measured on this machine:
console subsystem gave a correct 647-byte PowerShell redirect; GUI subsystem
plus `AttachConsole(ATTACH_PARENT_PROCESS)` gave 0 bytes; GUI subsystem plus
a conditional `SetStdHandle` also gave 0 bytes, and additionally panicked on
redirection in both PowerShell and cmd. `AttachConsole` does not rebind the
standard handles, and a GUI-subsystem process starts with them invalid, so a
redirected `chibipop lookup X > out.txt` broke outright. Reverted rather than
iterated further.

### Live lookup log

A setting that shows that same hidden console and prints each resolved
hover: what OCR read, which character resolved, which entry won. It answers
*"why isn't it reading this?"* without a command line.

**Trap:** closing a console terminates the process by default, and a
`CTRL_CLOSE_EVENT` handler returning TRUE does not prevent that —
`HandlerRoutine`'s own docs say the system terminates the process regardless,
with no other handler called. So the log window's close item is removed
from its system menu instead (`GetSystemMenu` + `DeleteMenu(SC_CLOSE)`): the
X is greyed out and there is no close event to ever have to answer.

---

## 7. Settings UI

Two groups, because the two kinds genuinely differ — only term dictionaries
have a display order; frequency archives all merge into one effective ranking.

- **Dictionaries — topmost is shown first.** Today's list, plus **Add…** and
  **Remove**. Move up/down unchanged.
- **Frequency data.** A second list with **Add…** and **Remove** only.

Kind is detected on import, exactly as `build.py:139` does it — `frequencyMode`
in `index.json`, or `Freq` in the filename — so the user is never asked which
kind they are adding.

Changes are **staged**. Nothing touches the library until Apply, so adding
three dictionaries costs one rebuild and Apply keeps the meaning it has
everywhere else in that window.

**Trap:** the file picker is modal and pumps its own message loop — the same
`SCROLL_ARMED` latch documented for `TrackPopupMenuEx` in D9. It needs the same
`before_blocking` disarm.

**Rejected:** *one list with a type badge.* Move up/down would silently do
nothing on a frequency row, which reads as a bug.

---

## 8. Build order

The ordering principle is to kill the riskiest unknown first. That unknown is
not the interface — it is whether Rust reads real Yomitan archives correctly.

**The Python builder is an oracle.** It works, it is tested, and it has already
produced a 232 MB database from real archives. Every ported piece can be
checked by running both against the same input and comparing. That is a
stronger guarantee than porting assertions, and it shapes the sequence.

### Phase 0 — the oracle harness

A tiny checked-in fixture archive (three entries, a few KB) and a test pinning
what the Python builder produces from it.

This also closes a real blind spot: `tests/golden.rs` self-skips without a
database, so today a builder that produced a subtly wrong database would pass
CI green.

### Phase 1 — double-click

Exe-relative paths → hide the console when we own it → no-args means
`run` → graceful first run → the live log panel.

First because Phase 3's `library/` lives beside the executable, so the path
work is a prerequisite regardless. Independently shippable.

*Intermediate state:* until Phase 3 exists, "no dictionary" points at the
existing Python builder. That message is upgraded later, not written twice.

### Phase 2 — the port

`archive` → `glossary` → `frequency` → `build`. Pure modules before I/O; each
verified against Python on the fixture, then the whole builder against the
real archives.

### Phase 3 — store and interface

`library.rs` → rebuild orchestration → the two settings groups → staged
changes wired to Apply.

Last deliberately: it is the only part needing a human at the keyboard, so
nothing else queues behind it.

---

## 9. Acceptance

**Tier 0** as always — the gate in `docs/REGRESSION.md`.

**Phase 2 is accepted when** the Rust builder reproduces the Python builder's
output on the fixture, and on the real archives produces identical row counts
with spot-checked content, and `tests/golden.rs` passes against a Rust-built
database.

**Phase 1 needs a tier 2 pass:** double-click a shortcut from a directory that
is not the chibipop folder, confirm no console appears and the popup works;
confirm `probe` from a terminal still prints; confirm closing the live log
does not kill the process.

**Phase 3 needs a tier 2 pass:** import two term archives and one frequency
archive from a clean library, Apply, confirm the rebuild completes and lookups
reflect all three; remove one, Apply, confirm it is gone and the others still
rank correctly.

**Budget checks:** binary under 100 MB, idle memory and CPU unchanged from
Section 1's baselines.

---

## 10. Open

- **No `LICENSE` file.** Out of scope here, but it gates a public release.
- **The settings window grows** by two groups. It auto-sizes via `fit_to`, but
  it may overflow a short screen. Measure before assuming.
- **Rebuild time is inherited, not measured.** 70 s is `build.py`'s figure for
  the current archive set. The Rust builder's time is unknown and worth
  recording rather than assuming it matches.

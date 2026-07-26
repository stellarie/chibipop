# chibipop — Design

**Date:** 2026-07-26
**Status:** Approved (design); not yet implemented
**Revision:** 2 — build order inverted to lookup-and-OCR-first; dictionary sources inspected and
documented (§5); structured-content flattening promoted from non-goal to v1 requirement.
**Supersedes:** nothing. Sits alongside `weikipop`, which stays installed until this beats it.

---

## 1. Problem

`weikipop` (a PyQt6 fork of the Meikipop line, ~8,400 lines) works, but it is heavy and hard to maintain.

The weight is not diffuse — it has two named sources, both verified against the codebase wiki at
`~/notes/wiki/weikipop/` (pinned to commit `df65603`):

1. **The dictionary is eagerly unpickled whole.** `dictionary.pkl` (~39MB) is loaded in full into
   native Python `dict`/`list`/`tuple`/`str` objects on startup, and kept resident
   (`Dictionary-Lookup.md:46`). Python's per-object overhead multiplies that several times over. There
   is no lazy loading on `main`.
2. **The runtime is over-coordinated.** Five daemon threads joined by three single-slot
   "latest-wins" queues, with `None` overloaded to mean both *"clear the popup"* and *"shut down"*
   (`Architecture.md:60`), and a shutdown path that must poke every wait point individually or hang
   (`Architecture.md:76`). Add three coexisting coordinate spaces (`Architecture.md:81`) and the
   Magpie transform layer, and the cost of a safe change is high.

Neither problem is fixable by trimming weikipop. Both are consequences of its data layer and its
process shape.

## 2. Goal and acceptance test

Build a lightweight, Windows-native pop-up dictionary that reads Japanese text **anywhere on
screen** — not only in a browser.

> **Acceptance test.** With the hotkey held, hovering a Japanese word anywhere on screen shows the
> correct headword, reading, and definition beside the cursor; the popup never steals focus; and
> resident memory stays under **50MB (hard) / 20MB (target)**.

The 50MB figure is the bar. The 20MB figure is the ambition. Both are **first measured at M3**, once
a window exists to measure — see §9.

### Scope: lookup-only core

A walking skeleton, fattened only where it is missed in practice. `weikipop` remains installed until
`chibipop` is demonstrably better.

## 3. Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Tiered text acquisition: UI Automation → OCR** | "Anything on screen" makes OCR the mandatory floor — games and video are pixels. But UIA returns *exact* characters instantly, with no capture and no recognition, wherever it is supported. Two providers behind one interface. |
| D2 | **Rust + `windows-rs`** | Smallest resident footprint, no runtime, single executable. The COM/WinRT verbosity is confined to the interop modules and written once. |
| D3 | **SQLite (`rusqlite`), memory-mapped** | Resident RAM ≈ zero; the OS pages what is touched. ~25 indexed point queries per hover is sub-millisecond. Inspectable with off-the-shelf tools, which matters more than raw speed when debugging. |
| D4 | **Raw Win32 layered window + Direct2D/DirectWrite** | The popup must be `NOACTIVATE`, `TOPMOST`, `TOOLWINDOW`, click-through, and absent from its own screen captures. GUI frameworks fight all of these because they want the event loop and the focus model. `windows-rs` is already a dependency, so this adds no new tree. DirectWrite handles Japanese layout natively. |
| D5 | **Event-driven core + one worker thread** | weikipop's five stages are strictly sequential and only the newest request ever matters, so stage-per-thread buys nothing and costs four queues. One worker deletes the entire stale-intermediate bug class. |
| D6 | **Low-level input hooks, not polling** | weikipop polls at ~100Hz forever (`Architecture.md:41`). `WH_MOUSE_LL` / `WH_KEYBOARD_LL` are event-driven: idle CPU ≈ 0%. |

### Rejected alternatives

- **Async (`tokio`), task-per-request.** Cancellation via dropped futures is genuinely elegant, but
  COM/UIA objects are apartment-threaded (STA) and thread-affine. Pairing them with a work-stealing
  executor is a known source of pain, for one concurrent operation's worth of benefit.
- **Python, done properly.** Fixing the pickle would help materially, but CPython + Qt baseline
  remains, and the maintainability half of the complaint is untouched.
- **Node/TS.** Familiar from `chibi-anki`, but ~40MB baseline plus a native addon for UIA plus
  WebView2 for the popup contradicts the premise.
- **WebView2 popup.** Beautiful definitions, second process, 50-100MB. Same contradiction.
- **Memory-mapped FST index.** Genuinely better than SQLite for longest-prefix scanning, and smaller.
  Deferred: opaque when wrong, and we do not yet know the real query distribution. Revisit once we do.

## 4. Architecture

```
┌─ MAIN THREAD ───────────────────────────────────┐
│  Win32 message loop                             │
│   ├── WH_MOUSE_LL / WH_KEYBOARD_LL hooks        │──▶ Trigger{cursor, RequestId}
│   ├── Popup HWND (layered, NOACTIVATE, TOPMOST) │        │  single-slot channel
│   └── Tray icon                                 │        │  (latest wins)
│         ▲                                       │        ▼
│         │ PostMessage(WM_APP_RESULT, id)  ┌─ WORKER THREAD ──────────┐
│         └────────────────────────────────┤  resolve(trigger):        │
└─────────────────────────────────────────  │   1. TextSource::at()    │
                                            │   2. LookupEngine::run() │
                                            └──────────────────────────┘
```

Two threads. One channel. Staleness is resolved by comparing a monotonic `RequestId` — no sentinel
value carries control meaning. Shutdown is: clear the run flag, drop the sender, `PostQuitMessage`.

### Module layout

```
src/
  main.rs          entry, COM init, wiring, message loop
  app.rs           AppState, RequestId epoch, trigger dispatch
  input/hooks.rs   low-level hooks; hotkey-held state
  text/
    mod.rs         trait TextSource; the tiered resolver
    uia.rs         tier 1 — ElementFromPoint → TextPattern → RangeFromPoint
    ocr.rs         tier 2 — Windows.Media.Ocr over a cursor-local capture
    capture.rs     screen grab of a ~600×200px region around the cursor
  lookup/
    mod.rs         trait Dictionary; LookupEngine (prefix scan, POS filter, rank)
    deconj.rs      BFS deconjugator; rules loaded from deconjugator.json
    sqlite.rs      SQLite-backed Dictionary
    model.rs       Entry, Sense, TermRow, Hit
  ui/
    window.rs      layered popup, D2D device, WDA_EXCLUDEFROMCAPTURE
    layout.rs      DirectWrite layout + measure → window size
  geom.rs          coordinate discipline (see §4.3)
tools/build-dict/  offline builder → chibipop.sqlite
```

**Hard rule:** `lookup/`, `deconj.rs`, and `geom.rs` must not depend on the `windows` crate. They are
the pure core and must compile and test without Windows APIs. This is what makes the project testable
at all (§8).

`geom.rs` therefore holds **types and arithmetic only** — `PhysPoint`, `PhysRect`, `MonitorInfo`,
and the mapping functions between them. The OS queries that *produce* a `MonitorInfo` (monitor
enumeration, per-monitor DPI) live in `ui/window.rs` alongside the other Win32 calls and are passed
into `geom.rs` as plain data. The math is testable; the queries are not, and they are kept apart for
exactly that reason.

### 4.1 Interfaces

```rust
pub trait TextSource {
    fn at(&self, p: PhysPoint) -> Result<Option<TextSpan>>;
}

pub struct TextSpan {
    pub text: String,
    pub cursor_byte_offset: usize, // byte offset into `text`, on a char boundary, of the
                                   // first byte of the hovered character
    pub anchor: PhysRect,          // where to put the popup
}

pub trait Dictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>>;
}
```

Both sides of the worker are swappable and fakeable. A fake `TextSource` plus a fixture database
exercises the entire resolve chain headlessly.

### 4.2 Data flow, one hover

1. A hook fires (mouse moved **while the hotkey is held**) → `Trigger{cursor, id}` is written into the
   single-slot channel. Any older pending trigger is overwritten.
2. Worker: `TextSource::at(cursor)` walks the tiers and returns a `TextSpan`.
3. Worker: `LookupEngine::run(&span.text[span.cursor_byte_offset..])` — scan every prefix
   longest-first; deconjugate each prefix; query SQLite for each candidate form; filter by requiring
   the deconjugation's terminal POS tag to appear in the entry's sense POS set; rank; take the top 10.
4. Worker: `PostMessage(WM_APP_RESULT, id)`.
5. Main thread: drop the result if `id < latest_id`; otherwise lay out with DirectWrite, size the
   window to the measured content, position at `span.anchor`, show.

The prefix scan does **not** early-exit. The correct word is frequently reachable only by
deconjugating at a shorter prefix — this is inherited behavior from weikipop and it is correct.

### 4.3 Coordinate discipline

weikipop's documented misplacement bugs come from three coordinate spaces coexisting
(`Architecture.md:81`). Here there is one:

> **Every internal coordinate is a physical pixel in virtual-desktop space.**

Conversion happens only at the OS boundary, only inside `geom.rs`. The process is per-monitor-v2 DPI
aware. `PhysPoint` and `PhysRect` are distinct types from any logical-pixel type, so mixing them is a
compile error rather than a rendering bug.

### 4.4 Popup exclusion from capture

weikipop serializes screen access with a `screen_lock` so the popup is not captured by its own OCR
pass. Instead, `chibipop` calls `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`, which makes
the window invisible to capture APIs outright. The lock disappears.

> ⚠️ **Unverified.** `WDA_EXCLUDEFROMCAPTURE` requires Windows 10 2004+. Its interaction with `BitBlt`
> specifically has not been confirmed, and stays unverified until **M3** (§9). If it does not hold, the
> fallback is to hide the popup for the duration of the capture — one extra state transition, not a
> redesign. The failure is local to `ui/window.rs` and cannot reach the engine.

## 5. Data model

`chibipop.sqlite`, built offline. Senses are stored as JSON **TEXT**, not an opaque blob, so the
database stays inspectable with off-the-shelf tools — half the reason SQLite was chosen (D3).

```sql
CREATE TABLE term (
    surface  TEXT NOT NULL,
    written  TEXT,
    reading  TEXT,
    pos      TEXT NOT NULL DEFAULT '',
    freq     INTEGER,
    entry_id INTEGER NOT NULL REFERENCES entry(entry_id)
);
CREATE INDEX idx_term_surface ON term(surface);

CREATE TABLE entry (
  entry_id INTEGER PRIMARY KEY,
  dict_id  INTEGER NOT NULL REFERENCES dict(dict_id),
  senses   TEXT NOT NULL         -- JSON: [{glosses:[...], pos:[...], misc:[...]}]
);

CREATE TABLE dict (dict_id INTEGER PRIMARY KEY, name TEXT, priority INTEGER);
CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT);  -- schema_version, built_at, source_hashes
```

`term.pos` holds the Yomitan `rules` field verbatim (space-separated keys such as `v1 v5k`). It is denormalised onto the term row so the part-of-speech filter in §4.2 costs no extra query on the hot path.

`deconjugator.json` (~1,200 rules) ships as a **sibling file** loaded at startup, not baked into the
database. Rule tuning should not require a dictionary rebuild, and parsing it costs nothing.

### Ranking

Ported from weikipop, which is a known-good baseline
(`Dictionary-Lookup.md:32`): score on match length, then frequency, then dictionary priority; penalise
by deconjugation step count; bonus for an unconjugated all-kana match. Entries are grouped by
`(written, reading, dict)` and sorted by `(-match_len, -priority, dict_priority)`.

### Dictionary sourcing

Source material lives in `C:\Users\Stella\Documents\dicts`. All three archives were inspected
directly on 2026-07-26; the facts below are read from the files, not recalled.

| Archive | Format | Role | Notes |
|---|---|---|---|
| `01 [JA-EN] jitendex-yomitan (2026-07-09).zip` | Yomitan `format: 3`, `sequenced: true` | Primary JA→EN | rev `2026.07.09.0`, CC BY-SA 4.0. `term_bank_*.json`, `tag_bank_*.json`, `styles.css`, `graphics/*.avif`, `HanaMinA/*.svg`. **No `term_meta_bank`** — carries no frequency data. |
| `[JA-JA] 大辞林　第四版.zip` | Yomitan `format: 3`, `sequenced: true` | JA→JA monolingual | 3,028 entries, rev `daijirin2;2023-07-10`, © Sanseido. `term_bank_*.json` + `gaiji/*.svg`. |
| `[JA Freq] jiten_freq_global (2026-06-14).zip` | Yomitan `format: 3`, `sequenced: false` | Frequency | `frequencyMode: "rank-based"`, one `term_meta_bank_1.json`. |

**There is no JMdict XML step.** Jitendex is already JMdict-derived and shipped in Yomitan format, so
the builder is a Yomitan archive reader and nothing more. weikipop's `build_dictionary.py` is a
reference for the *transformations*, not a component to port wholesale.

**Term bank row schema** (verified against both archives):

```
[ term, reading, definitionTags, rules, score, glossary[], sequence, termTags ]
    0      1           2           3      4        5           6         7
```

Field 3, `rules`, carries the Yomitan deconjugation part-of-speech key (`v1`, `v5k`, `adj-i`, …).
**This is the field that feeds the POS filter** in §4.2 — it is the same vocabulary
`deconjugator.json` emits as its terminal tag, so the two line up without a mapping table. Field 4,
`score`, is a per-dictionary sort hint, not a frequency.

**Frequency bank rows have two shapes in the same file.** Both were observed in
`term_meta_bank_1.json`:

```json
["の","freq",{"value":1,"displayValue":"1㋕"}]
["乃","freq",{"reading":"の","frequency":{"value":1,"displayValue":"1㋕"}}]
```

The second form is **reading-scoped** and nests `value` one level deeper under `frequency`. The
builder must handle both, and must write the reading into `term.reading` for the scoped form —
otherwise a rare kanji spelling inherits the rank of its common homophone. Because the mode is
`rank-based`, `value` maps directly onto `term.freq` with lower meaning more common, which is the
semantics the schema above already specifies.

**Builder gotchas, learned the hard way:**

- **Do not use .NET Framework's `ZipArchive`.** Windows PowerShell 5.1 opens the Daijirin archive
  without error and reports **0 entries**, despite a hand-verified central directory declaring 3,028.
  Python's `zipfile` reads the identical file correctly. The archive is structurally sound
  (`PK\x03\x04` header; CD offset 86,825,921 + size 342,950 lands exactly 22 bytes — one EOCD record —
  before EOF).
- Archive **filenames contain `[`, `]`, and a full-width space (U+3000)**. Square brackets are
  PowerShell wildcard metacharacters, so every path must be passed as `-LiteralPath`, and native
  executables invoked with these paths lose the Japanese characters to the console codepage. The
  builder should glob the directory rather than accept hand-typed filenames.

Build time is not runtime memory, so the builder stays in Python if that is quickest. Only the emitted
`.sqlite` is a `chibipop` artifact.

### Structured content

Both dictionaries store glossaries as Yomitan `structured-content` trees, not plain strings — Jitendex
wraps every sense in `div`/`span` nodes carrying POS tags, cross-references, and `ruby`/`rt` furigana;
Daijirin uses Japanese semantic node names (`見出部`, `語義G`, `語釈`) and references `gaiji/*.svg`
glyphs. **Rendering nothing is not an option: it would leave every entry blank.**

Resolution: **the offline builder flattens structured content to plain text**, mirroring weikipop,
which bakes its rendering at import time rather than query time (`Dictionary-Lookup.md:51`). The
runtime stays a dumb consumer of strings, and the tree-walking complexity lives where it costs no
memory and is trivially unit-testable.

v1 flattening rules: keep gloss text and POS labels; keep `ruby` base text and **drop `rt` furigana**;
drop `img` and `gaiji` references; drop styling; render cross-references as their plain label. Rich
rendering at runtime remains a non-goal (§7) — but *flattened* structured content is a v1 requirement,
not an extra.

## 6. Error handling

| Case | Response |
|---|---|
| UIA returns nothing | **Expected, not an error.** Fall through to OCR silently. |
| OCR has no Japanese language pack | Detect at startup; one tray notification; degrade to UIA-only. Do not exit. |
| Dictionary file missing or corrupt | **Hard fail at startup** with a clear message. It is the entire product. |
| COM initialization fails | Hard fail, naming which call failed. |
| Hook installation fails | Hard fail, naming which hook. |
| Any error inside the resolve chain | Log it, show nothing, keep running. A failed hover is never fatal. |
| **Panic inside a hook or D2D callback** | `catch_unwind` at every FFI boundary. Unwinding across a Win32 callback boundary is undefined behavior. Not optional. |

Startup performs a **capability probe** — OCR language availability, UIA reachability, display
affinity support — and logs the resulting tier configuration once, so a degraded run is visible rather
than mysterious.

## 7. Non-goals for v1

Explicitly out of scope, so their absence later reads as a decision rather than an oversight:

- Anki / `chibi-anki` mining
- Settings GUI (configuration is a hand-edited TOML file)
- Multi-dictionary merging (one dictionary)
- Kanji information panel
- Yomitan live-API integration
- **Rich** structured content at runtime: images, `gaiji` glyphs, furigana, styling, live
  cross-reference links. Note the distinction — structured content is *flattened to text by the
  builder* and that part is required, not deferred (§5).
- Cross-platform support
- **Magpie support.** Deliberately deferred (see §10). `geom.rs` keeps one clean coordinate
  discipline for v1; Magpie's transform arrives later as its own bounded module, not as a
  pre-emptive abstraction.

## 8. Testing

**What is tested automatically** (all of it in the pure core, no Windows APIs):

- Deconjugation: rule application, tag threading, BFS fixpoint termination, iteration cap.
- Lookup engine: prefix scan order, POS filtering, kana/kanji suppression rules, ranking, truncation.
- Coordinate math in `geom.rs`.
- **Golden corpus** — `(sentence, cursor_byte_offset) → expected headword`. This is the highest-value test in
  the project: it is pure, fast, and catches every regression that changes what the user actually sees.
- Integration: a fixture `.sqlite` plus a fake `TextSource` drives the whole resolve chain headlessly.

**What cannot be tested here, and will be reported as unverified:**

UIA behavior against real applications, OCR accuracy, Direct2D rendering output, focus and topmost
behavior, DPI scaling, and multi-monitor placement. This is the same wall weikipop hit. These get a
written manual acceptance checklist, executed by the user on the target machine, and results are
recorded rather than assumed.

The `chibipop probe --at <x>,<y>` command built at M2 (§9) is the **manual verification harness** for
the untestable half: it prints what was captured, what OCR recognised, which token the hit-scan chose,
and what the engine looked up — turning "the popup showed the wrong word" into four separately
inspectable stages. Building it is not optional polish; it is how the OCR tier gets debugged at all.

## 9. Build order — lookup and OCR first

> **Note the inversion.** D1 specifies UIA → OCR as the *runtime* tier order, and that is unchanged.
> The *build* order is the reverse: OCR is built first because it is the tier that must work
> everywhere, while UIA is an optimization layered onto an already-working product. Building OCR
> first also means a failure to get UIA coverage costs nothing already built.

### M0 · OCR availability probe *(throwaway, ~30 minutes, gates everything)*

Is `ja` present in `Windows.Media.Ocr`'s `AvailableRecognizerLanguages` on this machine?

**Branch condition:** if it is absent and the language pack cannot be installed, the OCR tier needs a
different engine, and the memory budget in §2 is re-decided **before** any production code is written.
Every other milestone assumes this probe passes.

### M1 · Pure lookup core *(no Windows APIs, no GUI)*

Builder (Yomitan archives → `chibipop.sqlite`, including structured-content flattening and both
frequency row shapes), SQLite store, deconjugator, lookup engine, and a CLI entry point:

```
chibipop lookup 食べさせられた
```

Golden corpus green. This milestone is fully verifiable on any machine and carries the majority of the
product's correctness risk.

### M2 · OCR tier — the walking skeleton *(still headless)*

`capture.rs` + `ocr.rs` + hit-scan wired to M1, exposed as `chibipop probe --at <x>,<y>`, which
captures around a screen point and prints what it recognised and what it looked up.

**This is the real walking skeleton:** the complete acquire → recognise → lookup path, end to end,
verifiable from a terminal, with zero Win32 window code written. If OCR accuracy or hit-scan
positioning is bad, it surfaces here — before any effort goes into rendering.

### M3 · Popup

Layered `NOACTIVATE | TOPMOST | TOOLWINDOW` window, Direct2D/DirectWrite rendering, low-level input
hooks, and the `WDA_EXCLUDEFROMCAPTURE` probe from §4.4. **Measure resident memory** against §2.

### M4 · UIA tier

Add tier 1 to a product that already works. Probe `ElementFromPoint` → `TextPattern` →
`RangeFromPoint` coverage across real applications, now informed by actual usage rather than
speculation.

**Branch condition:** if coverage is poor across the board, delete the tier. Because OCR shipped in
M2, this costs one module and no rework.

### M5 · Polish

DPI and multi-monitor placement, TOML configuration, tray menu.

**Ordering rationale.** M0 is the one unknown that can invalidate the whole approach, so it goes
first and costs half an hour. M1 and M2 deliver the entire product value while remaining fully
verifiable headlessly — all Win32 window and focus complexity is deferred behind them. M3 introduces
GUI risk only once the engine is proven. M4 is an enhancement with a clean delete path.

The one risk this ordering accepts knowingly: the layered-window behaviour in §4.4 stays unverified
until M3. That is tolerable because its failure mode is local — hide the popup during capture instead
of excluding it — and does not reach the engine.

## 10. Deferred, with rationale

| Item | Why deferred | Cost to add later |
|---|---|---|
| Magpie support | Was a significant complexity source in weikipop (sticky source-rect-clamped transforms, topmost re-assertion, flip hysteresis). Adding it before the foundation is proven risks contaminating `geom.rs`. | A bounded transform module plus a call site in `geom.rs`. Higher than pre-designing a seam, accepted deliberately. |
| FST index | Better than SQLite for this access pattern, but opaque to debug and premature before the query distribution is known. | `Dictionary` is already a trait; a second implementation slots in without touching the engine. |
| Mining to `chibi-anki` | Not part of the lookup-only acceptance test. | `chibi-anki` already exposes an AnkiConnect v6 endpoint over HTTP; this becomes an HTTP call plus a key binding. |
| Settings GUI | TOML is sufficient for a single user. | Independent of the core. |

## 11. Open questions

None blocking. Three unknowns remain, each scheduled at the milestone that can settle it cheaply:

| Unknown | Settled at | If it fails |
|---|---|---|
| Is `ja` OCR available on this machine? | **M0** (~30 min, gates everything) | Different OCR engine; §2 memory budget re-decided before any production code. |
| Does `WDA_EXCLUDEFROMCAPTURE` hold against `BitBlt`? | **M3** | Hide the popup during capture instead. Local to `ui/window.rs`. |
| Does UIA `RangeFromPoint` return usable text and offsets in real applications? | **M4** | Delete the tier. Costs one module, no rework, because OCR shipped at M2. |

None of the three can invalidate M1, which is where most of the correctness risk lives.

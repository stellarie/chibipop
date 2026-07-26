# chibipop — Design

**Date:** 2026-07-26
**Status:** Approved (design); not yet implemented
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

The 50MB figure is the bar. The 20MB figure is the ambition. Both are **measured at M0**, before
any dictionary work — see §9.

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
> specifically has not been confirmed. This is probe #1 of the M-1 spike (§9). If it does not hold, the
> fallback is to hide the popup for the duration of the capture — one extra state transition, not a
> redesign.

## 5. Data model

`chibipop.sqlite`, built offline. Senses are stored as JSON **TEXT**, not an opaque blob, so the
database stays inspectable with off-the-shelf tools — half the reason SQLite was chosen (D3).

```sql
CREATE TABLE term (              -- the hot index; ~25 point queries per hover
  surface  TEXT NOT NULL,        -- scan key (kana or kanji surface form)
  written  TEXT,                 -- kanji headword; NULL if the headword is kana-only
  reading  TEXT,
  freq     INTEGER,              -- lower = more common; NULL = unranked
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

`deconjugator.json` (~1,200 rules) ships as a **sibling file** loaded at startup, not baked into the
database. Rule tuning should not require a dictionary rebuild, and parsing it costs nothing.

### Ranking

Ported from weikipop, which is a known-good baseline
(`Dictionary-Lookup.md:32`): score on match length, then frequency, then dictionary priority; penalise
by deconjugation step count; bonus for an unconjugated all-kana match. Entries are grouped by
`(written, reading, dict)` and sorted by `(-match_len, -priority, dict_priority)`.

### Dictionary sourcing

The offline builder is a **port of, or a direct reuse of, weikipop's `scripts/build_dictionary.py`**
(JMdict + Yomitan import + JPDB/jiten.moe frequency data), retargeted to emit the schema above. Build
time is not runtime memory, so this stays in Python if that is faster to get working. Only the emitted
`.sqlite` is a `chibipop` artifact.

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
- Structured content: images, furigana, styled glosses
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

## 9. Build order — riskiest first

### M-1 · Spike (throwaway code, before anything real)

Three probes that can each invalidate part of the design:

1. **Window behavior.** A layered `NOACTIVATE | TOPMOST | TOOLWINDOW` window with
   `WDA_EXCLUDEFROMCAPTURE`: does it never take focus, and is it genuinely absent from a `BitBlt`
   capture?
2. **UIA coverage.** `ElementFromPoint` → `TextPattern` → `RangeFromPoint` in five applications the
   user actually reads in: does it return usable text *and* a correct character offset?
3. **OCR availability.** Is `ja` present in `Windows.Media.Ocr`'s `AvailableRecognizerLanguages` on
   this machine?

**Deliverable:** a one-page findings note.
**Branch conditions:** if #2 fails across the board, the UIA tier is deleted and the design simplifies
to OCR-only. If #3 fails, an alternative OCR engine is needed and the memory budget is re-decided
before proceeding.

### M0 · Skeleton
Hotkey + hooks + a popup rendering a hardcoded string at the cursor. **Measure resident memory** and
compare against §2.

### M1 · Pure core
Offline builder, SQLite store, deconjugator, lookup engine, and a CLI entry point
(`chibipop lookup 食べさせられた`) so the engine is usable and provable with no GUI at all. Golden
corpus green.

### M2 · UIA tier
Wire tier 1. Real lookups in a browser and a text editor.

### M3 · OCR tier
Wire tier 2. Real lookups in a game or video.

### M4 · Polish
DPI and multi-monitor placement, TOML configuration, tray menu.

Ordering rationale: M-1 and M0 attack the unknowns that could invalidate the architecture; M1 is
pure logic and fully verifiable; M2 and M3 add the two text tiers independently, so a failure in one
does not block the other.

## 10. Deferred, with rationale

| Item | Why deferred | Cost to add later |
|---|---|---|
| Magpie support | Was a significant complexity source in weikipop (sticky source-rect-clamped transforms, topmost re-assertion, flip hysteresis). Adding it before the foundation is proven risks contaminating `geom.rs`. | A bounded transform module plus a call site in `geom.rs`. Higher than pre-designing a seam, accepted deliberately. |
| FST index | Better than SQLite for this access pattern, but opaque to debug and premature before the query distribution is known. | `Dictionary` is already a trait; a second implementation slots in without touching the engine. |
| Mining to `chibi-anki` | Not part of the lookup-only acceptance test. | `chibi-anki` already exposes an AnkiConnect v6 endpoint over HTTP; this becomes an HTTP call plus a key binding. |
| Settings GUI | TOML is sufficient for a single user. | Independent of the core. |

## 11. Open questions

None blocking. The three M-1 probes are the outstanding unknowns, and they are scheduled as the first
unit of work precisely because they are unknown.

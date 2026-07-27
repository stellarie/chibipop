# chibipop M3 — The Popup

**Date:** 2026-07-27
**Status:** Approved (design); not yet implemented
**Parent spec:** `docs/superpowers/specs/2026-07-26-chibipop-design.md` (rev 3). Where the two
disagree, the parent governs — reconcile before coding.
**Depends on:** M1 (the lookup core) and M2 (the OCR tier), both built and merged.

---

## 1. Scope

M3 turns chibipop from a terminal tool into the product: a popup that appears beside the character
under the cursor, showing its definition, without stealing focus.

Everything it needs already exists. M2's `OcrTextSource` resolves a screen point to a `TextSpan`, and
M1's `LookupEngine` turns that into ranked `Hit`s. M3 adds the three things still missing — deciding
what to *show*, painting it, and reacting to input.

This is also where the memory budget stops being an estimate. The parent spec's **50MB hard / 20MB
target** is measured here for the first time, because it is the first milestone with a resident
process.

## 2. What the parent spec already fixes

Not re-litigated here: raw Win32 layered window with Direct2D/DirectWrite (D4); `NOACTIVATE`,
`TOPMOST`, `TOOLWINDOW`, click-through; `WDA_EXCLUDEFROMCAPTURE` to keep the popup out of captures;
an event-driven main thread with **one** worker and staleness resolved by a monotonic `RequestId`
(D5); low-level `WH_MOUSE_LL`/`WH_KEYBOARD_LL` hooks rather than polling (D6).

## 3. Decisions

| # | Decision | Rationale |
|---|---|---|
| M3-D1 | **Top match rendered in full; every other match collapsed to one line** | A single hover on `日` returns 10 entries, one of them a ~900-character 大辞林 essay. Showing everything is unusable; showing only the top hides the alternative when ranking picks wrong — and with 10 candidates for one kanji, sometimes it will. |
| M3-D2 | **Merge entries by `(written, reading)`; 大辞林 before Jitendex inside a card** | The engine returns one entry per dictionary, so `昨日` legitimately appears four times. Merging fixes that duplication. The monolingual definition leads because that is the one being read; English confirms. |
| M3-D3 | **Two trigger modes: always-live and hold-Shift**, switchable from the tray | Always-live is what M2's `watch` proved pleasant for sustained reading. Hold-Shift is what stops a popup appearing while writing code. Neither is right for every session, so it is a mode, not a default. |
| M3-D4 | **Cap the popup at 45% of monitor height and truncate with a visible marker; no scrolling** | weikipop's wiki records popup scrolling as a bug source: scroll expansion, scroll-race guards, a render epoch to stop stale content flashing. Scrolling also needs wheel capture in the hook and per-lookup scroll state. The cost is losing the tail of very long 大辞林 entries — usually its archaic and literary senses. |
| M3-D5 | **A pure `present.rs` with no Windows dependency** | Every content decision — what merges, what order, what collapses, where truncation falls — is a pure function from `Vec<Hit>` to a `Presentation`. This is the part of M3 most likely to be wrong, and it is the part that can be fully unit-tested. Only painting touches Win32. |
| M3-D7 | **Constant-alpha layered window (`SetLayeredWindowAttributes` + `WM_PAINT`), not per-pixel alpha** | Verified by controlled experiment: `WDA_EXCLUDEFROMCAPTURE` is **incompatible with `UpdateLayeredWindow`**. See §5. Capture exclusion wins, because without it the popup must be hidden and re-shown around every capture — in live mode, on every >4px move — which is a constant flicker while reading. Cost: `SetWindowRgn`-clipped corners are hard-edged rather than antialiased, and no soft/variable transparency. |
| M3-D6 | **Dark theme by default** | The reading contexts are manga, visual novels and terminals — mostly dark. A light popup flashing over dark content at night is genuinely unpleasant; the reverse is milder. |

### Rejected alternatives

- **Scrolling the popup.** Reaches all content, at the cost of the exact bug class the predecessor
  documented. Revisit if truncation actually bites in use.
- **Showing all matches uniformly compact.** Densest overview, but 大辞林's value *is* the detail,
  and a one-line summary of it says nothing.
- **Keeping per-dictionary entries separate as ranked.** Simplest, no grouping pass — but the top
  slot and the first collapsed row would routinely be the same word twice.
- **Following the Windows app theme.** Feels native, but the system setting frequently disagrees
  with whatever application is actually being read in.

## 4. Architecture

```
┌─ MAIN THREAD ──────────────────────────────┐
│  Win32 message loop                        │
│   ├── WH_MOUSE_LL / WH_KEYBOARD_LL         │──▶ Trigger{cursor, RequestId}
│   ├── Popup HWND (layered, NOACTIVATE)     │        │  single-slot channel
│   └── Tray icon (mode toggle, quit)        │        ▼
│         ▲                            ┌─ WORKER ─────────────┐
│         │ PostMessage(WM_APP_RESULT)  │ OcrTextSource::at()  │
│         └────────────────────────────┤ LookupEngine::run()  │
└──────────────────────────────────────│ present::build()     │
                                       └──────────────────────┘
```

Two threads, one channel. The worker reuses M2's `OcrTextSource` and M1's `LookupEngine`
**unchanged** — M3 adds no lookup logic. Staleness is resolved by comparing `RequestId`; no sentinel
value carries control meaning. Shutdown clears the run flag, drops the sender, unhooks, and posts
`WM_QUIT`.

### Module layout

```
src/present.rs      Vec<Hit> -> Presentation                                  [pure]  NEW
src/app.rs          AppState, RequestId epoch, worker thread, dispatch      [windows] NEW
src/input/
  mod.rs                                                                              NEW
  hooks.rs          low-level hooks, modifier state, mode                   [windows] NEW
src/ui/
  mod.rs                                                                              NEW
  theme.rs          colours, sizes, font names                               [pure]  NEW
  window.rs         layered popup, D2D device, capture exclusion            [windows] NEW
  render.rs         DirectWrite measure + paint                             [windows] NEW
  tray.rs           tray icon, mode toggle, quit                            [windows] NEW
```

**Hard rule, extended from M1 and M2:** `src/lookup/`, `src/geom.rs`, `src/text/mod.rs`,
`src/text/layout.rs`, **`src/present.rs`** and **`src/ui/theme.rs`** must not depend on the `windows`
crate.

### 4.1 The presentation model

```rust
// present.rs - no Windows types anywhere
pub struct Presentation {
    pub top: Option<Card>,
    pub collapsed: Vec<CollapsedRow>,
}

pub struct Card {
    pub written: Option<String>,
    pub reading: Option<String>,
    pub pos: Vec<String>,
    pub freq: Option<i64>,
    /// One block per dictionary, already in display order.
    pub blocks: Vec<GlossBlock>,
}

pub struct GlossBlock {
    pub dict_name: String,
    pub glosses: Vec<String>,
}

pub struct CollapsedRow {
    pub written: Option<String>,
    pub reading: Option<String>,
    /// First gloss, truncated on a char boundary.
    pub summary: String,
}

/// Dictionary identity, loaded once at startup and passed in as plain data so
/// `present.rs` stays free of any database dependency.
pub struct DictInfo { pub dict_id: i64, pub name: String }

pub fn build(hits: &[Hit], dicts: &[DictInfo], cfg: &PresentConfig) -> Presentation;
```

**One small addition to M1's reader.** `Entry` carries `dict_id` but nothing exposes the dictionary's
*name*, which a `GlossBlock` needs and which `dict_order` matches against. The `Dictionary` trait
gains `fn dicts(&self) -> Result<Vec<DictInfo>>`, reading the existing `dict` table; `SqliteDictionary`
implements it, `FakeDictionary` returns whatever a test seeds. It is called **once at startup**, not
per lookup — the `dict` table has two rows.

**Merging.** Group hits by `(written, reading)`. A group's position is the rank of its best hit, so
merging never reorders results relative to each other. The first group becomes `top`; the rest become
`CollapsedRow`s.

**Dictionary order inside a card.** `PresentConfig` carries `dict_order: Vec<String>` — a list of
**case-insensitive substrings** matched against the dictionary's `name`. Default
`["大辞林", "Jitendex"]`. Blocks whose name matches nothing listed sort last, by `dict_id`.
Substring matching rather than exact names because Jitendex's name embeds a release date
(`Jitendex.org [2026-07-09]`) that changes whenever the dictionary is updated — an exact-match config
would silently stop applying after an update.

**Truncation.** `CollapsedRow::summary` is the first gloss of the first block, cut to
`summary_chars` (default 40) **characters, not bytes**, with `…` appended when cut. `Card` glosses
are not truncated in `present.rs`; the popup's height cap is enforced during rendering, where the
measured text is known.

### 4.2 Behaviour

**Trigger modes.** `Live` fires on any cursor movement; `HoldShift` fires only while Shift is down.
The mode lives in `AppState`, is read by the hook, and is persisted to the TOML when changed from the
tray. Both modes still apply M2's 4-pixel movement gate, but it moves out of `watch`'s poll loop and
into `app.rs`'s dispatch, since M3 is hook-driven rather than polled — the hook fires on every mouse
event, and re-resolving on a one-pixel tremor would be far worse than it was at 8 Hz.

**Positioning.** The popup is placed below and to the right of the hovered character's `anchor`, with
a **12-pixel gap**, so it never covers the character being read. If it would cross the current monitor's
right or bottom edge it flips to the other side of the anchor on that axis. The flip arithmetic is
pure and lives in `geom.rs`, where it is testable; the monitor rectangle is passed in as plain data.

**Overflow.** Height is capped at 45% of the current monitor's height. Content beyond the cap is not
drawn; the last visible line is followed by a dimmed `…` on its own row, so a truncated entry is
visibly different from one that simply ended.

**Theme.** Dark by default, overridable in the TOML. Font is Yu Gothic UI — already present on the
target machine and the face the OCR fixtures were rendered in.

### 4.3 Configuration

A TOML file beside the executable, created with defaults on first run:

```toml
[trigger]
mode = "live"            # "live" | "hold-shift"

[popup]
theme = "dark"           # "dark" | "light"
exclude_from_capture = true   # false makes the popup recordable - see section 5
max_height_percent = 45
summary_chars = 40
font = "Yu Gothic UI"

[dictionaries]
display_order = ["大辞林", "Jitendex"]
```

## 5. Capture exclusion — measured, not assumed

`WDA_EXCLUDEFROMCAPTURE` was unverified from the first spec until now. It matters more here than
anywhere: M2 captures a region around the cursor on **every hover**, so a popup that is not excluded
photographs itself and feeds its own text into the next lookup.

A throwaway spike settled it with an A/B/control test on this machine, measuring captured pixels
rather than trusting a return value:

| Window | `SetWindowDisplayAffinity` | Popup pixels in a BitBlt |
|---|---|---|
| Layered, per-pixel alpha via `UpdateLayeredWindow` | **Fails** — `HRESULT(0x80070008)` | **69,908 / 76,800 (91%)** |
| Layered, constant alpha via `SetLayeredWindowAttributes(LWA_ALPHA)`, `WM_PAINT`-painted | Succeeds | **0 / 76,800 (0%)** |
| **Control:** not layered at all, opaque `WM_PAINT` | Succeeds | 0 / 76,800 (0%) |
| Constant alpha **plus** `SetWindowRgn` rounded silhouette | Succeeds | 0 / 76,800 (0%) |

Two things that would have destroyed a plan written from documentation:

1. **The error message lies.** `0x80070008` is "not enough memory". It has nothing to do with memory —
   it is how Windows reports that display affinity is unsupported for this window flavour. Worse, code
   that ignores the return value gets a **silent no-op**: the call appears to work and the popup is
   captured anyway.
2. **The control run is what makes this conclusive.** A plain non-layered window also excludes cleanly,
   which isolates `UpdateLayeredWindow` specifically as the trigger — not `WS_EX_LAYERED` in general,
   and not `TOPMOST`/`NOACTIVATE`/`TOOLWINDOW`.

**Consequence, baked into M3-D7:** the popup is `WS_EX_LAYERED` + `SetLayeredWindowAttributes(LWA_ALPHA)`,
painted through an `ID2D1HwndRenderTarget` on `WM_PAINT`, with `SetWindowRgn(CreateRoundRectRgn(...))`
for a rounded silhouette. Not `ID2D1DCRenderTarget` + `UpdateLayeredWindow`, which is prettier and
cannot be excluded.

**Implementation requirement:** `SetWindowDisplayAffinity`'s result must be **checked and reported at
startup**, never discarded. A silent no-op here is the failure mode that turns the whole OCR tier into
a feedback loop.

### 5.1 The cost of exclusion, and the opt-out

Exclusion is not selective: it hides the popup from **every** capture API, not just chibipop's own.
Screen recording, screenshots, screen sharing in Discord/Zoom/Teams, and remote desktop all show the
popup missing while it is plainly visible on the physical display. This was discovered in use, not
predicted — and it is the same wall that prevented the popup's rendered text from being verified by
anything except the user's own eyes during M3.

`[popup] exclude_from_capture` (default `true`) opts out. When it is `false`, the popup **must be
hidden for the duration of each capture and reshown afterwards**, rather than simply leaving the
affinity call out.

Leaving it out unguarded is not acceptable: the popup would then sit inside the 900×300 region
captured on every hover. It never covers the cursor — there is a 12-pixel gap and the never-covers-the-anchor
guarantee — so the correct character is usually still chosen, but the popup's own text can be caught by
the hit-scan's near-miss tolerance or contaminate the assembled line. That produces occasional silent
wrong answers, which is a worse failure than a visible flicker.

The hide-and-reshow costs a brief flicker on every re-resolution while recording. That is precisely
what weikipop does full-time via a `screen_lock`; here it is opt-in and off by default, so ordinary
reading is unaffected.

## 6. Error handling

| Case | Response |
|---|---|
| Hook installation fails | **Hard fail** at startup, naming which hook. Without hooks there is no product. |
| Tray icon creation fails | **Hard fail** — it is the only way to change mode or quit. |
| Direct2D device lost | Recreate the device on the next paint. One dropped frame, not a crash. |
| Render error | Log, hide the popup, keep running. |
| Worker error (capture, OCR, lookup) | Log, show nothing, keep running. A failed hover is never fatal. |
| No result for a position | Hide the popup. Not an error. |
| **Panic inside a hook or D2D callback** | `catch_unwind` at every FFI boundary. Unwinding across a Win32 callback boundary is undefined behaviour. Not optional. |

## 7. Testing

**Pure, runs anywhere:**

- `present.rs`: the four `昨日` hits merge to one card; 大辞林 orders before Jitendex regardless of
  `dict_id`; a dictionary matching no configured substring sorts last; group order follows the best
  hit's rank; `summary` truncates on a char boundary and appends `…` only when it actually cut; an
  empty hit list yields no card and no rows; a single hit yields a card and no rows.
- `geom.rs`: edge-flip placement — a popup that fits is placed below-right; one that would cross the
  right edge flips left; one that would cross the bottom flips up; one that would cross both flips
  both.
- `theme.rs`: constants are self-consistent (no zero sizes, both themes define every colour used).
- `present.rs` dictionary naming: a `GlossBlock` carries the name from `DictInfo`, and a hit whose
  `dict_id` has no matching `DictInfo` still produces a block rather than being dropped.

**Honestly untestable, and reported as such:** focus behaviour, topmost ordering, capture exclusion,
glyph rendering, tray interaction, and the memory figure. These get a written manual checklist,
executed by the user, with results recorded rather than assumed.

## 8. Non-goals for M3

UIA tier and the tiered resolver (M4) · DPI/multi-monitor placement polish beyond edge-flipping, and
Magpie (M5) · popup scrolling · clicking anything in the popup · mining to `chibi-anki` · a settings
GUI (the TOML is edited by hand) · audio · furigana rendering · images and `gaiji` glyphs.

## 9. Acceptance

> **Automated:** `cargo test` green including the new `present.rs` and `geom.rs` tests, zero warnings.
>
> **Manual, run by the user:** with chibipop running, hover Japanese text in a real application and
> see the correct definition appear beside the character — in both trigger modes; focus is never
> stolen (the window being read keeps its caret and title bar); the popup does not appear in its own
> OCR capture; it flips rather than crossing a screen edge; and the tray can switch mode and quit.
>
> **Measured and reported honestly:** resident memory of the running process, against the parent
> spec's 50MB hard / 20MB target. If it exceeds the hard bar, that is a finding to report, not a
> number to quietly omit.

## 10. Open questions

None blocking. Two risks, each with a stated fallback:

| Risk | If it bites |
|---|---|
| ~~`WDA_EXCLUDEFROMCAPTURE` does not hold against BitBlt~~ | **Resolved before implementation** — measured working with constant alpha, measured broken with per-pixel alpha. See §5. |
| Resident memory exceeds the 50MB hard bar | Report it. The likely culprits are the D2D/DirectWrite device stack and the OCR engine, both of which are one-time allocations — the measurement tells us which, and neither is addressed by guessing now. |

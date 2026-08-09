# chibipop — backlog

Work that is designed or measured but deliberately not built yet, with the reason and the
evidence needed to pick it up cheaply.

**Append only; never renumber.** These numbers are stable IDs and other documents cite them by
number — `docs/REGRESSION.md` alone points at 7 and 8. New items go at the **end**. (The header
said "Newest first" until 2026-08-09, which no item had ever obeyed and which would have had the
next person inserting at the top and shifting every ID below it.)

---

## 1. Forward tiling: make multi-pass resolve the character under the cursor

**Deferred 2026-07-28 by the user's decision:** single-pass is accurate today
(`max_ocr_passes = 1`, 9/9 on the measured sweep), so the tiling rework is not worth gating
working changes on. The design is done and independently reviewed — this is a build, not a
design round.

**Source:** the accuracy-and-polish design §3 (D1-R, D1-R2) and its plan,
Tasks 3–4 and 6 §2.1. Those working notes are not published with the repo.

### What is already built

- `text::layout::TextGeom` and `union_chars` — landed `17e18b0`.
- `TextSpan::geom`, populated by `resolve` on the single-pass path — landed `17e18b0`.
- **`drop_leading(words, start, orientation)`** in `text/layout.rs` — the mirror of
  `split_at_clipped`'s trailing rule, carrying the same `EDGE_MARGIN` tolerance, wired into
  `tile_forward` right after `read(tile)`. Five unit tests, including the load-bearing "a word
  just inside the margin is kept" boundary case, plus one `tile_forward`-level integration test
  proving a tile that returns a word before its own `start` no longer prepends it.
- **Head + tail assembly**, as `head_and_tail(lines, cursor, region)` in `text/layout.rs` (pure,
  screen-independent) plus the orchestration in `resolve_at_tiled_scanned`
  (`text/ocr.rs`). Pass 1's own tail — from the hovered word through its last word not clipped by
  pass 1's own region edge — becomes the stitched text's head, verbatim, never re-read. Tile 1
  then opens at that last kept word's own trailing edge (`head_and_tail`'s own `split_at_clipped`
  call gives this directly), not at pass 1's region edge or the hovered word's own leading edge —
  closing exactly the gap the 2026-08-07 revision named. `MAX_LOOKUP_CHARS` is split between head
  and tail so the combined text still respects the existing budget. Five unit tests on
  `head_and_tail` (including the case where pass 1 distrusts even its own hit, at the region's own
  edge) plus the full existing suite, unchanged.

### What is left

1. ~~`drop_leading`~~ — **done**, see above.
2. ~~Head + tail assembly~~ — **done**, see above.
3. **Carry `geom` through the seam.** `tile_forward` returns a `String` and drops the boxes, so a
   stitched span has empty `geom` and **the match highlight does not draw on the tiled path**.
   It is absent rather than wrong (`union_chars` returns `None` on empty geometry), and
   `resolve_at_tiled_scanned`'s doc comment says so — but it is a real gap the moment tiling
   comes back on. Still not built; head+tail assembly did not need it (lookup correctness,
   not highlight geometry, was in scope).
4. **Acceptance:** plan Task 6 §2.1 — nine hovers, ground truth transcribed by eye *first*, and
   all three of: 9/9 resolved character, no duplicated/spurious run, no omitted character. Count
   only **seam-local** defects; ordinary OCR misreads are not failures of this criterion. **Not
   run** — items 1–2 above were built and unit-verified from a non-interactive environment with no
   real screen or Japanese on-screen text to hover; §2.1 needs a human at the keyboard.

**Do not re-enable `max_ocr_passes = 2+` by default until §2.1 passes.** Unchanged by this round:
the code-level bugs §2.1 would have measured are now fixed, but the sweep itself has not been
re-run, so the gate stays closed until someone does.

---

## 2. Vertical text: square-to-detect, transpose-to-read

**Measured 2026-07-28**, fix deliberately left to its own round (spec §5 says so).
**Evidence:** the vertical-text measurement of 2026-07-28, summarised below.

Vertical text at the shipped 500×100 region resolves the correct character **2 times in 6** and
twice returns a sentence spliced across four unrelated columns. A transposed 100×500 probe scores
6/6 characters and 5/6 exact column text.

**The measurement already rules out spec §5's candidate (b) as written:** "take a second
orientation-aware probe when pass 1's line looks vertical" cannot fire, because at the top of a
column pass 1 reports **Horizontal** — the 100 px band catches the top glyph of four columns and
`orientation_of` reads their spread as horizontal. Both square shapes reported Vertical at all
six points.

So the shape that *detects* best is a square and the shape that *reads* best is the narrow
transpose. Design the round around **square pass 1 → transposed second capture**, and note:

- `band_of` floors the perpendicular extent with `REGION_H`, i.e. floors a *width* with a
  *height* constant, so vertical tiles already come out 100×500. That is luck, not design, and is
  untested at any other text size.
  > **Updated 2026-08-09.** `band_of` now takes an explicit `short_floor` argument
  > (`src/text/layout.rs:107`) and the callers pass the *configured* short axis, so the
  > width-floored-by-a-height-constant accident is gone and the floor is tested at a second value
  > (`band_of_floors_on_the_configured_short_axis`). What this changes for the item: the shape is
  > now **tunable** — a user can already ask for a square pass 1 — so the round below is about
  > choosing the shape per pass automatically, not about making the shape settable at all. The
  > 2/6-vs-6/6 measurement is unaffected; it was never about who owned the constant.
- Pass 1's region is **not clamped to the monitor** (tiles are). Measured live: a hover at
  x=2696 produced a region starting at x=2446, 114 px onto the neighbouring monitor.

---

## 3. Memory under real popup rendering

**Open since M3; not re-measurable without a human at the keyboard.**

`watch` (capture → OCR → resolve → lookup, 417 hovers, 3-pass) plateaus at **37 MB WS / 15 MB
private**, flat, no handle growth. `run` idle with every window created is **12 MB WS / 2.6 MB
private**. But `run` under sustained *real* hovering adds the DirectWrite/D2D glyph path, and
that configuration recorded **94.8 MB WS / 60 MB private** at M3 — over the 50 MB target.

Synthetic input does not reach the app's `WH_MOUSE_LL` hook from an agent environment, so this
gap can only be closed by hovering by hand for a couple of minutes and reading Task Manager.
If it still misses, the DirectWrite glyph/font cache is the first suspect, not OCR.

---

## 4. Smaller carried items

- ~~**Executable icon.**~~ **SHIPPED** in the settings-GUI round: `assets/chibipop.rc` +
  `assets/chibipop.manifest` compiled once to a committed `chibipop.res`, linked by a `build.rs`
  that only emits `cargo:rustc-link-arg-bins`. Verified by extracting the icon back out of the
  built exe and locating `RT_MANIFEST` id 1 inside it. No `Cargo.toml` change was needed — Cargo
  runs a root `build.rs` by convention. `rc.exe` must still be run from PowerShell; MSYS2 mangles
  its `/flag` arguments under git-bash.
- **`TextSource::at` / `resolve_at` are unreachable *and* single-pass.** An M4 hazard: the UIA
  tier will come in through that trait and silently get the old behaviour.
- **Ruby hover on pass 1.** `nearest_line` keeps the tiled path from splicing furigana, but
  `hit_scan` on pass 1 will happily resolve a ruby character if the cursor is nearer to it than
  to the base text. Reproduced live at (3550,1450) → `ん` from `かんたん`.
- ~~**Text clipped by a window edge is unrecoverable**~~ at any capture shape. **DONE** — it is
  now a row in the README's troubleshooting table. It is a ceiling, not a defect.

---

## 5. Single-instance guard

**Raised 2026-07-28** by the adversarial review of the wheel-swallow feature. There is no
`CreateMutexW` or equivalent anywhere in `src/`, so two `chibipop run` processes install two
`WH_MOUSE_LL` hooks.

> **Partly closed 2026-07-29.** `lock::LibraryLock` now serialises the *library* across
> processes — a named `CreateMutexW` per library folder, held for the whole Apply — because two
> instances between them could delete every archive the user had. It does **not** guard the
> hooks: two `chibipop run` processes still install two `WH_MOUSE_LL` hooks, which is what this
> item was originally about. A startup-wide guard is still wanted; it can reuse `lock.rs`.

Two residual races the library lock does not cover, both non-destructive and both stated rather
than fixed:

- Another process calling `Library::load` while an Apply is in flight restores that Apply's
  quarantine, because `reconcile` cannot tell a live quarantine from one a killed process left
  behind. The archives survive; the removal is silently undone and the database no longer matches
  the library until the next Apply. Closing it wants an owner marker in `.removed/` plus a
  liveness check, not more locking.
- A quarantined file whose name is already taken at the top level is left in `.removed/` on the
  next load, as a duplicate of a live archive. Harmless, but it accumulates.

That was harmless before wheel capture existed. It is not now: the armed instance swallows wheel
events for everybody, and quitting the instance whose popup is visible does not necessarily fix it,
because the other one's arm is independent. One named mutex at startup, with a clear message and a
non-zero exit, closes it.

Related smaller items from the same review, all accepted-not-fixed and recorded in the spec's §8:

- The arm is stale for up to one 20 ms tick after the cursor leaves the popup.
- The arm survives a capture-guard hide, so a capture in flight while the cursor sits in the popup's
  rectangle leaves the wheel briefly dead with no popup visible. Gating on `IsWindowVisible` was
  considered and rejected — it would hand the wheel to the window underneath mid-read.
- `LLMHF_INJECTED` is unchecked, so synthetic wheel input is swallowed like any other. Deliberate:
  it is also the only reason an agent can verify this feature at all.

---

## 6. ~~Lookup ranking~~ — RETRACTED, and what was actually going on

**Raised and withdrawn 2026-07-28, same session.** Recorded rather than deleted, because the way it
was got wrong is more useful than the claim was.

**The claim:** hovering 振 in 振り向けた returned `振り` (match 2, freq 1 501, score 6.71) instead of
`振り向ける` (match 5, freq 37 505, score 5.38), so `score()`'s frequency term must be outweighing
`match_len`.

**Why it is false.** `lookup/engine.rs`'s final sort is `b.match_len.cmp(&a.match_len)` **first**,
with the score only a tiebreak after it. A 5-character match therefore cannot lose to a
2-character one, whatever their scores. The two numbers were real and the story around them was
plausible, and I wrote it down without reading the twenty lines that decide the order.

**What is actually reproducible.** Six consecutive captures at the same coordinate now return
`振り向ける` with `match=5` every time, and the hold span it produces (x=3007..3150, 143px) covers
**all five characters** of 振り向けた, releasing only at the 。 — the sticky-hold does exactly what
it should on that verb.

**The residue, which is real but is not new.** The one-off `振り` reading must have come from a
capture whose OCR text differed — the same variability already recorded elsewhere in this file (a
sweep across one glyph produced a hallucinated `冫` in the line tail). When OCR truncates a verb,
the match is short, so the hold span is short, so scanning it flickers again. That is a symptom of
OCR variability, not of ranking, and it needs no separate entry.

**Lesson worth keeping:** two numbers plus a coherent explanation is not a finding. The sort order
was one `grep` away.


---

## 7. The tray menu does not open on a real right-click

**Reported by oniichan twice, on 2026-07-28 and again on 2026-07-29 after a "fixed" claim from me
that was not.** Right-clicking chibipop's notification-area icon shows no menu at all.

### The distinction that cost a round

The menu **does** open when `WM_TRAYICON` with `WM_RBUTTONUP` is *posted to the window by hand*.
I took that as proof the tray worked and recorded it as verified. It is not: it proves the
**handler** is correct and says nothing about whether Windows **delivers** the callback from a real
click. Delivery and handling are separate failures and the test only exercised one.

### Established

- Not a pile-up of orphaned instances — exactly **one** process was running when it failed.
- The icon is **not** in the visible notification area. It sits behind the `^` chevron, in the
  overflow. Confirmed by dumping `Shell_TrayWnd` (this is Windows 10 LTSC, so the Win10 tray:
  `TrayNotifyWnd` → unnamed `Button` chevron at x=2293 → `SysPager` → `User Promoted Notification
  Area`, which holds no chibipop icon).
- `Shell_NotifyIconW(NIM_ADD)` must be succeeding, because `Tray::create` hard-fails otherwise and
  the app starts.

### The suspect, explicitly UNPROVEN

`src/ui/tray.rs` creates a dedicated `menu_owner` window (its own class, its own wndproc) and then
registers the icon against a *different* window:

```rust
let menu_owner = CreateWindowExW(WS_EX_TOOLWINDOW, owner_class_name(), ...)?;
let mut nid = NOTIFYICONDATAW { hWnd: hwnd, uID: TRAY_UID, ... };  // <- the POPUP, not menu_owner
```

`hwnd` is the popup: `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, and
hidden most of the time. That inconsistency is real and worth fixing regardless. **It is not
established as the cause** — do not "fix" it and declare victory without the log below.

Second candidate, independent of the first: the icon is registered with **version 0** semantics and
never calls `NIM_SETVERSION`. The modern contract is `NOTIFYICON_VERSION_4`, under which right-click
arrives as **`WM_CONTEXTMENU`**, not `WM_RBUTTONUP`, and the click coordinates come in `wParam`
rather than needing `GetCursorPos`.

### Why it was not simply tested

No agent-side path to a real right-click on that icon exists here, and each was checked rather than
assumed: `SendInput` from a tool shell returns **0**; `SetCursorPos` moves the pointer but fires no
`WH_MOUSE_LL`; the chevron rejects UI Automation's `Invoke` pattern with
`InvalidOperationException`; and the harness stringifies array-typed MCP parameters, so the
coordinate-taking click tools reject their own input. This is the tier-1/tier-2 boundary in
`docs/REGRESSION.md`, hit from a new direction.

### Next step — instrument, do not theorise

The mitigation is shipped and **confirmed by oniichan on 2026-07-29**, so this is not urgent:
settings open automatically at startup, `chibipop settings` reaches the same window with no tray at
all, and **`Quit chibipop` is a button in that window** — the tray menu now offers nothing that is
not reachable without it. Fixing the tray is therefore a correctness and polish item, not a blocker.

To actually fix it, log every message arriving in `app::run`'s loop with its `hwnd`, `message`,
`wParam` and `lParam`, run it, have oniichan right-click once, and read the log. That distinguishes
all three possibilities in a single click — nothing arrives (delivery), something arrives at an
unexpected hwnd (the `menu_owner` mismatch), or `WM_CONTEXTMENU` arrives and is dropped by the
`match` (the version-4 case). Only then change the code.

**Lesson worth keeping:** "I made the feature happen" is not "the user's action makes the feature
happen". Posting the message bypassed precisely the layer that was broken.

---

## 8. Outlined ("hollow") glyphs read at about half

**Measured 2026-08-08. Accepted as a limitation by oniichan, not built.** This is a ceiling of
the recogniser on one glyph *style*, not a defect in the capture or layout path — the control
below is what makes that a finding rather than an excuse.

Same sentence, same capture path, same engine, one variable changed:

| Source | Recall vs ground truth |
|---|---|
| Solid font, rendered on screen (Yu Gothic UI Bold 20pt) | **100.0%** at 1x, 96.2% at 2x |
| The reported image, outlined glyphs at 28-31px | **53.8%** at 1x, 46.2% at 2x |

Ground truth `すっかり気が抜け、ただの苦水と化した液体が喉を焼く。` (26 chars); best real
output `すっかり一け。ただの曹水と化した一候を《.`. The solid control returns the sentence
character-exact, so everything from capture through hit-scan is sound.

The glyphs are a thin dark contour around a white interior, on a white page. There is almost no
ink, and what there is encloses background rather than forming strokes.

**Two preprocessing ideas were tried and are REFUTED — do not re-try them blind:**

- 3x3 min filter, to grow the outline inward until strokes close: **23.1%**. Dense kanji merge
  into blobs; it destroys more than it fills.
- Downscale-average then upscale, to blend outline and interior into one stroke: **34.6%**.
  Washes out the little contrast there was.

Both variants were re-displayed and re-captured, so they carry one screen round trip the raw
number does not. Directionally clear; not a clean comparison.

**Reproduce** (portrait secondary, text on screen, `--region` wide enough for the whole line):

```bash
./target/release/chibipop.exe probe --at <x>,<y> --region 820,60 --upscale 1
./target/release/chibipop.exe probe --at <x>,<y> --region 820,60 --upscale 2
```

Score the `ocr line 0:` string against a ground truth transcribed **by eye first** — recall by
longest common subsequence, not "did more characters appear". Read `ocr line 0:` rather than
`line:`: `line:` only prints when hit-scan resolves, and scoring off it silently reports 0% for
a read that in fact succeeded. That cost a round.

**If picked up:** edge-aware fill of *enclosed* interiors (flood from the page border, keep what
it cannot reach), not blunt morphology. And gather a real sample of outlined-text images first —
one screenshot is not a sample, and this style is common in games and subtitles, which is
exactly chibipop's use case.

**Adjacent and unbuilt:** upscale 1 beat upscale 2 on **both** texts (53.8 vs 46.2 outlined,
100.0 vs 96.2 solid). `UPSCALE = 2` may simply be wrong once glyphs are already ~28px or larger.
It is one constant, but moving a global default on the evidence of one image is overfitting;
measure across several glyph sizes before touching it.

---

## 9. `chibipop settings` says "Apply & Restart" and restarts nothing

**Raised 2026-08-09 by the hot-reload branch. A product decision, deliberately not taken by the
agent that found it.** Not a regression: this wording predates the branch and is unchanged by it.

`chibipop settings` opens the settings window in its own process, with no `chibipop run` to talk
to. Its Apply writes `chibipop.toml` for the *next* start. The button nevertheless reads
**"Apply & Restart"** and the hint reads **"Applying saves your settings and restarts chibipop."**
Neither is true there — nothing is restarted, because there is nothing running to restart.

The design spec for this branch (§6, row 3 — a working note, not published with the repo, as with
item 1's sources) wants that third row to read **"Apply"** with a next-start hint instead, on the
grounds that `chibipop settings` edits the file for the next start and should say so. That is a
reasonable request and it is **not** what the code does.

### What is actually there

`apply_caption` / `apply_hint` (`src/ui/settings_window.rs:772`) key on
`ApplyMode` × *dictionary staged*:

| Opened by | Staged? | Caption | Hint |
|---|---|---|---|
| `run` (`ApplyMode::Live`) | no | Apply | "…uses them right away." |
| `run` | yes | Apply & Restart | "…restarts chibipop." |
| `settings` (`ApplyMode::Standalone`) | either | Apply & Restart | "…restarts chibipop." |

The branch made the `run` rows vary; it left `Standalone` exactly as it found it.

### Why it was left

Changing what a shipped button says to the user is oniichan's call, not an implementer's, and the
branch had no requirement that turned on it. It is also the *safe* wording to leave standing: it
over-promises a restart rather than under-promising one, so a user who follows it restarts
needlessly instead of wondering why nothing changed.

### If picked up

One arm of `apply_caption` and one of `apply_hint`, plus the three assertions at
`settings_window.rs:2502`. Decide the wording first — "Apply" alone is what the spec asks for, but
"Saved. Restart chibipop to use them." carries more information and is what
`docs/REGRESSION.md` 11b spent its whole life believing was there. Whatever is chosen, **11b's
table must move with it**; that item was wrong for months precisely because a caption and a
checklist drifted apart with nobody comparing them to the program.

---

## 10. `ScanDisplay::any()` has no callers

**Raised 2026-08-09. Trivial, and recorded only because nothing will ever tell you.**

`ScanDisplay::any()` (`src/geom.rs:90`) returns `captures || highlight`. Its last non-test caller
went away when the scan overlay stopped being created conditionally — the overlay is now created
unconditionally and shown on demand, so nobody needs to ask in advance whether *any* overlay might
be wanted. The only remaining reference is its own unit test at `src/geom.rs:366`.

**No warning fires**, and that is the durable point: `any()` is `pub`, and in a crate that is both
a library and a binary, `pub` items are part of the library's API, so `dead_code` never triggers.
The tier 0 clippy gate is exactly the mechanism that would normally catch this and it is
structurally blind to it. Any `pub` helper in this crate can quietly lose its last caller and the
gate will stay green — a unit test on a dead function looks identical to a unit test on a live one.

**Not removed here** because deletion is a code change and the branch that orphaned it was a
documentation task by then. Removing it is `any()` plus its test, and nothing else references it.
Worth a moment first on whether the *reverse* is wanted: if a future round wants to skip creating
the overlay again on some cheaper grounds, this is the predicate it would want back.

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
- ~~**`TextSource::at` / `resolve_at` are unreachable *and* single-pass.**~~ **DONE** — `TextSource`
  is deleted, and so is the `TextProvider` that replaced it. **Reworded 2026-08-19**: the seam
  moved one layer down, so the earlier proof of reachability — *"`src/app.rs`'s `resolve_trigger`
  calls it through the trait"* — no longer describes the code. What is true now: the trait is
  `text::recogniser::Recogniser` (`src/text/recogniser.rs`), `WindowsOcr` implements it, and the
  live OCR path calls it at both of its capture sites, `recognise_at_capture` and `words_in`
  inside `src/text/ocr.rs`. That is the part that actually proves reachable, not just the `impl`
  block existing. `src/app.rs`'s `resolve_trigger` now calls the multi-pass
  `resolve_at_tiled_scanned` concretely at both its call sites, which is what reaches that path.
  The single-pass `resolve_at` is deleted too — see item 34. `docs/REGRESSION.md` §1.24 is the
  human check that the swap changed nothing on screen, and it is **still owed**: it covers this
  re-cut as well as the one it was written for.
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

`apply_caption` (`src/ui/settings_window.rs:954`) keys on `ApplyMode` alone; `apply_hint` (`:959`)
keys on `ApplyMode` × *dictionary staged*:

| Opened by | Staged? | Caption | Hint |
|---|---|---|---|
| `run` (`ApplyMode::Live`) | no | Apply | "…uses them right away." |
| `run` | yes | Apply | "…rebuilds your dictionary." |
| `settings` (`ApplyMode::Standalone`) | either | Apply & Restart | "…restarts chibipop." |

The hot-reload branch made the `run` rows vary; it left `Standalone` exactly as it found it, and
so did everything since. **Row 2 changed on 2026-08-13**: v0.7.2 stopped restarting after a
rebuild, so `run` no longer promises one — `apply_caption` lost its `staged` parameter and the
`run` caption is now always "Apply". `REGRESSION.md` 11b's table moved in the same commit, which is
what the "If picked up" note below demands of any change here. **This item is unaffected**: it has
always been about `Standalone`, which still says "Apply & Restart" and still restarts nothing.

### Why it was left

Changing what a shipped button says to the user is oniichan's call, not an implementer's, and the
branch had no requirement that turned on it. It is also the *safe* wording to leave standing: it
over-promises a restart rather than under-promising one, so a user who follows it restarts
needlessly instead of wondering why nothing changed.

### If picked up

One arm of `apply_caption` and one of `apply_hint`, plus the three tests under
`// ---- apply caption ----` (`settings_window.rs:2962-2987`). Decide the wording first —
"Apply" alone is what the spec asks for, but
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

---

## 11. ~~The settings window is height-constrained and cannot scroll~~ — DONE

> **DONE 2026-08-19**, built across the scrollable-settings-window plan (commits `17cc8ea` spike,
> `75c8f0a` viewport, `2eedf8b`+`db95d84` reparenting, `9f59d03` scrollbar, `4b87873` pinned button
> row). The window now carries a clipping viewport and a scrollable content pane under a fixed
> bottom row — the first "If picked up" option below — **plus** the bottom row is pinned to the
> window's real client bottom (`place_bottom`) rather than laid out at the cross-tab `max()`'s own
> `y`, so `fit_to`'s clamp now costs only visibility, recovered by scrolling, never the ability to
> reach Apply. The window's *target* height is still the cross-tab `max()` — that deliberately did
> not change, since that is the number `fit_to` clamps in the first place. The
> `y = y_general.max(y_dict).max(y_ocr).max(y_ank)` line this item cites has moved twice since:
> **`settings_window.rs:2169`** here, corrected to **2170** while this plan was scoped, and now
> **`settings_window.rs:2555`** (`+ CONTENT_Y` was folded in along the way) — the file keeps
> growing under a change like this, so re-grep rather than trust either number.
>
> **Two more of this item's own figures were wrong, both corrected by measuring a running window,
> not by arguing with the table below.** The table gives Dictionaries as 364 clean / 448 worst
> case; a real run measured **412**, because only the `library_empty` branch was firing on that
> machine — 364 + 48 (that branch's cost) = 412 exactly, and the further +36 the stale-entries
> branch would add reaches the table's own 448. The three figures agree; 412 is just a real machine
> caught between the table's two extremes. And the margin the clean-config figure implies against
> the governing 426 (426 − 364 = **62**) is not what a real window shows: measured directly against
> the real Dictionaries figure, it is **426 − 412 = 14px** — a materially thinner cushion than the
> table alone would suggest.
>
> **"The two want deciding together", below — resolved, see item 12.** Both items closed in this
> same round, and item 12's own closure found the coupling this sentence feared never held: its
> fix spent none of this item's headroom.
>
> Tasks 4-6 verified each of their changes with `chibipop settings --audit` (Task 0's tool, added
> mid-plan; see [`REFERENCE.md`](REFERENCE.md)) — a JSON dump of the window's control tree, diffed
> before and after. Tasks 1-3, which predate that tool, each built and threw away their own
> harness instead.

### Why `ScrollWindowEx` was rejected

The first attempt (`17cc8ea`, a throwaway spike, never shipped) tried the obvious Win32 primitive
instead of a clipping parent, so nobody has to re-run the experiment to learn why it does not work
here.

- **`SW_SCROLLCHILDREN` moves hidden children too, not only the visible ones.** Every scroll
  displaced every tab's controls, including tabs `SW_HIDE`n at the time. After three scrolls of
  General, switching to Dictionaries showed it **120px out of place**, its list box painted
  straight over the tab strip — and switching back left the strip itself overpainted and
  unclickable.
- **`prcClip` clips the blit, not the children.** A child scrolled above the clip band keeps
  drawing at its new position regardless, over the tab strip, because the flag only limits what
  `ScrollWindowEx` itself paints, not where a repositioned child is free to paint on its own.
- **Neither is fixable without a clipping parent** — a real window whose client area *is* the
  scrolling band, so its children are clipped for free and cannot paint outside it. That parent is
  the viewport this item shipped with.

**Raised 2026-08-11 by the per-character-retrigger / OCR-language branch, which nearly tripped it
and was rerouted instead of fixing it.** Not a regression — this ceiling predates the branch and
the branch ships clear of it. It is recorded because the next tab that grows will hit it, and
nothing will warn.

### The mechanism

Three facts compose into the problem:

- **The bottom block sits at the tallest tab.** `settings_window.rs:2169` is
  `y = y_general.max(y_dict).max(y_ocr).max(y_ank)`, and the Apply / Cancel / Quit row is placed
  from that `y`. So a tab growing taller pushes the buttons down on **every** tab, not just its own.
- **`fit_to` clamps to the work area** (`settings_window.rs:1738-1740`): `outer_h.min(cap)`, where
  `cap` is `work_area_height`. When the content is taller than the screen allows, the window is
  silently made shorter than its content.
- **There is no scrolling.** `grep -c "WM_VSCROLL\|SetScrollInfo" src/ui/settings_window.rs`
  returns **0**. (The `WS_VSCROLL` flags in that file are all on combo boxes — dropdown
  scrollbars, not a scrollable window.) So the clamp does not hide content behind a scrollbar; it
  truncates it, and what is at the bottom is the row containing **Apply**.

That failure has shipped once already. The comment at `settings_window.rs:1122-1128` records the
first version of the file passing a hand-tuned height to `CreateWindowExW` — which takes the
*outer* size — so 39px of caption and frame ate Apply and Cancel and the window opened with no way
to accept anything. Measuring the content fixed *that* cause. It does not protect against this one.

### What was measured

The branch's **first attempt** put the checkbox on the General tab, which took the governing
tallest-tab figure **426 → 484** and the client height **594 → 652 logical px** on every tab. That
attempt was discarded; what shipped is the last row below.

Tab heights, walked from the layout constants — `content_y` = `PAD` + `TAB_H` + 4 = 46, then group
by group:

| | `y_general` | `y_dict` | `y_ocr` | `y_ank` | governing `max()` | client height |
|---|---|---|---|---|---|---|
| v0.6.0 | **426** | 316 (400 worst case) — *stale, see below* | 280 | 260 | **426** | **594** |
| Task 7's first attempt — **discarded, never shipped** | **484** | 316 (400 worst case) — *stale* | 328 | 260 | **484** | **652** |
| shipped v0.7.0 | **426** | 316 (400 worst case) — *stale* | **380** | 260 | **426** | **594** |
| shipped v0.7.1 and v0.7.2 — **re-walked 2026-08-13** | **426** | **364** (**448** worst case) | **380** | 260 | **426**, or **448** with both warnings | **594**, or **616** |

**Only `y_ocr` moved between v0.6.0 and v0.7.0**; the other four columns are identical across the
first three rows, which is exactly why the governing `max()` and the client height did not move.
The bottom block and padding add a constant 168px below the governing `max()`, which is why every
row is internally consistent (426 + 168 = 594; 484 + 168 = 652; 448 + 168 = 616). 484 only ever
existed on the discarded attempt.

> [!warning] Corrected 2026-08-13 — the `y_dict` column was stale, and it is the column that
> governs the worst case
> Re-walked from the layout constants: the Dictionaries tab is **364** with no conditional warning
> and **448** with both, not 316 / 400. The three historical rows are left as they were written,
> because nothing re-measured them at the time and inventing a date for the drift would be worse
> than marking it; the figure was already stale before the two-box rewrite, and that rewrite did
> **not** move it — `dict_group_h()` is 218 both before and after, so 364 / 448 holds across v0.7.1
> and v0.7.2 alike.
>
> **The consequence is that 426 is not always the number to budget against.** With both conditional
> warnings visible, `y_dict` (448) exceeds `y_general` (426), *Dictionaries* becomes the governing
> tab, and the client height is **616** rather than 594. That is 22px worse than the "headroom is
> ~0 at 150%" estimate below assumed, and it is the state `REGRESSION.md` §1.17's layout paragraph
> asks a human to reproduce.

- **On oniichan's machine this is not a problem and would not have been.** 96 dpi / 100% scaling,
  work area 2560x1050. The ~702px figure measured during the branch was against the **652** client
  height of the discarded attempt; on the same ~50px non-client allowance the shipped 594 needs
  ~644. Either way, enormous headroom against 1050.
- **At 150% on a 1080-tall laptop the margin is roughly zero**, and at **175% it exceeds the clamp
  for certain — as it already did before this branch**, at 594. The non-client and taskbar figures
  in that estimate are approximations and the margin sits inside their uncertainty, so the honest
  claim is "headroom is ~0 at 150%", not "broken today".

The checkbox was moved to the OCR / Debug tab instead, returning the governing figure to exactly
its pre-branch **426** and the client height to 594. The OCR tab absorbed the growth as
`y_ocr` **328 → 380** (+52: the checkbox's 24px row plus a 28px explanatory static) — 328 being
v0.6.0's 280 plus the 48px the language dropdown and its caption had already added — which stays
46px clear of the governing 426 — so OCR did **not** become the tallest tab and the bottom block
did not move. That is a **reroute, not a fix**: the next tab to grow inherits the whole problem.

### If picked up

Either make the window scrollable (a scrollable content pane under a fixed bottom row, so the
clamp costs visibility rather than the ability to accept), or stop laying the bottom row out at the
cross-tab `max()` and pin it per-tab. The first is the real fix; the second is cheaper and removes
the "one tab grows, every tab pays" coupling that makes this so easy to trip.

**Note the interaction with item 12** — that fix makes the General tab ~38px taller, which spends
headroom this item says is already thin at 150%. The two want deciding together.

**Resolved 2026-08-19.** Both were closed in the same round. Item 12's own closure measured the
coupling this paragraph predicts and found it did not hold — see the DONE note at the top of this
item and item 12's own closure below.

---

## 12. ~~The General tab's "Popup" group box is 32px too short for its contents~~ — DONE

**Present in shipped v0.6.0. Spotted 2026-08-11 by the per-character-retrigger / OCR-language
branch and correctly left alone — it was out of that branch's scope and is purely cosmetic.**

The `Popup` group box on the General tab is drawn 238px tall but encloses 270px of controls, so the
fourth checkbox — **`Hide the popup from screen capture`** — draws entirely **below its own frame**,
and the third (`Show related words beside the popup`) has its bottom 8px clipped by it too.

> **DONE 2026-08-19**, commit `e0a5c09` (`fix(ui): make the Popup group box tall enough for its
> four checkboxes`), `settings_window.rs:2245` (not the `:1600` cited below — the file has grown):
>
> ```rust
> // before
> gen.push(group_start("Popup", y, 5 * (ROW_H + ROW_GAP) + 3 * ROW_H + 16)?);
> // after
> gen.push(group_start("Popup", y, 5 * (ROW_H + ROW_GAP) + 4 * ROW_H + 30)?);
> ```
>
> Height **238 → 276**, exactly this item's own "If picked up" arithmetic below. Proven by rects
> from `chibipop settings --audit`, not just arithmetic: the fourth checkbox (id 110) sits at
> `y=384, h=24`, bottom **408**, unmoved by the fix. The group box now bottoms at **414**
> (`y=138, h=276`) — the checkbox sits **6px inside** the frame, where it used to sit **32px past**
> it (old frame bottom 376 against the same 408 — this item's own title). The third checkbox moves
> from 8px clipped to 30px inside, matching the "8px clipped" claim above exactly.
>
> **This item's central prediction, below, did not hold — say so plainly.** It claims this fix
> "lands squarely on item 11", adding ~38px to the General tab and spending item 11's headroom.
> The audit diff between the two builds is **ten lines total, all `"h": 238` → `"h": 276` on this
> one group box**, once per dump, across all five dumps — nothing else in any hunk. `group_start`'s
> height argument only sizes the drawn `BS_GROUPBOX` rectangle; every control's position comes from
> the hand-threaded `y` counter, which never reads that height back. So the frame can grow to match
> its own contents without moving `y_general`, any checkbox, or anything on another tab. **Items 11
> and 12 shared a piece of reasoning that turned out to be false**: they did not, in the end, need
> deciding together, and this fix shipped spending none of item 11's headroom. See item 11's own
> DONE note.

### The arithmetic, so nobody has to re-derive it

`settings_window.rs:1600` declares the height as a formula that literally counts **three**
checkboxes:

```rust
group_start("Popup", y, 5 * (ROW_H + ROW_GAP) + 3 * ROW_H + 16)?   // = 238
```

With `ROW_H = 24` and `ROW_GAP = 6`, the content the group actually spans is 20px of top inset,
five combo rows (Theme, Font, Max width, Max height, Summary — the last carrying an extra 4px) at
154px, then **four** checkboxes at 24px each:

| | |
|---|---|
| Declared frame height | `5*(24+6) + 3*24 + 16` = 150 + 72 + 16 = **238** |
| Content actually enclosed | 20 (top inset) + 154 (five combo rows) + 4×24 (checkboxes) = **270** |
| Shortfall | **32** |

So a fourth checkbox was added at some point and the `3 * ROW_H` in the formula was never moved to
`4 * ROW_H`. Nothing catches this: the group box is a `BS_GROUPBOX` with no layout relationship to
the controls it visually contains, so the frame and its contents are free to disagree forever.

### If picked up

Changing `3 * ROW_H` to `4 * ROW_H` gets to 262, which is still 8px short of the content. The house
bottom inset in this file is **6px** — the `Trigger` group is 80px against 74px of content — so the
matching height is **276**, i.e. **+38px** on today's 238.

**That +38px lands squarely on item 11.** It makes the General tab, already the tallest, taller
still, and General is what sets the bottom row's position on every tab. Fix the scrolling or the
per-tab layout first, or fix both in one round; do not spend the headroom without looking at it.

**Resolved 2026-08-19 — this did not hold.** See the DONE note above: the audit diff proved growing
this frame moves nothing else, on this tab or any other. Item 11 was fixed in the same round anyway
(the real fix, a scrollable viewport), so the question this paragraph raises never had to be
answered on its own terms.

---

## 13. The startup OCR-language fallback is a pre-check, not a retry — and it is silent

**Added 2026-08-11 by the v0.7.0 fix wave.** Not a defect; this records what the fallback does and
does not cover, and why it is shaped the way it is, because both limits are invisible from the
diff.

### What it does

`worker_main` (`src/app.rs`) now asks `recogniser_available` before building the engine. If the
configured language has no recognizer **and** it is not already the default, it prints a line and
starts on `default_ocr_language()` (`"ja"`) instead of returning `Err` from `run`. Without it,
picking a second language from the new v0.7.0 dropdown and later removing that language pack made
chibipop **do nothing at all** on the next launch: `main.rs:100` has already hidden the console by
then (`ui/console.rs:51`), so the error goes to a hidden window, and a double-click launch never
reaches the settings window — there is no in-app way back.

### Limit 1 — it cannot be a retry, because DPI awareness is a one-shot

The obvious shape is "try the configured language, and on failure retry with the default". **That
cannot work in this process.** `OcrTextSource::new` opens with `init_dpi_awareness()`, and
`SetProcessDpiAwarenessContext` fails once a process's awareness has been established — the
`assets/chibipop.manifest` comment already records this, which is why the manifest deliberately
omits `<dpiAware>`. Measured directly:

```
first=Ok(()) second=Err(setting per-monitor DPI awareness
Caused by:
    Access is denied. (0x80070005))
```

So a second `OcrTextSource::new` in one process fails at the DPI step whatever language it is
handed, and a retry-shaped fallback would have looked correct in review and never once produced a
working engine.

The pre-check also matches the reload path, which gates on the same `recogniser_available` before
rebuilding — so startup and Apply now answer "is that pack there?" the same way.

### Limit 2 — "installed but will not build" is still fatal at startup

`recogniser_available` answers *is the pack listed*, not *will the engine build*. A language that
is listed yet fails `OcrEngine::TryCreateFromLanguage` still aborts startup exactly as before. The
reload path has the same split and handles it (`ocr.rs:277`, "recogniser failed, keeping ..."),
because there it has a working engine to keep. Startup has nothing to keep, and cannot call `new`
again. Covering it needs `init_dpi_awareness` split out of `new` — a separate change with the
capture path in its blast radius.

### Limit 3 — the substitution notice is as invisible as the error it replaced

The `eprintln!` lands on the same hidden console. Someone who double-clicks chibipop sees it come
up reading Japanese with no explanation of why their Korean stopped working; only a launch from a
terminal shows the line. **The fallback fixes "does nothing", not "says nothing".** If picked up:
the honest surfacing is a one-shot notice on the popup or a `MessageBoxW` at startup, and the same
question applies to every other `eprintln!` on the startup path.

### Covered by tests

`startup_language` — the keep/substitute decision — is pure and unit-tested for all four
outcomes, including that the default language never substitutes itself and never queries WinRT.
The wiring (that `worker_main` builds with the language the decision returned) is **not** tested;
it needs a real `OcrEngine`. Tier 1 **§1.16** is where that gets exercised by hand, and it has
**not been run** — so as of 2026-08-11 the wiring is witnessed by nothing at all. This line used
to point at §1.15, which does not exercise the startup path: §1.15 is the *reload* path, and no
step for the startup fallback existed anywhere until §1.16 was written on 2026-08-11.

---

## 14. `stale_order_entries` does not look at `per_language`

**Raised 2026-08-12 by the per-language-dictionary-lists branch. The cheapest real improvement
available to that feature, and deliberately not taken in v0.7.1.**

`stale_order_entries` (`src/settings.rs:432`) walks `cfg.dictionaries.display_order` and returns
the entries matching no installed dictionary, so the settings window can name the one that would
otherwise just sort last. It never reads `cfg.dictionaries.per_language`, which means a typo in a
per-language list — or a dictionary named before it was imported — is reported by nothing at all.

**What that costs is two documented behaviours staying silent rather than visible.**
`docs/REFERENCE.md` under `per_language` has to tell users that a list naming nothing installed is
ignored, and that a hand-written entry naming a not-yet-installed dictionary is overwritten by the
next Apply. Both are only *surprising* because there is no warning; a stale-entry notice naming the
unmatched string would turn each from a silent surprise into a visible one, without changing any
behaviour.

**If picked up:** the function already takes `&[DictInfo]`, so the extension is to fold the map's
values into the same filter and return the language tag alongside the entry — the caller renders a
string, so the return type is the only real decision. Note that the per-language case has a
legitimate transient the `display_order` case does not: a list is stale for the whole window
between configuring it and importing the dictionary, so the notice must read as informational
rather than as an error.

---

## 15. The settings window's `unreadable` set is not refreshed after a library rebuild

**Raised 2026-08-12 by the per-language-dictionary-lists branch. Narrow, no data loss, recorded
because the symptom is a button that looks wrong rather than anything that fails.**

The settings window captures which archives are unreadable when the window opens, and does not
recompute it after a library rebuild. Import a corrupt archive mid-session and it is treated as
readable until the window is reopened — the row's own state is stale, not the list's.

**The consequence is bounded to cosmetics**, because the reader and the writer share the one stale
set: the move handler and the button greying both classify rows through the same cached
`unreadable` — they now share one predicate, `dict_move_target` — and `read` hands that same cached
set to `apply_to`, which keys the list through it. The two therefore agree with each other even
while both are stale. Nothing is written wrong and no list is emptied; the empty-list guard catches
the consequence that would have mattered.

**Updated 2026-08-13, and the trigger is now wider, not narrower.** This item used to say the
reachable trigger was a rebuild that *fails* and leaves the window open, "an import that succeeds
ends in a restart, which reopens the window and recaptures the set". v0.7.2 removed that restart:
a **successful** import now leaves the same window open with the same stale set, so both outcomes
reach it. The window is also not re-rendered after a rebuild at all, which is the same gap seen
from the other side. It stays cosmetic — the greying and the guard cannot disagree any more, since
the two-box rewrite gave them one shared predicate, so the old symptom (a button that looked
enabled and did nothing) is gone and what remains is a row classified from an out-of-date reading
of disk.

**If picked up:** the fix is to recompute the set at the same point the rebuilt library is handed
back to the window, not to make the button smarter — the greying and the guard disagreeing is the
actual defect, and there is one place where the two inputs are both in scope.

---

## 16. The dictionary filter trusts `recogniser_available`, not the engine that was built

**Raised 2026-08-12 by the per-language-dictionary-lists branch review, inside the fix that closed
the missing-pack half of it. Narrow: it needs a tag Windows lists but will not build.**

`resolve_dict_filter` (`src/app.rs:2386`) honours a language's list only when
`configured_recogniser_runs` (`:2379`) says the configured tag will really run — the same
`startup_language` + `recogniser_available` decision the worker makes. That closes the reachable
case: with the pack uninstalled the worker substitutes `ja`, and without the guard the popup would
filter Japanese hits through the Chinese list and come back **empty**.

**One case it does not close.** `recogniser_available` can report a tag as installed while
`make_engine` still fails on it; `apply_settings` then prints
`chibipop: <tag> recogniser failed, keeping <tag>` and holds the previous engine
(`src/text/ocr.rs:291-294`). The filter was resolved against the configured tag before that, so OCR
runs one language while the lookups are scoped to another's list — the empty popup again, one
layer down. `docs/REGRESSION.md` §1.16 already names this engine-will-not-build case as uncovered.

**If picked up:** the honest fix is for the worker to report the tag it ended up with, which is
exactly the cross-thread plumbing this round declined to add — a field on the startup message and
on the reload path, plus somewhere on the main thread to hold it. Anything cheaper is another
mirror of a decision taken on the other thread, which is what this item exists to warn about.

---

## 17. The main-thread WinRT probe relies on an undocumented `windows-core` fallback

**Raised 2026-08-12 by the re-review of the per-language-dictionary-lists fix wave, which proved
the current behaviour by execution rather than by reading. Not a defect today.**

`configured_recogniser_runs` (`src/app.rs:2379`) calls into WinRT from the **main** thread, at
startup and on Apply. `RoInitialize` appears exactly once in the tree (`src/text/ocr.rs:271`) and
runs only on the **worker** thread; apartment initialisation is per-thread, so the main thread's
apartment is never initialised by us.

It works anyway because `windows-core`'s `load_factory` falls back to `CoIncrementMTAUsage` when a
factory lookup returns `CO_E_NOTINITIALIZED`, then retries. Measured in a process where no thread
ever called `RoInitialize`: `AvailableRecognizerLanguages()` returned `Ok(size=4)` in 3.77 ms and
`recogniser_available("ja")` was true in 284 us — the same as after `RoInitialize`. The probe is
also double-short-circuited, so a config with no `per_language` never reaches WinRT at all.

**Why it is worth writing down.** If a future `windows` bump drops that fallback, the probe starts
answering "the configured recogniser will not run" for every tag, and `resolve_dict_filter` then
declines to apply **every** per-language list. That fails safe — everything is searched — but it is
silent: no error, no stderr line, and the release's headline feature simply stops doing anything.
Nothing in the test suite would catch it, because the tests exercise the pure resolver rather than
the probe.

**If picked up:** either initialise the main thread's apartment explicitly at startup and stop
depending on the fallback, or have the probe report failure distinguishably from "not installed" so
the silent path becomes a visible one. The second is cheaper and is what makes the failure
diagnosable.

---

## 18. Two dictionaries with the same name cannot be told apart by anything the config can say

**Raised 2026-08-13 by the rebuild-promotion round. Observed, not hypothetical: the staged database
built on the user's machine that day held six dictionaries, and 白水社中国語辞典 was one of them
twice.** Nothing refused the second import and nothing said a word.

**Every ordering and scoping rule in the program keys on the name.** `dict_order_rank`
(`src/present.rs:189`) lower-cases the dictionary's name and asks whether any `display_order` entry
is a **substring** of it; `sorted_by_order` (`src/settings.rs:246`) sorts on that rank and breaks
ties with `dict_id`. So one entry matches both duplicates, at the same rank, and their relative
priority is settled by the order their archives happened to be built in. `any_listed`
(`src/present.rs:198`) collapses them the same way, so a `per_language` list cannot include one and
exclude the other either.

**`keyed_names` can manufacture the collision out of two names that differ.** It cuts each name at
its first `[` or `(` before writing it (`src/settings.rs:318`) — the deliberate fix for
`Jitendex.org [2026-07-09]` renaming itself on every rebuild — so two builds of one dictionary
whose full titles differ only by a date stamp key to the same string.

**The library never checks.** `reconcile`'s stray-adoption loop keys on the archive's **file** name
(`src/library.rs:79-96`), so two different files carrying the same `index.json` title are two
ordinary entries. In the Dictionaries tab they are two identical rows, and the only gesture that
distinguishes them is **Remove**, which acts on the row rather than on the name.

**What it costs is bounded but real:** no data loss and nothing silently unsearched — both copies
are in the database and both are reachable — but the user cannot order them, cannot scope one away,
and cannot tell which row is which.

**If picked up: there is no obviously right key, and that is the interesting part.** `dict_id` is
assigned by filename order at build time, so it moves whenever the library does; the archive
filename is stable but the codebase deliberately keeps filenames out of the config
(`a filename must never reach display_order`, `src/settings.rs`). The cheapest honest step is
therefore to **detect and report** the collision rather than to re-key anything — the settings
window already has somewhere to put a stale-entry notice — so the user can rename or remove one
archive instead of wondering why one of two identical rows will not move on its own. Note the
interaction with item 14: both want the same warning surface.

---

## 19. A quarantine that outlives its transaction comes back at the bottom of the order

**Raised 2026-08-13 by the rebuild-promotion round, which found it while tracing why a failed
promotion left `.removed/` populated. Not the bug that round fixed — a consequence one layer down,
and still open.**

`Library::load` calls `reconcile` (`src/library.rs:71`), which calls `restore_quarantined`
(`:278`) **unconditionally, on every load, in every process** — the behaviour is deliberate and
pinned by `a_quarantine_left_by_a_dead_process_is_restored_on_load` (`:488`). `Pending`
(`:197`) is in-memory only: nothing on disk records that a removal is halfway through.

So an interrupted removal is undone, which is the intent — but it is undone **badly**.
`library.json` was written without those archives, so when they reappear on disk `reconcile`'s
retain pass drops nothing and the stray-adoption loop (`:79-96`) re-adopts them by `push`, at the
**end** of `entries`. The dictionaries come back last in the order, with no message anywhere. The
user's priority is silently rewritten by a failure they were told nothing about.

**v0.7.2 narrowed the window without closing it.** The promote and the commit-or-rollback are now
one synchronous block with no restart in the middle, so an ordinary failed promote rolls the
quarantine back properly rather than leaving it for the next load. What remains is a process that
dies inside that block — and the general fact that the restore is unconditional in every process,
so the recovery path is reached without anyone deciding it should be.

**v0.8.0 narrowed it further, again without closing it.** There is no promote left; the quarantine
now sits open only for the duration of the edit itself — `Pending::commit` runs immediately after
the last transaction and the last `library.save` (`src/app.rs:668-669`), and that whole span is
measured in hundreds of milliseconds rather than the minutes a full rebuild took. A process that
dies inside *that* window still comes back with its archives re-adopted at the end of the order,
silently, exactly as described above.

**If picked up:** the fix is to make the restore *re-rank* rather than re-adopt — the manifest still
holds the surviving entries' order, and a restored archive's original position is recoverable if
`quarantine` records it. Failing that, the honest cheap version is to say so: one stderr line
naming what was restored, so a silently reordered library becomes a visible one. Do **not** make
the restore conditional without replacing it with something on disk; the unconditional restore is
what stops a killed process from eating someone's 200 MB download.

---

## 20. Four comments cite design documents that do not exist

**Raised 2026-08-13 across the rebuild-promotion round's tasks. Trivial to fix, recorded because it
is the kind of thing that reads as authoritative until someone tries to follow it.**

Four comments in `src/app.rs` cite a numbered decision or contract:

| Site | Comment |
|---|---|
| `:754` | `// Contract 3: DPI before GDI.` |
| `:769` | `// Contract 2: report all three.` |
| `:1693` | `// Shutdown, decision 5's order.` |
| `:1780` | `// Decision 2: read once.` |

**No such document is in the repository.** Grepping `docs/` and `README.md` for `Contract 2`,
`Contract 3`, `Decision 2` and `decision 5` returns nothing. They point at a working spec that was
never published — `.superpowers/` is gitignored — so a reader cannot check whether the cited rule
still holds, or even what it said.

**The house comment limit is what makes this a trap rather than a nit.** Comments are capped at 30
characters, which is exactly enough room to cite a document and not enough to state the rule, so
the citation habit is a pressure the standard creates. Two of the four survive the removal of the
citation with their meaning intact — "DPI before GDI" and "report all three" say something
checkable. The other two say nothing at all once the phantom document is taken away.

**If picked up:** delete the citations and keep the assertions, writing an assertion where there is
none — `// Decision 2: read once.` sits above the worker's one `dict.dicts()` call, and the
shutdown comment sits above a `KillTimer` that precedes the worker join, so in both cases what the
comment is *for* is legible from three lines of context and the citation adds nothing. Anything
that genuinely needs more than 30 characters to explain belongs in `docs/`, where it can be cited
by name.

---

## 21. `settings_only`'s message loop spins at 100% CPU on a `GetMessage` error

**Found 2026-08-13 while root-causing the quit freeze. Not that bug — filed because it is a real
hang and the two loops in this file already disagree about it.**

`src/app.rs:234` is the settings-only path's pump:

```rust
while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
```

`GetMessage` has **three** return values, not two: `> 0` for a message, `0` for `WM_QUIT`, and
**`-1` on error**. `BOOL(-1).as_bool()` is `self.0 != 0`, so an error is **true** and the loop keeps
calling `GetMessage` on a queue that has already failed — a 100%-CPU spin with no exit.

`run` gets this right at `src/app.rs:987`, which reads the value once and breaks on `<= 0`. The
same file therefore contains both the correct and the incorrect reading of the same API, three
hundred lines apart, which is what makes this worth a number rather than a passing note.

**If picked up:** give `settings_only` the same shape as `run` — bind the `BOOL`, `break` on
`got.0 <= 0`. No test seam: `GetMessage` cannot be made to return `-1` from a unit test, and a test
over `as_bool()` would only restate the `windows` crate.

---

## 22. `settings_only` has no way out while a rebuild is running

**Found 2026-08-13 alongside 21. The severity is entirely about which path you are on.**

`src/app.rs:279-281` discards `window.take_outcome()` for as long as `rebuild.is_some()`, exactly
as `run` does. In `run` that is safe, because the tray icon is a second, ungated exit
(`TrayCommand::Quit` sits on a different message and is not behind the busy check). **`settings_only`
has no tray.** It is the path taken when there is no database to open at all, so if the builder
child ever wedges — a stalled archive read, a full disk, a network path that stops answering — the
only control the user has left is Task Manager.

The window is also `set_busy(true)` for the whole rebuild, so `ID_QUIT` is greyed; the X produces
an outcome that the same three lines throw away.

**If picked up:** the cheap version is a Cancel that kills the child (`Child::kill`) and rolls the
`Pending` back, which is a real feature rather than a one-line fix — hence the number. The honest
interim is a documented note that the settings-only rebuild cannot be interrupted.

**Amended 2026-08-16, v0.8.0 — still open, and now the *only* place it happens.** The phrase
"exactly as `run` does" above is no longer true: `run` has no rebuild to gate on, so its copy of
those three lines is deleted along with the rest of the rebuild-and-promote path. `settings_only`
keeps its rebuild (first run genuinely needs one) and keeps this gate, so the item stands unchanged
in substance and narrows in scope to the one path that has no tray icon to escape through.

---

## 23. Quitting during a rebuild orphans the `build-dict` child

**Found 2026-08-13 alongside 21 and 22. This one keeps burning the user's machine after chibipop is
gone, which is why it is filed even though it is not a hang in chibipop itself.**

`src/app.rs:1727` ends `run` with `std::process::exit(0)`. That does not run destructors and does
not touch child processes, so the `chibipop.exe build-dict` spawned at `src/rebuild.rs:59-71` keeps
running: it keeps writing `<db>.new.tmp`, and it keeps its CPU and disk load. On the 400 MB library
on this machine that is **minutes of invisible work by a process the user believes they closed**,
with no window and no tray icon to find it by.

It is worth noting explicitly that this makes the orphan an **independent candidate for "chibipop
slowed my machine down"** in any incident where a rebuild had actually started — a different
mechanism from the unserviced input hooks fixed on 2026-08-13, and one that outlives the process
rather than ending with it.

The same `exit(0)` also drops the in-flight `Pending` without `rollback()`, leaving the user's
archives in `library/.removed`. That half **self-heals**: `Library::load` → `reconcile` →
`restore_quarantined` (`src/library.rs:278-290`) puts them back on the next launch. See item 19 for
what the restored order looks like.

**If picked up:** the child handle would have to outlive `InFlight` far enough for the shutdown
block to `kill()` and `wait()` it before `exit(0)`. Note that killing a builder mid-write leaves a
partial `<db>.new.tmp`, which is already handled — `rebuild::run` removes an existing `tmp` before
starting (`src/rebuild.rs:55-57`).

**Amended 2026-08-16, v0.8.0 — mostly, but not entirely, overtaken.** `run` no longer spawns
`build-dict` at all: dictionary changes edit the live database on a background thread and the whole
rebuild-from-`run` path is deleted, so the scenario as written — quit the tray icon while `run`'s
rebuild child is working — **is unreachable**. `std::process::exit(0)` is still there
(`src/app.rs:1815`, moved from 1727) and still runs no destructors, so the *general* hazard stands
wherever a child does exist: that is now `settings_only` only, where item 22's gate means the user
cannot reach Quit mid-build anyway and Task Manager is the only exit. The `.new` staging this item's
last paragraph described is gone with the rest; `rebuild::run`'s `<out>.tmp` is what a killed
builder leaves now.

---

## 24. The `join()` deadlock was never fixed — only made unreachable

**Raised 2026-08-16 by the v0.8.0 incremental-dictionary round. Read this before reintroducing any
`JoinHandle::join()` on the worker. It is filed as a backlog item rather than a note because the
underlying defect is still there and nobody has diagnosed it past the symptom.**

**What happened.** v0.7.2's rebuild promoted a staged database by stopping the worker, `join()`ing
it to prove its SQLite handle had closed, renaming, then respawning. The join never returned. The
trace is unambiguous: the worker logged `worker.thread.end` — its closure *completed* — and
`join()` still blocked, while the main thread pumped **zero** messages for the rest of the run. At
the time, the main thread owned `WH_MOUSE_LL` and `WH_KEYBOARD_LL`, and Windows serialised every
mouse move and keystroke on the entire desktop behind a hook whose owner was not pumping. **The
user's whole machine froze, twice.**

**The leading hypothesis, unconfirmed.** WinRT/COM apartment teardown on the worker needs a message
pump that the blocked main thread cannot give it, so the thread cannot finish exiting and the join
cannot complete — a circular wait between `join()` and the pump. That was never proven, because:

**What was actually done.** The user's decision was to delete the path that reaches it rather than
debug it. v0.8.0 edits the live database in place, so no promote and no respawn exist; and the
demolition also deleted the **shutdown** `stop_worker` call, which existed only because a
`dead_code` warning had forced v0.7.2 to give the function a caller. `spawn_worker`'s handle is now
bound as `_worker` and never touched again. **No `JoinHandle::join()` on the worker survives
anywhere in the crate** — the only two `.join()` calls left are `join_save`'s config-save thread
and the builder's stdout reader, neither of which runs on the hook-owning thread while hooks are
installed. Verified by a zero-reference grep over 21 deleted identifiers plus a clean
`cargo check --all-targets`.

**So the defect moved to hook ownership.** The main UI thread no longer owns the hooks. Reintroducing
main-thread hook ownership, or blocking the dedicated hook pump, reintroduces a whole-desktop
freeze. It will not look like a chibipop bug when it does: the symptom is the *user's mouse* going
syrupy, with chibipop's window looking merely busy.

**If picked up**, preserve the `chibipop-hooks` owner thread. If a worker join is genuinely needed,
keep it away from that thread and keep the hook pump alive. Note that Windows may answer a hook that
misses `LowLevelHooksTimeout` by **dropping** it rather than waiting again, in which case the symptom
is not a slow desktop but hover going silently dead — see the 2026-07-27 spike finding. Both shapes
are covered by `docs/REGRESSION.md` §1.18 step 14.

---

## 25. A combined remove + add can rename the incoming archive to `terms (2).zip`

**Raised 2026-08-16 by the v0.8.0 round. Traced through the source, not reproduced — the conditions
below are read off the code, and no test exercises a removal and an addition in one Apply.**

`Library::free_name` (`src/library.rs:164-188`) appends ` (2)`, ` (3)` … when the incoming file name
is already `taken`, and `taken` (`:190-192`) is `dir.join(file).exists() || entries.any(|e| e.file
== file)`. On the ordinary combined path this is harmless: `apply_edits` runs **all removals before
any addition** (`src/app.rs:651-666`), and each removal quarantines its archive into
`library/.removed/` (`remove_one`, `:690`) before the additions loop calls `import`. The old file is
out of `dir` by then, so re-adding the same file name reuses it.

**Two reachable paths break that ordering guarantee**, and both leave the old file sitting in `dir`
while the add runs:

1. **The removal fails.** `remove_one` returns early on any error from `remove_dictionary`, and on
   the "dictionary was no longer in the database" bail (`:686`) — **both before the quarantine**.
   The loop records it in `report.failed` and carries straight on to the additions.
2. **The removal cannot name its archive.** `removal.file` is `None` whenever
   `settings::removed_files` could not resolve one (see item 26), so the rows are deleted and the
   `.zip` is never moved.

In both, the incoming archive lands as `terms (2).zip`, `library.json` records a name the user did
not choose, and — because `dict.name` comes from `index.json` and not from the file name — nothing
on screen explains where the `(2)` came from.

**If picked up:** the cheap correct fix is to make `free_name` ignore names the *current* `Pending`
has already quarantined, which is exactly the information `Pending.held` carries. The failure-path
half wants a decision first: an addition that follows a failed removal of the same file is arguably
a case Apply should refuse rather than half-do.

---

## 26. `settings::removed_files` cannot resolve a removal whose archive has an empty title

**Raised 2026-08-16 by the v0.8.0 round. Pre-existing — the two namespaces have disagreed since the
library was introduced; incremental removal is only what made it reachable.**

Two different things are both called a dictionary's "name", and they are populated by two functions
that do not agree on one input:

| | source | empty title |
|---|---|---|
| `dict.name` (database) | `build::dict_title` | kept as `""` |
| `library::Entry.name` | `library::title_of` | falls back to the file **stem** |

`title_of` filters `!t.is_empty()`; `dict_title` does not. So an archive whose `index.json` carries
`"title": ""` is `""` in the database and `terms` in the library. The staged-removal list carries the
**database** name (it comes from `from_config`), while `settings::removed_files`
(`src/settings.rs:181`) matches on **library** names — and for that archive neither it nor
`plan_edits` finds the file. **The rows are deleted and the `.zip` stays in `library/`**, which then
reads as drift (§1.19) forever, and feeds item 25's second path.

**If picked up:** the honest fix is to stop having two name functions — give `dict_title` the same
`!t.is_empty()` filter and rebuild, or key the removal on something that is not a title at all.
`meta.source_hashes[].name` is the archive **file** name and would join cleanly to `Entry.file`,
which is what §1.19's drift detection already does; the removal path could use the same join. Note
that `dict.name` is what `DictInfo` and `present.rs` match on, so changing it is a behaviour change
for anyone whose config names a `""` dictionary — which is nobody, since `""` cannot be typed into
the order list usefully.

---

## 27. `golden_corpus` reports `ok` without asserting anything, and is counted as a pass

**Raised 2026-08-16 by the v0.8.0 round, after four consecutive tasks re-reported it. Pre-existing,
and it quietly inflates every test total this project has ever published.**

> **Amended 2026-08-27 (ticket 17): the guard is fixed, and half of this item stands.**
> `tests/golden.rs` no longer probes one repo path. It resolves the dictionary the way the
> product does — `$CHIBIPOP_GOLDEN_DB`, then the Linux daemon's
> `$XDG_DATA_HOME/chibipop/chibipop.sqlite`, then the path the Windows bin's default `--out`
> lands on in a cargo tree — so the corpus **runs** on any box with a real library, and it
> failed the first time it did: `してしまった: expected する in top 3, got ["仕手", "して", "梓"]`
> (ticket 16). A skip now prints every path it looked at, so the reason is checkable.
>
> What stands: in dictionary-free CI it still early-returns and libtest still counts that as a
> pass, so CI totals still include one test that did not run. Option (b) below is **not** the
> answer it looks like — a committed fixture holding only the corpus words gives every case one
> candidate, and "expected X in top 3" is then true by construction. This corpus grades ranking
> against 660k terms of competition; a fixture would restore the green and delete the meaning.

`tests/golden.rs:31-34` early-returns when `data/chibipop.sqlite` is absent:

```rust
if !db.exists() {
    eprintln!("SKIP golden_corpus: {} not built", db.display());
    return;
}
```

A `#[test]` that returns is a **pass**. It is not `#[ignore]`d and does not appear in the `1 ignored`
beside the total, so on any tree without a built 242 MB database — every fresh clone, every git
worktree, and CI — the suite reports one more passing test than it ran. Every number in Tier 0 of
`docs/REGRESSION.md`, from **416** through **873**, includes it.

The cost is not the arithmetic; it is that **the one test that checks real deconjugation against a
real dictionary is the one test that silently does not run**, and it does not run in exactly the
environment where a regression would be cheapest to catch.

**If picked up:** three options, in increasing honesty and cost. (a) `#[ignore]` it and require
`--ignored` locally — the count then tells the truth and the test never runs in CI either.
(b) `panic!` on a missing database and give CI a small committed fixture instead of the real
242 MB file, which is the only option that actually makes the corpus run somewhere.
(c) Leave it and subtract, which is what `docs/REGRESSION.md`'s Tier 0 callout now does in prose.
**(b) is the one worth the money** — the golden corpus is 30-odd deconjugation cases and does not
need the real database, only *a* database containing those words.

---

## 28. Four rebuild-era strings that no live code path can now honour

**Raised 2026-08-16 by the v0.8.0 round. Two are pre-existing dead ends; two were made wrong by
this release. They are one item because the fix is one decision: which route does the app tell you
to take when a rebuild really is needed?**

v0.8.0 set a precedent — the frequency refusal (`app.rs:467`) and the drift notice
(`settings.rs:542`) both name the literal command, with the user's real paths substituted, and both
say "quit chibipop first" because `build::build` renames onto `<out>` and that rename fails against
an open database. **These four have not caught up:**

1. **`src/lookup/sqlite.rs:41`** — a schema mismatch says *"rebuild the dictionary from the settings
   window"*. **That window is unreachable from this error.** `chibipop run` dies in `spawn_worker`
   and `chibipop settings` dies at `SqliteDictionary::open` (`src/main.rs:369`) **before**
   `settings_only` is ever called. Verified: the CLI is the only route. Pre-existing, and the
   friendliest-sounding of the four.
2. **`src/main.rs:370`** — *"opening {} - rebuild it in the settings window"*, the same dead end
   reached from the other side, and the comment two lines above it (*"A rebuild renames onto it"*)
   now describes a path that only `settings_only` takes.
3. **`src/ui/settings_window.rs:1997-1999`** — *"chibipop is using a dictionary built outside the
   app. Adding or removing here rebuilds from this list only — import your original .zip files
   first."* **Made false in live mode by this release.** Its whole content is a warning that Apply
   will rebuild from the library and therefore drop the dictionaries the library does not have —
   and Apply no longer rebuilds from anything. Removing a dictionary now deletes exactly the rows
   asked for and touches nothing else. It stays **true for `chibipop settings`**, which still
   rebuilds, so this is not a deletion: it is a mode split the string does not currently make.
   Left alone deliberately in the v0.8.0 doc task, because gating it on `ApplyMode` means writing
   new copy for the live half and that is a product decision, not a documentation fix.
   `self.apply_mode` is already in scope at that call site, so the mechanics are one condition.
4. **`src/app.rs:555-556`** — the WAL refusal says *"Rebuild the dictionary to convert it"* without
   saying with what. New in this release, reachable only for a legacy `delete`-mode file, and the
   one of the four where the instruction is at least *correct* — `build.rs` sets `journal_mode=WAL`,
   so a rebuild does convert it.

**If picked up:** decide once, then apply it to all four — either every "you need a rebuild" message
names `chibipop build-dict --library "<lib>" --out "<db>"` with real paths and a "quit chibipop
first", matching the two that already do, or a `rebuild_instruction(library, db)` helper is written
once and called from all six sites. The second is the better shape and is why this is an item rather
than a one-line fix.

## 29. The match highlight is the whole merged run, not the matched word — **FIXED 2026-08-17**

> **Fixed in `d1508dc`, the day it was filed.** `resolve` builds `span.geom` from the **unmerged**
> words, so one entry is one OCR word and `union_chars` boxes exactly the matched characters.
> `merge_spaced_words`, `spaced_on_line` and `union_rect` had no other caller and are deleted
> (−55 lines, −10 tests, +2 guards). `f3668bb` wanted the highlight to span gaps; `union_chars`
> returns a bounding box, so gaps inside a match were always covered. The merge solved a problem
> the union had already solved.
>
> Live on the fixture: 宿舎 `w=314 → 56` (**predicted before running, matched exactly**),
> 図書館 `267 → 106`, 風邪をひいて `273 → 158`, a 1-char vertical match `h=185 → 27`.
>
> The account below is kept because the mechanism recurs.

**Regressed 2026-08-04 in `f3668bb`, found 2026-08-17 by `docs/REGRESSION.md` §1.2.** User-visible
only with `popup.highlight_match = true`, which is why it survived thirteen days and a release.

`merge_spaced_words` (`src/text/layout.rs:334`) folds consecutive **single-character** OCR words
into one `OcrWord` carrying the union rect, and `resolve` builds `span.geom` from that merged list.
Windows OCR returns one word per CJK character, so an ordinary Japanese line collapses to a
**single** `TextGeom` with `char_count = line length`. `union_chars` unions whole entries, so any
match inside that entry gets the entire run's rect.

Measured on `docs/fixtures/ocr-corpus.html`: 宿舎 (2 chars) → `w=314`; 図書館 (4) → `w=267`;
風邪をひいて (6) → `w=273`; a 1-char match on the *vertical* column → `h=185`. Expected for the
first is `x=176 y=123 w=56 h=30`. The heights are right, which is what makes it look plausible.
Falsifier, same binary: the fixture's `letter-spacing:80px` line puts the gap over
`spaced_on_line`'s `2 × size` ceiling, the merge does not fire, and the box is exact —
`x=416 y=733 w=29 h=30` for a `w=23` glyph.

**Why the suite is green.** Every `union_chars` test hand-builds geom at `char_count: 1` per entry,
and `resolve_carries_one_geom_entry_per_word_aligned_to_the_text` asserts three entries for three
words that do not merge. Nothing feeds `resolve` a run of evenly spaced single CJK characters.

**If picked up:** the merge exists for line assembly and for OCR that splits a word across spaced
glyphs — deleting it is not the fix. The geometry wants to stay per-character while the *text* is
merged: keep the component rects on the merged entry (`Vec<PhysRect>` beside `char_count`, or a
parallel per-character geom vector that `union_chars` indexes), so `union_chars` can slice inside an
entry instead of only between entries. Whatever the shape, the guard is the missing test above:
resolve a real evenly-spaced CJK run and assert a 2-char match yields ink + 2×3px, not the line.

---

## 30. ~~`plugin::host`, `plugin::text`, `plugin::strikes` and `PluginText` have no production caller~~ — **FIXED 2026-08-19**

> **Fixed by giving config a real engine choice.** `[ocr].engine` and `[plugins].enabled`
> (`src/config.rs`) resolve through `resolve_engine` to an `EngineChoice`. `src/app.rs`'s
> `worker_main` calls it once at startup: `EngineChoice::Plugin(name)` runs `find_text_plugin`
> against `plugin::discover::discover`, then `plugin::host::spawn` and `PluginText::new`, and hands
> the result to `text::ocr::OcrTextSource::with_recogniser` — a new, additive constructor next to
> `new`, so the built-in path is untouched. `EngineChoice::Builtin`, `EngineChoice::FellBack` and
> any discovery/spawn failure all take the same fallback: one `eprintln!` naming the plugin and the
> reason, then the built-in engine, with `cfg.ocr.engine` never rewritten — a returning plugin
> resumes on the next start, per spec section 6.
>
> **Verified live, not reasoned.** A throwaway test (written, run, deleted) drove exactly this
> chain against the real meikiocr plugin under `target/release/plugins/`: `with_recogniser` built
> an `OcrTextSource` around a spawned `PluginText`, and `recognise()` read `"昨日は"` off
> `tests/fixtures/japanese_bgra.bin` — the same string Task 3 read directly off `PluginText`, now
> reached through the config seam instead of by hand.
>
> `PluginText`'s five fields (`src/plugin/text.rs:84-89`) are `pub(crate)` again — `app.rs`'s
> `spawn_plugin_recogniser` is the second crate-internal caller of `PluginText::new` the item asked
> to check for, and `cargo check` stays clean at that visibility, confirming every field now has an
> in-crate reader.

**Raised 2026-08-18 by the whole-branch review. Deliberate, not an oversight — recorded because
nothing tracked will say so once this branch merges.**

Nothing in the popup path calls a plugin. `chibipop plugin list` and `chibipop plugin test` are
the only way any of `src/plugin/host.rs`, `src/plugin/text.rs` or `src/plugin/strikes.rs` runs.
`PluginText::new` (`src/plugin/text.rs:91`) has **zero callers anywhere in the crate**, including
its own test module — a full grep of `src/` and `tests/` for `PluginText` returns only its
declaration, its `impl` block, and the constructor body itself. `Strikes`
(`src/plugin/strikes.rs`) is built only inside that constructor, so it sits one layer further from
anything that runs.

This gap is by design. The `TextProvider` impl that would wire `PluginText` into the popup path
was withheld on purpose: an impl whose only method always errors type-checks as a working provider
and fails at run time, not build time, which hides the gap instead of naming it. Task 8 shipped
the struct and its pure helpers. It did not connect them.

**The cost this leaves behind.** `PluginText`'s five fields — `host`, `name`, `geometry`,
`language`, `timeout` (`src/plugin/text.rs:81-88`) — are `pub`, wider than any of them needs to
be, **only** to satisfy `dead_code`. BACKLOG item 10 already records the reason: a `pub` item in a
crate that is both a library and a binary never trips that lint, caller or not, and Task 8's
review used exactly that fact to widen these fields rather than reach for `#[allow]`. `strikes`
alone stays `pub(crate)`, because `disabled()` reads it from inside the same module.

**If picked up:** the moment a real `impl TextProvider for PluginText` reads these fields, shrink
all five back to `pub(crate)`. Widening them was a workaround for an impl that did not exist yet,
not a decision that plugin internals should be public API. Grep for `PluginText::new` before
landing that impl, to confirm it still has exactly one caller — the new impl — and not a second
one hiding somewhere else.

---

## 31. ~~`TextProvider` is shaped as "do the whole lookup", not "supply text"~~ — **FIXED 2026-08-19**

> **Fixed by the re-cut this item predicted.** `TextProvider`, `TextRead` and `read_at` are
> deleted, and `src/text/provider.rs` with them. The trait is now
> `text::recogniser::Recogniser` (`src/text/recogniser.rs`):
> `recognise(&self, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>>`, plus `name` and
> `provides_geometry`. That is the shape `text::ocr::recognise` already had, and it is exactly
> what a plugin can promise — pixels in, lines out.
>
> **Everything the item said a plugin cannot supply now sits above the seam and never reaches
> one.** Capture, tiling, orientation, `nearest_line`, hit scan and the `ScanRect` debug overlay
> all stay inside `OcrTextSource`. `src/app.rs`'s `resolve_trigger` calls the concrete
> `resolve_at_tiled_scanned` again, which is simpler than the trait call it replaced.
>
> `OcrTextSource` holds `Box<dyn Recogniser>` and the built-in engine is `WindowsOcr`, so the
> two live call sites — `recognise_at_capture` and `words_in` — are the only places OCR happens.
> Net effect on `src/` and `tests/`: **+101 lines, −193**. `tests/ocr_fixture.rs` proves the
> delegation is faithful against the real Windows engine and the committed BGRA fixture.

**Raised 2026-08-18 by the whole-branch review. This is the branch's known seam, expected to be
re-cut on first contact — not a hidden defect.**

`TextProvider::read_at(&self, cursor: PhysPoint, collect_scan: bool) -> Result<TextRead>`
(`src/text/provider.rs:10-11`) was widened during Task 2 to fit its one live caller, `app.rs`'s
`resolve_trigger`. `TextRead` carries `{ resolved: Option<Resolved>, scan: Vec<ScanRect> }`
(`:5-8`), and `Resolved` carries `{ span: TextSpan, orientation: Orientation }`
(`src/text/layout.rs:51-54`).

`PluginText` cannot implement this trait as it stands. A plugin supplies OCR text over a pipe. It
does not capture the screen, tile a region, detect orientation, or scan a debug overlay —
redoing any of those inside a plugin's impl would duplicate work the host already does for the
built-in engine. `orientation` has no plugin-side source at all: the wire protocol
(`src/plugin/proto.rs`'s `RecogniseResult`, `Line`, `Word`) carries no such field, so an impl
would have to invent a value rather than report one.

`ScanRect` reaches the trait for the same reason. It is a debug-overlay concept — the boxes
`probe --show-region` draws — that means nothing to a plugin; it is only in `TextRead` because
the one existing caller needed it.

**If picked up:** narrow `TextProvider` to what a plugin can actually promise — text, and
geometry only when geometry exists — and give the built-in engine's tiling, scanning and
orientation detection their own interface above that, called only from the one site that still
needs `ScanRect`. This is design work, not a mechanical split: it touches
`impl TextProvider for OcrTextSource` (`src/text/ocr.rs:506-513`) and every call site in
`src/app.rs` and `src/main.rs`.

---

## 32. The host's inbox is unbounded — the mirror of a bug already fixed on the outbox

**Raised 2026-08-18 by the whole-branch review. Harmless today; stops being harmless the moment a
host outlives one CLI command.**

`host::spawn` (`src/plugin/host.rs:166-174`) reads a plugin's stdout on its own thread, one line
at a time, and forwards every line onto an `mpsc::channel()` with no bound. `Host::attempt`
(`:232-274`) drains that channel with `recv_timeout` against a deadline, but the deadline only
bounds how long `attempt` **waits** for the next line. It does nothing to the reader thread, which
keeps pushing every line the child prints, on schedule or not, for as long as the process lives.

**This is the exact mirror of a bug Task 6 already fixed, one direction over.** Task 6's review
found `call`'s write to the plugin's stdin unbounded — a plugin that stopped draining its input
could block the caller forever. The fix bounded the *outbox* (`Outbox`, `:39-77`) to a single
slot: a second request replaces the first instead of queueing behind it. Nobody applied the same
fix to the *inbox*. A plugin that never emits a newline, or emits lines continuously while idle —
a heartbeat, a stray log, a bug — grows this channel's backing `Vec` without limit.

**Why it has not bitten anyone yet.** Every `Host` this branch creates lives for the length of one
`chibipop plugin test` invocation, seconds at most, and the channel goes away with the process.
The leak needs a long-lived host — the kind a future "keep the plugin warm across hovers" feature
would create — before it becomes a real, unbounded process.

**If picked up, `sync_channel` is the wrong fix.** A bounded `mpsc::sync_channel` sender
**blocks** when full, and blocking the reader thread on a full channel reintroduces the exact
caller-blocking defect Task 6 removed, only moved from the write side to the read side. The
honest fix bounds the channel's logical backlog, not the call that fills it — drop or coalesce
lines the way `Outbox::put` already replaces a stale outbound request, so the reader thread never
blocks and the channel never grows past a small constant.

---

## 33. ~~`span_from_lines` reads `lines.first()` only and ignores `cursor.y`~~ — **DISSOLVED 2026-08-19**

> **Dissolved, not repaired.** `span_from_lines` is deleted, together with its private `to_screen`
> helper and its three tests. The seam moved down to `Recogniser` (item 31), so a plugin now
> returns `Vec<OcrLine>` and line choice belongs to the layer above — where
> `text::layout::nearest_line` already does exactly this job, by geometry, with 7 tests.
>
> The item asked for `span_from_lines` to be given `nearest_line`'s behaviour. Deleting the caller
> was cheaper than duplicating the callee. `estimate_offset` and `PluginText` are untouched and
> still have no production caller; item 30 tracks that, and Task 4 of the current plan closes it.

**Raised 2026-08-18 by the whole-branch review.**

`span_from_lines` (`src/plugin/text.rs:28-73`) opens with `let line = r.lines.first()?;` and
never reads `cursor.y` or looks at any other entry in `r.lines`. Any plugin response carrying more
than one line resolves against whichever line the plugin put first, regardless of where the
cursor actually sits.

The built-in engine does not have this gap. `text::layout::nearest_line`
(`src/text/layout.rs:140`) picks a line by comparing the cursor's position against every
candidate, and the OCR path calls it for exactly that reason. `span_from_lines` has no
equivalent. Its own tests — `geometry_maps_image_pixels_back_to_the_screen`,
`a_text_only_line_yields_empty_geometry` and the rest — all build a `RecogniseResult` with exactly
one line, so nothing in the suite exercises the multi-line case at all.

**If picked up:** give `span_from_lines` the same job `nearest_line` already does for the
built-in path — pick the line closest to `cursor.y` (by geometry when words are present, by read
order when they are not), then run the existing per-line offset logic against that line instead
of `lines[0]`. The fix needs a new test with two or more lines and a cursor placed over the second
one; nothing today covers that shape.

---

## 34. ~~The inherent single-pass `resolve_at` on `OcrTextSource` is dead~~ — **FIXED 2026-08-19**

> **Deleted, after the fresh grep this item asked for.** `.resolve_at(` returned nothing anywhere
> in `src/` or `tests/` — only the definition — so `resolve_at` and its doc comment are gone. The
> differently-named `resolve_at_tiled`, `resolve_at_tiled_scanned` and `resolve_at_verbose` are
> untouched and still live.
>
> **One claim in the same round's design note was wrong, and is corrected here.** That note listed
> `OcrTextSource::engine()` as a second zero-caller deletion. It had one caller:
> `tests/ocr_fixture.rs`, which reached through it to run the real engine. `engine()` is gone all
> the same — it leaked a raw WinRT `OcrEngine` out of a type that no longer holds one — and
> `recogniser(&self) -> &dyn Recogniser` replaces it. The fixture test now exercises the built-in
> path **through the trait**, which is a better regression check than the one it replaced.

**Raised 2026-08-18 by the whole-branch review. Task 2's review flagged this as a deferred minor
and named the whole-branch review as the place to triage it.**

`OcrTextSource::resolve_at` (`src/text/ocr.rs:369-371`) has had zero callers repo-wide since
Task 2 (`a98efc2`) moved `app.rs`'s two live call sites onto `TextProvider::read_at`, which
reaches the multi-pass `resolve_at_tiled_scanned` instead. A direct grep confirms it: no
`.resolve_at(` call survives anywhere in `src/` or `tests/` — only the definition itself, and the
differently-named `resolve_at_tiled`, `resolve_at_tiled_scanned` and `resolve_at_verbose`, which
are all still live and called.

**Nothing will warn.** `resolve_at` is `pub`, and BACKLOG item 10 already records the blind spot
this depends on: a `pub` item in a crate that is both a library and a binary counts as library API
to `dead_code`, caller or not. The clippy gate that would normally catch an orphaned function
structurally cannot see this one.

A `pub` single-pass `resolve_at` sitting beside a `pub` multi-pass `resolve_at_tiled_scanned`,
both callable by name, is the same trap BACKLOG item 4 was originally about — a faster,
wrong-shaped path left reachable beside the real one, waiting for some future caller to reach for
the shorter name and get single-pass accuracy by accident.

**If picked up:** delete `resolve_at` (`:369-371`) and its doc comment. It is three lines with no
test of its own; `resolve_at_verbose`, which it wraps, is what the existing tests already
exercise. Confirm with a fresh grep for `.resolve_at(` (not `resolve_at_tiled`, not
`resolve_at_verbose`) that nothing started calling it between this being written and it being
removed.

---

## 35. Spec section 7.2's capability check is one-directional

**Raised 2026-08-18 by the whole-branch review.**

`test_one` (`src/plugin/cli.rs:112-117`) checks one half of the capability contract the design
spec's section 7.2 defines:

```rust
let claimed = cfg.provides_geometry;
let got = parsed.lines.iter().any(|l| l.words.is_some());
if claimed && !got {
    eprintln!("VIOLATION: manifest claims geometry, the response carries none");
    return 1;
}
```

This catches a plugin whose manifest sets `provides_geometry = true` while its response carries
no `words` on any line. It does not catch the reverse: `provides_geometry = false` in the
manifest, with a response that **does** carry `words`. The spec calls both directions a violation
— any disagreement between the manifest and the response — but only one direction here ever sets
the exit code. (The spec lives under the gitignored `docs/superpowers/`, not published with the
repo — the same caveat as BACKLOG item 1's sources.)

`cli.rs` is the only place in the crate that compares claimed geometry against actual geometry at
all, since no `TextProvider` impl for a plugin exists yet (BACKLOG item 30) to make the same
comparison at run time. So today the gap is silent everywhere it could matter.

**If picked up:** add the second branch — `!claimed && got` — with its own message naming the
direction ("manifest claims no geometry, the response carries words"). The two branches are
symmetric, so the code change is small; the reason only one shipped is that only one direction
had a fixture to expose it — `plugin-echo`'s `text/recognise` reply always carries `words`, so
nothing in this branch's tests could have failed on the missing half.

---

## 36. A blank region is not a protocol violation — **FIXED 2026-08-19**

> **Fixed in `31c7ca7`, the day it was raised.** `test_one` decided whether a plugin had
> honoured its `provides_geometry` claim with `parsed.lines.iter().any(|l|
> l.words.is_some())`. `.any()` over an empty `Vec` returns `false`, so a blank region —
> correctly reported as zero lines — was indistinguishable from a plugin that claimed
> geometry and silently withheld it. Both exited **1**, the code this command's contract
> reserves for the plugin author's fault.
>
> The check is now `violates_geometry(claimed, r)`, gated on `r.lines.is_empty()` before
> the `.any()` call, with three unit tests over the predicate. Proven against the real
> meikiocr plugin, both cases in one session: `blank-500x100.png` now exits 0 with 0
> lines and no accusation; `ref-line.png` still exits 0 with its 23-word line.

**Found 2026-08-19 by the controller, running the real meikiocr plugin against a blank
region — not by anything in the suite.** It survived Task 9's review, that task's fix
round, and its scoped re-review, because every test written against this check pointed
at an image that had text in it. The general shape is worth naming: a `.any()` predicate
over a collection that can legitimately be empty makes "no matches" and "nothing to
match against" the same `false`, unless the empty case is excluded first, explicitly.

Item **35**, raised the day before against this same function, is the check's other gap
— the missing reverse direction, `!claimed && got`. That item's code quote and line
citation were not re-verified against this fix and may now be stale; item 35 itself
stays open, per this plan.

---

## 37. Tier 0 cannot be all-green on a developer machine: the geometry goldens are pinned to the CI image

**Found 2026-08-29, running tier 0 before tagging v0.9.9 at `98b133c`.**
`geometry_golden_full_chrome` fails on the Windows development box and passes on CI, on
the same commit. One field diverges:

```
variants.default.elements.3.w: golden "46.43" -> measured "47.03"  ["Text" "ざつだん"]
```

Everything else matches: the other seven golden fixtures pass, and the rest of the suite
is 1338 green. Clippy is exactly 1 and the release build finishes.

**This is not a regression.** The gate asserts DirectWrite metrics with **no tolerance**,
against goldens blessed on `windows-2025`, and drift enters only via runner-image font
updates. Two machines with different Yu Gothic UI files measure the same string
differently, and 0.6 px is exactly that size of difference. What that rule did not
anticipate is that a *developer box* is a second image, permanently.

**The consequence is a process one.** `RELEASING.md` step 2 says "Run tier 0 locally. Do
not tag a commit you have not gated." That instruction can no longer be satisfied on this
machine: its test line is red before any change is made. A reader who does not know why
has two bad options — bless locally, which reds CI for everyone else, or stop reading the
line, which is how the next real golden failure gets waved through.

### What would fix it, and what would not

| Option | Why it is not free |
|---|---|
| Widen to a tolerance | Refused by a standing rule — no tolerance is permitted. A tolerance masks the bug class the gate exists for: rounding moved into core, off-by-one gap accounting, scroll-culling boundary shifts. |
| Bless locally | Reds CI. The goldens are one file, shared. |
| Skip the goldens off CI (`CI` env var) | Cheap, and it makes the local gate green — but it also means the adapter's only regression net never runs where the code is written. |
| A second golden set per machine | Two baselines to keep honest, and nothing tells you when they disagree for a real reason. |
| Ship the font with the fixtures | The measurement would stop depending on the machine at all. Largest change; also the only one that makes local and CI mean the same thing. |

**Not decided.** The cheap option and the correct option are not the same one here, which
is why this is a backlog item and not a fix. Until it is picked up, tier 0's table says
what a green run looks like on this machine: 1338 passed, 1 failed, and the failure named.

**Evidence:** `docs/REGRESSION.md` tier 0; CI run 33111391694 (all four jobs green at
`98b133c`); `crates/chibipop-windows/tests/geometry_goldens.rs`;
[`ARCHITECTURE.md`](../ARCHITECTURE.md#verification).

---

## 38. ~~`release.yml` runs the geometry goldens on `windows-latest`, which the image pin forbids~~ — **FIXED 2026-08-29**

> [!note] Fixed the same day it was found, on oniichan's instruction
> `release.yml`'s Windows job is now `runs-on: windows-2025`, matching
> `ci.yml`'s tier0 job, and carries a comment saying why and naming this item.
> Moving either pin now means moving both, in the same commit as the re-blessed
> goldens. The v0.9.9 release was already built and drafted on the old label —
> which resolved to `windows-2025` anyway, so those assets are unaffected.

**Found 2026-08-29, reading the release workflow before tagging v0.9.9.**
`ci.yml`'s tier 0 job is pinned to `windows-2025`, with a comment explaining that the
runner image is part of the goldens' baseline. `release.yml`'s Windows job runs
`cargo test --workspace --exclude chibipop-linux` — the same suite, including the same
goldens — on `runs-on: windows-latest`.

```yaml
  windows:
    name: Build and package (Windows)
    runs-on: windows-latest
```

**It works today**, because `windows-latest` currently resolves to `windows-2025`. It
stops working the day GitHub migrates the label. The failure mode is the bad one: the tag
is already pushed and permanent, the release job reds on a test that has nothing to do
with the release, and no asset is produced.

**The fix is one word** — `windows-2025`, matching `ci.yml`, with the same comment. It was
left out of the v0.9.9 release commit deliberately: changing the release workflow is a
change to what gets released, and that is oniichan's call, not a side effect of cutting a
tag. He made it the same day, and the pin landed on its own branch.

**Evidence:** `.github/workflows/release.yml`; `.github/workflows/ci.yml` tier 0's
`runs-on` and its comment; [`ARCHITECTURE.md`](../ARCHITECTURE.md#verification), "The CI
image is pinned".

---

## 39. 55% of a hover is SQLite probes that find nothing

Measured, not suspected: [docs/research/lookup-cost.md](research/lookup-cost.md).

A `LookupEngine::run` over a 25-character OCR line issues **139** point queries at p50
(204 at p95, 291 at max), and **131 of them miss**. Each miss costs 4.2-5.1 µs, almost
all of it fixed per-call SQLite overhead - `prepare_cached` + bind + step - with under
0.8 µs of Rust on top. It does not grow with the library: a miss costs 4.22 µs against
435 k entries and 4.30 µs against 2.6 M.

That is **553.5 µs of a 1 011.7 µs median hover on a 12-dictionary library (54.7%)**,
and 537.0 of 825.5 µs (65.1%) on a single-dictionary one.

A bloom filter over every distinct `term.surface` was built for real and probed with
the 37 855 misses recorded from the engine itself:

| | library | live |
|---|---:|---:|
| filter size, 10 bits/key, k=7 | 2.1 MB | 1.0 MB |
| build at startup | 0.2 s | <0.1 s |
| probe | 0.053 µs | 0.053 µs |
| false positives | 1.39% | 0.32% |
| **saved per 25-char hover** | **652.5 µs (54%)** | **525.2 µs (64%)** |

Build it at daemon start and never persist it; 0.2 s is cheaper than invalidating it.
Rebuild whenever the library changes. Use xxHash rather than the harness's FNV-1a,
which is faster on short keys and would lower both the probe cost and the
false-positive rate.

The saving scales with input length because misses do: 131 per 25-char line, 34 per
8-char line, 4 for a bare headword. On a bare headword this is worth ~20 µs and is not
worth doing. It earns its keep on running text, which is what OCR hands the engine.

Prior art: `Manhhao/hoshidicts` keeps exactly this - a `bloom.filter` consulted before
its `hash.table`, both `mmap`'d (`src/query.cpp:46-61,120-151`). Its win is not faster
glossary decoding; it is never making the call.

---

## 40. `terms_for` spends more time allocating rows than SQLite spends producing them

Measured: [docs/research/lookup-cost.md](research/lookup-cost.md).

For 「こう」, which the 12-dictionary library answers with 862 rows:

| stage | µs | share of `terms_for` |
|---|---:|---:|
| SQLite: index seek + 862 steps, no column decoded | 161.1 | 22% |
| + every column decoded, borrowed `ValueRef`, zero copy | 234.3 | 32% |
| + mapped into `Vec<TermRow>`, what `terms_for` returns | 734.7 | 100% |
| **row-mapping allocation alone** | **500.2** | **68%** |

68% of the call is three heap allocations per row. On the worst-case headword set it is
185 µs p50 (53%); on a median frequency headword it is 5.6 µs (28% of `terms_for`,
0.6% of a hover), so this is a tail fix, not a median one. The grouping that follows in
`engine.rs` clones `written` and `reading` a second time, worth up to a further 47 µs
p50 on that set.

On the worst hover measured - 「ていただく・盗み見る・盗み見する・盗視する・目を通」,
185 queries, 65 002 term rows - `terms_for` is 38.6 ms and grouping plus sorting
11.6 ms, together 98% of a **51.3 ms** hover. Three frames, on one real input.

Shape of the fix: hand the ranker borrowed `&str` out of SQLite's page, or one arena
per lookup, and only materialise owned strings for the `MAX_RESULTS` survivors -
hoshidicts's `materialize()` pattern (`src/query.cpp:492-496`), which defers
decompression until after ranking. **[MODELLED]** ceiling is the measured
`mapped − borrowed` delta, because it cannot touch the step or the column decode.

---

## 41. Deconjugation is 23% of a hover and never touches the database

Measured: [docs/research/lookup-cost.md](research/lookup-cost.md). The prefix loop -
25 prefixes x 104 rules - costs **228.5 µs at p50** on a 12-dictionary library (22.6%)
and 224.1 µs (27.1%) on a single-dictionary one. It is identical on both, because it is
pure CPU with no database involved, and it is larger than everything the dictionary
rendering work touches.

It is also the source of the 131 misses in item 39: most of what it produces does not
exist in any dictionary. The two items compose - a bloom filter rejects the products
cheaply, memoisation stops producing them twice - and neither subsumes the other.

Not investigated: whether the same prefix set is re-deconjugated across consecutive
hovers on the same OCR line, which is the case a cache would collapse. Measure that
before building anything.

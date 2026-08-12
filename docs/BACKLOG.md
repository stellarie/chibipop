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

---

## 11. The settings window is height-constrained and cannot scroll

**Raised 2026-08-11 by the per-character-retrigger / OCR-language branch, which nearly tripped it
and was rerouted instead of fixing it.** Not a regression — this ceiling predates the branch and
the branch ships clear of it. It is recorded because the next tab that grows will hit it, and
nothing will warn.

### The mechanism

Three facts compose into the problem:

- **The bottom block sits at the tallest tab.** `settings_window.rs:1907` is
  `y = y_general.max(y_dict).max(y_ocr).max(y_ank)`, and the Apply / Cancel / Quit row is placed
  from that `y`. So a tab growing taller pushes the buttons down on **every** tab, not just its own.
- **`fit_to` clamps to the work area** (`settings_window.rs:1478-1480`): `outer_h.min(cap)`, where
  `cap` is `work_area_height`. When the content is taller than the screen allows, the window is
  silently made shorter than its content.
- **There is no scrolling.** `grep -c "WM_VSCROLL\|SetScrollInfo" src/ui/settings_window.rs`
  returns **0**. (The `WS_VSCROLL` flags in that file are all on combo boxes — dropdown
  scrollbars, not a scrollable window.) So the clamp does not hide content behind a scrollbar; it
  truncates it, and what is at the bottom is the row containing **Apply**.

That failure has shipped once already. The comment at `settings_window.rs:965-971` records the
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
| v0.6.0 | **426** | 316 (400 worst case) | 280 | 260 | **426** | **594** |
| Task 7's first attempt — **discarded, never shipped** | **484** | 316 (400 worst case) | 328 | 260 | **484** | **652** |
| shipped v0.7.0 | **426** | 316 (400 worst case) | **380** | 260 | **426** | **594** |

**Only `y_ocr` moved between v0.6.0 and v0.7.0**; the other four columns are identical across all
three rows, which is exactly why the governing `max()` and the client height did not move. The
bottom block and padding add a constant 168px below the governing `max()`, which is why every row
is internally consistent (426 + 168 = 594; 484 + 168 = 652). **426 is the number to budget
against**; 484 only ever existed on the discarded attempt.

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

---

## 12. The General tab's "Popup" group box is 32px too short for its contents

**Present in shipped v0.6.0. Spotted 2026-08-11 by the per-character-retrigger / OCR-language
branch and correctly left alone — it was out of that branch's scope and is purely cosmetic.**

The `Popup` group box on the General tab is drawn 238px tall but encloses 270px of controls, so the
fourth checkbox — **`Hide the popup from screen capture`** — draws entirely **below its own frame**,
and the third (`Show related words beside the popup`) has its bottom 8px clipped by it too.

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
set: `toggle_selected` re-reads the rows at click time but classifies them with the cached
`unreadable` (`src/ui/settings_window.rs:753`), and `read` hands that same cached set to
`apply_to` (`:2258`), which keys the list through it. The two therefore agree with each other even
while both are stale, so the visible symptom is `Include / exclude` looking enabled on a row that
will not move, which `docs/REGRESSION.md` §1.17 documents as expected. Nothing is written wrong
and no list is emptied; the empty-list guard catches the consequence that would have mattered.

**The reachable trigger is a rebuild that *fails*** and leaves the window open — an import that
succeeds ends in a restart, which reopens the window and recaptures the set.

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

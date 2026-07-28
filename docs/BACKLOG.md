# chibipop — backlog

Work that is designed or measured but deliberately not built yet, with the reason and the
evidence needed to pick it up cheaply. Newest first.

---

## 1. Forward tiling: make multi-pass resolve the character under the cursor

**Deferred 2026-07-28 by the user's decision:** single-pass is accurate today
(`max_ocr_passes = 1`, 9/9 on the measured sweep), so the tiling rework is not worth gating
working changes on. The design is done and independently reviewed — this is a build, not a
design round.

**Spec:** `docs/superpowers/specs/2026-07-28-accuracy-and-polish-design.md` §3 (D1-R, D1-R2).
**Plan:** `docs/superpowers/plans/2026-07-28-accuracy-and-polish.md` Tasks 3–4, 6 §2.1.

### What is already built

- `text::layout::TextGeom` and `union_chars` — landed `17e18b0`.
- `TextSpan::geom`, populated by `resolve` on the single-pass path — landed `17e18b0`.

### What is left

1. **`drop_leading(words, start, orientation)`** in `text/layout.rs` — the mirror of
   `split_at_clipped`'s trailing rule, carrying the **same `EDGE_MARGIN` tolerance**. Without the
   margin it deletes exactly the glyph the previous tile deliberately deferred. The plan has the
   implementation and its four tests written out verbatim, including the load-bearing
   *"a word just inside the margin is kept"* case and the instruction to prove it by deleting the
   margin and watching that test fail.
2. **Head + tail assembly in `text/ocr.rs`** (plan Task 4 Step 2). Pass 1 keeps its text from the
   hovered word to its last unclipped word; tiles supply only the tail. Three details that are
   easy to get wrong are enumerated in the plan; the sharpest is that **tile 1 must start at the
   last kept word's trailing edge, not at pass 1's region edge.**
3. **Carry `geom` through the seam.** `tile_forward` returns a `String` and drops the boxes, so a
   stitched span has empty `geom` and **the match highlight does not draw on the tiled path**.
   It is absent rather than wrong (`union_chars` returns `None` on empty geometry), and
   `resolve_at_tiled_scanned`'s doc comment says so — but it is a real gap the moment tiling
   comes back on.
4. **Acceptance:** plan Task 6 §2.1 — nine hovers, ground truth transcribed by eye *first*, and
   all three of: 9/9 resolved character, no duplicated/spurious run, no omitted character. Count
   only **seam-local** defects; ordinary OCR misreads are not failures of this criterion.

**Do not re-enable `max_ocr_passes = 2+` by default until §2.1 passes.**

---

## 2. Vertical text: square-to-detect, transpose-to-read

**Measured 2026-07-28**, fix deliberately left to its own round (spec §5 says so).
**Evidence:** `docs/superpowers/findings/2026-07-28-vertical-text-measurement.md`.

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
- **Text clipped by a window edge is unrecoverable** at any capture shape. Worth a line in the
  README if users report it as a bug; it is a ceiling, not a defect.

---

## 5. Single-instance guard

**Raised 2026-07-28** by the adversarial review of the wheel-swallow feature. There is no
`CreateMutexW` or equivalent anywhere in `src/`, so two `chibipop run` processes install two
`WH_MOUSE_LL` hooks.

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

The mitigation is shipped, so this is not urgent: **settings now open automatically at startup**,
and `chibipop settings` reaches the same window with no tray at all.

To actually fix it, log every message arriving in `app::run`'s loop with its `hwnd`, `message`,
`wParam` and `lParam`, run it, have oniichan right-click once, and read the log. That distinguishes
all three possibilities in a single click — nothing arrives (delivery), something arrives at an
unexpected hwnd (the `menu_owner` mismatch), or `WM_CONTEXTMENU` arrives and is dropped by the
`match` (the version-4 case). Only then change the code.

**Lesson worth keeping:** "I made the feature happen" is not "the user's action makes the feature
happen". Posting the message bypassed precisely the layer that was broken.

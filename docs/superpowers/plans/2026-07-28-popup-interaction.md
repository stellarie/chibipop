# Popup Interaction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the popup something you can move into and read — it stays put while the cursor is on it, long entries scroll, and it stops re-rendering when the answer has not changed.

**Architecture:** `app.rs` gains an `Option<Shown>` recording what is on screen. A pure three-rect "sticky region" in `geom.rs` decides whether the cursor is on the word or the popup; while it is, no trigger is dispatched at all. `render.rs` stops clamping content and instead reports its natural height, painting at a scroll offset with a scrollbar. Wheel events arrive through the existing low-level mouse hook, which swallows them only while the main thread has armed it.

**Tech Stack:** Rust 2021, `windows` 0.62.2, Direct2D/DirectWrite. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-28-popup-interaction-design.md` (at `b2df7c5`, including the D2a correction)

## Global Constraints

- **`src/geom.rs`, `src/text/layout.rs`, `src/lookup/`, `src/present.rs`, `src/config.rs` and `src/ui/theme.rs` must not depend on the `windows` crate.** They compile and test with no screen.
- **Comments:** default is none; non-doc comment text **under 30 characters**. Rustdoc (`///`, `//!`) and `// SAFETY:` are exempt and expected to be detailed. Every `unsafe` block gets a `// SAFETY:`. **Over-budget rationale gets relocated to rustdoc, never deleted.**
- **Do NOT run `cargo fmt`.** This repo has never been rustfmt-clean.
- **Clippy must stay at exactly 5 accepted errors** (`app.rs`, `input/hooks.rs`, `lookup/deconj.rs`, `lookup/model.rs`, `ui/render.rs`). **Check it in every task.** The usual command aborts on those 5 before the *binary* target compiles, so to lint `src/main.rs` also run it with them suppressed:
  `-A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity`
- **Cargo is not on PATH.** Prefix every cargo command with:
  `export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup;`
- **chibipop may hold a lock on its own binary.** On "failed to remove file":
  `powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"`
- **`Cargo.toml` has a pre-existing unrelated unstaged change. Leave it; never stage it.**
- **Never `git add -A`, `git add .`, or `git add -u`.** Stage only the files you deliberately changed, by name. `git add -u` sweeps up `Cargo.toml` — this happened once already.
- **Git identity is set repo-locally.** Do not change it or pass `--author`.
- **Screen constraint:** anything opening a window, capturing, or moving the OS cursor uses the **portrait secondary monitor (x ≥ 2560)**. The 2560×1080 primary at (0,0) is the user's.
- **Synthetic input cannot reach the app's `WH_MOUSE_LL` hook.** `SendInput` from any tool-spawned shell returns **0** (zero events accepted); `SetCursorPos` moves the pointer but generates no hook event. **Do not attempt to verify hover behaviour by injecting mouse movement — it cannot work.** `mcp__Windows-MCP__Scroll` *does* inject real wheel input (verified: the browser page scrolled). See Task 6.
- **Test totals drift.** Never hard-code a total without running the suite and reading it off.
- **`max_ocr_passes` defaults to `1`.** Tiling is off; nothing here depends on it.

**Task order is a spec requirement.** Tasks 1–3 (sticky + dedupe) land before Tasks 4–5 (scrolling): scrolling carries the only risk to system input, and per spec D11 it does not work without the dedupe.

---

### Task 1: The sticky region

**Files:**
- Modify: `src/geom.rs`

**Interfaces:**
- Consumes: `PhysPoint`, `PhysRect`, `PhysRect::contains`.
- Produces:
  - `pub fn sticky_region(anchor: PhysRect, popup: PhysRect) -> [PhysRect; 3]`
  - `pub fn in_sticky(p: PhysPoint, anchor: PhysRect, popup: PhysRect) -> bool`

Pure geometry. No `windows` dependency, no screen.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/geom.rs`, at the end before the closing brace:

```rust
    /// The popup as `place_popup` produces it: flush with the anchor's left
    /// edge, POPUP_GAP below its bottom.
    fn anchor_and_popup() -> (PhysRect, PhysRect) {
        (r(100, 100, 26, 27), r(100, 139, 420, 300))
    }

    #[test]
    fn the_anchor_and_the_popup_are_both_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(in_sticky(p(113, 113), a, pop), "anchor centre");
        assert!(in_sticky(p(310, 289), a, pop), "popup centre");
        assert!(in_sticky(p(100, 100), a, pop), "anchor top-left is inclusive");
        assert!(in_sticky(p(519, 438), a, pop), "popup bottom-right interior");
    }

    #[test]
    fn the_bridge_covers_the_gap_between_them() {
        let (a, pop) = anchor_and_popup();
        for y in 127..139 {
            assert!(in_sticky(p(113, y), a, pop), "gap row {y} must be sticky");
        }
    }

    /// `contains` is inclusive of the top-left and exclusive of the
    /// bottom-right, so the three rects must tile with no missed row.
    #[test]
    fn the_three_rects_tile_without_a_seam() {
        let (a, pop) = anchor_and_popup();
        for y in 100..439 {
            assert!(in_sticky(p(113, y), a, pop), "row {y} fell through a seam");
        }
    }

    /// D2a's actual guarantee: straight down from the anchor's centre.
    #[test]
    fn the_vertical_path_into_the_popup_never_leaves_the_region() {
        for ax in [-900, 0, 2560, 3400] {
            for ay in [-40, 0, 500, 1800] {
                for (pw, ph) in [(420, 300), (200, 60), (420, 800)] {
                    let a = r(ax, ay, 26, 27);
                    let pop = r(ax, ay + 27 + 12, pw, ph);
                    let cx = ax + 13;
                    for y in ay..(ay + 27 + 12 + ph) {
                        assert!(in_sticky(p(cx, y), a, pop),
                                "anchor ({ax},{ay}) popup {pw}x{ph}: row {y}");
                    }
                }
            }
        }
    }

    /// `place_popup` flips the popup above the anchor near a monitor's
    /// bottom edge, so the bridge must be computed from whichever rect is
    /// upper rather than assuming the popup is below.
    #[test]
    fn the_bridge_works_with_the_popup_above_the_anchor() {
        let a = r(100, 900, 26, 27);
        let pop = r(100, 588, 420, 300);
        for y in 888..900 {
            assert!(in_sticky(p(113, y), a, pop), "gap row {y} above the anchor");
        }
        assert!(in_sticky(p(310, 700), a, pop), "popup centre");
    }

    /// THE assertion that fails if anyone replaces the three rects with
    /// their bounding box. The next character along the line must stay
    /// hoverable, or scanning a line freezes the popup on the previous word.
    #[test]
    fn the_next_character_along_the_line_is_not_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(!in_sticky(p(139, 113), a, pop), "one glyph right of the anchor");
        assert!(!in_sticky(p(300, 113), a, pop), "far along the same line");
    }

    /// Pinned deliberately (spec D2a): a shallow diagonal DOES leave the
    /// region, exiting onto the neighbouring character. That is correct
    /// behaviour, and pinning it stops a future widening of the bridge from
    /// silently breaking `the_next_character_along_the_line_is_not_sticky`.
    #[test]
    fn a_shallow_diagonal_leaves_the_region_on_purpose() {
        let (a, pop) = anchor_and_popup();
        assert!(!in_sticky(p(127, 126), a, pop),
                "exits the anchor's right edge one row before the bridge");
    }

    #[test]
    fn a_point_well_away_from_both_is_not_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(!in_sticky(p(50, 50), a, pop));
        assert!(!in_sticky(p(1000, 1000), a, pop));
    }

    /// A zero-height bridge must contribute nothing rather than being
    /// treated as a containing rect - the same rule `inset` applies.
    #[test]
    fn a_zero_gap_needs_no_bridge_and_still_tiles() {
        let a = r(100, 100, 26, 27);
        let pop = r(100, 127, 420, 300);
        for y in 100..427 {
            assert!(in_sticky(p(113, y), a, pop), "row {y} with gap 0");
        }
    }

    #[test]
    fn sticky_region_returns_the_anchor_the_popup_and_the_bridge() {
        let (a, pop) = anchor_and_popup();
        let rects = sticky_region(a, pop);
        assert_eq!(a, rects[0]);
        assert_eq!(pop, rects[1]);
        assert_eq!(r(100, 127, 420, 12), rects[2], "bridge spans the union's x-extent");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib geom 2>&1 | tail -20
```

Expected: `cannot find function 'in_sticky' in this scope`. **Paste the real output — a paraphrased RED transcript is a task failure.**

- [ ] **Step 3: Write the implementation**

Add to `src/geom.rs`, immediately after the `ScanDisplay` impl block:

```rust
/// The three rectangles that keep a popup on screen: the hovered character,
/// the popup itself, and the **bridge** across the gap between them.
///
/// Returned in that order — `[anchor, popup, bridge]`.
///
/// **Three rectangles, deliberately not their bounding box.** The popup is
/// flush with the anchor's left edge and up to `POPUP_MAX_WIDTH` (420px)
/// wide, while the anchor is one glyph (~26px). A bounding box would
/// therefore also cover ~400px of screen *beside* the hovered character, at
/// that character's own height — so scanning sideways along a line of text
/// would hold the popup on the previous word and the next word could never
/// be read. The bridge is only the gap tall, so brushing it on the way to
/// the line below holds nothing.
///
/// The bridge is derived from whichever rect is upper, because
/// [`place_popup`] flips the popup above the anchor near a monitor's bottom
/// edge.
pub fn sticky_region(anchor: PhysRect, popup: PhysRect) -> [PhysRect; 3] {
    [anchor, popup, bridge_between(anchor, popup)]
}

/// The gap band between two rects, spanning their combined x-extent.
///
/// Zero or negative height when they touch or overlap; [`in_sticky`] treats
/// that as containing nothing, and the two rects are adjacent in that case
/// so nothing is lost.
fn bridge_between(a: PhysRect, b: PhysRect) -> PhysRect {
    let (upper, lower) = if a.y <= b.y { (a, b) } else { (b, a) };
    let top = upper.y + upper.h;
    let left = a.x.min(b.x);
    let right = (a.x + a.w).max(b.x + b.w);
    PhysRect { x: left, y: top, w: right - left, h: lower.y - top }
}

/// Whether `p` is on the hovered word, on its popup, or in the gap between.
///
/// While this is true the application dispatches no trigger at all, so the
/// popup stays exactly as it is (spec D3).
///
/// **What this guarantees, and what it does not** (spec D2a): the vertical
/// path from the anchor's centre into the popup is entirely covered, as is
/// any approach steeper than roughly 45°. A *shallower* diagonal leaves the
/// region by the anchor's side edge before reaching the bridge — landing on
/// the neighbouring character, which is then correctly resolved as the word
/// the cursor actually moved to. Covering that strip instead is precisely
/// what would break sideways scanning, so the two cannot both hold.
pub fn in_sticky(p: PhysPoint, anchor: PhysRect, popup: PhysRect) -> bool {
    sticky_region(anchor, popup)
        .iter()
        .any(|r| r.w > 0 && r.h > 0 && r.contains(p))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib geom 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 5: Prove the three-rect choice is load-bearing**

Temporarily replace the body of `sticky_region`'s third element with the bounding box:

```rust
    let left = anchor.x.min(popup.x);
    let top = anchor.y.min(popup.y);
    let right = (anchor.x + anchor.w).max(popup.x + popup.w);
    let bottom = (anchor.y + anchor.h).max(popup.y + popup.h);
    [anchor, popup, PhysRect { x: left, y: top, w: right - left, h: bottom - top }]
```

Re-run and confirm `the_next_character_along_the_line_is_not_sticky` **FAILS**. Restore and confirm it passes. Capture both outputs verbatim. A design decision whose test passes either way is not being tested.

- [ ] **Step 6: Check clippy, then commit**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
git add src/geom.rs
git commit -m "feat(geom): the sticky region that holds a popup on screen"
```

Expected clippy: exactly the 5 accepted lines.

---

### Task 2: The app remembers what is on screen, and freezes while the cursor is on it

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `in_sticky` (Task 1); `place_popup`, `Presentation`, `PhysRect`.
- Produces: `struct Shown { anchor: PhysRect, popup: PhysRect, presentation: Presentation }`; `show_presentation` returns the placed rect.

**Feature 1 of three.** No scrolling yet — `Shown` grows in Task 4.

- [ ] **Step 1: Make `show_presentation` return where it put the window**

`show_presentation` currently returns `Result<()>`. Change its signature to `Result<PhysRect>` and end it with `Ok(rect)` — the caller needs the placed rectangle and must never re-derive it. In `src/app.rs`, the last statements of that function become:

```rust
    let rect = place_popup(anchor, (w, h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer.paint(presentation, theme).context("painting the popup")?;
    Ok(rect)
```

(Keep whatever order of `show_at`/`paint` the existing body already uses; only the return changes.)

- [ ] **Step 2: Add the `Shown` struct**

Add above `fn handle_worker_outcome` in `src/app.rs`:

```rust
/// What is on screen right now. `None` whenever the popup is hidden.
///
/// **Must never outlive the visible window.** While this is `Some`,
/// [`geom::in_sticky`] suppresses trigger dispatch for its region; a `Shown`
/// left behind by a hidden popup would therefore turn a patch of the screen
/// into a dead zone where hovering silently does nothing. It is set only
/// after `show_presentation` has succeeded and cleared on every hide path.
struct Shown {
    /// The hovered character's own box.
    anchor: PhysRect,
    /// Where `place_popup` put the window — stored, never re-derived.
    popup: PhysRect,
    presentation: Presentation,
}
```

- [ ] **Step 3: Thread `Option<Shown>` through the outcome handler**

`handle_worker_outcome` gains a `shown: &mut Option<Shown>` parameter. Every arm maintains it:

```rust
        WorkerOutcome::Hide => {
            let _ = popup.hide();
            if let Some(ov) = overlay {
                ov.hide();
            }
            *shown = None;
        }
        WorkerOutcome::Failed(msg) => {
            eprintln!("chibipop: hover lookup failed: {msg}");
            let _ = popup.hide();
            if let Some(ov) = overlay {
                ov.hide();
            }
            *shown = None;
        }
        WorkerOutcome::Ready { presentation, anchor, scan } => {
            match show_presentation(popup, renderer, theme, max_height_percent, &presentation, anchor)
            {
                Err(e) => {
                    eprintln!("chibipop: showing the popup failed: {e:#}");
                    let _ = popup.hide();
                    if let Some(ov) = overlay {
                        ov.hide();
                    }
                    *shown = None;
                }
                Ok(rect) => {
                    *shown = Some(Shown { anchor, popup: rect, presentation });
                    if let Some(ov) = overlay {
                        if let Err(e) = ov.show_rects(&scan, theme) {
                            eprintln!("chibipop: showing the scan overlay failed: {e:#}");
                        }
                    }
                }
            }
        }
```

In `run`, declare `let mut shown: Option<Shown> = None;` beside the other loop state (near `let mut overlay_prev_visible = false;`) and pass `&mut shown` at the `handle_worker_outcome` call site.

- [ ] **Step 4: Suppress dispatch while sticky**

In `run`'s message loop, the `WM_TIMER` arm currently reads:

```rust
        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            if let Some(cursor) = Hooks::take_pending() {
                next_id += 1;
                latest_dispatched = RequestId(next_id);
                let _ = trigger_tx.send(Trigger { cursor, id: latest_dispatched });
            }
        } else if msg.message == WM_APP_RESULT {
```

Replace the inner block with:

```rust
        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            if let Some(cursor) = Hooks::take_pending() {
                // Spec D3: on the word or its popup, change nothing.
                let frozen = shown
                    .as_ref()
                    .is_some_and(|s| in_sticky(cursor, s.anchor, s.popup));
                if !frozen {
                    next_id += 1;
                    latest_dispatched = RequestId(next_id);
                    let _ = trigger_tx.send(Trigger { cursor, id: latest_dispatched });
                }
            }
        } else if msg.message == WM_APP_RESULT {
```

Note the pending point is still **consumed** whether or not it is dispatched, so a stale position cannot be delivered later.

- [ ] **Step 5: Clear `Shown` when the trigger mode changes**

Find where `TrayCommand::SetMode` is handled in `run` and add `shown = None;` alongside the existing `Hooks::set_mode` call. A mode change must not leave a sticky region from the previous mode alive.

- [ ] **Step 6: Add the import and verify**

Add `in_sticky` to the `use crate::geom::{...}` line at the top of `src/app.rs`.

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo clippy --all-targets --all-features -- -D warnings -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -E "^error|^warning" | head
cargo build --release 2>&1 | grep -E "^error|Finished"
```

The third command must print nothing. If `handle_worker_outcome` crosses 7 parameters, clippy's `too_many_arguments` will fire as a **sixth** error — if so, add `#[allow(clippy::too_many_arguments)]` to it (the precedent `resolve_trigger` already sets) and say so in the report.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): hold the popup while the cursor is on it"
```

---

### Task 3: Do not re-render when the answer has not changed

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `Shown` (Task 2), `Presentation`'s `PartialEq`.
- Produces: `const ANCHOR_JITTER_PX: i32`; `fn same_content(prev: &Shown, new: &Presentation, anchor: PhysRect) -> bool`.

**Spec D10/D11.** This is a prerequisite for Task 4, not an optional polish: without it any jitter would reset the scroll offset and a long entry could not be read.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/app.rs`:

```rust
    fn shown_of(written: &str, anchor: PhysRect) -> Shown {
        Shown {
            anchor,
            popup: PhysRect { x: anchor.x, y: anchor.y + anchor.h + POPUP_GAP, w: 420, h: 300 },
            presentation: presentation_of(written),
        }
    }

    fn presentation_of(written: &str) -> Presentation {
        Presentation {
            top: Some(Card {
                written: Some(written.to_string()),
                reading: None,
                pos: vec![],
                freq: None,
                blocks: vec![],
                match_len: 2,
            }),
            collapsed: vec![],
        }
    }

    /// UPSCALE is 2, so every anchor is re-measured through integer division
    /// from a differently-framed capture: +/-1px per edge. An exact-match
    /// test would fail spuriously and re-render anyway.
    #[test]
    fn an_equal_card_with_a_jittered_anchor_is_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        let jittered = PhysRect { x: 101, y: 199, w: 26, h: 27 };
        assert!(same_content(&prev, &presentation_of("宿舎"), jittered));
    }

    /// Two occurrences of one word on screen produce identical
    /// presentations, and the popup genuinely has to move.
    #[test]
    fn an_equal_card_that_moved_is_not_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("猫", a);
        let elsewhere = PhysRect { x: 700, y: 900, w: 26, h: 27 };
        assert!(!same_content(&prev, &presentation_of("猫"), elsewhere));
    }

    #[test]
    fn a_different_card_at_the_same_anchor_is_not_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        assert!(!same_content(&prev, &presentation_of("駅長"), a));
    }

    /// Exactly at the tolerance must still count as unchanged; one past it
    /// must not.
    #[test]
    fn the_jitter_tolerance_is_inclusive_and_bounded() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        let at = PhysRect { x: 100 + ANCHOR_JITTER_PX, y: 200, w: 26, h: 27 };
        let past = PhysRect { x: 100 + ANCHOR_JITTER_PX + 1, y: 200, w: 26, h: 27 };
        assert!(same_content(&prev, &presentation_of("宿舎"), at));
        assert!(!same_content(&prev, &presentation_of("宿舎"), past));
    }
```

Add `use crate::present::Card;` to the test module's imports if it is not already reachable through `use super::*`.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib app 2>&1 | tail -20
```

Expected: `cannot find function 'same_content'`. Paste the real output.

- [ ] **Step 3: Write the implementation**

Add near `POPUP_GAP` in `src/app.rs`:

```rust
/// How far an anchor may move and still count as the same hover.
///
/// Not slop. `text::capture::UPSCALE` is 2, so every OCR word box is
/// re-measured through `PhysRect::scaled_down`'s integer division from a
/// differently-framed capture — ±1px per edge. An exact comparison would
/// fail spuriously and re-render on every hover, defeating the check.
const ANCHOR_JITTER_PX: i32 = 4;
```

And beside `show_presentation`:

```rust
/// Whether a new outcome would draw exactly what is already on screen.
///
/// Keyed on **content**, because the question is literally "would the
/// rendered output be identical". That also covers the case `main.rs`'s
/// `watch` documents: the recognised line gains or loses characters at
/// either end as the capture region slides, changing `cursor_byte_offset`
/// while the hovered glyph — and therefore the hits — stay the same.
///
/// The anchor is part of the key regardless, because two occurrences of one
/// word on screen produce equal presentations and the popup must still move.
fn same_content(prev: &Shown, new: &Presentation, anchor: PhysRect) -> bool {
    prev.presentation == *new
        && (prev.anchor.x - anchor.x).abs() <= ANCHOR_JITTER_PX
        && (prev.anchor.y - anchor.y).abs() <= ANCHOR_JITTER_PX
}
```

- [ ] **Step 4: Use it in the `Ready` arm**

At the top of `WorkerOutcome::Ready`'s arm in `handle_worker_outcome`, before calling `show_presentation`:

```rust
        WorkerOutcome::Ready { presentation, anchor, scan } => {
            if let Some(prev) = shown.as_ref() {
                if same_content(prev, &presentation, anchor) {
                    return; // Already on screen, unchanged.
                }
            }
```

The overlay is intentionally left alone on this path: the match box is derived from the same anchor and content, so it is already correct.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 6: Check clippy, then commit**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
git add src/app.rs
git commit -m "fix(app): do not redraw a popup that would look identical"
```

---

### Task 4: `render.rs` reports natural height and paints at a scroll offset

**Files:**
- Modify: `src/ui/render.rs`, `src/ui/theme.rs`, `src/app.rs`

**Interfaces:**
- Consumes: `Theme::dimmed_text`.
- Produces:
  - `Theme` gains nothing; `pub const SCROLLBAR_W: i32 = 4;` and `pub const SCROLLBAR_MIN_THUMB: i32 = 16;` in `src/ui/theme.rs`.
  - `Renderer::measure(...) -> Result<(i32, i32, i32)>` — `(w, view_h, content_h)`.
  - `Renderer::paint(&mut self, p: &Presentation, theme: &Theme, scroll: i32) -> Result<()>`.
  - `pub fn max_scroll(content_h: i32, view_h: i32) -> i32`
  - `pub fn scrollbar_thumb(track_h: i32, content_h: i32, view_h: i32, scroll: i32) -> Option<(i32, i32)>`
- `Shown` gains `scroll: i32`, `content_h: i32`, `view_h: i32`.

**The window's height still never exceeds the cap.** `place_popup`'s anchor-never-covered proof depends on that; scrolling moves content inside a fixed window.

- [ ] **Step 1: Write the failing tests for the pure functions**

Add to `mod tests` in `src/ui/render.rs`:

```rust
    #[test]
    fn content_that_fits_cannot_scroll() {
        assert_eq!(0, max_scroll(200, 300));
        assert_eq!(0, max_scroll(300, 300));
    }

    #[test]
    fn max_scroll_is_the_overflow() {
        assert_eq!(200, max_scroll(500, 300));
    }

    #[test]
    fn content_that_fits_has_no_thumb() {
        assert_eq!(None, scrollbar_thumb(300, 200, 300, 0));
        assert_eq!(None, scrollbar_thumb(300, 300, 300, 0));
    }

    #[test]
    fn the_thumb_is_proportional_and_starts_at_the_top() {
        let (top, h) = scrollbar_thumb(300, 600, 300, 0).unwrap();
        assert_eq!(0, top);
        assert_eq!(150, h, "half the content is visible, so half the track");
    }

    /// At the bottom the thumb must be flush with the track's end, or the
    /// popup looks unscrolled when it is fully scrolled.
    #[test]
    fn the_thumb_ends_flush_with_the_track_at_max_scroll() {
        let (top, h) = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
        assert_eq!(300, top + h);
    }

    /// A very long entry would otherwise produce a 1px sliver.
    #[test]
    fn the_thumb_has_a_floor() {
        let (_, h) = scrollbar_thumb(300, 100_000, 300, 0).unwrap();
        assert_eq!(SCROLLBAR_MIN_THUMB, h);
    }

    /// The floor must not push the thumb past the track's end.
    #[test]
    fn a_floored_thumb_still_ends_inside_the_track() {
        let m = max_scroll(100_000, 300);
        let (top, h) = scrollbar_thumb(300, 100_000, 300, m).unwrap();
        assert!(top + h <= 300, "thumb {top}+{h} escaped a 300px track");
        assert_eq!(300, top + h);
    }

    #[test]
    fn a_scroll_beyond_the_end_is_treated_as_the_end() {
        let a = scrollbar_thumb(300, 600, 300, 999_999).unwrap();
        let b = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
        assert_eq!(b, a);
    }
```

- [ ] **Step 2: Run them to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib ui::render 2>&1 | tail -20
```

Expected: `cannot find function 'max_scroll'`. Paste it.

- [ ] **Step 3: Add the theme constants**

In `src/ui/theme.rs`, above the `Theme` struct:

```rust
/// Width of the scrollbar track and thumb, in physical pixels.
pub const SCROLLBAR_W: i32 = 4;

/// Shortest the thumb may get, so a very long entry still shows a thumb
/// rather than a one-pixel sliver.
pub const SCROLLBAR_MIN_THUMB: i32 = 16;
```

- [ ] **Step 4: Write the two pure functions**

In `src/ui/render.rs`, above `impl Renderer`:

```rust
/// How far the content can scroll: the overflow past the visible height, or
/// zero when it fits.
pub fn max_scroll(content_h: i32, view_h: i32) -> i32 {
    (content_h - view_h).max(0)
}

/// The scrollbar thumb as `(top, height)` within a `track_h`-tall track, or
/// `None` when the content fits and no scrollbar should be drawn.
///
/// The thumb's height is the visible fraction of the content, floored at
/// [`SCROLLBAR_MIN_THUMB`]. The floor is applied *before* positioning, so a
/// floored thumb still ends flush with the track at full scroll rather than
/// overhanging it.
pub fn scrollbar_thumb(
    track_h: i32,
    content_h: i32,
    view_h: i32,
    scroll: i32,
) -> Option<(i32, i32)> {
    let span = max_scroll(content_h, view_h);
    if span == 0 || track_h <= 0 || content_h <= 0 {
        return None;
    }
    let ideal = (track_h as i64 * view_h as i64 / content_h as i64) as i32;
    let thumb_h = ideal.clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
    let travel = track_h - thumb_h;
    let at = scroll.clamp(0, span);
    let top = (travel as i64 * at as i64 / span as i64) as i32;
    Some((top, thumb_h))
}
```

Add `use crate::ui::theme::{SCROLLBAR_MIN_THUMB, SCROLLBAR_W};` to `render.rs`'s imports (`SCROLLBAR_W` is used in Step 7).

- [ ] **Step 5: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib ui::render 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 6: Stop `layout_pass` clamping; make it scroll instead**

`layout_pass` currently breaks out of its loop when an element would exceed `content_h`, sets `clamped = true`, and afterwards draws `CLAMP_MARKER`. Replace that behaviour:

- Change its signature: drop nothing, **add** `scroll: f32` as the final parameter, and change the return type from `windows::core::Result<(f32, bool)>` to `windows::core::Result<f32>` (the used height only).
- Delete `let mut clamped = false;`.
- In the `Elem::Separator` arm, delete the `if y + top_gap + h > content_h { clamped = true; break; }` block.
- In the `Elem::Text` arm, delete the `if y + line.top_gap + h > content_h { clamped = true; break; }` block.
- In the `Elem::Corner` arm, change `if m.height > content_h { continue; }` to `if m.height > content_h && scroll == 0.0 { continue; }` — the corner is decoration and skipping it once it cannot fit is existing behaviour worth keeping for the unscrolled case.
- Both drawing calls change their `Y` from `origin_y + y` to `origin_y + y - scroll`.
- Delete the entire `if clamped { ... }` marker block after the loop, and delete the `CLAMP_MARKER` constant.
- Return `Ok(y)`.

D2D clips drawing to the render target, so a negative `Y` renders a partially-visible line at the top edge, which is exactly what smooth scrolling should look like. Nothing needs to skip off-screen elements for correctness; a `Presentation` is bounded (at most ten hits, each summary capped at `summary_chars`).

Update the module doc comment and `layout_pass`'s own rustdoc: the sentence describing the shared measure/paint clamp, and any mention of the truncation marker, are now false. **Replace them with what the code does — do not just delete them.**

- [ ] **Step 7: Draw the scrollbar in `paint_once`**

`paint_once` gains a `scroll: i32` parameter. Inside its draw closure, replace the `layout_pass` call and add the scrollbar after it:

```rust
            let content_w = (w - 2 * theme.padding).max(0) as f32;
            let content_h = (h - 2 * theme.padding).max(0) as f32;
            let origin = theme.padding as f32;
            let used = layout_pass(
                &self.dwrite_factory,
                Some(target),
                &elems,
                theme,
                origin,
                origin,
                content_w,
                content_h,
                scroll as f32,
            )?;

            let total = used.ceil() as i32 + 2 * theme.padding;
            if let Some((top, thumb_h)) =
                scrollbar_thumb(h - 2 * theme.padding, total, h, scroll)
            {
                let brush =
                    unsafe { target.CreateSolidColorBrush(&color_f(theme.dimmed_text), None) }?;
                let x = (w - theme.padding / 2 - SCROLLBAR_W) as f32;
                let rect = D2D_RECT_F {
                    left: x,
                    top: (theme.padding + top) as f32,
                    right: x + SCROLLBAR_W as f32,
                    bottom: (theme.padding + top + thumb_h) as f32,
                };
                unsafe { target.FillRectangle(&rect, &brush) };
            }
            Ok(())
```

- [ ] **Step 8: Update `measure` and `paint`**

`measure` returns `(w, view_h, content_h)`:

```rust
    pub fn measure(
        &self,
        p: &Presentation,
        theme: &Theme,
        max_w: i32,
        max_h: i32,
    ) -> Result<(i32, i32, i32)> {
        let elems = build_elements(p, theme);
        let content_w = (max_w - 2 * theme.padding).max(0) as f32;
        let used_h = layout_pass(
            &self.dwrite_factory,
            None,
            &elems,
            theme,
            0.0,
            0.0,
            content_w,
            f32::MAX,
            0.0,
        )
        .context("measuring popup content")?;
        let content_h = used_h.ceil() as i32 + 2 * theme.padding;
        Ok((max_w, content_h.min(max_h), content_h))
    }
```

`f32::MAX` as the content height is what makes the measure pass unclamped: nothing is drawn in measure mode, so a huge box only means the walk visits every element.

`paint` takes and forwards `scroll`, passing it to both `paint_once` calls (the initial one and the device-lost retry).

Replace `measure`'s long rustdoc paragraphs about the `bool` third element and the truncation marker: the third element is now the natural content height, and the height cap is still enforced by the `.min(max_h)` for exactly the `place_popup` reason the old comment gave. **Keep that reason.**

- [ ] **Step 9: Update the callers in `app.rs`**

`Shown` gains three fields:

```rust
    /// Vertical content offset in physical pixels; 0 is the top.
    scroll: i32,
    /// Natural content height, unclamped — what `scroll` ranges against.
    content_h: i32,
    /// Visible height, which is the window's height.
    view_h: i32,
```

`show_presentation` takes a `scroll: i32` parameter and returns `Result<(PhysRect, i32, i32)>` — the placed rect, `content_h`, and `view_h`:

```rust
    let (w, view_h, content_h) = renderer
        .measure(presentation, theme, max_w, max_h)
        .context("measuring popup content")?;
    let rect = place_popup(anchor, (w, view_h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer.paint(presentation, theme, scroll).context("painting the popup")?;
    Ok((rect, content_h, view_h))
```

In the `Ready` arm, pass `0` for `scroll` (a new word always starts at the top) and store all three:

```rust
                Ok((rect, content_h, view_h)) => {
                    *shown = Some(Shown {
                        anchor,
                        popup: rect,
                        presentation,
                        scroll: 0,
                        content_h,
                        view_h,
                    });
```

Task 3's `same_content` early-return path keeps the existing `Shown` untouched, so its `scroll` survives — which is the whole reason Task 3 comes first.

- [ ] **Step 10: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo clippy --all-targets --all-features -- -D warnings -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -E "^error|^warning" | head
cargo build --release 2>&1 | grep -E "^error|Finished"
```

`layout_pass` has 9 parameters now, but it is a private free function in a file that already carries an accepted `too_many_arguments` — confirm the clippy count is still exactly 5 and report it if the attribution moved.

- [ ] **Step 11: Commit**

```bash
git add src/ui/render.rs src/ui/theme.rs src/app.rs
git commit -m "feat(ui): scrollable popup content with a scrollbar"
```

---

### Task 5: The wheel

**Files:**
- Modify: `src/input/hooks.rs`, `src/config.rs`, `src/app.rs`, `README.md`

**Interfaces:**
- Consumes: `in_sticky` (Task 1), `Shown` with its scroll fields (Task 4), `max_scroll` (Task 4).
- Produces:
  - `Hooks::set_scroll_armed(bool)`, `Hooks::take_scroll() -> i32`
  - `PopupConfig::scroll_popup: bool`
  - `const SCROLL_STEP_PX: i32 = 48;` in `src/app.rs`

- [ ] **Step 1: Write the config tests**

Add to `mod tests` in `src/config.rs`:

```rust
    #[test]
    fn popup_scrolling_defaults_on() {
        assert!(Config::default().popup.scroll_popup);
    }

    #[test]
    fn disabled_scrolling_round_trips() {
        let p = tmp("scroll_off");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.scroll_popup = false;
        c.save(&p).unwrap();
        assert!(!load_or_create(&p).unwrap().popup.scroll_popup);
        let _ = std::fs::remove_file(&p);
    }

    /// The bare-`serde(default)` trap again: a `[popup]` section written
    /// before this field existed must load with scrolling ON.
    #[test]
    fn a_config_written_before_scroll_popup_loads_with_it_on() {
        let p = tmp("no_scroll_field");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-scroll_popup config must still load");
        assert!(c.popup.scroll_popup, "a missing field must take the field default");
        let _ = std::fs::remove_file(&p);
    }
```

- [ ] **Step 2: Run them to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib config 2>&1 | tail -12
```

Expected: `no field 'scroll_popup' on type 'PopupConfig'`. Paste it.

- [ ] **Step 3: Add the config field**

In `src/config.rs`, after `highlight_match` in `PopupConfig`:

```rust
    /// Let the wheel scroll a popup whose content overflows the height cap.
    ///
    /// On by default. Turning it off disables **only** the wheel handling —
    /// the scrollbar is still drawn, so overflowing content is still visibly
    /// marked as overflowing rather than silently cut. This is the escape
    /// hatch for the one part of the feature that can affect input outside
    /// chibipop: while armed, the low-level hook swallows wheel events so
    /// the window underneath does not scroll too.
    #[serde(default = "default_scroll_popup")]
    pub scroll_popup: bool,
```

with, beside `default_highlight_match`:

```rust
/// On. See [`PopupConfig::scroll_popup`].
fn default_scroll_popup() -> bool {
    true
}
```

and `scroll_popup: default_scroll_popup(),` in `Config::default()`. Add `scroll_popup: true,` to `popup_config()` in `src/app.rs`'s test module.

- [ ] **Step 4: Add the hook statics and accessors**

In `src/input/hooks.rs`, beside the other statics:

```rust
/// Whether the popup currently wants wheel events. Written by the main
/// thread from the live cursor position every dispatch tick, read by the
/// mouse hook.
///
/// A stuck `true` would disable the scroll wheel for every application until
/// chibipop exits, so it is recomputed from scratch on every tick rather
/// than latched on an edge, cleared on every path that hides the popup, and
/// cleared again in `Hooks::drop` so even the panic path restores the wheel.
static SCROLL_ARMED: AtomicBool = AtomicBool::new(false);

/// Wheel delta accumulated while armed, in `WHEEL_DELTA` units. Drained by
/// `take_scroll`.
static PENDING_SCROLL: AtomicI32 = AtomicI32::new(0);
```

Add `AtomicI32` to the `std::sync::atomic` import. Then, in `impl Hooks`:

```rust
    /// Arms or disarms wheel capture. See [`SCROLL_ARMED`].
    pub fn set_scroll_armed(armed: bool) {
        SCROLL_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Takes the accumulated wheel delta, in `WHEEL_DELTA` units, and resets
    /// the accumulator. Positive is a scroll up (away from the user), matching
    /// Win32's sign convention.
    pub fn take_scroll() -> i32 {
        PENDING_SCROLL.swap(0, Ordering::SeqCst)
    }
```

- [ ] **Step 5: Handle the wheel in the hook**

`mouse_hook_proc` currently reads:

```rust
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEMOVE {
        let _ = catch_unwind(|| unsafe { record_mouse_move(lparam) });
    }
    CallNextHookEx(None, code, wparam, lparam)
}
```

Replace with:

```rust
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        match wparam.0 as u32 {
            WM_MOUSEMOVE => {
                let _ = catch_unwind(|| unsafe { record_mouse_move(lparam) });
            }
            WM_MOUSEWHEEL if SCROLL_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe { record_wheel(lparam) });
                return LRESULT(1);
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
```

and add beside `record_mouse_move`:

```rust
/// Accumulate one wheel event's delta.
///
/// `mouseData`'s high word carries a signed `WHEEL_DELTA` multiple; the low
/// word is unused for wheel events. `saturating_add` because this
/// accumulates unboundedly between drains if the main thread ever stalls.
unsafe fn record_wheel(lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` only calls this with code >= 0 and
    // wparam == WM_MOUSEWHEEL, the WH_MOUSE_LL contract that guarantees
    // lparam is a valid, aligned MSLLHOOKSTRUCT pointer for this call.
    let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let delta = (data.mouseData >> 16) as i16 as i32;
    let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
        Some(v.saturating_add(delta))
    });
}
```

Import `WM_MOUSEWHEEL` from `windows::Win32::UI::WindowsAndMessaging`.

**Amend the module documentation.** `mouse_hook_proc`'s existing comment states that "the early-return-free structure below is deliberate, not incidental: there is exactly one `return`-shaped statement in this function." That is now false. Replace it with a note that there is exactly **one** path which does not call `CallNextHookEx` — a wheel event consumed while armed — and that consuming it is the point, since otherwise the window underneath would scroll as well. A comment that has silently stopped being true is worse than no comment.

- [ ] **Step 6: Clear the arm in `Drop`**

In `impl Drop for Hooks`, before the unhook calls:

```rust
        // Never leave the wheel captured.
        SCROLL_ARMED.store(false, Ordering::SeqCst);
```

- [ ] **Step 7: Arm, drain and apply in `app.rs`**

Add beside `ANCHOR_JITTER_PX`:

```rust
/// Pixels of content scrolled per `WHEEL_DELTA` notch.
///
/// A feel constant, not a measured one — expect to tune it against a real
/// overflowing entry, the way `REGION_W`/`REGION_H` were tuned.
const SCROLL_STEP_PX: i32 = 48;
```

In `run`, capture `let scroll_popup = cfg.popup.scroll_popup;` beside the other config reads, and extend the `WM_TIMER` arm. It becomes:

```rust
        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            let live = cursor_now();
            let armed = scroll_popup
                && shown.as_ref().is_some_and(|s| {
                    in_sticky(live, s.anchor, s.popup) && s.content_h > s.view_h
                });
            Hooks::set_scroll_armed(armed);

            let notches = Hooks::take_scroll();
            if notches != 0 {
                if let Some(s) = shown.as_mut() {
                    let span = max_scroll(s.content_h, s.view_h);
                    // Win32 wheel is positive away from the user.
                    let want = s.scroll - (notches / 120) * SCROLL_STEP_PX;
                    let next = want.clamp(0, span);
                    if next != s.scroll {
                        s.scroll = next;
                        if let Err(e) = renderer.paint(&s.presentation, &theme, s.scroll) {
                            eprintln!("chibipop: repainting for scroll failed: {e:#}");
                        }
                    }
                }
            }

            if let Some(cursor) = Hooks::take_pending() {
                // Spec D3: on the word or its popup, change nothing.
                let frozen = shown
                    .as_ref()
                    .is_some_and(|s| in_sticky(cursor, s.anchor, s.popup));
                if !frozen {
                    next_id += 1;
                    latest_dispatched = RequestId(next_id);
                    let _ = trigger_tx.send(Trigger { cursor, id: latest_dispatched });
                }
            }
        } else if msg.message == WM_APP_RESULT {
```

and add a helper beside `monitor_rect_for`:

```rust
/// The pointer's position right now.
///
/// Read live rather than from `Hooks::take_pending`, which is gated at
/// `MOVEMENT_GATE_PX`: easing into the popup in small steps could otherwise
/// leave the wheel arm stale. From the live position the arm is correct
/// within one tick regardless of movement history, and it self-corrects from
/// any wrong state instead of latching.
fn cursor_now() -> PhysPoint {
    let mut pt = POINT::default();
    // SAFETY: FFI call taking a pointer to local stack storage that outlives
    // the call. Cannot fail in a way that matters - on failure `pt` stays
    // zeroed, which merely disarms the wheel for one tick.
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    PhysPoint { x: pt.x, y: pt.y }
}
```

Import `GetCursorPos` from `windows::Win32::UI::WindowsAndMessaging` and `max_scroll` from `crate::ui::render`.

Also clear the arm wherever `shown` is set to `None` in `handle_worker_outcome` — add `Hooks::set_scroll_armed(false);` beside each `*shown = None;`. The tick would clear it within 20ms anyway; doing it eagerly means a hidden popup never holds the wheel even for that long.

- [ ] **Step 8: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error: " | grep -v "could not compile" | sort | uniq -c
cargo clippy --all-targets --all-features -- -D warnings -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -E "^error|^warning" | head
cargo build --release 2>&1 | grep -E "^error|Finished"
```

- [ ] **Step 9: Document and commit**

In `README.md`'s configuration section, add `scroll_popup = true` to the `[popup]` block and a paragraph: what it does, that it is on by default, that a long entry scrolls with the wheel and shows a scrollbar, and that turning it off keeps the scrollbar but stops chibipop taking wheel events from the window underneath. Also add a line to the sticky behaviour: the popup now stays put while the cursor is on the word or on the popup itself.

```bash
git add src/input/hooks.rs src/config.rs src/app.rs README.md
git commit -m "feat(ui): scroll the popup with the wheel"
```

---

### Task 6: Verification — what an agent can prove, and the script for what it cannot

**Files:**
- Create: `docs/superpowers/findings/2026-07-28-popup-interaction-acceptance.md`

**This task measures. It does not tune.** If a number or behaviour misses, record it and stop — do not adjust `SCROLL_STEP_PX`, `ANCHOR_JITTER_PX`, or any constant to make it pass.

- [ ] **Step 1: Attempt the wheel path live**

`mcp__Windows-MCP__Scroll` injects real wheel input — verified 2026-07-28 by scrolling the browser (22.8% of sampled pixels changed). Note that `Move`/`Click` are **not** usable (their `loc` schema is untyped, so this harness serialises the array as a string).

The wheel path is therefore reachable *if* a popup can be put on screen, which needs a hover, which cannot be injected. So attempt it in this order and record honestly which step blocked:

1. Start `run` from PowerShell against the portrait secondary.
2. Park the pointer on Japanese text with `SetCursorPos` (this does move it) and check whether `ChibipopPopupClass` becomes visible. **It is expected not to** — record that.
3. If a popup *is* somehow visible, park the pointer inside it, call `mcp__Windows-MCP__Scroll` with `direction: "down"`, and screenshot before/after at full resolution to see whether the content moved and the thumb travelled.

Report the outcome either way. "Attempted and blocked at step 2" is a result.

- [ ] **Step 2: Verify what the tests can reach**

Record the final counts and the clippy state, and confirm the two load-bearing proofs were actually run:

- Task 1 Step 5's bounding-box substitution made `the_next_character_along_the_line_is_not_sticky` fail.
- The vertical-path sweep covers negative coordinates and the portrait secondary's x-range.

- [ ] **Step 3: Write the manual script for the user**

The document must end with a numbered list the user can work through in a couple of minutes, each item stating what to do and what counts as pass:

1. Hover a word with a short definition; move the cursor down into the popup. **Pass:** the popup does not change or vanish; you can move around inside it freely.
2. From inside the popup, move the cursor fully off it onto other text. **Pass:** normal hovering resumes immediately.
3. Hover a word and jiggle the mouse slightly on it. **Pass:** no flicker at all.
4. Hover a word along a line, then move sideways to the next word. **Pass:** the next word resolves — the popup does **not** stay stuck on the previous one. *(This is the one that fails if the sticky region is ever widened to a bounding box.)*
5. Find an entry long enough to overflow (a 大辞林 entry for a common word). **Pass:** a thin scrollbar appears at the right edge.
6. With the cursor in that popup, wheel down to the last line and back to the first. **Pass:** content moves, the thumb travels and ends flush with the bottom.
7. Move the cursor off the popup and wheel. **Pass:** the window underneath scrolls normally.
8. Quit chibipop from the tray, then wheel. **Pass:** the wheel still works. *(Guards spec D9 — the one failure mode that outlives the app.)*
9. Set `scroll_popup = false`, restart, hover the same long entry. **Pass:** the scrollbar still shows, the wheel scrolls the window underneath instead.

State plainly that items 1–9 are **unverified until the user runs them**, and that the round is not "done" in the acceptance sense until they do.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/findings/2026-07-28-popup-interaction-acceptance.md
git commit -m "docs: popup interaction acceptance and the manual script"
```

---

## Self-Review

**Spec coverage:** D1 (`Shown`) → Task 2 Step 2, extended in Task 4 Step 9. D2 (three rects, bridge, degenerate case, tiling) → Task 1. D2a (the corrected guarantee and the pinned non-property) → Task 1 Steps 1/3/5. D3 (dispatch suppression) → Task 2 Step 4. D4 (window height never grows) → Task 4 Step 8's `.min(max_h)`, kept with its reason. D5 (`measure` returns natural height; marker retired) → Task 4 Steps 6/8. D6 (scrollbar, `max_scroll`, `scrollbar_thumb`) → Task 4 Steps 3/4/7. D7 (statics, live cursor, swallow, drain) → Task 5 Steps 4/5/7. D8 (amend the invariant comment) → Task 5 Step 5. D9 (three mitigations + `scroll_popup`) → Task 5 Steps 3/6/7 and manual item 8. D10 (`same_content`) → Task 3. D11 (order) → the header's ordering note and Task 3 preceding Task 4. §6's error table → Task 2 Step 3, Task 3 Step 4, Task 4 Step 9, Task 5 Step 7. §7's test list → Tasks 1, 3, 4, 5 Step 1. §2's acceptance → Task 6.

**Gap found and closed:** §6 requires `Shown` to be cleared on a trigger-mode change, and no task step covered it. Added as Task 2 Step 5.

**Second gap found and closed:** §7 requires a test that the capture-kind/scrollbar interaction does not regress the `place_popup` height guarantee. Task 4 Step 8 keeps the `.min(max_h)` and its rationale explicitly rather than dropping it with the rest of the old doc comment; the existing `place_popup` tests continue to cover the guarantee itself.

**Placeholder scan:** none. Task 6 Step 1 is deliberately investigative with an explicit instruction to report which step blocked, because the outcome is genuinely unknown before running it.

**Type consistency:** `sticky_region(PhysRect, PhysRect) -> [PhysRect; 3]` and `in_sticky(PhysPoint, PhysRect, PhysRect) -> bool` defined in Task 1, consumed in Tasks 2 and 5. `Shown` defined in Task 2 with three fields, extended in Task 4 to six; Task 3's `same_content` reads only `presentation` and `anchor`, so it is unaffected by the extension. `measure -> (i32, i32, i32)` in Task 4 is consumed only by `show_presentation` in the same task. `show_presentation -> Result<PhysRect>` in Task 2 becomes `Result<(PhysRect, i32, i32)>` in Task 4 — both call sites are in the tasks that change it. `max_scroll(i32, i32) -> i32` defined in Task 4 Step 4, used in Task 4 Step 7 and Task 5 Step 7. `Hooks::set_scroll_armed(bool)` / `Hooks::take_scroll() -> i32` defined in Task 5 Step 4, used in Step 7. `SCROLLBAR_W`/`SCROLLBAR_MIN_THUMB` defined in Task 4 Step 3, used in Steps 4 and 7.

---

## Defects found in this plan while executing it

Recorded so future plans do not repeat them.

**1. A task that adds a struct field must be the task that reads it.** Task 2 stores
`Shown.presentation` and Task 3 is what reads it, so Task 2 alone produced a *sixth* clippy error
(`field never read`) against a "must stay at exactly 5" constraint. Tasks 2 and 3 had to land as one
commit. The same shape bit twice: Task 4's `SCROLLBAR_W` import is unused until Step 7, so it had to
be held back rather than added with the constant it belongs to.

**2. Task 4 Step 9 broke a test helper the plan never mentioned.** Extending `Shown` from three
fields to six breaks `shown_of` in Task 3's test module — a struct literal, so every field is
mandatory. The plan's own Self-Review reasoned about `same_content` surviving the extension and
missed the builder entirely, even though it got the equivalent case right for `popup_config` one
task later. **Grep for every construction site of a struct before extending it**, and list them in
the step.

**3. "Both drawing calls" was wrong — there are three.** Task 4 Step 6 said two `DrawTextLayout`
origins needed the scroll offset. The `Elem::Separator` arm positions a `FillRectangle` at
`origin_y + y` too, and missing it would have left the rule between the top card and the collapsed
rows detached from the content as it scrolled. No unit test could have caught it: `layout_pass`
needs a real device. Count the sites, do not describe them by shape.

**4. Two decisions in the spec were wrong, and an adversarial review found both.** The wheel arm
predicate (D7) and the mitigation count (D9) — see the spec's own revision notes and
`427919e`. Neither was a coding error; both were reasoning errors that would have shipped as
working code doing the wrong thing. The plan faithfully implemented what the spec said, which is
exactly why the spec needed attacking separately.

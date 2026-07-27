# Scan-Region Overlay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Draw a faint, colour-coded outline of every rectangle a hover actually captured, so the OCR's framing is visible instead of inferred from numbers.

**Architecture:** The worker collects the rectangles it used and returns them with the result. The main thread turns them into one shaped window: `SetWindowRgn` set to the union of frame regions, `WM_PAINT` filling each rectangle's bounds in its kind's colour, and only the frames survive the clip. Per-pixel alpha is unavailable on this project (it breaks capture exclusion), which is why the window is *shaped* rather than painted transparent.

**Tech Stack:** Rust 2021, `windows` 0.62.2 (Win32 GDI + window management), no new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-28-scan-overlay-design.md`

## Global Constraints

- **`src/geom.rs`, `src/text/layout.rs`, `src/lookup/`, `src/present.rs` and `src/ui/theme.rs` must not depend on the `windows` crate.** They must compile and test with no screen.
- **Comments:** default is none; write one only when the code cannot be made obvious by naming or structure, and then keep non-doc comment text **under 30 characters**. Rustdoc (`///`, `//!`) and `// SAFETY:` are exempt and are expected to be detailed. Every `unsafe` block gets a `// SAFETY:` naming the invariants relied on.
- **Do NOT run `cargo fmt`.** This repo has never been rustfmt-clean; reformatting would bury the change.
- **Clippy must stay at exactly 5 accepted errors** (`app.rs`, `input/hooks.rs`, `lookup/deconj.rs`, `lookup/model.rs`, `ui/render.rs`), none in `src/text/` or the new files. **Check clippy in every task**, not just the last — a previous plan checked only at the end and shipped two lint errors undetected for four tasks.
- **Cargo is not on PATH.** Prefix every cargo command with:
  `export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup;`
- **chibipop may hold a lock on its own binary.** If a build fails with "failed to remove file":
  `powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"`
- **`Cargo.toml` has a pre-existing unrelated unstaged change.** Leave it; never stage it.
- **Never `git add -A` or `git add .`** — stage only files you deliberately changed.
- **Git identity is set repo-locally.** Do not change it or pass `--author`.
- **Screen constraint:** anything opening a window, capturing, or moving the OS cursor uses the **portrait secondary monitor (x ≥ 2560)**. The 2560×1080 primary at (0,0) is the user's.
- **Test totals drift.** Never hard-code a total in an assertion or commit message without running the suite and reading the number off it.

---

### Task 1: Pure overlay geometry

**Files:**
- Modify: `src/geom.rs` (add types + two functions + tests)

**Interfaces:**
- Consumes: `PhysRect { x, y, w, h }` (already in this file).
- Produces:
  - `pub enum ScanKind { Pass1, Tile, Anchor }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct ScanRect { pub rect: PhysRect, pub kind: ScanKind }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn overlay_layout(rects: &[ScanRect]) -> Option<(PhysRect, Vec<ScanRect>)>`
  - `pub fn inset(rect: PhysRect, thickness: i32) -> Option<PhysRect>`

`overlay_layout` returns the bounding box of every rectangle plus the same rectangles translated into
window-local coordinates, or `None` for empty input. `inset` returns a rectangle's interior, or `None`
when the rectangle is too small to have one — **which is not hypothetical**: real OCR word boxes on the
user's screen include one measuring `w=17 h=3`, and a naive `h - 2*thickness` underflows to a negative
extent there.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/geom.rs`:

```rust
    fn sr(x: i32, y: i32, w: i32, h: i32, kind: ScanKind) -> ScanRect {
        ScanRect { rect: PhysRect { x, y, w, h }, kind }
    }

    #[test]
    fn layout_of_nothing_is_none() {
        assert!(overlay_layout(&[]).is_none());
    }

    #[test]
    fn a_single_rect_bounds_itself_and_sits_at_the_origin() {
        let (bounds, local) = overlay_layout(&[sr(100, 200, 50, 20, ScanKind::Pass1)]).unwrap();
        assert_eq!(PhysRect { x: 100, y: 200, w: 50, h: 20 }, bounds);
        assert_eq!(PhysRect { x: 0, y: 0, w: 50, h: 20 }, local[0].rect);
        assert_eq!(ScanKind::Pass1, local[0].kind);
    }

    #[test]
    fn bounds_span_every_rect_and_locals_are_relative_to_it() {
        let (bounds, local) = overlay_layout(&[
            sr(100, 200, 50, 20, ScanKind::Pass1),
            sr(400, 180, 50, 60, ScanKind::Tile),
        ])
        .unwrap();
        assert_eq!(PhysRect { x: 100, y: 180, w: 350, h: 60 }, bounds);
        assert_eq!(PhysRect { x: 0, y: 20, w: 50, h: 20 }, local[0].rect);
        assert_eq!(PhysRect { x: 300, y: 0, w: 50, h: 60 }, local[1].rect);
    }

    /// Tiles routinely overlap the pass-1 box they were derived from, so the
    /// union must not assume they are disjoint.
    #[test]
    fn overlapping_rects_still_produce_one_covering_bounds() {
        let (bounds, _) = overlay_layout(&[
            sr(0, 0, 100, 100, ScanKind::Pass1),
            sr(50, 50, 100, 100, ScanKind::Tile),
        ])
        .unwrap();
        assert_eq!(PhysRect { x: 0, y: 0, w: 150, h: 150 }, bounds);
    }

    #[test]
    fn inset_leaves_an_interior_for_an_ordinary_rect() {
        let inner = inset(PhysRect { x: 10, y: 10, w: 100, h: 40 }, 2).unwrap();
        assert_eq!(PhysRect { x: 12, y: 12, w: 96, h: 36 }, inner);
    }

    /// A real OCR word box from the user's screen measures w=17 h=3. Subtracting
    /// two 2px edges from a 3px height must yield no interior, not a negative one.
    #[test]
    fn inset_of_a_rect_thinner_than_its_border_has_no_interior() {
        assert!(inset(PhysRect { x: 0, y: 0, w: 17, h: 3 }, 2).is_none());
        assert!(inset(PhysRect { x: 0, y: 0, w: 3, h: 17 }, 2).is_none());
        assert!(inset(PhysRect { x: 0, y: 0, w: 4, h: 4 }, 2).is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib geom 2>&1 | tail -20
```

Expected: compile error, `cannot find function 'overlay_layout' in this scope`. **Paste the real output into your report — a paraphrased or invented RED transcript is a task failure.**

- [ ] **Step 3: Write the implementation**

Add to `src/geom.rs`, after the `PhysRect` impl block:

```rust
/// Which stage of text acquisition a drawn rectangle came from. Drives the
/// overlay's colour; the drawing layer needs no other knowledge of the OCR
/// pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// The cursor-centred box pass 1 captures to find the hovered word.
    Pass1,
    /// One forward tile.
    Tile,
    /// The resolved word's own box.
    Anchor,
}

/// One rectangle the overlay draws, tagged with where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanRect {
    pub rect: PhysRect,
    pub kind: ScanKind,
}

/// The overlay window's bounds, and every rectangle translated into that
/// window's local coordinates.
///
/// `None` for empty input - the caller must then show no window at all rather
/// than an empty one.
pub fn overlay_layout(rects: &[ScanRect]) -> Option<(PhysRect, Vec<ScanRect>)> {
    let first = rects.first()?;
    let mut left = first.rect.x;
    let mut top = first.rect.y;
    let mut right = first.rect.x + first.rect.w;
    let mut bottom = first.rect.y + first.rect.h;

    for r in rects.iter().skip(1) {
        left = left.min(r.rect.x);
        top = top.min(r.rect.y);
        right = right.max(r.rect.x + r.rect.w);
        bottom = bottom.max(r.rect.y + r.rect.h);
    }

    let bounds = PhysRect { x: left, y: top, w: right - left, h: bottom - top };
    let local = rects
        .iter()
        .map(|r| ScanRect { rect: r.rect.translated(-left, -top), kind: r.kind })
        .collect();
    Some((bounds, local))
}

/// A rectangle's interior once a border `thickness` wide is removed.
///
/// `None` when nothing is left - a rectangle no thicker than two borders is
/// all border. OCR word boxes really do get this small (a measured box on the
/// user's screen is 17x3), so this case is reached in practice, and computing
/// it by subtraction alone would produce a negative extent.
pub fn inset(rect: PhysRect, thickness: i32) -> Option<PhysRect> {
    let w = rect.w - 2 * thickness;
    let h = rect.h - 2 * thickness;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(PhysRect { x: rect.x + thickness, y: rect.y + thickness, w, h })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib geom 2>&1 | grep -E "^test result|FAILED"
```

Expected: `test result: ok.` Record the count.

- [ ] **Step 5: Check clippy**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
```

Expected: exactly 5 errors, none in `src/geom.rs`.

- [ ] **Step 6: Commit**

```bash
git add src/geom.rs
git commit -m "feat(overlay): pure geometry for the scan-region overlay"
```

---

### Task 2: `[debug] show_scan_region` configuration

**Files:**
- Modify: `src/config.rs`
- Modify: `README.md` (configuration section)

**Interfaces:**
- Produces: `Config::debug: DebugConfig`, `pub struct DebugConfig { pub show_scan_region: bool }`.

**The `#[serde(default)]` is load-bearing.** `config.rs` treats malformed TOML as a hard error naming
the file, never a silent fallback. A new *required* section makes every existing `chibipop.toml` fail
to parse, and chibipop refuses to start after an upgrade. The user's live config has no `[debug]`
section. `[ocr]` documented this exact trap.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/config.rs`:

```rust
    #[test]
    fn the_scan_overlay_defaults_off() {
        assert!(!Config::default().debug.show_scan_region,
                "the overlay is a debug aid and must be opt-in");
    }

    #[test]
    fn an_enabled_overlay_round_trips() {
        let p = tmp("overlay_on");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.debug.show_scan_region = true;
        c.save(&p).unwrap();
        assert!(load_or_create(&p).unwrap().debug.show_scan_region);
        let _ = std::fs::remove_file(&p);
    }

    /// Every config written before this section existed lacks [debug]. Without
    /// serde(default) they stop loading and chibipop refuses to start.
    #[test]
    fn a_config_written_before_the_debug_section_existed_still_loads() {
        let p = tmp("no_debug_section");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\nmax_ocr_passes = 3\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-[debug] config must still load");
        assert!(!c.debug.show_scan_region);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_empty_debug_section_still_defaults_the_toggle_off() {
        let p = tmp("empty_debug");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\nmax_ocr_passes = 3\n\n",
            "[debug]\n",
        )).unwrap();
        assert!(!load_or_create(&p).unwrap().debug.show_scan_region);
        let _ = std::fs::remove_file(&p);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib config 2>&1 | tail -20
```

Expected: `no field 'debug' on type 'Config'`. Paste the real output.

- [ ] **Step 3: Write the implementation**

Add the field to `Config`, after `ocr`:

```rust
    #[serde(default)]
    pub debug: DebugConfig,
```

Add the struct next to the other section structs:

```rust
/// `[debug]`. Carries `#[serde(default)]` at the `Config` level so a file
/// written before this section existed still loads - `load_or_create` treats
/// malformed TOML as a hard error rather than falling back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebugConfig {
    /// Draw a faint outline around every region a hover captured: pass 1's
    /// box, each forward tile, and the resolved word's anchor.
    ///
    /// Off by default. When off nothing is collected and no window is
    /// created - the feature is inert, not merely hidden.
    #[serde(default)]
    pub show_scan_region: bool,
}

impl Default for DebugConfig {
    fn default() -> DebugConfig {
        DebugConfig { show_scan_region: false }
    }
}
```

Add to `impl Default for Config`, after `ocr`:

```rust
            debug: DebugConfig::default(),
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib config 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 5: Prove the `#[serde(default)]` does the work**

Temporarily delete `#[serde(default)]` from `Config::debug`, re-run
`a_config_written_before_the_debug_section_existed_still_loads`, and confirm it **FAILS** with
`missing field 'debug'`. Restore it and confirm it passes. Capture both outputs verbatim. Without this
the test could be passing for an unrelated reason.

- [ ] **Step 6: Update the README**

In `README.md`'s configuration TOML block, append:

```toml

[debug]
show_scan_region = false   # outline what each hover captured - see the scan-overlay spec
```

And after the `max_ocr_passes` paragraph, add:

```markdown
**`show_scan_region`** draws a faint outline around every region a hover
captured — pass 1's box, each forward tile, and the word it resolved. It is a
debugging aid: turn it on when OCR is behaving oddly and you want to see what
it actually looked at rather than infer it. `probe --show-region` shows the
same outlines for a single coordinate without running the app.
```

- [ ] **Step 7: Check clippy, then commit**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
git add src/config.rs README.md
git commit -m "feat(config): [debug] show_scan_region, defaulting off"
```

---

### Task 3: The overlay window

**Files:**
- Create: `src/ui/overlay.rs`
- Modify: `src/ui/mod.rs` (declare the module)
- Modify: `src/ui/theme.rs` (three colours + a test)

**Interfaces:**
- Consumes: `ScanRect`, `ScanKind`, `overlay_layout`, `inset` (Task 1); `PhysRect`.
- Produces:
  - `pub struct Overlay`
  - `pub fn Overlay::create(exclude_from_capture: bool) -> Result<Overlay>`
  - `pub fn Overlay::hwnd(&self) -> HWND`
  - `pub fn Overlay::show_rects(&self, rects: &[ScanRect], theme: &Theme) -> Result<()>`
  - `pub fn Overlay::hide(&self)`
  - `Theme::scan_pass1`, `Theme::scan_tile`, `Theme::scan_anchor`, all `(u8, u8, u8)`

**Model this file on `src/ui/window.rs`.** Read it first. It already solves every hard part: the
window class registration guard, `WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW
| WS_EX_TRANSPARENT`, `SetLayeredWindowAttributes(..., LWA_ALPHA)`, `SetWindowDisplayAffinity`, and
`SetWindowRgn` including the GDI region-leak trap on failure. Follow its structure rather than
inventing a parallel one.

**Why the window is shaped rather than painted transparent:** `WDA_EXCLUDEFROMCAPTURE` is incompatible
with `UpdateLayeredWindow` (measured — the affinity call fails with a misleading "not enough memory"
HRESULT and silently no-ops), so per-pixel alpha is unavailable, and constant alpha cannot make an
interior transparent. `SetWindowRgn` set to the union of *frame* regions gives outlines instead.

- [ ] **Step 1: Add the theme colours and their test**

In `src/ui/theme.rs`, add three fields to `Theme` with `///` docs saying they are the scan overlay's
outline colours, and populate them in both `dark()` and `light()`. Choose values that stay
distinguishable from one another at alpha 90 over arbitrary content — `Pass1` dimmest, `Tile` the
accent hue, `Anchor` brightest. Add this test:

```rust
    /// Three outlines drawn at one alpha over arbitrary screen content are
    /// only readable if the colours differ from each other.
    #[test]
    fn scan_colours_are_distinct_in_both_themes() {
        for t in [Theme::dark(), Theme::light()] {
            assert_ne!(t.scan_pass1, t.scan_tile);
            assert_ne!(t.scan_tile, t.scan_anchor);
            assert_ne!(t.scan_pass1, t.scan_anchor);
        }
    }
```

Run `cargo test --lib theme` and confirm it fails first (missing fields), then passes.

- [ ] **Step 2: Create `src/ui/overlay.rs`**

Write the window. Requirements, all of which `window.rs` demonstrates:

- A distinct window class name (e.g. `ChibipopOverlayClass`) with the same one-shot registration guard
  `window.rs` uses. Do **not** reuse the popup's class.
- Extended styles exactly as listed above; `WS_POPUP` as the style.
- `SetLayeredWindowAttributes(hwnd, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA)` with
  `const OVERLAY_ALPHA: u8 = 90;` and a `///` comment recording that "faint" is the spec's word.
- `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` only when `exclude_from_capture` is true,
  mirroring `Popup::create`.
- `show_rects`: call `overlay_layout`; on `None`, `hide()` and return `Ok(())`. Otherwise move and size
  the window to the bounds, build the region, apply it, and show without activating.
- **The region:** for each local rect, `CreateRectRgn` for the outer rect; if `inset(rect,
  FRAME_THICKNESS)` is `Some`, `CreateRectRgn` the inner and `CombineRgn(RGN_DIFF)` to leave a frame;
  if `None`, keep the solid outer rect (a rectangle thinner than two borders is all border).
  `CombineRgn(RGN_OR)` each into an accumulator. **Every intermediate `HRGN` must be deleted with
  `DeleteObject` on every path, including early returns** — `window.rs` documents this exact leak.
- `SetWindowRgn(hwnd, Some(rgn), TRUE)` transfers ownership of `rgn` to the system on success; on
  failure the caller still owns it and must delete it. Get this right and say so in a `// SAFETY:`.
- `WM_PAINT`: `BeginPaint`, then for each local rect `FillRect` with a brush for its `ScanKind`, then
  `EndPaint`. Delete every brush created. The region clips the fills to frames.
- `Drop`: destroy the window.
- `const FRAME_THICKNESS: i32 = 2;`

The rect list must be reachable from the `wndproc`, which is a bare `extern "system" fn` with no
`self`. Follow whatever mechanism `window.rs` already uses to reach per-window state
(`GWLP_USERDATA` or equivalent); if it uses none, store the current rects and theme colours in a
thread-local owned by this module and document why.

`src/ui/mod.rs` gains `pub mod overlay;`.

- [ ] **Step 3: Build and check clippy**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo build 2>&1 | grep -E "^error|^warning: unused|Finished"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
```

Expected: builds clean; clippy at exactly 5 errors, none in `src/ui/overlay.rs`.

- [ ] **Step 4: Run the whole suite**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 5: Commit**

```bash
git add src/ui/overlay.rs src/ui/mod.rs src/ui/theme.rs
git commit -m "feat(ui): shaped overlay window outlining captured regions"
```

---

### Task 4: Collect the regions and show them from the app

**Files:**
- Modify: `src/text/ocr.rs` (collect during resolve)
- Modify: `src/app.rs` (plumb, display, capture guard)

**Interfaces:**
- Consumes: `Overlay` (Task 3), `ScanRect`/`ScanKind` (Task 1), `cfg.debug.show_scan_region` (Task 2).
- Produces: `OcrTextSource::resolve_at_tiled_scanned(&self, cursor: PhysPoint, collect: bool) -> Result<(Option<Resolved>, Vec<ScanRect>)>`, with `resolve_at_tiled` kept as a thin wrapper passing `collect: false` so existing callers are untouched.

**Only the worker knows the tiles** — their positions come from OCR output inside `tile_forward`'s
reader closure. Pass 1's box could be recomputed on the main thread, but the tiles cannot, so all of
them travel back together.

**When `collect` is false, allocate nothing.** The returned `Vec` is empty and no rectangle is ever
constructed. "Off" means inert.

- [ ] **Step 1: Collect in `ocr.rs`**

Add `resolve_at_tiled_scanned`. It does what `resolve_at_tiled` does, and when `collect` is true also
records:
- `ScanRect { rect: region_around(cursor), kind: ScanKind::Pass1 }`
- one `ScanRect { rect: tile, kind: ScanKind::Tile }` per tile — push from inside the reader closure
  passed to `tile_forward`, which already receives each tile rectangle
- `ScanRect { rect: first.span.anchor, kind: ScanKind::Anchor }`

Change `resolve_at_tiled` to `self.resolve_at_tiled_scanned(cursor, false).map(|(r, _)| r)`.

- [ ] **Step 2: Plumb through `app.rs`**

- `resolve_trigger` takes the collect flag and returns the rectangles alongside its outcome. Both
  `resolve_at` call sites inside it (the capture-guard branch and the plain branch) become
  `resolve_at_tiled_scanned`.
- `WorkerOutcome::Ready` carries `scan: Vec<ScanRect>`.
- The main thread creates the `Overlay` at startup **only when `cfg.debug.show_scan_region` is true**,
  holding it as an `Option<Overlay>`, and calls `show_rects` when a result arrives, `hide` on
  `WorkerOutcome::Hide`.

- [ ] **Step 3: Add the overlay to the capture guard — mind the borrow**

`drain_capture_guard` (around `src/app.rs:355`) is a closure that already borrows `popup` and
`capture_guard_prev_visible`. **Do not capture the `Overlay` struct in it** — the main loop needs the
overlay for `show_rects`, and a second mutable borrow will not compile. Capture the overlay's bare
`HWND` (which is `Copy`) and hide/show it with `ShowWindow` directly, tracking its previous visibility
the same way the popup's is tracked.

This matters because the overlay from hover *N* is still on screen during hover *N+1*'s capture, so it
must hide with the popup.

- [ ] **Step 4: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

Expected: suite green, clippy at exactly 5, release builds.

- [ ] **Step 5: Commit**

```bash
git add src/text/ocr.rs src/app.rs
git commit -m "feat(app): collect captured regions and show them in the overlay"
```

---

### Task 5: `probe --show-region`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Overlay` (Task 3), `resolve_at_tiled_scanned` (Task 4), `Theme` (Task 3).

`probe` is the measurement tool and does **not** read the TOML, so `[debug] show_scan_region` has no
effect here — the flag is the only control, the same split `--tiles` already has against
`[ocr] max_ocr_passes`.

- [ ] **Step 1: Add the flag**

In the `Probe` variant:

```rust
        /// Draw the captured regions for this many seconds after probing.
        /// Bare `--show-region` shows them for 3s. Omitted, nothing is drawn.
        #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "3")]
        show_region: Option<u64>,
```

- [ ] **Step 2: Show the overlay after all captures**

At the **end** of the `Probe` arm, after every capture and print that run performs:

```rust
            if let Some(seconds) = show_region {
                let (_, scan) = source.resolve_at_tiled_scanned(cursor, true)?;
                if scan.is_empty() {
                    println!("\nshow-region: nothing was captured");
                } else {
                    let overlay = chibipop::ui::overlay::Overlay::create(false)
                        .context("creating the scan overlay")?;
                    overlay.show_rects(&scan, &chibipop::ui::theme::Theme::dark())?;
                    println!("\nshow-region: {} rect(s) for {seconds}s", scan.len());
                    pump_messages_for(std::time::Duration::from_secs(seconds));
                }
            }
```

Ordering is the safety property: the overlay is created strictly after every capture this run makes,
so it can never appear inside one.

Add the pump helper to `src/main.rs`:

```rust
/// Pump this thread's messages for `dur`, so a window created by a one-shot
/// command stays painted instead of appearing frozen. `probe` has no message
/// loop of its own; this exists only for `--show-region`.
fn pump_messages_for(dur: std::time::Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let deadline = std::time::Instant::now() + dur;
    let mut msg = MSG::default();
    while std::time::Instant::now() < deadline {
        // SAFETY: `msg` is a valid, thread-local MSG; PeekMessageW with a null
        // HWND retrieves messages for any window this thread owns, and both
        // TranslateMessage and DispatchMessageW take it by const pointer.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
```

- [ ] **Step 3: Verify it builds and the suite stays green**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
cargo build --release 2>&1 | grep -E "^error|Finished"
./target/release/chibipop.exe probe --help
```

Expected: `--show-region` appears in the help with `[SECONDS]`.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(probe): --show-region draws what the probe captured"
```

---

### Task 6: Live acceptance

**Files:**
- Create: `docs/superpowers/findings/2026-07-28-scan-overlay-acceptance.md`

**This task measures and observes. It does not tune.** If something misses, record it and stop.

**Screen constraint:** use the **portrait secondary monitor (x ≥ 2560)** only. Do not move the OS
cursor or click. `probe --at` reads a coordinate without moving the pointer.

**Sample text is already on that monitor**, located and measured: a horizontal line at
**y ≈ 589–612**, glyphs ~17–23px, running rightward from about **x = 2850**. A probe at `3100,600`
resolves `よ` with anchor `x=3100 y=590 w=17 h=21`, single-pass line
`ーディングシステムにより、表示の差が認め`, and tiled `より、表示の差が認められることが` (16 chars).

- [ ] **Step 1: Verify the four acceptance criteria from spec §2**

1. **Outlines appear and are distinguishable.** Run
   `./target/release/chibipop.exe probe --at 3100,600 --show-region 8` and take a screenshot **of the
   vertical monitor only** while it is up. Confirm you can see pass 1's box, the tile(s), and the
   anchor, and that they are distinguishable. Attach or describe the screenshot precisely.
2. **The rectangles land where the numbers say.** Compare the drawn outlines against the coordinates
   `probe` printed in the same run. The anchor outline must sit on the resolved character.
3. **Inert when off.** With `show_scan_region = false` in `target/release/chibipop.toml`, start
   `chibipop run`, confirm no overlay window exists (`Get-Process` plus a screenshot), and stop it.
   **Read the config file first and restore it byte-for-byte afterwards — it is the user's live file.**
4. **Lookups are unchanged.** Run `probe --at 3100,600 --tiles 3` with and without `--show-region` and
   confirm the `tiled:` line is byte-identical. The instrument must not perturb what it measures.

- [ ] **Step 2: Note the thing the spec flagged as unknown**

Spec §8 flags that three colours at alpha 90 over arbitrary content may not stay distinguishable.
State plainly whether they did. If they did not, say so and recommend per-kind thickness — **do not
change the colours to make this pass.**

- [ ] **Step 3: Write the findings note and commit**

Record each criterion with what you actually observed, pass or fail, plus the text used and its pixel
size. State any miss plainly.

```bash
git add docs/superpowers/findings/2026-07-28-scan-overlay-acceptance.md
git commit -m "docs: scan-overlay acceptance observations"
```

---

## Self-Review

**Spec coverage:** D1 (draw after the scan) → Task 5 Step 2's ordering and Task 4's post-resolve
display. D2 (shaped window) → Task 3 Step 2. D3 (GDI, not D2D) → Task 3 Step 2's `FillRect`. D4
(regions from the worker, collected only when enabled) → Task 4 Steps 1–2. D5 (follows the popup's
capture setting and guard) → Task 3's `exclude_from_capture` parameter and Task 4 Step 3. D6
(`--show-region`) → Task 5. D7 (`[debug]` with `serde(default)`) → Task 2. §2's four acceptance
criteria → Task 6 Step 1. §5's error table → Task 3 Step 2 (`None` → hide; region failure → delete and
hide) and Task 5 (`context` on creation failure). §6's test list → Tasks 1, 2, 3 Step 1, and Task 6.
§4.1's constants → `FRAME_THICKNESS` and `OVERLAY_ALPHA` in Task 3, `default_missing_value = "3"` in
Task 5.

**Gap found and closed:** §6 asks for a frame-geometry test in "both a wide-flat and a tall-narrow
rectangle". Task 1's `inset_of_a_rect_thinner_than_its_border_has_no_interior` covers both
orientations explicitly (`17x3` and `3x17`), so the vertical-text shape is not an afterthought.

**Placeholder scan:** none — every step carries its code or its exact command. Task 3 Step 2 is
specified as requirements rather than a full code block because it must mirror `window.rs`'s existing
class-registration, region-leak and `SAFETY` patterns, which the implementer has to read anyway;
copying a stale transcription of that file into the plan would be worse than pointing at it.

**Type consistency:** `ScanRect`/`ScanKind` are defined in Task 1 and used unchanged in Tasks 3, 4, 5.
`overlay_layout` returns `Option<(PhysRect, Vec<ScanRect>)>` and Task 3 destructures exactly that.
`inset` returns `Option<PhysRect>` and Task 3 branches on `Some`/`None`. `resolve_at_tiled_scanned`
returns `(Option<Resolved>, Vec<ScanRect>)` in Task 4 and Task 5 destructures `(_, scan)`.
`Overlay::create` takes `bool` and returns `Result<Overlay>` in both Task 3 and Task 5.

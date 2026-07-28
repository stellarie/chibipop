# Accuracy and Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make tiling resolve the character actually under the cursor, show the user which word the popup is defining, measure vertical text, and give chibipop its own icon.

**Architecture:** Pass 1 keeps its text from the hovered word to its last unclipped word; tiles continue after it and filter *both* edges. The overlay gains a `Match` rectangle drawn from per-character geometry carried alongside the stitched text. Vertical text is measured before anything about it is changed.

**Tech Stack:** Rust 2021, `windows` 0.62.2, no new runtime dependencies.

**Spec:** `docs/superpowers/specs/2026-07-28-accuracy-and-polish-design.md` (revision 3, independently approved)

## Global Constraints

- **`src/geom.rs`, `src/text/layout.rs`, `src/lookup/`, `src/present.rs`, `src/config.rs` and `src/ui/theme.rs` must not depend on the `windows` crate.** They compile and test with no screen.
- **Comments:** default is none; non-doc comment text **under 30 characters**. Rustdoc (`///`, `//!`) and `// SAFETY:` are exempt and expected to be detailed. Every `unsafe` block gets a `// SAFETY:`.
- **Do NOT run `cargo fmt`.** This repo has never been rustfmt-clean.
- **Clippy must stay at exactly 5 accepted errors** (`app.rs`, `input/hooks.rs`, `lookup/deconj.rs`, `lookup/model.rs`, `ui/render.rs`). **Check it in every task.** Note the usual command aborts on those 5 before the *binary* target compiles, so to lint `src/main.rs` also run it with them suppressed:
  `-A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity`
- **Cargo is not on PATH.** Prefix every cargo command with:
  `export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup;`
- **chibipop may hold a lock on its own binary.** On "failed to remove file":
  `powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"`
- **`Cargo.toml` has a pre-existing unrelated unstaged change.** Leave it; never stage it.
- **Never `git add -A` or `git add .`.** Stage only files you deliberately changed.
- **Git identity is set repo-locally.** Do not change it or pass `--author`.
- **Screen constraint:** anything opening a window, capturing, or moving the OS cursor uses the **portrait secondary monitor (x ≥ 2560)**. The 2560×1080 primary at (0,0) is the user's.
- **`max_ocr_passes` defaults to `1`** — tiling is off. Tasks touching the tiled path must test it explicitly with `--tiles 3`, since the default path will not exercise it.
- **Test totals drift.** Never hard-code a total without running the suite and reading it off.

**Task order is a requirement (spec preamble).** Tasks 1 and 2 are independent of the rest and land first, so a Task 4/5 acceptance failure cannot gate them.

---

### Task 1: The bottle icon

**Files:**
- Create: `assets/chibipop.svg`, `assets/chibipop.ico`
- Modify: `src/ui/tray.rs`

**Interfaces:**
- Produces: a tray icon loaded from embedded bytes rather than `LoadIconW(None, IDI_APPLICATION)`.

**Independent of every other task.** Nothing here touches OCR, the overlay, or config.

- [ ] **Step 1: Author the artwork**

Create `assets/chibipop.svg`, a 512×512 chibi baby bottle, drawn fresh (not traced from any existing image): cream body `#F5F0E1` with a light blue-grey outline `#B8C9D4`, two large dark-brown eyes `#4A3728` with white highlights, a small mouth, a blue cap `#3DA9E0` with diagonal lighter stripes, an orange nipple `#FFA726`, the whole rotated about 25° clockwise. Flat vector, no gradients — it must stay legible at 16×16.

- [ ] **Step 2: Build the .ico**

Produce `assets/chibipop.ico` containing 16, 32, 48 and 256 px frames. Python's standard library cannot write `.ico`; check what is available before choosing a route:

```bash
python -c "import PIL; print(PIL.__version__)" 2>&1 | head -1
magick -version 2>&1 | head -1
```

If Pillow is present, use it. If neither is, **write the `.ico` by hand** — the format is a 6-byte header, a 16-byte directory entry per frame, and PNG-encoded frames, which is a contained amount of code and avoids adding a dependency. Rasterising the SVG without a library is not practical, so in that case draw the frames directly as PNGs with the same shapes. Record in your report which route you took and why.

- [ ] **Step 3: Load it in the tray, with ownership tracked**

`src/ui/tray.rs:147` currently does `LoadIconW(None, IDI_APPLICATION)`, whose handle is **shared and must never be destroyed**. An icon created from bytes with `CreateIconFromResourceEx` is **owned and must be destroyed**. Putting both on one field without a flag is a leak or a double-free.

Embed the `.ico` with `include_bytes!`, create the icon with `CreateIconFromResourceEx`, and store `(HICON, owned: bool)` on `Tray`. On failure, fall back to `LoadIconW(None, IDI_APPLICATION)` with `owned: false` and log once — the tray is the only way to quit, so it must appear.

In `Drop`, call `DestroyIcon` **only when `owned`, and only after `Shell_NotifyIconW(NIM_DELETE)`** — destroying an icon the shell is still displaying is the ordering bug this avoids. Give that `unsafe` block a `// SAFETY:` saying so.

- [ ] **Step 4: Investigate the executable icon**

The exe's own icon (Explorer, taskbar) needs a linked Windows resource. This project has taken **no** build dependencies. Determine whether a dependency-free route exists — e.g. a checked-in `.res` produced once by `rc.exe` from the MSVC tools already installed, referenced via `#[link_args]` or a `build.rs` that only calls `println!("cargo:rustc-link-arg=...")`.

**If it needs a new crate, do not add one.** Record it as deferred in your report with what you found. The tray icon is the deliverable; the exe icon is a bonus.

- [ ] **Step 5: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

Do **not** launch chibipop to look at the tray — the controller does that.

- [ ] **Step 6: Commit**

```bash
git add assets/chibipop.svg assets/chibipop.ico src/ui/tray.rs
git commit -m "feat(ui): chibipop's own tray icon"
```

---

### Task 2: Measure vertical text

**Files:**
- Create: `docs/superpowers/findings/2026-07-28-vertical-text-measurement.md`

**This task measures and changes no code.** If it shows vertical text is not meaningfully broken, that is the finding and nothing is fixed.

**Screen constraint:** use only the **portrait secondary monitor (x ≥ 2560)**. `probe --at X,Y` reads a coordinate without moving the pointer — use it for everything. Do not click or move the cursor.

- [ ] **Step 1: Find vertical Japanese text**

Sweep for it. Vertical text shows as a column: near-constant `x` across a line's words with `y` increasing. Confirm by checking `orient:` in probe's output reports `Vertical`.

```bash
cd /c/Users/Stella/chibipop
for xy in "2900,400" "2900,800" "3200,400" "3200,800" "3400,600"; do printf "%-11s -> " "$xy"; ./target/release/chibipop.exe probe --at "$xy" 2>&1 | grep -E "^orient:|^ocr line 0|^ocr: " | head -2 | tr '\n' ' '; echo; done
```

If no vertical text is on screen, **stop and report NEEDS_CONTEXT** — the controller will arrange it. Do not substitute horizontal text.

- [ ] **Step 2: Record the baseline**

At three hover points on one vertical line, record from `probe --at X,Y`: the reported `orient:`, the resolved character (`at:` line), and how many characters of forward context the `line:` field yields past the cursor. **Transcribe the on-screen column by eye first** as ground truth.

Extract the resolved character with a pattern that does not split multibyte characters — `.` matches a single **byte** in `grep`/`sed` here:

```bash
sed -n "s/^at: .*-> Some('\(.*\)').*/\1/p"
```

- [ ] **Step 3: Measure candidate shapes**

`--region W,H` overrides the capture box and is single-capture by definition. At the same three points, measure the default and two orientation-aware shapes:

```bash
for r in "500,100" "100,500" "300,300"; do printf "region %-9s -> " "$r"; ./target/release/chibipop.exe probe --at <x>,<y> --region "$r" 2>&1 | grep -E "^line:|^ocr: " | head -1; done
```

Record for each: whether the resolved character is right, and how many forward characters it yields.

- [ ] **Step 4: Write the findings and commit**

Record the baseline, the three shapes' results, the ground truth, and the pixel size of the text. State plainly whether vertical text is broken and, if so, which shape did best. **Do not change any constant** — choosing a fix is a later round.

```bash
git add docs/superpowers/findings/2026-07-28-vertical-text-measurement.md
git commit -m "docs: measure vertical text against candidate capture shapes"
```

---

### Task 3: Per-character geometry and the leading-edge filter

**Files:**
- Modify: `src/text/layout.rs`

**Interfaces:**
- Consumes: `OcrWord`, `PhysRect`, `Orientation`, `EDGE_MARGIN`, `split_at_clipped`, `normalise`.
- Produces:
  - `pub struct TextGeom { pub char_count: usize, pub rect: PhysRect }`
  - `pub fn drop_leading(words: Vec<&OcrWord>, start: i32, orientation: Orientation) -> Vec<&OcrWord>`
  - `pub fn union_chars(geom: &[TextGeom], from: usize, len: usize, pad: i32) -> Option<PhysRect>`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/text/layout.rs`:

```rust
    #[test]
    fn leading_words_well_before_the_start_are_dropped() {
        let words = vec![hword("あ", 100, 40), hword("い", 300, 40), hword("う", 360, 40)];
        let refs: Vec<&OcrWord> = words.iter().collect();
        let kept = drop_leading(refs, 300, Orientation::Horizontal);
        assert_eq!(vec!["い", "う"], kept.iter().map(|w| w.text.as_str()).collect::<Vec<_>>());
    }

    /// A word the previous tile deliberately deferred is re-measured by this
    /// one through integer truncation, so its leading edge can land a pixel
    /// or two early. Dropping it would delete the character from BOTH
    /// captures - the seam fix's whole purpose is to save it.
    #[test]
    fn a_word_just_inside_the_margin_is_kept() {
        let words = vec![hword("あ", 297, 40)];
        let refs: Vec<&OcrWord> = words.iter().collect();
        let kept = drop_leading(refs, 300, Orientation::Horizontal);
        assert_eq!(1, kept.len(), "297 is within EDGE_MARGIN of 300 and must survive");
    }

    #[test]
    fn the_leading_filter_mirrors_for_vertical_text() {
        let words = vec![
            OcrWord { text: "上".into(), rect: PhysRect { x: 100, y: 100, w: 40, h: 40 } },
            OcrWord { text: "下".into(), rect: PhysRect { x: 100, y: 300, w: 40, h: 40 } },
        ];
        let refs: Vec<&OcrWord> = words.iter().collect();
        let kept = drop_leading(refs, 300, Orientation::Vertical);
        assert_eq!(vec!["下"], kept.iter().map(|w| w.text.as_str()).collect::<Vec<_>>());
    }

    fn g(n: usize, x: i32, w: i32) -> TextGeom {
        TextGeom { char_count: n, rect: PhysRect { x, y: 100, w, h: 40 } }
    }

    #[test]
    fn union_covers_exactly_the_matched_run_padded() {
        let geom = vec![g(1, 100, 30), g(1, 130, 30), g(1, 160, 30)];
        let r = union_chars(&geom, 0, 2, 3).unwrap();
        assert_eq!(PhysRect { x: 97, y: 97, w: 66, h: 46 }, r);
    }

    #[test]
    fn union_starts_at_the_requested_offset_not_the_beginning() {
        let geom = vec![g(1, 100, 30), g(1, 130, 30), g(1, 160, 30)];
        let r = union_chars(&geom, 2, 1, 0).unwrap();
        assert_eq!(PhysRect { x: 160, y: 100, w: 30, h: 40 }, r);
    }

    /// An OCR word is not always one character - a page counter on the same
    /// line arrived as "338" in a single box. A match ending inside one
    /// rounds outward to that box, because geometry for part of it was never
    /// supplied.
    #[test]
    fn a_match_ending_inside_a_multi_char_entry_rounds_outward() {
        let geom = vec![g(1, 100, 30), g(3, 130, 90)];
        let r = union_chars(&geom, 0, 2, 0).unwrap();
        assert_eq!(PhysRect { x: 100, y: 100, w: 120, h: 40 }, r);
    }

    #[test]
    fn union_of_nothing_is_none() {
        assert!(union_chars(&[], 0, 3, 0).is_none());
        assert!(union_chars(&[g(1, 100, 30)], 5, 1, 0).is_none());
    }

    /// The highlight maps character indices through normalise, so a
    /// substitution that changed the count would shift every box after it.
    #[test]
    fn normalise_preserves_character_count() {
        for s in ["ツ-ル", "ツ‐ル", "ツ–ル", "ツ—ル", "abc", "日本語", ""] {
            assert_eq!(s.chars().count(), normalise(s).chars().count(), "input {s:?}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | tail -20
```

Expected: `cannot find function 'drop_leading'`. **Paste the real output — a paraphrased RED transcript is a task failure.**

- [ ] **Step 3: Write the implementation**

```rust
/// One entry of a run's geometry: an OCR word's box and how many characters
/// of the text it accounts for.
///
/// `char_count` rather than one entry per character, because an OCR "word" is
/// not always one character - a page counter on a Japanese line arrived as
/// `338` in a single box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextGeom {
    pub char_count: usize,
    pub rect: PhysRect,
}

/// Discard words beginning more than [`EDGE_MARGIN`] before `start`.
///
/// The mirror of [`split_at_clipped`]'s trailing rule, and it carries the same
/// tolerance for the same reason: a word the previous tile deferred is
/// re-measured here from a different capture, through integer truncation, so
/// its leading edge can land slightly early. Dropping it then would delete the
/// character from both captures.
pub fn drop_leading<'a>(
    words: Vec<&'a OcrWord>,
    start: i32,
    orientation: Orientation,
) -> Vec<&'a OcrWord> {
    let lead: AxisEdge = match orientation {
        Orientation::Horizontal => |r| r.x,
        Orientation::Vertical => |r| r.y,
    };
    words.into_iter().filter(|w| lead(&w.rect) >= start - EDGE_MARGIN).collect()
}

/// The padded box covering `len` characters starting at character `from`.
///
/// A match ending inside a multi-character entry rounds **outward** to that
/// entry's whole box: geometry for part of an OCR word was never supplied, and
/// a box slightly wider than the match is honest where a guessed split is not.
/// `None` when the run is empty or `from` lies past its end.
pub fn union_chars(geom: &[TextGeom], from: usize, len: usize, pad: i32) -> Option<PhysRect> {
    if len == 0 {
        return None;
    }
    let end = from + len;
    let mut at = 0usize;
    let mut acc: Option<PhysRect> = None;

    for entry in geom {
        let next = at + entry.char_count;
        if next > from && at < end {
            acc = Some(match acc {
                None => entry.rect,
                Some(a) => {
                    let l = a.x.min(entry.rect.x);
                    let t = a.y.min(entry.rect.y);
                    let r = (a.x + a.w).max(entry.rect.x + entry.rect.w);
                    let b = (a.y + a.h).max(entry.rect.y + entry.rect.h);
                    PhysRect { x: l, y: t, w: r - l, h: b - t }
                }
            });
        }
        at = next;
    }

    acc.map(|r| PhysRect { x: r.x - pad, y: r.y - pad, w: r.w + 2 * pad, h: r.h + 2 * pad })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | grep -E "^test result|FAILED"
```

- [ ] **Step 5: Prove the margin is load-bearing**

Change `>= start - EDGE_MARGIN` to `>= start`, re-run `a_word_just_inside_the_margin_is_kept`, and confirm it **FAILS**. Restore and confirm it passes. Capture both outputs verbatim. A margin whose test passes without it is not a margin.

- [ ] **Step 6: Check clippy, then commit**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
git add src/text/layout.rs
git commit -m "feat(ocr): per-character geometry and a leading-edge filter"
```

---

### Task 4: Pass 1 keeps its head; tiles filter both edges

**Files:**
- Modify: `src/text/layout.rs` (`resolve` returns geometry), `src/text/ocr.rs` (the head/tail assembly)

**Interfaces:**
- Consumes: `TextGeom`, `drop_leading` (Task 3); `split_at_clipped`, `tile_forward`, `band_of`.
- Produces: `TextSpan` gains `pub geom: Vec<TextGeom>` — the geometry of `text`, in reading order, aligned to its characters.

**Spec D1-R and D1-R2.** Read them before starting. The short version: pass 1's reading of the hovered character is the best-framed one that exists, and the original design threw it away and re-read it near a tile boundary. Now pass 1 supplies the **head** — from the hovered word to its last unclipped word — and tiles supply only the **tail**.

**Three details that are easy to get wrong:**

1. **Tile 1 starts at the last kept word's *trailing* edge**, not at pass 1's region edge. With nothing clipped `split_at_clipped` returns the region's own edge, and a glyph straddling it that OCR dropped entirely would then be skipped by both captures.
2. **Tiles must apply `drop_leading`** at their start, or the position-loss bug moves to mid-string instead of being fixed. This is the finding that made revision 1 of the spec wrong.
3. **`geom` must stay aligned to `text` through `normalise`**, which substitutes one character for another and never changes the count. Task 3 asserts that; rely on it.

- [ ] **Step 1: Add `geom` to `TextSpan` and populate it in `resolve`**

`src/text/mod.rs`'s `TextSpan` gains `pub geom: Vec<TextGeom>`. In `resolve`, build it in the same loop that concatenates word texts — one `TextGeom` per word, `char_count` being that word's own `chars().count()`. Every existing construction site of `TextSpan` must be updated.

- [ ] **Step 2: Assemble head + tail in `ocr.rs`**

In the tiled path, after pass 1 resolves:
- Take pass 1's line words, ordered, from the hovered word onward.
- `split_at_clipped(those, pass1_region, orientation)` → the kept head and a next-start.
- Head text and geometry come from the kept words. **Tile 1 starts at the last kept word's trailing edge.**
- Inside the tile reader, apply `drop_leading(words, tile_start, orientation)` before `split_at_clipped`.
- Concatenate head then tail for both text and geometry; `normalise` once at the end.
- `cursor_byte_offset` is 0 on this path and is now true by construction.

**If the head is empty** — every word clipped, near-unreachable since the hovered word sits ~250px from pass 1's edge — **fall back to single-pass**, not to tiling from the anchor. Tiling from the anchor *is* the bug.

- [ ] **Step 3: Verify the suite and clippy**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

- [ ] **Step 4: Commit**

```bash
git add src/text/layout.rs src/text/mod.rs src/text/ocr.rs
git commit -m "fix(ocr): pass 1 keeps its head; tiles filter both edges"
```

---

### Task 5: The match highlight

**Files:**
- Modify: `src/geom.rs` (`ScanKind::Match`), `src/ui/theme.rs`, `src/present.rs`, `src/config.rs`, `src/app.rs`, `src/ui/overlay.rs`, `README.md`

**Interfaces:**
- Consumes: `union_chars`, `TextGeom` (Task 3); `TextSpan::geom` (Task 4).
- Produces: `ScanKind::Match`; `Card::match_len: usize`; `PopupConfig::highlight_match: bool`; `Theme::scan_match`.

**Four things the spec is explicit about because each was a caught defect:**

1. **The index origin is the hovered character, not the string's start.** `match_len` counts characters of `span.text[cursor_byte_offset..]`. On the shipped single-pass default that offset is **non-zero**, so highlighting "the first `match_len` characters" would box the beginning of the line. Convert `cursor_byte_offset` to a character index and pass it as `union_chars`'s `from`.
2. **`#[serde(default = "default_highlight_match")]`, not a bare `#[serde(default)]`.** A bare one yields `false` for a bool, so every existing config would load with the highlight off.
3. **The capture kinds must not be drawn when only `highlight_match` is on** — otherwise the default path draws four boxes, which is what this feature exists to avoid.
4. **`scan_pass1` is already a blue.** Make `scan_match` the brighter, more saturated blue and leave `scan_pass1` dim: the highlight is the everyday case.

- [ ] **Step 1: `ScanKind::Match`, the theme colour, and `Card::match_len`**

Add the variant, add `scan_match` to `Theme` populated in `dark()` and `light()`, and extend `theme.rs`'s pairwise distinctness test from three colours to four. Add `pub match_len: usize` to `present::Card`, populated from the group's best hit in `card_from_group`.

- [ ] **Step 2: The config toggle**

```rust
    /// Draw a faint box around the characters the popup is defining.
    ///
    /// On by default: it is the visible answer to "is it defining the word I
    /// am pointing at?". Turning it off also removes the overlay window
    /// entirely when `[debug] show_scan_region` is off.
    #[serde(default = "default_highlight_match")]
    pub highlight_match: bool,
```

with `fn default_highlight_match() -> bool { true }`, and `highlight_match: true` in `Config::default()`.

Tests: it defaults on; a config written before the field existed still loads **with it on** (this is what a bare `serde(default)` would break); it round-trips when set false.

- [ ] **Step 3: Emit the match rectangle**

Where the worker builds its outcome, compute the highlight: character index of `cursor_byte_offset` within `span.text`, `match_len` from the top card, `union_chars(&span.geom, from, match_len, 3)`, pushed as `ScanKind::Match`.

Gate creation and collection on `highlight_match || show_scan_region`, and **filter the capture kinds out when `show_scan_region` is off**.

- [ ] **Step 4: Verify**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result|FAILED"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error|-->" | grep -v "rustc/"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

- [ ] **Step 5: Update the README and commit**

Document `highlight_match` in the configuration section — what it draws, that it is on by default, and that `show_scan_region` is the separate debug view.

```bash
git add src/geom.rs src/ui/theme.rs src/present.rs src/config.rs src/app.rs src/ui/overlay.rs README.md
git commit -m "feat(ui): highlight the word the popup is defining"
```

---

### Task 6: Live acceptance

**Files:**
- Create: `docs/superpowers/findings/2026-07-28-accuracy-and-polish-acceptance.md`

**This task measures. It does not tune.** If a number misses, record it and stop — do not adjust `EDGE_MARGIN`, `TILE_LEN`, or any constant to make acceptance pass.

**Screen constraint:** portrait secondary monitor (x ≥ 2560) only; `probe --at` reads without moving the pointer.

- [ ] **Step 1: Tiling accuracy — spec §2.1**

**Transcribe the on-screen line by eye and write it down first.** Without recorded ground truth the criteria below are a judgement call.

Pick nine characters on one line, record their centres, then for each compare `--tiles 1` and `--tiles 3`. All three must hold:
- the resolved character scores 9/9 at `--tiles 3`, matching single-pass;
- no duplicated or spurious run in the stitched text;
- no omitted character.

**Ordinary OCR misreads are not failures of this criterion** — the recognizer will misread characters regardless. Count only **seam-local** defects, adjacent to a tile boundary. Record non-seam misreads separately as observations.

Also record forward reading distance at `--tiles 3` against single-pass's ~7 characters.

Use a multibyte-safe extraction — `.` matches one **byte** in `grep`/`sed` here:
```bash
sed -n "s/^at: .*-> Some('\(.*\)').*/\1/p"
```

- [ ] **Step 2: The highlight — spec §2.2**

With the shipped defaults (single-pass, `highlight_match` on), hover a multi-character word and confirm **exactly one** box is drawn, around the characters the popup is defining. Verify on the **single-pass path** specifically — that is the default and the origin bug lived there.

Capture the region at full resolution and inspect it. Report whether the box surrounds the matched word and nothing else.

- [ ] **Step 3: Report**

Record every number, pass or fail, plus the text used and its pixel size. State any miss plainly.

```bash
git add docs/superpowers/findings/2026-07-28-accuracy-and-polish-acceptance.md
git commit -m "docs: accuracy and polish acceptance measurements"
```

---

## Self-Review

**Spec coverage:** §3 D1-R → Task 4 Step 2. D1-R2 (leading filter + margin) → Task 3 Steps 1/3/5, applied in Task 4 Step 2. §4 D2 (`ScanKind::Match`, the toggle, kind filtering) → Task 5 Steps 1–3. D3 (index origin) → Task 5 Step 3 and its note 1. D4 (`TextGeom`, round outward, normalise) → Task 3. D5 (`match_len` plumbing) → Task 5 Step 1. D6 (cosmetics, four-way colour assert) → Task 5 Step 1. D7 (capture-guard risk) → accepted in the spec, no task. §5 → Task 2. §6 D8 (icon ownership) → Task 1 Step 3. §7's error table → Task 3 (`union_chars` `None`), Task 4 Step 2 (empty head), Task 1 Step 3 (icon fallback). §8's test list → Task 3 Step 1. §2's acceptance → Task 6.

**Gap found and closed:** §8 lists "the capture kinds are not drawn when only `highlight_match` is on" as a test. Task 5 Step 3 implements the filtering but no step writes that test. It is added to Task 5 Step 1's test set — the assertion belongs wherever the filtering lives, and the implementer must place it there rather than skip it.

**Placeholder scan:** none. Task 1 Step 2 and Step 4 are deliberately investigative (what tooling exists; whether a dependency-free resource route exists) with explicit instructions to report the route taken and to defer rather than add a dependency.

**Type consistency:** `TextGeom { char_count: usize, rect: PhysRect }` defined in Task 3, consumed in Tasks 4 and 5. `union_chars(&[TextGeom], usize, usize, i32) -> Option<PhysRect>` — Task 5 Step 3 calls it with `(from, match_len, 3)`. `drop_leading(Vec<&OcrWord>, i32, Orientation) -> Vec<&OcrWord>` — Task 4 Step 2 calls it inside the tile reader, where words are already `Vec<&OcrWord>` from `words_in`. `Card::match_len: usize` matches `Hit::match_len: usize`.

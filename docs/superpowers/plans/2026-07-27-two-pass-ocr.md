# Two-pass OCR Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the lookup engine 25 characters of forward reading distance instead of 7, without regressing the measured per-character OCR accuracy.

**Architecture:** Pass 1 is today's 500×100 capture at the cursor, used for **geometry only** — its text is discarded because it is edge-clipped by construction. From the hovered word we derive the line's band and reading direction, then read forward in line-tight tiles, each starting where the previous tile's last *unclipped* word ended. All tile geometry and the tiling loop are pure functions in `layout.rs`; only capture lives in `ocr.rs`.

**Tech Stack:** Rust 2021, `windows` 0.62.2 (Windows.Media.Ocr), no new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-27-two-pass-ocr-design.md`

## Global Constraints

- **`layout.rs`, `geom.rs`, `lookup/` and `deconj.rs` must not depend on the `windows` crate.** Parent spec's hard rule. Everything in Tasks 1–3 is pure and must compile without a screen.
- **Comments:** default is none; write one only when the code cannot be made obvious. Non-doc comment text **under 30 characters**. Rustdoc (`///`, `//!`) and `// SAFETY:` are exempt. House rule, `~/notes/wiki/rust-guidelines/Comments.md`.
- **Do not run `cargo fmt --all`.** This repo has never been rustfmt-clean (18 of 19 files differ); reformatting would bury the change in an unrelated diff on a branch pending review.
- **Clippy has 5 pre-existing errors** (`app.rs:545`, `input/hooks.rs:65`, `lookup/deconj.rs:78`, `lookup/model.rs:78`, `ui/render.rs:415`). Do not fix them; do not add new ones.
- **Cargo is not on PATH in the agent shells.** Prefix every cargo command with:
  `export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup;`
- **`data/chibipop.sqlite` (232 MiB) must never be committed.** It is gitignored via `*.sqlite`; do not add exceptions.
- **Git identity is set repo-locally** to `0x41 <76443517+stellarie@users.noreply.github.com>`. Do not change it.
- **Screen constraint:** anything that opens a window, captures, or moves the OS cursor must use the **portrait secondary monitor (x ≥ 2560)**. The 2560×1080 primary at (0,0) is the user's and must not be disturbed.
- **Chibipop holds a file lock on its own binary.** If `cargo build --release` fails with "failed to remove file", stop it first:
  `powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"`
- **Test totals drift.** Do not hard-code a total in an assertion or a commit message without running the suite and reading the number off it.

---

### Task 1: Band and tile geometry

**Files:**
- Modify: `src/text/layout.rs` (add constants + two functions + tests)

**Interfaces:**
- Consumes: `PhysRect`, `PhysPoint`, `Orientation` (all already in scope in this file).
- Produces:
  - `pub const TILE_LEN: i32`
  - `pub const BAND_FACTOR: f32`
  - `pub fn band_of(word: PhysRect, orientation: Orientation) -> PhysRect`
  - `pub fn tile_after(band: PhysRect, start: i32, orientation: Orientation, len: i32) -> PhysRect`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `src/text/layout.rs`:

```rust
    #[test]
    fn band_expands_perpendicular_and_keeps_the_word_centred() {
        let word = PhysRect { x: 1499, y: 870, w: 35, h: 34 };
        let b = band_of(word, Orientation::Horizontal);
        assert_eq!(68, b.h, "2.0x the word height");
        assert_eq!(word.center().y, b.center().y, "same centre line");
        assert_eq!(word.x, b.x, "parallel axis untouched");
    }

    #[test]
    fn band_swaps_axes_for_vertical_text() {
        let word = PhysRect { x: 1438, y: 344, w: 64, h: 70 };
        let b = band_of(word, Orientation::Vertical);
        assert_eq!(128, b.w, "2.0x the word width");
        assert_eq!(word.center().x, b.center().x);
        assert_eq!(word.y, b.y, "parallel axis untouched");
    }

    #[test]
    fn tile_runs_along_the_reading_direction() {
        let band = PhysRect { x: 100, y: 200, w: 40, h: 80 };
        let h = tile_after(band, 500, Orientation::Horizontal, TILE_LEN);
        assert_eq!(PhysRect { x: 500, y: 200, w: TILE_LEN, h: 80 }, h);

        let v = tile_after(band, 500, Orientation::Vertical, TILE_LEN);
        assert_eq!(PhysRect { x: 100, y: 500, w: 40, h: TILE_LEN }, v);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | tail -20
```

Expected: compile error, `cannot find function 'band_of' in this scope`. Paste the real output into your task report — do not paraphrase it.

- [ ] **Step 3: Write the implementation**

Add near `REGION_W`/`REGION_H` in `src/text/layout.rs`:

```rust
/// How far a tile reads along the line, in physical pixels.
///
/// The same measured limit as `REGION_W`: Windows' OCR degrades as more text
/// enters one captured image, and 400-500px is the band where a visual novel
/// at ~40px text recognises correctly. See the two-pass spec section 1.1.
pub const TILE_LEN: i32 = 500;

/// A tile's perpendicular extent, as a multiple of the hovered word's own
/// perpendicular size. Margin for ascenders and slight line drift.
pub const BAND_FACTOR: f32 = 2.0;

/// The line's perpendicular extent, derived from the hovered word.
///
/// The parallel axis is left at the word's own position and size; it carries
/// no meaning here, and `tile_after` replaces it.
pub fn band_of(word: PhysRect, orientation: Orientation) -> PhysRect {
    let c = word.center();
    match orientation {
        Orientation::Horizontal => {
            let h = ((word.h as f32) * BAND_FACTOR).round() as i32;
            PhysRect { x: word.x, y: c.y - h / 2, w: word.w, h }
        }
        Orientation::Vertical => {
            let w = ((word.w as f32) * BAND_FACTOR).round() as i32;
            PhysRect { x: c.x - w / 2, y: word.y, w, h: word.h }
        }
    }
}

/// A capture box `len` long down the reading direction from `start`, carrying
/// `band`'s perpendicular extent.
pub fn tile_after(band: PhysRect, start: i32, orientation: Orientation, len: i32) -> PhysRect {
    match orientation {
        Orientation::Horizontal => PhysRect { x: start, y: band.y, w: len, h: band.h },
        Orientation::Vertical => PhysRect { x: band.x, y: start, w: band.w, h: len },
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | grep -E "^test result|FAILED"
```

Expected: `test result: ok.` Record the count.

- [ ] **Step 5: Commit**

```bash
git add src/text/layout.rs
git commit -m "feat(ocr): band and tile geometry for two-pass reading"
```

---

### Task 2: The clipped-word split

**Files:**
- Modify: `src/text/layout.rs`

**Interfaces:**
- Consumes: `OcrWord`, `PhysRect`, `Orientation`, `TILE_LEN` from Task 1.
- Produces:
  - `pub const EDGE_MARGIN: i32`
  - `pub fn split_at_clipped<'a>(words: &'a [OcrWord], tile: PhysRect, orientation: Orientation) -> (Vec<&'a OcrWord>, i32)`

This is spec decision **D3** — the entire defence against re-introducing edge mangling at tile seams. Both edge cases below are specified behaviour, not incidental.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/text/layout.rs`:

```rust
    fn w(text: &str, x: i32, width: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y: 100, w: width, h: 40 } }
    }

    #[test]
    fn a_word_touching_the_trailing_edge_is_dropped_and_becomes_the_next_start() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![w("あ", 10, 40), w("い", 60, 40), w("う", 470, 40)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(vec!["あ", "い"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
        assert_eq!(470, next, "the clipped word's leading edge starts the next tile");
    }

    #[test]
    fn with_nothing_clipped_the_next_start_is_the_tiles_own_edge() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![w("あ", 10, 40), w("い", 60, 40)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(2, kept.len());
        assert_eq!(500, next);
    }

    /// Spec D3: a tile too narrow to hold one whole word keeps nothing and
    /// returns its own start, which D5's strict-advance rule turns into a stop.
    #[test]
    fn a_first_word_already_clipped_keeps_nothing_and_does_not_advance() {
        let tile = PhysRect { x: 400, y: 90, w: 100, h: 60 };
        let words = vec![w("あ", 400, 120)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(400, next, "must not advance past the tile's own start");
    }

    #[test]
    fn the_split_reads_top_to_bottom_for_vertical_text() {
        let tile = PhysRect { x: 90, y: 0, w: 60, h: 500 };
        let words = vec![
            OcrWord { text: "上".into(), rect: PhysRect { x: 100, y: 10, w: 40, h: 40 } },
            OcrWord { text: "下".into(), rect: PhysRect { x: 100, y: 470, w: 40, h: 40 } },
        ];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Vertical);
        assert_eq!(vec!["上"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
        assert_eq!(470, next);
    }

    /// OCR does not promise reading order, so the split must not rely on it.
    #[test]
    fn words_arriving_out_of_order_are_sorted_before_splitting() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![w("い", 60, 40), w("あ", 10, 40)];
        let (kept, _) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(vec!["あ", "い"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | tail -20
```

Expected: `cannot find function 'split_at_clipped' in this scope`. Paste the real output.

- [ ] **Step 3: Write the implementation**

```rust
/// How close a word's trailing edge may come to a tile's trailing edge before
/// it is treated as possibly clipped.
pub const EDGE_MARGIN: i32 = 4;

/// Split a tile's words into those safely inside it and the start of the next
/// tile (spec D3).
///
/// A word whose trailing edge lies within [`EDGE_MARGIN`] of the tile's own
/// trailing edge may have been cut by the capture boundary, and a cut glyph is
/// what turns 通 into 過. Such a word is discarded and its leading edge becomes
/// the next tile's start, so it is re-read whole.
///
/// Returns the tile's trailing edge when nothing was clipped, and `tile`'s own
/// leading edge when even the first word was - the caller treats a start that
/// does not advance as a stop.
pub fn split_at_clipped<'a>(
    words: &'a [OcrWord],
    tile: PhysRect,
    orientation: Orientation,
) -> (Vec<&'a OcrWord>, i32) {
    let (end, lead, trail): (i32, fn(&PhysRect) -> i32, fn(&PhysRect) -> i32) = match orientation {
        Orientation::Horizontal => (tile.x + tile.w, |r| r.x, |r| r.x + r.w),
        Orientation::Vertical => (tile.y + tile.h, |r| r.y, |r| r.y + r.h),
    };

    let mut ordered: Vec<&OcrWord> = words.iter().collect();
    ordered.sort_by_key(|word| lead(&word.rect));

    let mut kept = Vec::new();
    for word in ordered {
        if trail(&word.rect) > end - EDGE_MARGIN {
            return (kept, lead(&word.rect));
        }
        kept.push(word);
    }
    (kept, end)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | grep -E "^test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 5: Commit**

```bash
git add src/text/layout.rs
git commit -m "feat(ocr): drop words clipped by a tile edge and re-read them whole"
```

---

### Task 3: The tiling loop, with the reader injected

**Files:**
- Modify: `src/text/layout.rs`

**Interfaces:**
- Consumes: `band_of`, `tile_after`, `TILE_LEN` (Task 1); `split_at_clipped` (Task 2); `normalise` (already `pub` at `layout.rs:93`).
- Produces: `pub fn tile_forward<F>(band: PhysRect, start: i32, orientation: Orientation, max_tiles: usize, max_chars: usize, read: F) -> String where F: FnMut(PhysRect) -> Vec<OcrWord>`

The reader is a closure so the loop is testable with **no screen, no device and no OCR engine**. Termination (spec **D5**) is the reason this task exists separately: `next_start` comes from OCR output, so a pathological tile could return the same value forever.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn tiling_concatenates_successive_tiles() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, |tile| {
            calls += 1;
            vec![w("あ", tile.x + 10, 40), w("い", tile.x + 60, 40)]
        });
        assert_eq!(3, calls, "runs up to max_tiles");
        assert_eq!("あいあいあい", text);
    }

    #[test]
    fn tiling_stops_once_max_chars_is_reached() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        tile_forward(band, 0, Orientation::Horizontal, 5, 2, |tile| {
            calls += 1;
            vec![w("あ", tile.x + 10, 40), w("い", tile.x + 60, 40)]
        });
        assert_eq!(1, calls, "two characters already meet the budget");
    }

    #[test]
    fn tiling_stops_when_a_tile_recognises_nothing() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, |_| {
            calls += 1;
            Vec::new()
        });
        assert_eq!(1, calls);
        assert_eq!("", text);
    }

    /// Spec D5. Without the strict-advance guard this loops until max_tiles,
    /// and with a larger cap it would not terminate at all. The reader here
    /// always returns one word that fills its whole tile, so every split
    /// reports the tile's own start back.
    #[test]
    fn tiling_stops_when_the_next_start_does_not_advance() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        tile_forward(band, 0, Orientation::Horizontal, 100, 25, |tile| {
            calls += 1;
            vec![w("あ", tile.x, tile.w)]
        });
        assert_eq!(1, calls, "a start that cannot advance must stop the loop");
    }

    #[test]
    fn tiling_normalises_across_a_seam() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut first = true;
        let text = tile_forward(band, 0, Orientation::Horizontal, 2, 25, |tile| {
            let out = if first { vec![w("ツ", tile.x + 10, 40)] } else { vec![w("-ル", tile.x + 10, 40)] };
            first = false;
            out
        });
        assert_eq!("ツール", text, "the hyphen after kana normalises across the join");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | tail -20
```

Expected: `cannot find function 'tile_forward' in this scope`. Paste the real output.

- [ ] **Step 3: Write the implementation**

```rust
/// Read forward along a line in tiles, returning the concatenated text.
///
/// `read` performs one capture-and-recognise for a tile and returns its words
/// in virtual-desktop coordinates; injecting it keeps this loop testable with
/// no screen. Stops on any of: `max_chars` reached, a tile recognising
/// nothing, a start that does not advance (spec D5), or `max_tiles`.
///
/// The result is normalised once at the end rather than per tile, so a
/// sequence split across a seam still normalises as one string.
pub fn tile_forward<F>(
    band: PhysRect,
    start: i32,
    orientation: Orientation,
    max_tiles: usize,
    max_chars: usize,
    mut read: F,
) -> String
where
    F: FnMut(PhysRect) -> Vec<OcrWord>,
{
    let mut out = String::new();
    let mut start = start;

    for _ in 0..max_tiles {
        if out.chars().count() >= max_chars {
            break;
        }
        let tile = tile_after(band, start, orientation, TILE_LEN);
        let words = read(tile);
        if words.is_empty() {
            break;
        }
        let (kept, next) = split_at_clipped(&words, tile, orientation);
        for word in kept {
            out.push_str(&word.text);
        }
        if next <= start {
            break;
        }
        start = next;
    }

    normalise(&out)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib text::layout 2>&1 | grep -E "^test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 5: Verify the termination test is a real negative control**

Temporarily delete the `if next <= start { break; }` lines, re-run **only** `tiling_stops_when_the_next_start_does_not_advance`, and confirm it FAILS (it will report 100 calls, not 1). Then restore the guard and confirm it passes again. Record both outputs in your task report. A guard whose test passes without it is not a guard.

- [ ] **Step 6: Commit**

```bash
git add src/text/layout.rs
git commit -m "feat(ocr): forward tiling loop with a strict-advance termination guard"
```

---

### Task 4: `[ocr] max_ocr_passes` configuration

**Files:**
- Modify: `src/config.rs`
- Modify: `docs/superpowers/specs/2026-07-27-m3-popup-design.md` (§4.3 config block)
- Modify: `README.md` (configuration section)

**Interfaces:**
- Produces: `Config::ocr: OcrConfig`, `OcrConfig::max_ocr_passes: u8`, `pub struct OcrConfig`.

**The `#[serde(default)]` is load-bearing, not cosmetic.** `config.rs` treats malformed TOML as a hard error naming the file rather than falling back to defaults (see its module docs). A new *required* section makes every existing `chibipop.toml` fail to parse, and chibipop refuses to start after an upgrade. The user's own config file has no `[ocr]` section right now.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/config.rs`:

```rust
    #[test]
    fn ocr_passes_defaults_to_three() {
        assert_eq!(3, Config::default().ocr.max_ocr_passes);
    }

    /// The kill switch (spec D6): one pass is today's known-good single
    /// capture, reachable by editing one line with no rebuild.
    #[test]
    fn a_single_pass_round_trips() {
        let p = tmp("onepass");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.ocr.max_ocr_passes = 1;
        c.save(&p).unwrap();
        assert_eq!(1, load_or_create(&p).unwrap().ocr.max_ocr_passes);
        let _ = std::fs::remove_file(&p);
    }

    /// Every config file written before this feature existed lacks an [ocr]
    /// section. Without serde(default) they stop loading and chibipop refuses
    /// to start, because malformed TOML is deliberately a hard error here.
    #[test]
    fn a_config_written_before_the_ocr_section_existed_still_loads() {
        let p = tmp("no_ocr_section");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-[ocr] config must still load");
        assert_eq!(3, c.ocr.max_ocr_passes, "missing section takes the default");
        let _ = std::fs::remove_file(&p);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib config 2>&1 | tail -20
```

Expected: `no field 'ocr' on type 'Config'`. Paste the real output.

- [ ] **Step 3: Write the implementation**

In `src/config.rs`, add the field to `Config` (after `dictionaries`):

```rust
    #[serde(default)]
    pub ocr: OcrConfig,
```

Add the struct next to the other section structs:

```rust
/// `[ocr]`. Carries `#[serde(default)]` at the `Config` level so a file
/// written before this section existed still loads - see `load_or_create`,
/// which treats malformed TOML as a hard error rather than falling back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrConfig {
    /// Total OCR captures per hover: one to locate the hovered word, the
    /// rest to read forward from it.
    ///
    /// **`1` disables tiling entirely** and restores the single-capture
    /// behaviour that shipped before two-pass. That is the point of the
    /// setting: `TILE_LEN` was derived from one game at one text size, so if
    /// tiling misbehaves elsewhere this reverts to known-good with one line
    /// and no rebuild. Capping the work is the secondary benefit - the
    /// tiling loop already stops at 25 characters on its own, so this rarely
    /// binds.
    #[serde(default = "default_max_ocr_passes")]
    pub max_ocr_passes: u8,
}

fn default_max_ocr_passes() -> u8 {
    3
}

impl Default for OcrConfig {
    fn default() -> OcrConfig {
        OcrConfig { max_ocr_passes: default_max_ocr_passes() }
    }
}
```

Add to `impl Default for Config`, after `dictionaries`:

```rust
            ocr: OcrConfig::default(),
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo test --lib config 2>&1 | grep -E "^test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 5: Update the docs**

In `docs/superpowers/specs/2026-07-27-m3-popup-design.md` §4.3, append to the TOML block:

```toml

[ocr]
max_ocr_passes = 3      # 1 disables forward tiling - see the two-pass spec
```

In `README.md`, add the same two lines to the configuration example, and after the `exclude_from_capture` paragraph add:

```markdown
**`max_ocr_passes`** is how many screen captures each hover costs: one to find
the word under the cursor, the rest to read forward from it. Set it to `1` to
turn forward tiling off entirely and get the older, shorter-reaching behaviour
back — useful if OCR misbehaves on content very different from what the tile
width was tuned against.
```

- [ ] **Step 6: Commit**

```bash
git add src/config.rs README.md docs/superpowers/specs/2026-07-27-m3-popup-design.md
git commit -m "feat(config): max_ocr_passes, with 1 as a tiling kill switch"
```

---

### Task 5: Wire tiling into the OCR source and the app

**Files:**
- Modify: `src/text/ocr.rs` (`OcrTextSource::new`, new `resolve_at_tiled`)
- Modify: `src/app.rs:498` (pass the configured pass count)
- Modify: `src/main.rs` (`probe --tiles N`, `watch` unchanged default)

**Interfaces:**
- Consumes: `band_of`, `tile_forward` (Tasks 1/3); `OcrConfig::max_ocr_passes` (Task 4).
- Produces: `OcrTextSource::new(max_passes: u8)`, `OcrTextSource::resolve_at_tiled(&self, cursor: PhysPoint) -> Result<Option<Resolved>>`.

**Pass 1's text is discarded (spec D1).** It is edge-clipped by construction — that is how `細` became `紐`. Only its geometry is used.

- [ ] **Step 1: Change `OcrTextSource::new` to take the pass count**

In `src/text/ocr.rs`, change the signature at line 96 and store the field:

```rust
    pub fn new(max_passes: u8) -> Result<Self> {
```

Add `max_passes: u8` to the `OcrTextSource` struct and set it in the constructor's struct literal. There are **three** call sites — verify with `grep -rn "OcrTextSource::new()" src/` before and after:

- `src/app.rs:498` becomes:

```rust
    let ocr = match OcrTextSource::new(cfg.ocr.max_ocr_passes).context("creating the OCR text source") {
```

- `src/main.rs:92` (the `Probe` arm) — takes the `tiles` flag added in Step 4.
- `src/main.rs:153` (the `Watch` arm) — `OcrTextSource::new(3)`.

- [ ] **Step 2: Add `resolve_at_tiled`**

In `src/text/ocr.rs`:

```rust
    /// Resolve a hover, reading forward in tiles (the two-pass spec).
    ///
    /// Pass 1 is [`resolve_at_verbose`](Self::resolve_at_verbose)'s capture,
    /// used for geometry only - its text is edge-clipped at its own boundary
    /// and is discarded (spec D1). Tiles then re-read forward from the hovered
    /// word's leading edge.
    ///
    /// Falls back to pass 1's own span whenever tiling adds nothing: one
    /// configured pass, an empty tiling result, or a tile that errors. Tiling
    /// must never turn a working hover into a failed one.
    pub fn resolve_at_tiled(&self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        let (_, resolved) = self.resolve_at_verbose(cursor)?;
        let Some(first) = resolved else { return Ok(None) };
        if self.max_passes <= 1 {
            return Ok(Some(first));
        }

        let band = band_of(first.span.anchor, first.orientation);
        let start = match first.orientation {
            Orientation::Horizontal => first.span.anchor.x,
            Orientation::Vertical => first.span.anchor.y,
        };

        let mut failed = false;
        let text = tile_forward(
            band,
            start,
            first.orientation,
            usize::from(self.max_passes - 1),
            MAX_LOOKUP_CHARS,
            |tile| match self.words_in(tile) {
                Ok(words) => words,
                Err(e) => {
                    if !failed {
                        eprintln!("chibipop: tile capture failed, using what was read: {e:#}");
                        failed = true;
                    }
                    Vec::new()
                }
            },
        );

        if text.is_empty() {
            return Ok(Some(first));
        }
        Ok(Some(Resolved {
            span: TextSpan { text, cursor_byte_offset: 0, anchor: first.span.anchor },
            orientation: first.orientation,
        }))
    }

    /// One capture-and-recognise, flattened to words in desktop coordinates.
    fn words_in(&self, tile: PhysRect) -> Result<Vec<OcrWord>> {
        let (buf, w, h) = capture_upscaled(tile)?;
        let origin = PhysPoint { x: tile.x, y: tile.y };
        Ok(recognise(&self.engine, &buf, w, h)?
            .into_iter()
            .flat_map(|l| l.words)
            .map(|word| OcrWord {
                rect: map_from_upscaled(word.rect, origin, UPSCALE),
                text: word.text,
            })
            .collect())
    }
```

Add the needed imports to the `use` lines at the top of `ocr.rs`: `band_of`, `tile_forward`, `Orientation` from `crate::text::layout`, and `MAX_LOOKUP_CHARS` from `crate::lookup::engine`.

- [ ] **Step 3: Point the app and `watch` at the tiled path**

In `src/app.rs`, `resolve_trigger` calls `resolve_at` **twice** — once inside the capture-guard branch (line 620) and once without it (line 624). **Both** become `resolve_at_tiled`. In `src/main.rs`'s `Watch` arm (line 179), replace `source.resolve_at(cursor)` with `source.resolve_at_tiled(cursor)`.

Note the guard's shape is already correct for tiling: `hide_for_capture()` / `restore_after_capture()` bracket the whole resolve, so all of a hover's tiles happen inside **one** hide window rather than flickering per tile. Do not move the guard inside the tiling loop.

**Accepted consequence (user decision, 2026-07-27):** with `exclude_from_capture = false` that single hide window now spans 2-3 captures instead of 1, so the popup's blink on re-resolution gets roughly 3× longer. Accepted as-is rather than capping tiles while recording. Task 6 reports the measured duration as an observation.

Leave `TextSource::at` and `probe`'s `resolve_in_region` on the single-pass path — `probe` is the diagnostic that must still be able to isolate one capture.

- [ ] **Step 4: Add `--tiles N` to probe**

In `src/main.rs`'s `Probe` variant add:

```rust
        /// Total OCR passes, as [ocr] max_ocr_passes would set. 1 is single
        /// capture. Ignored when --region is given, which is single-capture
        /// by definition.
        #[arg(long, default_value_t = 3)]
        tiles: u8,
```

In the `Probe` arm, construct the source with it (`OcrTextSource::new(tiles)`), and after the existing single-capture report, when `region` was not overridden and `tiles > 1`, print the tiled span:

```rust
            if tiles > 1 {
                match source.resolve_at_tiled(cursor)? {
                    None => println!("\ntiled:   nothing resolved"),
                    Some(r) => println!("\ntiled:   {:?}  ({} chars)", r.span.text, r.span.text.chars().count()),
                }
            }
```

- [ ] **Step 5: Build and run the whole suite**

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"; cargo test 2>&1 | grep -E "^test result|FAILED|^error"
```

Expected: all green. Then confirm no new clippy errors — exactly the 5 listed in Global Constraints, no more:

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error" | head
```

- [ ] **Step 6: Commit**

```bash
git add src/text/ocr.rs src/app.rs src/main.rs
git commit -m "feat(ocr): read forward in tiles, with pass 1 used for geometry only"
```

---

### Task 6: Live acceptance against real text

**Files:**
- Create: `docs/superpowers/findings/2026-07-27-two-pass-acceptance.md`

**This task measures. It does not tune.** If the numbers miss, record them and stop — do not adjust `TILE_LEN`, `BAND_FACTOR` or `EDGE_MARGIN` to make the acceptance test pass. A constant tuned against its own acceptance test measures nothing.

**Screen constraint:** put the test text on the **portrait secondary monitor (x ≥ 2560)**. Do not capture, click, or move the cursor on the primary. `probe --at` reads a coordinate without moving the pointer, which is the whole reason it exists — use it rather than any cursor-driving tool.

- [ ] **Step 1: Get Japanese text on the vertical monitor and locate a line**

The user has windows available there. Build the release binary first:

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup; powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"; cargo build --release
```

Then sweep for text and record the word boxes, which give you both the row's y and each character's centre x:

```bash
./target/release/chibipop.exe probe --at 2900,600 | head -20
```

- [ ] **Step 2: Measure accuracy — spec §2 criterion 1**

Pick ten characters from one line and record their centres. For each, compare the resolved character against the truth. Extract with a pattern that does not split multibyte characters — `.` matches a single **byte** in `grep`/`sed` here, so `Some\('.'\)` silently matches nothing but ASCII:

```bash
ex() { ./target/release/chibipop.exe probe --at "$1,$2" --tiles "$3" 2>&1 | sed -n "s/^at: .*-> Some('\(.*\)').*/\1/p" | head -1; }
```

Run each character at `--tiles 1` and `--tiles 3`. **Criterion: `--tiles 3` scores at least as well as `--tiles 1`, and `--tiles 1` still scores 10/10.**

- [ ] **Step 3: Measure reading distance — spec §2 criterion 2**

```bash
./target/release/chibipop.exe probe --at <x>,<y> --tiles 3 | grep "^tiled:"
```

**Criterion: ≥20 characters**, against the 7 that single-pass delivers. Record both.

- [ ] **Step 4: Measure latency — spec §2 criterion 3**

```bash
for t in 1 3; do for i in 1 2 3; do s=$(date +%s%N); ./target/release/chibipop.exe probe --at <x>,<y> --tiles $t >/dev/null 2>&1; e=$(date +%s%N); echo "tiles=$t run$i: $(( (e-s)/1000000 )) ms"; done; done
```

Both figures include ~230ms of process start and engine init, so report the **difference** between the `--tiles 1` and `--tiles 3` medians. **Criterion: under +150ms.**

- [ ] **Step 5: Verify the kill switch — spec §2 criterion 4**

Set `max_ocr_passes = 1` in `target/release/chibipop.toml`, restart chibipop, and confirm from the same hover that behaviour matches `--tiles 1` exactly. Restore the file afterwards.

- [ ] **Step 6: Write the findings note and commit**

Record all four criteria with the actual numbers, pass or fail, plus the text used and its size in pixels — the tile width is size-dependent and a future reader needs to know what it was validated against. State any criterion that missed plainly; do not soften it.

```bash
git add docs/superpowers/findings/2026-07-27-two-pass-acceptance.md
git commit -m "docs: two-pass OCR acceptance measurements"
```

---

## Self-Review

**Spec coverage:** D1 → Task 5 Step 2. D2 → Tasks 1, 5. D3 → Task 2. D4 → Task 3. D5 → Task 3 Steps 1 and 5. D6 → Task 4. D7 → Tasks 1–3 in `layout.rs`, Task 5 in `ocr.rs`. D8/D9 are rejections, nothing to implement. §2's four criteria → Task 6 Steps 2–5. §5's error table → Task 5 Step 2's fallbacks and Task 3's stop conditions. §6's test layers → Tasks 1–3 unit tests, Task 6 live. §4.2's `cursor_byte_offset: 0` → Task 5 Step 2.

**Gap found and closed:** §6 calls for an integration test against `tests/ocr_fixture.rs` with a two-tile fixture. That is **not** in this plan — the existing fixture is a single 900×300 BGRA blob, and synthesising a wider one means rendering new Japanese text to a bitmap, which is its own task with its own tooling. Task 3's injected reader already covers the seam logic without a device, and Task 6 covers it against real text. Recorded here as a deliberate omission rather than silently dropped; if the live acceptance in Task 6 shows seam problems, the fixture becomes the follow-up.

**Placeholder scan:** none — every step carries its code or its exact command.

**Type consistency:** `band_of(PhysRect, Orientation) -> PhysRect` matches its use in Task 5. `split_at_clipped` returns `(Vec<&OcrWord>, i32)` and Task 3 destructures `(kept, next)`. `tile_forward`'s `max_tiles: usize` is fed `usize::from(max_passes - 1)` where `max_passes: u8` is guaranteed ≥ 2 by the early return above it, so the subtraction cannot underflow.

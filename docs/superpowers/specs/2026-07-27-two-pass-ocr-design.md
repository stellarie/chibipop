# Two-pass OCR — reading distance without losing accuracy

**Status:** designed, not implemented
**Date:** 2026-07-27
**Parent:** `2026-07-26-chibipop-design.md` §4.2 (text acquisition), `2026-07-27-m2-ocr-tier-design.md`

---

## 1. Problem

`REGION_W`/`REGION_H` were reduced from 900×300 to 500×100 after measuring that Windows'
`Windows.Media.Ocr` degrades sharply as more text enters the captured image. That fixed accuracy and
shipped. It also capped **reading distance**: a 500-wide box centred on the cursor leaves only 250px
ahead of it, which at visual-novel text size (~40px per character) is about **7 characters**.

`engine::MAX_LOOKUP_CHARS` is **25**. So the lookup engine is willing to consider 25 characters of
context and is being handed 7. Long compounds truncate.

Widening the box to recover those characters is exactly what broke accuracy. The two requirements are
in direct opposition **within a single OCR call**, and no choice of one rectangle satisfies both.

### 1.1 What was measured

Hovering ten characters of one visual-novel line and scoring the character actually handed to lookup:

| capture box | correct |
|---|---|
| 900×300 (the original) | **6/10** — `通`→`過`, `に`→`0`, `風` and `私` dropped entirely |
| 500×100 | **10/10** |
| 400×100 | 10/10 on that line, `ロ`→`回` on the next |

**Width is the dominant variable, not height.** At width 900, four different heights — 300, 150, 100,
80 — all failed *identically*. At widths 500 and 600 the same character read correctly. The usable
band is roughly 400–500px wide; both directions fail outside it.

**The wide box was not buying reading distance.** It returned six forward characters of
`…を過抜ける` against the narrow box's seven of `…を通り抜ける。制`. The extra 400px was spent on
mangling, not on text.

**Recognition is unstable with respect to framing.** Identical screen text, identical horizontal
extent, capture window moved 50px vertically:

```
region y=700..1000 → "よ風景に私は目を細めて、すぐに廊下を通り抜ける3"
region y=750..1050 → "0は目を細めて。すぐに廊下を過抜ける◎"
ground truth       → "そんな風景に私は目を細めて、すぐに廊下を通り抜ける。"
```

Because the box is centred on the cursor, every mouse movement re-rolls that dice. This is why the
failure read as intermittent rather than reproducible.

**Latency headroom.** A full `probe` — process start, WinRT engine init, dictionary open, one OCR —
takes **233ms** (three runs: 233/231/235). The `watch` loop sustains 125ms cycles including OCR and a
dictionary lookup, which bounds a single OCR call well below that. Budget agreed with the user:
**up to ~150ms extra per hover**, i.e. 2–3 OCR calls total.

---

## 2. Goal and acceptance test

Deliver 25 characters of forward reading distance at visual-novel text size **without** regressing the
measured per-character accuracy.

Acceptance is the diagnosis's own measurements, re-run:

1. **Accuracy holds.** The ten-character sweep stays at **10/10**. Not "looks better" — the same
   sweep, the same score.
2. **Distance improves.** Forward characters from the hovered position goes from **7** to **≥20**.
3. **Latency.** Measured, recorded, and within **+150ms** per hover over single-pass. Recorded as an
   observation, not assumed from the ~40ms-per-call estimate.
4. **The kill switch works.** `max_ocr_passes = 1` reproduces today's single-pass behaviour exactly.

Failing 1 fails the feature. Failing 3 is reported, not hidden.

---

## 3. Decisions

### D1 — Pass 1 is used for geometry only; its text is discarded

Pass 1 is today's 500×100 box at the cursor. It answers three questions: **where is the hovered
character**, **which way does the line run**, and **how thick is the line**. Its recognised *text* is
thrown away.

This is the load-bearing decision. Pass 1's text is edge-clipped at its own boundary — that is
precisely how `細` became `紐` and `そん` became `よ` in the measurements. Reusing its trailing
characters would import the bug the feature exists to fix.

### D2 — Tiles are line-tight and anchored to the hovered word, not to the cursor

Each tile's perpendicular extent comes from the measured line band, not from a cursor-centred
constant. Its start comes from the hovered word's leading edge.

Anchoring to the word box rather than the raw cursor pixel is what buys stability: pointing anywhere
within a character produces identical tiling, so a given word resolves the same way every time.

### D3 — A word touching a tile's trailing edge is discarded as clipped

The stitching rule, stated once: **a word whose trailing edge lies within `EDGE_MARGIN` of the tile's
trailing boundary is discarded, and its leading edge becomes the next tile's start.** A word further
inside than that is kept.

Two edge cases that would otherwise be read either way:

- **No word is clipped** — every word sits clear of the boundary. `next_start` is then the tile's own
  trailing edge, so the next tile continues from where this one ended.
- **The first word is already clipped** — nothing is kept and `next_start` is **that word's own
  leading edge**. When the word begins at the tile's start (it fills the whole tile), that equals the
  current start, which violates D5's strict-advance rule and stops the loop — the intended outcome,
  since a tile too narrow to hold one whole word cannot be made to progress by retrying. When the word
  begins later in the tile, `next_start` advances to it and the next tile re-reads it whole, which is
  the case that actually recovers a glyph straddling a seam.

This is the entire defence against re-introducing edge mangling at tile seams.

### D4 — Tiling stops adaptively

Stop when any of: accumulated characters ≥ `MAX_LOOKUP_CHARS` (25); a tile yields no new words; the
next start does not advance (D5); or `max_ocr_passes` is reached.

The common case ends after two OCR calls. Only long compounds at large text sizes pay for a third. At
small text (a browser at ~16px) a single 500px tile already exceeds 25 characters, so tiling
effectively disables itself.

### D5 — The next start must strictly advance, or the loop stops

`next_start` is derived from OCR output, so a pathological tile can return the same value repeatedly.

**Corrected 2026-07-28, after review.** An earlier draft called strict advance the *primary*
termination guarantee and `max_ocr_passes` a mere backstop. That is backwards: the loop is written as
`for _ in 0..max_tiles`, which bounds it absolutely — non-termination is impossible with or without
the check. The advance guard's real job is to stop *early* rather than spend up to `max_tiles` real
screen captures re-reading a position that cannot move. That is a cost guarantee, not a termination
one, and the distinction matters because each wasted iteration is a full capture-and-recognise.

Two properties need dedicated tests, and both must be proven by deleting the line they cover and
watching the test fail:

- the advance guard itself — a tile whose words cannot advance the start must stop the loop;
- **the advance itself** (`start = next`) — a reader keyed to the tile's absolute position must
  observe successive tiles landing on successive screen regions. Without this, a regression that
  reads the same region forever still passes every other test in the suite.

### D6 — `[ocr] max_ocr_passes` is a kill switch first, a latency cap second

Default `3`. `1` disables tiling entirely and restores today's single-pass behaviour.

As a tuning knob this would be near-useless — D4's adaptive stop already ends at 25 characters, so the
cap rarely binds. Its real value is as an escape hatch: the 400–500px tile width is derived from
**one game at one text size**, and if it misbehaves elsewhere the user can revert to known-good
behaviour by editing one line, with no rebuild and no waiting for a fix.

**The field must carry `#[serde(default)]`.** `config.rs` deliberately treats malformed TOML as a hard
error naming the file rather than silently falling back to defaults (see its module docs). A new
*required* section would therefore make every existing `chibipop.toml` fail to load, and chibipop
would refuse to start after an upgrade. `serde(default)` makes the section optional and old files keep
working.

`probe` gains `--tiles N` to match, since `probe` and `watch` do not read the config — only `run`
does. Without it the diagnostic silently stops matching the app it is diagnosing.

### D7 — Pure geometry in `layout.rs`, capture and orchestration in `ocr.rs`

`lookup/`, `deconj.rs`, `geom.rs` and `layout.rs` must not depend on the `windows` crate — the parent
spec's hard rule. Tile geometry and stitching are pure functions over plain rectangles, so they live in
`layout.rs` and are testable with no screen, no device and no OCR engine.

This matters more than tidiness: the bugs in this feature will live in tile boundaries and seam
stitching, and those must be reachable by ordinary unit tests rather than only by hovering a game.

### D8 — Rejected: "locate with a wide pass, read with a narrow one"

The textbook two-pass shape. **Ruled out by measurement**, and recorded here so it is not re-proposed.

When a wide capture fails, its *geometry* fails too, not just its text. In the failing 900px read, the
word box covering `そんな風景に私` — seven characters spanning 260px of screen — came back as a
single **10px-wide** box labelled `"0"`. A wide pass cannot reliably report where the line is, so it
cannot serve as the locator.

### D9 — Rejected: fixed two-tile with a forward shift

Pass 1 at the cursor, pass 2 shifted forward ~450px, stitched on an overlap. Simpler and always
exactly two calls, but height stays cursor-derived rather than line-derived, so it does not follow a
line that drifts, and it reaches ~14 characters rather than 25. Kept as the fallback design if D3's
seam handling proves troublesome in implementation.

---

## 4. Architecture

```
cursor
 → region_around(cursor)                       500×100, unchanged        [OCR 1]
 → hit_scan → hovered word + its line
 → orientation_of(line)                        Horizontal | Vertical
 → band_of(hovered, orientation)               perpendicular extent
 ↓
 loop, at most max_ocr_passes - 1 tiles:
   tile = tile_after(band, start, orientation, TILE_LEN)                 [OCR 2..N]
   (complete, next_start) = split_at_clipped(words, tile, orientation)
   append complete
   stop per D4 / D5
 ↓
 TextSpan { text: stitched forward text, cursor_byte_offset: 0, anchor: hovered word }
```

| Function | Home | Purity |
|---|---|---|
| `band_of(word, orientation) -> PhysRect` | `layout.rs` | pure |
| `tile_after(band, start, orientation, len) -> PhysRect` | `layout.rs` | pure |
| `split_at_clipped(words, tile, orientation) -> (Vec<&OcrWord>, i32)` | `layout.rs` | pure |
| tiling loop | `ocr.rs` | I/O |

### 4.1 Constants

| Constant | Value | Rationale |
|---|---|---|
| `REGION_W` × `REGION_H` | 500 × 100 | unchanged; pass 1 is today's box |
| `TILE_LEN` | 500 | the measured accurate width (§1.1) |
| `BAND_FACTOR` | 3.0, floored at `REGION_H` | band extent = max(factor × hovered word's perpendicular size, `REGION_H`), **centred on that word's centre** — margin for ascenders and slight line drift, with a floor so small text still clears the recogniser's threshold |
| `EDGE_MARGIN` | 4px | a word whose trailing edge is within this of the tile's trailing edge counts as clipped (D3) |
| default `max_ocr_passes` | 3 | 1 (pass 1) + 2 tiles ≈ 1000px ≈ 25 chars at VN size |

Measured on real on-screen Japanese at a fixed 500px width: a 44px-tall band (what `BAND_FACTOR = 2.0` produced for ~22-26px glyphs) recognised nothing, and the threshold between failure and success sat between 60px and 66px.

### 4.2 Behaviour change: `cursor_byte_offset` becomes 0

The stitched text begins **at** the hovered character rather than at the line's start. Downstream code
already reads `&span.text[span.cursor_byte_offset..]`, so it continues to work unchanged, but the
field's meaning changes and `probe` will report `byte 0`. Recorded here because a reader of `probe`
output would otherwise think the resolver had regressed.

---

## 5. Error handling

Tiling is an enhancement layered on a working popup. **It must never turn a successful hover into a
failed one.** Every failure degrades to the best result accumulated so far.

| Failure | Response |
|---|---|
| Pass 1 finds nothing | `None` — unchanged from today |
| A tile's capture or OCR errors | Stop tiling, return what is accumulated; log once |
| A tile recognises no words | Stop, return what is accumulated |
| Tile extends past the monitor | Clamp; if it clamps to zero extent, stop |
| Every word in a tile is clipped | `next_start` does not advance → stop (D5) |

---

## 6. Testing

**Pure unit tests** — `band_of`, `tile_after`, `split_at_clipped`, each **horizontal and vertical**.
Vertical text swaps every axis and is where an off-by-one will hide.

**The stitching rule (D3)** — a word flush against the trailing edge is dropped *and* becomes the next
start; a word comfortably inside is kept. Both directions asserted, not just the happy one.

**Termination (D5)** — a crafted tile result whose words do not advance the start must stop rather than
spin. This is a genuine negative control: it must be confirmed to fail without the guard.

**Integration** — `tests/ocr_fixture.rs`, extended with a fixture wide enough to require two tiles.

**Live acceptance** — §2's four measurements, re-run and recorded.

---

## 7. Non-goals

- Rich rendering, furigana handling, or any change to the lookup engine.
- Parallel tiles. Tile *n+1*'s start depends on tile *n*'s output, so tiles are inherently sequential.
- Backward reading. The lookup engine reads forward from the cursor only; extending backward would
  change ranking semantics and is out of scope.
- Auto-deriving `TILE_LEN` from measured glyph size. Attractive, but it would be tuning a tuning
  parameter with no second data point to validate against. See §8.

---

## 8. Open risks

**`TILE_LEN ≈ 500` rests on two lines from one game at ~40px text.** It is not established as a
property of the recognizer. If it proves font- or size-dependent, the tiling architecture still holds
and only the constant moves — `probe --region W,H` exists to re-derive it, and `max_ocr_passes = 1`
disables the feature meanwhile.

**Latency is additive and unmeasured at the tile level.** The ~40ms-per-call figure is inferred from
`watch` sustaining 125ms cycles, not measured directly. §2's acceptance test measures it properly.

**Accuracy at tile seams is the untested case.** Every measurement so far is of a single capture. D3
is designed to prevent seam mangling but has never been exercised against real screen text; the
integration fixture and live acceptance are what will settle it.

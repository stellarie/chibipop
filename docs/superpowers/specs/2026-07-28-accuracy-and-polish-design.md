# Accuracy and polish — tiling correctness, match highlight, vertical text, identity

**Status:** designed, not implemented. **Revision 2 — rewritten after independent review returned NEEDS REWORK on revision 1.**
**Date:** 2026-07-28
**Parent:** `2026-07-26-chibipop-design.md`
**Revises:** `2026-07-27-two-pass-ocr-design.md` (D1), `2026-07-28-scan-overlay-design.md` (what is drawn)

Four changes in one round.

**Ordering is a requirement, not a convenience.** §6 (identity) shares no code with anything else, and
§5 (vertical) is a measurement whose inputs `probe` already prints. Only §3 and §4 are genuinely
coupled — both change what the forward-reading path returns. §3 is also the only item that can fail its
acceptance test. **§6 and §5 therefore land first and independently**, so a §3 failure cannot gate three
working changes. This was raised in review as an argument for splitting the round; sequencing achieves
the same protection while keeping one delivery.

---

## 1. Problem

**Tiling resolves the wrong character.** Measured 2026-07-28 on a browser reader at ~25px text, nine
hovers on one line, scoring the character actually handed to lookup: **single-pass 9/9, three passes
4/9**. Tiling is disabled by default because of it (`max_ocr_passes = 1`).

**The overlay draws capture boxes, not meaning.** It shows where chibipop looked — which found two real
defects — but not *what it decided*, so confirming "the popup is defining the word I am pointing at"
still means reading numbers.

**Vertical text is spotty.** `region_around` is orientation-blind — always 500 wide × 100 tall — so a
vertical line gets five columns it does not care about and ~100px, about three characters, of the axis
it does.

**chibipop has no identity.** The tray shows `IDI_APPLICATION`, the generic Windows box.

## 2. Goals and acceptance

1. **Tiling accuracy.** Re-run the nine-hover sweep that found the bug, with `--tiles 3`.

   **Transcribe the on-screen line by eye and record it as ground truth *before* running the sweep.**
   Without that written down first, the criteria below are a judgement call dressed as acceptance.

   **All three must hold**, and the last two exist because the first alone passes on corrupt text:
   - the resolved character scores **9/9**, matching single-pass;
   - **no duplicated or spurious run** — a string like `…明りを` + `を振り向けた` duplicates a character
     mid-string while `cursor_byte_offset: 0` is still true, so a first-character check passes it;
   - **no omitted character** — the leading and trailing filters can otherwise discard the same glyph
     from both captures and it vanishes silently.

   **Ordinary OCR misrecognition is not a failure of this criterion.** The recognizer will misread
   characters (§1 records `明`→`月`), and that is not a stitching defect. Count only **seam-local**
   duplications and omissions — a discrepancy adjacent to a tile boundary — as failures, and record
   non-seam misreads separately as observations.

   Reading distance must also exceed single-pass's ~7 characters. If accuracy and distance cannot both
   hold, that is a real result: tiling stays off and it is recorded.
2. **Match highlight.** Hovering draws exactly **one** faint padded box around the characters the popup
   is defining — hover 可哀想, the popup shows 可哀想, one box surrounds 可哀想. Verified on the
   **single-pass path**, which is the shipped default.
3. **Vertical text.** Measured, then improved against that measurement. The acceptance number is set by
   §5's measurement, not asserted here, because no measurement of vertical text exists.
4. **Identity.** A real tray icon; the executable's icon too if reachable without a new dependency.

---

## 3. Tiling accuracy

### D1-R — pass 1 keeps its head

Original D1: *"pass 1 is used for geometry only; its text is discarded"*, because pass 1's text is
edge-clipped by construction.

**True at pass 1's edges, false at its centre.** The hovered character sits at the middle of pass 1's
capture — the best-framed reading of it anywhere — and the original design discarded it and re-read it
near a *tile* boundary. That is failure mode 1: `明` read as `月`, `振` as `一`, both correct in pass 1.

**Revised: pass 1's text is kept from the hovered word up to its last unclipped word; tiles continue
after it.** Each character is read once, by whichever capture framed it best.

`split_at_clipped` decides where the head ends — the same rule and the same function the tile seam
already uses, applied one capture earlier. Note this is not free plumbing: `resolve_at_tiled_scanned`
currently discards the recognised lines and `Resolved` exposes only the anchor, so pass 1's words must
be retained rather than re-derived.

**Tile 1 starts at the last kept word's trailing edge, not at pass 1's region edge.** With nothing
clipped, `split_at_clipped` returns the region's own trailing edge — and a glyph straddling that
boundary which OCR dropped entirely (measured: `風` and `私` dropped) would then be skipped by both
captures. Starting at the last kept word's trailing edge leaves that gap inside tile 1's range.

### D1-R2 — tiles must filter their leading edge too

**Revision 1 claimed keeping the head removed failure mode 2 structurally. That was wrong**, and the
error is worth recording because it nearly shipped.

The leading-word leak belongs to **tiles**, not to pass 1: `words_in` returns the whole nearest line,
and `split_at_clipped` filters only the **trailing** edge. A tile opening at the head's end is still
handed words to the *left* of that start. Keeping the head moves the corruption from the front of the
string to its middle — `…明りを` + `を振り向けた` — where `cursor_byte_offset: 0` remains true and a
first-character acceptance check passes while the text is wrong.

**So a leading-edge filter is a required complement, not a rejected alternative:** a tile discards any
word whose leading edge falls before the tile's start. Its own test, and §2.1's whole-text criterion
exists to catch its absence.

**The leading filter needs the same tolerance the trailing one has, or it deletes what D1-R saves.**
`split_at_clipped` defers a straddling word by returning **tile 1's** measurement of its leading edge.
Tile 2 then re-measures that same glyph from a *different* capture, through `scaled_down`'s integer
truncation. If the re-measured edge lands even one pixel before the tile's start, a naive filter
discards a word the previous tile had already deferred — and the character disappears from both
captures. The trailing side carries `EDGE_MARGIN` for exactly this uncertainty.

**The leading filter therefore discards only words whose leading edge falls more than `EDGE_MARGIN`
before the tile's start.** Symmetric with the trailing rule, and for the same reason. A test must cover
a word whose re-measured leading edge sits just inside that margin and must be **kept**.

### Rejected

- **Leading filter alone.** Fixes mode 2 only; leaves tiling corrupting the character under the cursor.
- **Overlap and vote** — read the hovered character twice, prefer pass 1 on disagreement. Keeps the
  double read, adds a reconciliation rule, and spends a capture producing a character it already had.

---

## 4. Match highlight

### D2 — the overlay draws the match, not only the captures

`ScanKind` gains `Match`. When a lookup produces a top hit, the overlay draws **one** rectangle: the
union of the boxes covering the matched characters, padded outward.

Capture rectangles (`Pass1`, `Tile`, `Anchor`) are **not removed** — they diagnosed the last two rounds
of defects. They stay under `[debug] show_scan_region`; the highlight gets its own setting so the
everyday case is one clean box, not four overlapping ones.

**`[popup] highlight_match`, defaulting `true`.**

**It needs `#[serde(default = "default_highlight_match")]`, not a bare `#[serde(default)]`.** A bare
`serde(default)` yields `bool`'s default — **`false`** — so every config file written before this field
existed would load with the highlight *off*, and §2.2 would fail on every current install while passing
on a fresh one. `PopupConfig` has no `Default` derive and no field-level defaults today, so there is
nothing to fall back to. This is the same distinction `OcrConfig` already documents, and revision 1
cited that lesson while making the mistake.

It lives under `[popup]` because it is user-facing behaviour, not debugging.

**The overlay window is created, and geometry collected, when *either* `highlight_match` or
`show_scan_region` is on.** Both gates currently key on `show_scan_region` alone.

**But "collected" is not "drawn".** The overlay draws every `ScanRect` it is handed, and the collector
pushes `Pass1`, `Tile` and `Anchor` whenever collection is on. If collection simply became
`show_scan_region || highlight_match`, the default path would draw **four** boxes — exactly what this
decision exists to avoid, and what §2.2 forbids. **The capture kinds must be filtered out when only
`highlight_match` is on**, at collection or at draw. Whichever, it must be tested.

### D3 — the index origin is the hovered character, not the string's start

**This is the everyday path and revision 1 got it wrong.** Lookup runs on
`span.text[cursor_byte_offset..]`, and `match_len` counts characters **of that slice**. With
`max_ocr_passes = 1` — the shipped default — `resolve` builds text for the whole line and
`cursor_byte_offset` is **not** zero. Taking "the first `match_len` characters of the resolved text"
would box the beginning of the line.

**The highlight covers `match_len` characters starting at the character at `cursor_byte_offset`.**
§8 must test the single-pass path explicitly, not only the tiled one.

`clean_input` also trims leading whitespace before matching, so index 0 of the matched run is the first
**non-whitespace** character at or after the cursor.

### D4 — per-character geometry must be carried

`match_len` counts characters; turning it into a rectangle needs each character's box.
`tile_forward` returns only a `String` and drops the boxes, and `resolve` likewise joins word texts.

Both paths must carry text *plus* an ordered list of `(char_count, PhysRect)`. **`char_count` per
entry, not one entry per character**: an OCR "word" is not always one character — the same line's page
counter arrived as `338` and `0.50%` in single words.

**When `match_len` ends inside a multi-character entry, round outward** — include that entry's whole
box. A box slightly wider than the match is honest; one that stops mid-entry would need geometry OCR
never supplied. This case must be tested.

`normalise` substitutes one character for another and never inserts or removes, so character indices
survive it. **Asserted by test, not assumed.**

### D5 — `match_len` must reach the drawing layer

`WorkerOutcome::Ready` carries `presentation, anchor, scan`; neither `Presentation` nor `Card` carries
`match_len`. The top card is built from `hits[0]`, so the value exists — the route does not. Add it to
`Card`, since it describes the card's own best hit.

### D6 — cosmetics

One rectangle, `FRAME_THICKNESS` as elsewhere, padded outward **3px** so it does not sit on the glyphs'
ink. Same constant alpha as the rest of the overlay.

Its own colour in `ui::theme`. **`scan_pass1` is already a blue**, so "a faint blue" needs deliberate
separation from it, not just a different hex: make the highlight the *brighter, more saturated* blue and
leave `scan_pass1` the dim grey-blue it is — the highlight is the everyday case and the capture boxes
are the debug case, so the everyday one should read first. `theme.rs`'s pairwise distinctness assertion
covers three colours and must be extended to four.

### D7 — a debug-only risk becomes a default-path risk

The overlay rides the popup's `CaptureGuard`, whose `ACK_TIMEOUT` proceeds anyway under load (overlay
spec §8). That was acceptable while the window existed only behind a debug flag. **With
`highlight_match` defaulting on, the overlay exists on every hover**, adding a second hide/restore per
capture permanently.

Accepted, and named here rather than discovered later. If hover latency or flicker regresses
measurably, `highlight_match = false` is the escape hatch, and the guard's cost is the first suspect.

---

## 5. Vertical text — measure first, then fix

**Runs before §3 and §4** — it needs nothing from them.

`probe` prints every quantity candidate **(a)** needs directly. Candidate **(b)** — a second
orientation-aware probe — cannot be measured in one invocation, because `--tiles` is ignored when
`--region` is given: `--region` is single-capture by definition. Measuring (b) means two runs at the
same coordinate with different `--region` shapes, comparing what each yields. That is a measurement
procedure, not a code change, and it is what this task does.

Two things are known from the code; neither is yet known to be *the* problem:

- `region_around` is orientation-blind: 500 × 100 always. For a vertical line, ~3 characters of the
  reading axis and five columns of irrelevant neighbours.
- `band_of` floors the perpendicular extent at `REGION_H` — flooring a **width** with a **height**
  constant — so vertical tiles are 100 × 500, the untested transpose of the only measured shape.

Orientation is not known until after pass 1, so pass 1's box cannot be orientation-aware. That
chicken-and-egg is real.

**This spec does not choose the fix.** The measurement task records, for real vertical text: forward
characters yielded today, whether the resolved character is correct, and what each candidate shape
yields — **(a)** a squarer pass-1 probe serving both orientations, or **(b)** a second orientation-aware
probe taken only when pass 1's line looks vertical.

**If the measurement shows vertical is not meaningfully broken, no code changes and that is the
finding.** If it shows a fix is needed, that fix gets its own design round rather than being
improvised here — this round delivers the measurement.

---

## 6. Identity — the bottle

**Runs first; shares no code with anything else.**

A chibi baby bottle: cream body, light blue-grey outline, two large eyes, blue cap with diagonal
stripes, orange nipple, tilted. The user proposed the design; the artwork is authored fresh as SVG in
this repository, in `assets/`, rather than copied.

- **Tray icon** replaces `LoadIconW(None, IDI_APPLICATION)`.
- **Executable icon** if reachable without a new build dependency; this project has taken none since
  the start. If it is not, the tray icon ships and the executable icon is recorded as deferred.

### D8 — icon handle ownership must be tracked, not assumed

`LoadIconW`'s handle is **shared and must never be destroyed**; `CreateIconFromResourceEx`'s is **owned
and must be**. `Tray` currently stores no `HICON` and destroys nothing, which is correct for the shared
handle it has. §7's fallback would put both kinds on the same field.

**Store `(HICON, owned: bool)`. `DestroyIcon` only when `owned`, and only after
`Shell_NotifyIconW(NIM_DELETE)`** — destroying an icon the shell is still displaying is the ordering
bug this avoids.

---

## 7. Error handling

| Failure | Response |
|---|---|
| Pass 1's head is empty (every word clipped) | **Fall back to single-pass**, not to tiling from the anchor — tiling from the anchor *is* failure mode 2. Near-unreachable by construction, since the hovered word sits ~250px from pass 1's edge |
| `match_len` exceeds the characters with known geometry | Highlight what geometry exists; never index past it |
| `match_len` ends mid-entry | Round outward to that entry's whole box (D4) |
| No top hit | No highlight; capture regions still drawn if enabled |
| Icon fails to load | Fall back to `IDI_APPLICATION` with `owned: false`; log once. The tray is the only way to quit, so it must appear |

## 8. Testing

Pure and screen-free:

- **head/tail split** — a head ending at pass 1's last unclipped word; tile 1 starting at that word's
  *trailing edge*; an empty head falling back to single-pass;
- **leading-edge filter** — a tile handed a word starting well before its start discards it. Prove it
  by deleting the filter and watching the test fail;
- **the leading filter's margin** — a word whose leading edge sits *just inside* `EDGE_MARGIN` of the
  tile's start must be **kept**, because that is a re-measurement of a glyph the previous tile
  deliberately deferred. Deleting the margin must make this test fail;
- **the capture kinds are not drawn** when only `highlight_match` is on;
- **char-index → box** with a **multi-character entry** in the run, and a match ending **inside** one;
- **the single-pass origin** — a non-zero `cursor_byte_offset` must box the hovered word, not the line's
  start. This is the shipped default and revision 1 got it wrong;
- **`normalise` preserves character count**, asserted;
- **padded union** across several boxes, and for a single-character match.

Live: §2's four acceptance items, and §5's measurement.

## 9. Non-goals

- Re-enabling tiling by default — decided by §2.1's result, in a later change.
- Highlighting more than the top match.
- Choosing the vertical fix (§5 measures; a later round fixes).
- Any change to ranking, deconjugation, or the dictionary.

## 10. Open risks

**§3 assumes pass 1's head is trustworthy beyond its centre.** Pass 1 was measured correct *at the
hovered character*; how far that holds toward its own edge is unmeasured, and `split_at_clipped`'s
margin is the only bound. If §2.1 fails, this is the first suspect.

**Pass 1's region is not clamped to the monitor**, so at a screen edge `split_at_clipped` judges words
against a nominal boundary the capture never reached. Tiles are clamped; pass 1 is not.

**§6's executable icon may be unreachable without a new dependency**, in which case it is deferred.

**Unverified:** whether Windows OCR groups multiple *Japanese* characters into one box — the
multi-character case is confirmed only for Latin runs, so D4's round-outward rule may be exercised
rarely or often.

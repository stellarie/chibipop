# chibipop M2 — OCR Text-Acquisition Tier

**Date:** 2026-07-27
**Status:** Built and merged. 83 tests green; `probe` and `watch` verified against live screen text.
**Parent spec:** `docs/superpowers/specs/2026-07-26-chibipop-design.md` (rev 3). Where the two
disagree, the parent governs — reconcile before coding.
**Depends on:** M0 (OCR availability, answered: `ja` is present) and M1 (the lookup core, built and
merged).

---

## 1. Scope

M2 delivers the tier that makes "read Japanese **anywhere on screen**" true: capture pixels around the
cursor, recognise the text, work out which character is under the pointer, and hand a `TextSpan` to the
M1 lookup engine.

It remains **headless**. No window, no popup, no hotkey, no input hooks — those are M3. Everything is
exercised from a terminal, which is deliberate: if OCR accuracy or hit-scan positioning is wrong, it
surfaces here, before any effort goes into rendering.

## 2. What was measured before designing

These are observations from running Windows' OCR engine directly on rendered Japanese, not
assumptions. They drive most of the decisions below.

| Observation | Evidence | Consequence |
|---|---|---|
| **Japanese is segmented one `Word` per character**, each with its own `BoundingRect` | `昨日は友達と…` returned 17 words for 17 characters | Cursor→character hit-scan is **exact**, not approximate. This is the single most important finding — it is what makes the whole tier tractable. |
| **`OcrLine.Text` inserts a space between every character** | Returned `昨 日 は 友 達 と 映 画 を 見 に 行 き ま し た 。` | Text **must** be assembled by concatenating `Word.Text`. Using `Line.Text` would feed spaces into the lookup engine and break every query. |
| **Latin runs stay whole** | `Rust` came back as one word with one box | Mixed Japanese/Latin text needs no special handling |
| **Small text degrades into plausible wrong characters** | At 12pt, `映画` was read as `映亘` | Upscaling before OCR is required, not optional. A wrong-but-valid character is the worst failure mode here: it silently looks up a different word. |
| **The long-vowel mark `ー` is misread as a hyphen** | `ツール` came back as `ツ - ル` | Requires normalisation, or every katakana word containing `ー` fails lookup |
| **Vertical text is structurally supported** | `TextAngle: 4.5`; characters stacked downward with correct per-character boxes | Vertical is worth supporting. Recognition was partial (9 of 16 characters) — an engine limitation we cannot fix, but partial text still yields a correct lookup for the hovered word. |

## 3. Decisions

| # | Decision | Rationale |
|---|---|---|
| M2-D1 | **BitBlt / GDI capture** | Simplest, no device setup, nothing beyond `windows-rs`. Captures windowed and borderless-fullscreen applications, which is what modern games and video players use. weikipop uses `mss` — the same class of mechanism — and works for this user's actual reading today. Fails only on true exclusive-fullscreen and protected content. |
| M2-D2 | **Fixed 900×300 physical-px box centred on the cursor, upscaled 2× before OCR** | Bounded, predictable cost. The lookup engine reads only *forward* from the cursor and truncates at 25 characters, so this comfortably covers what can ever be used. The upscale directly attacks the measured small-text accuracy loss. |
| M2-D3 | **Detect line orientation; support horizontal and vertical** | The detection is a few lines of geometry over boxes we already have, and it is far cheaper now than retrofitting once every downstream assumption is horizontal. |
| M2-D4 | **A pure layout layer with no Windows dependency** | The OCR engine's output is converted to plain `OcrWord` values immediately. Every subsequent decision — which character, which line, which direction, what string — is then unit-testable without a screen. This mirrors M1's `lookup/` rule and is the same reason. |
| M2-D5 | **`watch` mode polls the cursor; no input hooks** | The manual acceptance test is "hover and see the lookup happen". Polling `GetCursorPos` at ~8 Hz achieves that in a few lines and leaves M3's low-level hook work in M3, where the focus and lifetime concerns belong. |

### Rejected alternatives

- **Windows.Graphics.Capture.** Most reliable for DirectX content, but needs a capture session and a
  D3D device, and on older Windows builds draws a yellow border around the captured region — visually
  intrusive for a tool whose entire point is being invisible.
- **DXGI Desktop Duplication.** Built for streaming full-screen capture; we take one small region on
  demand. Would mean managing a D3D device and handling device-lost, for no benefit at this size.
- **Adaptive region growth.** Better accuracy-per-millisecond, but two captures on the slow path and
  more moving parts in the module that most needs to stay debuggable.
- **Hotkey-held trigger in `watch`.** Matches the eventual product, but pulls M3's keyboard hook
  forward and makes a debug tool capable of getting stuck on a held key.

## 4. Architecture

```
src/geom.rs           PhysPoint, PhysRect — types + arithmetic only          [pure]  NEW
src/text/
  mod.rs              trait TextSource, TextSpan                             [pure]  NEW
  layout.rs           OcrWord/OcrLine, orientation, hit-scan, assembly,
                      normalisation                                          [pure]  NEW
  capture.rs          BitBlt a region → RGBA buffer, 2× upscale             [windows] NEW
  ocr.rs              OcrTextSource: capture → Windows.Media.Ocr → layout   [windows] NEW
```

**Every file above is new.** M1 built only `src/lookup/`; neither `geom.rs` nor `src/text/` exists
yet, and `TextSource`/`TextSpan` appear in the parent spec as a contract but have never been written.
This milestone introduces the `windows` crate to the project for the first time — `Cargo.toml` gains
two dependencies: `windows` 0.62.2 and `windows-future` 0.3.2. The second is unavoidable — `windows`
does not re-export the `IAsyncOperation`/`AsyncStatus` types that its own generated APIs return, so
without it the async result type cannot be named in a signature.

**Hard rule, inherited from the parent spec and extended:** `geom.rs`, `text/mod.rs`, and
`text/layout.rs` must not depend on the `windows` crate. They must compile and test on any platform.
Only `capture.rs` and `ocr.rs` touch Windows APIs, and their job is to produce plain data for
`layout.rs` to reason about.

### 4.1 Types

```rust
// layout.rs — no Windows types anywhere
pub struct OcrWord { pub text: String, pub rect: PhysRect }
pub struct OcrLine { pub words: Vec<OcrWord> }

pub enum Orientation { Horizontal, Vertical }

/// The result of resolving a cursor position against recognised text.
pub struct Resolved { pub span: TextSpan, pub orientation: Orientation }

pub fn resolve(lines: &[OcrLine], cursor: PhysPoint) -> Option<Resolved>;
```

`TextSpan` is unchanged from the parent spec §4.1: `{ text: String, cursor_byte_offset: usize, anchor: PhysRect }`.

`OcrTextSource` implements the existing `TextSource` trait, so M4's tiered resolver can slot it in
behind UIA without changing anything downstream.

### 4.2 Data flow, one resolution

1. **Capture.** `capture.rs` BitBlts a 900×300 physical-pixel box centred on the cursor in
   virtual-desktop space, and upscales it 2×. The region is **not** clamped to the desktop bounds:
   a virtual desktop legitimately has negative coordinates when a monitor sits left of the primary,
   and BitBlt accepts out-of-bounds source coordinates, returning background for the uncovered part.
2. **Recognise.** `ocr.rs` runs `Windows.Media.Ocr` with the `ja` recogniser, then converts
   `OcrResult` into `Vec<OcrLine>`, **mapping every coordinate back out of upscaled-image space into
   virtual-desktop space**: `virtual = region_origin + (ocr_coord / UPSCALE)`. This conversion is the
   one place the two coordinate spaces meet, and it lives behind a pure function so it can be tested.
3. **Resolve.** `layout::resolve(lines, cursor)`:
   - **Hit-scan.** Find the word whose rect contains the cursor. Failing that, take the word with the
     smallest edge-distance from the cursor, and accept it only if that distance is at most **half of
     that candidate word's own height** — hovering the gap between two characters is normal and should
     not fail. Beyond that, `None`. Ties are broken by document order (line index, then position along
     the reading axis) so the result is deterministic.
   - **Orientation.** For the hit word's line, compute the spread of word-centre X values
     (`max − min`) and the spread of Y values. Greater spread in Y → `Vertical`. A single-word line is
     `Horizontal` by convention; the choice does not matter, because with one word the reading order
     is identical either way.
   - **Assembly.** Sort that line's words along the reading axis (ascending X for horizontal,
     ascending Y for vertical) and concatenate their `text`. **Never `OcrLine.Text`** — see §2.
   - **Normalise.** Replace `-`, `‐`, `–`, `—` with `ー` when the preceding character is kana. This
     recovers `ツ - ル` → `ツール`. The rule is deliberately conservative: it fires only after kana, so
     ordinary hyphenated Latin text is untouched.
   - **Emit.** `Resolved { span, orientation }`, where `span` is
     `TextSpan { text: the whole assembled line, cursor_byte_offset: the byte index at which the hit
     character begins, anchor: the hit character's rect }`. The orientation travels alongside rather
     than inside `TextSpan` so the parent spec's contract stays unchanged — M4's UIA tier has no
     orientation to report.
4. **Look up.** The caller passes `&span.text[span.cursor_byte_offset..]` to M1's `LookupEngine`,
   exactly as the parent spec §4.2 step 3 specifies.

### 4.3 Coordinate discipline

The parent spec's rule holds without exception: **every internal coordinate is a physical pixel in
virtual-desktop space.** The upscale factor exists only inside the capture→recognise boundary and is
divided out in step 2 before any coordinate reaches `layout.rs`. `layout.rs` never learns that
upscaling happened.

## 5. Command-line surface

Two subcommands, both headless. Together they are the manual verification harness the parent spec §8
requires for the parts that cannot be tested automatically.

**`chibipop probe --at X,Y`** — one resolution at an explicit point, printing each stage separately so
a wrong result can be attributed: the captured region's bounds, every recognised line with its
per-character boxes, the chosen character, the assembled span, and finally the lookup result.

**`chibipop watch`** — the human-usable acceptance test. Polls `GetCursorPos` at ~8 Hz and re-resolves
only when the cursor has moved more than **4 physical pixels** since the last resolution. It prints
only when the **dedup key** changes, where the key is `(span.text, span.cursor_byte_offset)` — i.e.
the hovered character within the recognised line. Two different pixels over the same character print
once; moving to the next character prints again. So: run it, hover over Japanese anywhere on screen,
and watch correct definitions appear in the terminal. Ctrl-C exits.

Both dedups are what make it readable — without them the terminal floods with the same word while the
cursor sits still.

## 6. Error handling

| Case | Response |
|---|---|
| OCR engine cannot be created for `ja` | **Hard fail** at construction, naming the language and pointing at the M0 findings note. M0 verified it is present on this machine, so this means the environment changed. |
| BitBlt fails | `Err` — a genuine failure. Logged; no result. |
| Captured region contains no recognisable text | `Ok(None)` — expected, not an error |
| Cursor is not within tolerance of any word | `Ok(None)` — expected |
| Word rect falls outside the captured region after coordinate mapping | Left as-is, **not** clamped. Such a box simply fails to hit-scan, which is the correct outcome. The binding requirement is that the coordinate arithmetic never panics — pure `i32` operations, tested against negative origins. |
| A resolution fails inside `watch` | Print the error and keep polling. One bad frame must not end the session. |

## 7. Testing

**Pure, runs anywhere, no Windows APIs:**

- Hit-scan: direct containment; near-miss within tolerance; beyond tolerance returning `None`; a cursor
  equidistant from two words resolving deterministically.
- Orientation detection on synthetic box sets for both horizontal and vertical lines, including the
  single-word line.
- Reading-order assembly for both orientations, including words supplied out of order.
- `ー` normalisation: fires after kana, does not fire in Latin text, handles all four dash variants.
- Coordinate round-tripping: a rect in upscaled-image space maps back to the expected virtual-desktop
  rect, including a non-zero region origin.
- `cursor_byte_offset` lands on a char boundary for multi-byte text — the failure this prevents is a
  panic on slicing.

**Integration, against the real engine:**

Feed a committed **raw BGRA fixture** to the actual OCR engine, and assert both the recognised text
and that a chosen pixel resolves to the expected character. Skips when the engine is unavailable —
the same approved pattern the M1 golden corpus uses, for the same reason.

A raw BGRA dump rather than a PNG, deliberately: it is byte-for-byte the format `capture.rs`
produces, so the test exercises the production path exactly, and it needs no image decoder — hence no
extra dependency. It is also immune to the CRLF-mangling risk a text-sniffable fixture would carry on
Windows (the file contains no `0x0A` or `0x0D` bytes at all).

**Honestly untestable, and reported as such:** BitBlt against real applications, and OCR accuracy on
real rendering rather than rendered fixtures. These get a written manual checklist executed with
`watch`, and results recorded rather than assumed.

## 8. Non-goals for M2

Popup, window, hotkey, input hooks, tray (M3) · UIA tier and the tiered resolver (M4) · DPI and
multi-monitor placement polish (M5) · Magpie · screen-change detection · memory measurement · any
capture mechanism beyond BitBlt.

## 9. Acceptance

> **Automated:** `cargo test` green including the fixture-image integration test, zero warnings.
>
> **Manual, run by the user:** start `chibipop watch`, hover over Japanese text in a real application,
> and see the correct dictionary entry for the hovered word appear in the terminal — in both
> horizontal and vertical text, and in at least one application that is not a browser.

## 10. Open questions

None blocking. Two known risks, both accepted with a stated fallback:

| Risk | If it bites |
|---|---|
| BitBlt returns black for a specific application (exclusive fullscreen, protected content) | Recorded as a limitation with the application named. Escalating to Windows.Graphics.Capture is a `capture.rs`-local change; nothing downstream of it would move. |
| Windows OCR's vertical recognition is too partial to be useful in practice | Orientation handling is already isolated in `layout.rs`. Falling back to horizontal-only is deleting one branch, not a redesign. |

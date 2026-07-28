# Scan-region overlay — seeing what the OCR actually looks at

**Status:** designed, not implemented
**Date:** 2026-07-28
**Parent:** `2026-07-26-chibipop-design.md`
**Related:** `2026-07-27-m3-popup-design.md` (the popup this borrows machinery from), `2026-07-27-two-pass-ocr-design.md` (the tiles this draws)

---

## 1. Problem

Text acquisition is invisible. A hover captures a 500×100 box, then reads forward in tiles whose
position and size come from the hovered word's measured geometry — and none of that is observable
except by reading `probe`'s numbers and picturing rectangles in your head.

Both defects found during two-pass acceptance would have been obvious on sight:

- A band sized at 2.0× the hovered word gave a **44px-tall tile**, in which Windows' OCR recognises
  nothing at all. Diagnosing it took a height sweep and two rounds of measurement. A drawn rectangle
  would have shown a hairline strip immediately.
- A band tall enough to recognise text was tall enough to contain **furigana**, which was being
  spliced into the sentence. A drawn rectangle would have shown it enclosing the ruby line.

Two open questions are blocked on the same missing instrument. **Vertical text is spotty** — and
reading the merged code shows why it might be: `region_around` is orientation-blind (always 500 wide
× 100 tall, so a vertical line gets ~100px along the axis that matters and 500px across columns it
does not care about), and `band_of` floors the perpendicular extent at `REGION_H` — flooring a
*width* with a *height* constant, producing 100×500 tiles that are the transpose of the only shape
ever measured. **Small text is untested** — at ~16px a fixed 500px tile holds about 31 characters,
past the ~20-character point where accuracy was measured to fall off.

Neither should be "fixed" by guessing. This spec builds the instrument that makes both measurable.

## 2. Goal and acceptance

A faint, colour-coded outline of every rectangle a hover actually captured, toggleable from the TOML,
and drivable from `probe` so a single coordinate can be inspected without running the whole app.

Acceptance:

1. With `show_scan_region = true`, hovering draws outlines for pass 1's box, each tile, and the
   resolved word's anchor, distinguishable from one another.
2. `probe --at X,Y --show-region` draws the same outlines and holds them long enough to look at.
3. With the toggle off, no overlay window is created and no region data is collected — the feature is
   inert, not merely hidden.
4. Lookups are unchanged with the overlay on. The instrument must not perturb what it measures.

## 3. Decisions

### D1 — Draw after the scan, never before

The overlay shows where chibipop *just* looked, not where it is *about* to look. This is what makes
the feature safe at all: a rectangle drawn before a capture would sit inside that capture, and a line
through text is exactly the kind of contamination that turns 通 into 過.

It also means the overlay's lifecycle matches the popup's exactly — both are products of a completed
resolve, and both can reuse the same show/hide handling.

### D2 — A shaped window, not a transparent one

M3-D7 measured `WDA_EXCLUDEFROMCAPTURE` as **incompatible with `UpdateLayeredWindow`** — the affinity
call fails with a misleading "not enough memory" HRESULT and silently no-ops. Per-pixel alpha is
therefore unavailable, and constant alpha (`SetLayeredWindowAttributes(LWA_ALPHA)`) cannot make a
window's interior transparent. An outline is precisely an interior-transparent shape.

Resolution: shape the window instead of painting it transparent. `SetWindowRgn` is set to the union
of **frame-shaped regions** — for each rectangle, its outer region minus its inner region, OR'd
together. The region clips; `WM_PAINT` fills each rectangle's full bounds with that rectangle's own
colour, and only the frame survives the clip. Coloured outlines, one uniform alpha, no per-pixel alpha
anywhere.

`ui/window.rs` already uses `SetWindowRgn` for the popup's rounded corners, so this is machinery the
project has already proven on this exact constraint.

### D3 — GDI `FillRect`, not Direct2D

A deliberate deviation from the popup's renderer. The overlay draws a handful of solid rectangles with
no text, no layout and no measurement; standing up an `ID2D1HwndRenderTarget`, its device-lost
recovery, and its factory lifetime for that is disproportionate. Recorded here rather than left as an
inconsistency for a reviewer to discover.

### D4 — Regions travel back from the worker, and are collected only when enabled

Pass 1's box could be recomputed on the main thread from the cursor, but **tiles cannot** — their
positions come from OCR output inside the worker. So the worker collects a `Vec<ScanRect>` and returns
it with the result.

`ScanRect { rect: PhysRect, kind: ScanKind }`, where `ScanKind` is `Pass1 | Tile | Anchor`. The kind
drives the colour; the drawing layer holds no knowledge of what a tile is.

Collection is gated at the source: with the toggle off the worker allocates nothing and the main
thread creates no window. "Off" means inert, not hidden.

### D5 — The overlay follows the popup's capture setting and its guard

D1 keeps the overlay out of the capture it describes, but hover *N*'s overlay is still on screen during
hover *N+1*'s capture. So it is treated exactly as the popup is: `SetWindowDisplayAffinity` follows
`[popup] exclude_from_capture`, and `CaptureGuard`'s existing hide/restore covers it — one additional
`ShowWindow` in a handler that already does this for the popup.

Deliberately **not** given its own capture setting. Two independent toggles governing the same
invariant is how one of them ends up wrong.

### D6 — `probe --show-region [SECONDS]`

`probe` is the measurement tool, and the whole point of this feature is making measurement visual.
Without this the overlay would only be reachable by running the full app and moving a real mouse,
which the screen constraint often forbids.

`probe` has no message loop today, so the flag brings a minimal one scoped to the display duration:
capture and resolve as now, then create the overlay, pump until the timeout elapses, then tear down.
Ordering is what keeps it safe — the overlay is created strictly after every capture that run will
make.

**Exact form**, so it is not left to interpretation: `--show-region [SECONDS]`, taking an optional
value (clap `num_args(0..=1)` with `default_missing_value = "3"`). Absent → no overlay;
`--show-region` → 3 seconds; `--show-region 10` → 10 seconds. `probe` does not read the TOML, so
`[debug] show_scan_region` has no effect here and the flag is the only control — the same split that
already applies to `--tiles` versus `[ocr] max_ocr_passes`.

### D7 — `[debug] show_scan_region`, defaulting off

A new `[debug]` section. **The field must carry `#[serde(default)]`.** `config.rs` treats malformed
TOML as a hard error naming the file rather than falling back to defaults, so a new *required* section
would stop every existing `chibipop.toml` from loading and chibipop would refuse to start after an
upgrade. This is the same trap `[ocr]` documented.

One boolean only. No colour, thickness, or alpha knobs — YAGNI, and every knob is a thing that can be
set to something useless.

### Rejected alternatives

- **`LWA_COLORKEY` chroma transparency.** Simpler than region shaping, but colour-keying is hard-edged
  and a faint outline is exactly where key bleed shows worst.
- **One window per rectangle.** Trivial to shape, but multiplies window lifetimes, capture-guard
  entries, and teardown paths by the number of tiles.
- **Drawing directly on the screen DC.** No window to manage, but it is erased by the next repaint of
  whatever is underneath and cannot be excluded from capture.

## 4. Architecture

```
worker (on resolve, only when enabled)
  collects ScanRect{Pass1}, ScanRect{Tile} x N, ScanRect{Anchor}
  → returned with WorkerOutcome
       ↓
main thread
  → overlay_layout(&[ScanRect]) -> (window bounds, Vec<(local rect, kind)>)   [pure]
  → SetWindowRgn( OR of frame regions )
  → ShowWindow; WM_PAINT fills each local rect in its kind's colour
```

| Unit | Home | Purity |
|---|---|---|
| `ScanRect`, `ScanKind` | `src/geom.rs` | pure |
| `overlay_layout(rects) -> (PhysRect, Vec<(PhysRect, ScanKind)>)` | `src/geom.rs` | pure |
| overlay window: create, region, paint, destroy | `src/ui/overlay.rs` (new) | Win32 |
| region collection | `src/text/ocr.rs`, `src/app.rs` | I/O |
| `--show-region` and its message pump | `src/main.rs` | Win32 |

`overlay_layout` returns the bounding box of all rectangles plus each rectangle translated into
window-local coordinates. That is the entire piece of reasoning worth testing, and it needs no screen.

### 4.1 Constants

| Constant | Value | Rationale |
|---|---|---|
| `FRAME_THICKNESS` | 2px | Visible at a glance without obscuring the text underneath |
| `OVERLAY_ALPHA` | 90 (of 255) | "Faint" — legible against both dark and light content, matching the popup's own translucency register |
| default `--show-region` seconds | 3 | Long enough to read, short enough not to block a sweep of probes |

Colours come from `ui::theme` so light and dark themes stay coherent: `Pass1` the dimmest, `Tile` the
accent, `Anchor` the brightest.

## 5. Error handling

The overlay is a debug aid. **It must never be able to break a lookup.**

| Failure | Response |
|---|---|
| Window creation fails | Log once, continue without an overlay; the hover still resolves |
| `SetWindowRgn` fails | Destroy the region, hide the window rather than showing an unshaped block |
| No rectangles collected | Hide the overlay; do not create an empty window |
| `probe --show-region` cannot create the window | Print the probe results as normal, report the failure on stderr, exit 0 |

## 6. Testing

- **Pure:** `overlay_layout` — bounds of a single rectangle; bounds of several disjoint rectangles;
  overlapping rectangles; local coordinates correct for a bounding box not at the origin; empty input
  yields no window.
- **Frame geometry:** an outer-minus-inner frame leaves the interior uncovered, in both a wide-flat
  and a tall-narrow rectangle — the vertical-text shape must not be an afterthought.
- **Config:** the toggle defaults off; a `chibipop.toml` written before `[debug]` existed still loads.
- **By eye, via `--show-region`:** outlines land on the text they describe, kinds are distinguishable,
  and the overlay disappears when the toggle is off.

## 7. Non-goals

- Drawing per-character OCR word boxes. The rectangles that matter are the captures; per-word boxes are
  already printed by `probe` and would be visual noise.
- Any change to tile geometry, band sizing, or `region_around`. This spec builds the instrument; the
  measurements it enables are a separate round.
- A GUI for the toggle. Configuration remains a hand-edited TOML.

## 8. Open risks

**The instrument could perturb what it measures.** With `exclude_from_capture = false` the overlay
relies on `CaptureGuard`, which has an `ACK_TIMEOUT` fallback that proceeds anyway under load. If that
fires, an outline sits inside the next capture. Low likelihood and it already applies to the popup, but
the overlay adds a second window to the same guard. Watch for it in first use rather than assuming it
away.

**Colour may not survive the alpha.** At alpha 90 over arbitrary screen content, three colours must
stay distinguishable. If they do not, thickness per kind is the fallback — it is unambiguous where
colour is not.

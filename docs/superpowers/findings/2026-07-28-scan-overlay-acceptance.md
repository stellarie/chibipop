# Scan-region overlay — acceptance observations

**Date:** 2026-07-28
**Spec:** `docs/superpowers/specs/2026-07-28-scan-overlay-design.md` §2
**Branch:** `feat/scan-overlay`

All four criteria **PASS**. Measured against real screen content, not a synthetic fixture.

## Test subject

Portrait secondary monitor (x ≥ 2560). A horizontal line of Japanese in a browser-based reader,
glyphs roughly 20px, with **furigana above some kanji** — which turned out to matter (see §1).

Probe used throughout: `probe --at 3100,600`, which reports
`region: x=2850 y=550 w=500 h=100` and resolves `ろ`.

## 1. Outlines appear and are distinguishable — PASS

Captured the region at full resolution with `CopyFromScreen` while
`probe --at 3100,600 --show-region` held the overlay up. Three visually distinct rectangles:

- **Blue-grey** — pass 1's box. Spans exactly the printed `region`, left edge at x=2850.
- **Orange** — a forward tile. Visibly **taller than pass 1 and offset downward**, which is
  `band_of`'s `REGION_H` floor made visible.
- **Amber** — the resolved word's anchor.

**The first thing the picture showed was a real defect's fingerprint.** The tile's band visibly
encloses the furigana `かわ いそう` printed above `可哀想`. That is exactly the bug that cost two rounds
of numeric measurement to find during two-pass acceptance — the taller band swallowing ruby text,
which `words_in` then spliced into the sentence. Seeing it took one screenshot.

Colour distinctness at `OVERLAY_ALPHA = 90` over real content was flagged as an open risk in spec §8.
**It held** — the three kinds are separable by eye against a dark reader background. Not tested
against a light background; the fallback recorded in §8 (per-kind thickness) remains available and was
not needed here.

## 2. The rectangles land where the numbers say — PASS

Enumerated the live window by class through `EnumWindows`/`GetWindowRect` rather than judging by eye:

```
Class                   X   Y   W   H Visible
ChibipopOverlayClass 2850 550 790 115 True
```

Origin `2850,550` matches the printed `region: x=2850 y=550` exactly. The `790x115` extent is the
union of pass 1 (500x100) with the forward tile and the anchor, which reach further right and slightly
lower — consistent with `overlay_layout`'s bounding box and with the drawn image.

## 3. Inert when the toggle is off — PASS

Started `chibipop run` with the shipped config and enumerated window classes:

```
chibipop window classes while running: ChibipopPopupClass, ChibipopTrayOwnerClass
```

No `ChibipopOverlayClass`. The window is not created at all, not merely hidden — the spec's
requirement was *inert*, and it is.

**Unplanned confirmation of the `[serde(default)]` work.** The live `chibipop.toml` used for this run
contains **no `[debug]` section whatsoever** — the grep for `show_scan_region` returned nothing — and
chibipop started normally. That is the upgrade path (a config written before the section existed)
verified against the user's real file rather than only in a unit test. The file's MD5 was identical
before and after the run.

## 4. Lookups are unchanged with the overlay on — PASS

The instrument must not perturb what it measures.

```
without --show-region: tiled: "ろうよ若いのに可尺だな卩65"  (14 chars)
with    --show-region: tiled: "ろうよ若いのに可尺だな卩65"  (14 chars)
```

Byte-identical. This is the criterion the Task 5 review's Important finding was about: the overlay
originally performed its own extra capture, which could have described different pixels than the
printed output. It no longer captures anything of its own.

## Observed, not a criterion

The OCR in these runs reads `可哀想` as `可尺` and trails with `卩65` — the `65` is page-counter chrome
(`338 / 67695 0.50%`) sitting on the same line inside the tile. Both are recognizer behaviour on this
content, unrelated to the overlay, and are exactly the kind of thing the overlay now makes diagnosable:
the drawn tile shows the counter falling inside the captured band.

## Not verified

- **Light theme.** Only the dark theme's three colours were checked against a dark background.
- **Vertical text.** The overlay draws whatever rectangles it is given; whether the *rectangles* are
  right for vertical text is the open question this instrument was built to answer, and is a separate
  round of work.

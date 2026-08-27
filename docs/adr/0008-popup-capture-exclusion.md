# Popup capture exclusion by core-side masking

On Windows, chibipop keeps its popup out of its own OCR input with
`WDA_EXCLUDEFROMCAPTURE` (opt-in) or a hide/reshow **capture guard** around every grab
(default). Neither ports to Wayland: no portable protocol-level surface exclusion
exists, and a guard costs a presentation-fenced round-trip per grab — a visible popup
blink at hover cadence, and unresolvable frame correlation on the portal/PipeWire
backend. Compositor rules (Hyprland `no_screen_share`, niri block-out) render a black
rectangle for **all** screencopy clients — the same information loss as a mask, but
compositor-specific and also blacking the popup out of the user's own recordings.

**Decision: the Worker masks in core.** Before OCR, the captured image's overlap with
chibipop's own on-screen rects (the popup panel, known from `Event::PopupPlaced`) is
filled with a flat color. Both rects are core state in physical pixels, so exclusion is
pure arithmetic — no protocol dependency, no flicker, identical on both capture
backends, unit-testable. The Windows bin supplies no mask rects; its guard/WDA path is
unchanged.

Safeguards:

- **Mask boundary is a capture edge.** The existing clipped-word exclusion
  (`layout.rs` head/tail logic) applies at mask edges — words intersecting the mask are
  dropped, never half-recognized.
- **Measured, not assumed.** The OCR candidate benchmark gained masked-fixture variants
  (popup-sized rects at realistic positions); mask color and edge handling were tuned
  from measured accuracy deltas. *Resolved 2026-08-24 (ADR-0009): white fill, hard
  edge — fill color is irrelevant to the chosen engine, white is safe across all
  benchmarked engines, and the 1 px feather bought nothing measurable.*
- **Named fallback.** If masking measurably poisons detection, plan-B is the
  Windows-style hide-during-grab guard, blink cost and all. *Resolved: masking passed
  the benchmark (+11.6 pp interior after drop-filtering); the guard is not built.*

Accepted divergences from Windows:

- **Text under the shown popup is occluded to OCR.** Windows' guard reads through the
  popup; on Linux the pixels are unrecoverable by any non-guard mechanism. Hovering
  inside the popup is already sticky (no lookup) and `place_popup` never covers the
  anchor, so only peripheral context is lost. Matches what the user's own eyes see.
- **`exclude_from_capture` has no Linux checkbox.** The app cannot hide its surface
  from third-party capture (Hyprland's rule is config-file-only; KWin's
  Hide-from-ScreenCast is user-driven; GNOME has nothing). The settings window shows a
  channel-aware copyable snippet instead — `layerrule = no_screen_share, chibipop` on
  Hyprland, a pointer to Hide-from-ScreenCast on KDE, "not available" elsewhere — the
  same shape as the hotkey-snippet precedent (ADR-0005).

The debug scan-region overlay needs no mask: its outline strokes are drawn **outset**,
outside the capture rect, so they never intersect a grab on any backend.

Trigger mode may sidestep masking entirely — capture at trigger press predates the
popup, and a frozen OCR buffer never self-contaminates; that refresh policy belongs to
the live-hover cadence decision.

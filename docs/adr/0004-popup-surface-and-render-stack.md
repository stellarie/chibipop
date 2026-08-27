# Popup surface and rendering stack on Wayland

Amends
ADR-0001's "exactly two traits" and its rejection of a shared scene. Research basis:
`docs/research/rust-wayland-ui.md`.

## The stack

**SCTK + tiny-skia + cosmic-text, hand-rolled.** smithay-client-toolkit 0.21
(`shell::wlr_layer`) drives the surface, `wl_shm` `SlotPool` holds the buffers, tiny-skia
0.12 rasters the chrome, cosmic-text 0.19 shapes and rasterizes text. Nothing winit-based
can create a layer surface (winit#2582 open since 2022, PR #4044 unmerged, maintainer
wants layer-shell to live outside winit proper), so eframe/egui and stock iced are
disqualified from the popup by construction. Between the two remaining routes we chose
control and a lean resident daemon over `iced_layershell`'s convenience: no GPU context in
a tray-resident process (wgpu costs ~100 MB idle USS), first-party Smithay dependencies
only, and every sharp edge — input region, the Hyprland initial-scale race, `closed`
recreation — is our code rather than an upstream issue away. Cost accepted: ~1–2k lines of
Wayland plumbing (SCTK's `simple_layer.rs` is the template) plus ~100 lines of manual
`wp_fractional_scale_v1` + `wp_viewporter` binding, since SCTK ships no fractional-scale
helper. `iced_layershell` stays the documented fallback if the plumbing overruns. This
choice leaves the settings-window toolkit free (its own ticket).

## The surface

Layer `overlay` (the popup must sit above fullscreen readers and games), namespace
`chibipop`, `exclusive_zone = -1` (may overlap bars, never reserves space),
`keyboard_interactivity = none` (focus-stealing is impossible by construction), anchor
`top|left` with `set_margin` for placement — margin, anchor, size, layer and input region
are all double-buffered, so a reposition rides the frame commit.

**One surface per output, all mapped once.** A layer surface's output is fixed at creation
and margins are relative to it, so `output = NULL` would hand us an origin we did not
choose while the cursor roams. Instead: enumerate outputs via SCTK `OutputState`, create a
hidden layer surface per output at startup, and show on whichever output holds the cursor.
Buffers are allocated lazily, so only the active output holds one; scale is tracked per
output, which mixed-DPI setups need anyway. Output hotplug is the per-surface `closed`
event, handled as routine recreation rather than an error.

**Hiding attaches a fully transparent buffer and clears the input region**; the surface is
never unmapped during normal operation. Hyprland animates layer surfaces by default, so
map/unmap per lookup would visibly fly the popup in on every hover — a regression against
Windows, where show/hide is instant. This makes correct behavior the default instead of
depending on users adding a `layerrule no_anim`. `set_layer` needs no recreation, so an
`overlay`↔`top` toggle is a runtime setting.

**Content-sized, resized when content changes.** Resize costs a configure round-trip
(`set_size` + commit → `configure` → attach matching buffer + `ack_configure` + commit) but
happens once per new entry, never per cursor move; the intermediate commit attaches no
buffer, so there is no visible jump. Rejected: a permanent maximum-size surface (multi-MB
buffer plus compositor blending of a mostly-transparent surface for as long as the popup is
up).

**Every commit is gated on a `wl_surface.frame` callback**, coalescing to one pending state
(margins, buffer, input region). Work is bounded by refresh rate rather than cursor-event
rate, at the cost of up to one frame of positional latency. Show/hide must not be
frame-gated — hidden and occluded surfaces receive no frame callbacks. Polling rates and
idle behavior belong to the cadence/power ticket, not here.

**Input region is the whole panel while the popup is shown.** Windows keeps its popup
`WS_EX_TRANSPARENT` and synthesizes every interaction from a machine-wide `WH_MOUSE_LL`
hook; Wayland has no equivalent, and ADR-0003 already moved wheel and click onto the
popup's own input region. Hover-alive does *not* depend on it — `geom::sticky_region` runs
off global cursor position from the cursor channel — but scroll-anywhere-over-the-popup
parity does. Accepted cost: while the pointer is over the panel, pointer focus leaves the
app underneath, so its hover states drop and text beneath the popup cannot be looked up
(the popup covers that text regardless). Cursor shape over interactive rects uses
`wp_cursor_shape_v1` where advertised; where it is not, the cursor is left unchanged rather
than loading XCursor themes ourselves.

**Panel alpha is per-pixel premultiplied**, opacity fixed at Windows' 230/255 as a `Theme`
constant with antialiased rounded corners — better than Win32's hard-clipped
`CreateRoundRectRgn` silhouette, and identical in appearance. Windows only used constant
`LWA_ALPHA` because per-pixel alpha breaks `WDA_EXCLUDEFROMCAPTURE`; that coupling does not
exist here. Opacity stays non-configurable, like colors and font sizes.

## Layout moves into core behind `TextMeasure`

`present.rs` stays a pure unmeasured view model, but the 1359-line layout engine currently
welded to DirectWrite inside `src/ui/render.rs` moves into core as a `layout` module
producing a **`PopupScene`**: element construction, wrapping, line stacking, the side
panel, `max_scroll`/scrollbar geometry, match highlight, and hit rects — in one uniform
pixel space whose unit is whatever the platform hands in (physical pixels on Linux, DIPs
on Windows; core never sees a scale factor), scroll offset applied and off-panel runs
culled by core (matching today's renderer, which uses no D2D clipping). Bins only paint
runs and fill rects.

This amends ADR-0001: two measurers (DirectWrite, cosmic-text) make this a real seam rather
than the hypothetical one that was rejected, and `PopupScene` is a measured scene of
positioned runs, not the draw-command IR that was rejected.

**`TextMeasure` is measure-only**: wrap a string at a max width, return line metrics.
Nothing else. The scene carries positioned runs as data, so it stays plain and inspectable
and the state machine is testable with a fake measurer returning fixed glyph metrics. Bins
re-shape at paint time — exactly what `render.rs` does today (two `layout_pass()` walks,
one `IDWriteTextLayout` per line per walk) — and both backends absorb it in their own
caches (`FormatCache`; cosmic-text's `FontSystem`). Rejected: an opaque shaped-layout
handle in the scene (infects core with generics) and a metrics-plus-cache-key scheme (an
eviction policy in both bins, and stale keys paint text that does not match the geometry).

**Both bins convert in the same change**, guarded by golden measurement tests: capture
measured scenes for fixture `Presentation`s from the pre-refactor build, assert identical
geometry after. A Linux-first conversion would open a duplication window that quietly
becomes permanent; a Windows-first one would design the interface with only DirectWrite in
hand.

**The bin measures and places; the Controller learns the result as an `Event`.** Placement
needs the measured size (`geom::place_popup` edge-flips a known-size popup;
`sticky_region` needs the final rect), and measurers are bin-owned. So
`Command::ShowPopup { presentation, anchor }` goes out, the bin measures, calls core
`layout` + `place_popup`, shows the surface, and feeds `Event::PopupPlaced { rect }` back.
The Controller keeps zero bin-supplied dependencies and stays testable on plain data; the
shown-but-rect-unknown interval becomes a modelled Controller state rather than an
accident. Rejected: passing `&mut dyn TextMeasure` into `Controller::handle` (every state
machine test then needs a fake measurer), and measuring in the `Worker` (turns a scroll or
scale-change repaint into a pipeline job).

## Fractional scale and fonts

**Physical pixels stay authoritative** (ADR-0001), logical geometry is derived: the bin
computes physical `max_w`/`max_h` from the output's physical size × the monitor-percent
settings, lays out and rasters in physical pixels, then derives logical size as
`ceil(physical / scale)` and margins as `round(physical / scale)`, with
`wp_viewport.set_destination` = the logical size and `buffer_scale` left at 1. Sub-pixel
slack lands in trailing panel padding where it is invisible. `Theme`'s font sizes stay
logical and are multiplied by scale at measure time. **Scale is never latched** — Hyprland
may send `preferred_scale = 1.0` first and correct it later (won't-fix upstream: apps must
handle scale changes at any time), so every `preferred_scale` re-renders and re-commits.

**The `FontSystem` is constructed with locale `ja`.** cosmic-text resolves `Script::Han`
through `han_unification(locale)`, and only `"ja"` yields Noto Sans CJK JP — the default
arm, including `en-US`, yields Noto Sans CJK **SC**, i.e. Chinese glyph variants for kanji.
This is a silent wrong-output failure mode for the product's entire purpose. The Linux
`Theme` default family is `Noto Sans CJK JP` (Windows keeps `Yu Gothic UI`, so the theme
default becomes per-platform), and fontdb is probed at startup for any JP-capable family;
if none exists the app still runs, with a visible tray/settings warning naming the package
to install — consistent with the degrade-visibly posture used for tray and capture.

## The Anki affordance stays bin-owned

Core layout reserves its slot in the panel and reports its `PhysRect` as a hit target;
realisation is per-bin. Windows keeps its separate `AnkiButton` HWND — which exists only
because a `WS_EX_TRANSPARENT` popup cannot receive a click — positioned against that rect;
Linux paints it inside the panel and serves the click from the panel's own input region.
Rejected: deleting `AnkiButton` (a behavior change on the regression-forbidden platform for
a Linux-driven reason) and a second layer surface on Linux (reproducing a Win32 workaround
Wayland does not need).

//! The outline overlay draws frame-only rectangles on click-through
//! `zwlr_layer_shell_v1` surfaces.
//!
//! Linux uses this module for two roles of layer surfaces that match Windows roles.
//! It draws the scan-rect outline in Windows `ui/overlay.rs` and the
//! static-region border in Windows `ui/static_overlay.rs`. Both roles draw N
//! border strips with transparent centers above all other surfaces.
//! Neither surface accepts input or focus.
//! This module accepts `&[Mark]` and supports any number of rects.
//! Each [`Mark`] carries a color because the four [`ScanKind`] values
//! represent the pass-1 capture, forward tile, resolved word, and defined
//! characters. Windows assigns each kind a theme color. The static region
//! supplies one rectangle with one color. This module does not identify
//! its caller.
//!
//! **Nothing here can be interacted with.** Each commit sets an empty
//! input region. A pointer then reaches the surface below.
//! `keyboard_interactivity` is `None`, which keeps the popup rule for this surface too.
//! Focus on an outline would break every use of this module.
//!
//! **Sized to the rects, not to the screen.**
//! A full-output surface would require a 33 MB buffer per output at 4K.
//! It would also repaint on every hover for a scan overlay.
//! This module sizes each output surface to the
//! box around its rects and places it with offsets. The buffer
//! then matches only the outlined area. A rect across two monitors gets
//! one surface per monitor, and each surface draws its clipped half.
//!
//! The raster path matches the popup path. It reuses the
//! `wl_compositor` and `wl_shm` already bound by this process and borrowed
//! from [`Popup`]. It uses an `wl_shm` `SlotPool` and `Argb8888` buffers
//! with premultiplied `[B, G, R, A]` words. The popup's `wp_viewporter`
//! carries logical size while `buffer_scale` remains one.
//! This module does not bind `wp_fractional_scale_v1`. The popup has one
//! surface on each output, and its `preferred_scale` is the only scale
//! source. [`Screen`] provides that value on every show, so no scale is
//! fixed between shows. Physical pixels remain authoritative
//! ([`crate::popup::derive`] performs the only shared derivation).

use crate::daemon::App;
use crate::popup::{derive, Popup, Screen};
use chibipop::geom::{PhysRect, ScanKind, ScanRect};
use chibipop::ui::theme::Theme;
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::QueueHandle;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

/// `hyprctl layers` and `layerrule` inspect this namespace.
const NAMESPACE: &str = "chibipop-outline";

/// One premultiplied `Argb8888` pixel as `wl_shm` stores it.
/// The little-endian `0xAARRGGBB` word has B, G, R, A in memory.
/// This matches the order that the popup's `to_argb` function uses.
pub type Px = [u8; 4];

/// A fully transparent pixel. The frame center shows the screen through it.
pub const CLEAR: Px = [0, 0, 0, 0];

/// Physical outline thickness in pixels. The Windows bin uses `BORDER_PX`,
/// so both platforms use the same line width.
pub const BORDER_PX: i32 = 2;

/// Opaque outline color. The Windows bin uses teal `BORDER_COLOR`
/// with R 0xC0, G 0xE0, and B 0xE0.
pub const BORDER: Px = [0xE0, 0xE0, 0xC0, 0xFF];

/// One rectangle and the color that identifies its purpose.
///
/// A rectangle alone cannot describe the scan overlay. Core passes four
/// box kinds at once ([`ScanKind`]). Each kind has a separate purpose:
/// pass-1 capture, forward tile, resolved word, and defined characters.
/// Windows gives each kind its own theme color (`ui/overlay.rs`).
/// One border for all kinds would lose this distinction.
/// The static region passes one rectangle with one color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub rect: PhysRect,
    pub colour: Px,
}

/// Convert `(r, g, b)` to an opaque premultiplied `Argb8888` word.
/// At full alpha, premultiplication changes no channel.
/// This function only swaps channels as [`Px`] documents.
pub fn opaque((r, g, b): (u8, u8, u8)) -> Px {
    [b, g, r, 0xFF]
}

/// Get one scan rect color from the resolved theme.
///
/// The Windows overlay uses the same four theme fields. A user who sets
/// `.scan-match` then sees the same box color on both platforms.
pub fn scan_colour(kind: ScanKind, theme: &Theme) -> Px {
    match kind {
        ScanKind::Pass1 => opaque(theme.scan_pass1),
        ScanKind::Tile => opaque(theme.scan_tile),
        ScanKind::Anchor => opaque(theme.scan_anchor),
        ScanKind::Match => opaque(theme.scan_match),
    }
}

/// Convert core scan rects to marks. Place each mark outside the rect by
/// the border thickness and assign its color by kind.
///
/// **Outset, never inset**.
/// This follows the Windows overlay rule and the capture rule in
/// `ARCHITECTURE.md#capture-and-masking`.
/// A stroke inside a scan rect would touch pixels that the next grab reads.
/// chibipop would then OCR its own furniture. The border thickness puts the whole
/// frame in the band around the rect and leaves the rect itself untouched.
pub fn scan_marks(rects: &[ScanRect], theme: &Theme) -> Vec<Mark> {
    rects
        .iter()
        .map(|r| Mark {
            rect: r.rect.inflated(BORDER_PX, BORDER_PX),
            colour: scan_colour(r.kind, theme),
        })
        .collect()
}

/// Return four border strips for one rect in surface-local physical pixels.
///
/// Keep the Windows bin's `edge_strips` rule for degenerate rects.
/// Use `<=` instead of `<`. A rect exactly two borders tall has no middle.
/// Draw it solid instead of two adjacent bands and two zero-height slivers.
/// Windows `FillRect` ignores an empty rect. The returned strips cannot contain
/// empty rectangles.
pub fn strips(rect: PhysRect, t: i32) -> Vec<PhysRect> {
    let t = t.max(1);
    if rect.w <= 0 || rect.h <= 0 {
        return Vec::new();
    }
    if rect.w <= 2 * t || rect.h <= 2 * t {
        return vec![rect];
    }
    vec![
        PhysRect { x: rect.x, y: rect.y, w: rect.w, h: t },
        PhysRect { x: rect.x, y: rect.y + rect.h - t, w: rect.w, h: t },
        PhysRect { x: rect.x, y: rect.y + t, w: t, h: rect.h - 2 * t },
        PhysRect { x: rect.x + rect.w - t, y: rect.y + t, w: t, h: rect.h - 2 * t },
    ]
}

/// Return the smallest box that contains all marks, or `None` when no mark exists.
///
/// `bounds` needs at least one mark with a non-empty rectangle.
/// It ignores marks with zero or negative width or height.
/// This box sets the surface size. Three scan rects use one surface whose
/// width covers all three. They do not use three surfaces or the whole screen.
/// Color does not affect geometry. Each mark contributes only its rect.
pub fn bounds(marks: &[Mark]) -> Option<PhysRect> {
    let mut it = marks.iter().map(|m| m.rect).filter(|r| r.w > 0 && r.h > 0);
    let first = it.next()?;
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
    for r in it {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.x + r.w);
        y1 = y1.max(r.y + r.h);
    }
    Some(PhysRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// Return marks that touch this output, clipped to its bounds.
/// Keep each mark's color after the clip. A match rect split at a monitor
/// boundary remains match color on the second monitor.
pub fn on_screen(marks: &[Mark], monitor: PhysRect) -> Vec<Mark> {
    marks
        .iter()
        .filter_map(|m| Some(Mark { rect: clip(m.rect, monitor)?, colour: m.colour }))
        .collect()
}

/// Return `rect` ∩ `bounds`, or `None` when they do not overlap.
pub fn clip(rect: PhysRect, bounds: PhysRect) -> Option<PhysRect> {
    let x0 = rect.x.max(bounds.x);
    let y0 = rect.y.max(bounds.y);
    let x1 = (rect.x + rect.w).min(bounds.x + bounds.w);
    let y1 = (rect.y + rect.h).min(bounds.y + bounds.h);
    (x1 > x0 && y1 > y0).then_some(PhysRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// Fill one axis-aligned box in a premultiplied `Argb8888` buffer.
///
/// Use row-wise slice fills instead of a tiny-skia pass. Both overlays draw
/// axis-aligned opaque boxes, so no blend or antialias operation is needed.
/// This keeps geometry as a pure function that tests can inspect pixel by pixel.
pub fn fill(px: &mut [Px], w: i32, h: i32, box_: PhysRect, colour: Px) {
    let Some(box_) = clip(box_, PhysRect { x: 0, y: 0, w, h }) else { return };
    for row in box_.y..box_.y + box_.h {
        let base = (row as usize) * (w as usize);
        let from = base + box_.x as usize;
        let to = from + box_.w as usize;
        px[from..to].fill(colour);
    }
}

/// Paint one surface frame. Clear all pixels, then draw each mark's border
/// strips in its color.
///
/// `marks` contains surface-local physical pixels. The caller subtracts the origin
/// of the box around all rects. Paint marks in the supplied order. Core appends the
/// `Match` box last in `worker.rs`, so it covers a capture box below it as on Windows.
pub fn paint(px: &mut [Px], w: i32, h: i32, marks: &[Mark]) {
    px.fill(CLEAR);
    for mark in marks {
        for strip in strips(mark.rect, BORDER_PX) {
            fill(px, w, h, strip, mark.colour);
        }
    }
}

/// A frame that awaits the configure message that sets its size.
struct Pending {
    /// Device-pixel dimensions for raster output.
    buffer: (i32, i32),
    /// The logical dimensions for `set_size` and the viewport destination.
    logical: (i32, i32),
    /// Surface-local marks to paint.
    marks: Vec<Mark>,
}

/// The outline surface for one output.
struct Pane {
    /// Stable popup surface ID for this output. Both surfaces then use
    /// the same monitor number.
    id: usize,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    /// Logical size from the most recent compositor configure.
    configured: Option<(i32, i32)>,
    /// Frame data for the next configure message. Do not coalesce it with
    /// a frame callback. Outline changes occur at hover cadence at most and
    /// remain hidden otherwise. No cursor-event rate needs a bound.
    pending: Option<Pending>,
}

/// State for the outline overlay.
pub struct Outline {
    qh: QueueHandle<App>,
    /// Clone the popup's `wl_compositor` handle.
    /// This global creates surfaces and has no state. One handle per process
    /// reduces the number of proxies that need review.
    compositor: CompositorState,
    shell: LayerShell,
    /// Clone the popup's `wp_viewporter`. The module docs explain why this
    /// state has no fractional-scale object.
    viewporter: Option<WpViewporter>,
    pool: SlotPool,
    panes: Vec<Pane>,
    /// Global physical marks that are on screen.
    /// A diagnostic can report them. A show at a new scale can derive them again.
    shown: Vec<Mark>,
    notes: Vec<String>,
}

impl Outline {
    /// Bind globals for the outline. Create surfaces at the first
    /// [`Outline::show`] because no size exists before that.
    ///
    /// `None` means that the compositor lacks `zwlr_layer_shell_v1`.
    /// Apply the same *state, not error* rule as `Popup::bind`.
    /// The caller reports an unavailable outline through its current channel.
    /// Other channels continue.
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>, popup: &Popup) -> Option<Outline> {
        let shell = LayerShell::bind(globals, qh).ok()?;
        // Start with space for one modest outline. The pool grows as needed.
        let pool = SlotPool::new(256 * 256 * 4, popup.shm()).ok()?;
        Some(Outline {
            qh: qh.clone(),
            compositor: popup.compositor().clone(),
            shell,
            viewporter: popup.viewporter().cloned(),
            pool,
            panes: Vec::new(),
            shown: Vec::new(),
            notes: Vec::new(),
        })
    }

    /// Diagnostics since the last drain. The outline has no log.
    /// The daemon thread owns the log.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Return the global physical marks that the outline shows now.
    pub fn marks(&self) -> &[Mark] {
        &self.shown
    }

    pub fn surface_count(&self) -> usize {
        self.panes.len()
    }

    /// Test whether a layer surface belongs to the outline.
    /// Shared SCTK handlers use this check.
    /// The outline requests no frame callbacks, so no `wl_surface` twin exists.
    /// A `wl_surface` event therefore cannot name an outline surface.
    pub fn owns_layer(&self, layer: &LayerSurface) -> bool {
        self.panes.iter().any(|p| &p.layer == layer)
    }

    /// Show these marks. An empty slice calls [`Outline::hide`].
    pub fn show(&mut self, marks: &[Mark], screens: &[Screen]) {
        self.shown = marks.to_vec();
        if marks.is_empty() {
            self.hide();
            return;
        }
        let mut drawn = 0usize;
        for screen in screens {
            let mine = on_screen(marks, screen.rect);
            let Some(box_) = bounds(&mine) else {
                // No mark touches this output. Clear any surface that showed one before.
                self.clear(screen.id);
                continue;
            };
            drawn += mine.len();
            let placement = derive(box_, screen.rect, screen.scale);
            // Use surface-local coordinates. Draw strips from the box origin,
            // not the global origin.
            let local: Vec<Mark> = mine
                .iter()
                .map(|m| Mark {
                    rect: PhysRect { x: m.rect.x - box_.x, y: m.rect.y - box_.y, ..m.rect },
                    colour: m.colour,
                })
                .collect();
            let pending = Pending {
                buffer: placement.buffer,
                logical: placement.logical,
                marks: local,
            };
            self.commit(screen, placement.margin, pending);
        }
        self.notes.push(format!(
            "outline: {} rect(s) on {} surface(s), {} drawn after clipping",
            marks.len(),
            self.panes.len(),
            drawn,
        ));
    }

    /// Hide the outline with a transparent buffer on every surface.
    /// Never unmap it. Hyprland animates layer surfaces after an unmap.
    /// This motion is worse than no outline.
    pub fn hide(&mut self) {
        self.shown.clear();
        let ids: Vec<usize> = self.panes.iter().map(|p| p.id).collect();
        for id in ids {
            self.clear(id);
        }
    }

    /// Remove a surface after its output disappears or the compositor closes it.
    /// This is routine. The next show maps a new surface.
    pub fn drop_layer(&mut self, layer: &LayerSurface) {
        let Some(slot) = self.panes.iter().position(|p| &p.layer == layer) else { return };
        let id = self.panes[slot].id;
        self.panes.remove(slot);
        self.notes.push(format!("outline: surface {id} closed; {} left", self.panes.len()));
    }

    /// Paint the requested frame after a configure message arrives.
    pub fn configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(idx) = self.panes.iter().position(|p| &p.layer == layer) else { return };
        self.panes[idx].configured = Some((size.0 as i32, size.1 as i32));
        if let Some(pending) = self.panes[idx].pending.take() {
            self.draw(idx, pending);
        }
    }

    /// Map this output surface when needed, then size and paint it.
    /// A resize needs one configure round trip. The intermediate commit has
    /// no buffer, so the surface does not flicker.
    fn commit(&mut self, screen: &Screen, margin: (i32, i32), pending: Pending) {
        let idx = match self.panes.iter().position(|p| p.id == screen.id) {
            Some(idx) => idx,
            None => self.map(screen),
        };
        let pane = &mut self.panes[idx];
        pane.layer.set_margin(margin.0, 0, 0, margin.1);
        if pane.configured == Some(pending.logical) {
            self.draw(idx, pending);
            return;
        }
        pane.layer.set_size(pending.logical.0.max(1) as u32, pending.logical.1.max(1) as u32);
        pane.layer.commit();
        pane.pending = Some(pending);
    }

    /// Create one output surface. The first commit has no buffer.
    /// The compositor then sends configure, and this module paints the frame after it.
    fn map(&mut self, screen: &Screen) -> usize {
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
        let layer = self.shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some(NAMESPACE),
            Some(&screen.output),
        );
        // Place it above fullscreen readers such as the popup.
        // Reserve no space and accept no focus. An outline only marks the screen.
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_margin(0, 0, 0, 0);
        layer.set_size(1, 1);
        layer.commit();
        self.notes.push(format!(
            "outline: layer surface {} on {} (overlay layer, keyboard none, click-through)",
            screen.id, screen.name
        ));
        self.panes.push(Pane {
            id: screen.id,
            layer,
            viewport,
            configured: None,
            pending: None,
        });
        self.panes.len() - 1
    }

    /// Raster and commit one frame.
    fn draw(&mut self, idx: usize, pending: Pending) {
        let (bw, bh) = pending.buffer;
        let surface = self.panes[idx].layer.wl_surface().clone();
        let buffer = match self.pool.create_buffer(bw, bh, bw * 4, Format::Argb8888) {
            Ok((buffer, canvas)) => {
                let (px, _) = canvas.as_chunks_mut::<4>();
                paint(px, bw, bh, &pending.marks);
                buffer
            }
            Err(e) => {
                self.notes.push(format!("outline: no buffer to draw with: {e}"));
                return;
            }
        };
        let pane = &mut self.panes[idx];
        if let Some(viewport) = &pane.viewport {
            viewport.set_destination(pending.logical.0.max(1), pending.logical.1.max(1));
        }
        // Set an empty input region on every commit. An outline is never a hit target.
        // The pointer then reaches the surface below.
        if let Ok(region) = Region::new(&self.compositor) {
            surface.set_input_region(Some(region.wl_region()));
        }
        surface.damage_buffer(0, 0, bw, bh);
        if let Err(e) = buffer.attach_to(&surface) {
            self.notes.push(format!("outline: attaching the buffer failed: {e}"));
            return;
        }
        pane.layer.commit();
    }

    /// Give one surface a fully transparent buffer at its current size.
    /// Do not wait for a frame callback. A hidden surface receives no callback,
    /// so that wait could never hide it.
    ///
    /// Give an unconfigured surface no buffer.
    /// A buffer before the first configure violates protocol
    /// (`layer_surface has never been configured`) and kills the connection.
    /// This case occurs if show and hide happen before configure returns.
    /// Discard the stored frame to hide the surface. A layer surface without
    /// an attached buffer is not mapped.
    fn clear(&mut self, id: usize) {
        let Some(idx) = self.panes.iter().position(|p| p.id == id) else { return };
        self.panes[idx].pending = None;
        let Some(logical) = self.panes[idx].configured else { return };
        let buffer = (logical.0.max(1), logical.1.max(1));
        self.draw(idx, Pending { buffer, logical, marks: Vec::new() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PhysRect {
        PhysRect { x, y, w, h }
    }

    /// Use the default color for tests about geometry, not the four kinds.
    fn mark(x: i32, y: i32, w: i32, h: i32) -> Mark {
        Mark { rect: rect(x, y, w, h), colour: BORDER }
    }

    #[test]
    fn one_rect_becomes_four_border_strips_that_leave_the_middle_untouched() {
        let got = strips(rect(10, 20, 100, 50), 2);
        assert_eq!(
            got,
            vec![
                rect(10, 20, 100, 2), // top edge
                rect(10, 68, 100, 2), // bottom edge
                rect(10, 22, 2, 46),  // left edge
                rect(108, 22, 2, 46), // right edge
            ],
            "the strips must tile the frame and nothing else"
        );
        // The frame area is the outline's total cost. A 100x50 box costs only
        // its border, not 5000 px of fill.
        let painted: i32 = got.iter().map(|s| s.w * s.h).sum();
        assert_eq!(painted, 100 * 2 * 2 + 46 * 2 * 2);
    }

    #[test]
    fn a_rect_too_small_for_a_hole_is_drawn_solid_rather_than_with_a_negative_middle() {
        assert_eq!(strips(rect(0, 0, 3, 40), 2), vec![rect(0, 0, 3, 40)]);
        assert_eq!(strips(rect(0, 0, 40, 4), 2), vec![rect(0, 0, 40, 4)]);
        assert!(strips(rect(0, 0, 0, 10), 2).is_empty(), "an empty rect draws nothing");
    }

    #[test]
    fn several_rects_share_one_surface_sized_to_their_bounding_box() {
        let marks = [mark(100, 100, 50, 20), mark(400, 300, 10, 10), mark(90, 500, 5, 5)];
        assert_eq!(bounds(&marks), Some(rect(90, 100, 320, 405)));
        assert_eq!(bounds(&[]), None, "nothing to outline is not a surface");
        assert_eq!(bounds(&[mark(0, 0, 0, 0)]), None, "degenerate rects do not size a surface");
    }

    #[test]
    fn n_rects_paint_n_frames_and_never_fill_between_them() {
        // Two 8x8 boxes sit side by side in a 20x8 surface.
        let (w, h) = (20, 8);
        let mut px = vec![CLEAR; (w * h) as usize];
        paint(&mut px, w, h, &[mark(0, 0, 8, 8), mark(12, 0, 8, 8)]);
        let at = |x: i32, y: i32| px[(y * w + x) as usize];

        for (x, y) in [(0, 0), (7, 0), (0, 7), (7, 7), (12, 0), (19, 7)] {
            assert_eq!(at(x, y), BORDER, "corner {x},{y} is border");
        }
        for (x, y) in [(4, 4), (16, 4)] {
            assert_eq!(at(x, y), CLEAR, "the middle of a frame at {x},{y} stays transparent");
        }
        for (x, y) in [(9, 0), (10, 4), (11, 7)] {
            assert_eq!(at(x, y), CLEAR, "the gap between two frames at {x},{y} is not painted");
        }
        // The border is 2 px thick. Row 1 has border, and row 2 has none.
        assert_eq!(at(4, 1), BORDER);
        assert_eq!(at(4, 2), CLEAR);
    }

    #[test]
    fn a_rect_hanging_off_the_surface_is_clipped_instead_of_writing_past_the_buffer() {
        let (w, h) = (10, 10);
        let mut px = vec![CLEAR; (w * h) as usize];
        // Half the rect lies outside every edge. The fill must remain in bounds.
        paint(&mut px, w, h, &[mark(-5, -5, 20, 20)]);
        assert_eq!(px.len(), 100, "the buffer must not have been resized");
        assert!(px.iter().all(|p| *p == CLEAR || *p == BORDER));
    }

    #[test]
    fn a_rect_spanning_two_outputs_is_outlined_on_both_clipped_to_each() {
        let left = rect(0, 0, 1920, 1080);
        let right = rect(1920, 0, 1920, 1080);
        let spanning = [mark(1900, 100, 100, 40)];
        assert_eq!(on_screen(&spanning, left), vec![mark(1900, 100, 20, 40)]);
        assert_eq!(on_screen(&spanning, right), vec![mark(1920, 100, 80, 40)]);
        assert!(on_screen(&[mark(5000, 0, 10, 10)], left).is_empty(), "a rect on neither output");
    }

    // -- the scan overlay's four kinds --

    #[test]
    fn each_scan_kind_takes_its_own_theme_colour_and_none_of_them_is_a_literal() {
        let t = Theme::dark();
        for (kind, want) in [
            (ScanKind::Pass1, t.scan_pass1),
            (ScanKind::Tile, t.scan_tile),
            (ScanKind::Anchor, t.scan_anchor),
            (ScanKind::Match, t.scan_match),
        ] {
            assert_eq!(scan_colour(kind, &t), opaque(want), "{kind:?} paints its own theme field");
        }
        // Carry the kind because the defined word must not use the capture box color.
        assert_ne!(
            scan_colour(ScanKind::Match, &t),
            scan_colour(ScanKind::Pass1, &t),
            "a match that looked like a capture box would tell the user nothing"
        );
        // The palette follows the theme, not the binary.
        assert_ne!(
            scan_colour(ScanKind::Match, &Theme::light()),
            scan_colour(ScanKind::Match, &Theme::dark()),
            "the two themes disagree about the match colour, so the overlay must too"
        );
    }

    #[test]
    fn a_theme_colour_becomes_an_opaque_premultiplied_argb_word() {
        // `Px` stores B, G, R, A in memory. This order matches the
        // little-endian `0xAARRGGBB` word from `wl_shm`.
        assert_eq!(opaque((0x11, 0x22, 0x33)), [0x33, 0x22, 0x11, 0xFF]);
    }

    #[test]
    fn a_scan_rect_is_framed_just_outside_itself_so_the_next_grab_reads_no_border() {
        let t = Theme::dark();
        let scanned = rect(100, 200, 60, 30);
        let marks = scan_marks(&[ScanRect { rect: scanned, kind: ScanKind::Pass1 }], &t);
        assert_eq!(1, marks.len());
        assert_eq!(
            rect(100 - BORDER_PX, 200 - BORDER_PX, 60 + 2 * BORDER_PX, 30 + 2 * BORDER_PX),
            marks[0].rect,
            "the frame lands in the band around the rect, never in the pixels OCR reads"
        );
        // Every painted strip stays outside the scanned box.
        for strip in strips(marks[0].rect, BORDER_PX) {
            assert_eq!(
                None,
                clip(strip, scanned),
                "strip {strip:?} overlaps the capture rect {scanned:?}"
            );
        }
    }

    #[test]
    fn the_four_kinds_of_one_hover_paint_four_colours_into_one_frame() {
        let t = Theme::dark();
        // A real hover provides the pass-1 box, a tile, the anchor, and the
        // match last. Core sets this order in `worker.rs`.
        let hover = [
            ScanRect { rect: rect(2, 2, 20, 20), kind: ScanKind::Pass1 },
            ScanRect { rect: rect(32, 2, 20, 20), kind: ScanKind::Tile },
            ScanRect { rect: rect(62, 2, 20, 20), kind: ScanKind::Anchor },
            ScanRect { rect: rect(92, 2, 20, 20), kind: ScanKind::Match },
        ];
        let marks = scan_marks(&hover, &t);
        let (w, h) = (120, 30);
        let mut px = vec![CLEAR; (w * h) as usize];
        paint(&mut px, w, h, &marks);
        let at = |x: i32, y: i32| px[(y * w + x) as usize];

        // The outset puts each frame's top-left corner two px above and left
        // of its scanned box.
        for (x, kind) in [(0, ScanKind::Pass1), (30, ScanKind::Tile), (60, ScanKind::Anchor), (90, ScanKind::Match)] {
            assert_eq!(at(x, 0), scan_colour(kind, &t), "the {kind:?} frame at x={x}");
        }
        assert_eq!(at(11, 11), CLEAR, "the middle of the pass-1 frame is the screen");
        assert_eq!(at(26, 0), CLEAR, "the gap between two kinds is not painted");
    }

    #[test]
    fn a_match_rect_split_across_two_outputs_keeps_the_match_colour_on_both() {
        let t = Theme::dark();
        let matched = scan_colour(ScanKind::Match, &t);
        let marks = scan_marks(&[ScanRect { rect: rect(1900, 100, 100, 40), kind: ScanKind::Match }], &t);
        let left = on_screen(&marks, rect(0, 0, 1920, 1080));
        let right = on_screen(&marks, rect(1920, 0, 1920, 1080));
        assert_eq!(vec![matched], left.iter().map(|m| m.colour).collect::<Vec<_>>());
        assert_eq!(vec![matched], right.iter().map(|m| m.colour).collect::<Vec<_>>());
        assert_eq!(1898, left[0].rect.x, "the outset half on the left output");
        assert_eq!(2002, right[0].rect.x + right[0].rect.w, "and the outset half on the right");
    }
}

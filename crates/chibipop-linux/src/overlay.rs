//! The outline overlay: click-through, frame-only rectangles on their
//! own `zwlr_layer_shell_v1` surfaces.
//!
//! The Linux counterpart of two Windows windows at once - the static
//! region's border (`ui/static_overlay.rs`, a frame-only window region)
//! and the scan-rect overlay (`ui/overlay.rs`). Both draw the same
//! thing: N rectangles as border strips with nothing in the middle, on
//! top of everything, taking no input and no focus. So this takes
//! `&[Mark]` and never assumes one rect - and a [`Mark`] carries a
//! colour, because the scan overlay's four [`ScanKind`]s mean four
//! different things and Windows paints them in four theme colours. The
//! static region passes one colour and one rect; nothing here knows
//! which caller it is serving.
//!
//! **Nothing here can be interacted with.** The input region is empty
//! on every commit, so a pointer falls straight through to whatever is
//! underneath, and `keyboard_interactivity` is `None` - the popup's
//! inviolable setting (ADR-0004), and for the same reason: an outline
//! that stole focus would be a bug in every possible use of it.
//!
//! **Sized to the rects, not to the screen.** A full-output surface
//! would mean a 33 MB buffer per output at 4K, repainted on every hover
//! for a scan overlay. Instead each output's surface is sized to the
//! bounding box of the rects that land on it and positioned by margins,
//! exactly as the popup positions its panel - so the buffer is as small
//! as the thing being outlined, and a rect spanning two monitors is
//! outlined on both, each surface drawing its own half.
//!
//! The raster path is the popup's, not a second one: the same
//! `wl_compositor` and `wl_shm` this process already bound (borrowed off
//! [`Popup`]), an `wl_shm` `SlotPool`, `Argb8888` buffers written as
//! premultiplied `[B, G, R, A]` words, and the popup's own
//! `wp_viewporter` carrying the logical size while `buffer_scale` stays
//! at one. It binds no `wp_fractional_scale_v1` of its own either: the
//! popup has a surface on every output and its `preferred_scale` is the
//! one source of scale truth, read back through [`Screen`] on every
//! show, so the scale is still never latched. Physical pixels stay
//! authoritative all the way down ([`crate::popup::derive`] is the one
//! derivation, shared).

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

/// `hyprctl layers` and `layerrule` see this.
const NAMESPACE: &str = "chibipop-outline";

/// One premultiplied `Argb8888` pixel as `wl_shm` stores it: a
/// little-endian `0xAARRGGBB` word, i.e. B, G, R, A in memory. Same
/// convention the popup's `to_argb` converts *into*.
pub type Px = [u8; 4];

/// Nothing: fully transparent, so the middle of a frame shows the
/// screen through it.
pub const CLEAR: Px = [0, 0, 0, 0];

/// Outline thickness, physical px. The Windows bin's `BORDER_PX`, so
/// the two platforms draw the same weight of line.
pub const BORDER_PX: i32 = 2;

/// The outline's colour: the Windows bin's teal `BORDER_COLOR`
/// (R 0xC0, G 0xE0, B 0xE0), fully opaque.
pub const BORDER: Px = [0xE0, 0xE0, 0xC0, 0xFF];

/// One rectangle and the colour that says what it means.
///
/// A bare rect is not enough for the scan overlay: core hands over four
/// kinds of box at once ([`ScanKind`]) and they mean four different
/// things to the person looking at the screen - the pass-1 capture, a
/// forward tile, the resolved word, and the characters actually being
/// defined. Windows paints each in its own theme colour
/// (`ui/overlay.rs`); flattening them into one border would say less.
/// The static region uses one colour and passes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub rect: PhysRect,
    pub colour: Px,
}

/// A theme's `(r, g, b)` as an opaque premultiplied `Argb8888` word.
/// At full alpha premultiplication is the identity, so this is only the
/// channel swap [`Px`] documents.
pub fn opaque((r, g, b): (u8, u8, u8)) -> Px {
    [b, g, r, 0xFF]
}

/// One scan rect's colour, from the theme the popup resolved.
///
/// The same four fields Windows' overlay paints from, so a user who
/// themed `.scan-match` sees the same box on both platforms.
pub fn scan_colour(kind: ScanKind, theme: &Theme) -> Px {
    match kind {
        ScanKind::Pass1 => opaque(theme.scan_pass1),
        ScanKind::Tile => opaque(theme.scan_tile),
        ScanKind::Anchor => opaque(theme.scan_anchor),
        ScanKind::Match => opaque(theme.scan_match),
    }
}

/// Core's scan rects as marks: outset by the border thickness, coloured
/// by kind.
///
/// **Outset, never inset** - the Windows overlay's rule and for its
/// reason (ADR-0008): a stroke drawn *inside* a scan rect would land in
/// the very pixels the next grab reads, so chibipop would OCR its own
/// furniture. Inflating by the thickness puts the whole frame in the
/// band around the rect and leaves the rect itself untouched.
pub fn scan_marks(rects: &[ScanRect], theme: &Theme) -> Vec<Mark> {
    rects
        .iter()
        .map(|r| Mark {
            rect: r.rect.inflated(BORDER_PX, BORDER_PX),
            colour: scan_colour(r.kind, theme),
        })
        .collect()
}

/// One rect's four border strips, surface-local physical pixels.
///
/// The Windows bin's `edge_strips`, with its degenerate case kept and
/// tightened from `<` to `<=`: a rect exactly two borders tall has no
/// middle left, so it is drawn solid rather than as two touching bands
/// plus two zero-height slivers. Windows gets away with the slivers
/// because `FillRect` ignores an empty rect; nothing here should be
/// asked to.
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

/// The smallest box holding all of them, or `None` for none.
///
/// This is the surface's size: an outline of three scan rects is one
/// surface as wide as the three together, not three surfaces and not
/// the whole screen. Colour plays no part - a mark's rect is all the
/// geometry it has.
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

/// Which of these marks touch this output, clipped to it. The colour
/// survives the clip: the half of a match rect that lands on the second
/// monitor is still the match colour.
pub fn on_screen(marks: &[Mark], monitor: PhysRect) -> Vec<Mark> {
    marks
        .iter()
        .filter_map(|m| Some(Mark { rect: clip(m.rect, monitor)?, colour: m.colour }))
        .collect()
}

/// `rect` ∩ `bounds`, or `None` when they do not overlap.
pub fn clip(rect: PhysRect, bounds: PhysRect) -> Option<PhysRect> {
    let x0 = rect.x.max(bounds.x);
    let y0 = rect.y.max(bounds.y);
    let x1 = (rect.x + rect.w).min(bounds.x + bounds.w);
    let y1 = (rect.y + rect.h).min(bounds.y + bounds.h);
    (x1 > x0 && y1 > y0).then_some(PhysRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// Fill one axis-aligned box into a premultiplied `Argb8888` buffer.
///
/// Row-wise slice fills rather than a tiny-skia pass: every box these
/// two overlays draw is axis-aligned and opaque, so there is nothing to
/// blend and nothing to antialias - and this way the geometry is a pure
/// function a test can read back pixel by pixel.
pub fn fill(px: &mut [Px], w: i32, h: i32, box_: PhysRect, colour: Px) {
    let Some(box_) = clip(box_, PhysRect { x: 0, y: 0, w, h }) else { return };
    for row in box_.y..box_.y + box_.h {
        let base = (row as usize) * (w as usize);
        let from = base + box_.x as usize;
        let to = from + box_.w as usize;
        px[from..to].fill(colour);
    }
}

/// Paint one surface's frame: transparent everywhere, border strips in
/// each mark's own colour.
///
/// `marks` are surface-local physical pixels - the caller has already
/// subtracted the bounding box's origin. They are drawn in the order
/// given, so the `Match` box core appends last (`worker.rs`) paints
/// over any capture box it overlaps, exactly as on Windows.
pub fn paint(px: &mut [Px], w: i32, h: i32, marks: &[Mark]) {
    px.fill(CLEAR);
    for mark in marks {
        for strip in strips(mark.rect, BORDER_PX) {
            fill(px, w, h, strip, mark.colour);
        }
    }
}

/// A frame waiting for the configure that will size it.
struct Pending {
    /// Device pixels to raster.
    buffer: (i32, i32),
    /// `set_size` and the viewport destination, logical units.
    logical: (i32, i32),
    /// Surface-local marks, ready to paint.
    marks: Vec<Mark>,
}

/// One output's outline surface.
struct Pane {
    /// The popup's stable surface id for this output, so both write the
    /// same monitor number.
    id: usize,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    /// The logical size the compositor last configured.
    configured: Option<(i32, i32)>,
    /// The frame this surface owes once its configure lands. Not
    /// coalesced against a frame callback: an outline changes at hover
    /// cadence at worst and is hidden the rest of the time, so there is
    /// no cursor-event rate here to bound.
    pending: Option<Pending>,
}

/// The outline overlay.
pub struct Outline {
    qh: QueueHandle<App>,
    /// The popup's own `wl_compositor` handle, cloned: a factory global
    /// with no state, and one per process is one fewer proxy to reason
    /// about.
    compositor: CompositorState,
    shell: LayerShell,
    /// The popup's `wp_viewporter`, cloned. See the module doc for why
    /// there is no fractional-scale object here.
    viewporter: Option<WpViewporter>,
    pool: SlotPool,
    panes: Vec<Pane>,
    /// What is on screen, global physical - so a diagnostic can say
    /// what is outlined, and a re-show at a new scale has something to
    /// re-derive from.
    shown: Vec<Mark>,
    notes: Vec<String>,
}

impl Outline {
    /// Bind the outline's globals. Surfaces come with the first
    /// [`Outline::show`], because there is nothing to size them to
    /// before that.
    ///
    /// `None` means this compositor advertises no `zwlr_layer_shell_v1`:
    /// the same *state, not error* rule `Popup::bind` follows (ADR-0004,
    /// ticket 49), so the caller reports the outline unavailable through
    /// the channel it already has and every other channel keeps
    /// running.
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>, popup: &Popup) -> Option<Outline> {
        let shell = LayerShell::bind(globals, qh).ok()?;
        // One modest outline's worth; the pool grows on demand.
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

    /// Diagnostics accumulated since the last drain. The outline owns no
    /// log; the daemon thread does.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// What is outlined right now, global physical.
    pub fn marks(&self) -> &[Mark] {
        &self.shown
    }

    pub fn surface_count(&self) -> usize {
        self.panes.len()
    }

    /// Does one layer surface belong to the outline? The routing
    /// question every shared SCTK handler now asks. There is no
    /// `wl_surface` twin: the outline requests no frame callbacks, so a
    /// `wl_surface` event never names one of these.
    pub fn owns_layer(&self, layer: &LayerSurface) -> bool {
        self.panes.iter().any(|p| &p.layer == layer)
    }

    /// Outline these marks. An empty slice is a [`Outline::hide`].
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
                // Nothing on this monitor: whatever it was outlining
                // last time must come off it.
                self.clear(screen.id);
                continue;
            };
            drawn += mine.len();
            let placement = derive(box_, screen.rect, screen.scale);
            // Surface-local: the strips are drawn against the bounding
            // box's own origin, never the global one.
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

    /// Take the outline down: a transparent buffer on every surface,
    /// never an unmap - Hyprland animates layer surfaces, and an
    /// outline that flew in and out on every hover would be worse than
    /// no outline at all.
    pub fn hide(&mut self) {
        self.shown.clear();
        let ids: Vec<usize> = self.panes.iter().map(|p| p.id).collect();
        for id in ids {
            self.clear(id);
        }
    }

    /// An output went away, or the compositor closed its surface:
    /// routine, and the next show maps a fresh one.
    pub fn drop_layer(&mut self, layer: &LayerSurface) {
        let Some(slot) = self.panes.iter().position(|p| &p.layer == layer) else { return };
        let id = self.panes[slot].id;
        self.panes.remove(slot);
        self.notes.push(format!("outline: surface {id} closed; {} left", self.panes.len()));
    }

    /// A configure landed: paint the frame it was asked for.
    pub fn configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(idx) = self.panes.iter().position(|p| &p.layer == layer) else { return };
        self.panes[idx].configured = Some((size.0 as i32, size.1 as i32));
        if let Some(pending) = self.panes[idx].pending.take() {
            self.draw(idx, pending);
        }
    }

    /// Map this output's surface if it has none, then size and draw. A
    /// resize costs a configure round-trip; the intermediate commit
    /// carries no buffer, so nothing flickers.
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

    /// Create one output's surface. The initial commit carries no
    /// buffer, so the compositor answers with a configure and the frame
    /// follows it.
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
        // Above fullscreen readers like the popup, never reserving
        // space, and never focusable: an outline is a mark on the screen
        // and nothing else.
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

    /// Raster one frame and commit it.
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
        // Empty on every commit: an outline is never a hit target, so
        // the pointer belongs to whatever is underneath it.
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

    /// Hand one surface a fully transparent buffer at its current size.
    /// Not gated on a frame callback: a hidden surface gets none, so
    /// waiting for one would never let it hide.
    ///
    /// A surface the compositor has not configured yet gets *nothing*.
    /// Attaching a buffer before the first configure is a protocol error
    /// (`layer_surface has never been configured`) that kills the whole
    /// connection, and it is reachable in one step: show then hide
    /// before the configure has come back. Dropping the pending frame is
    /// the correct hide anyway - a layer surface with no buffer attached
    /// is not mapped.
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

    /// A mark in the default colour, for the tests that are about
    /// geometry and not about which of the four kinds it is.
    fn mark(x: i32, y: i32, w: i32, h: i32) -> Mark {
        Mark { rect: rect(x, y, w, h), colour: BORDER }
    }

    #[test]
    fn one_rect_becomes_four_border_strips_that_leave_the_middle_untouched() {
        let got = strips(rect(10, 20, 100, 50), 2);
        assert_eq!(
            got,
            vec![
                rect(10, 20, 100, 2), // top
                rect(10, 68, 100, 2), // bottom
                rect(10, 22, 2, 46),  // left
                rect(108, 22, 2, 46), // right
            ],
            "the strips must tile the frame and nothing else"
        );
        // The frame's area is the outline's whole cost: a 100x50 box
        // costs its border, never 5000 px of fill.
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
        // Two 8x8 boxes side by side in a 20x8 surface.
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
        // The border is 2 px thick, so row 1 is border and row 2 is not.
        assert_eq!(at(4, 1), BORDER);
        assert_eq!(at(4, 2), CLEAR);
    }

    #[test]
    fn a_rect_hanging_off_the_surface_is_clipped_instead_of_writing_past_the_buffer() {
        let (w, h) = (10, 10);
        let mut px = vec![CLEAR; (w * h) as usize];
        // Half off every edge; the fill must survive and stay in bounds.
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
        // The point of carrying the kind at all: the word being defined
        // must not look like the box it was found in.
        assert_ne!(
            scan_colour(ScanKind::Match, &t),
            scan_colour(ScanKind::Pass1, &t),
            "a match that looked like a capture box would tell the user nothing"
        );
        // And the palette follows the theme rather than the binary.
        assert_ne!(
            scan_colour(ScanKind::Match, &Theme::light()),
            scan_colour(ScanKind::Match, &Theme::dark()),
            "the two themes disagree about the match colour, so the overlay must too"
        );
    }

    #[test]
    fn a_theme_colour_becomes_an_opaque_premultiplied_argb_word() {
        // `Px` is B, G, R, A in memory - the order `wl_shm`'s
        // little-endian 0xAARRGGBB puts them in.
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
            "the frame lands in the band around the rect, never in the pixels OCR reads (ADR-0008)"
        );
        // Every painted strip is outside the scanned box.
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
        // What a real hover hands over: the pass-1 box, a tile, the
        // anchor, and the match last (core's order, `worker.rs`).
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

        // Each frame's top-left corner, which the outset put two px up
        // and left of the scanned box.
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

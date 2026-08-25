//! Where the popup goes, and in which pixel space (ADR-0004).
//!
//! Physical pixels are authoritative: core places a known-size popup
//! with `geom::place_popup` in the global physical space, and the
//! surface's own geometry is derived back out of that - logical size
//! `ceil(physical / scale)`, margins `round(physical / scale)`, and the
//! `wp_viewport` destination equal to the logical size with
//! `buffer_scale` left at 1. Sub-pixel slack lands in the trailing
//! panel padding, where it is invisible.
//!
//! A layer surface's output is fixed at creation and its margins are
//! relative to that output, so placement is always "which output holds
//! the anchor, and where inside it" - never a global origin the
//! compositor picked for us.
//!
//! The scale is never latched. Hyprland may send `preferred_scale = 1.0`
//! first and correct it later (won't-fix upstream: apps must handle a
//! scale change at any time), so every `preferred_scale` is a re-render
//! decision, made by [`Visibility::rescale`], not a one-time fact.

use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{place_popup, PhysRect};
use smithay_client_toolkit::output::OutputInfo;
use wayland_client::protocol::wl_output::Transform;

/// Gap between anchor and panel, physical px. The Windows bin's
/// `POPUP_GAP`: the same number, so a hover looks the same on both
/// platforms.
pub const POPUP_GAP: i32 = 40;

/// One output's box in the global physical space.
///
/// Same convention as the cursor channel (`cursor::outputs`): each
/// output's logical origin scaled by that output's own scale. Exact
/// for single-output and uniform-scale layouts, and the documented
/// approximation otherwise - the popup and the cursor must agree about
/// where an anchor is, so they share the convention rather than each
/// inventing one.
pub fn output_physical(geo: &OutputGeometry) -> PhysRect {
    let (x, y) = geo.physical_origin();
    let w = geo.physical_w();
    let h = if geo.transform_swaps { geo.mode_w } else { geo.mode_h };
    PhysRect { x, y, w, h }
}

/// One SCTK `OutputInfo` as the layout facts the conversions need.
///
/// The popup enumerates outputs through SCTK while the cursor channel
/// binds its own `wl_output`s; both end up in `OutputGeometry` so the
/// two agree about the global physical space by construction. Logical
/// geometry comes from xdg-output where the compositor offers it, and
/// is derived from the mode and the integer scale where it does not.
pub fn geometry_of(info: &OutputInfo) -> OutputGeometry {
    let mode = info.modes.iter().find(|m| m.current).or_else(|| info.modes.first());
    let (mode_w, mode_h) = mode.map_or((0, 0), |m| m.dimensions);
    let transform_swaps = matches!(
        info.transform,
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
    );
    let (logical_x, logical_y) = info.logical_position.unwrap_or(info.location);
    let (logical_w, logical_h) = info.logical_size.unwrap_or_else(|| {
        let s = info.scale_factor.max(1);
        let (w, h) = if transform_swaps { (mode_h, mode_w) } else { (mode_w, mode_h) };
        (w / s, h / s)
    });
    OutputGeometry { logical_x, logical_y, logical_w, logical_h, mode_w, mode_h, transform_swaps }
}

/// The scale to render at.
///
/// `wp_fractional_scale_v1.preferred_scale` arrives in 120ths, which is
/// the only source that ever reports 1.5. Until it has spoken, the
/// output's own physical/logical ratio is the best guess (it is what
/// the cursor channel uses), and 1.0 is the floor - never zero, which
/// would divide by nothing on the way back to logical units.
pub fn fractional(preferred: Option<u32>, geo: &OutputGeometry) -> f64 {
    let scale = match preferred {
        Some(n) if n > 0 => f64::from(n) / 120.0,
        _ => geo.scale(),
    };
    if scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// One popup's whole geometry, both spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Where it sits, global physical: what `Event::PopupPlaced`
    /// reports and what the capture mask has to blank out.
    pub rect: PhysRect,
    /// The buffer to raster, device pixels.
    pub buffer: (i32, i32),
    /// `set_size` and the viewport destination, logical units.
    pub logical: (i32, i32),
    /// `set_margin` top and left, logical units relative to the output
    /// the surface was created on. The surface is anchored top-left, so
    /// these two are the position.
    pub margin: (i32, i32),
}

/// Place a measured popup on one output.
///
/// `size` is the panel's physical pixel size (body view plus the Anki
/// strip - one surface holds both on Linux); `monitor` is that output's
/// physical box, in the same global space as `anchor`.
pub fn place(anchor: PhysRect, size: (i32, i32), monitor: PhysRect, scale: f64) -> Placement {
    let rect = place_popup(anchor, size, monitor, POPUP_GAP);
    let scale = if scale > 0.0 { scale } else { 1.0 };

    // Output-local, because margins are.
    let local_x = f64::from(rect.x - monitor.x);
    let local_y = f64::from(rect.y - monitor.y);

    Placement {
        rect,
        buffer: (rect.w.max(1), rect.h.max(1)),
        logical: (
            (f64::from(rect.w) / scale).ceil() as i32,
            (f64::from(rect.h) / scale).ceil() as i32,
        ),
        margin: ((local_y / scale).round() as i32, (local_x / scale).round() as i32),
    }
}

/// Which output holds an anchor.
///
/// The one containing its top-left, or - for an anchor in a layout gap
/// or off-screen - the nearest by edge distance. `None` only while no
/// output geometry has arrived yet.
pub fn output_at<'a>(outputs: impl Iterator<Item = (usize, &'a OutputGeometry)>, anchor: PhysRect) -> Option<usize> {
    let point = chibipop::geom::PhysPoint { x: anchor.x, y: anchor.y };
    let mut nearest: Option<(usize, f64)> = None;
    for (idx, geo) in outputs {
        let rect = output_physical(geo);
        if rect.w <= 0 || rect.h <= 0 {
            continue;
        }
        if rect.contains(point) {
            return Some(idx);
        }
        let d = rect.edge_distance_to(point);
        if nearest.is_none_or(|(_, best)| d < best) {
            nearest = Some((idx, d));
        }
    }
    nearest.map(|(idx, _)| idx)
}

/// The show/hide state machine.
///
/// Hiding attaches a fully transparent buffer and clears the input
/// region; the surface is never unmapped, because Hyprland animates
/// layer surfaces by default and a map per lookup would fly the popup
/// in on every hover - a regression against Windows, where show and
/// hide are instant (ADR-0004). The state exists so a redundant hide
/// costs no commit at all: at the hover cadence this runs, "already
/// hidden" is the common case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Visibility {
    Hidden,
    Shown(Shown),
}

/// What is on screen right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    /// Which output's surface carries it.
    pub output: usize,
    pub placement: Placement,
    /// The scale that buffer was rastered at.
    pub scale: f64,
}

impl Visibility {
    /// Show on `output`. Returns the surface that must be cleared
    /// first, when the popup was up on a *different* output: one
    /// surface per output means a cross-output move is a hide plus a
    /// show, and leaving the old one painted would show two popups.
    pub fn show(&mut self, next: Shown) -> Option<usize> {
        let stale = match *self {
            Visibility::Shown(prev) if prev.output != next.output => Some(prev.output),
            _ => None,
        };
        *self = Visibility::Shown(next);
        stale
    }

    /// Hide. `Some(output)` is the surface to hand a transparent
    /// buffer; `None` means nothing is up and no commit is owed.
    pub fn hide(&mut self) -> Option<usize> {
        match std::mem::replace(self, Visibility::Hidden) {
            Visibility::Shown(shown) => Some(shown.output),
            Visibility::Hidden => None,
        }
    }

    pub fn shown(&self) -> Option<Shown> {
        match *self {
            Visibility::Shown(shown) => Some(shown),
            Visibility::Hidden => None,
        }
    }

    /// A `preferred_scale` arrived for `output`: must the frame be
    /// rastered again?
    ///
    /// Only when that output is the one currently showing the popup and
    /// the number actually moved - a hidden surface's buffer is
    /// transparent at every scale, and a repeat of the same scale is
    /// the compositor being chatty. The new scale is recorded either
    /// way, so the *next* show renders at it.
    pub fn rescale(&mut self, output: usize, scale: f64) -> bool {
        let Visibility::Shown(shown) = self else { return false };
        if shown.output != output || !scale_moved(shown.scale, scale) {
            return false;
        }
        shown.scale = scale;
        true
    }
}

/// Scale equality, with float slack. 120ths in, so anything under half
/// a 240th apart is the same scale.
pub fn scale_moved(was: f64, now: f64) -> bool {
    (was - now).abs() > 1.0 / 240.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2560x1440 panel at 1.5, logical origin 0.
    fn output(logical_x: i32, logical_w: i32, mode_w: i32) -> OutputGeometry {
        OutputGeometry {
            logical_x,
            logical_y: 0,
            logical_w,
            logical_h: 960,
            mode_w,
            mode_h: 1440,
            transform_swaps: false,
        }
    }

    #[test]
    fn an_outputs_physical_box_is_its_mode_at_its_own_scaled_origin() {
        // Second head of a 1.5x pair: 2560 device px shown as 1707
        // logical, sitting immediately right of the first.
        let geo = output(1707, 1707, 2560);
        let rect = output_physical(&geo);
        // The origin lands on the panel edge exactly, because the scale
        // is this output's own ratio (2560/1707) rather than the 1.5 the
        // compositor rounded it from - which is the point of deriving it
        // per output instead of trusting one global number.
        assert_eq!(2560, rect.x);
        assert_eq!(2560, rect.w);
        assert_eq!(1440, rect.h);
    }

    #[test]
    fn a_rotated_output_swaps_its_physical_extent() {
        let mut geo = output(0, 1707, 2560);
        geo.transform_swaps = true;
        let rect = output_physical(&geo);
        assert_eq!(1440, rect.w);
        assert_eq!(2560, rect.h);
    }

    #[test]
    fn a_preferred_scale_of_180_120ths_is_one_and_a_half() {
        let geo = output(0, 1707, 2560);
        assert_eq!(1.5, fractional(Some(180), &geo));
    }

    #[test]
    fn without_a_preferred_scale_the_outputs_own_ratio_stands_in() {
        let geo = output(0, 1280, 2560);
        assert_eq!(2.0, fractional(None, &geo));
        assert_eq!(2.0, fractional(Some(0), &geo), "a zero denominator is not a scale");
    }

    #[test]
    fn an_unknown_output_still_reports_a_usable_scale() {
        assert_eq!(1.0, fractional(None, &OutputGeometry::default()));
    }

    #[test]
    fn placement_derives_logical_size_and_margins_from_the_physical_rect() {
        let monitor = PhysRect { x: 0, y: 0, w: 2560, h: 1440 };
        let anchor = PhysRect { x: 600, y: 300, w: 90, h: 30 };
        let p = place(anchor, (640, 480), monitor, 1.5);

        // place_popup: below the anchor, gap 40.
        assert_eq!(PhysRect { x: 600, y: 370, w: 640, h: 480 }, p.rect);
        assert_eq!((640, 480), p.buffer, "the raster is device pixels");
        assert_eq!((427, 320), p.logical, "ceil(640/1.5)=427, ceil(480/1.5)=320");
        assert_eq!((247, 400), p.margin, "round(370/1.5)=247 top, round(600/1.5)=400 left");
    }

    #[test]
    fn margins_are_relative_to_the_output_the_surface_lives_on() {
        // Second head at physical x 2560, scale 1.0.
        let monitor = PhysRect { x: 2560, y: 0, w: 1920, h: 1080 };
        let anchor = PhysRect { x: 2600, y: 100, w: 50, h: 20 };
        let p = place(anchor, (400, 300), monitor, 1.0);
        assert_eq!(PhysRect { x: 2600, y: 160, w: 400, h: 300 }, p.rect);
        assert_eq!((160, 40), p.margin, "output-local, not global");
    }

    #[test]
    fn scale_one_leaves_physical_and_logical_identical() {
        let monitor = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };
        let p = place(PhysRect { x: 10, y: 10, w: 10, h: 10 }, (300, 200), monitor, 1.0);
        assert_eq!((300, 200), p.logical);
        assert_eq!(p.buffer, p.logical);
    }

    #[test]
    fn a_popup_that_would_fall_off_the_bottom_flips_above_the_anchor() {
        let monitor = PhysRect { x: 0, y: 0, w: 2560, h: 1440 };
        let anchor = PhysRect { x: 100, y: 1300, w: 80, h: 30 };
        let p = place(anchor, (400, 600), monitor, 1.5);
        assert!(p.rect.y + p.rect.h <= 1440, "placed at {:?}", p.rect);
        assert_eq!(660, p.rect.y, "gap above the anchor, not below it");
        assert_eq!((440, 67), p.margin, "round(660/1.5) top, round(100/1.5) left");
    }

    #[test]
    fn the_anchors_output_is_the_one_containing_it() {
        let left = output(0, 1707, 2560);
        let right = output(1707, 1707, 2560);
        let outs = [left, right];
        let at = |x| output_at(outs.iter().enumerate(), PhysRect { x, y: 10, w: 4, h: 4 });
        assert_eq!(Some(0), at(100));
        assert_eq!(Some(1), at(3000));
    }

    #[test]
    fn an_anchor_in_no_output_lands_on_the_nearest() {
        let outs = [output(0, 1707, 2560)];
        let at = output_at(outs.iter().enumerate(), PhysRect { x: 9000, y: 10, w: 4, h: 4 });
        assert_eq!(Some(0), at);
        assert_eq!(None, output_at(std::iter::empty::<(usize, &OutputGeometry)>(), PhysRect { x: 0, y: 0, w: 1, h: 1 }));
    }

    fn shown(output: usize, scale: f64) -> Shown {
        let monitor = PhysRect { x: 0, y: 0, w: 2560, h: 1440 };
        Shown {
            output,
            placement: place(PhysRect { x: 10, y: 10, w: 10, h: 10 }, (300, 200), monitor, scale),
            scale,
        }
    }

    #[test]
    fn hiding_twice_owes_only_one_commit() {
        let mut v = Visibility::Hidden;
        assert_eq!(None, v.hide(), "nothing is up, so nothing is committed");
        v.show(shown(0, 1.5));
        assert_eq!(Some(0), v.hide());
        assert_eq!(None, v.hide(), "the second hide is free");
        assert_eq!(Visibility::Hidden, v);
    }

    #[test]
    fn showing_on_another_output_clears_the_one_it_left() {
        let mut v = Visibility::Hidden;
        assert_eq!(None, v.show(shown(0, 1.5)));
        assert_eq!(None, v.show(shown(0, 1.5)), "a re-show in place clears nothing");
        assert_eq!(Some(0), v.show(shown(1, 1.0)), "two popups would be visible otherwise");
        assert_eq!(Some(1), v.shown().map(|s| s.output));
    }

    #[test]
    fn a_scale_change_re_renders_only_the_surface_that_is_showing() {
        let mut v = Visibility::Hidden;
        assert!(!v.rescale(0, 2.0), "a hidden buffer is transparent at every scale");
        v.show(shown(0, 1.0));
        assert!(!v.rescale(1, 2.0), "another output's scale is not our frame");
        assert!(v.rescale(0, 1.5), "Hyprland corrects 1.0 to 1.5 after the first commit");
        assert_eq!(Some(1.5), v.shown().map(|s| s.scale));
        assert!(!v.rescale(0, 1.5), "the same scale again is chatter, not a repaint");
    }

    #[test]
    fn scale_equality_has_slack_but_not_a_120th_of_it() {
        assert!(!scale_moved(1.5, 1.5));
        assert!(!scale_moved(1.5, 1.5 + 1e-9));
        assert!(scale_moved(1.5, 1.5 + 1.0 / 120.0), "121/120ths is a different scale");
    }
}

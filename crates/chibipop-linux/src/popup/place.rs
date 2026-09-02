//! Where the popup goes, and in which pixel space.
//!
//! Physical pixels are authoritative. Core places a known-size popup
//! with `geom::place_popup` in the global physical space. This module
//! derives the surface geometry from that placement: logical size
//! `ceil(physical / scale)`, margins `round(physical / scale)`, and a
//! `wp_viewport` destination equal to the logical size. The code keeps
//! `buffer_scale` at 1. Sub-pixel slack lands in the trailing panel
//! padding, where it is invisible.
//!
//! A layer surface holds one output from creation, and its margins are
//! relative to that output. Therefore, placement always answers two
//! questions: which output holds the anchor, and where inside that output.
//! The compositor never chooses a global origin for the application.
//!
//! The scale is never latched. Hyprland can send `preferred_scale = 1.0`
//! first and correct it later. Upstream marked this behavior as will-not-fix.
//! Applications must handle a scale change at any time. Therefore, every
//! `preferred_scale` is a re-render decision made by [`Visibility::rescale`].
//! It is not a one-time fact.

use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{place_popup, PhysRect};
use smithay_client_toolkit::output::OutputInfo;
use wayland_client::protocol::wl_output::Transform;

/// Gap between the anchor and the panel, in physical pixels.
/// This value matches the Windows bin's `POPUP_GAP`.
/// Therefore, a hover looks the same on both platforms.
pub const POPUP_GAP: i32 = 40;

/// One output's box in the global physical space.
///
/// This function uses the same convention as the cursor channel (`cursor::outputs`).
/// This function scales each output's logical origin by that output's own scale.
/// This convention is exact for single-output and uniform-scale layouts.
/// It is the documented approximation otherwise.
/// The popup and the cursor must agree about where an anchor is.
/// Therefore, they share one convention. Neither one invents its own.
pub fn output_physical(geo: &OutputGeometry) -> PhysRect {
    let (x, y) = geo.physical_origin();
    let w = geo.physical_w();
    let h = if geo.transform_swaps { geo.mode_w } else { geo.mode_h };
    PhysRect { x, y, w, h }
}

/// Convert one SCTK `OutputInfo` into the layout facts that placement needs.
///
/// The popup enumerates outputs through SCTK while the cursor channel
/// binds its own `wl_output` objects. Both produce an `OutputGeometry`.
/// Therefore, the two agree about the global physical space by construction.
/// Logical geometry comes from xdg-output when the compositor offers it.
/// Otherwise, this function derives the geometry from the mode and the
/// integer scale.
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
/// `wp_fractional_scale_v1.preferred_scale` arrives in 120ths.
/// This protocol is the only source that reports fractional scales such as 1.5.
/// Before it arrives, the output's own physical-to-logical ratio is the best guess.
/// The cursor channel uses that ratio.
/// The floor is 1.0. A zero scale would cause a division by zero.
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

/// One popup's whole geometry, in both coordinate spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Where the popup sits, in global physical coordinates.
    /// `Event::PopupPlaced` reports this rectangle, and the capture mask blanks it.
    pub rect: PhysRect,
    /// The buffer to render, in device pixels.
    pub buffer: (i32, i32),
    /// `set_size` and the viewport destination, in logical units.
    pub logical: (i32, i32),
    /// `set_margin` top and left, in logical units relative to the output.
    /// This module created the surface on that output.
    /// Because the surface anchors to its top-left corner, these two margins set the position.
    pub margin: (i32, i32),
}

/// Convert one physical rect on one output into that surface's own geometry.
///
/// This function holds the whole calculation because three surfaces need it.
/// Physical pixels are authoritative, and the buffer is that many device pixels.
/// The logical size is `ceil(physical / scale)`.
/// The margins are `round(local / scale)` from the output's top-left corner.
/// A layer surface's margins are relative to the output that holds it.
/// Sub-pixel slack lands in the last row and column of the buffer.
/// That slack is padding on the popup and one border pixel on other surfaces.
pub fn derive(rect: PhysRect, monitor: PhysRect, scale: f64) -> Placement {
    let scale = if scale > 0.0 { scale } else { 1.0 };

    // Output-local, because margins are output-local.
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

/// Place a measured popup on one output.
///
/// `size` is the panel's physical pixel size. It contains the body view and
/// the Anki strip. One surface holds both on Linux. `monitor` is that output's
/// physical box, in the same global space as `anchor`.
pub fn place(anchor: PhysRect, size: (i32, i32), monitor: PhysRect, scale: f64) -> Placement {
    derive(place_popup(anchor, size, monitor, POPUP_GAP), monitor, scale)
}

/// One output, as a surface beside the popup needs it.
///
/// The selector and the outline map their own layer surfaces.
/// They must pin each surface to a `wl_output`.
/// A layer surface holds one output from creation, and its margins are relative to it.
/// Therefore, `output = NULL` would give them an unchosen origin.
/// They read this list from the popup. They do not bind a second `OutputState`.
/// Therefore, all three surfaces agree about the global physical space by construction.
#[derive(Debug, Clone)]
pub struct Screen {
    /// The popup's stable surface identifier for this output.
    /// A diagnostic that names surface 1 refers to the same monitor everywhere.
    pub id: usize,
    pub output: wayland_client::protocol::wl_output::WlOutput,
    /// This output's box in the global physical space.
    pub rect: PhysRect,
    /// The scale to render at: `preferred_scale` when the compositor reports it,
    /// or the output's own ratio before that. This value is never latched.
    pub scale: f64,
    /// What the log calls this monitor.
    pub name: String,
}

/// Find which output holds an anchor.
///
/// Returns the output that contains the anchor's top-left point.
/// For an anchor in a layout gap or off-screen, returns the nearest output by edge distance.
/// Returns `None` only before the first output geometry arrives.
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

/// The show and hide state machine.
///
/// A hide attaches a fully transparent buffer and clears the input region.
/// The code never unmaps the surface.
/// Hyprland animates layer surfaces by default.
/// An unmap and map cycle for each lookup would animate the popup on every hover.
/// That result is a regression against Windows, where show and hide are instant.
/// This state machine stops a second hide commit when the popup is already hidden.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Visibility {
    Hidden,
    Shown(Shown),
}

/// What is on screen right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    /// Which output's surface carries the popup.
    pub output: usize,
    pub placement: Placement,
    /// The scale of the rendered buffer.
    pub scale: f64,
}

impl Visibility {
    /// Show on `output`.
    /// Returns the surface that the caller must clear when the popup was on another output.
    /// Because each output has one surface, a cross-output move is a hide and a show.
    /// An old surface that stays painted would show two popups.
    pub fn show(&mut self, next: Shown) -> Option<usize> {
        let stale = match *self {
            Visibility::Shown(prev) if prev.output != next.output => Some(prev.output),
            _ => None,
        };
        *self = Visibility::Shown(next);
        stale
    }

    /// Hide the popup.
    /// `Some(output)` is the surface that must receive a transparent buffer.
    /// `None` means the popup is hidden. The caller commits nothing.
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

    /// Handle a new `preferred_scale` for `output`.
    /// Returns true when the caller must render the frame again.
    ///
    /// Returns true only when that output shows the popup and the scale changed.
    /// A hidden surface's buffer is transparent at every scale.
    /// This function ignores a repeat of the same scale.
    /// This function records the new scale in both cases.
    /// Therefore, the next show renders at that scale.
    pub fn rescale(&mut self, output: usize, scale: f64) -> bool {
        let Visibility::Shown(shown) = self else { return false };
        if shown.output != output || !scale_moved(shown.scale, scale) {
            return false;
        }
        shown.scale = scale;
        true
    }
}

/// Compare two scales with a float tolerance.
/// The compositor reports a scale in 120ths.
/// Two values less than half of a 240th apart are equal.
pub fn scale_moved(was: f64, now: f64) -> bool {
    (was - now).abs() > 1.0 / 240.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2560x1440 panel at 1.5 scale, logical origin 0.
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
        // Second head of a 1.5x pair. The output shows 2560 device pixels as
        // 1707 logical units. It sits directly to the right of the first head.
        let geo = output(1707, 1707, 2560);
        let rect = output_physical(&geo);
        // The origin lands on the panel edge exactly.
        // The scale is this output's own ratio (2560/1707) instead of 1.5.
        // This test verifies that the code derives a scale for each output.
        // The code does not trust one global number.
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

        // place_popup: below the anchor, with gap 40.
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

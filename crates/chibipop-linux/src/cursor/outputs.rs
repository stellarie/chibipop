//! Per-output geometry, and the conversions both rungs share.
//!
//! Core speaks *physical* pixels (`PhysPoint`). Cursor-session
//! positions arrive in transformed buffer pixels relative to one
//! output; hyprctl positions arrive in the compositor's logical layout
//! space. Both convert here.
//!
//! The global physical space is anchored by scaling each output's
//! logical origin by that output's own scale. Exact for single-output
//! and uniform-scale layouts; for mixed-scale layouts a global
//! physical space is not well-defined, and this is the documented
//! approximation until a ticket needs better.

use chibipop::geom::PhysPoint;

/// One output's layout facts, accumulated from `wl_output` and
/// `zxdg_output_v1` events. Zero until the first roundtrip delivers
/// them; conversions guard against the empty state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputGeometry {
    /// Logical layout origin (`zxdg_output_v1.logical_position`, with
    /// `wl_output.geometry` x/y as the fallback).
    pub logical_x: i32,
    pub logical_y: i32,
    /// Logical layout size (`zxdg_output_v1.logical_size`).
    pub logical_w: i32,
    pub logical_h: i32,
    /// Current-mode size, hardware pixels, untransformed.
    pub mode_w: i32,
    pub mode_h: i32,
    /// The output transform rotates 90/270: buffer w/h swap.
    pub transform_swaps: bool,
}

impl OutputGeometry {
    /// Buffer width after the output transform — the space cursor
    /// session positions live in.
    pub fn physical_w(&self) -> i32 {
        if self.transform_swaps { self.mode_h } else { self.mode_w }
    }

    /// Physical pixels per logical unit. 1.0 until both sizes are
    /// known — an identity fallback, never a crash.
    pub fn scale(&self) -> f64 {
        if self.logical_w > 0 && self.physical_w() > 0 {
            f64::from(self.physical_w()) / f64::from(self.logical_w)
        } else {
            1.0
        }
    }

    /// This output's origin in the global physical space.
    pub fn physical_origin(&self) -> (i32, i32) {
        let s = self.scale();
        (
            (f64::from(self.logical_x) * s).round() as i32,
            (f64::from(self.logical_y) * s).round() as i32,
        )
    }

    /// A cursor-session position (buffer pixels, this output) to
    /// global physical.
    pub fn buffer_to_global(&self, x: i32, y: i32) -> PhysPoint {
        let (ox, oy) = self.physical_origin();
        PhysPoint { x: ox + x, y: oy + y }
    }

    /// A cursor-session `position` event to global physical.
    ///
    /// The protocol says these are transformed buffer pixel
    /// coordinates, and wlroots complies (output cursor x/y are
    /// stored pre-multiplied by scale — wlroots
    /// `types/output/cursor.c`). Hyprland <= 0.55 deviates: it sends
    /// output-local *logical* units (`ImageCopyCapture.cpp` subtracts
    /// the source's `logicalBox()` origin from the layout position,
    /// v0.55.4 lines 317-335), so under Hyprland the sample scales
    /// first. Verified live on Hyprland 0.55.4 at scale 1.5.
    pub fn session_to_global(&self, x: i32, y: i32, logical: bool) -> PhysPoint {
        if logical {
            let s = self.scale();
            self.buffer_to_global(
                (f64::from(x) * s).round() as i32,
                (f64::from(y) * s).round() as i32,
            )
        } else {
            self.buffer_to_global(x, y)
        }
    }

    /// A logical layout point to global physical.
    pub fn logical_to_global(&self, x: f64, y: f64) -> PhysPoint {
        let s = self.scale();
        let (ox, oy) = self.physical_origin();
        PhysPoint {
            x: ox + ((x - f64::from(self.logical_x)) * s).round() as i32,
            y: oy + ((y - f64::from(self.logical_y)) * s).round() as i32,
        }
    }

    pub fn contains_logical(&self, x: f64, y: f64) -> bool {
        x >= f64::from(self.logical_x)
            && x < f64::from(self.logical_x + self.logical_w)
            && y >= f64::from(self.logical_y)
            && y < f64::from(self.logical_y + self.logical_h)
    }
}

/// Convert a logical layout point using the output that contains it;
/// `None` while no output geometry is known yet.
pub fn logical_to_global<'a>(
    geometries: impl Iterator<Item = &'a OutputGeometry>,
    x: f64,
    y: f64,
) -> Option<PhysPoint> {
    let mut first: Option<&OutputGeometry> = None;
    for geo in geometries {
        if geo.contains_logical(x, y) {
            return Some(geo.logical_to_global(x, y));
        }
        first.get_or_insert(geo);
    }
    // Off every known output (mid-layout gap): the first output's
    // scale is the least-wrong anchor.
    first.map(|geo| geo.logical_to_global(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This box: 3840x2160 at scale 1.5 -> 2560x1440 logical.
    const DP1: OutputGeometry = OutputGeometry {
        logical_x: 0,
        logical_y: 0,
        logical_w: 2560,
        logical_h: 1440,
        mode_w: 3840,
        mode_h: 2160,
        transform_swaps: false,
    };

    #[test]
    fn fractional_scale_comes_from_the_size_ratio() {
        assert_eq!(1.5, DP1.scale());
    }

    #[test]
    fn a_logical_point_lands_on_physical_pixels() {
        assert_eq!(PhysPoint { x: 969, y: 1310 }, DP1.logical_to_global(646.0, 873.0));
    }

    #[test]
    fn buffer_positions_offset_by_the_physical_origin() {
        assert_eq!(PhysPoint { x: 100, y: 200 }, DP1.buffer_to_global(100, 200));
        let second = OutputGeometry { logical_x: 2560, ..DP1 };
        assert_eq!(PhysPoint { x: 3840 + 100, y: 200 }, second.buffer_to_global(100, 200));
    }

    /// The Hyprland deviation: logical in, physical out; spec-conform
    /// buffer pixels pass through.
    #[test]
    fn session_positions_scale_only_under_the_hyprland_quirk() {
        assert_eq!(PhysPoint { x: 3000, y: 1800 }, DP1.session_to_global(2000, 1200, true));
        assert_eq!(PhysPoint { x: 2000, y: 1200 }, DP1.session_to_global(2000, 1200, false));
    }

    #[test]
    fn the_containing_output_wins() {
        let right = OutputGeometry { logical_x: 2560, ..DP1 };
        let geos = [DP1, right];
        assert_eq!(
            Some(PhysPoint { x: 3840 + 150, y: 0 }),
            logical_to_global(geos.iter(), 2660.0, 0.0)
        );
        assert_eq!(Some(PhysPoint { x: 150, y: 0 }), logical_to_global(geos.iter(), 100.0, 0.0));
    }

    #[test]
    fn no_outputs_means_no_position() {
        assert_eq!(None, logical_to_global([].iter(), 10.0, 10.0));
    }

    #[test]
    fn a_rotated_output_swaps_buffer_axes() {
        let rotated = OutputGeometry {
            logical_w: 1440,
            logical_h: 2560,
            transform_swaps: true,
            ..DP1
        };
        assert_eq!(2160, rotated.physical_w());
        assert_eq!(1.5, rotated.scale());
    }

    /// Before any events arrive the conversion must not divide by
    /// zero or invent an offset.
    #[test]
    fn the_empty_geometry_is_identity() {
        let empty = OutputGeometry::default();
        assert_eq!(1.0, empty.scale());
        assert_eq!(PhysPoint { x: 5, y: 7 }, empty.buffer_to_global(5, 7));
    }
}

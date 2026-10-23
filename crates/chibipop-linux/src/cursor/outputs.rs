//! This module provides per-output geometry and conversions for both cursor rungs.
//!
//! Core uses *physical* pixels (`PhysPoint`). The protocol defines
//! cursor-session positions as transformed buffer pixels relative to one
//! output. Affected Hyprland versions can send logical units instead.
//! `hyprctl` positions arrive in the compositor's logical layout space.
//! This module converts both forms.
//!
//! This module builds global physical space by multiplying each output's
//! logical origin by that output's scale. The result is exact for one
//! output or uniform-scale layouts. Mixed-scale layouts do not define
//! one global physical space. This documented approximation remains until
//! a caller needs a better model.

use chibipop::geom::PhysPoint;

/// `OutputGeometry` stores layout facts for one output. `wl_output` and
/// `zxdg_output_v1` events fill these fields. Fields can remain zero before
/// the first roundtrip. Conversions support default and fallback geometry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputGeometry {
    /// Store the logical layout origin from `zxdg_output_v1.logical_position`.
    /// `wl_output.geometry` supplies fallback x/y values.
    pub logical_x: i32,
    pub logical_y: i32,
    /// Store the logical layout size from `zxdg_output_v1.logical_size`.
    pub logical_w: i32,
    pub logical_h: i32,
    /// Store the current-mode size in untransformed hardware pixels.
    pub mode_w: i32,
    pub mode_h: i32,
    /// `transform_swaps` is true for `_90`, `_270`, `Flipped90`, and `Flipped270`.
    /// These transforms swap the buffer width and height.
    pub transform_swaps: bool,
}

impl OutputGeometry {
    /// Return the buffer width after the output transform. Cursor-session
    /// positions use this space.
    pub fn physical_w(&self) -> i32 {
        if self.transform_swaps { self.mode_h } else { self.mode_w }
    }

    /// Calculate physical pixels per logical unit from the transformed physical
    /// width and logical width. Return 1.0 until both widths are positive.
    pub fn scale(&self) -> f64 {
        if self.logical_w > 0 && self.physical_w() > 0 {
            f64::from(self.physical_w()) / f64::from(self.logical_w)
        } else {
            1.0
        }
    }

    /// Return this output's origin in global physical space.
    pub fn physical_origin(&self) -> (i32, i32) {
        let s = self.scale();
        (
            (f64::from(self.logical_x) * s).round() as i32,
            (f64::from(self.logical_y) * s).round() as i32,
        )
    }

    /// Add this output's global physical origin to buffer-local coordinates.
    /// The buffer-local x and y values stay unchanged before this translation.
    pub fn buffer_to_global(&self, x: i32, y: i32) -> PhysPoint {
        let (ox, oy) = self.physical_origin();
        PhysPoint { x: ox + x, y: oy + y }
    }

    /// Convert a cursor-session `position` event to global physical pixels.
    ///
    /// The protocol defines transformed buffer pixel coordinates. wlroots
    /// complies with this rule. It stores output cursor x/y pre-multiplied
    /// by scale in wlroots `types/output/cursor.c`. Affected Hyprland versions
    /// can send output-local logical units. `ImageCopyCapture.cpp` subtracts
    /// the source `logicalBox()` origin from the layout position. See v0.55.4
    /// lines 317-335. For those samples, scale the sample before
    /// global-origin translation.
    /// Live verification used Hyprland 0.55.4 at scale 1.5.
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

    /// Convert a logical layout point to global physical pixels.
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

/// Convert a logical layout point with a containing output when one exists.
/// Use the first output's origin and scale for a layout gap.
/// Return `None` only when no output exists.
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
    // Extrapolate a point in a layout gap from the first output's origin and scale.
    // This is the least-wrong anchor available.
    first.map(|geo| geo.logical_to_global(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This box has 3840x2160 physical pixels at scale 1.5.
    /// It has 2560x1440 logical pixels.
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
    fn a_logical_point_lands_on_physical_pixels() {
        assert_eq!(PhysPoint { x: 969, y: 1310 }, DP1.logical_to_global(646.0, 873.0));
    }
}

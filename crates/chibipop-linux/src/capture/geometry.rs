//! This module maps a requested region to output boxes and buffer pixels.
//! It uses pure arithmetic and no Wayland calls.
//!
//! This helper maps regions across three coordinate spaces and keeps them distinct.
//!
//! - **global physical** — Core uses `PhysRect` here for every region and
//!   word box (`chibipop::text`). The cursor channel anchors this space
//!   with `cursor::outputs`. A cursor position and capture region then
//!   agree by construction. Mixed-scale layouts have no well-defined global
//!   physical space. This module uses the cursor's approximation, so
//!   output seams can overlap.
//! - **output-local logical** — `capture_output_region` uses this space.
//!   The wlr screencopy v3 protocol states, "The region is given in output
//!   logical coordinates". The compositor clips this box. This module
//!   clips first, so the compositor clip has no effect.
//! - **buffer pixels** — The compositor returns these pixels. It sizes
//!   the buffer from the logical box and its scale.
//!
//! **Fractional scales need bounded snapping.** wlroots truncates a scaled box.
//! `screencopy.c` assigns `box.x * output->scale` to an `int`.
//! Hyprland rounds the scaled box with `CBox::scale` and `CBox::round`.
//! The two rules can differ by one pixel. `Axis::new` computes a per-axis step
//! where `logical * scale` is an integer. When that step is no greater than
//! `MAX_SNAP`, this module snaps the logical box outward to that step. When
//! the step exceeds `MAX_SNAP`, it uses a unit step instead, so the result is
//! not guaranteed to match both rules. Integer arithmetic computes the crop
//! offset. No float enters this path.
//!
//! A region can extend past an output edge. The frame then contains pixels only
//! where an output exists. Other pixels stay black. Off-screen has no
//! content, and blank pixels cost nothing for one hover. A failed grab
//! costs every edge hover.

use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{PhysPoint, PhysRect};

/// `MAX_SNAP` bounds the computed snap step for each axis.
///
/// `Axis::new` computes each axis step as `logical / gcd(physical, logical)`.
/// If that step exceeds `MAX_SNAP`, the axis uses a unit step instead.
const MAX_SNAP: i64 = 64;

/// Return the physical height after the output transform.
fn physical_h(g: &OutputGeometry) -> i32 {
    if g.transform_swaps {
        g.mode_w
    } else {
        g.mode_h
    }
}

/// Return true when all sizes are known.
pub fn known(g: &OutputGeometry) -> bool {
    g.logical_w > 0 && g.logical_h > 0 && g.physical_w() > 0 && physical_h(g) > 0
}

/// Return one output's box in global physical space.
pub fn physical_box(g: &OutputGeometry) -> PhysRect {
    let (x, y) = g.physical_origin();
    PhysRect { x, y, w: g.physical_w(), h: physical_h(g) }
}

/// Return the overlap, or `None` when the boxes do not meet.
pub fn intersect(a: PhysRect, b: PhysRect) -> Option<PhysRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.w).min(b.x + b.w);
    let bottom = (a.y + a.h).min(b.y + b.h);
    if right > x && bottom > y {
        Some(PhysRect { x, y, w: right - x, h: bottom - y })
    } else {
        None
    }
}

/// Return the smallest rect that covers both inputs.
pub fn cover(a: PhysRect, b: PhysRect) -> PhysRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    PhysRect { x, y, w: right - x, h: bottom - y }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// One axis in the physical-to-logical conversion.
#[derive(Debug, Clone, Copy)]
struct Axis {
    /// Physical pixels across the full output.
    physical: i64,
    /// Logical units across the full output.
    logical: i64,
    /// Logical step used to snap the requested box.
    snap: i64,
}

impl Axis {
    fn new(physical: i32, logical: i32) -> Axis {
        let (physical, logical) = (i64::from(physical), i64::from(logical));
        let step = logical / gcd(physical, logical);
        Axis { physical, logical, snap: if step <= MAX_SNAP { step.max(1) } else { 1 } }
    }

    /// Return the largest snapped logical unit at or below `phys`.
    fn floor_to(&self, phys: i32) -> i64 {
        let logical = i64::from(phys) * self.logical / self.physical;
        (logical / self.snap) * self.snap
    }

    /// Return the smallest snapped logical unit at or above `phys`.
    fn ceil_to(&self, phys: i32) -> i64 {
        let logical =
            (i64::from(phys) * self.logical + self.physical - 1) / self.physical;
        ((logical + self.snap - 1) / self.snap) * self.snap
    }

    /// Return the physical pixel for a logical unit with integer arithmetic.
    fn to_physical(self, logical: i64) -> i32 {
        (logical * self.physical / self.logical) as i32
    }
}

/// One output's part of a requested region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Index in the caller's output list.
    pub output: usize,
    /// Arguments for `capture_output_region` in output-local logical space.
    pub logical: PhysRect,
    /// Requested pixels in output-local physical space.
    pub want: PhysRect,
    /// Output-local physical pixel at the buffer's (0,0) coordinate.
    pub origin: PhysPoint,
    /// Destination of the requested pixels in the frame.
    pub dest: PhysPoint,
}

/// This helper splits `region` across covered outputs in list order.
///
/// Return no pieces when the region touches no output. The caller then fails the
/// grab because it has no pixels to copy or invent.
pub fn split(geoms: &[OutputGeometry], region: PhysRect, out: &mut Vec<Piece>) {
    out.clear();
    if region.w <= 0 || region.h <= 0 {
        return;
    }
    for (output, g) in geoms.iter().enumerate() {
        if !known(g) {
            continue;
        }
        let ob = physical_box(g);
        let Some(hit) = intersect(region, ob) else { continue };
        let (ax, ay) = (Axis::new(g.physical_w(), g.logical_w), Axis::new(physical_h(g), g.logical_h));
        let local = hit.translated(-ob.x, -ob.y);
        // Expand outward to the snapped step. Clip the result as the compositor does.
        let lx = ax.floor_to(local.x).clamp(0, ax.logical);
        let ly = ay.floor_to(local.y).clamp(0, ay.logical);
        let rx = ax.ceil_to(local.x + local.w).clamp(lx, ax.logical);
        let ry = ay.ceil_to(local.y + local.h).clamp(ly, ay.logical);
        if rx == lx || ry == ly {
            continue;
        }
        out.push(Piece {
            output,
            logical: PhysRect {
                x: lx as i32,
                y: ly as i32,
                w: (rx - lx) as i32,
                h: (ry - ly) as i32,
            },
            want: local,
            origin: PhysPoint { x: ax.to_physical(lx), y: ay.to_physical(ly) },
            dest: PhysPoint { x: hit.x - region.x, y: hit.y - region.y },
        });
    }
}

/// Where a piece's pixels sit in the returned buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Source rect in buffer pixels.
    pub src: PhysRect,
    /// Destination origin in the frame.
    pub dest: PhysPoint,
}

/// Locate `piece.want` in the `bw x bh` buffer from the compositor.
///
/// Return the pixels that the buffer holds when it is smaller than requested.
/// Leave the other frame pixels black. Do not fail the hover.
pub fn cut(piece: &Piece, bw: i32, bh: i32) -> Option<Cut> {
    if bw <= 0 || bh <= 0 {
        return None;
    }
    let mut src = piece.want.translated(-piece.origin.x, -piece.origin.y);
    let mut dest = piece.dest;
    if src.x < 0 {
        dest.x -= src.x;
        src.w += src.x;
        src.x = 0;
    }
    if src.y < 0 {
        dest.y -= src.y;
        src.h += src.y;
        src.y = 0;
    }
    src.w = src.w.min(bw - src.x);
    src.h = src.h.min(bh - src.y);
    if src.w <= 0 || src.h <= 0 {
        return None;
    }
    Some(Cut { src, dest })
}

/// Return the output under `p`, or the nearest output.
///
/// Core uses this result to bound the tile layout. Return a bound instead of an
/// error because a wrong bound costs one tile and an error costs the hover.
/// With no output geometry, return a plausible box around `p` until the first
/// roundtrip provides geometry.
pub fn bounds_containing(geoms: &[OutputGeometry], p: PhysPoint) -> PhysRect {
    let mut nearest: Option<(f64, PhysRect)> = None;
    for g in geoms.iter().filter(|g| known(g)) {
        let b = physical_box(g);
        if b.contains(p) {
            return b;
        }
        let d = b.edge_distance_to(p);
        if nearest.is_none_or(|(best, _)| d < best) {
            nearest = Some((d, b));
        }
    }
    nearest
        .map(|(_, b)| b)
        .unwrap_or(PhysRect { x: p.x - 960, y: p.y - 540, w: 1920, h: 1080 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920x1080 output at scale 1 with logical origin `(x, y)`.
    fn plain(x: i32, y: i32) -> OutputGeometry {
        OutputGeometry {
            logical_x: x,
            logical_y: y,
            logical_w: 1920,
            logical_h: 1080,
            mode_w: 1920,
            mode_h: 1080,
            transform_swaps: false,
        }
    }

    /// A 3840x2160 panel at scale 1.5 with logical size 2560x1440.
    fn fractional(x: i32) -> OutputGeometry {
        OutputGeometry {
            logical_x: x,
            logical_y: 0,
            logical_w: 2560,
            logical_h: 1440,
            mode_w: 3840,
            mode_h: 2160,
            transform_swaps: false,
        }
    }

    /// A square output with physical size `mode` and logical size `logical`, at
    /// the origin.
    fn scaled(mode: i32, logical: i32) -> OutputGeometry {
        OutputGeometry {
            logical_x: 0,
            logical_y: 0,
            logical_w: logical,
            logical_h: logical,
            mode_w: mode,
            mode_h: mode,
            transform_swaps: false,
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PhysRect {
        PhysRect { x, y, w, h }
    }

    fn one(geoms: &[OutputGeometry], region: PhysRect) -> Piece {
        let mut out = Vec::new();
        split(geoms, region, &mut out);
        assert_eq!(out.len(), 1, "expected one piece for {region:?}");
        out[0]
    }

    /// Return the box that a compositor such as wlroots truncates.
    fn wlroots_buffer(piece: &Piece, g: &OutputGeometry) -> (i32, i32, i32, i32) {
        let s = g.scale();
        (
            (f64::from(piece.logical.x) * s) as i32,
            (f64::from(piece.logical.y) * s) as i32,
            (f64::from(piece.logical.w) * s) as i32,
            (f64::from(piece.logical.h) * s) as i32,
        )
    }

    /// Return the box that a compositor such as Hyprland rounds.
    fn hyprland_buffer(piece: &Piece, g: &OutputGeometry) -> (i32, i32, i32, i32) {
        let s = g.scale();
        (
            (f64::from(piece.logical.x) * s).round() as i32,
            (f64::from(piece.logical.y) * s).round() as i32,
            (f64::from(piece.logical.w) * s).round() as i32,
            (f64::from(piece.logical.h) * s).round() as i32,
        )
    }

    #[test]
    fn scale_one_asks_for_the_region_itself() {
        let piece = one(&[plain(0, 0)], rect(100, 50, 640, 360));
        assert_eq!(piece.logical, rect(100, 50, 640, 360));
        assert_eq!(piece.want, rect(100, 50, 640, 360));
        assert_eq!(piece.origin, PhysPoint { x: 100, y: 50 });
        assert_eq!(piece.dest, PhysPoint { x: 0, y: 0 });
        assert_eq!(cut(&piece, 640, 360).unwrap().src, rect(0, 0, 640, 360));
    }

    #[test]
    fn fractional_scale_covers_the_physical_rect() {
        let piece = one(&[fractional(0)], rect(100, 50, 640, 360));
        // At 1.5x, 100 physical pixels equal 66.67 logical pixels. Snap the
        // left edge down to 66 and the right edge up from 493.33 to 494.
        assert_eq!(piece.logical, rect(66, 32, 428, 242));
        assert_eq!(piece.origin, PhysPoint { x: 99, y: 48 });
        let cut = cut(&piece, 642, 363).unwrap();
        assert_eq!(cut.src, rect(1, 2, 640, 360));
    }

    /// The snap operation makes both compositor rules produce the same buffer.
    /// One crop then works for both compositors.
    #[test]
    fn truncating_and_rounding_compositors_agree_on_every_offset() {
        for g in [fractional(0), scaled(2400, 1920), scaled(3840, 2880), plain(0, 0)] {
            for x in 0..400 {
                let piece = one(&[g], rect(x, x / 3, 300, 200));
                let wlr = wlroots_buffer(&piece, &g);
                let hypr = hyprland_buffer(&piece, &g);
                assert_eq!(wlr, hypr, "scale {} at x={x} splits the two conventions", g.scale());
                // Integer arithmetic computes the same offset as both compositors.
                assert_eq!((piece.origin.x, piece.origin.y), (wlr.0, wlr.1));
                let cut = cut(&piece, wlr.2, wlr.3)
                    .unwrap_or_else(|| panic!("scale {} at x={x} lost its cut", g.scale()));
                assert_eq!(cut.src.w, 300, "scale {} at x={x} lost columns", g.scale());
                assert_eq!(cut.src.h, 200, "scale {} at x={x} lost rows", g.scale());
                assert!(cut.src.x + cut.src.w <= wlr.2, "past the buffer at x={x}");
                assert!(cut.src.y + cut.src.h <= wlr.3, "past the buffer at x={x}");
            }
        }
    }

    /// A scale without a small snap step must stay within buffer bounds.
    #[test]
    fn an_awkward_scale_still_crops_inside_the_buffer() {
        let g = scaled(1920, 1234);
        for x in 0..300 {
            let piece = one(&[g], rect(x, 0, 200, 100));
            let (_, _, bw, bh) = wlroots_buffer(&piece, &g);
            let cut = cut(&piece, bw, bh).unwrap_or_else(|| panic!("x={x} lost its cut"));
            assert!(cut.src.x + cut.src.w <= bw, "x={x} runs past the buffer");
            assert!(cut.src.y + cut.src.h <= bh, "x={x} runs past the buffer");
        }
    }

    #[test]
    fn a_region_off_the_right_edge_keeps_the_part_that_exists() {
        let piece = one(&[plain(0, 0)], rect(1900, 100, 100, 50));
        assert_eq!(piece.want, rect(1900, 100, 20, 50));
        assert_eq!(piece.dest, PhysPoint { x: 0, y: 0 });
        assert_eq!(cut(&piece, 20, 50).unwrap().src, rect(0, 0, 20, 50));
    }

    #[test]
    fn a_region_off_the_left_edge_lands_at_a_dest_offset() {
        let piece = one(&[plain(0, 0)], rect(-30, 100, 100, 50));
        assert_eq!(piece.want, rect(0, 100, 70, 50));
        // The first 30 frame columns have no output. Leave these columns black.
        assert_eq!(piece.dest, PhysPoint { x: 30, y: 0 });
    }

    #[test]
    fn a_straddling_region_splits_over_both_outputs() {
        let geoms = [plain(0, 0), plain(1920, 0)];
        let mut out = Vec::new();
        split(&geoms, rect(1900, 10, 100, 20), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].output, 0);
        assert_eq!(out[0].logical, rect(1900, 10, 20, 20));
        assert_eq!(out[0].dest, PhysPoint { x: 0, y: 0 });
        assert_eq!(out[1].output, 1);
        // Output-local: the second output has origin 0.
        assert_eq!(out[1].logical, rect(0, 10, 80, 20));
        assert_eq!(out[1].dest, PhysPoint { x: 20, y: 0 });
    }

    /// Mixed-scale layouts have no defined global physical space.
    /// Use the cursor channel's anchor so a hover captures the cursor's location.
    /// Output boxes can overlap. The later output remains last.
    #[test]
    fn mixed_scales_anchor_the_way_the_cursor_channel_anchors() {
        let geoms = [fractional(0), plain(2560, 0)];
        assert_eq!(physical_box(&geoms[0]), rect(0, 0, 3840, 2160));
        assert_eq!(physical_box(&geoms[1]), rect(2560, 0, 1920, 1080));
        let mut out = Vec::new();
        split(&geoms, rect(3830, 0, 40, 40), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].want, rect(3830, 0, 10, 40));
        assert_eq!(out[1].want, rect(1270, 0, 40, 40));
    }

    #[test]
    fn a_region_on_no_output_splits_into_nothing() {
        let mut out = Vec::new();
        split(&[plain(0, 0)], rect(5000, 5000, 100, 100), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn geometry_that_has_not_arrived_is_skipped() {
        let mut out = Vec::new();
        split(&[OutputGeometry::default()], rect(0, 0, 10, 10), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn degenerate_regions_split_into_nothing() {
        let mut out = Vec::new();
        split(&[plain(0, 0)], rect(10, 10, 0, 40), &mut out);
        assert!(out.is_empty());
        split(&[plain(0, 0)], rect(10, 10, 40, -1), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_transformed_output_swaps_its_physical_box() {
        let g = OutputGeometry {
            logical_x: 0,
            logical_y: 0,
            logical_w: 1080,
            logical_h: 1920,
            mode_w: 1920,
            mode_h: 1080,
            transform_swaps: true,
        };
        assert_eq!(physical_box(&g), rect(0, 0, 1080, 1920));
    }

    #[test]
    fn a_short_buffer_yields_what_it_holds() {
        let piece = one(&[plain(0, 0)], rect(0, 0, 100, 100));
        assert_eq!(cut(&piece, 100, 40).unwrap().src, rect(0, 0, 100, 40));
        assert_eq!(cut(&piece, 0, 40), None);
    }

    #[test]
    fn bounds_prefer_the_output_under_the_point() {
        let geoms = [plain(0, 0), plain(1920, 0)];
        assert_eq!(
            bounds_containing(&geoms, PhysPoint { x: 2000, y: 5 }),
            rect(1920, 0, 1920, 1080)
        );
    }

    #[test]
    fn bounds_fall_back_to_the_nearest_output() {
        let geoms = [plain(0, 0), plain(4000, 0)];
        assert_eq!(
            bounds_containing(&geoms, PhysPoint { x: 3900, y: 5 }),
            rect(4000, 0, 1920, 1080)
        );
    }

    #[test]
    fn bounds_without_geometry_still_answer() {
        let b = bounds_containing(&[], PhysPoint { x: 100, y: 100 });
        assert!(b.contains(PhysPoint { x: 100, y: 100 }));
    }

    #[test]
    fn cover_spans_both_rects() {
        assert_eq!(cover(rect(0, 0, 10, 10), rect(20, 5, 10, 10)), rect(0, 0, 30, 15));
    }
}

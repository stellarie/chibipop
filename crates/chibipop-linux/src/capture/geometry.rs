//! Which output a requested rect lives on, which
//! `capture_output_region` box to ask for, and which pixels to cut out
//! of the buffer that comes back. Pure arithmetic, no Wayland.
//!
//! Three coordinate spaces meet here, and confusing them is the whole
//! risk:
//!
//! - **global physical** — core's `PhysRect`: the space every region
//!   and every word box is in (`chibipop::text`). Anchored exactly as
//!   the cursor channel anchors it (`cursor::outputs`), so a cursor
//!   position and a capture region agree by construction. Mixed-scale
//!   layouts have no well-defined global physical space; agreeing with
//!   the cursor matters more than being tidy, so the same
//!   approximation is used here and the seams can overlap.
//! - **output-local logical** — what `capture_output_region` takes
//!   ("The region is given in output logical coordinates", wlr
//!   screencopy v3) and what the compositor clips against. We clip
//!   first, so its clip is always a no-op and the box we assume is the
//!   box it used.
//! - **buffer pixels** — what comes back, sized by the compositor by
//!   scaling the logical box it was given.
//!
//! **Fractional scales are made exact rather than guessed.** The two
//! implementations that matter disagree on rounding: wlroots truncates
//! the scaled box (`screencopy.c` assigns `box.x * output->scale` into
//! an `int`), Hyprland rounds it (`CBox::scale` then `CBox::round`). A
//! backend that assumed either would be a pixel out on the other. So
//! the requested logical box is snapped outward to the step where
//! `logical * scale` is exactly an integer - two logical pixels at
//! 1.5x, four at 1.25x - and then truncation and rounding are the same
//! number. The crop offset is integer arithmetic from there, with no
//! float in the path at all.
//!
//! A region reaching past its output (core's pass-1 box around a
//! cursor near an edge) yields pixels only where an output actually
//! is; the rest of the frame stays black. Off-screen has no content,
//! and blank pixels cost one hover nothing, where failing the grab
//! would cost every edge hover.

use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{PhysPoint, PhysRect};

/// Widest logical step still worth snapping to.
///
/// The step is `logical / gcd(physical, logical)`, which is small for
/// every scale a compositor actually offers, because compositors pick
/// scales whose logical size is a whole number: 2 at 1.5x, 4 at 1.25x,
/// 3 at 1.333x. A layout past this bound gets floored offsets and may
/// sit one pixel off on a rounding compositor - visible to nobody, and
/// never out of bounds, because the cut is clamped.
const MAX_SNAP: i64 = 64;

/// Physical height after the output transform.
fn physical_h(g: &OutputGeometry) -> i32 {
    if g.transform_swaps {
        g.mode_w
    } else {
        g.mode_h
    }
}

/// True once every size has arrived.
pub fn known(g: &OutputGeometry) -> bool {
    g.logical_w > 0 && g.logical_h > 0 && g.physical_w() > 0 && physical_h(g) > 0
}

/// One output's box in the global physical space.
pub fn physical_box(g: &OutputGeometry) -> PhysRect {
    let (x, y) = g.physical_origin();
    PhysRect { x, y, w: g.physical_w(), h: physical_h(g) }
}

/// Overlap, or `None` when they miss.
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

/// Smallest rect covering both.
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

/// One axis of the physical-to-logical conversion.
#[derive(Debug, Clone, Copy)]
struct Axis {
    /// Physical pixels across the whole output.
    physical: i64,
    /// Logical units across the whole output.
    logical: i64,
    /// Logical step at which `logical * scale` is a whole number.
    snap: i64,
}

impl Axis {
    fn new(physical: i32, logical: i32) -> Axis {
        let (physical, logical) = (i64::from(physical), i64::from(logical));
        let step = logical / gcd(physical, logical);
        Axis { physical, logical, snap: if step <= MAX_SNAP { step.max(1) } else { 1 } }
    }

    /// Largest snapped logical unit at or below `phys`.
    fn floor_to(&self, phys: i32) -> i64 {
        let logical = i64::from(phys) * self.logical / self.physical;
        (logical / self.snap) * self.snap
    }

    /// Smallest snapped logical unit at or above `phys`.
    fn ceil_to(&self, phys: i32) -> i64 {
        let logical =
            (i64::from(phys) * self.logical + self.physical - 1) / self.physical;
        ((logical + self.snap - 1) / self.snap) * self.snap
    }

    /// The physical pixel a snapped logical unit sits on. Exact
    /// whenever the unit is snapped, which is what makes truncating and
    /// rounding compositors agree.
    fn to_physical(self, logical: i64) -> i32 {
        (logical * self.physical / self.logical) as i32
    }
}

/// One output's share of a requested region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Index into the caller's output list.
    pub output: usize,
    /// `capture_output_region` arguments: output-local logical.
    pub logical: PhysRect,
    /// The pixels wanted, output-local physical.
    pub want: PhysRect,
    /// The output-local physical pixel the buffer's (0,0) holds.
    pub origin: PhysPoint,
    /// Where the wanted pixels land in the frame.
    pub dest: PhysPoint,
}

/// Split `region` over the outputs it covers, in list order.
///
/// Empty when it touches no output at all: there is nothing to copy
/// and nothing to invent, so the caller fails the grab.
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
        // Outward to the snapped step, then clipped exactly as the
        // compositor would clip - so it never has to.
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

/// Where a piece's pixels sit in the buffer that came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Source rect, buffer pixels.
    pub src: PhysRect,
    /// Destination origin in the frame.
    pub dest: PhysPoint,
}

/// Locate `piece.want` in the `bw x bh` buffer the compositor sent.
///
/// A buffer holding less than was asked for still yields the pixels it
/// does hold; the rest of the frame stays black rather than failing a
/// hover.
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

/// The output under `p`, else the nearest one.
///
/// Bounds core's tiling, so an answer always beats an error: a wrong
/// bound costs a tile, a failure costs the hover. With no output
/// geometry at all, a plausible box around `p` keeps tiling honest
/// until the first roundtrip lands.
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

    /// A 1920x1080 output at scale 1, logical origin `(x, y)`.
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

    /// A 3840x2160 panel at scale 1.5: logical 2560x1440.
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

    /// `mode` physical over `logical`, at the origin.
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

    /// What a truncating compositor (wlroots) makes of a logical box.
    fn wlroots_buffer(piece: &Piece, g: &OutputGeometry) -> (i32, i32, i32, i32) {
        let s = g.scale();
        (
            (f64::from(piece.logical.x) * s) as i32,
            (f64::from(piece.logical.y) * s) as i32,
            (f64::from(piece.logical.w) * s) as i32,
            (f64::from(piece.logical.h) * s) as i32,
        )
    }

    /// What a rounding compositor (Hyprland) makes of it.
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
        // 100px is 66.67 logical, snapped down to the even 66; the right
        // edge 740 is 493.33, snapped up to 494.
        assert_eq!(piece.logical, rect(66, 32, 428, 242));
        assert_eq!(piece.origin, PhysPoint { x: 99, y: 48 });
        let cut = cut(&piece, 642, 363).unwrap();
        assert_eq!(cut.src, rect(1, 2, 640, 360));
    }

    /// The point of the snapping: both rounding conventions produce the
    /// same buffer, so one crop is right on both compositors.
    #[test]
    fn truncating_and_rounding_compositors_agree_on_every_offset() {
        for g in [fractional(0), scaled(2400, 1920), scaled(3840, 2880), plain(0, 0)] {
            for x in 0..400 {
                let piece = one(&[g], rect(x, x / 3, 300, 200));
                let wlr = wlroots_buffer(&piece, &g);
                let hypr = hyprland_buffer(&piece, &g);
                assert_eq!(wlr, hypr, "scale {} at x={x} splits the two conventions", g.scale());
                // And the offset this module computed is that same
                // number, from integer arithmetic alone.
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

    /// A scale with no small snapping step must still stay in bounds.
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
        // The first 30 columns of the frame have no output behind them
        // and stay black.
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
        // Output-local: the second output's own origin is 0.
        assert_eq!(out[1].logical, rect(0, 10, 80, 20));
        assert_eq!(out[1].dest, PhysPoint { x: 20, y: 0 });
    }

    /// Mixed-scale layouts have no well-defined global physical space.
    /// The cursor channel's anchoring wins - a hover must capture where
    /// the cursor says it is - and the documented price is that boxes
    /// can overlap, later output last.
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

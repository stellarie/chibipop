//! Coordinate types for chibipop.
//!
//! Every coordinate in this project is a physical pixel in virtual-desktop
//! space. These types carry arithmetic only — the OS queries that produce them
//! live in the Windows-facing modules and pass plain data in, so this file
//! compiles and tests on any platform.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl PhysRect {
    /// Inclusive of the top-left edge, exclusive of the bottom-right — the
    /// usual half-open pixel convention, so adjacent rects never both claim
    /// the same pixel.
    pub fn contains(&self, p: PhysPoint) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    /// Shortest distance from `p` to this rect's boundary; 0.0 when inside.
    pub fn edge_distance_to(&self, p: PhysPoint) -> f64 {
        let dx = (self.x - p.x).max(0).max(p.x - (self.x + self.w - 1));
        let dy = (self.y - p.y).max(0).max(p.y - (self.y + self.h - 1));
        ((dx as f64).powi(2) + (dy as f64).powi(2)).sqrt()
    }

    pub fn center(&self) -> PhysPoint {
        PhysPoint { x: self.x + self.w / 2, y: self.y + self.h / 2 }
    }

    pub fn translated(&self, dx: i32, dy: i32) -> PhysRect {
        PhysRect { x: self.x + dx, y: self.y + dy, w: self.w, h: self.h }
    }

    /// Grown by `dx` on each side horizontally and `dy` vertically.
    ///
    /// Negative amounts shrink, and may produce a non-positive extent; callers
    /// that treat such a rect as containing nothing (see [`in_sticky`]) are
    /// already correct without a clamp here.
    pub fn inflated(&self, dx: i32, dy: i32) -> PhysRect {
        PhysRect {
            x: self.x - dx,
            y: self.y - dy,
            w: self.w + 2 * dx,
            h: self.h + 2 * dy,
        }
    }

    /// Integer division of every field — used to map coordinates back out of
    /// upscaled-image space.
    ///
    /// Calling convention: this divides image-local (non-negative)
    /// coordinates and is applied *before* `translated` moves the rect into
    /// (possibly negative) virtual-desktop space, so in the intended
    /// composition it never sees a negative input. Division truncates toward
    /// zero (Rust's `/`), not floor — e.g. `-11 / 2 == -5`. That only matters
    /// if this convention is violated and a negative value is passed in
    /// directly; see the tests below for the pinned behaviour.
    pub fn scaled_down(&self, factor: i32) -> PhysRect {
        PhysRect {
            x: self.x / factor,
            y: self.y / factor,
            w: self.w / factor,
            h: self.h / factor,
        }
    }
}

/// Which stage of text acquisition a drawn rectangle came from. Drives the
/// overlay's colour; the drawing layer needs no other knowledge of the OCR
/// pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// The cursor-centred box pass 1 captures to find the hovered word.
    Pass1,
    /// One forward tile.
    Tile,
    /// The resolved word's own box.
    Anchor,
    /// The characters the popup is defining - not a capture at all, but the
    /// decision the captures exist to reach. The everyday case, and the only
    /// kind drawn when `[debug] show_scan_region` is off.
    Match,
}

/// One rectangle the overlay draws, tagged with where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanRect {
    pub rect: PhysRect,
    pub kind: ScanKind,
}

/// What one hover's overlay is allowed to show, resolved from the two
/// independent settings that feed it.
///
/// They are genuinely independent: `[popup] highlight_match` is the everyday
/// answer to "is it defining the word I am pointing at?", while `[debug]
/// show_scan_region` is the diagnostic view of where chibipop looked. Keeping
/// them in one value is what makes "the capture boxes are **not** drawn when
/// only the highlight is on" a checkable statement instead of a convention -
/// four overlapping rectangles on every hover is precisely what the highlight
/// exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanDisplay {
    /// `Pass1`, `Tile` and `Anchor`. Never collected when false, so "off" is
    /// inert rather than collected-and-filtered.
    pub captures: bool,
    /// `Match`.
    pub highlight: bool,
}

impl ScanDisplay {
    /// Whether an overlay window is needed at all. Both off means no window is
    /// ever created.
    ///
    /// Built by struct literal on purpose - there is no `new`, because two
    /// adjacent `bool` parameters are a silent swap waiting to happen and the
    /// field names are the only thing that tells the two settings apart.
    pub fn any(self) -> bool {
        self.captures || self.highlight
    }
}

/// The three rectangles that keep a popup on screen: the hovered character,
/// the popup itself, and the **bridge** across the gap between them.
///
/// Returned in that order — `[anchor, popup, bridge]`.
///
/// **Three rectangles, deliberately not their bounding box.** The popup is
/// flush with the anchor's left edge and up to `POPUP_MAX_WIDTH` (420px) wide,
/// while the anchor is one glyph (~26px). A bounding box would therefore also
/// cover ~400px of screen *beside* the hovered character, at that character's
/// own height — so scanning sideways along a line of text would hold the popup
/// on the previous word and the next word could never be read. The bridge is
/// only the gap tall, so brushing it on the way to the line below holds
/// nothing.
///
/// The bridge is derived from whichever rect is upper, because [`place_popup`]
/// flips the popup above the anchor near a monitor's bottom edge.
pub fn sticky_region(anchor: PhysRect, popup: PhysRect) -> [PhysRect; 3] {
    [anchor, popup, bridge_between(anchor, popup)]
}

/// The gap band between two rects, spanning their combined x-extent.
///
/// Zero or negative height when they touch or overlap; [`in_sticky`] treats
/// that as containing nothing, and the two rects are adjacent in that case so
/// nothing is lost.
fn bridge_between(a: PhysRect, b: PhysRect) -> PhysRect {
    let (upper, lower) = if a.y <= b.y { (a, b) } else { (b, a) };
    let top = upper.y + upper.h;
    let left = a.x.min(b.x);
    let right = (a.x + a.w).max(b.x + b.w);
    PhysRect { x: left, y: top, w: right - left, h: lower.y - top }
}

/// Whether `p` is on the hovered word, on its popup, or in the gap between.
///
/// While this is true the application dispatches no trigger at all, so the
/// popup stays exactly as it is (spec D3).
///
/// **What this guarantees, and what it does not** (spec D2a): the vertical path
/// from the anchor's centre into the popup is entirely covered, as is any
/// approach steeper than roughly 45°. A *shallower* diagonal leaves the region
/// by the anchor's side edge before reaching the bridge — landing on the
/// neighbouring character, which is then correctly resolved as the word the
/// cursor actually moved to. Covering that strip instead is precisely what
/// would break sideways scanning, so the two cannot both hold.
pub fn in_sticky(p: PhysPoint, anchor: PhysRect, popup: PhysRect) -> bool {
    sticky_region(anchor, popup)
        .iter()
        .any(|r| r.w > 0 && r.h > 0 && r.contains(p))
}

/// The overlay window's bounds, and every rectangle translated into that
/// window's local coordinates.
///
/// `None` for empty input - the caller must then show no window at all rather
/// than an empty one.
pub fn overlay_layout(rects: &[ScanRect]) -> Option<(PhysRect, Vec<ScanRect>)> {
    let first = rects.first()?;
    let mut left = first.rect.x;
    let mut top = first.rect.y;
    let mut right = first.rect.x + first.rect.w;
    let mut bottom = first.rect.y + first.rect.h;

    for r in rects.iter().skip(1) {
        left = left.min(r.rect.x);
        top = top.min(r.rect.y);
        right = right.max(r.rect.x + r.rect.w);
        bottom = bottom.max(r.rect.y + r.rect.h);
    }

    let bounds = PhysRect { x: left, y: top, w: right - left, h: bottom - top };
    let local = rects
        .iter()
        .map(|r| ScanRect { rect: r.rect.translated(-left, -top), kind: r.kind })
        .collect();
    Some((bounds, local))
}

/// A rectangle's interior once a border `thickness` wide is removed.
///
/// `None` when nothing is left - a rectangle no thicker than two borders is
/// all border. OCR word boxes really do get this small (a measured box on the
/// user's screen is 17x3), so this case is reached in practice, and computing
/// it by subtraction alone would produce a negative extent.
pub fn inset(rect: PhysRect, thickness: i32) -> Option<PhysRect> {
    let w = rect.w - 2 * thickness;
    let h = rect.h - 2 * thickness;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(PhysRect { x: rect.x + thickness, y: rect.y + thickness, w, h })
}

/// Places a `size`-shaped popup relative to `anchor`, so it never covers
/// `anchor` and never crosses `monitor`'s edges.
///
/// Default position (spec §4.2): flush with the anchor's left edge, `gap`
/// pixels below its bottom edge - "below and to the right". Each axis flips
/// independently to the anchor's *other* side when the default would cross
/// that axis's monitor edge:
/// - X carries no gap in either direction - flush-left with the anchor
///   normally, flush-right with it when flipped - because X is never what
///   keeps the popup off the anchor; Y does that job unconditionally below.
/// - Y always carries `gap`, on both the below and the above placement,
///   because Y is the axis that actually separates the popup from the
///   character being read.
///
/// That split is what makes the anchor provably uncovered on every call,
/// not just the cases this module happens to test: whichever branch Y takes,
/// the popup's Y-span lands entirely before or entirely after the anchor's
/// (given `gap >= 0`), and a 2D overlap needs both axes to overlap - so X
/// landing anywhere on-screen can never re-introduce it.
///
/// The result is finally clamped into `monitor`. That clamp is a no-op
/// whenever a flip alone already fits (true for every case exercised
/// below); it exists because the secondary monitor here is only 1080px
/// wide, so a popup that grows wide with many collapsed rows could
/// plausibly need it even after flipping.
///
/// The clamp is also the one place the anchor-uncovered proof above can
/// break: if neither the space above nor below the anchor is large enough
/// to hold `gap + h`, the Y clamp pulls the popup back toward the anchor
/// and it can genuinely end up covering it. This was checked, not just
/// reasoned about: an ad hoc sweep (anchor swept across three monitors incl.
/// non-zero and negative origins, both orientations, sizes at and above the
/// M3-D4 45%-of-monitor-height contract) found overlap in exactly the
/// bucket that broke that contract - `h` at 74% of a 1080px-tall monitor -
/// and nowhere else, including the identical 800px height at 41.7% of the
/// 1920px-tall portrait monitor. So: any anchor, any monitor origin
/// (incl. negative), either orientation, is safe *provided* the caller caps
/// `size` to the M3-D4 45%-of-monitor-height contract first (`present.rs`'s
/// height cap exists for exactly this) - which is a fine bar for a real
/// display (the cap can't force this failure until the monitor is under
/// roughly 240px tall). That call belongs to the code that already knows
/// the clamp happened - Task 5's `measure()` - not here.
pub fn place_popup(anchor: PhysRect, size: (i32, i32), monitor: PhysRect, gap: i32) -> PhysRect {
    let (w, h) = size;

    let mut x = anchor.x;
    if x + w > monitor.x + monitor.w {
        x = anchor.x + anchor.w - w;
    }
    x = x.max(monitor.x).min(monitor.x + monitor.w - w);

    let mut y = anchor.y + anchor.h + gap;
    if y + h > monitor.y + monitor.h {
        y = anchor.y - gap - h;
    }
    y = y.max(monitor.y).min(monitor.y + monitor.h - h);

    PhysRect { x, y, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> PhysRect { PhysRect { x, y, w, h } }
    fn p(x: i32, y: i32) -> PhysPoint { PhysPoint { x, y } }
    fn sr(x: i32, y: i32, w: i32, h: i32, kind: ScanKind) -> ScanRect {
        ScanRect { rect: PhysRect { x, y, w, h }, kind }
    }

    #[test]
    fn contains_is_inclusive_of_the_top_left_edge() {
        assert!(r(10, 10, 20, 20).contains(p(10, 10)));
    }

    #[test]
    fn contains_is_exclusive_of_the_bottom_right_edge() {
        let rect = r(10, 10, 20, 20);
        assert!(rect.contains(p(29, 29)));
        assert!(!rect.contains(p(30, 30)));
    }

    #[test]
    fn contains_rejects_points_outside() {
        let rect = r(10, 10, 20, 20);
        assert!(!rect.contains(p(9, 15)));
        assert!(!rect.contains(p(15, 9)));
    }

    #[test]
    fn edge_distance_is_zero_inside() {
        assert_eq!(0.0, r(10, 10, 20, 20).edge_distance_to(p(15, 15)));
    }

    #[test]
    fn edge_distance_is_orthogonal_when_aligned() {
        // 5px to the left of the rect, vertically within it.
        assert_eq!(5.0, r(10, 10, 20, 20).edge_distance_to(p(5, 15)));
    }

    #[test]
    fn edge_distance_is_diagonal_at_a_corner() {
        // 3 left, 4 above the top-left corner -> 5 by Pythagoras.
        assert_eq!(5.0, r(10, 10, 20, 20).edge_distance_to(p(7, 6)));
    }

    #[test]
    fn edge_distance_is_orthogonal_to_the_right() {
        // Occupied span is x,y in [10, 29]. 6px to the right, vertically within it.
        assert_eq!(6.0, r(10, 10, 20, 20).edge_distance_to(p(35, 15)));
    }

    #[test]
    fn edge_distance_is_orthogonal_below() {
        // 6px below the rect, horizontally within it.
        assert_eq!(6.0, r(10, 10, 20, 20).edge_distance_to(p(15, 35)));
    }

    #[test]
    fn edge_distance_is_diagonal_off_the_bottom_right_corner() {
        // 3 right of the far edge (32 - 29), 4 below the far edge (33 - 29) -> 5 by Pythagoras.
        assert_eq!(5.0, r(10, 10, 20, 20).edge_distance_to(p(32, 33)));
    }

    #[test]
    fn edge_distance_degrades_correctly_when_near_and_far_edges_coincide() {
        // A 1x1 rect's near and far edge are the same pixel (10). 3 right, 4 below -> 5.
        // Proves the far-edge term (`self.x + self.w - 1`) still works when it
        // collapses onto the near edge instead of silently reducing to it.
        assert_eq!(5.0, r(10, 10, 1, 1).edge_distance_to(p(13, 14)));
    }

    #[test]
    fn center_rounds_down() {
        assert_eq!(p(20, 20), r(10, 10, 21, 21).center());
    }

    #[test]
    fn translated_moves_origin_only() {
        assert_eq!(r(15, 5, 20, 20), r(10, 10, 20, 20).translated(5, -5));
    }

    #[test]
    fn scaled_down_divides_every_field() {
        assert_eq!(r(5, 10, 15, 20), r(10, 20, 30, 40).scaled_down(2));
    }

    #[test]
    fn scaled_down_truncates_toward_zero_for_odd_values() {
        // 11/2 = 5.5, 21/2 = 10.5, 31/2 = 15.5, 41/2 = 20.5 -- truncation
        // drops the fraction, distinguishing it from round-to-nearest (which
        // would take w:31 to 16, not 15).
        assert_eq!(r(5, 10, 15, 20), r(11, 21, 31, 41).scaled_down(2));
    }

    #[test]
    fn scaled_down_truncates_toward_zero_for_negative_origin() {
        // -11/2 = -5.5, -21/2 = -10.5 -- Rust's `/` truncates toward zero, so
        // these land on -5 and -10, not floor's -6 and -11.
        assert_eq!(r(-5, -10, 15, 20), r(-11, -21, 30, 40).scaled_down(2));
    }

    #[test]
    fn popup_sits_below_right_of_the_anchor_when_it_fits() {
        let mon = r(0, 0, 1920, 1080);
        let got = place_popup(r(100, 100, 20, 20), (300, 200), mon, 12);
        assert_eq!(100, got.x);
        assert_eq!(132, got.y, "anchor bottom 120 plus a 12px gap");
    }

    #[test]
    fn popup_flips_left_at_the_right_edge() {
        let mon = r(0, 0, 1920, 1080);
        let got = place_popup(r(1800, 100, 20, 20), (300, 200), mon, 12);
        assert!(got.x + got.w <= mon.x + mon.w, "must not cross the right edge");
        assert!(got.x < 1800, "must flip to the anchor's left");
    }

    #[test]
    fn popup_flips_up_at_the_bottom_edge() {
        let mon = r(0, 0, 1920, 1080);
        let got = place_popup(r(100, 1000, 20, 20), (300, 200), mon, 12);
        assert!(got.y + got.h <= mon.y + mon.h);
        assert!(got.y < 1000, "must flip above the anchor");
    }

    #[test]
    fn popup_flips_both_axes_in_the_corner() {
        let mon = r(0, 0, 1920, 1080);
        let got = place_popup(r(1850, 1040, 20, 20), (300, 200), mon, 12);
        assert!(got.x + got.w <= mon.x + mon.w);
        assert!(got.y + got.h <= mon.y + mon.h);
    }

    #[test]
    fn popup_respects_a_monitor_with_a_non_zero_origin() {
        // The secondary monitor on this machine starts at x=2560.
        let mon = r(2560, 0, 1080, 1920);
        let got = place_popup(r(3500, 100, 20, 20), (300, 200), mon, 12);
        assert!(got.x >= mon.x, "must not spill onto the primary monitor");
        assert!(got.x + got.w <= mon.x + mon.w);
    }

    #[test]
    fn popup_never_covers_the_anchor() {
        let mon = r(0, 0, 1920, 1080);
        for ax in [100, 900, 1800] {
            for ay in [100, 500, 1040] {
                let anchor = r(ax, ay, 20, 20);
                let got = place_popup(anchor, (300, 200), mon, 12);
                let overlaps = got.x < anchor.x + anchor.w
                    && anchor.x < got.x + got.w
                    && got.y < anchor.y + anchor.h
                    && anchor.y < got.y + got.h;
                assert!(!overlaps, "popup covered the anchor at ({ax},{ay})");
            }
        }
    }

    #[test]
    fn layout_of_nothing_is_none() {
        assert!(overlay_layout(&[]).is_none());
    }

    #[test]
    fn a_single_rect_bounds_itself_and_sits_at_the_origin() {
        let (bounds, local) = overlay_layout(&[sr(100, 200, 50, 20, ScanKind::Pass1)]).unwrap();
        assert_eq!(PhysRect { x: 100, y: 200, w: 50, h: 20 }, bounds);
        assert_eq!(PhysRect { x: 0, y: 0, w: 50, h: 20 }, local[0].rect);
        assert_eq!(ScanKind::Pass1, local[0].kind);
    }

    #[test]
    fn bounds_span_every_rect_and_locals_are_relative_to_it() {
        let (bounds, local) = overlay_layout(&[
            sr(100, 200, 50, 20, ScanKind::Pass1),
            sr(400, 180, 50, 60, ScanKind::Tile),
        ])
        .unwrap();
        assert_eq!(PhysRect { x: 100, y: 180, w: 350, h: 60 }, bounds);
        assert_eq!(PhysRect { x: 0, y: 20, w: 50, h: 20 }, local[0].rect);
        assert_eq!(ScanKind::Pass1, local[0].kind);
        assert_eq!(PhysRect { x: 300, y: 0, w: 50, h: 60 }, local[1].rect);
        assert_eq!(ScanKind::Tile, local[1].kind);
    }

    /// Spec D2, the whole point of the setting being separate: with only the
    /// highlight on, a hover must draw ONE box. Collecting the capture kinds
    /// would put four overlapping rectangles on the default path.
    #[test]
    fn the_capture_kinds_are_not_shown_when_only_the_highlight_is_on() {
        let d = ScanDisplay { captures: false, highlight: true };
        assert!(!d.captures);
        assert!(d.highlight);
        assert!(d.any(), "a window is still needed - the highlight draws in it");
    }

    #[test]
    fn the_debug_view_shows_the_captures_as_well() {
        let d = ScanDisplay { captures: true, highlight: true };
        assert!(d.captures);
        assert!(d.highlight);
    }

    /// Both off must create no window at all - the M3 "inert, not merely
    /// hidden" rule survives the highlight being added beside it.
    #[test]
    fn both_settings_off_needs_no_overlay_window() {
        assert!(!ScanDisplay { captures: false, highlight: false }.any());
    }

    #[test]
    fn the_debug_view_alone_still_shows_the_captures() {
        let d = ScanDisplay { captures: true, highlight: false };
        assert!(d.captures);
        assert!(!d.highlight);
        assert!(d.any());
    }

    /// Tiles routinely overlap the pass-1 box they were derived from, so the
    /// union must not assume they are disjoint.
    #[test]
    fn overlapping_rects_still_produce_one_covering_bounds() {
        let (bounds, _) = overlay_layout(&[
            sr(0, 0, 100, 100, ScanKind::Pass1),
            sr(50, 50, 100, 100, ScanKind::Tile),
        ])
        .unwrap();
        assert_eq!(PhysRect { x: 0, y: 0, w: 150, h: 150 }, bounds);
    }

    #[test]
    fn inset_leaves_an_interior_for_an_ordinary_rect() {
        let inner = inset(PhysRect { x: 10, y: 10, w: 100, h: 40 }, 2).unwrap();
        assert_eq!(PhysRect { x: 12, y: 12, w: 96, h: 36 }, inner);
    }

    /// A real OCR word box from the user's screen measures w=17 h=3. Subtracting
    /// two 2px edges from a 3px height must yield no interior, not a negative one.
    #[test]
    fn inset_of_a_rect_thinner_than_its_border_has_no_interior() {
        assert!(inset(PhysRect { x: 0, y: 0, w: 17, h: 3 }, 2).is_none());
        assert!(inset(PhysRect { x: 0, y: 0, w: 3, h: 17 }, 2).is_none());
        assert!(inset(PhysRect { x: 0, y: 0, w: 4, h: 4 }, 2).is_none());
    }

    /// The popup as `place_popup` produces it: flush with the anchor's left
    /// edge, POPUP_GAP (12) below its bottom.
    fn anchor_and_popup() -> (PhysRect, PhysRect) {
        (r(100, 100, 26, 27), r(100, 139, 420, 300))
    }

    #[test]
    fn the_anchor_and_the_popup_are_both_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(in_sticky(p(113, 113), a, pop), "anchor centre");
        assert!(in_sticky(p(310, 289), a, pop), "popup centre");
        assert!(in_sticky(p(100, 100), a, pop), "anchor top-left is inclusive");
        assert!(in_sticky(p(519, 438), a, pop), "popup bottom-right interior");
    }

    #[test]
    fn the_bridge_covers_the_gap_between_them() {
        let (a, pop) = anchor_and_popup();
        for y in 127..139 {
            assert!(in_sticky(p(113, y), a, pop), "gap row {y} must be sticky");
        }
    }

    /// `contains` is inclusive of the top-left and exclusive of the
    /// bottom-right, so the three rects must tile with no missed row.
    #[test]
    fn the_three_rects_tile_without_a_seam() {
        let (a, pop) = anchor_and_popup();
        for y in 100..439 {
            assert!(in_sticky(p(113, y), a, pop), "row {y} fell through a seam");
        }
    }

    /// D2a's actual guarantee: straight down from the anchor's centre.
    #[test]
    fn the_vertical_path_into_the_popup_never_leaves_the_region() {
        for ax in [-900, 0, 2560, 3400] {
            for ay in [-40, 0, 500, 1800] {
                for (pw, ph) in [(420, 300), (200, 60), (420, 800)] {
                    let a = r(ax, ay, 26, 27);
                    let pop = r(ax, ay + 27 + 12, pw, ph);
                    let cx = ax + 13;
                    for y in ay..(ay + 27 + 12 + ph) {
                        assert!(
                            in_sticky(p(cx, y), a, pop),
                            "anchor ({ax},{ay}) popup {pw}x{ph}: row {y}"
                        );
                    }
                }
            }
        }
    }

    /// `place_popup` flips the popup above the anchor near a monitor's bottom
    /// edge, so the bridge must come from whichever rect is upper rather than
    /// assuming the popup is below.
    #[test]
    fn the_bridge_works_with_the_popup_above_the_anchor() {
        let a = r(100, 900, 26, 27);
        let pop = r(100, 588, 420, 300);
        for y in 888..900 {
            assert!(in_sticky(p(113, y), a, pop), "gap row {y} above the anchor");
        }
        assert!(in_sticky(p(310, 700), a, pop), "popup centre");
    }

    /// THE assertion that fails if anyone replaces the three rects with their
    /// bounding box. The next character along the line must stay hoverable, or
    /// scanning a line freezes the popup on the previous word.
    #[test]
    fn the_next_character_along_the_line_is_not_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(!in_sticky(p(139, 113), a, pop), "one glyph right of the anchor");
        assert!(!in_sticky(p(300, 113), a, pop), "far along the same line");
    }

    /// Pinned deliberately (spec D2a): a shallow diagonal DOES leave the
    /// region, exiting onto the neighbouring character. That is correct
    /// behaviour, and pinning it stops a future widening of the bridge from
    /// silently breaking `the_next_character_along_the_line_is_not_sticky`.
    #[test]
    fn a_shallow_diagonal_leaves_the_region_on_purpose() {
        let (a, pop) = anchor_and_popup();
        assert!(
            !in_sticky(p(127, 126), a, pop),
            "exits the anchor's right edge one row before the bridge"
        );
    }

    #[test]
    fn a_point_well_away_from_both_is_not_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(!in_sticky(p(50, 50), a, pop));
        assert!(!in_sticky(p(1000, 1000), a, pop));
    }

    /// A zero-height bridge must contribute nothing rather than being treated
    /// as a containing rect - the same rule `inset` applies.
    #[test]
    fn a_zero_gap_needs_no_bridge_and_still_tiles() {
        let a = r(100, 100, 26, 27);
        let pop = r(100, 127, 420, 300);
        for y in 100..427 {
            assert!(in_sticky(p(113, y), a, pop), "row {y} with gap 0");
        }
    }

    #[test]
    fn sticky_region_returns_the_anchor_the_popup_and_the_bridge() {
        let (a, pop) = anchor_and_popup();
        let rects = sticky_region(a, pop);
        assert_eq!(a, rects[0]);
        assert_eq!(pop, rects[1]);
        assert_eq!(r(100, 127, 420, 12), rects[2], "bridge spans the union's x-extent");
    }
}

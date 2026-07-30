//! Physical pixel geometry.

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
    /// Half-open: top-left in.
    pub fn contains(&self, p: PhysPoint) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    /// 0.0 when `p` is inside.
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

    /// Negative amounts shrink.
    pub fn inflated(&self, dx: i32, dy: i32) -> PhysRect {
        PhysRect {
            x: self.x - dx,
            y: self.y - dy,
            w: self.w + 2 * dx,
            h: self.h + 2 * dy,
        }
    }

    /// Truncates toward zero.
    pub fn scaled_down(&self, factor: i32) -> PhysRect {
        PhysRect {
            x: self.x / factor,
            y: self.y / factor,
            w: self.w / factor,
            h: self.h / factor,
        }
    }
}

/// Origin of a drawn rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// Pass 1's capture box.
    Pass1,
    /// One forward tile.
    Tile,
    /// The resolved word's box.
    Anchor,
    /// The characters defined.
    Match,
}

/// A rect the overlay draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanRect {
    pub rect: PhysRect,
    pub kind: ScanKind,
}

/// What one hover may show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanDisplay {
    /// Captures; inert when off.
    pub captures: bool,
    /// `Match`.
    pub highlight: bool,
}

impl ScanDisplay {
    /// Is any overlay needed?
    pub fn any(self) -> bool {
        self.captures || self.highlight
    }
}

/// Anchor, popup, bridge.
pub fn sticky_region(anchor: PhysRect, popup: PhysRect) -> [PhysRect; 3] {
    [anchor, popup, bridge_between(anchor, popup)]
}

/// The gap band between two.
fn bridge_between(a: PhysRect, b: PhysRect) -> PhysRect {
    let (upper, lower) = if a.y <= b.y { (a, b) } else { (b, a) };
    let top = upper.y + upper.h;
    let left = a.x.min(b.x);
    let right = (a.x + a.w).max(b.x + b.w);
    PhysRect { x: left, y: top, w: right - left, h: lower.y - top }
}

/// On anchor, popup, or gap.
pub fn in_sticky(p: PhysPoint, anchor: PhysRect, popup: PhysRect) -> bool {
    sticky_region(anchor, popup)
        .iter()
        .any(|r| r.w > 0 && r.h > 0 && r.contains(p))
}

/// Bounds plus local rects.
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

/// `None` when nothing is left.
pub fn inset(rect: PhysRect, thickness: i32) -> Option<PhysRect> {
    let w = rect.w - 2 * thickness;
    let h = rect.h - 2 * thickness;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(PhysRect { x: rect.x + thickness, y: rect.y + thickness, w, h })
}

/// Never covers `anchor`.
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
        assert_eq!(5.0, r(10, 10, 20, 20).edge_distance_to(p(5, 15)));
    }

    #[test]
    fn edge_distance_is_diagonal_at_a_corner() {
        // 3-4-5 triangle.
        assert_eq!(5.0, r(10, 10, 20, 20).edge_distance_to(p(7, 6)));
    }

    #[test]
    fn edge_distance_is_orthogonal_to_the_right() {
        // Occupied span is 10..=29.
        assert_eq!(6.0, r(10, 10, 20, 20).edge_distance_to(p(35, 15)));
    }

    #[test]
    fn edge_distance_is_orthogonal_below() {
        assert_eq!(6.0, r(10, 10, 20, 20).edge_distance_to(p(15, 35)));
    }

    #[test]
    fn edge_distance_is_diagonal_off_the_bottom_right_corner() {
        // 3-4-5 off the far corner.
        assert_eq!(5.0, r(10, 10, 20, 20).edge_distance_to(p(32, 33)));
    }

    #[test]
    fn edge_distance_degrades_correctly_when_near_and_far_edges_coincide() {
        // Near and far edge coincide.
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
        // Truncation, not rounding.
        assert_eq!(r(5, 10, 15, 20), r(11, 21, 31, 41).scaled_down(2));
    }

    #[test]
    fn scaled_down_truncates_toward_zero_for_negative_origin() {
        // Truncates toward zero.
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
        // Secondary starts at x=2560.
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

    /// Highlight only: one box.
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

    /// Off means no window.
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

    /// Tiles overlap pass 1.
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

    /// A real box measures 17x3.
    #[test]
    fn inset_of_a_rect_thinner_than_its_border_has_no_interior() {
        assert!(inset(PhysRect { x: 0, y: 0, w: 17, h: 3 }, 2).is_none());
        assert!(inset(PhysRect { x: 0, y: 0, w: 3, h: 17 }, 2).is_none());
        assert!(inset(PhysRect { x: 0, y: 0, w: 4, h: 4 }, 2).is_none());
    }

    /// Flush left, 12px below.
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

    /// No missed row at a seam.
    #[test]
    fn the_three_rects_tile_without_a_seam() {
        let (a, pop) = anchor_and_popup();
        for y in 100..439 {
            assert!(in_sticky(p(113, y), a, pop), "row {y} fell through a seam");
        }
    }

    /// D2a: straight down.
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

    /// Popup may sit above.
    #[test]
    fn the_bridge_works_with_the_popup_above_the_anchor() {
        let a = r(100, 900, 26, 27);
        let pop = r(100, 588, 420, 300);
        for y in 888..900 {
            assert!(in_sticky(p(113, y), a, pop), "gap row {y} above the anchor");
        }
        assert!(in_sticky(p(310, 700), a, pop), "popup centre");
    }

    /// Fails on a bounding box.
    #[test]
    fn the_next_character_along_the_line_is_not_sticky() {
        let (a, pop) = anchor_and_popup();
        assert!(!in_sticky(p(139, 113), a, pop), "one glyph right of the anchor");
        assert!(!in_sticky(p(300, 113), a, pop), "far along the same line");
    }

    /// Leaving is on purpose.
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

    /// Zero-height bridge is nil.
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

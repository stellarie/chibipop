//! Core geometry uses physical pixels.

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
    /// The rectangle uses half-open bounds. It includes the top and left edges and
    /// excludes the bottom and right edges.
    pub fn contains(&self, p: PhysPoint) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    /// Return `0.0` when `p` is inside the rectangle.
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

    /// A negative value shrinks the rectangle.
    pub fn inflated(&self, dx: i32, dy: i32) -> PhysRect {
        PhysRect {
            x: self.x - dx,
            y: self.y - dy,
            w: self.w + 2 * dx,
            h: self.h + 2 * dy,
        }
    }

    /// Return the overlap of two rectangles.
    ///
    /// The bounds are half-open, like `contains`. Rectangles that share only an
    /// edge do not intersect.
    pub fn intersection(&self, other: PhysRect) -> Option<PhysRect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x.saturating_add(self.w)).min(other.x.saturating_add(other.w));
        let bottom = (self.y.saturating_add(self.h)).min(other.y.saturating_add(other.h));
        if right <= x || bottom <= y {
            return None;
        }
        Some(PhysRect { x, y, w: right - x, h: bottom - y })
    }

    /// Division truncates each value toward zero.
    pub fn scaled_down(&self, factor: i32) -> PhysRect {
        PhysRect {
            x: self.x / factor,
            y: self.y / factor,
            w: self.w / factor,
            h: self.h / factor,
        }
    }
}

/// The kind identifies the source of a drawn rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// This is the Pass 1 capture rectangle.
    Pass1,
    /// This is a tile that follows Pass 1.
    Tile,
    /// This is the rectangle of the resolved word.
    Anchor,
    /// This is the rectangle of the matched characters.
    Match,
}

/// A rectangle that the overlay draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanRect {
    pub rect: PhysRect,
    pub kind: ScanKind,
}

/// This is the overlay content for one hover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanDisplay {
    /// This value shows capture rectangles when it is `true`.
    pub captures: bool,
    /// Show the `Match` rectangle when this value is `true`.
    pub highlight: bool,
}

impl ScanDisplay {
    /// Return `true` when the overlay has content.
    pub fn any(self) -> bool {
        self.captures || self.highlight
    }
}

/// Return the freeze rectangle, the popup rectangle, and the corridor.
pub fn sticky_region(freeze: PhysRect, reach: PhysRect, popup: PhysRect) -> [PhysRect; 3] {
    // Set the bridge source to the freeze row and the full reach width.
    let bridge_src = PhysRect { x: reach.x, w: reach.w, y: freeze.y, h: freeze.h };
    [freeze, popup, bridge_between(bridge_src, popup)]
}

/// Return the gap band between two rectangles.
fn bridge_between(a: PhysRect, b: PhysRect) -> PhysRect {
    let (upper, lower) = if a.y <= b.y { (a, b) } else { (b, a) };
    let top = upper.y + upper.h;
    let left = a.x.min(b.x);
    let right = (a.x + a.w).max(b.x + b.w);
    PhysRect { x: left, y: top, w: right - left, h: lower.y - top }
}

/// Return `true` when `p` is in the freeze rectangle, popup, or corridor.
pub fn in_sticky(p: PhysPoint, freeze: PhysRect, reach: PhysRect, popup: PhysRect) -> bool {
    sticky_region(freeze, reach, popup)
        .iter()
        .any(|r| r.w > 0 && r.h > 0 && r.contains(p))
}

/// Return the bounds and rectangles with local coordinates.
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

/// Return `None` when no interior remains.
pub fn inset(rect: PhysRect, thickness: i32) -> Option<PhysRect> {
    let w = rect.w - 2 * thickness;
    let h = rect.h - 2 * thickness;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(PhysRect { x: rect.x + thickness, y: rect.y + thickness, w, h })
}

/// Limit selected-text popups to one side of their source instead of clamping
/// a tall panel over the selected word. Zero means no non-overlapping space.
pub fn popup_height_outside(anchor: PhysRect, monitor: PhysRect, gap: i32) -> i32 {
    let top = i64::from(monitor.y);
    let bottom = top + i64::from(monitor.h.max(0));
    let above = i64::from(anchor.y).min(bottom) - top - i64::from(gap.max(0));
    let below = bottom - (i64::from(anchor.y) + i64::from(anchor.h)).max(top) - i64::from(gap.max(0));
    above.max(below).clamp(0, i64::from(monitor.h.max(0))) as i32
}

/// Place the popup so it does not cover `anchor`.
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
    #[test]
    fn popup_flips_both_axes_in_the_corner() {
        let mon = r(0, 0, 1920, 1080);
        let got = place_popup(r(1850, 1040, 20, 20), (300, 200), mon, 12);
        assert!(got.x + got.w <= mon.x + mon.w);
        assert!(got.y + got.h <= mon.y + mon.h);
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

    /// D2a: the path goes straight down.
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
                            in_sticky(p(cx, y), a, a, pop),
                            "anchor ({ax},{ay}) popup {pw}x{ph}: row {y}"
                        );
                    }
                }
            }
        }
    }

}

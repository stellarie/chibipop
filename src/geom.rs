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

    /// Integer division of every field — used to map coordinates back out of
    /// upscaled-image space.
    pub fn scaled_down(&self, factor: i32) -> PhysRect {
        PhysRect {
            x: self.x / factor,
            y: self.y / factor,
            w: self.w / factor,
            h: self.h / factor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> PhysRect { PhysRect { x, y, w, h } }
    fn p(x: i32, y: i32) -> PhysPoint { PhysPoint { x, y } }

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
}

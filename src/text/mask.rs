//! This module removes chibipop's popup from its OCR source.
//! (ARCHITECTURE.md#capture-and-masking).
//!
//! Wayland has no portable protocol to exclude one surface from another surface's capture.
//! Therefore, the Worker applies the mask in Core.
//! Before OCR, the code fills the overlap between the captured frame and the popup rect with flat white.
//! The fill has a hard edge.
//!
//! Benchmarks define this method. Each candidate OCR engine produced the same result for each fill color.
//! A one-pixel feather changed no measured result.
//!
//! Both rects use physical pixels in Core.
//! The mask uses arithmetic only and needs no protocol call.
//! Every capture backend uses the same mask.
//!
//! The engine treats the mask boundary as a capture edge.
//! The engine drops each word that overlaps this boundary.
//! It does not recognize a word part.
//! The engine applies the same rule to a word clipped by a tile edge.

use crate::geom::PhysRect;

/// This enum records whether a grab occurred before or after the popup appeared.
///
/// A live grab can include the visible popup, so the code applies the mask to each live grab.
/// Trigger mode takes a frozen grab before it creates the popup.
/// The frozen buffer therefore contains no popup pixels, and the code leaves it unchanged.
/// (ARCHITECTURE.md#capture-and-masking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// A live grab can include the popup, so the code applies the mask.
    Live,
    /// A frozen grab occurs before the popup exists, so the code does not need a mask.
    Frozen,
}

/// A `CaptureMask` identifies the screen area that OCR must not read.
///
/// The area is the popup when a popup exists.
/// All rects below the seams use physical pixels.
/// The platform bin decides whether the code needs a mask.
/// Windows uses its capture guard and WDA exclusion, so it supplies [`CaptureMask::NONE`].
/// Wayland supplies the Controller's `PopupPlaced` rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureMask {
    popup: Option<PhysRect>,
}

impl CaptureMask {
    /// This value represents a capture with no popup mask.
    pub const NONE: CaptureMask = CaptureMask { popup: None };

    /// This function uses `popup` as the mask for a live grab.
    /// A frozen grab precedes the popup, so the returned mask is empty.
    /// (ARCHITECTURE.md#capture-and-masking).
    pub fn for_mode(mode: CaptureMode, popup: Option<PhysRect>) -> CaptureMask {
        match mode {
            CaptureMode::Live => CaptureMask { popup },
            CaptureMode::Frozen => CaptureMask::NONE,
        }
    }

    /// Return the part of the mask that overlaps `region`.
    ///
    /// Return [`CaptureMask::NONE`] when the popup does not overlap `region`.
    /// The clipped mask controls the white fill, word removal, and reuse key for an unchanged grab.
    ///
    /// The code clips the mask first, so these three uses cannot differ.
    /// Two grabs of the same region ask the same question when their clipped masks are equal.
    /// A popup outside the region does not change the clipped mask or the OCR result.
    pub fn clipped_to(&self, region: PhysRect) -> CaptureMask {
        CaptureMask { popup: self.popup.and_then(|p| p.intersection(region)) }
    }

    /// Return the popup overlap in coordinates relative to `region`.
    ///
    /// Return `None` when no popup or overlap exists.
    /// A shared edge does not overlap because `PhysRect::intersection` uses half-open bounds.
    pub fn overlap_in(&self, region: PhysRect) -> Option<PhysRect> {
        let hit = self.popup?.intersection(region)?;
        Some(hit.translated(-region.x, -region.y))
    }

    /// Report whether a recognized word rect overlaps the mask.
    ///
    /// The engine treats the mask boundary as a capture edge.
    /// It must drop an overlapping word, not recognize part of that word.
    /// Both `rect` and the mask use physical pixels.
    /// (ARCHITECTURE.md#capture-and-masking).
    pub fn hides(&self, rect: PhysRect) -> bool {
        self.popup.is_some_and(|p| p.intersection(rect).is_some())
    }

    /// Fill the popup overlap with flat white in a captured frame.
    ///
    /// `buf` uses [`super::Frame`]'s top-down BGRA8 format and has `w * h * 4` bytes.
    /// The buffer maps one-to-one to `region` in physical pixels.
    /// The fill has a hard edge.
    /// Benchmarks found this fill safe for all candidate OCR engines.
    pub fn apply(&self, buf: &mut [u8], w: i32, h: i32, region: PhysRect) {
        let Some(local) = self.overlap_in(region) else { return };
        fill_white(buf, w, h, local);
    }
}

/// Fill `rect` with flat white and clamp the rect to the image.
///
/// The function also clamps the rect to the length of `buf`.
/// A short buffer cannot cause a panic.
fn fill_white(buf: &mut [u8], w: i32, h: i32, rect: PhysRect) {
    let stride = w.max(0) as usize * 4;
    if stride == 0 {
        return;
    }
    let rows = h.max(0).min((buf.len() / stride) as i32);
    let x0 = rect.x.clamp(0, w) as usize;
    let x1 = rect.x.saturating_add(rect.w).clamp(0, w) as usize;
    let y0 = rect.y.clamp(0, rows) as usize;
    let y1 = rect.y.saturating_add(rect.h).clamp(0, rows) as usize;
    if x1 <= x0 {
        return;
    }
    for y in y0..y1 {
        buf[y * stride + x0 * 4..y * stride + x1 * 4].fill(0xFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> PhysRect {
        PhysRect { x, y, w, h }
    }

    fn mask(popup: PhysRect) -> CaptureMask {
        CaptureMask::for_mode(CaptureMode::Live, Some(popup))
    }

    #[test]
    fn no_popup_masks_nothing() {
        let m = CaptureMask::NONE;
        assert_eq!(None, m.overlap_in(r(0, 0, 100, 100)));
        assert!(!m.hides(r(10, 10, 20, 20)));
    }

    #[test]
    fn frozen_grabs_are_maskless_even_with_a_popup() {
        let m = CaptureMask::for_mode(CaptureMode::Frozen, Some(r(0, 0, 500, 500)));
        assert_eq!(CaptureMask::NONE, m);
        assert_eq!(None, m.overlap_in(r(0, 0, 100, 100)));
        assert!(!m.hides(r(10, 10, 20, 20)));
    }

    #[test]
    fn disjoint_popup_and_region_do_not_overlap() {
        assert_eq!(None, mask(r(500, 500, 100, 100)).overlap_in(r(0, 0, 100, 100)));
    }

    #[test]
    fn partial_overlap_is_clipped_and_region_local() {
        // The popup extends beyond the region's bottom-right corner.
        let got = mask(r(150, 180, 100, 100)).overlap_in(r(100, 100, 100, 100));
        assert_eq!(Some(r(50, 80, 50, 20)), got);
    }

    #[test]
    fn popup_containing_the_region_masks_all_of_it() {
        let got = mask(r(0, 0, 1000, 1000)).overlap_in(r(300, 300, 100, 50));
        assert_eq!(Some(r(0, 0, 100, 50)), got);
    }

    #[test]
    fn popup_inside_the_region_masks_its_own_box() {
        let got = mask(r(320, 310, 40, 20)).overlap_in(r(300, 300, 100, 50));
        assert_eq!(Some(r(20, 10, 40, 20)), got);
    }

    #[test]
    fn a_shared_edge_is_no_overlap() {
        // The popup starts at the exact end of the region.
        assert_eq!(None, mask(r(200, 100, 50, 50)).overlap_in(r(100, 100, 100, 100)));
        assert!(!mask(r(200, 100, 50, 50)).hides(r(150, 100, 50, 50)));
    }

    #[test]
    fn hides_words_that_straddle_the_mask_boundary() {
        let m = mask(r(100, 100, 100, 100));
        assert!(m.hides(r(150, 150, 20, 20)), "fully inside");
        assert!(m.hides(r(90, 150, 20, 20)), "straddling the left edge");
        assert!(!m.hides(r(60, 150, 20, 20)), "fully outside");
        assert!(!m.hides(r(80, 150, 20, 20)), "adjacent, touching only the edge");
    }

    /// The frame uses distinct nonwhite bytes.
    /// The mask must fill selected pixels with flat white.
    /// All other bytes must stay unchanged.
    #[test]
    fn apply_white_fills_exactly_the_overlap() {
        let (w, h) = (6, 4);
        let region = r(10, 20, w, h);
        let mut buf: Vec<u8> = (0..(w * h * 4) as usize).map(|i| (i % 251) as u8).collect();
        let before = buf.clone();

        // The popup covers region columns 2 through 4 and rows 1 through 3.
        mask(r(12, 21, 2, 2)).apply(&mut buf, w, h, region);

        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let inside = (2..4).contains(&x) && (1..3).contains(&y);
                if inside {
                    assert_eq!([0xFF; 4], buf[i..i + 4], "pixel ({x},{y}) must be white");
                } else {
                    let why = "must be untouched";
                    assert_eq!(before[i..i + 4], buf[i..i + 4], "pixel ({x},{y}) {why}");
                }
            }
        }
    }

    #[test]
    fn apply_clamps_a_popup_hanging_off_the_frame() {
        let (w, h) = (4, 4);
        let region = r(0, 0, w, h);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // The popup extends beyond two region edges.
        // The code must fill only the part inside the frame and must not panic.
        mask(r(2, -3, 100, 5)).apply(&mut buf, w, h, region);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let expect = if x >= 2 && y < 2 { [0xFF; 4] } else { [0u8; 4] };
                assert_eq!(expect, buf[i..i + 4], "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn apply_with_no_overlap_leaves_the_frame_byte_identical() {
        let (w, h) = (4, 4);
        let mut buf: Vec<u8> = (0..(w * h * 4) as usize).map(|i| i as u8).collect();
        let before = buf.clone();
        mask(r(500, 500, 10, 10)).apply(&mut buf, w, h, r(0, 0, w, h));
        assert_eq!(before, buf);
    }

    /// A short buffer cannot stop the process.
    /// The mask fills only the available rows.
    #[test]
    fn apply_to_a_short_buffer_fills_what_is_there_and_does_not_panic() {
        let (w, h) = (4, 4);
        // The buffer has two rows, but the call claims that it has four rows.
        let mut buf = vec![0u8; (w * 2 * 4) as usize];
        mask(r(0, 0, 4, 4)).apply(&mut buf, w, h, r(0, 0, w, h));
        assert_eq!(vec![0xFFu8; (w * 2 * 4) as usize], buf, "both real rows are white");
    }

    #[test]
    fn apply_to_a_zero_width_frame_is_a_no_op() {
        let mut buf = Vec::new();
        mask(r(0, 0, 10, 10)).apply(&mut buf, 0, 10, r(0, 0, 0, 10));
        assert!(buf.is_empty());
    }
}

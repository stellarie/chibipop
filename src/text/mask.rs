//! Keeping chibipop's own popup out of its own OCR input (ADR-0008).
//!
//! No portable protocol-level surface exclusion exists on Wayland, so the
//! Worker masks in core: before OCR, the captured frame's overlap with the
//! popup rect is filled flat white with a hard edge (benchmark-tuned; the
//! fill color is irrelevant to the chosen engine and the 1 px feather
//! bought nothing measurable). Both rects are core state in physical
//! pixels, so exclusion is pure arithmetic - no protocol dependency,
//! identical on every capture backend.
//!
//! The mask boundary is a capture edge: words whose boxes intersect the
//! mask are dropped, never half-recognised, exactly like words clipped at
//! a tile edge.

use crate::geom::PhysRect;

/// How the pixels relate to the popup, in time.
///
/// A live grab happens while the popup may be on screen, so the popup must
/// be masked out of it. A frozen grab predates the popup by construction -
/// trigger mode captures at trigger press, before anything is shown - so a
/// frozen buffer never self-contaminates and is left untouched (ADR-0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// Grabbed while the popup may be visible: mask it.
    Live,
    /// Grabbed before the popup existed: nothing to mask.
    Frozen,
}

/// The screen area OCR must not read: our own popup, if any.
///
/// Physical pixels, like every rect below the seams. The platform bin
/// decides whether to supply one at all: Windows keeps its guard/WDA
/// exclusion and supplies [`CaptureMask::NONE`]; Wayland supplies the
/// controller's `PopupPlaced` rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureMask {
    popup: Option<PhysRect>,
}

impl CaptureMask {
    /// No popup to hide.
    pub const NONE: CaptureMask = CaptureMask { popup: None };

    /// Mask `popup` out of live grabs; frozen grabs predate the popup
    /// and stay maskless (ADR-0008).
    pub fn for_mode(mode: CaptureMode, popup: Option<PhysRect>) -> CaptureMask {
        match mode {
            CaptureMode::Live => CaptureMask { popup },
            CaptureMode::Frozen => CaptureMask::NONE,
        }
    }

    /// The popup∩region overlap, in `region`-local pixels.
    ///
    /// `None` when nothing needs masking: no popup, or no overlap. A
    /// shared edge is no overlap (`PhysRect::intersection` is half-open).
    pub fn overlap_in(&self, region: PhysRect) -> Option<PhysRect> {
        let hit = self.popup?.intersection(region)?;
        Some(hit.translated(-region.x, -region.y))
    }

    /// Whether a recognised word's box touches the mask.
    ///
    /// The mask boundary is a capture edge: a word intersecting it must be
    /// dropped rather than half-recognised (ADR-0008). `rect` is in
    /// desktop pixels, like the mask.
    pub fn hides(&self, rect: PhysRect) -> bool {
        self.popup.is_some_and(|p| p.intersection(rect).is_some())
    }

    /// White-fill the popup overlap in a grabbed frame.
    ///
    /// `buf` is BGRA8, `w * h * 4` bytes, top-down - [`super::Frame`]'s
    /// format - covering `region`'s pixels one-to-one. White, hard edge:
    /// the benchmarked safe fill across all candidate engines.
    pub fn apply(&self, buf: &mut [u8], w: i32, h: i32, region: PhysRect) {
        let Some(local) = self.overlap_in(region) else { return };
        fill_white(buf, w, h, local);
    }
}

/// Flat white over `rect`, clamped to the image.
///
/// Clamped to `buf` as well as to `w`/`h`: a backend that hands back a
/// short buffer must not turn a mask into a panic.
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
        // Popup hangs off the region's bottom-right corner.
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
        // Popup starts exactly where the region ends.
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

    /// A 6x4 frame with a distinct non-white byte pattern; the masked
    /// pixels must be flat white and every other byte untouched.
    #[test]
    fn apply_white_fills_exactly_the_overlap() {
        let (w, h) = (6, 4);
        let region = r(10, 20, w, h);
        let mut buf: Vec<u8> = (0..(w * h * 4) as usize).map(|i| (i % 251) as u8).collect();
        let before = buf.clone();

        // Popup covers the region's columns 2..4, rows 1..3.
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
        // Popup extends past the region on two sides; must not panic and
        // must fill only the in-frame part.
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

    /// A backend that hands back fewer rows than it promised must cost a
    /// mask, not the process.
    #[test]
    fn apply_to_a_short_buffer_fills_what_is_there_and_does_not_panic() {
        let (w, h) = (4, 4);
        // Two rows only, where four were claimed.
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

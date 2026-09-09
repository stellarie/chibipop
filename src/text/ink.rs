//! This module measures ink in a captured frame.
//!
//! It answers one question: when the engine returns no hit, does text-sized ink
//! exist under the cursor because the capture box is too small? The Linux engine
//! scales a crop to its detector size. A 100 px glyph in a 500x100 box is too
//! large to detect, so the engine returns no words. A word-based rule cannot detect
//! this case. Pixel data can answer this question. (ARCHITECTURE.md#capture-and-masking)
//!
//! This module does not use stroke-level analysis. It measures one thing: the
//! cross-axis extent of ink near the cursor. A glyph fills its cell on the cross
//! axis. A rule, an underline, and a small icon do not.

use crate::geom::{PhysPoint, PhysRect};
use crate::text::layout::box_orientation;
use crate::text::Frame;

/// A pixel is ink when one channel differs from the background by this much.
///
/// Anti-aliased edges fall below this threshold. Text in any color on a flat
/// background passes this threshold, including yellow on white, which differs in blue alone.
const INK_CONTRAST: u8 = 48;

/// A cross-axis line holds ink when at least this many pixels in the window are ink.
/// One pixel is noise. A vertical stroke is two or more.
const INK_PIXELS: usize = 2;

/// The ink extent must reach this share of the reference short side.
///
/// Issue #92: a white 80 px `活` on black filled 76 px of a 100 px box, and the
/// engine returned no words. A 40 px body line reaches 40 %, and the engine reads
/// it without a larger box.
const SPAN_PERCENT: i32 = 60;

/// Return true when ink near `cursor` spans at least [`SPAN_PERCENT`] of `reference`
/// on the short side of `region`.
///
/// `frame` contains `region` at scale `factor`. `masked` lists popup rects in frame
/// pixels. The mask fills those rects with flat white, so the rects do not count as
/// ink. The window on the reading axis extends one short side to each side of the
/// cursor. The background is the most common frame color, with each channel grouped
/// into 16 levels.
pub fn spans_short_side(
    frame: &Frame,
    region: PhysRect,
    cursor: PhysPoint,
    factor: i32,
    reference: i32,
    masked: &[PhysRect],
) -> bool {
    let (w, h) = (frame.w as usize, frame.h as usize);
    if w == 0 || h == 0 || frame.buf.len() < w * h * 4 {
        return false;
    }
    let pixels = frame.buf[..w * h * 4].as_chunks::<4>().0;
    let background = background_of(pixels);
    let is_ink = |x: usize, y: usize| {
        let px = &pixels[y * w + x];
        let at = PhysPoint { x: x as i32, y: y as i32 };
        !masked.iter().any(|m| m.contains(at))
            && px[..3].iter().zip(background).any(|(&c, b)| c.abs_diff(b) >= INK_CONTRAST)
    };

    let horizontal = box_orientation(region) == crate::text::layout::Orientation::Horizontal;
    let short = if horizontal { h } else { w } as i32;
    let cursor_x = (cursor.x - region.x) * factor;
    let cursor_y = (cursor.y - region.y) * factor;
    let (lead, lead_len) = if horizontal { (cursor_x, w as i32) } else { (cursor_y, h as i32) };
    let from = (lead - short).max(0) as usize;
    let to = (lead + short).min(lead_len) as usize;
    if to <= from {
        return false;
    }

    let mut first = None;
    let mut last = 0;
    for cross in 0..short as usize {
        let count = (from..to)
            .filter(|&along| {
                let (x, y) = if horizontal { (along, cross) } else { (cross, along) };
                is_ink(x, y)
            })
            .count();
        if count >= INK_PIXELS {
            first.get_or_insert(cross);
            last = cross;
        }
    }
    let Some(first) = first else { return false };
    let extent = (last - first + 1) as i32;
    extent * 100 >= reference * factor * SPAN_PERCENT
}

/// Return the most common color of the frame, quantized to 16 levels per channel.
fn background_of(pixels: &[[u8; 4]]) -> [u8; 3] {
    let mut counts = [0u32; 4096];
    for px in pixels {
        let key = (usize::from(px[0] >> 4) << 8) | (usize::from(px[1] >> 4) << 4) | usize::from(px[2] >> 4);
        counts[key] += 1;
    }
    let key = counts.iter().enumerate().max_by_key(|(_, &n)| n).map_or(0, |(k, _)| k);
    let level = |shift: usize| ((key >> shift) & 0xF) as u8 * 16 + 8;
    [level(8), level(4), level(0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: i32, h: i32, bg: [u8; 3], paint: impl Fn(i32, i32) -> bool, fg: [u8; 3]) -> Frame {
        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let c = if paint(x, y) { fg } else { bg };
                buf.extend_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
        Frame { buf, w, h, source: "test", fallback: None, unchanged: false }
    }

    const BOX: PhysRect = PhysRect { x: 100, y: 200, w: 500, h: 100 };
    const CURSOR: PhysPoint = PhysPoint { x: 350, y: 250 };
    const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
    const BLACK: [u8; 3] = [0, 0, 0];

    #[test]
    fn a_white_glyph_on_black_that_fills_the_box_spans_it() {
        let f = frame(500, 100, BLACK, |x, y| (210..290).contains(&x) && (3..97).contains(&y), WHITE);
        assert!(spans_short_side(&f, BOX, CURSOR, 1, 100, &[]));
    }

    #[test]
    fn a_body_line_does_not_span_the_box() {
        let f = frame(500, 100, WHITE, |x, y| (210..290).contains(&x) && (30..70).contains(&y), BLACK);
        assert!(!spans_short_side(&f, BOX, CURSOR, 1, 100, &[]));
    }

    #[test]
    fn an_empty_frame_does_not_span_the_box() {
        let f = frame(500, 100, WHITE, |_, _| false, BLACK);
        assert!(!spans_short_side(&f, BOX, CURSOR, 1, 100, &[]));
    }

    #[test]
    fn a_rule_at_one_edge_does_not_span_the_box() {
        let f = frame(500, 100, WHITE, |_, y| y < 2, BLACK);
        assert!(!spans_short_side(&f, BOX, CURSOR, 1, 100, &[]));
    }

    #[test]
    fn ink_far_from_the_cursor_does_not_count() {
        let f = frame(500, 100, WHITE, |x, _| x < 40, BLACK);
        assert!(!spans_short_side(&f, BOX, CURSOR, 1, 100, &[]));
    }

    #[test]
    fn a_grown_box_measures_against_the_configured_short_side() {
        let tall = PhysRect { x: 100, y: 150, w: 500, h: 200 };
        let f = frame(500, 200, BLACK, |x, y| (210..290).contains(&x) && (50..150).contains(&y), WHITE);
        assert!(spans_short_side(&f, tall, CURSOR, 1, 100, &[]));
    }

    #[test]
    fn an_upscaled_frame_maps_the_cursor() {
        let f = frame(1000, 200, BLACK, |x, y| (420..580).contains(&x) && (6..194).contains(&y), WHITE);
        assert!(spans_short_side(&f, BOX, CURSOR, 2, 100, &[]));
    }

    #[test]
    fn a_vertical_box_measures_its_width() {
        let tall = PhysRect { x: 300, y: 0, w: 100, h: 500 };
        let f = frame(100, 500, WHITE, |x, y| (3..97).contains(&x) && (210..290).contains(&y), BLACK);
        assert!(spans_short_side(&f, tall, PhysPoint { x: 350, y: 250 }, 1, 100, &[]));
    }

    /// The mask fills our popup with flat white. On a dark desktop that white spans
    /// the box, and it is not text.
    #[test]
    fn a_masked_popup_is_not_ink() {
        let popup = PhysRect { x: 200, y: 0, w: 100, h: 100 };
        let f = frame(500, 100, BLACK, |x, y| popup.contains(PhysPoint { x, y }), WHITE);
        assert!(spans_short_side(&f, BOX, CURSOR, 1, 100, &[]));
        assert!(!spans_short_side(&f, BOX, CURSOR, 1, 100, &[popup]));
    }
}

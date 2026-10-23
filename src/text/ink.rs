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

/// Checks the ink span near the cursor.
/// Masks use scaled frame pixels.
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
    let Some(background) = background_of(pixels, w, masked) else { return false };
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

/// Return the most common unmasked color, quantized to 16 levels per channel.
fn background_of(pixels: &[[u8; 4]], width: usize, masked: &[PhysRect]) -> Option<[u8; 3]> {
    let mut counts = [0u32; 4096];
    let mut included = 0usize;
    for (index, px) in pixels.iter().enumerate() {
        let at = PhysPoint { x: (index % width) as i32, y: (index / width) as i32 };
        if masked.iter().any(|m| m.contains(at)) {
            continue;
        }
        let key = (usize::from(px[0] >> 4) << 8) | (usize::from(px[1] >> 4) << 4) | usize::from(px[2] >> 4);
        counts[key] += 1;
        included += 1;
    }
    if included == 0 {
        return None;
    }
    let key = counts.iter().enumerate().max_by_key(|(_, &n)| n).map_or(0, |(k, _)| k);
    let level = |shift: usize| ((key >> shift) & 0xF) as u8 * 16 + 8;
    Some([level(8), level(4), level(0)])
}


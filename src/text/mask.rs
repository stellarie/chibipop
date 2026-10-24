//! This module removes chibipop's popup from its OCR source.
//! (ARCHITECTURE.md#capture-and-masking).
//!
//! Wayland has no portable protocol to exclude one surface from another surface's capture.
//! Therefore, the Worker applies the mask in Core.
//! Before OCR, the code fills each captured popup rectangle with flat white.
//! Gaps between retained popup surfaces stay readable.
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
/// Each rectangle belongs to one visible popup surface.
/// All rects below the seams use physical pixels.
/// The platform bin decides whether the code needs a mask.
/// Windows uses its capture guard and WDA exclusion, so it supplies [`CaptureMask::NONE`].
/// Wayland supplies every visible Controller popup rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureMask {
    rects: [PhysRect; Self::MAX_RECTS],
    len: usize,
}

impl CaptureMask {
    /// One root and at most sixteen retained parents need no heap allocation.
    pub const MAX_RECTS: usize = 17;

    /// This value represents a capture with no popup mask.
    pub const NONE: CaptureMask = CaptureMask {
        rects: [PhysRect { x: 0, y: 0, w: 0, h: 0 }; Self::MAX_RECTS],
        len: 0,
    };

    /// This function uses `popup` as the mask for a live grab.
    /// A frozen grab precedes the popup, so the returned mask is empty.
    /// (ARCHITECTURE.md#capture-and-masking).
    pub fn for_mode(mode: CaptureMode, popup: Option<PhysRect>) -> CaptureMask {
        let mut mask = Self::NONE;
        if mode == CaptureMode::Live {
            if let Some(rect) = popup.filter(|rect| rect.w > 0 && rect.h > 0) {
                mask.rects[0] = rect;
                mask.len = 1;
            }
        }
        mask
    }

    /// Reject excess input instead of letting an unmasked popup enter OCR.
    /// Frozen captures remain maskless. Capacity applies to both capture modes.
    pub fn for_rects(mode: CaptureMode, rects: &[PhysRect]) -> anyhow::Result<CaptureMask> {
        anyhow::ensure!(rects.len() <= Self::MAX_RECTS,
            "capture mask has {} rectangles; maximum is {}", rects.len(), Self::MAX_RECTS);
        let mut mask = Self::NONE;
        if mode == CaptureMode::Live {
            for &rect in rects.iter().filter(|rect| rect.w > 0 && rect.h > 0) {
                mask.rects[mask.len] = rect;
                mask.len += 1;
            }
            mask.normalize();
        }
        Ok(mask)
    }

    fn normalize(&mut self) {
        self.rects[..self.len].sort_unstable_by_key(|rect| (rect.x, rect.y, rect.w, rect.h));
        let mut kept = 0;
        for index in 0..self.len {
            let rect = self.rects[index];
            if kept == 0 || self.rects[kept - 1] != rect {
                self.rects[kept] = rect;
                kept += 1;
            }
        }
        self.rects[kept..].fill(PhysRect { x: 0, y: 0, w: 0, h: 0 });
        self.len = kept;
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
        let mut mask = Self::NONE;
        for rect in &self.rects[..self.len] {
            if let Some(hit) = rect.intersection(region) {
                mask.rects[mask.len] = hit;
                mask.len += 1;
            }
        }
        mask.normalize();
        mask
    }

    /// Return each popup overlap in coordinates relative to `region`.
    ///
    /// No overlap yields an empty iterator, never a bounding rectangle.
    /// A shared edge does not overlap because `PhysRect::intersection` uses half-open bounds.
    pub fn overlap_in(&self, region: PhysRect) -> impl Iterator<Item = PhysRect> + '_ {
        self.rects[..self.len].iter().filter_map(move |rect| {
            let hit = rect.intersection(region)?;
            Some(PhysRect { x: hit.x - region.x, y: hit.y - region.y, ..hit })
        })
    }

    /// Report whether a recognized word rect overlaps the mask.
    ///
    /// The engine treats the mask boundary as a capture edge.
    /// It must drop an overlapping word, not recognize part of that word.
    /// Both `rect` and the mask use physical pixels.
    /// (ARCHITECTURE.md#capture-and-masking).
    pub fn hides(&self, rect: PhysRect) -> bool {
        self.rects[..self.len].iter().any(|popup| popup.intersection(rect).is_some())
    }

    /// Fill the popup overlap with flat white in a captured frame.
    ///
    /// `buf` uses [`super::Frame`]'s top-down BGRA8 format and has `w * h * 4` bytes.
    /// The buffer maps one-to-one to `region` in physical pixels.
    /// The fill has a hard edge.
    /// Benchmarks found this fill safe for all candidate OCR engines.
    pub fn apply(&self, buf: &mut [u8], w: i32, h: i32, region: PhysRect) {
        for local in self.overlap_in(region) {
            fill_white(buf, w, h, local);
        }
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


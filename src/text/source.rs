//! The core-internal facade over `RegionCapture` + `OcrEngine` + the shared
//! layout/hit-scan logic: point in, text span out (ADR-0001). Platform code
//! supplies the two seams; everything below them is shared.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::layout::{
    band_of, head_and_tail, map_from_upscaled, nearest_line, normalise, region_around, resolve,
    tile_forward, CaptureSize, OcrLine, OcrWord, Orientation, Resolved,
};
use crate::text::mask::CaptureMask;
use crate::text::{Frame, OcrEngine, RegionCapture, TextSpan};
use anyhow::{Context, Result};

/// Small text else misreads.
pub const UPSCALE: i32 = 2;

// MAINTAINER NOTE - adaptive upscale retry, disabled 2026-08-08.
// (Deliberately longer than the 30-char house rule: this records a
// method and a retraction, and Stella asked for it to live here.)
//
// What it does: after the first pass at UPSCALE, if the tallest word
// recognised is under SMALL_GLYPH_PX, capture and OCR the same region
// again at RETRY_UPSCALE and prefer that result when it is non-empty.
//
// Why it is off: it was added on the observation that a line of text
// vanished at 2x and reappeared at 4x. That evidence is void. It was
// gathered while DXGI Desktop Duplication was silently returning
// all-black frames (see the Windows capture backend), so "2x found
// nothing" was often a dead capture rather than a scale problem, and
// which pass got a live frame was luck. Re-measured on the repaired
// pipeline it cost ~36 ms of the ~141 ms round trip - two captures and
// two OCR passes, the second over 4x the pixels - and it was not
// reliably better:
//
//   line 3, 28-31px glyphs, ground truth すっかり気が抜け、...
//     retry on : すっかーけ。ただ水と化
//     single 2x: すっかり一け。ただ水と化   <- more accurate
//
// It also fires on ordinary body text: 28-31px is comfortably legible
// yet sits under the 32px threshold, so the cost was paid constantly.
//
// How to re-enable honestly: flip ADAPTIVE_RETRY, then measure with
//   probe --at X,Y --repeat N            (warm timings, one process)
//   probe --at X,Y --upscale 2|4         (single pass, no retry)
// against text whose true string is known, and compare transcription
// accuracy, not just whether more characters appeared. Keep it only
// if it wins on accuracy at a cost you can defend. Re-tuning
// SMALL_GLYPH_PX downward (~22px) is the cheaper alternative: it
// would limit the retry to genuinely tiny text.

/// Retry small text at a bigger scale.
const ADAPTIVE_RETRY: bool = false;

/// Below this, retry at RETRY_UPSCALE.
const SMALL_GLYPH_PX: i32 = 32;

/// Upscale used on a small-glyph retry.
const RETRY_UPSCALE: i32 = 4;

/// True if the tallest word looks tiny.
///
/// Empty is not small: nothing to retry for.
fn glyphs_look_small(lines: &[OcrLine]) -> bool {
    lines
        .iter()
        .flat_map(|l| l.words.iter())
        .map(|w| w.rect.h)
        .max()
        .is_some_and(|max_h| max_h < SMALL_GLYPH_PX)
}

/// One region read, with provenance.
pub struct RegionRead {
    pub lines: Vec<OcrLine>,
    pub resolved: Option<Resolved>,
    /// Which backend path produced the pixels.
    pub source: &'static str,
    /// Why the preferred path was not used, if it was not.
    pub fallback: Option<String>,
}

/// The OCR knobs, reloadable.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SettingsSnapshot {
    pub max_passes: u8,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
}

/// Point in, text span out.
pub struct TextSource {
    capture: Box<dyn RegionCapture>,
    ocr: Box<dyn OcrEngine>,
    settings: SettingsSnapshot,
}

impl TextSource {
    pub fn new(
        capture: Box<dyn RegionCapture>,
        ocr: Box<dyn OcrEngine>,
        settings: SettingsSnapshot,
    ) -> Self {
        TextSource { capture, ocr, settings }
    }

    /// Swap in new OCR settings.
    pub fn apply_settings(&mut self, settings: SettingsSnapshot, language: &str) {
        self.ocr.set_language(language);
        self.settings = settings;
    }

    /// Lines plus the outcome.
    fn resolve_at_verbose(
        &mut self,
        cursor: PhysPoint,
        mask: CaptureMask,
    ) -> Result<(Vec<OcrLine>, Option<Resolved>)> {
        let read = self.resolve_in_region(
            cursor,
            region_around(cursor, self.settings.prefer_vertical, self.settings.capture),
            mask,
        )?;
        Ok((read.lines, read.resolved))
    }

    /// As above, explicit box.
    pub fn resolve_in_region(
        &mut self,
        cursor: PhysPoint,
        region: PhysRect,
        mask: CaptureMask,
    ) -> Result<RegionRead> {
        let (lines, frame) = self.recognise_at_capture(region, UPSCALE, mask)?;
        let (lines, frame) = if ADAPTIVE_RETRY && glyphs_look_small(&lines) {
            match self.recognise_at_capture(region, RETRY_UPSCALE, mask) {
                Ok((bigger, big_frame)) if !bigger.is_empty() => (bigger, big_frame),
                _ => (lines, frame),
            }
        } else {
            (lines, frame)
        };
        let resolved = resolve(&lines, cursor, self.settings.scan_alphanumeric);
        Ok(RegionRead {
            lines,
            resolved,
            source: frame.source,
            fallback: frame.fallback,
        })
    }

    /// Capture + recognise at `factor`, mapped to physical.
    ///
    /// `mask` is white-filled in the grabbed pixels before OCR sees them,
    /// and words that touch it are dropped on the way back out
    /// (ADR-0008).
    pub fn recognise_at_capture(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
    ) -> Result<(Vec<OcrLine>, Frame)> {
        let frame = grab_upscaled(self.capture.as_mut(), region, factor, mask)?;
        let raw = self.ocr.recognise(&frame.buf, frame.w, frame.h)?;
        let origin = PhysPoint { x: region.x, y: region.y };
        Ok((to_desktop(raw, origin, factor, mask), frame))
    }

    /// Tiled, scan rects dropped.
    pub fn resolve_at_tiled(
        &mut self,
        cursor: PhysPoint,
        mask: CaptureMask,
    ) -> Result<Option<Resolved>> {
        self.resolve_at_tiled_scanned(cursor, false, mask).map(|(r, _)| r)
    }

    /// Tiled read + scan rects. One logical read: brackets the backend's
    /// `begin_read`/`end_read` around every pass.
    ///
    /// `mask` is what OCR must not read - our own popup, on a live grab
    /// (ADR-0008) - and governs every pass of this one read.
    pub fn resolve_at_tiled_scanned(
        &mut self,
        cursor: PhysPoint,
        collect: bool,
        mask: CaptureMask,
    ) -> Result<(Option<Resolved>, Vec<ScanRect>)> {
        self.capture.begin_read();
        let out = self.resolve_tiled_inner(cursor, collect, mask);
        self.capture.end_read();
        out
    }

    fn resolve_tiled_inner(
        &mut self,
        cursor: PhysPoint,
        collect: bool,
        mask: CaptureMask,
    ) -> Result<(Option<Resolved>, Vec<ScanRect>)> {
        let (lines, resolved) = self.resolve_at_verbose(cursor, mask)?;
        let mut scan = Vec::new();
        let Some(first) = resolved else { return Ok((None, scan)) };
        if collect {
            scan.push(ScanRect {
                rect: region_around(cursor, self.settings.prefer_vertical, self.settings.capture),
                kind: ScanKind::Pass1,
            });
        }
        if self.settings.max_passes <= 1 {
            if collect {
                scan.push(ScanRect { rect: first.span.anchor, kind: ScanKind::Anchor });
            }
            return Ok((Some(first), scan));
        }

        // Pass 1's own kept tail; no re-read.
        let region = region_around(cursor, self.settings.prefer_vertical, self.settings.capture);
        let alnum = self.settings.scan_alphanumeric;
        let Some((head, tail_start, orientation)) = head_and_tail(&lines, cursor, region, alnum)
        else {
            if collect {
                scan.push(ScanRect { rect: first.span.anchor, kind: ScanKind::Anchor });
            }
            return Ok((Some(first), scan));
        };
        let head_chars = head.chars().count();

        let anchor = first.span.anchor;
        let band = band_of(anchor, orientation, self.settings.capture.short());
        let perpendicular_centre = match orientation {
            Orientation::Horizontal => anchor.center().y,
            Orientation::Vertical => anchor.center().x,
        };
        let line_tolerance = match orientation {
            Orientation::Horizontal => anchor.h / 2,
            Orientation::Vertical => anchor.w / 2,
        };
        let bounds = self.capture.bounds_containing(band.center());
        let max_tiles = usize::from(self.settings.max_passes - 1);

        let mut failed = false;
        let tail = tile_forward(
            band,
            tail_start,
            orientation,
            max_tiles,
            MAX_LOOKUP_CHARS.saturating_sub(head_chars),
            bounds,
            |tile| {
                if collect {
                    scan.push(ScanRect { rect: tile, kind: ScanKind::Tile });
                }
                match self.words_in(tile, perpendicular_centre, orientation, line_tolerance, mask)
                {
                    Ok(words) => words,
                    Err(e) => {
                        if !failed {
                            eprintln!("chibipop: tile capture failed, using what was read: {e:#}");
                            failed = true;
                        }
                        Vec::new()
                    }
                }
            },
        );

        if collect {
            scan.push(ScanRect { rect: anchor, kind: ScanKind::Anchor });
        }

        let text = normalise(&format!("{head}{tail}"));
        if text.is_empty() {
            return Ok((Some(first), scan));
        }
        Ok((
            Some(Resolved {
                // Stitched: no geometry.
                span: TextSpan {
                    text,
                    cursor_byte_offset: 0,
                    anchor,
                    geom: Vec::new(),
                },
                orientation,
            }),
            scan,
        ))
    }

    /// One capture; hovered line.
    fn words_in(
        &mut self,
        tile: PhysRect,
        perpendicular_centre: i32,
        orientation: Orientation,
        tolerance: i32,
        mask: CaptureMask,
    ) -> Result<Vec<OcrWord>> {
        let frame = grab_upscaled(self.capture.as_mut(), tile, UPSCALE, mask)?;
        let origin = PhysPoint { x: tile.x, y: tile.y };
        let raw = self.ocr.recognise(&frame.buf, frame.w, frame.h)?;
        let lines = to_desktop(raw, origin, UPSCALE, mask);
        Ok(nearest_line(&lines, perpendicular_centre, orientation, tolerance)
            .map(|line| line.words.clone())
            .unwrap_or_default())
    }
}

/// Image-pixel word boxes to desktop pixels, mask-touching words dropped.
///
/// The mask boundary is a capture edge (ADR-0008): the pixels under the
/// mask are flat white, so a word overlapping it was read off half a
/// glyph and goes the way of a word clipped at a tile edge - dropped,
/// never half-recognised. Lines left with no words are dropped too, so
/// [`OcrEngine`]'s "no words means nothing recognised" still holds.
fn to_desktop(
    raw: Vec<OcrLine>,
    origin: PhysPoint,
    factor: i32,
    mask: CaptureMask,
) -> Vec<OcrLine> {
    raw.into_iter()
        .map(|l| OcrLine {
            words: l
                .words
                .into_iter()
                .filter_map(|word| {
                    let rect = map_from_upscaled(word.rect, origin, factor);
                    if mask.hides(rect) {
                        None
                    } else {
                        Some(OcrWord { rect, text: word.text })
                    }
                })
                .collect(),
        })
        .filter(|l: &OcrLine| !l.words.is_empty())
        .collect()
}

/// Grab, mask, upscale by `factor`; BGRA.
///
/// Masked before the upscale: a quarter of the pixels to write at 2x, and
/// the nearest-neighbour blow-up carries the hard edge through exactly.
fn grab_upscaled(
    capture: &mut dyn RegionCapture,
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
) -> Result<Frame> {
    let mut frame = capture.grab(region)?;
    if frame.w != region.w || frame.h != region.h {
        anyhow::bail!(
            "capture is {}x{}, wanted {}x{}",
            frame.w,
            frame.h,
            region.w,
            region.h
        );
    }
    let need = (region.w as usize)
        .checked_mul(region.h as usize)
        .and_then(|n| n.checked_mul(4))
        .context("region too large")?;
    if frame.buf.len() < need {
        anyhow::bail!("capture is short: {} < {need}", frame.buf.len());
    }
    mask.apply(&mut frame.buf, region.w, region.h, region);
    let (buf, w, h) = upscale_by(&frame.buf, region.w, region.h, factor);
    Ok(Frame { buf, w, h, ..frame })
}

/// Nearest-neighbour upscale by `factor`.
fn upscale_by(src: &[u8], w: i32, h: i32, factor: i32) -> (Vec<u8>, i32, i32) {
    let (w2, h2) = (w * factor, h * factor);
    let mut dst = vec![0u8; (w2 as usize) * (h2 as usize) * 4];
    for y in 0..h2 as usize {
        let sy = y / factor as usize;
        for x in 0..w2 as usize {
            let sx = x / factor as usize;
            let si = (sy * w as usize + sx) * 4;
            let di = (y * w2 as usize + x) * 4;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
            dst[di + 3] = 0xFF;
        }
    }
    (dst, w2, h2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::mask::CaptureMode;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn upscale_by_doubles_a_2x2_pixel() {
        let src = [255u8, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 255, 255, 0, 0];
        let (dst, w2, h2) = upscale_by(&src, 2, 2, 2);
        assert_eq!((4, 4), (w2, h2));
        assert_eq!(dst.len(), 4 * 4 * 4);
        // Top-left source pixel fills a 2x2 block.
        assert_eq!(&dst[0..4], &[255, 0, 0, 255]);
        assert_eq!(&dst[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn upscale_by_one_is_a_pass_through_size() {
        let src = vec![10u8; 3 * 3 * 4];
        let (dst, w2, h2) = upscale_by(&src, 3, 3, 1);
        assert_eq!((3, 3), (w2, h2));
        assert_eq!(dst.len(), src.len());
    }

    #[test]
    fn upscale_by_four_quadruples_each_dimension() {
        let src = vec![1u8; 2 * 2 * 4];
        let (_, w2, h2) = upscale_by(&src, 2, 2, 4);
        assert_eq!((8, 8), (w2, h2));
    }

    fn word(text: &str, h: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x: 0, y: 0, w: h, h } }
    }

    fn line(words: Vec<OcrWord>) -> OcrLine {
        OcrLine { words }
    }

    #[test]
    fn empty_lines_are_not_small() {
        assert!(!glyphs_look_small(&[]));
    }

    #[test]
    fn a_line_of_only_short_words_is_small() {
        let lines = [line(vec![word("し", 27), word("な", 29)])];
        assert!(glyphs_look_small(&lines));
    }

    #[test]
    fn the_ceiling_itself_is_not_small() {
        let lines = [line(vec![word("大", SMALL_GLYPH_PX)])];
        assert!(!glyphs_look_small(&lines));
    }

    #[test]
    fn one_past_the_ceiling_is_not_small() {
        let lines = [line(vec![word("大", SMALL_GLYPH_PX + 1)])];
        assert!(!glyphs_look_small(&lines));
    }

    #[test]
    fn one_under_the_ceiling_is_small() {
        let lines = [line(vec![word("大", SMALL_GLYPH_PX - 1)])];
        assert!(glyphs_look_small(&lines));
    }

    /// One tall word rescues the whole region.
    #[test]
    fn a_single_tall_word_among_small_ones_is_not_small() {
        let lines = [
            line(vec![word("し", 27), word("な", 29)]),
            line(vec![word("大", 40)]),
        ];
        assert!(!glyphs_look_small(&lines));
    }

    // -- the capture mask at the seam (ADR-0008) --

    /// Every byte of every grab, so a mask is visible as a change.
    const DESK: u8 = 0x20;

    /// Region-sized frames of flat [`DESK`].
    struct SolidCapture;

    impl RegionCapture for SolidCapture {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            Ok(Frame {
                buf: vec![DESK; (region.w * region.h * 4) as usize],
                w: region.w,
                h: region.h,
                source: "solid",
                fallback: None,
            })
        }

        fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
            PhysRect { x: p.x - 1000, y: p.y - 1000, w: 2000, h: 2000 }
        }
    }

    /// The last image handed to OCR: its pixels, width, height.
    type SeenImage = Rc<RefCell<Option<(Vec<u8>, i32, i32)>>>;

    /// Keeps the pixels it was handed; reports fixed boxes.
    struct RecordingOcr {
        seen: SeenImage,
        /// Image-pixel word boxes to report, one per line.
        words: Vec<PhysRect>,
    }

    impl OcrEngine for RecordingOcr {
        fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
            *self.seen.borrow_mut() = Some((bgra.to_vec(), w, h));
            Ok(self
                .words
                .iter()
                .enumerate()
                .map(|(i, rect)| OcrLine {
                    words: vec![OcrWord { text: format!("w{i}"), rect: *rect }],
                })
                .collect())
        }

        fn set_language(&mut self, _tag: &str) {}
    }

    fn snap() -> SettingsSnapshot {
        SettingsSnapshot {
            max_passes: 1,
            prefer_vertical: false,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
        }
    }

    /// A source over the fakes, plus the handle on what OCR saw.
    fn recording(words: Vec<PhysRect>) -> (TextSource, SeenImage) {
        let seen = Rc::new(RefCell::new(None));
        let ocr = RecordingOcr { seen: Rc::clone(&seen), words };
        (TextSource::new(Box::new(SolidCapture), Box::new(ocr), snap()), seen)
    }

    fn live(popup: PhysRect) -> CaptureMask {
        CaptureMask::for_mode(CaptureMode::Live, Some(popup))
    }

    /// One pixel of what OCR was handed, as BGRA.
    fn px(buf: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// White fill, hard edge, exactly over the popup - and nowhere else.
    #[test]
    fn the_popup_reaches_ocr_as_flat_white() {
        let region = PhysRect { x: 100, y: 200, w: 8, h: 6 };
        // Overlaps the region's columns 2..5, rows 1..3.
        let popup = PhysRect { x: 102, y: 201, w: 3, h: 2 };
        let (mut source, seen) = recording(Vec::new());

        source.recognise_at_capture(region, 1, live(popup)).unwrap();

        let (buf, w, h) = seen.borrow_mut().take().expect("OCR must have been called");
        assert_eq!((8, 6), (w, h));
        for y in 0..h {
            for x in 0..w {
                let masked = (2..5).contains(&x) && (1..3).contains(&y);
                let want = if masked { [0xFF; 4] } else { [DESK, DESK, DESK, 0xFF] };
                assert_eq!(want, px(&buf, w, x, y), "pixel ({x},{y})");
            }
        }
    }

    /// The mask lands in the frame, not on the desktop: a region whose
    /// origin is far from zero must still be masked in the right place.
    #[test]
    fn the_fill_is_placed_in_frame_local_pixels() {
        let region = PhysRect { x: 1000, y: 900, w: 4, h: 4 };
        let popup = PhysRect { x: 1000, y: 900, w: 1, h: 1 };
        let (mut source, seen) = recording(Vec::new());

        source.recognise_at_capture(region, 1, live(popup)).unwrap();

        let (buf, w, _) = seen.borrow_mut().take().unwrap();
        assert_eq!([0xFF; 4], px(&buf, w, 0, 0), "the region's own top-left is the mask");
        assert_eq!([DESK, DESK, DESK, 0xFF], px(&buf, w, 1, 0), "one pixel over is not");
    }

    /// Maskless: the pixels reach OCR exactly as grabbed.
    #[test]
    fn a_maskless_read_hands_ocr_the_untouched_grab() {
        let region = PhysRect { x: 0, y: 0, w: 4, h: 4 };
        let popup = PhysRect { x: 1, y: 1, w: 2, h: 2 };
        let (mut source, seen) = recording(Vec::new());

        // Frozen: the grab predates the popup, so the rect masks nothing.
        let frozen = CaptureMask::for_mode(CaptureMode::Frozen, Some(popup));
        source.recognise_at_capture(region, 1, frozen).unwrap();

        let (buf, _, _) = seen.borrow_mut().take().unwrap();
        let untouched: Vec<u8> = (0..4 * 4).flat_map(|_| [DESK, DESK, DESK, 0xFF]).collect();
        assert_eq!(untouched, buf, "a frozen grab is handed over as it came");
    }

    /// The mask boundary is a capture edge: touching words are dropped.
    #[test]
    fn words_touching_the_mask_are_dropped_and_the_rest_survive() {
        let region = PhysRect { x: 0, y: 0, w: 40, h: 20 };
        let popup = PhysRect { x: 10, y: 0, w: 10, h: 20 };
        let words = vec![
            PhysRect { x: 0, y: 0, w: 8, h: 10 },  // clear of the mask
            PhysRect { x: 12, y: 0, w: 4, h: 10 }, // wholly inside it
            PhysRect { x: 8, y: 0, w: 6, h: 10 },  // straddling its edge
            PhysRect { x: 20, y: 0, w: 8, h: 10 }, // flush against it
        ];
        let (mut source, _seen) = recording(words);

        let (lines, _) = source.recognise_at_capture(region, 1, live(popup)).unwrap();

        let kept: Vec<&str> =
            lines.iter().flat_map(|l| l.words.iter().map(|w| w.text.as_str())).collect();
        assert_eq!(
            vec!["w0", "w3"],
            kept,
            "only the words with no mask overlap survive; a shared edge is no overlap"
        );
        assert_eq!(2, lines.len(), "lines emptied by the mask are dropped, not returned empty");
    }

    /// The same words, maskless: nothing is dropped.
    #[test]
    fn a_maskless_read_drops_no_words() {
        let region = PhysRect { x: 0, y: 0, w: 40, h: 20 };
        let words = vec![
            PhysRect { x: 0, y: 0, w: 8, h: 10 },
            PhysRect { x: 12, y: 0, w: 4, h: 10 },
        ];
        let (mut source, _seen) = recording(words);

        let (lines, _) = source.recognise_at_capture(region, 1, CaptureMask::NONE).unwrap();

        assert_eq!(2, lines.len());
    }

    /// Masked before the upscale, so the hard edge lands on the 2x grid.
    #[test]
    fn the_fill_survives_the_upscale_as_a_hard_edge() {
        let region = PhysRect { x: 0, y: 0, w: 4, h: 2 };
        let popup = PhysRect { x: 2, y: 0, w: 2, h: 2 };
        let (mut source, seen) = recording(Vec::new());

        source.recognise_at_capture(region, 2, live(popup)).unwrap();

        let (buf, w, h) = seen.borrow_mut().take().unwrap();
        assert_eq!((8, 4), (w, h), "OCR sees the upscaled image");
        for y in 0..h {
            assert_eq!([DESK, DESK, DESK, 0xFF], px(&buf, w, 3, y), "just left of the mask");
            assert_eq!([0xFF; 4], px(&buf, w, 4, y), "first upscaled masked column");
        }
    }
}

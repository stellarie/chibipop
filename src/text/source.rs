//! The core-internal facade over `RegionCapture` + `OcrEngine` + the shared
//! layout/hit-scan logic: point in, text span out (ADR-0001). Platform code
//! supplies the two seams; everything below them is shared.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::layout::{
    band_of, head_and_tail, map_from_upscaled, nearest_line, normalise, region_around, resolve,
    tile_forward, CaptureSize, OcrLine, OcrWord, Orientation, Resolved,
};
use crate::text::frozen::FrozenFrame;
use crate::text::mask::CaptureMask;
use crate::text::{Frame, OcrEngine, RegionCapture, TextSpan};
use anyhow::{Context, Result};

// The capture upscale factor is per-platform and lives in
// `SettingsSnapshot::upscale`: the Windows engine misreads small text
// at native resolution (it supplies 2), the Linux engine measures
// strictly worse on upscaled crops and never upscales (ADR-0009 - it
// supplies 1).

// MAINTAINER NOTE - adaptive upscale retry, disabled 2026-08-08.
// (Deliberately longer than the 30-char house rule: this records a
// method and a retraction, and Stella asked for it to live here.)
//
// What it does: after the first pass at the configured upscale, if the
// tallest word
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
    /// The pixels were the previous grab's, so the OCR was reused.
    pub unchanged: bool,
}

/// One region's words, kept for an unchanged re-grab.
///
/// Word rects are already mapped to physical, so a reuse is a clone
/// and nothing else.
///
/// The key is `(region, factor, mask)`, all three. `unchanged` is the
/// backend's answer about the *raw* pixels it copied, but what OCR read
/// was those pixels after masking, and what came back was filtered by
/// the same mask - so a popup appearing over an otherwise still region
/// is an unchanged grab whose question changed. Keying on the mask too
/// is what stops that from serving unmasked words to a masked read, and
/// vice versa. The mask is stored already clipped to `region`
/// ([`CaptureMask::clipped_to`]), so a popup that moved somewhere this
/// box cannot see does not cost a pass.
struct Recognised {
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
    lines: Vec<OcrLine>,
}

/// The OCR knobs, reloadable.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SettingsSnapshot {
    pub max_passes: u8,
    /// Nearest-neighbour factor applied to every grab before OCR sees
    /// it. A platform fact, not a user knob: 2 on Windows (WinRT OCR
    /// misreads small text at 1x), 1 on Linux (ADR-0009 - meikiocr is
    /// strictly worse on upscaled crops).
    pub upscale: i32,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
}

/// What trigger mode's press-time grab left behind.
enum Frozen {
    /// The frame every lookup in this hold reads (ADR-0010).
    Held(FrozenFrame),
    /// The press-time grab failed. Trigger mode without a frozen frame
    /// is not trigger mode: every lookup in the hold says so, rather
    /// than quietly reading a live grab whose pixels our own popup
    /// would be in (ADR-0008).
    Failed(String),
}

/// Point in, text span out.
pub struct TextSource {
    capture: Box<dyn RegionCapture>,
    ocr: Box<dyn OcrEngine>,
    settings: SettingsSnapshot,
    /// The trigger hold's press-time grab, while one is held. Every
    /// grab is served out of it and the backend is not touched at all
    /// (ADR-0010).
    frozen: Option<Frozen>,
    /// What this read has recognised.
    recognised: Vec<Recognised>,
    /// What the previous read did, for a dwell re-check to reuse
    /// (ADR-0010): a damage-paced backend answers an unchanged region
    /// with the same pixels, and OCR of the same pixels is the same
    /// answer. Two generations, so one read's tiles cannot evict each
    /// other; both are bounded by the passes one read makes.
    previous: Vec<Recognised>,
}

impl TextSource {
    pub fn new(
        capture: Box<dyn RegionCapture>,
        ocr: Box<dyn OcrEngine>,
        settings: SettingsSnapshot,
    ) -> Self {
        TextSource {
            capture,
            ocr,
            settings,
            frozen: None,
            recognised: Vec::new(),
            previous: Vec::new(),
        }
    }

    /// Swap in new OCR settings.
    pub fn apply_settings(&mut self, settings: SettingsSnapshot, language: &str) {
        self.ocr.set_language(language);
        self.settings = settings;
        // Another language or capture size is another answer.
        self.recognised.clear();
        self.previous.clear();
    }

    /// Freeze on the output holding `at`: one full grab now, read by
    /// every lookup until [`TextSource::thaw`] (ADR-0010).
    ///
    /// Answers the box it froze. A failed grab is remembered too: the
    /// hold then reports it rather than falling back to live pixels,
    /// which in trigger mode nothing masks.
    pub fn freeze(&mut self, at: PhysPoint) -> Result<PhysRect> {
        // Frozen pixels are a different answer to the same question, so
        // no earlier pass may be reused across the edge.
        self.recognised.clear();
        self.previous.clear();
        match FrozenFrame::take(self.capture.as_mut(), at) {
            Ok(frame) => {
                let region = frame.region();
                self.frozen = Some(Frozen::Held(frame));
                Ok(region)
            }
            Err(e) => {
                self.frozen = Some(Frozen::Failed(format!("{e:#}")));
                Err(e)
            }
        }
    }

    /// Drop the hold's frozen frame; grabs go live again.
    pub fn thaw(&mut self) {
        if self.frozen.take().is_some() {
            self.recognised.clear();
            self.previous.clear();
        }
    }

    /// The box the hold's frozen grab covers, if one is held.
    pub fn frozen_region(&self) -> Option<PhysRect> {
        match &self.frozen {
            Some(Frozen::Held(f)) => Some(f.region()),
            _ => None,
        }
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
        let (lines, frame) = self.recognise_at_capture(region, self.settings.upscale, mask)?;
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
            unchanged: frame.unchanged,
        })
    }

    /// Capture + recognise at `factor`, mapped to physical.
    ///
    /// `mask` is white-filled in the grabbed pixels before OCR sees them,
    /// and words that touch it are dropped on the way back out
    /// (ADR-0008). A frozen hold answers with no mask at all, whatever
    /// the caller asked for: those pixels predate the popup, so there is
    /// nothing in them to hide (ADR-0010).
    ///
    /// A backend that says the pixels are unchanged (ADR-0002's damage
    /// race) skips the recogniser entirely: same pixels, same words. The
    /// mask is part of that "same", because the pixels OCR sees are the
    /// grab *after* masking - see [`CaptureMask::clipped_to`].
    pub fn recognise_at_capture(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
    ) -> Result<(Vec<OcrLine>, Frame)> {
        // One value governs the fill, the word drop and the reuse key,
        // so "same pixels, same words" can never drift from what was
        // actually masked.
        let mask = match self.frozen {
            Some(_) => CaptureMask::NONE,
            None => mask.clipped_to(region),
        };
        let frame = self.grab(region, factor, mask)?;
        if frame.unchanged {
            if let Some(lines) = self.reuse(region, factor, mask) {
                return Ok((lines, frame));
            }
        }
        let raw = self.ocr.recognise(&frame.buf, frame.w, frame.h)?;
        let origin = PhysPoint { x: region.x, y: region.y };
        let lines = to_desktop(raw, origin, factor, mask);
        self.remember(region, factor, mask, &lines);
        Ok((lines, frame))
    }

    /// This pass's pixels: the hold's frozen frame, or a live grab.
    fn grab(&mut self, region: PhysRect, factor: i32, mask: CaptureMask) -> Result<Frame> {
        match &mut self.frozen {
            Some(Frozen::Held(f)) => finish_grab(f.crop(region), region, factor, mask),
            // The hold has no pixels, so it has no lookups either.
            Some(Frozen::Failed(why)) => Err(anyhow::anyhow!(why.clone())),
            None => grab_upscaled(self.capture.as_mut(), region, factor, mask),
        }
    }

    /// This region's words from an earlier pass, if any pass asked the
    /// same question - same box, same scale, same mask.
    ///
    /// A hit is promoted into this read's generation, so a dwell that
    /// never re-OCRs never forgets either.
    fn reuse(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
    ) -> Option<Vec<OcrLine>> {
        let same =
            |r: &&Recognised| r.region == region && r.factor == factor && r.mask == mask;
        if let Some(hit) = self.recognised.iter().find(same) {
            return Some(hit.lines.clone());
        }
        let hit = self.previous.iter().find(same)?;
        let lines = hit.lines.clone();
        self.recognised.push(Recognised { region, factor, mask, lines: lines.clone() });
        Some(lines)
    }

    /// Keep this pass's words for the next read.
    fn remember(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
        lines: &[OcrLine],
    ) {
        let entry = Recognised { region, factor, mask, lines: lines.to_vec() };
        let slot = self
            .recognised
            .iter_mut()
            .find(|r| r.region == region && r.factor == factor && r.mask == mask);
        match slot {
            Some(slot) => *slot = entry,
            None => self.recognised.push(entry),
        }
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
    /// A frozen hold brackets nothing, because it touches no backend:
    /// every pass is a crop of the press-time frame (ADR-0010), and
    /// arming a damage race around pixels nobody is reading would cost
    /// wakeups for an answer the hold ignores.
    ///
    /// `mask` is what OCR must not read - our own popup, on a live grab
    /// (ADR-0008) - and governs every pass of this one read.
    pub fn resolve_at_tiled_scanned(
        &mut self,
        cursor: PhysPoint,
        collect: bool,
        mask: CaptureMask,
    ) -> Result<(Option<Resolved>, Vec<ScanRect>)> {
        let live = self.frozen.is_none();
        if live {
            self.capture.begin_read();
        }
        // One read, one generation: the previous read's passes stay
        // reusable, older ones go.
        self.previous = std::mem::take(&mut self.recognised);
        let out = self.resolve_tiled_inner(cursor, collect, mask);
        if live {
            self.capture.end_read();
        }
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
        let (lines, _) = self.recognise_at_capture(tile, self.settings.upscale, mask)?;
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
fn grab_upscaled(
    capture: &mut dyn RegionCapture,
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
) -> Result<Frame> {
    let frame = capture.grab(region)?;
    finish_grab(frame, region, factor, mask)
}

/// Shape-check, mask and upscale one grabbed frame - live or frozen.
///
/// Masked before the upscale: a quarter of the pixels to write at 2x, and
/// the nearest-neighbour blow-up carries the hard edge through exactly.
fn finish_grab(
    mut frame: Frame,
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
) -> Result<Frame> {
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
                unchanged: false,
            })
        }

        fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
            PhysRect { x: p.x - 1000, y: p.y - 1000, w: 2000, h: 2000 }
        }
    }

    /// A capture whose `unchanged` flag the test drives, so the reuse
    /// path can be exercised without a compositor.
    struct Paced {
        unchanged: bool,
        grabs: std::cell::Cell<u32>,
    }

    impl RegionCapture for Paced {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            self.grabs.set(self.grabs.get() + 1);
            Ok(Frame {
                buf: vec![0u8; (region.w * region.h * 4) as usize],
                w: region.w,
                h: region.h,
                source: "paced",
                fallback: None,
                unchanged: self.unchanged,
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

    /// Counts how often the recogniser actually ran.
    #[derive(Default)]
    struct Counting {
        runs: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl OcrEngine for Counting {
        fn recognise(&self, _bgra: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            self.runs.set(self.runs.get() + 1);
            Ok(vec![OcrLine {
                words: vec![OcrWord {
                    text: "本".to_string(),
                    rect: PhysRect { x: 0, y: 0, w: 40, h: 40 },
                }],
            }])
        }

        fn set_language(&mut self, _tag: &str) {}
    }

    fn snap() -> SettingsSnapshot {
        SettingsSnapshot {
            max_passes: 1,
            upscale: 2,
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

    /// The factor is the snapshot's, not a core constant: an upscale-1
    /// platform (Linux, ADR-0009) must put native-resolution pixels in
    /// front of its engine, and an upscale-2 one (Windows) doubled
    /// ones - through the resolve path a real lookup takes, not just
    /// `recognise_at_capture`'s explicit factor argument.
    #[test]
    fn resolve_reads_at_the_snapshot_upscale() {
        let region = PhysRect { x: 0, y: 0, w: 4, h: 2 };
        for (factor, expect) in [(1, (4, 2)), (2, (8, 4))] {
            let seen = Rc::new(RefCell::new(None));
            let ocr = RecordingOcr { seen: Rc::clone(&seen), words: Vec::new() };
            let mut source = TextSource::new(
                Box::new(SolidCapture),
                Box::new(ocr),
                SettingsSnapshot { upscale: factor, ..snap() },
            );
            source.resolve_in_region(PhysPoint { x: 1, y: 1 }, region, CaptureMask::NONE).unwrap();
            let (_, w, h) = seen.borrow_mut().take().expect("OCR must have run");
            assert_eq!(expect, (w, h), "factor {factor}");
        }
    }

    // -- reusing an unchanged grab's words (ADR-0002/ADR-0010) --

    fn paced(unchanged: bool) -> (TextSource, std::rc::Rc<std::cell::Cell<u32>>) {
        let runs = std::rc::Rc::new(std::cell::Cell::new(0));
        let source = TextSource::new(
            Box::new(Paced { unchanged, grabs: std::cell::Cell::new(0) }),
            Box::new(Counting { runs: runs.clone() }),
            SettingsSnapshot {
                max_passes: 1,
                upscale: 2,
                prefer_vertical: false,
                capture: CaptureSize::default(),
                scan_alphanumeric: false,
            },
        );
        (source, runs)
    }

    const BOX: PhysRect = PhysRect { x: 100, y: 100, w: 80, h: 40 };
    const OTHER: PhysRect = PhysRect { x: 400, y: 100, w: 80, h: 40 };
    const AT: PhysPoint = PhysPoint { x: 120, y: 110 };

    /// The whole point of the damage race: unchanged pixels must not be
    /// recognised twice (ADR-0002).
    #[test]
    fn unchanged_pixels_reuse_the_words_already_recognised() {
        let (mut source, runs) = paced(true);
        let first = source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1, "the first read must recognise");
        assert!(!first.lines.is_empty());

        let again = source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("second read");
        assert_eq!(runs.get(), 1, "an unchanged region must not be recognised again");
        assert_eq!(again.lines, first.lines, "and must answer the same words");
        assert!(again.unchanged, "the signal must reach the caller");
    }

    #[test]
    fn changed_pixels_are_always_recognised() {
        let (mut source, runs) = paced(false);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        let again = source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("second read");
        assert_eq!(runs.get(), 2);
        assert!(!again.unchanged);
    }

    /// `unchanged` is a hint, never a promise that words are held: a
    /// region never recognised must still be recognised.
    #[test]
    fn an_unchanged_region_with_nothing_held_is_still_recognised() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        source.resolve_in_region(AT, OTHER, CaptureMask::NONE).expect("a different box");
        assert_eq!(runs.get(), 2, "a box never read cannot be reused");
    }

    /// A dwell through the real read bracket: last read's words must
    /// survive into this one, or a static screen re-OCRs every period
    /// (ADR-0010's dwell re-check would cost what it saves).
    #[test]
    fn a_dwell_through_the_read_bracket_reuses_the_previous_read() {
        let (mut source, runs) = paced(true);
        source.resolve_at_tiled(AT, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1, "the first read must recognise");
        for _ in 0..4 {
            source.resolve_at_tiled(AT, CaptureMask::NONE).expect("dwell read");
        }
        assert_eq!(runs.get(), 1, "a static dwell must never recognise again");
    }

    /// Several passes in one read must not evict each other: a tiled
    /// read asks for a handful of boxes, and all of them dwell.
    #[test]
    fn two_regions_in_one_read_are_both_kept() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("pass 1");
        source.resolve_in_region(AT, OTHER, CaptureMask::NONE).expect("tile");
        assert_eq!(runs.get(), 2);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("pass 1 again");
        source.resolve_in_region(AT, OTHER, CaptureMask::NONE).expect("tile again");
        assert_eq!(runs.get(), 2, "neither region may evict the other");
    }

    /// New settings mean new answers: nothing held may survive them.
    #[test]
    fn new_settings_drop_everything_held() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1);
        let settings = SettingsSnapshot {
            max_passes: 2,
            upscale: 2,
            prefer_vertical: true,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
        };
        source.apply_settings(settings, "ja");
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("read after reload");
        assert_eq!(runs.get(), 2, "a reload must re-recognise");
    }

    // -- where the two features meet: the mask is part of "same words" --

    /// `Counting`'s one word maps to this box out of `BOX` at the
    /// snapshot's upscale of 2.
    const WORD: PhysRect = PhysRect { x: 100, y: 100, w: 20, h: 20 };

    /// The dangerous direction, and the reason the mask is in the key:
    /// the backend compares *raw* pixels, so a popup appearing over a
    /// still region is an unchanged grab whose masked pixels changed.
    /// Serving the held words there would hand the app its own popup
    /// text - exactly what ADR-0008 exists to prevent.
    #[test]
    fn an_unchanged_regrab_under_a_new_mask_is_recognised_again() {
        let (mut source, runs) = paced(true);
        let bare = source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1);
        assert_eq!(1, bare.lines.len(), "the word is there when nothing is masked");

        // The popup lands on the word: same raw pixels, different question.
        let over_the_word = live(WORD.inflated(4, 4));
        let masked = source.resolve_in_region(AT, BOX, over_the_word).expect("masked read");

        assert_eq!(runs.get(), 2, "a new mask must not be answered from the old words");
        assert!(
            masked.lines.is_empty(),
            "the masked word must be dropped, never served from the unmasked pass"
        );
    }

    /// And back the other way: words recognised under a mask must not be
    /// served to a read that masks nothing, or the popup's shadow
    /// outlives it.
    #[test]
    fn an_unchanged_regrab_that_drops_its_mask_is_recognised_again() {
        let (mut source, runs) = paced(true);
        let over_the_word = live(WORD.inflated(4, 4));
        let masked = source.resolve_in_region(AT, BOX, over_the_word).expect("masked read");
        assert_eq!(runs.get(), 1);
        assert!(masked.lines.is_empty(), "the word is masked away");

        let bare = source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("maskless read");
        assert_eq!(runs.get(), 2, "dropping the mask is a different question");
        assert_eq!(1, bare.lines.len(), "the word must come back");
    }

    /// The same mask twice still reuses: the rule keys on the question,
    /// not on whether a mask exists.
    #[test]
    fn an_unchanged_regrab_under_the_same_mask_reuses() {
        let (mut source, runs) = paced(true);
        let popup = live(WORD.inflated(4, 4));
        source.resolve_in_region(AT, BOX, popup).expect("first read");
        assert_eq!(runs.get(), 1);
        source.resolve_in_region(AT, BOX, popup).expect("second read");
        assert_eq!(runs.get(), 1, "same box, same mask, same pixels: same answer");
    }

    /// The case the reuse exists for. A popup is placed after the first
    /// hover, so the mask changes - but it is nowhere near this box, and
    /// a mask that does not reach a grab does not change it. Keying on
    /// the *clipped* mask is what keeps the dwell re-check cheap here.
    #[test]
    fn a_popup_that_never_reaches_the_box_does_not_spoil_the_reuse() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("hover");
        assert_eq!(runs.get(), 1);

        let elsewhere = live(PhysRect { x: 2000, y: 2000, w: 300, h: 200 });
        let again = source.resolve_in_region(AT, BOX, elsewhere).expect("dwell re-check");

        assert_eq!(runs.get(), 1, "a popup outside the box is the same question");
        assert_eq!(1, again.lines.len());
    }

    /// Two boxes, one popup that only reaches one of them: the box it
    /// misses keeps reusing, the box it covers does not.
    #[test]
    fn a_popup_spoils_only_the_boxes_it_actually_covers() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("box");
        source.resolve_in_region(AT, OTHER, CaptureMask::NONE).expect("other box");
        assert_eq!(runs.get(), 2);

        let over_box_only = live(BOX);
        source.resolve_in_region(AT, OTHER, over_box_only).expect("other box again");
        assert_eq!(runs.get(), 2, "the uncovered box still reuses");
        source.resolve_in_region(AT, BOX, over_box_only).expect("covered box again");
        assert_eq!(runs.get(), 3, "the covered box must be recognised afresh");
    }

    // -- trigger mode's frozen hold (ADR-0010) --

    /// A backend whose pixels differ on every grab, so a frozen read
    /// can be told from a live one, and whose grabs are counted through
    /// a handle the test keeps. Can be made to refuse the copy.
    struct Moving {
        grabs: Rc<std::cell::Cell<u8>>,
        fails: bool,
    }

    impl RegionCapture for Moving {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            anyhow::ensure!(!self.fails, "the compositor refused the copy");
            self.grabs.set(self.grabs.get() + 1);
            Ok(Frame {
                // Nth grab, so the pixels say which one they came from.
                buf: vec![0x10 + self.grabs.get(); (region.w * region.h * 4) as usize],
                w: region.w,
                h: region.h,
                source: "moving",
                fallback: None,
                unchanged: false,
            })
        }

        fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
            OUTPUT
        }
    }

    /// The one output `Moving` knows about.
    const OUTPUT: PhysRect = PhysRect { x: 0, y: 0, w: 600, h: 400 };

    /// A source over `Moving`, what OCR was shown, and the grab count.
    /// `words` are the image-pixel boxes OCR reports, one line each.
    fn moving(
        fails: bool,
        words: Vec<PhysRect>,
    ) -> (TextSource, SeenImage, Rc<std::cell::Cell<u8>>) {
        let seen = Rc::new(RefCell::new(None));
        let grabs = Rc::new(std::cell::Cell::new(0));
        let ocr = RecordingOcr { seen: Rc::clone(&seen), words };
        let capture = Moving { grabs: Rc::clone(&grabs), fails };
        (TextSource::new(Box::new(capture), Box::new(ocr), snap()), seen, grabs)
    }

    /// Which grab OCR was shown the pixels of. Alpha is the upscale's
    /// own, so only the colour bytes carry the backend's answer.
    fn shown_grab(seen: &SeenImage) -> u8 {
        let (buf, _, _) = seen.borrow_mut().take().expect("OCR ran");
        let colours: Vec<u8> =
            buf.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let first = colours[0];
        assert!(colours.iter().all(|&b| b == first), "one grab's pixels are one value");
        first - 0x10
    }

    /// The freeze itself: one grab of the whole output holding the point.
    #[test]
    fn a_freeze_takes_one_full_output_grab() {
        let (mut source, _seen, grabs) = moving(false, Vec::new());
        assert_eq!(None, source.frozen_region(), "nothing is frozen to begin with");
        assert_eq!(OUTPUT, source.freeze(PhysPoint { x: 300, y: 200 }).expect("the freeze"));
        assert_eq!(Some(OUTPUT), source.frozen_region());
        assert_eq!(1, grabs.get(), "one press, one copy");
    }

    /// What "frozen" means: the screen moves on, the hold does not.
    #[test]
    fn a_frozen_hold_reads_the_press_time_pixels_and_no_others() {
        let (mut source, seen, _grabs) = moving(false, Vec::new());
        source.freeze(PhysPoint { x: 300, y: 200 }).expect("the freeze");
        source.recognise_at_capture(BOX, 1, CaptureMask::NONE).expect("a read in the hold");
        assert_eq!(1, shown_grab(&seen), "the press-time grab, not a later one");
    }

    /// A hold copies nothing and arms nothing: every pass is a crop of
    /// the one press-time frame.
    #[test]
    fn a_frozen_hold_never_touches_the_backend_again() {
        let (mut source, seen, grabs) = moving(false, Vec::new());
        source.freeze(AT).expect("the freeze");
        for _ in 0..3 {
            source.resolve_at_tiled(AT, CaptureMask::NONE).expect("a read in the hold");
        }
        source.recognise_at_capture(OTHER, 1, CaptureMask::NONE).expect("another box");
        assert_eq!(1, grabs.get(), "the whole hold costs one copy");
        assert_eq!(1, shown_grab(&seen), "every box comes out of that copy");
    }

    /// The mask is the caller's belief about a live grab; frozen pixels
    /// predate the popup, so the belief is ignored rather than obeyed.
    /// This is the read-through property, at the seam that decides it.
    #[test]
    fn a_mask_over_a_frozen_hold_is_ignored() {
        let (mut source, seen, _grabs) = moving(false, vec![PhysRect { x: 0, y: 0, w: 20, h: 20 }]);
        source.freeze(AT).expect("the freeze");
        // A popup right over the box we are about to read.
        let read = source.resolve_in_region(AT, BOX, live(BOX)).expect("a read under the popup");
        assert_eq!(1, shown_grab(&seen), "no white fill may reach a frozen read");
        assert_eq!(1, read.lines.len(), "and no word may be dropped for touching it");
    }

    /// Release drops the frame: the next grab is the screen's again.
    #[test]
    fn a_thaw_returns_the_source_to_live_grabs() {
        let (mut source, seen, grabs) = moving(false, Vec::new());
        source.freeze(AT).expect("the freeze");
        source.thaw();
        assert_eq!(None, source.frozen_region());
        source.recognise_at_capture(BOX, 1, CaptureMask::NONE).expect("a live read");
        assert_eq!(2, grabs.get(), "a live read copies again");
        assert_eq!(2, shown_grab(&seen), "and OCR sees the newer pixels");
    }

    /// Trigger mode without a frozen frame is not trigger mode: a
    /// press-time grab that failed must fail the hold's lookups rather
    /// than quietly serve live pixels nothing is masking.
    #[test]
    fn a_failed_press_time_grab_fails_every_lookup_in_the_hold() {
        let (mut source, _seen, _grabs) = moving(true, Vec::new());
        let Err(e) = source.freeze(AT) else { panic!("a refusing backend cannot freeze") };
        assert!(format!("{e:#}").contains("refused the copy"), "{e:#}");

        let Err(e) = source.resolve_in_region(AT, BOX, CaptureMask::NONE) else {
            panic!("a hold with no pixels must not answer a lookup")
        };
        assert!(format!("{e:#}").contains("refused the copy"), "{e:#}");
    }

    /// A freeze is a different answer to the same question, so words
    /// recognised live must not be served out of the frozen frame.
    #[test]
    fn a_freeze_drops_the_words_recognised_before_it() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("a live read");
        assert_eq!(runs.get(), 1);
        source.freeze(AT).expect("the freeze");
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("a read in the hold");
        assert_eq!(runs.get(), 2, "frozen pixels must be recognised for themselves");
        source.thaw();
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("a live read again");
        assert_eq!(runs.get(), 3, "and the live pixels again on the way out");
    }

    /// Within a hold the pixels cannot change, so the second read of a
    /// box reuses the OCR the first one paid for.
    #[test]
    fn a_second_read_of_the_same_box_in_one_hold_reuses_its_words() {
        let (mut source, runs) = paced(false);
        source.freeze(AT).expect("the freeze");
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("second read");
        assert_eq!(runs.get(), 1, "the same box of one frozen frame is the same words");
    }
}

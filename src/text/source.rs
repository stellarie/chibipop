//! The core-internal facade over `RegionCapture` + `OcrEngine` + the shared
//! layout/hit-scan logic: point in, text span out (ADR-0001). Platform code
//! supplies the two seams; everything below them is shared.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::layout::{
    band_of, head_and_tail, map_from_upscaled, nearest_line, normalise, region_around, resolve,
    tile_forward, CaptureSize, OcrLine, OcrWord, Orientation, Resolved,
};
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
    /// The pixels were the previous grab's, so the OCR was reused.
    pub unchanged: bool,
}

/// One region's words, kept for an unchanged re-grab.
///
/// Word rects are already mapped to physical, so a reuse is a clone
/// and nothing else.
struct Recognised {
    region: PhysRect,
    factor: i32,
    lines: Vec<OcrLine>,
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

    /// Lines plus the outcome.
    fn resolve_at_verbose(
        &mut self,
        cursor: PhysPoint,
    ) -> Result<(Vec<OcrLine>, Option<Resolved>)> {
        let read = self.resolve_in_region(
            cursor,
            region_around(cursor, self.settings.prefer_vertical, self.settings.capture),
        )?;
        Ok((read.lines, read.resolved))
    }

    /// As above, explicit box.
    pub fn resolve_in_region(&mut self, cursor: PhysPoint, region: PhysRect) -> Result<RegionRead> {
        let (lines, frame) = self.recognise_at_capture(region, UPSCALE)?;
        let (lines, frame) = if ADAPTIVE_RETRY && glyphs_look_small(&lines) {
            match self.recognise_at_capture(region, RETRY_UPSCALE) {
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
    /// A backend that says the pixels are unchanged (ADR-0002's damage
    /// race) skips the recogniser entirely: same pixels, same words.
    pub fn recognise_at_capture(
        &mut self,
        region: PhysRect,
        factor: i32,
    ) -> Result<(Vec<OcrLine>, Frame)> {
        let frame = grab_upscaled(self.capture.as_mut(), region, factor)?;
        if frame.unchanged {
            if let Some(lines) = self.reuse(region, factor) {
                return Ok((lines, frame));
            }
        }
        let raw = self.ocr.recognise(&frame.buf, frame.w, frame.h)?;
        let origin = PhysPoint { x: region.x, y: region.y };
        let lines: Vec<OcrLine> = raw
            .into_iter()
            .map(|l| OcrLine {
                words: l
                    .words
                    .into_iter()
                    .map(|word| OcrWord {
                        rect: map_from_upscaled(word.rect, origin, factor),
                        text: word.text,
                    })
                    .collect(),
            })
            .collect();
        self.remember(region, factor, &lines);
        Ok((lines, frame))
    }

    /// This region's words from an earlier pass, if any pass had it.
    ///
    /// A hit is promoted into this read's generation, so a dwell that
    /// never re-OCRs never forgets either.
    fn reuse(&mut self, region: PhysRect, factor: i32) -> Option<Vec<OcrLine>> {
        let same = |r: &&Recognised| r.region == region && r.factor == factor;
        if let Some(hit) = self.recognised.iter().find(same) {
            return Some(hit.lines.clone());
        }
        let hit = self.previous.iter().find(same)?;
        let lines = hit.lines.clone();
        self.recognised.push(Recognised { region, factor, lines: lines.clone() });
        Some(lines)
    }

    /// Keep this pass's words for the next read.
    fn remember(&mut self, region: PhysRect, factor: i32, lines: &[OcrLine]) {
        let entry = Recognised { region, factor, lines: lines.to_vec() };
        match self.recognised.iter_mut().find(|r| r.region == region && r.factor == factor) {
            Some(slot) => *slot = entry,
            None => self.recognised.push(entry),
        }
    }

    /// Tiled, scan rects dropped.
    pub fn resolve_at_tiled(&mut self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        self.resolve_at_tiled_scanned(cursor, false).map(|(r, _)| r)
    }

    /// Tiled read + scan rects. One logical read: brackets the backend's
    /// `begin_read`/`end_read` around every pass.
    pub fn resolve_at_tiled_scanned(
        &mut self,
        cursor: PhysPoint,
        collect: bool,
    ) -> Result<(Option<Resolved>, Vec<ScanRect>)> {
        self.capture.begin_read();
        // One read, one generation: the previous read's passes stay
        // reusable, older ones go.
        self.previous = std::mem::take(&mut self.recognised);
        let out = self.resolve_tiled_inner(cursor, collect);
        self.capture.end_read();
        out
    }

    fn resolve_tiled_inner(
        &mut self,
        cursor: PhysPoint,
        collect: bool,
    ) -> Result<(Option<Resolved>, Vec<ScanRect>)> {
        let (lines, resolved) = self.resolve_at_verbose(cursor)?;
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
                match self.words_in(tile, perpendicular_centre, orientation, line_tolerance) {
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
    ) -> Result<Vec<OcrWord>> {
        let (lines, _) = self.recognise_at_capture(tile, UPSCALE)?;
        Ok(nearest_line(&lines, perpendicular_centre, orientation, tolerance)
            .map(|line| line.words.clone())
            .unwrap_or_default())
    }
}

/// Grab + upscale by `factor`; BGRA.
fn grab_upscaled(
    capture: &mut dyn RegionCapture,
    region: PhysRect,
    factor: i32,
) -> Result<Frame> {
    let frame = capture.grab(region)?;
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

    fn paced(unchanged: bool) -> (TextSource, std::rc::Rc<std::cell::Cell<u32>>) {
        let runs = std::rc::Rc::new(std::cell::Cell::new(0));
        let source = TextSource::new(
            Box::new(Paced { unchanged, grabs: std::cell::Cell::new(0) }),
            Box::new(Counting { runs: runs.clone() }),
            SettingsSnapshot {
                max_passes: 1,
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
        let first = source.resolve_in_region(AT, BOX).expect("first read");
        assert_eq!(runs.get(), 1, "the first read must recognise");
        assert!(!first.lines.is_empty());

        let again = source.resolve_in_region(AT, BOX).expect("second read");
        assert_eq!(runs.get(), 1, "an unchanged region must not be recognised again");
        assert_eq!(again.lines, first.lines, "and must answer the same words");
        assert!(again.unchanged, "the signal must reach the caller");
    }

    #[test]
    fn changed_pixels_are_always_recognised() {
        let (mut source, runs) = paced(false);
        source.resolve_in_region(AT, BOX).expect("first read");
        let again = source.resolve_in_region(AT, BOX).expect("second read");
        assert_eq!(runs.get(), 2);
        assert!(!again.unchanged);
    }

    /// `unchanged` is a hint, never a promise that words are held: a
    /// region never recognised must still be recognised.
    #[test]
    fn an_unchanged_region_with_nothing_held_is_still_recognised() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX).expect("first read");
        source.resolve_in_region(AT, OTHER).expect("a different box");
        assert_eq!(runs.get(), 2, "a box never read cannot be reused");
    }

    /// A dwell through the real read bracket: last read's words must
    /// survive into this one, or a static screen re-OCRs every period
    /// (ADR-0010's dwell re-check would cost what it saves).
    #[test]
    fn a_dwell_through_the_read_bracket_reuses_the_previous_read() {
        let (mut source, runs) = paced(true);
        source.resolve_at_tiled(AT).expect("first read");
        assert_eq!(runs.get(), 1, "the first read must recognise");
        for _ in 0..4 {
            source.resolve_at_tiled(AT).expect("dwell read");
        }
        assert_eq!(runs.get(), 1, "a static dwell must never recognise again");
    }

    /// Several passes in one read must not evict each other: a tiled
    /// read asks for a handful of boxes, and all of them dwell.
    #[test]
    fn two_regions_in_one_read_are_both_kept() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX).expect("pass 1");
        source.resolve_in_region(AT, OTHER).expect("tile");
        assert_eq!(runs.get(), 2);
        source.resolve_in_region(AT, BOX).expect("pass 1 again");
        source.resolve_in_region(AT, OTHER).expect("tile again");
        assert_eq!(runs.get(), 2, "neither region may evict the other");
    }

    /// New settings mean new answers: nothing held may survive them.
    #[test]
    fn new_settings_drop_everything_held() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX).expect("first read");
        assert_eq!(runs.get(), 1);
        let settings = SettingsSnapshot {
            max_passes: 2,
            prefer_vertical: true,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
        };
        source.apply_settings(settings, "ja");
        source.resolve_in_region(AT, BOX).expect("read after reload");
        assert_eq!(runs.get(), 2, "a reload must re-recognise");
    }
}

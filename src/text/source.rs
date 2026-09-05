//! This module connects `RegionCapture`, `OcrEngine`, shared layout, and hit-scan.
//! It accepts a point and returns a text span.
//! (ARCHITECTURE.md#workspace-and-seams).
//! Platform code supplies the two seams.
//! Core shares all code below the seams.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::layout::{
    band_of, discard_furigana, head_and_tail, map_from_upscaled, nearest_line, normalise,
    region_around, resolve, resolve_wrap, tile_forward, trim_probe_edges, wrap_probe, CaptureSize,
    OcrLine, OcrWord, Orientation, Resolved,
};
use crate::text::sentence;
use crate::text::frozen::FrozenFrame;
use crate::text::mask::CaptureMask;
use crate::text::{Frame, OcrEngine, RegionCapture, TextSpan};
use anyhow::{Context, Result};

// The capture upscale factor belongs to each platform.
// It comes from `SettingsSnapshot::upscale`.
// Windows supplies 2 because its engine misreads small text at native resolution.
// Linux supplies 1 because meikiocr measures worse on upscaled crops.
// (ARCHITECTURE.md#ocr-engine).

// MAINTAINER NOTE - adaptive upscale retry, disabled 2026-08-08.
// This note exceeds the 30-char comment rule because it records a method and its removal.
// Stella asked that this note remain in this file.
//
// After the first pass at the configured upscale, the code checks the tallest recognized word.
// If that word is shorter than SMALL_GLYPH_PX, the code captures and reads the same region again at RETRY_UPSCALE.
// The code uses the new result only when it is not empty.
//
// A developer added this method after text vanished at 2x and reappeared at 4x.
// That evidence is invalid. DXGI Desktop Duplication silently returned all-black frames.
// The developer therefore collected the evidence from a capture that returned only black pixels.
// See the Windows capture backend.
// Therefore, "2x found nothing" often meant that capture failed, not that scale was wrong.
// A live frame reached one pass but not the other.
//
// A later measurement found a new cost.
// The method cost about 36 ms of a round trip of about 141 ms.
// That round trip uses two captures and two OCR passes.
// The second pass reads more than 4x the pixels.
// The method did not improve results reliably:
//
//   line 3, 28-31px glyphs, known text すっかり気が抜け、...
//     retry on : すっかーけ。ただ水と化
//     single 2x: すっかり一け。ただ水と化   <- more accurate
//
// The method also runs on ordinary body text.
// Glyphs of 28-31px remain clear, but stay below the 32px threshold.
// The method therefore added this cost to many reads.
//
// To re-enable this method, take these steps:
//
// 1. Set ADAPTIVE_RETRY.
// 2. Measure with these commands:
//      probe --at X,Y --repeat N            (warm timings, one process)
//      probe --at X,Y --upscale 2|4         (single pass, no retry)
// 3. Test against text with a known true string.
// 4. Compare transcription accuracy, not only whether more characters appeared.
// 5. Keep the method only when its accuracy gain justifies its measured cost.
//
// A lower SMALL_GLYPH_PX value of about 22px costs less.
// This change limits the retry to tiny text.

/// Retry small text at a larger scale.
const ADAPTIVE_RETRY: bool = false;

/// Below this height, retry at RETRY_UPSCALE.
const SMALL_GLYPH_PX: i32 = 32;

/// This scale applies to a small-glyph retry.
const RETRY_UPSCALE: i32 = 4;

/// Return true when the tallest word looks small.
///
/// An empty result is not small because no retry can help.
fn glyphs_look_small(lines: &[OcrLine]) -> bool {
    lines
        .iter()
        .flat_map(|l| l.words.iter())
        .map(|w| w.rect.h)
        .max()
        .is_some_and(|max_h| max_h < SMALL_GLYPH_PX)
}

/// `RegionRead` stores one region read and its source details.
pub struct RegionRead {
    pub lines: Vec<OcrLine>,
    pub resolved: Option<Resolved>,
    /// This source names the backend path that produced the pixels.
    pub source: &'static str,
    /// This field explains why the code did not use the preferred path.
    pub fallback: Option<String>,
    /// This flag reports that the backend returned the previous grab's pixels.
    pub unchanged: bool,
}

/// `Recognised` stores the words from one region read for unchanged re-grabs.
///
/// Word rects already use physical pixels, so reuse only clones these lines.
///
/// The reuse key is `(region, factor, mask)`.
/// `unchanged` describes the *raw* pixels that the backend copied.
/// The code applies the mask before OCR sees those pixels, so the mask belongs in the key.
/// A popup over a still region leaves the raw grab unchanged but changes the question.
/// This key keeps unmasked words out of a masked read.
/// It also keeps masked words out of an unmasked read.
/// The code stores the mask after it clips it to `region`.
/// ([`CaptureMask::clipped_to`]).
/// A popup that cannot reach this box then costs no OCR pass.
struct Recognised {
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
    lines: Vec<OcrLine>,
}

/// `SettingsSnapshot` stores reloadable OCR settings.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SettingsSnapshot {
    pub max_passes: u8,
    /// This nearest-neighbor factor applies to every grab before OCR sees it.
    /// It is a platform fact, not a user setting.
    /// Windows supplies 2 because WinRT OCR misreads small text at 1x.
    /// Linux supplies 1 because meikiocr measures worse on upscaled crops.
    /// (ARCHITECTURE.md#ocr-engine).
    pub upscale: i32,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
    pub discard_furigana: bool,
}

/// `Frozen` records the result of trigger mode's press-time grab.
enum Frozen {
    /// This frame serves every lookup in this hold.
    /// (ARCHITECTURE.md#hover-cadence).
    Held(FrozenFrame),
    /// The press-time grab failed.
    /// Trigger mode without a frozen frame cannot read.
    /// Every lookup in the hold reports this failure.
    /// The code never reads a live grab because it can contain the popup.
    /// (ARCHITECTURE.md#capture-and-masking).
    Failed(String),
}

/// `TextSource` accepts a point and returns a text span.
pub struct TextSource {
    capture: Box<dyn RegionCapture>,
    ocr: Box<dyn OcrEngine>,
    settings: SettingsSnapshot,
    /// This is the press-time grab for the active trigger hold.
    /// Every lookup reads from it, so the backend receives no more calls.
    /// (ARCHITECTURE.md#hover-cadence).
    frozen: Option<Frozen>,
    /// This vector stores the lines that this read recognized.
    recognised: Vec<Recognised>,
    /// This vector stores the previous read for a dwell re-check.
    /// A damage-paced backend returns an unchanged region with the same pixels.
    /// OCR then returns the same lines.
    /// (ARCHITECTURE.md#hover-cadence).
    /// The code keeps two generations, so one read's tiles cannot evict each other.
    /// The passes that one read makes bound each generation.
    previous: Vec<Recognised>,
}

impl TextSource {
    /// Send pixels that the caller already holds to the OCR engine.
    /// The bin calls this method between lookups with the Worker's `serve` hook.
    /// OCR backends are thread-affine, so a one-off OCR-to-clipboard job must run on this thread.
    /// The job cannot create a second engine elsewhere.
    /// This method gives one-off OCR calls the same seam as lookups.
    pub fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        let lines = self.ocr.recognise(bgra, w, h)?;
        Ok(if self.settings.discard_furigana {
            discard_furigana(lines)
        } else {
            lines
        })
    }

    /// Return the name of the engine that reads this source's pixels.
    /// A diagnostic uses this name in the `probe` report.
    pub fn engine_name(&self) -> &str {
        self.ocr.name()
    }
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

    /// Replace the OCR settings.
    pub fn apply_settings(&mut self, settings: SettingsSnapshot, language: &str) {
        self.ocr.set_language(language);
        self.settings = settings;
        // A new language or capture size produces a new answer.
        self.recognised.clear();
        self.previous.clear();
    }

    /// Freeze the output that contains `at`.
    /// One full grab now serves every lookup until [`TextSource::thaw`].
    /// (ARCHITECTURE.md#hover-cadence).
    ///
    /// The method returns the box that it froze.
    /// It also remembers a failed grab.
    /// The hold then reports that failure instead of a live pixel read.
    /// Trigger mode does not mask those pixels.
    pub fn freeze(&mut self, at: PhysPoint) -> Result<PhysRect> {
        // Frozen pixels answer a different capture question, so no earlier pass can cross this edge.
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

    /// Drop the hold's frozen frame and return to live grabs.
    pub fn thaw(&mut self) {
        if self.frozen.take().is_some() {
            self.recognised.clear();
            self.previous.clear();
        }
    }

    /// Return the box covered by the hold's frozen grab, if one exists.
    pub fn frozen_region(&self) -> Option<PhysRect> {
        match &self.frozen {
            Some(Frozen::Held(f)) => Some(f.region()),
            _ => None,
        }
    }

    /// Return the lines and outcome for one read.
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

    /// Resolve a caller-supplied box.
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

    /// Read the sentence around `anchor` from bounded OCR tiles.
    ///
    /// This read belongs here because `TextSource` owns capture, the OCR cache,
    /// the frozen frame, and the capture mask. Callers must not duplicate these
    /// rules. The first failed grab is an error because a missing tile can hold
    /// the sentence start. A partial sentence on an Anki card is worse than the
    /// hovered line.
    ///
    /// Frozen mode needs no separate path. [`Self::recognise_at_capture`] already
    /// reads the held frame.
    pub fn read_sentence(
        &mut self,
        anchor: PhysRect,
        orientation: Orientation,
        mask: CaptureMask,
    ) -> Result<Option<String>> {
        let bounds = self.capture.bounds_containing(anchor.center());
        let regions = sentence::probe_regions(anchor, orientation, bounds);
        if regions.is_empty() {
            return Ok(None);
        }

        let live = self.frozen.is_none();
        let read = |source: &mut Self| -> Result<Option<String>> {
            // Keep one cache generation for this logical read, like the tiled hover path.
            source.previous = std::mem::take(&mut source.recognised);
            let mut all_lines = Vec::new();
            for tile in regions {
                let (mut lines, _) =
                    source.recognise_at_capture(tile, source.settings.upscale, mask)?;
                trim_probe_edges(&mut lines, tile, bounds, orientation);
                all_lines.extend(lines);
            }
            Ok(sentence::sentence_at(&all_lines, anchor, orientation))
        };

        if !live {
            return read(self);
        }

        self.capture.begin_read();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(self)));
        self.capture.end_read();
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Capture and recognize at `factor`, then map the words to physical pixels.
    ///
    /// The code fills `mask` with white in the grabbed pixels before OCR sees them.
    /// It drops words that overlap the mask on the way back out.
    /// (ARCHITECTURE.md#capture-and-masking).
    /// A frozen hold answers without a mask because those pixels predate the popup.
    /// (ARCHITECTURE.md#hover-cadence).
    ///
    /// A backend can report unchanged pixels after a damage race.
    /// The code then skips OCR because the same pixels produce the same words.
    /// The mask belongs to that same question because OCR sees pixels after the code applies the mask.
    /// See [`CaptureMask::clipped_to`].
    pub fn recognise_at_capture(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
    ) -> Result<(Vec<OcrLine>, Frame)> {
        // One value controls the fill, word drop, and reuse key.
        // These three actions therefore use the same mask.
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
        let lines = if self.settings.discard_furigana {
            discard_furigana(lines)
        } else {
            lines
        };
        self.remember(region, factor, mask, &lines);
        Ok((lines, frame))
    }

    /// Return this pass's pixels from the frozen frame or a live grab.
    fn grab(&mut self, region: PhysRect, factor: i32, mask: CaptureMask) -> Result<Frame> {
        match &mut self.frozen {
            Some(Frozen::Held(f)) => finish_grab(f.crop(region), region, factor, mask),
            // The hold has no pixels, so it cannot serve lookups.
            Some(Frozen::Failed(why)) => Err(anyhow::anyhow!(why.clone())),
            None => grab_upscaled(self.capture.as_mut(), region, factor, mask),
        }
    }

    /// Return this region's words from an earlier pass when the question matches.
    /// The question has the same box, scale, and mask.
    ///
    /// The code promotes a hit into this read's generation.
    /// A dwell that skips OCR therefore keeps every earlier result.
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

    /// Save this pass's words for the next read.
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

    /// Resolve with tiles and discard scan rects.
    pub fn resolve_at_tiled(
        &mut self,
        cursor: PhysPoint,
        mask: CaptureMask,
    ) -> Result<Option<Resolved>> {
        self.resolve_at_tiled_scanned(cursor, false, mask).map(|(r, _, _)| r)
    }

    /// Resolve with tiles and return scan rects.
    /// This method performs one logical read.
    /// It calls the backend's `begin_read` before the passes and `end_read` after them.
    ///
    /// A frozen hold uses no backend calls.
    /// Each pass crops the press-time frame.
    /// (ARCHITECTURE.md#hover-cadence).
    /// A damage race on unread pixels causes wakeups, but the hold ignores that result.
    ///
    /// `mask` marks the pixels that OCR must not read.
    /// On a live grab, it marks our own popup.
    /// (ARCHITECTURE.md#capture-and-masking).
    /// It applies to every pass in this read.
    /// The third result contains pass 1's OCR lines for `SentenceMode::All` capture.
    pub fn resolve_at_tiled_scanned(
        &mut self,
        cursor: PhysPoint,
        collect: bool,
        mask: CaptureMask,
    ) -> Result<(Option<Resolved>, Vec<ScanRect>, Vec<OcrLine>)> {
        let live = self.frozen.is_none();
        if live {
            self.capture.begin_read();
        }
        // One read uses one generation. The previous read stays reusable, and older reads go.
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
    ) -> Result<(Option<Resolved>, Vec<ScanRect>, Vec<OcrLine>)> {
        let (lines, resolved) = self.resolve_at_verbose(cursor, mask)?;
        let mut scan = Vec::new();
        let Some(pass_one) = resolved else { return Ok((None, scan, lines)) };
        let region = region_around(cursor, self.settings.prefer_vertical, self.settings.capture);
        if collect {
            scan.push(ScanRect { rect: region, kind: ScanKind::Pass1 });
        }
        let wrapped = if let Some((probes, wrapped)) =
            self.probe_wrap(&lines, cursor, region, pass_one.orientation, mask)
        {
            if collect {
                for probe in probes {
                    scan.push(ScanRect { rect: probe, kind: ScanKind::Tile });
                }
            }
            wrapped
        } else {
            None
        };
        let wrapped_anchor =
            wrapped.as_ref().map_or(pass_one.span.anchor, |r| r.span.anchor);
        if self.settings.max_passes <= 1 {
            if collect {
                scan.push(ScanRect { rect: wrapped_anchor, kind: ScanKind::Anchor });
            }
            return Ok((Some(wrapped.unwrap_or(pass_one)), scan, lines));
        }
        // Reuse pass 1's kept tail. Do not read it again.
        let alnum = self.settings.scan_alphanumeric;
        let Some((head, tail_start, orientation)) = head_and_tail(&lines, cursor, region, alnum)
        else {
            if collect {
                scan.push(ScanRect { rect: wrapped_anchor, kind: ScanKind::Anchor });
            }
            return Ok((Some(wrapped.unwrap_or(pass_one)), scan, lines));
        };
        let head_chars = head.chars().count();

        let anchor = pass_one.span.anchor;
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
            let anchor = if tail.is_empty() { wrapped_anchor } else { pass_one.span.anchor };
            scan.push(ScanRect { rect: anchor, kind: ScanKind::Anchor });
        }

        // Keep pass 1's answer when tiles add no text. Preserve its geometry and any joined continuation.
        // When tiles add text, use the stitched head and tail and drop a joined continuation.
        // A word past the box on the same row shows that the line continues.
        if tail.is_empty() {
            return Ok((Some(wrapped.unwrap_or(pass_one)), scan, lines));
        }
        let text = normalise(&format!("{head}{tail}"));
        Ok((
            Some(Resolved {
                // The stitched text has no geometry.
                span: TextSpan {
                    text,
                    cursor_byte_offset: 0,
                    anchor,
                    geom: Vec::new(),
                },
                orientation,
            }),
            scan,
            lines,
        ))
    }

    /// Read bounded wrap probes when pass 1's box can hide a wrap.
    /// See [`wrap_probe`].
    /// `orientation` comes from pass 1 because a clipped probe can contain one word.
    ///
    /// The result lists every probe that this operation attempted.
    /// A failure stops the probe sequence at that probe. Later probes cannot join without it.
    /// Pass 1's answer then stands.
    fn probe_wrap(
        &mut self,
        lines: &[OcrLine],
        cursor: PhysPoint,
        region: PhysRect,
        orientation: Orientation,
        mask: CaptureMask,
    ) -> Option<(Vec<PhysRect>, Option<Resolved>)> {
        let alnum = self.settings.scan_alphanumeric;
        let bounds = self.capture.bounds_containing(cursor);
        let probes = wrap_probe(lines, cursor, region, alnum, bounds)?;
        let mut probe_lines: Vec<(PhysRect, Vec<OcrLine>)> = Vec::new();
        for (i, &probe) in probes.iter().enumerate() {
            match self.recognise_at_capture(probe, self.settings.upscale, mask) {
                Ok((lines, _)) => probe_lines.push((probe, lines)),
                Err(e) => {
                    eprintln!("chibipop: wrap probe failed, using pass 1: {e:#}");
                    return Some((probes[..=i].to_vec(), None));
                }
            }
        }
        let wrapped = resolve_wrap(&probe_lines, cursor, alnum, orientation, bounds);
        Some((probes, wrapped))
    }

    /// Read one capture and return the hovered line.
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

/// `to_desktop` maps image-pixel word boxes to desktop physical pixels and drops words that touch the mask.
///
/// The mask boundary is a capture edge.
/// (ARCHITECTURE.md#capture-and-masking).
/// The pixels under the mask are flat white, so a word that overlaps the mask loses part of a glyph.
/// The engine drops that word, as it drops a word that a tile edge clips.
/// It never recognizes only part of a word.
/// The code also drops a line with no words, so [`OcrEngine`]'s rule still holds:
/// no words means that the engine recognized nothing.
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

/// Grab, mask, and upscale by `factor`. Return BGRA pixels.
fn grab_upscaled(
    capture: &mut dyn RegionCapture,
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
) -> Result<Frame> {
    let frame = capture.grab(region)?;
    finish_grab(frame, region, factor, mask)
}

/// Check the frame shape, apply the mask, and upscale one live or frozen grab.
///
/// The code applies the mask before the upscale.
/// At 2x, this writes one quarter of the pixels, and nearest-neighbor expansion preserves the hard edge.
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

/// Upscale pixels with nearest-neighbor copies by `factor`.
///
/// Platform bins use this public function for one-off OCR-to-clipboard actions.
/// Those actions capture at 2x outside the Worker thread.
/// Lookups also upscale here.
pub fn upscale_by(src: &[u8], w: i32, h: i32, factor: i32) -> (Vec<u8>, i32, i32) {
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

    /// One tall word keeps the region above the small-text threshold.
    #[test]
    fn a_single_tall_word_among_small_ones_is_not_small() {
        let lines = [
            line(vec![word("し", 27), word("な", 29)]),
            line(vec![word("大", 40)]),
        ];
        assert!(!glyphs_look_small(&lines));
    }

    // -- the capture mask at the seam --

    /// Use one byte pattern per grab so the mask changes every byte.
    const DESK: u8 = 0x20;

    /// `SolidCapture` returns region-sized frames filled with [`DESK`].
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


    /// This capture lets the test control `unchanged`.
    /// It exercises reuse without a compositor.
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

    /// `SeenImage` stores the last image sent to OCR: pixels, width, and height.
    type SeenImage = Rc<RefCell<Option<(Vec<u8>, i32, i32)>>>;

    /// `RecordingOcr` keeps the pixels it receives and reports fixed boxes.
    struct RecordingOcr {
        seen: SeenImage,
        /// `RecordingOcr` reports one image-pixel word box per line.
        words: Vec<PhysRect>,
    }

    struct RubyOcr;

    impl OcrEngine for RubyOcr {
        fn recognise(&self, _bgra: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            Ok(vec![
                OcrLine {
                    words: vec![OcrWord {
                        text: "かんじ".to_string(),
                        rect: PhysRect { x: 5, y: 0, w: 45, h: 12 },
                    }],
                },
                OcrLine {
                    words: vec![OcrWord {
                        text: "漢字".to_string(),
                        rect: PhysRect { x: 0, y: 15, w: 60, h: 30 },
                    }],
                },
            ])
        }

        fn set_language(&mut self, _tag: &str) {}

        fn name(&self) -> &str {
            "ruby"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    #[test]
    fn recognise_keeps_ruby_when_discard_is_off() {
        let mut settings = snap();
        settings.discard_furigana = false;
        let source = TextSource::new(Box::new(SolidCapture), Box::new(RubyOcr), settings);
        assert_eq!(2, source.recognise(&[], 0, 0).unwrap().len());
    }

    #[test]
    fn recognise_discards_ruby_when_enabled() {
        let source = TextSource::new(Box::new(SolidCapture), Box::new(RubyOcr), snap());
        let lines = source.recognise(&[], 0, 0).unwrap();
        assert_eq!(1, lines.len());
        assert_eq!("漢字", lines[0].words[0].text);
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

        fn name(&self) -> &str {
            "recording"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    /// Count the number of OCR runs.
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

        fn name(&self) -> &str {
            "counting"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    fn snap() -> SettingsSnapshot {
        SettingsSnapshot {
            max_passes: 1,
            upscale: 2,
            prefer_vertical: false,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
            discard_furigana: true,
        }
    }

    /// Build a source over the fakes and return the handle that records OCR input.
    fn recording(words: Vec<PhysRect>) -> (TextSource, SeenImage) {
        let seen = Rc::new(RefCell::new(None));
        let ocr = RecordingOcr { seen: Rc::clone(&seen), words };
        (TextSource::new(Box::new(SolidCapture), Box::new(ocr), snap()), seen)
    }

    fn live(popup: PhysRect) -> CaptureMask {
        CaptureMask::for_mode(CaptureMode::Live, Some(popup))
    }

    /// Return one pixel that OCR received, in BGRA format.
    fn px(buf: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// Fill exactly the popup with white and keep a hard edge.
    #[test]
    fn the_popup_reaches_ocr_as_flat_white() {
        let region = PhysRect { x: 100, y: 200, w: 8, h: 6 };
        // The popup overlaps region columns 2..5 and rows 1..3.
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

    /// The mask belongs in frame-local coordinates, not desktop coordinates.
    /// The code must still place the mask correctly when the region origin is far from zero.
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

    /// A maskless read sends the grabbed pixels to OCR unchanged.
    #[test]
    fn a_maskless_read_hands_ocr_the_untouched_grab() {
        let region = PhysRect { x: 0, y: 0, w: 4, h: 4 };
        let popup = PhysRect { x: 1, y: 1, w: 2, h: 2 };
        let (mut source, seen) = recording(Vec::new());

        // The frozen grab predates the popup, so the mask covers no pixels.
        let frozen = CaptureMask::for_mode(CaptureMode::Frozen, Some(popup));
        source.recognise_at_capture(region, 1, frozen).unwrap();

        let (buf, _, _) = seen.borrow_mut().take().unwrap();
        let untouched: Vec<u8> = (0..4 * 4).flat_map(|_| [DESK, DESK, DESK, 0xFF]).collect();
        assert_eq!(untouched, buf, "a frozen grab is handed over as it came");
    }

    /// The mask boundary is a capture edge.
    /// The engine drops words that touch it.
    #[test]
    fn words_touching_the_mask_are_dropped_and_the_rest_survive() {
        let region = PhysRect { x: 0, y: 0, w: 40, h: 20 };
        let popup = PhysRect { x: 10, y: 0, w: 10, h: 20 };
        let words = vec![
            PhysRect { x: 0, y: 0, w: 8, h: 10 },  // This word stays clear of the mask.
            PhysRect { x: 12, y: 0, w: 4, h: 10 }, // This word lies inside the mask.
            PhysRect { x: 8, y: 0, w: 6, h: 10 },  // This word crosses the mask edge.
            PhysRect { x: 20, y: 0, w: 8, h: 10 }, // This word touches the mask edge without overlap.
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

    /// A maskless read keeps the same words.
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

    /// The mask runs before the upscale, so the hard edge lands on the 2x grid.
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

    /// The factor comes from the snapshot, not a Core constant.
    /// An upscale-1 platform, such as Linux, must send native-resolution pixels to its engine.
    /// (ARCHITECTURE.md#ocr-engine).
    /// An upscale-2 platform, such as Windows, must send doubled pixels.
    /// The resolve path must use this factor, not only an explicit `recognise_at_capture` argument.
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

    // -- reuse of unchanged grab words: damage race and dwell --

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
                discard_furigana: true,
            },
        );
        (source, runs)
    }

    const BOX: PhysRect = PhysRect { x: 100, y: 100, w: 80, h: 40 };
    const OTHER: PhysRect = PhysRect { x: 400, y: 100, w: 80, h: 40 };
    const AT: PhysPoint = PhysPoint { x: 120, y: 110 };

    /// An unchanged grab must not run OCR twice.
    /// (ARCHITECTURE.md#capture-and-masking).
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

    /// The `unchanged` flag does not promise stored words.
    /// A region with no result from an earlier read still needs OCR.
    #[test]
    fn an_unchanged_region_with_nothing_held_is_still_recognised() {
        let (mut source, runs) = paced(true);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        source.resolve_in_region(AT, OTHER, CaptureMask::NONE).expect("a different box");
        assert_eq!(runs.get(), 2, "a box never read cannot be reused");
    }

    /// The read bracket must carry words into the next read.
    /// Otherwise, a static screen can run OCR at every dwell interval.
    /// The dwell re-check can then cost what it saves.
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

    /// One read can use several passes.
    /// It must keep each pass so a later dwell can reuse every region.
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

    // -- the wrap probe --

    /// The hover sits on the last character of the hovered line.
    /// The line's margin lies far left of pass 1's 500x100 box. The wrap `かった` starts at that margin.
    const WRAP_AT: PhysPoint = PhysPoint { x: 1000, y: 500 };
    /// `region_around(WRAP_AT)` returns the default capture size.
    const WRAP_REGION: PhysRect = PhysRect { x: 750, y: 450, w: 500, h: 100 };
    /// These are the two bounded probes in the short fixture.
    const WRAP_MARGIN: PhysRect = PhysRect { x: 0, y: 470, w: 1000, h: 270 };
    const WRAP_TAIL: PhysRect = PhysRect { x: 500, y: 470, w: 540, h: 270 };
    /// The wide output fixture moves the line-end probe farther right.
    const WIDE_TAIL: PhysRect = PhysRect { x: 1000, y: 470, w: 940, h: 270 };
    /// `region_around` returns the capture size at the wide output cursor.
    const WIDE_REGION: PhysRect = PhysRect { x: 1650, y: 450, w: 500, h: 100 };
    /// The wide wrap fixture reports a larger box for the hovered glyph.
    const WIDE_WRAP_HIT: PhysRect = PhysRect { x: 970, y: 480, w: 60, h: 40 };
    /// The precedence fixture returns one same-row word from the first forward tile.
    const FORWARD_TILE: PhysRect = PhysRect { x: 1250, y: 440, w: 500, h: 120 };


    /// Select the bounded probe that fails in the fixture.
    enum ProbeFailure {
        Margin,
        Tail,
    }

    /// The `Scripted` fixture answers each grab by its shape in that grab's image pixels.
    /// The whole screen holds `にも疎` with its end at x=1020.
    /// It holds `かった` at x=100 on the line below.
    /// Pass 1's box sees only the line end. The two probes see both fragments.
    /// The source combines their mapped lines.
    struct Scripted {
        runs: Rc<std::cell::Cell<u32>>,
        region_x: Rc<std::cell::Cell<i32>>,
        /// Show a clear continuation inside pass 1's box.
        wrap_in_box: bool,
        /// Place the line end near x=1900 for the wide output test.
        wide: bool,
        /// Place one OCR word across the probe seam.
        glued_seam: bool,
        /// Fail one bounded probe after pass 1.
        fail: Option<ProbeFailure>,
        /// Give the hovered glyph a wider box in a wrap probe.
        wrap_wide_hit: bool,
        /// Return same-row text from the first forward tile.
        forward_tail: bool,
    }
    /// `ScriptedCapture` records each requested region for role-based OCR replies.
    struct ScriptedCapture {
        region_x: Rc<std::cell::Cell<i32>>,
        wide: bool,
    }

    impl RegionCapture for ScriptedCapture {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            self.region_x.set(region.x);
            Ok(Frame {
                buf: vec![0u8; (region.w * region.h * 4) as usize],
                w: region.w,
                h: region.h,
                source: "scripted",
                fallback: None,
                unchanged: true,
            })
        }

        fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
            if self.wide {
                PhysRect { x: 0, y: 0, w: 2560, h: 1500 }
            } else {
                PhysRect { x: p.x - 1000, y: p.y - 1000, w: 2000, h: 2000 }
            }
        }
    }

    impl OcrEngine for Scripted {
        fn recognise(&self, _bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
            self.runs.set(self.runs.get() + 1);
            let chars = |text: &str, x0: i32, y: i32| OcrLine {
                words: text
                    .chars()
                    .enumerate()
                    .map(|(i, c)| OcrWord {
                        text: c.to_string(),
                        rect: PhysRect { x: x0 + 40 * i as i32, y, w: 40, h: 40 },
                    })
                    .collect(),
            };
            let capture_x = self.region_x.get();
            if (w, h) == (WRAP_MARGIN.w, WRAP_MARGIN.h)
                && capture_x == WRAP_MARGIN.x
                && matches!(self.fail, Some(ProbeFailure::Margin))
            {
                return Err(anyhow::anyhow!("scripted margin probe failure"));
            }
            if (w, h) == (WRAP_TAIL.w, WRAP_TAIL.h)
                && capture_x == WRAP_TAIL.x
                && matches!(self.fail, Some(ProbeFailure::Tail))
            {
                return Err(anyhow::anyhow!("scripted tail probe failure"));
            }

            if (w, h) == (WRAP_REGION.w, WRAP_REGION.h) {
                let (x, origin) =
                    if self.wide { (1800, WIDE_REGION) } else { (900, WRAP_REGION) };
                let mut lines = vec![chars("にも疎", x - origin.x, 480 - origin.y)];
                if self.wrap_in_box {
                    lines.push(chars("かった", 850 - origin.x, 510 - origin.y));
                }
                return Ok(lines);
            }
            if self.wide {
                if (w, h) == (WRAP_MARGIN.w, WRAP_MARGIN.h) && capture_x == WRAP_MARGIN.x {
                    return Ok(vec![chars("かった", 100 - WRAP_MARGIN.x, 540 - WRAP_MARGIN.y)]);
                }
                if (w, h) == (WIDE_TAIL.w, WIDE_TAIL.h) && capture_x == WIDE_TAIL.x {
                    return Ok(vec![chars("にも疎", 1800 - WIDE_TAIL.x, 480 - WIDE_TAIL.y)]);
                }
                return Ok(Vec::new());
            }
            if (w, h) == (WRAP_MARGIN.w, WRAP_MARGIN.h) && capture_x == WRAP_MARGIN.x {
                if self.glued_seam {
                    let mut line = chars("に", 900 - WRAP_MARGIN.x, 480 - WRAP_MARGIN.y);
                    line.words.push(OcrWord {
                        text: "も疎".into(),
                        rect: PhysRect {
                            x: 940 - WRAP_MARGIN.x,
                            y: 480 - WRAP_MARGIN.y,
                            w: 60,
                            h: 40,
                        },
                    });
                    return Ok(vec![
                        line,
                        chars("かった", 100 - WRAP_MARGIN.x, 540 - WRAP_MARGIN.y),
                    ]);
                }
                return Ok(vec![
                    chars("にも", 900 - WRAP_MARGIN.x, 480 - WRAP_MARGIN.y),
                    chars("かった", 100 - WRAP_MARGIN.x, 540 - WRAP_MARGIN.y),
                ]);
            }
            if (w, h) == (WRAP_TAIL.w, WRAP_TAIL.h) && capture_x == WRAP_TAIL.x {
                let mut line = chars("にも疎", 900 - WRAP_TAIL.x, 480 - WRAP_TAIL.y);
                if self.wrap_wide_hit {
                    line.words[2].rect = PhysRect {
                        x: WIDE_WRAP_HIT.x - WRAP_TAIL.x,
                        y: WIDE_WRAP_HIT.y - WRAP_TAIL.y,
                        w: WIDE_WRAP_HIT.w,
                        h: WIDE_WRAP_HIT.h,
                    };
                }
                return Ok(vec![line]);
            }
            if self.forward_tail
                && (w, h) == (FORWARD_TILE.w, FORWARD_TILE.h)
                && capture_x == FORWARD_TILE.x
            {
                return Ok(vec![chars(
                    "先",
                    0,
                    480 - FORWARD_TILE.y,
                )]);
            }
            Ok(Vec::new())
        }


        fn set_language(&mut self, _tag: &str) {}

        fn name(&self) -> &str {
            "scripted"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    struct OutputCapture;

    impl RegionCapture for OutputCapture {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            Ok(Frame {
                buf: vec![0u8; (region.w * region.h * 4) as usize],

                w: region.w,
                h: region.h,
                source: "output",
                fallback: None,
                unchanged: true,
            })
        }

        fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
            PhysRect { x: 0, y: 0, w: 2560, h: 1500 }
        }
    }

    /// `FailingOutputCapture` refuses one tile after it returns earlier tiles.
    struct FailingOutputCapture {
        grabs: std::cell::Cell<u32>,
        fail_on: u32,
    }

    impl RegionCapture for FailingOutputCapture {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            let grab = self.grabs.get() + 1;
            self.grabs.set(grab);
            if grab == self.fail_on {
                return Err(anyhow::anyhow!("scripted sentence grab failure"));
            }
            Ok(Frame {
                buf: vec![0u8; (region.w * region.h * 4) as usize],

                w: region.w,
                h: region.h,
                source: "output",
                fallback: None,
                unchanged: true,
            })
        }

        fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
            PhysRect { x: 0, y: 0, w: 2560, h: 1440 }
        }
    }

    /// `SentenceScripted` returns rows in local pixels of the first tile.
    /// The source maps those boxes back to the desktop before it cuts the sentence.
    struct SentenceScripted {
        calls: Rc<std::cell::Cell<u32>>,
        empty: bool,
    }

    impl OcrEngine for SentenceScripted {
        fn recognise(&self, _bgra: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            let call = self.calls.get() + 1;
            self.calls.set(call);
            if self.empty || call != 1 {
                return Ok(Vec::new());
            }
            let row = |text: &str, y: i32| OcrLine {
                words: text
                    .chars()
                    .enumerate()
                    .map(|(i, character)| OcrWord {
                        text: character.to_string(),
                        rect: PhysRect { x: 100 + 40 * i as i32, y, w: 40, h: 40 },
                    })
                    .collect(),
            };
            Ok(vec![
                row("前の文。今日は", 180),
                row("いい天気で", 240),
                row("すね。次", 300),
            ])
        }


        fn set_language(&mut self, _tag: &str) {}

        fn name(&self) -> &str {
            "sentence-scripted"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    fn sentence_scripted(
        fail_on: Option<u32>,
        empty: bool,
    ) -> (TextSource, Rc<std::cell::Cell<u32>>) {
        let calls = Rc::new(std::cell::Cell::new(0));
        let capture: Box<dyn RegionCapture> = match fail_on {
            Some(fail_on) => Box::new(FailingOutputCapture {
                grabs: std::cell::Cell::new(0),
                fail_on,
            }),
            None => Box::new(OutputCapture),
        };
        let source = TextSource::new(
            capture,
            Box::new(SentenceScripted { calls: calls.clone(), empty }),
            SettingsSnapshot { upscale: 1, ..snap() },
        );
        (source, calls)
    }

    const SENTENCE_ANCHOR: PhysRect = PhysRect { x: 180, y: 500, w: 40, h: 40 };

    #[test]
    fn sentence_probe_reads_five_tiles_and_cuts_at_sentence_ends() {

        let (mut source, calls) = sentence_scripted(None, false);

        let sentence = source
            .read_sentence(SENTENCE_ANCHOR, Orientation::Horizontal, CaptureMask::NONE)
            .expect("sentence probe");

        assert_eq!(Some("今日はいい天気ですね。".to_string()), sentence);
        assert_eq!(5, calls.get(), "the 2560px output needs five bounded tiles");

    }

    #[test]
    fn a_sentence_probe_grab_failure_is_returned() {
        let (mut source, calls) = sentence_scripted(Some(2), false);

        let error = source
            .read_sentence(SENTENCE_ANCHOR, Orientation::Horizontal, CaptureMask::NONE)
            .expect_err("the second tile must fail the read");

        assert!(format!("{error:#}").contains("scripted sentence grab failure"));
        assert_eq!(1, calls.get(), "the second grab fails before OCR");
    }

    #[test]
    fn a_sentence_probe_without_an_anchor_word_returns_none() {
        let (mut source, calls) = sentence_scripted(None, true);

        let sentence = source
            .read_sentence(SENTENCE_ANCHOR, Orientation::Horizontal, CaptureMask::NONE)
            .expect("sentence probe");

        assert_eq!(None, sentence);
        assert_eq!(5, calls.get(), "all five tiles are read before the anchor scan");

    }

    fn scripted(wrap_in_box: bool, max_passes: u8) -> (TextSource, Rc<std::cell::Cell<u32>>) {
        scripted_fixture(wrap_in_box, max_passes, false, false, None)
    }

    fn glued_seam_scripted() -> (TextSource, Rc<std::cell::Cell<u32>>) {
        scripted_fixture(false, 1, false, true, None)
    }

    fn wide_scripted(max_passes: u8) -> (TextSource, Rc<std::cell::Cell<u32>>) {
        scripted_fixture(false, max_passes, true, false, None)
    }

    fn failing_scripted(failure: ProbeFailure) -> (TextSource, Rc<std::cell::Cell<u32>>) {
        scripted_fixture(false, 1, false, false, Some(failure))
    }

    fn scripted_fixture(
        wrap_in_box: bool,
        max_passes: u8,
        wide: bool,
        glued_seam: bool,
        fail: Option<ProbeFailure>,
    ) -> (TextSource, Rc<std::cell::Cell<u32>>) {
        let runs = Rc::new(std::cell::Cell::new(0));
        let region_x = Rc::new(std::cell::Cell::new(0));
        let capture: Box<dyn RegionCapture> = Box::new(ScriptedCapture {
            region_x: Rc::clone(&region_x),
            wide,
        });
        let source = TextSource::new(
            capture,
            Box::new(Scripted {
                runs: runs.clone(),
                region_x,
                wrap_in_box,
                wide,
                glued_seam,
                fail,
                wrap_wide_hit: false,
                forward_tail: false,
            }),
            SettingsSnapshot {
                max_passes,
                upscale: 1,
                prefer_vertical: false,
                capture: CaptureSize::default(),
                scan_alphanumeric: true,
                discard_furigana: true,
            },
        );
        (source, runs)
    }
    fn wide_wrap_with_forward_tail_scripted() -> (TextSource, Rc<std::cell::Cell<u32>>) {
        let runs = Rc::new(std::cell::Cell::new(0));
        let region_x = Rc::new(std::cell::Cell::new(0));
        let source = TextSource::new(
            Box::new(ScriptedCapture { region_x: Rc::clone(&region_x), wide: false }),
            Box::new(Scripted {
                runs: runs.clone(),
                region_x,
                wrap_in_box: false,
                wide: false,
                glued_seam: false,
                fail: None,
                wrap_wide_hit: true,
                forward_tail: true,
            }),
            SettingsSnapshot {
                max_passes: 2,
                upscale: 1,
                prefer_vertical: false,
                capture: CaptureSize::default(),
                scan_alphanumeric: true,
                discard_furigana: true,
            },
        );
        (source, runs)
    }

    fn assert_probe_failure_keeps_pass_one(
        failure: ProbeFailure,
        expected_runs: u32,
        expected_scan: &[(ScanKind, PhysRect)],
    ) {
        let (mut source, runs) = failing_scripted(failure);
        let (resolved, scan, _) = source
            .resolve_at_tiled_scanned(WRAP_AT, true, CaptureMask::NONE)
            .expect("read");
        let resolved = resolved.expect("pass 1 hit");

        assert_eq!(expected_runs, runs.get(), "the probe must stop at the first failure");
        assert_eq!("にも疎", resolved.span.text);
        assert_eq!(6, resolved.span.cursor_byte_offset);
        let geom: Vec<(usize, PhysRect)> =
            resolved.span.geom.iter().map(|g| (g.char_count, g.rect)).collect();
        assert_eq!(
            vec![
                (1, PhysRect { x: 900, y: 480, w: 40, h: 40 }),
                (1, PhysRect { x: 940, y: 480, w: 40, h: 40 }),
                (1, PhysRect { x: 980, y: 480, w: 40, h: 40 }),
            ],
            geom,
        );
        assert_eq!(PhysRect { x: 980, y: 480, w: 40, h: 40 }, resolved.span.anchor);

        let scan: Vec<(ScanKind, PhysRect)> = scan.iter().map(|s| (s.kind, s.rect)).collect();
        assert_eq!(expected_scan, scan.as_slice());
    }

    #[test]
    fn a_margin_probe_failure_keeps_pass_one() {
        assert_probe_failure_keeps_pass_one(
            ProbeFailure::Margin,
            2,
            &[
                (ScanKind::Pass1, WRAP_REGION),
                (ScanKind::Tile, WRAP_MARGIN),
                (ScanKind::Anchor, PhysRect { x: 980, y: 480, w: 40, h: 40 }),
            ],
        );
    }

    #[test]
    fn a_tail_probe_failure_keeps_pass_one() {
        assert_probe_failure_keeps_pass_one(
            ProbeFailure::Tail,
            3,
            &[
                (ScanKind::Pass1, WRAP_REGION),
                (ScanKind::Tile, WRAP_MARGIN),
                (ScanKind::Tile, WRAP_TAIL),
                (ScanKind::Anchor, PhysRect { x: 980, y: 480, w: 40, h: 40 }),
            ],
        );
    }

    #[test]
    fn a_wrap_outside_the_box_is_read_by_two_bounded_probes() {
        let (mut source, runs) = scripted(false, 1);
        let (resolved, scan, _) = source
            .resolve_at_tiled_scanned(WRAP_AT, true, CaptureMask::NONE)
            .expect("read");
        let resolved = resolved.expect("a hit");
        assert_eq!(runs.get(), 3, "pass 1 and the two probes");
        assert_eq!("にも疎かった", resolved.span.text);
        assert_eq!("にも".len(), resolved.span.cursor_byte_offset);
        assert_eq!(6, resolved.span.geom.len(), "the combined answer keeps geometry");
        let kinds: Vec<(ScanKind, PhysRect)> = scan.iter().map(|s| (s.kind, s.rect)).collect();
        assert_eq!(
            vec![
                (ScanKind::Pass1, WRAP_REGION),
                (ScanKind::Tile, WRAP_MARGIN),
                (ScanKind::Tile, WRAP_TAIL),
                (ScanKind::Anchor, PhysRect { x: 980, y: 480, w: 40, h: 40 }),
            ],
            kinds,
        );
    }

    #[test]
    fn a_glued_box_at_the_probe_seam_does_not_hide_the_hovered_word() {
        let (mut source, runs) = glued_seam_scripted();
        let resolved =
            source.resolve_at_tiled(WRAP_AT, CaptureMask::NONE).expect("read").expect("a hit");
        assert_eq!("にも疎かった", resolved.span.text);
        assert_eq!(6, resolved.span.cursor_byte_offset);
        assert_eq!(PhysRect { x: 980, y: 480, w: 40, h: 40 }, resolved.span.anchor);
        assert_eq!(3, runs.get());
    }

    #[test]
    fn a_wide_output_keeps_the_hit_tail_and_margin_continuation() {
        let (mut source, runs) = wide_scripted(1);
        let resolved = source
            .resolve_at_tiled(PhysPoint { x: 1900, y: 500 }, CaptureMask::NONE)
            .expect("read")
            .expect("a hit");
        assert_eq!(runs.get(), 4, "pass 1 and the three bounded probes");
        assert_eq!("にも疎かった", resolved.span.text);
        assert_eq!(6, resolved.span.geom.len());
        assert_eq!(PhysRect { x: 1880, y: 480, w: 40, h: 40 }, resolved.span.anchor);
        assert_eq!(6, resolved.span.cursor_byte_offset);
    }

    #[test]
    fn a_wrap_inside_the_box_costs_no_probe() {
        let (mut source, runs) = scripted(true, 1);
        let resolved =
            source.resolve_at_tiled(WRAP_AT, CaptureMask::NONE).expect("read").expect("a hit");
        assert_eq!(runs.get(), 1);
        assert_eq!("にも疎かった", resolved.span.text);
    }

    /// The probe joins the reuse set. A static dwell then costs no OCR for the probe.
    #[test]
    fn a_dwell_reuses_the_probe() {
        let (mut source, runs) = scripted(false, 1);
        source.resolve_at_tiled(WRAP_AT, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 3);
        for _ in 0..3 {
            let resolved =
                source.resolve_at_tiled(WRAP_AT, CaptureMask::NONE).expect("dwell").expect("a hit");
            assert_eq!("にも疎かった", resolved.span.text);
        }
        assert_eq!(runs.get(), 3, "a static dwell must never recognise again");
    }
    /// A wider wrap hit must not replace pass 1's anchor when a forward tile adds text.
    #[test]
    fn a_forward_tile_keeps_the_pass_one_anchor_after_a_wider_wrap_probe() {
        let (mut source, runs) = wide_wrap_with_forward_tail_scripted();
        let resolved =
            source.resolve_at_tiled(WRAP_AT, CaptureMask::NONE).expect("read").expect("a hit");

        assert_eq!("疎先", resolved.span.text);
        assert_eq!(PhysRect { x: 980, y: 480, w: 40, h: 40 }, resolved.span.anchor);
        assert_eq!(4, runs.get(), "pass 1, two wrap probes, and one forward tile must run");
    }

    /// Forward tiles find nothing after a line's last character. Pass 1's answer,
    /// with the continuation and its geometry, must survive the tiled path.
    #[test]
    fn tiles_that_add_nothing_keep_the_wrapped_answer() {
        let (mut source, _) = scripted(false, 3);
        let resolved =
            source.resolve_at_tiled(WRAP_AT, CaptureMask::NONE).expect("read").expect("a hit");
        assert_eq!("にも疎かった", resolved.span.text);
        assert_eq!(6, resolved.span.geom.len());
    }

    /// New settings produce new answers, so the source must drop all stored results.
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
            discard_furigana: true,
        };
        source.apply_settings(settings, "ja");
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("read after reload");
        assert_eq!(runs.get(), 2, "a reload must re-recognise");
    }

    // -- where the two features meet: the mask is part of "same words" --

    /// `Counting`'s word maps to this box when the snapshot uses an upscale of 2.
    const WORD: PhysRect = PhysRect { x: 100, y: 100, w: 20, h: 20 };

    /// The reuse key must include the mask because the backend compares *raw* pixels.
    /// A popup over a still region leaves the raw grab unchanged, but it changes the masked pixels.
    /// Reuse of held words can expose the popup text, which the capture mask must prevent.
    #[test]
    fn an_unchanged_regrab_under_a_new_mask_is_recognised_again() {
        let (mut source, runs) = paced(true);
        let bare = source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1);
        assert_eq!(1, bare.lines.len(), "the word is there when nothing is masked");

        // The popup covers the word. The raw pixels stay the same, but the question changes.
        let over_the_word = live(WORD.inflated(4, 4));
        let masked = source.resolve_in_region(AT, BOX, over_the_word).expect("masked read");

        assert_eq!(runs.get(), 2, "a new mask must not be answered from the old words");
        assert!(
            masked.lines.is_empty(),
            "the masked word must be dropped, never served from the unmasked pass"
        );
    }

    /// A masked read must not reuse words recognized under a mask.
    /// Otherwise, the popup shadow can remain after the popup leaves.
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

    /// The same mask can reuse the result.
    /// The reuse key checks the question, not whether a mask exists.
    #[test]
    fn an_unchanged_regrab_under_the_same_mask_reuses() {
        let (mut source, runs) = paced(true);
        let popup = live(WORD.inflated(4, 4));
        source.resolve_in_region(AT, BOX, popup).expect("first read");
        assert_eq!(runs.get(), 1);
        source.resolve_in_region(AT, BOX, popup).expect("second read");
        assert_eq!(runs.get(), 1, "same box, same mask, same pixels: same answer");
    }

    /// This case tests why the clipped mask belongs in the reuse key.
    /// A popup appears after the first hover, so the full mask changes.
    /// The popup stays away from this box, and its clipped mask remains unchanged.
    /// This key lets the dwell re-check reuse the result.
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

    /// A popup can reach one box but miss another.
    /// The missed box can reuse its result, but the covered box cannot.
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

    // -- trigger mode's frozen hold --

    /// This backend returns different pixels on each grab.
    /// The test can distinguish frozen reads from live reads and count grabs through a handle.
    /// The backend can also refuse the copy.
    struct Moving {
        grabs: Rc<std::cell::Cell<u8>>,
        fails: bool,
    }

    impl RegionCapture for Moving {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            anyhow::ensure!(!self.fails, "the compositor refused the copy");
            self.grabs.set(self.grabs.get() + 1);
            Ok(Frame {
                // The grab count identifies the source of these pixels.
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

    /// This is the one output that `Moving` knows.
    const OUTPUT: PhysRect = PhysRect { x: 0, y: 0, w: 600, h: 400 };

    /// Build a source over `Moving`, record OCR input, and return the grab count.
    /// `words` contains image-pixel boxes that OCR reports, one line per box.
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

    /// Return the grab number whose pixels OCR saw.
    /// Alpha comes from the upscale, so color bytes carry the backend value.
    fn shown_grab(seen: &SeenImage) -> u8 {
        let (buf, _, _) = seen.borrow_mut().take().expect("OCR ran");
        let colours: Vec<u8> =
            buf.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let first = colours[0];
        assert!(colours.iter().all(|&b| b == first), "one grab's pixels are one value");
        first - 0x10
    }

    /// Freeze one full-output grab that contains the point.
    #[test]
    fn a_freeze_takes_one_full_output_grab() {
        let (mut source, _seen, grabs) = moving(false, Vec::new());
        assert_eq!(None, source.frozen_region(), "nothing is frozen to begin with");
        assert_eq!(OUTPUT, source.freeze(PhysPoint { x: 300, y: 200 }).expect("the freeze"));
        assert_eq!(Some(OUTPUT), source.frozen_region());
        assert_eq!(1, grabs.get(), "one press, one copy");
    }

    /// A frozen hold keeps press-time pixels even when the screen changes.
    #[test]
    fn a_frozen_hold_reads_the_press_time_pixels_and_no_others() {
        let (mut source, seen, _grabs) = moving(false, Vec::new());
        source.freeze(PhysPoint { x: 300, y: 200 }).expect("the freeze");
        source.recognise_at_capture(BOX, 1, CaptureMask::NONE).expect("a read in the hold");
        assert_eq!(1, shown_grab(&seen), "the press-time grab, not a later one");
    }

    /// A frozen hold performs no further copies.
    /// Every pass crops the one press-time frame.
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

    /// A live caller supplies the mask for a live grab.
    /// Frozen pixels predate the popup, so the hold ignores that mask.
    /// This test checks the read-through rule at the seam.
    #[test]
    fn a_mask_over_a_frozen_hold_is_ignored() {
        let (mut source, seen, _grabs) = moving(false, vec![PhysRect { x: 0, y: 0, w: 20, h: 20 }]);
        source.freeze(AT).expect("the freeze");
        // The popup covers the box that the next read uses.
        let read = source.resolve_in_region(AT, BOX, live(BOX)).expect("a read under the popup");
        assert_eq!(1, shown_grab(&seen), "no white fill may reach a frozen read");
        assert_eq!(1, read.lines.len(), "and no word may be dropped for touching it");
    }

    /// Release drops the frozen frame, so the next grab reads the screen.
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

    /// Trigger mode without a frozen frame cannot read.
    /// A failed press-time grab must fail every lookup in the hold.
    /// It must not quietly serve live pixels when no mask hides the popup.
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

    /// A freeze answers the same question with different pixels.
    /// Live words must not serve the frozen frame.
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

    /// The hold keeps one pixel set.
    /// A second read of the same box can therefore reuse its OCR result.
    #[test]
    fn a_second_read_of_the_same_box_in_one_hold_reuses_its_words() {
        let (mut source, runs) = paced(false);
        source.freeze(AT).expect("the freeze");
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("first read");
        assert_eq!(runs.get(), 1);
        source.resolve_in_region(AT, BOX, CaptureMask::NONE).expect("second read");
        assert_eq!(runs.get(), 1, "the same box of one frozen frame is the same words");
    }

    /// The `serve` hook passes caller-held pixels directly to the engine.
    /// It does not capture, upscale, or mask.
    /// A one-off job therefore does not need direct seam access.
    #[test]
    fn a_one_off_recognise_hands_the_callers_pixels_to_the_engine_untouched() {
        let (source, seen) = recording(vec![PhysRect { x: 0, y: 0, w: 4, h: 4 }]);
        let buf: Vec<u8> = (0..16u8).collect();

        let lines = source.recognise(&buf, 2, 2).expect("the engine answers");

        assert_eq!(1, lines.len(), "the engine's own lines come back");
        let seen = seen.borrow().clone().expect("the engine must have been asked");
        assert_eq!((buf, 2, 2), seen, "unscaled, unmasked, exactly what was handed over");
        assert_eq!("recording", source.engine_name(), "and the facade names it");
    }
}

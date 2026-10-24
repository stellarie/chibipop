//! This module connects `RegionCapture`, `OcrEngine`, shared layout, and hit-scan.
//! It accepts a point and returns a text span.
//! (ARCHITECTURE.md#workspace-and-seams).
//! Platform code supplies the two seams.
//! Core shares all code below the seams.

use crate::config::ProfileSession;
use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::layout::{
    band_of, box_orientation, discard_furigana, drop_slivers, grow, head_and_tail, hit_cut_by_edge,
    hit_scan, map_from_upscaled, nearest_line, normalise, region_around, resolve, resolve_wrap,
    spans_short_side, tile_forward, trim_probe_edges, wrap_probe, CaptureSize, OcrLine, OcrWord,
    Orientation, Resolved, GROWTH_STEPS,
};
use crate::text::ink;
use crate::text::sentence;
use crate::text::frozen::FrozenFrame;
use crate::text::mask::CaptureMask;
use crate::text::{Frame, OcrEngine, RegionCapture, TextSpan};
use anyhow::{Context, Result};
use std::cell::RefCell;

// The capture upscale factor belongs to each platform.
// It comes from `SettingsSnapshot::upscale`.
// Windows supplies 2 because its engine misreads small text at native resolution.
// Linux supplies 1 because meikiocr measures worse on upscaled crops.
// (ARCHITECTURE.md#ocr-engine).

// MAINTAINER NOTE - adaptive upscale retry, disabled 2026-08-08, code removed 2026-09-06.
// This note exceeds the 30-char comment rule because it records a method and its removal.
// Stella asked that this note remain in this file.
//
// After the first pass at the configured upscale, the code checked the tallest recognized word.
// If that word was shorter than 32 px, the code captured and read the same region again at 4x.
// The code used the new result only when it was not empty.
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
// The method also ran on ordinary body text.
// Glyphs of 28-31px remain clear, but stay below the 32px threshold.
// The method therefore added this cost to many reads.
//
// The retry stayed behind a `false` constant for a month and then left the tree.
// Before a new attempt at this method, take these steps:
//
// 1. Measure with these commands:
//      probe --at X,Y --repeat N            (warm timings, one process)
//      probe --at X,Y --upscale 2|4         (single pass, no retry)
// 2. Test against text with a known true string.
// 3. Compare transcription accuracy, not only whether more characters appeared.
// 4. Keep the method only when its accuracy gain justifies its measured cost.
//
// A lower threshold of about 22px costs less.
// That change limits the retry to tiny text.

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
    /// The pixels that OCR saw, after the mask and the upscale. The growth rule
    /// measures ink in them when the engine returns no hit.
    pub frame: Frame,
}

/// Return true when the read that answers the hover saw a cut glyph. Its hit line
/// spans the short side of `region`, or its hit word touches one edge with at least
/// half the box's thickness. Such a read is not an answer.
fn hit_is_cut(lines: &[OcrLine], cursor: PhysPoint, scan_alnum: bool, region: PhysRect) -> bool {
    hit_scan(lines, cursor, scan_alnum)
        .is_some_and(|(li, _)| spans_short_side(std::slice::from_ref(&lines[li]), region))
        || hit_cut_by_edge(lines, cursor, scan_alnum, region)
}

/// `PassOne` stores the read that answers pass 1 and every box that pass 1 grabbed.
struct PassOne {
    lines: Vec<OcrLine>,
    resolved: Option<Resolved>,
    /// The box whose `lines` answered. The probes and tiles start from it.
    region: PhysRect,
    /// Every box in grab order. The first is the configured box. Each next box is
    /// the previous one grown on its short side.
    boxes: Vec<PhysRect>,
}

/// `Recognised` stores the words from one region read for unchanged re-grabs.
///
/// Word rects already use physical pixels, so reuse only clones these lines.
///
/// The reuse key is `(region, factor, mask, pixels)`.
/// Pixels are compared exactly after masking and scaling, even when damage metadata is absent.
/// Each generation holds at most 16 MiB of pixels. Larger reads still run OCR normally.
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
    pixels: Option<std::sync::Arc<[u8]>>,
}

const PIXEL_CACHE_BYTES: usize = 16 * 1024 * 1024;

struct CaptureLog<'a> {
    frame: &'a Frame,
    region: PhysRect,
    factor: i32,
    mask: CaptureMask,
    reused: bool,
    capture_ms: f64,
    ocr_ms: f64,
    lines: &'a [OcrLine],
    outcome: &'static str,
    error: Option<&'a anyhow::Error>,
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
    ocr: RefCell<Box<dyn OcrEngine>>,
    settings: SettingsSnapshot,
    language: RefCell<String>,
    profile_scope: Option<ProfileSession>,
    show_lookup_log: bool,
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
        self.recognise_with(
            bgra,
            w,
            h,
            self.settings.discard_furigana,
            self.show_lookup_log,
        )
    }

    pub fn recognise_for_session(
        &self,
        session: &ProfileSession,
        bgra: &[u8],
        w: i32,
        h: i32,
    ) -> Result<Vec<OcrLine>> {
        let config = session.config();
        self.ocr.borrow_mut().set_language(&config.ocr.language);
        *self.language.borrow_mut() = config.ocr.language.clone();
        self.recognise_with(
            bgra,
            w,
            h,
            config.ocr.discard_furigana,
            self.show_lookup_log,
        )
    }

    fn recognise_with(
        &self,
        bgra: &[u8],
        w: i32,
        h: i32,
        discard_ruby: bool,
        show_log: bool,
    ) -> Result<Vec<OcrLine>> {
        let started = std::time::Instant::now();
        let lines = match self.ocr.borrow().recognise(bgra, w, h) {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!(
                    "chibipop: ocr job=one_off size={}x{} bytes={} outcome=failed ocr_ms={:.3} error={error:#}",
                    w,
                    h,
                    bgra.len(),
                    started.elapsed().as_secs_f64() * 1000.0,
                );
                return Err(error);
            }
        };
        let lines = if discard_ruby {
            discard_furigana(lines)
        } else {
            lines
        };
        let words = lines.iter().map(|line| line.words.len()).sum::<usize>();
        eprintln!(
            "chibipop: ocr job=one_off size={}x{} bytes={} outcome=ok ocr_ms={:.3} lines={} words={}",
            w,
            h,
            bgra.len(),
            started.elapsed().as_secs_f64() * 1000.0,
            lines.len(),
            words,
        );
        if show_log {
            use std::fmt::Write;
            let mut message = String::new();
            for (line_index, line) in lines.iter().enumerate() {
                for (word_index, word) in line.words.iter().enumerate() {
                    let _ = writeln!(&mut message,
                        "chibipop: ocr job=one_off line={} word={} text={:?} rect={},{} {}x{}",
                        line_index,
                        word_index,
                        word.text,
                        word.rect.x,
                        word.rect.y,
                        word.rect.w,
                        word.rect.h,
                    );
                }
            }
            let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), message.as_bytes());
        }
        Ok(lines)
    }

    /// Return the name of the engine that reads this source's pixels.
    /// A diagnostic uses this name in the `probe` report.
    pub fn engine_name(&self) -> String {
        self.ocr.borrow().name().to_string()
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
            ocr: RefCell::new(ocr),
            settings,
            language: RefCell::new(String::new()),
            profile_scope: None,
            show_lookup_log: false,
            frozen: None,
            recognised: Vec::new(),
            previous: Vec::new(),
        }
    }

    pub(crate) fn set_show_lookup_log(&mut self, enabled: bool) {
        self.show_lookup_log = enabled;
    }

    /// Replace the OCR settings.
    pub fn apply_settings(&mut self, settings: SettingsSnapshot, language: &str) {
        if self.settings != settings || self.language.borrow().as_str() != language {
            self.ocr.borrow_mut().set_language(language);
            *self.language.borrow_mut() = language.to_string();
            self.settings = settings;
            self.recognised.clear();
            self.previous.clear();
        }
    }

    pub fn set_profile_scope(&mut self, session: &ProfileSession) {
        if self.profile_scope.as_ref() != Some(session) {
            self.profile_scope = Some(session.clone());
            self.recognised.clear();
            self.previous.clear();
        }
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

    /// Drop every lookup result owned by this source.
    pub fn clear_lookup_cache(&mut self) {
        self.frozen = None;
        self.recognised.clear();
        self.previous.clear();
    }

    /// Swap OCR backend.
    pub fn replace_ocr(&self, ocr: Box<dyn OcrEngine>) {
        drop(self.ocr.replace(ocr));
        self.language.borrow_mut().clear();
    }

    /// Return the box covered by the hold's frozen grab, if one exists.
    pub fn frozen_region(&self) -> Option<PhysRect> {
        match &self.frozen {
            Some(Frozen::Held(f)) => Some(f.region()),
            _ => None,
        }
    }

    /// Return pass 1: its lines and outcome, the box that answered, and every box it
    /// grabbed, in order.
    ///
    /// The box grows on its short side, at most [`GROWTH_STEPS`] times, when the
    /// box is too small for the text under the cursor (issue #92):
    ///
    /// - A recognized line spans the short side. The glyph is at least as tall as the
    ///   box, and the engine returned a fragment or a misread.
    /// - No hit exists, and ink under the cursor spans the configured short side
    ///   ([`ink::spans_short_side`]). The engine returned nothing for a large glyph.
    /// - A grown box returns no text. It still contains the glyph.
    ///
    /// A cut read is not an answer. Its hit line spans the box, and the engine read a
    /// cut glyph. The answer is the last read whose hit line does not span its box.
    /// When no read qualifies, the last read remains, with or without a hit. A failed
    /// growth grab stops the growth. The outline shows every box that this method grabs.
    fn resolve_at_verbose(&mut self, cursor: PhysPoint, mask: CaptureMask) -> Result<PassOne> {
        let reference = self.settings.capture.short();
        let factor = self.settings.upscale;
        let alnum = self.settings.scan_alphanumeric;
        let uncut_hit = |read: &RegionRead, region: PhysRect| {
            read.resolved.is_some() && !hit_is_cut(&read.lines, cursor, alnum, region)
        };
        let mut region = region_around(cursor, self.settings.prefer_vertical, self.settings.capture);
        let mut boxes = vec![region];
        let mut read = self.resolve_in_region(cursor, region, mask)?;
        let mut best: Option<(RegionRead, PhysRect)> = None;
        // A frozen hold reads without a mask, as `recognise_at_capture` does.
        let masked = if self.frozen.is_some() { CaptureMask::NONE } else { mask };
        for step in 0..GROWTH_STEPS {
            let ink_spans = || {
                let popup: Vec<PhysRect> = masked
                    .overlap_in(region)
                    .map(|r| PhysRect { x: r.x * factor, y: r.y * factor, w: r.w * factor, h: r.h * factor })
                    .collect();
                ink::spans_short_side(&read.frame, region, cursor, factor, reference, &popup)
            };
            let cut = spans_short_side(&read.lines, region)
                || hit_cut_by_edge(&read.lines, cursor, alnum, region)
                || (step > 0 && read.lines.is_empty())
                || (read.resolved.is_none() && ink_spans());
            if !cut {
                break;
            }
            let grown = grow(region, self.capture.bounds_containing(cursor));
            let next = match self.resolve_in_region(cursor, grown, mask) {
                Ok(next) => next,
                Err(e) => {
                    eprintln!("chibipop: capture growth failed. Use the smaller box: {e:#}");
                    break;
                }
            };
            if uncut_hit(&read, region) {
                best = Some((read, region));
            }
            read = next;
            region = grown;
            boxes.push(region);
        }
        let (read, region) = match best {
            Some(best) if !uncut_hit(&read, region) => best,
            _ => (read, region),
        };
        Ok(PassOne { lines: read.lines, resolved: read.resolved, region, boxes })
    }

    /// Resolve a caller-supplied box.
    pub fn resolve_in_region(
        &mut self,
        cursor: PhysPoint,
        region: PhysRect,
        mask: CaptureMask,
    ) -> Result<RegionRead> {
        let (lines, frame) = self.recognise_at_capture(region, self.settings.upscale, mask)?;
        let resolved = resolve(&lines, cursor, region, self.settings.scan_alphanumeric);
        Ok(RegionRead {
            lines,
            resolved,
            source: frame.source,
            fallback: frame.fallback.clone(),
            unchanged: frame.unchanged,
            frame,
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
                let (mut lines, _) = source.recognise_oriented(
                    tile, source.settings.upscale, mask, orientation,
                )?;
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
        self.recognise_oriented(region, factor, mask, box_orientation(region))
    }

    fn recognise_oriented(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
        orientation: Orientation,
    ) -> Result<(Vec<OcrLine>, Frame)> {
        let discard_ruby = self.settings.discard_furigana;
        let filter = |lines| {
            let lines = drop_slivers(lines, orientation);
            if discard_ruby { discard_furigana(lines) } else { lines }
        };
        // One value controls the fill, word drop, and reuse key.
        // These three actions therefore use the same mask.
        let mask = match self.frozen {
            Some(_) => CaptureMask::NONE,
            None => mask.clipped_to(region),
        };
        let capture_started = std::time::Instant::now();
        let frame = match self.grab(region, factor, mask) {
            Ok(frame) => frame,
            Err(error) => {
                let elapsed_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
                eprintln!(
                    "chibipop: capture region={},{} {}x{} factor={} mask={} source=unknown unchanged=false reused=false bytes=0 capture_ms={elapsed_ms:.3} ocr_ms=0.000 lines=0 words=0 outcome=failed error={error:#}",
                    region.x,
                    region.y,
                    region.w,
                    region.h,
                    factor,
                    mask_label(mask, region),
                );
                return Err(error);
            }
        };
        let capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
        if let Some(lines) = self.reuse(region, factor, mask, &frame.buf) {
            let lines = filter(lines);
            self.log_capture(CaptureLog {
                frame: &frame,
                region,
                factor,
                mask,
                reused: true,
                capture_ms,
                ocr_ms: 0.0,
                lines: &lines,
                outcome: "ok",
                error: None,
            });
            return Ok((lines, frame));
        }
        let ocr_started = std::time::Instant::now();
        let raw = match self.ocr.borrow().recognise(&frame.buf, frame.w, frame.h) {
            Ok(raw) => raw,
            Err(error) => {
                let ocr_ms = ocr_started.elapsed().as_secs_f64() * 1000.0;
                self.log_capture(CaptureLog {
                    frame: &frame,
                    region,
                    factor,
                    mask,
                    reused: false,
                    capture_ms,
                    ocr_ms,
                    lines: &[],
                    outcome: "failed",
                    error: Some(&error),
                });
                return Err(error);
            }
        };
        let ocr_ms = ocr_started.elapsed().as_secs_f64() * 1000.0;
        let origin = PhysPoint { x: region.x, y: region.y };
        let lines = to_desktop(raw, origin, factor, mask);
        self.remember(region, factor, mask, &lines, &frame.buf);
        let lines = filter(lines);
        self.log_capture(CaptureLog {
            frame: &frame,
            region,
            factor,
            mask,
            reused: false,
            capture_ms,
            ocr_ms,
            lines: &lines,
            outcome: "ok",
            error: None,
        });
        Ok((lines, frame))
    }

    fn log_capture(&self, log: CaptureLog<'_>) {
        use std::fmt::Write;
        let words = log.lines.iter().map(|line| line.words.len()).sum::<usize>();
        let mut message = format!(
            "chibipop: capture region={},{} {}x{} factor={} mask={} source={} fallback={} unchanged={} reused={} bytes={} capture_ms={:.3} ocr_ms={:.3} lines={} words={} outcome={}\n",
            log.region.x,
            log.region.y,
            log.region.w,
            log.region.h,
            log.factor,
            mask_label(log.mask, log.region),
            log.frame.source,
            log.frame.fallback.as_deref().unwrap_or("none"),
            log.frame.unchanged,
            log.reused,
            log.frame.buf.len(),
            log.capture_ms,
            log.ocr_ms,
            log.lines.len(),
            words,
            log.outcome,
        );
        if let Some(error) = log.error {
            let _ = writeln!(&mut message, "chibipop: capture stage=ocr outcome=failed error={error:#}");
        }
        if !self.show_lookup_log {
            let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), message.as_bytes());
            return;
        }
        for (line_index, line) in log.lines.iter().enumerate() {
            let text = line.words.iter().map(|word| word.text.as_str()).collect::<String>();
            let _ = writeln!(&mut message, "chibipop: capture line={} text={text:?}", line_index);
            for (word_index, word) in line.words.iter().enumerate() {
                let _ = writeln!(&mut message,
                    "chibipop: capture word={} line={} text={:?} rect={},{} {}x{}",
                    word_index,
                    line_index,
                    word.text,
                    word.rect.x,
                    word.rect.y,
                    word.rect.w,
                    word.rect.h,
                );
            }
        }
        let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), message.as_bytes());
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
        pixels: &[u8],
    ) -> Option<Vec<OcrLine>> {
        let same =
            |r: &&Recognised| r.region == region && r.factor == factor && r.mask == mask
                && r.pixels.as_deref() == Some(pixels);
        if let Some(hit) = self.recognised.iter().find(same) {
            return Some(hit.lines.clone());
        }
        let hit = self.previous.iter().find(same)?;
        let lines = hit.lines.clone();
        let used: usize = self.recognised.iter()
            .filter(|r| r.region != region || r.factor != factor || r.mask != mask)
            .filter_map(|r| r.pixels.as_ref())
            .map(|p| p.len()).sum();
        let pixels = hit.pixels.as_ref()
            .filter(|p| p.len() <= PIXEL_CACHE_BYTES.saturating_sub(used)).cloned();
        let entry = Recognised { region, factor, mask, lines: lines.clone(), pixels };
        match self.recognised.iter_mut().find(|r| r.region == region && r.factor == factor && r.mask == mask) {
            Some(slot) => *slot = entry,
            None => self.recognised.push(entry),
        }
        Some(lines)
    }

    /// Save this pass's words for the next read.
    fn remember(
        &mut self,
        region: PhysRect,
        factor: i32,
        mask: CaptureMask,
        lines: &[OcrLine],
        pixels: &[u8],
    ) {
        let used: usize = self.recognised.iter()
            .filter(|r| r.region != region || r.factor != factor || r.mask != mask)
            .filter_map(|r| r.pixels.as_ref()).map(|p| p.len()).sum();
        let pixels = (pixels.len() <= PIXEL_CACHE_BYTES.saturating_sub(used))
            .then(|| std::sync::Arc::from(pixels));
        let entry = Recognised { region, factor, mask, lines: lines.to_vec(), pixels };
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
        let PassOne { lines, resolved, region, boxes } = self.resolve_at_verbose(cursor, mask)?;
        let mut scan = Vec::new();
        let Some(pass_one) = resolved else { return Ok((None, scan, lines)) };
        if collect {
            scan.extend(boxes.into_iter().map(|rect| ScanRect { rect, kind: ScanKind::Pass1 }));
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
            match self.recognise_oriented(probe, self.settings.upscale, mask, orientation) {
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
        let (lines, _) =
            self.recognise_oriented(tile, self.settings.upscale, mask, orientation)?;
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

fn mask_label(mask: CaptureMask, region: PhysRect) -> String {
    let rectangles: Vec<_> = mask.overlap_in(region)
        .map(|rect| format!("{},{} {}x{}", rect.x, rect.y, rect.w, rect.h)).collect();
    if rectangles.is_empty() { "none".to_string() } else { rectangles.join(";") }
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
    let pitch = w2 as usize * 4;
    for y in 0..h as usize {
        let row = y * factor as usize * pitch;
        for (x, pixel) in src[y * w as usize * 4..(y + 1) * w as usize * 4]
            .as_chunks::<4>().0.iter().enumerate() {
            let start = row + x * factor as usize * 4;
            for dest in dst[start..start + factor as usize * 4].as_chunks_mut::<4>().0 {
                *dest = [pixel[0], pixel[1], pixel[2], 0xFF];
            }
        }
        for repeat in 1..factor as usize {
            dst.copy_within(row..row + pitch, row + repeat * pitch);
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
                buf: vec![if self.unchanged { 0 } else { self.grabs.get() as u8 }; (region.w * region.h * 4) as usize],

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

    // -- the wrap probe --


    // -- where the two features meet: the mask is part of "same words" --

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

    /// Freeze one full-output grab that contains the point.
    #[test]
    fn a_freeze_takes_one_full_output_grab() {
        let (mut source, _seen, grabs) = moving(false, Vec::new());
        assert_eq!(None, source.frozen_region(), "nothing is frozen to begin with");
        assert_eq!(OUTPUT, source.freeze(PhysPoint { x: 300, y: 200 }).expect("the freeze"));
        assert_eq!(Some(OUTPUT), source.frozen_region());
        assert_eq!(1, grabs.get(), "one press, one copy");
    }

}

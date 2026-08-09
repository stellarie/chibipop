//! Windows OCR recognition.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::capture::{
    capture_upscaled_by, cursor_position, init_dpi_awareness, Capture, CaptureSource, UPSCALE,
};
use crate::text::layout::{
    band_of, head_and_tail, map_from_upscaled, nearest_line, normalise, region_around, resolve,
    tile_forward, CaptureSize, OcrLine, OcrWord, Orientation, Resolved,
};
use crate::text::{TextSource, TextSpan};
use anyhow::{Context, Result};
use std::mem::size_of;
use std::time::{Duration, Instant};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Blocks; .get() is gone.
fn wait_blocking<T>(op: windows_future::IAsyncOperation<T>) -> Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    let deadline = Instant::now() + OCR_TIMEOUT;
    loop {
        if op.Status()? != windows_future::AsyncStatus::Started {
            return Ok(op.GetResults()?);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("OCR did not finish within {OCR_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Bound on one OCR call.
const OCR_TIMEOUT: Duration = Duration::from_secs(5);

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
// all-black frames (see capture.rs), so "2x found nothing" was often
// a dead capture rather than a scale problem, and which pass got a
// live frame was luck. Re-measured on the repaired pipeline it cost
// ~36 ms of the ~141 ms round trip - two captures and two OCR passes,
// the second over 4x the pixels - and it was not reliably better:
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

/// Coords are upscaled-image.
pub fn recognise(engine: &OcrEngine, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
    let ibuffer = CryptographicBuffer::CreateFromByteArray(buf)
        .context("wrapping the pixel buffer")?;
    // 32bpp BGRA; alpha is junk.
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &ibuffer,
        BitmapPixelFormat::Bgra8,
        w,
        h,
        BitmapAlphaMode::Ignore,
    )
    .context("building a SoftwareBitmap from the capture")?;

    let result = wait_blocking(engine.RecognizeAsync(&bitmap)?)
        .context("running OCR")?;

    let mut lines = Vec::new();
    // Size(), not Count().
    for line in &result.Lines()? {
        let mut words = Vec::new();
        for word in &line.Words()? {
            let r = word.BoundingRect()?;
            words.push(OcrWord {
                text: word.Text()?.to_string(),
                rect: PhysRect {
                    x: r.X as i32,
                    y: r.Y as i32,
                    w: r.Width as i32,
                    h: r.Height as i32,
                },
            });
        }
        if !words.is_empty() {
            lines.push(OcrLine { words });
        }
    }
    Ok(lines)
}

/// The monitor holding `p`.
fn monitor_bounds_containing(p: PhysPoint) -> PhysRect {
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: p.x, y: p.y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let rc = mi.rcMonitor;
            PhysRect { x: rc.left, y: rc.top, w: rc.right - rc.left, h: rc.bottom - rc.top }
        } else {
            PhysRect { x: p.x - 960, y: p.y - 540, w: 1920, h: 1080 }
        }
    }
}

/// One region read, with provenance.
pub struct RegionRead {
    pub lines: Vec<OcrLine>,
    pub resolved: Option<Resolved>,
    pub source: CaptureSource,
    pub dxgi_error: Option<String>,
}

/// The OCR knobs, reloadable.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SettingsSnapshot {
    pub max_passes: u8,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
}

impl SettingsSnapshot {
    /// Replace every field at once.
    pub fn apply(&mut self, max_passes: u8, prefer_vertical: bool,
                 capture: CaptureSize, scan_alphanumeric: bool) {
        self.max_passes = max_passes;
        self.prefer_vertical = prefer_vertical;
        self.capture = capture;
        self.scan_alphanumeric = scan_alphanumeric;
    }
}

pub struct OcrTextSource {
    engine: OcrEngine,
    settings: SettingsSnapshot,
}

impl OcrTextSource {
    /// Inits WinRT + engine once.
    pub fn new(max_passes: u8, prefer_vertical: bool,
               capture: CaptureSize, scan_alphanumeric: bool) -> Result<Self> {
        init_dpi_awareness()?;
        // Else CO_E_NOTINITIALIZED.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")? };
        let lang = Language::CreateLanguage(&HSTRING::from("ja"))?;
        let engine = OcrEngine::TryCreateFromLanguage(&lang).context(
            "no Japanese OCR recogniser available - add Japanese under \
             Windows Settings, Time & language, Language & region",
        )?;
        let settings =
            SettingsSnapshot { max_passes, prefer_vertical, capture, scan_alphanumeric };
        Ok(OcrTextSource { engine, settings })
    }

    /// Swap in new OCR settings.
    pub fn apply_settings(&mut self, max_passes: u8, prefer_vertical: bool,
                          capture: CaptureSize, scan_alphanumeric: bool) {
        self.settings.apply(max_passes, prefer_vertical, capture, scan_alphanumeric);
    }

    /// The engine from `new`.
    pub fn engine(&self) -> &OcrEngine {
        &self.engine
    }

    /// Lines plus the outcome.
    pub fn resolve_at_verbose(
        &self,
        cursor: PhysPoint,
    ) -> Result<(Vec<OcrLine>, Option<Resolved>)> {
        let read = self.resolve_in_region(
            cursor,
            region_around(cursor, self.settings.prefer_vertical, self.settings.capture),
        )?;
        Ok((read.lines, read.resolved))
    }

    /// As above, explicit box.
    pub fn resolve_in_region(&self, cursor: PhysPoint, region: PhysRect) -> Result<RegionRead> {
        let (lines, cap) = self.recognise_at_capture(region, UPSCALE)?;
        let (lines, cap) = if ADAPTIVE_RETRY && glyphs_look_small(&lines) {
            match self.recognise_at_capture(region, RETRY_UPSCALE) {
                Ok((bigger, big_cap)) if !bigger.is_empty() => (bigger, big_cap),
                _ => (lines, cap),
            }
        } else {
            (lines, cap)
        };
        let resolved = resolve(&lines, cursor, self.settings.scan_alphanumeric);
        Ok(RegionRead {
            lines,
            resolved,
            source: cap.source,
            dxgi_error: cap.dxgi_error,
        })
    }

    /// Capture + recognise at `factor`, mapped to physical.
    pub fn recognise_at(&self, region: PhysRect, factor: i32) -> Result<Vec<OcrLine>> {
        Ok(self.recognise_at_capture(region, factor)?.0)
    }

    /// As above, keeping the pixels read.
    pub fn recognise_at_capture(
        &self,
        region: PhysRect,
        factor: i32,
    ) -> Result<(Vec<OcrLine>, Capture)> {
        let cap = capture_upscaled_by(region, factor)?;
        let raw = recognise(&self.engine, &cap.buf, cap.w, cap.h)?;
        let origin = PhysPoint { x: region.x, y: region.y };
        let lines = raw
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
        Ok((lines, cap))
    }

    /// Span plus orientation.
    pub fn resolve_at(&self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        Ok(self.resolve_at_verbose(cursor)?.1)
    }

    /// Tiled, scan rects dropped.
    pub fn resolve_at_tiled(&self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        self.resolve_at_tiled_scanned(cursor, false).map(|(r, _)| r)
    }

    /// Tiled read + scan rects.
    pub fn resolve_at_tiled_scanned(
        &self,
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
        let bounds = monitor_bounds_containing(band.center());

        let mut failed = false;
        let tail = tile_forward(
            band,
            tail_start,
            orientation,
            usize::from(self.settings.max_passes - 1),
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
        &self,
        tile: PhysRect,
        perpendicular_centre: i32,
        orientation: Orientation,
        tolerance: i32,
    ) -> Result<Vec<OcrWord>> {
        let cap = capture_upscaled_by(tile, UPSCALE)?;
        let origin = PhysPoint { x: tile.x, y: tile.y };
        let lines: Vec<OcrLine> = recognise(&self.engine, &cap.buf, cap.w, cap.h)?
            .into_iter()
            .map(|l| OcrLine {
                words: l
                    .words
                    .into_iter()
                    .map(|word| OcrWord {
                        rect: map_from_upscaled(word.rect, origin, UPSCALE),
                        text: word.text,
                    })
                    .collect(),
            })
            .collect();
        Ok(nearest_line(&lines, perpendicular_centre, orientation, tolerance)
            .map(|line| line.words.clone())
            .unwrap_or_default())
    }

    /// Where the pointer is now.
    pub fn cursor(&self) -> Result<PhysPoint> {
        cursor_position()
    }
}

impl TextSource for OcrTextSource {
    fn at(&self, p: PhysPoint) -> Result<Option<TextSpan>> {
        Ok(self.resolve_at(p)?.map(|r| r.span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Reload replaces every OCR field.
    #[test]
    fn apply_settings_replaces_all_four_fields() {
        let mut s = SettingsSnapshot {
            max_passes: 1,
            prefer_vertical: false,
            capture: CaptureSize { w: 500, h: 100 },
            scan_alphanumeric: true,
        };
        s.apply(3, true, CaptureSize { w: 800, h: 120 }, false);
        assert_eq!(3, s.max_passes);
        assert!(s.prefer_vertical);
        assert_eq!(CaptureSize { w: 800, h: 120 }, s.capture);
        assert!(!s.scan_alphanumeric);
    }
}

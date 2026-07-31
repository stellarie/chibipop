//! Windows OCR recognition.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::lookup::engine::MAX_LOOKUP_CHARS;
use crate::text::capture::{capture_upscaled, cursor_position, init_dpi_awareness, UPSCALE};
use crate::text::layout::{
    band_of, map_from_upscaled, nearest_line, region_around, resolve, tile_forward, OcrLine,
    OcrWord, Orientation, Resolved,
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

pub struct OcrTextSource {
    engine: OcrEngine,
    max_passes: u8,
}

impl OcrTextSource {
    /// Inits WinRT + engine once.
    pub fn new(max_passes: u8) -> Result<Self> {
        init_dpi_awareness()?;
        // Else CO_E_NOTINITIALIZED.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")? };
        let lang = Language::CreateLanguage(&HSTRING::from("ja"))?;
        let engine = OcrEngine::TryCreateFromLanguage(&lang).context(
            "no Japanese OCR recogniser available - add Japanese under \
             Windows Settings, Time & language, Language & region",
        )?;
        Ok(OcrTextSource { engine, max_passes })
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
        self.resolve_in_region(cursor, region_around(cursor))
    }

    /// As above, explicit box.
    pub fn resolve_in_region(
        &self,
        cursor: PhysPoint,
        region: PhysRect,
    ) -> Result<(Vec<OcrLine>, Option<Resolved>)> {
        let (buf, w, h) = capture_upscaled(region)?;
        let raw = recognise(&self.engine, &buf, w, h)?;
        let origin = PhysPoint { x: region.x, y: region.y };
        let lines: Vec<OcrLine> = raw
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
        let resolved = resolve(&lines, cursor);
        Ok((lines, resolved))
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
        let (_, resolved) = self.resolve_at_verbose(cursor)?;
        let mut scan = Vec::new();
        let Some(first) = resolved else { return Ok((None, scan)) };
        if collect {
            scan.push(ScanRect { rect: region_around(cursor), kind: ScanKind::Pass1 });
        }
        if self.max_passes <= 1 {
            if collect {
                scan.push(ScanRect { rect: first.span.anchor, kind: ScanKind::Anchor });
            }
            return Ok((Some(first), scan));
        }

        let band = band_of(first.span.anchor, first.orientation);
        let start = match first.orientation {
            Orientation::Horizontal => first.span.anchor.x,
            Orientation::Vertical => first.span.anchor.y,
        };
        let perpendicular_centre = match first.orientation {
            Orientation::Horizontal => first.span.anchor.center().y,
            Orientation::Vertical => first.span.anchor.center().x,
        };
        let line_tolerance = match first.orientation {
            Orientation::Horizontal => first.span.anchor.h / 2,
            Orientation::Vertical => first.span.anchor.w / 2,
        };
        let bounds = monitor_bounds_containing(band.center());

        let mut failed = false;
        let text = tile_forward(
            band,
            start,
            first.orientation,
            usize::from(self.max_passes - 1),
            MAX_LOOKUP_CHARS,
            bounds,
            |tile| {
                if collect {
                    scan.push(ScanRect { rect: tile, kind: ScanKind::Tile });
                }
                match self.words_in(tile, perpendicular_centre, first.orientation, line_tolerance) {
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
            scan.push(ScanRect { rect: first.span.anchor, kind: ScanKind::Anchor });
        }

        if text.is_empty() {
            return Ok((Some(first), scan));
        }
        Ok((
            Some(Resolved {
                // Stitched: no geometry.
                span: TextSpan {
                    text,
                    cursor_byte_offset: 0,
                    anchor: first.span.anchor,
                    geom: Vec::new(),
                },
                orientation: first.orientation,
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
        let (buf, w, h) = capture_upscaled(tile)?;
        let origin = PhysPoint { x: tile.x, y: tile.y };
        let lines: Vec<OcrLine> = recognise(&self.engine, &buf, w, h)?
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

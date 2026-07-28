//! Windows.Media.Ocr recognition and the OCR-backed `TextSource`.
//!
//! Windows-only. Its job is to turn pixels into plain `OcrLine` values in
//! virtual-desktop coordinates and hand them to `layout`, which does all the
//! actual reasoning.

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

/// Block on a WinRT async operation.
///
/// `.get()` was removed in windows 0.62, and `windows-future`'s `Async::join()`
/// is `pub` inside a private module that is only glob-imported at that crate's
/// root — so it is unreachable from any downstream crate, not merely
/// feature-gated. Polling `Status()` is the working pattern.
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

/// Recognise Japanese in a tightly packed BGRA buffer, using an
/// already-created `engine`.
///
/// The engine is expensive to create (`Language::CreateLanguage` +
/// `OcrEngine::TryCreateFromLanguage`), so callers create it once — see
/// `OcrTextSource::new` — and pass it in on every frame rather than this
/// function creating one itself.
///
/// Returned coordinates are in **upscaled-image** space — the caller maps them
/// back to the virtual desktop.
pub fn recognise(engine: &OcrEngine, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
    let ibuffer = CryptographicBuffer::CreateFromByteArray(buf)
        .context("wrapping the pixel buffer")?;
    // Bgra8 is what a 32bpp GDI capture already is. Alpha is Ignore because
    // GDI never populates that byte with anything meaningful.
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
    // IVectorView exposes Size(), not Count() - the C# projection's name.
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

/// The bounds of the monitor containing `p` (spec §5) - same
/// `MonitorFromPoint` + `GetMonitorInfoW` pair `app.rs`'s `monitor_rect_for`
/// already uses to place the popup, applied here to keep a tile from
/// reading a neighbouring monitor's pixels (I2). `MONITOR_DEFAULTTONEAREST`
/// never yields a null `HMONITOR`, so only `GetMonitorInfoW` itself can
/// fail; on that unreachable-in-practice path this falls back to a
/// generous box centred on `p` rather than an absolute-origin guess, so a
/// clamp still has real room to work with instead of collapsing to nothing.
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
    /// Initialises the process for capture and WinRT, and creates the
    /// Japanese recogniser once so every later frame can reuse it. Fails
    /// loudly here rather than on the first hover.
    ///
    /// `max_passes` is the total captures a hover spends: one to locate the
    /// word, the rest reading forward from it in
    /// [`resolve_at_tiled`](Self::resolve_at_tiled). `1` disables tiling.
    pub fn new(max_passes: u8) -> Result<Self> {
        init_dpi_awareness()?;
        // WinRT activation fails with CO_E_NOTINITIALIZED without this.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")? };
        let lang = Language::CreateLanguage(&HSTRING::from("ja"))?;
        let engine = OcrEngine::TryCreateFromLanguage(&lang).context(
            "no Japanese OCR recogniser available - see \
             docs/superpowers/findings/2026-07-26-m0-ocr-availability.md",
        )?;
        Ok(OcrTextSource { engine, max_passes })
    }

    /// The engine created in `new`, for callers that need to drive
    /// `recognise` directly (the OCR fixture test) instead of going through
    /// `resolve_at`.
    pub fn engine(&self) -> &OcrEngine {
        &self.engine
    }

    /// Full resolution, exposing both the recognised OCR lines (mapped to
    /// virtual-desktop coordinates) and the resolution outcome. `probe` wants
    /// the lines too, so it can show what OCR actually saw; `resolve_at`
    /// below is this with the lines discarded.
    pub fn resolve_at_verbose(
        &self,
        cursor: PhysPoint,
    ) -> Result<(Vec<OcrLine>, Option<Resolved>)> {
        self.resolve_in_region(cursor, region_around(cursor))
    }

    /// [`resolve_at_verbose`](Self::resolve_at_verbose) against an explicit
    /// capture box instead of the standard one centred on `cursor`.
    ///
    /// Windows' OCR segments a whole captured image at once, so the framing
    /// of that image changes what it reads - the same screen text recognises
    /// differently when the box shifts by 50 pixels. This exists so `probe`
    /// can vary the box and measure that, rather than the region size being
    /// a constant nobody can test.
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

    /// Full resolution, exposing the orientation as well as the span. `watch`
    /// wants that much detail but not the per-word OCR spam; `TextSource::at`
    /// does not even want the orientation.
    pub fn resolve_at(&self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        Ok(self.resolve_at_verbose(cursor)?.1)
    }

    /// [`resolve_at_tiled_scanned`](Self::resolve_at_tiled_scanned) with
    /// collection off, and the scan rectangles discarded.
    pub fn resolve_at_tiled(&self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        self.resolve_at_tiled_scanned(cursor, false).map(|(r, _)| r)
    }

    /// Resolve a hover, reading forward in tiles (the two-pass spec), and -
    /// when `collect` is true - every region actually captured along the way.
    ///
    /// Pass 1 is [`resolve_at_verbose`](Self::resolve_at_verbose)'s capture,
    /// used for geometry only - its text is edge-clipped at its own boundary
    /// and is discarded (spec D1). Tiles then re-read forward from the hovered
    /// word's leading edge.
    ///
    /// Falls back to pass 1's own span whenever tiling adds nothing: one
    /// configured pass, an empty tiling result, or a tile that errors. Tiling
    /// must never turn a working hover into a failed one.
    ///
    /// **A stitched span carries no `TextSpan::geom`**, because `tile_forward`
    /// returns a `String` and drops the boxes. The match highlight therefore
    /// does not draw on the tiled path - `union_chars` returns `None` on empty
    /// geometry, so it is absent rather than wrong. The default is one pass, on
    /// which the highlight works; carrying geometry through the seam is
    /// deferred with the rest of the tiling rework (`docs/BACKLOG.md`).
    ///
    /// Two more guards travel with every tile (both pure, in `layout.rs`):
    /// `line_tolerance` is half the hovered word's own perpendicular size,
    /// mirroring `hit_scan`'s bound, so `nearest_line` cannot silently
    /// borrow a neighbouring line an empty tile has no line of its own near
    /// (I3). `bounds` is the monitor containing the hover, so a tile is
    /// clamped rather than reading a neighbouring monitor's pixels (I2).
    ///
    /// `collect` gates the scan overlay (M3 task 4). `false` is the hot path
    /// and returns an empty, never-allocated `Vec` with no `ScanRect` ever
    /// constructed - "off" means inert, not "collect and discard". `true`
    /// records pass 1's own box, every tile actually read - pushed from
    /// inside `tile_forward`'s reader closure, the only place a tile's real
    /// rectangle is known - and the resolved word's own anchor.
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

    /// One capture-and-recognise, keeping only the line under the hover.
    ///
    /// The tall tile `band_of` now produces can contain furigana as its own
    /// OCR line above or below the text it annotates. Flattening every line
    /// back into one word list would splice that ruby into the sentence, so
    /// `nearest_line` keeps only the line actually centred near
    /// `perpendicular_centre`, within `tolerance`, and drops the rest. See
    /// its doc comment for the axis convention and the distance bound.
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

//! Windows.Media.Ocr recognition and the OCR-backed `TextSource`.
//!
//! Windows-only. Its job is to turn pixels into plain `OcrLine` values in
//! virtual-desktop coordinates and hand them to `layout`, which does all the
//! actual reasoning.

use crate::geom::{PhysPoint, PhysRect};
use crate::text::capture::{capture_upscaled, cursor_position, init_dpi_awareness, UPSCALE};
use crate::text::layout::{map_from_upscaled, region_around, resolve, OcrLine, OcrWord, Resolved};
use crate::text::{TextSource, TextSpan};
use anyhow::{Context, Result};
use std::time::Duration;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Block on a WinRT async operation.
///
/// `.get()` was removed in windows 0.62, and `windows-future`'s `Async::join()`
/// is `pub` inside a private module that is only glob-imported at that crate's
/// root — so it is unreachable from any downstream crate, not merely
/// feature-gated. Polling `Status()` is the working pattern.
fn wait_blocking<T>(op: windows_future::IAsyncOperation<T>) -> windows::core::Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    loop {
        if op.Status()? != windows_future::AsyncStatus::Started {
            return op.GetResults();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Recognise Japanese in a tightly packed BGRA buffer.
///
/// Returned coordinates are in **upscaled-image** space — the caller maps them
/// back to the virtual desktop.
pub fn recognise(buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
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

    let lang = Language::CreateLanguage(&HSTRING::from("ja"))?;
    let engine = OcrEngine::TryCreateFromLanguage(&lang)
        .context("creating the Japanese OCR engine")?;
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

pub struct OcrTextSource;

impl OcrTextSource {
    /// Initialises the process for capture and WinRT, and proves the Japanese
    /// recogniser exists. Fails loudly here rather than on the first hover.
    pub fn new() -> Result<Self> {
        init_dpi_awareness()?;
        // WinRT activation fails with CO_E_NOTINITIALIZED without this.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED).context("RoInitialize")? };
        let lang = Language::CreateLanguage(&HSTRING::from("ja"))?;
        OcrEngine::TryCreateFromLanguage(&lang).context(
            "no Japanese OCR recogniser available - see \
             docs/superpowers/findings/2026-07-26-m0-ocr-availability.md",
        )?;
        Ok(OcrTextSource)
    }

    /// Full resolution, exposing the orientation as well as the span. `probe`
    /// and `watch` want the extra detail; `TextSource::at` does not.
    pub fn resolve_at(&self, cursor: PhysPoint) -> Result<Option<Resolved>> {
        let region = region_around(cursor);
        let (buf, w, h) = capture_upscaled(region)?;
        let raw = recognise(&buf, w, h)?;
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
        Ok(resolve(&lines, cursor))
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

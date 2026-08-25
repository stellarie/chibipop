//! Windows screen-text acquisition: the DXGI/GDI capture and WinRT OCR
//! backends behind core's `RegionCapture` and `OcrEngine` seams (ADR-0001).

pub mod capture;
pub mod ocr;

// Core owns the vocabulary and the layout/hit-scan logic (ADR-0001);
// re-exported so the modules above keep addressing them as `crate::text::…`,
// unchanged by the workspace split.
pub use chibipop::text::{layout, Frame, RegionCapture, SettingsSnapshot, TextSource, TextSpan};

use anyhow::Result;

/// The Windows `TextSource`, no capture guard: probe and watch. The app's
/// worker assembles its own parts, guard included.
pub fn text_source(settings: SettingsSnapshot, language: &str) -> Result<TextSource> {
    let cap = capture::WinCapture::new(None)?;
    let ocr = ocr::WinrtOcr::new(language)?;
    Ok(TextSource::new(Box::new(cap), Box::new(ocr), settings))
}

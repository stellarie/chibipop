//! Windows screen-text acquisition: the DXGI/GDI capture and WinRT OCR
//! backends behind core's `RegionCapture` and `OcrEngine` seams (ADR-0001).

pub mod capture;
pub mod ocr;

// Core owns the vocabulary and the layout/hit-scan logic (ADR-0001);
// re-exported so the modules above keep addressing them as `crate::text::…`,
// unchanged by the workspace split.
pub use chibipop::text::{
    layout, mask, Frame, RegionCapture, SettingsSnapshot, TextSource, TextSpan,
};

/// The Windows capture upscale: WinRT OCR misreads small text at
/// native resolution. A platform fact core no longer hardcodes
/// (ADR-0009 - the Linux engine supplies 1).
pub const UPSCALE: i32 = 2;

use anyhow::Result;

/// The Windows `TextSource`, no capture guard: probe and watch. The app's
/// worker assembles its own parts, guard included.
pub fn text_source(settings: SettingsSnapshot, language: &str) -> Result<TextSource> {
    let cap = capture::WinCapture::new(None)?;
    let ocr = ocr::WinrtOcr::new(language)?;
    Ok(TextSource::new(Box::new(cap), Box::new(ocr), settings))
}

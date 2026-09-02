//! Windows screen-text acquisition uses DXGI/GDI capture and WinRT OCR.
//! These backends implement core's `RegionCapture` and `OcrEngine` seams.
//! See `ARCHITECTURE.md#workspace-and-seams`.

pub mod capture;
pub mod ocr;

// Core owns the vocabulary and the layout and hit-scan logic.
// See `ARCHITECTURE.md#workspace-and-seams`.
// Re-export these items so the modules above keep the same
// `crate::text::…` path after the workspace split.
pub use chibipop::text::{
    layout, mask, Frame, RegionCapture, SettingsSnapshot, TextSource, TextSpan,
};

/// Windows uses this capture upscale because WinRT OCR misreads small text at
/// native resolution. Core does not hardcode this platform value.
/// See `ARCHITECTURE.md#ocr-engine`. The Linux engine supplies 1.
pub const UPSCALE: i32 = 2;

use anyhow::Result;

/// This function builds the Windows `TextSource` without a capture guard.
/// The `probe` and `watch` commands use it. The application `Worker` builds
/// its own parts and includes the guard.
pub fn text_source(settings: SettingsSnapshot, language: &str) -> Result<TextSource> {
    let cap = capture::WinCapture::new(None)?;
    let ocr = ocr::WinrtOcr::new(language)?;
    Ok(TextSource::new(Box::new(cap), Box::new(ocr), settings))
}

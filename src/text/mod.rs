//! This module defines the vocabulary for screen text.
//! It defines the `RegionCapture` and `OcrEngine` seams.
//! It also contains layout and hit-scan logic that joins these seams.
//! Platform code implements the seams with DXGI, GDI, WinRT OCR, or wlr-screencopy.
//! Each platform bin crate contains its implementation.
//! This module contains no platform code.

pub mod layout;
pub mod sentence;
pub mod mask;
mod frozen;
pub mod source;

pub use mask::{CaptureMask, CaptureMode};
pub use source::{RegionRead, SettingsSnapshot, TextSource};

use crate::geom::{PhysPoint, PhysRect};
use crate::text::layout::{OcrLine, TextGeom};
use anyhow::Result;

/// `TextSpan` stores text and the cursor position within that text.
#[derive(Debug)]
pub struct TextSpan {
    pub text: String,
    /// `cursor_byte_offset` stores the cursor byte offset.
    /// It always falls on a character boundary.
    pub cursor_byte_offset: usize,
    /// `anchor` stores the rect of the hovered character.
    pub anchor: PhysRect,
    /// `geom` stores word boxes. The list can be empty.
    pub geom: Vec<TextGeom>,
}

/// `Frame` stores pixels for one screen region and names the backend that produced them.
pub struct Frame {
    /// The format is BGRA8 and top-down, with no padding between rows.
    /// Each pixel uses 4 bytes, so the buffer holds `w * h * 4` bytes.
    /// The alpha channel has no meaningful value.
    pub buf: Vec<u8>,
    pub w: i32,
    pub h: i32,
    /// `source` names the backend path that produced the pixels.
    /// Logs and the `probe` tool use this name.
    pub source: &'static str,
    /// `fallback` records why the code did not use the preferred backend path.
    /// The field is empty when the code used the preferred path.
    pub fallback: Option<String>,
    /// `unchanged` is true when this grab returns the same pixels as the previous grab for the same region.
    ///
    /// This field only hints that the pipeline can reuse OCR.
    /// It never means that `buf` lacks data.
    /// The field `buf` always holds the real content of the region.
    /// A damage-paced backend, such as wlr-screencopy
    /// (ARCHITECTURE.md#capture-and-masking), already tracks damage.
    /// This backend can set the field at no extra cost.
    /// The pipeline can then reuse its existing OCR result.
    /// A backend that cannot tell must report `false`.
    /// This answer is always correct, but it costs one extra OCR pass.
    pub unchanged: bool,
}

/// This trait provides pixels for a screen region when the caller requests them.
///
/// **This trait uses a pull contract.** `grab` always returns the newest content that the backend has.
/// `grab` never waits for damage or a fresh frame.
/// The trait must answer for a still desktop as fast as for a busy desktop because hover latency matters most to the user.
/// Some backends use a push model.
/// A portal or PipeWire stream, for example, delivers a new frame only when the screen changes.
/// Such a backend must keep its newest buffer internally.
/// It must serve `grab` from that buffer.
/// This rule keeps the mismatch inside one unusual backend instead of every caller.
/// (ARCHITECTURE.md#workspace-and-seams).
///
/// The returned [`Frame`] holds exactly `region.w x region.h` pixels.
/// Code maps word geometry back to desktop coordinates with an offset from the region's origin.
/// A backend that cannot fill the exact requested box must fail the grab.
/// The backend must not return a frame of a different size.
///
/// Regions and results always use physical pixels.
/// The platform bin converts logical coordinates and the fractional scale before a value reaches this trait.
pub trait RegionCapture {
    /// Return the newest content of `region`.
    fn grab(&mut self, region: PhysRect) -> Result<Frame>;

    /// `bounds_containing` returns the bounds of the display that contains `p`.
    ///
    /// The layout code uses this bound to stop tile passes at the display edge.
    /// A backend that cannot find the exact bound must return an estimated bound instead of an error.
    /// A wrong bound wastes one tile.
    /// An error costs the whole hover.
    fn bounds_containing(&self, p: PhysPoint) -> PhysRect;

    /// `begin_read` starts one logical read.
    /// The read consists of `grab` calls.
    ///
    /// If a backend's own surfaces can appear in the pixels, the backend must hide them here.
    /// This hide step can block.
    /// A backend with an expensive session, such as a portal session, can open that session here.
    /// The backend then holds the session open for the whole read.
    /// Each call to `begin_read` must pair with one call to `end_read`.
    fn begin_read(&mut self) {}

    /// `end_read` closes the read that `begin_read` opened.
    fn end_read(&mut self) {}
}

/// An `OcrEngine` recognizes text and returns geometry for each word.
pub trait OcrEngine {
    /// `recognise` returns lines of words.
    /// Each word has a box in image pixels.
    ///
    /// The `bgra` argument holds `w * h * 4` bytes in the format of [`Frame`].
    /// Hit-scan resolves a point against these word boxes.
    /// If an engine cannot find a box for a word, it must drop that word.
    /// The engine must not invent a box.
    /// The engine must also drop a line that has no words.
    /// An empty result therefore means that the engine recognized nothing.
    fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>>;

    /// `set_language` changes the engine language when possible, for example on reload.
    ///
    /// If an engine cannot serve `tag`, it must keep its current language and report the failure on stderr.
    /// A hover in the wrong language is better than no hover at all.
    fn set_language(&mut self, tag: &str);

    /// `name` returns a stable engine id.
    /// The id is not a display name.
    ///
    /// A built-in engine names itself, for example `"windows-ocr"` or `"meiki-ocr"`.
    /// A plugin returns the name from its manifest.
    /// A log line or a `probe` report can name the exact engine that read the pixels, not only the platform.
    fn name(&self) -> &str;

    /// `provides_geometry` answers whether `recognise` returns a box for each word.
    ///
    /// Hit-scan needs these boxes to work.
    /// An engine that reads text without geometry must answer `false` here.
    /// The engine must not invent boxes.
    /// Both built-in engines box every word.
    /// A plugin answers with the claim in its manifest.
    /// The `plugin check` tool holds the plugin to that claim.
    fn provides_geometry(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TextOnly` reads text but cannot box it.
    /// It shows the honest shape of an engine without geometry.
    struct TextOnly;

    impl OcrEngine for TextOnly {
        fn recognise(&self, _bgra: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            Ok(Vec::new())
        }

        fn set_language(&mut self, _tag: &str) {}

        fn name(&self) -> &str {
            "text-only"
        }

        fn provides_geometry(&self) -> bool {
            false
        }
    }

    /// The metadata travels with the trait object that the pipeline holds.
    /// No caller needs to know the concrete engine behind it.
    #[test]
    fn a_geometry_less_engine_says_so_through_the_seam() {
        let engine: Box<dyn OcrEngine> = Box::new(TextOnly);
        assert!(engine.recognise(&[], 10, 10).unwrap().is_empty());
        assert_eq!("text-only", engine.name());
        assert!(!engine.provides_geometry());
    }
}

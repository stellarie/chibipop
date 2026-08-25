//! Screen text: shared vocabulary, the two acquisition seams, and the
//! layout/hit-scan logic that composes them. The seams' implementations
//! (DXGI/GDI, WinRT OCR, wlr-screencopy, ...) are platform code and live in
//! the bin crates; everything here is platform-neutral.

pub mod layout;
pub mod source;

pub use source::{RegionRead, SettingsSnapshot, TextSource};

use crate::geom::{PhysPoint, PhysRect};
use crate::text::layout::{OcrLine, TextGeom};
use anyhow::Result;

/// Text plus a cursor position.
#[derive(Debug)]
pub struct TextSpan {
    pub text: String,
    /// Char-boundary byte offset.
    pub cursor_byte_offset: usize,
    /// The hovered char's rect.
    pub anchor: PhysRect,
    /// Word boxes; may be empty.
    pub geom: Vec<TextGeom>,
}

/// Pixels of one screen region, and how they were obtained.
pub struct Frame {
    /// BGRA8, `w * h * 4` bytes, top-down, no row padding. Alpha is junk.
    pub buf: Vec<u8>,
    pub w: i32,
    pub h: i32,
    /// Which backend path produced them; for logs and `probe`.
    pub source: &'static str,
    /// Why the preferred path was not used, if it was not.
    pub fallback: Option<String>,
    /// These pixels are the ones the previous grab of *this same
    /// region* returned.
    ///
    /// An optimisation hint, never a data-absence marker: `buf` is
    /// always the region's real content. A damage-paced backend
    /// (wlr-screencopy, ADR-0002) learns this for free from the race
    /// it already runs, so the pipeline can reuse the OCR it already
    /// paid for; a backend that cannot tell says `false`, which is
    /// always correct and merely costs a pass.
    pub unchanged: bool,
}

/// Pixels of a screen region, on demand.
///
/// **Pull-shaped, by contract.** `grab` answers with the most recent content
/// the backend has and never blocks waiting for damage or for a fresh frame:
/// a still desktop must answer as fast as a busy one, because hover latency
/// is the product. A push-shaped backend - a portal/PipeWire stream that
/// only delivers on change - keeps its newest buffer internally and serves
/// `grab` out of that buffer, so one weird backend absorbs the mismatch
/// rather than every caller (ADR-0001).
///
/// The returned [`Frame`] is exactly `region.w x region.h` pixels: word
/// geometry is mapped back to the desktop by offsetting against the region's
/// origin, so a backend that cannot honour the requested box must fail the
/// grab rather than return a different one.
///
/// Regions and results are physical pixels throughout; logical coordinates
/// and fractional scale are the bin's business, converted before they reach
/// here.
pub trait RegionCapture {
    /// The most recent content of `region`.
    fn grab(&mut self, region: PhysRect) -> Result<Frame>;

    /// The display area containing `p`.
    ///
    /// Bounds multi-pass tiling, so one read never walks off the display the
    /// text is on. A backend that cannot say answers with something
    /// plausible rather than failing - a wrong bound costs a tile, an error
    /// costs the hover.
    fn bounds_containing(&self, p: PhysPoint) -> PhysRect;

    /// Opens a read: the several `grab`s that make up one logical scan.
    ///
    /// A backend whose own surfaces would land in the pixels hides them here
    /// and may block doing it; one with an expensive session may open it
    /// here and hold it for the read. Always paired with `end_read`.
    fn begin_read(&mut self) {}

    /// Closes the read `begin_read` opened.
    fn end_read(&mut self) {}
}

/// Recognised text, with geometry per word.
pub trait OcrEngine {
    /// Lines of words, each word carrying its box in image pixels.
    ///
    /// `bgra` is `w * h * 4` bytes in [`Frame`]'s format. Word boxes are what
    /// hit-scan resolves against, so a recogniser that cannot box a word must
    /// drop it rather than invent a rect. Lines with no words are dropped, so
    /// an empty result means "nothing recognised".
    fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>>;

    /// Best-effort language swap, for a reload.
    ///
    /// An engine that cannot serve `tag` keeps the language it has and says
    /// so on stderr: a hover in the wrong language still beats no hover.
    fn set_language(&mut self, tag: &str);
}

//! Text acquisition: getting Japanese off the screen and deciding which
//! character the cursor is on.

pub mod layout;
pub mod capture;
pub mod ocr;

use crate::geom::{PhysPoint, PhysRect};
use crate::text::layout::TextGeom;
use anyhow::Result;

/// A run of text with a position inside it.
#[derive(Debug)]
pub struct TextSpan {
    pub text: String,
    /// Byte offset into `text`, on a char boundary, of the first byte of the
    /// hovered character.
    pub cursor_byte_offset: usize,
    /// Where to anchor a popup — the hovered character's own rect.
    pub anchor: PhysRect,
    /// Where each of `text`'s characters sits on screen, in reading order and
    /// aligned to `text` character-for-character — what turns a match *length*
    /// into a rectangle (`layout::union_chars`).
    ///
    /// **May be empty**, and a caller must treat that as "no geometry known"
    /// rather than as a bug: the forward-tiling path stitches text from
    /// several captures and does not yet carry the boxes with it. An empty
    /// `geom` costs the match highlight, nothing else.
    pub geom: Vec<TextGeom>,
}

/// A way of obtaining text at a screen position. M2 provides an OCR-backed
/// implementation; M4 adds a UI Automation one in front of it.
pub trait TextSource {
    fn at(&self, p: PhysPoint) -> Result<Option<TextSpan>>;
}

//! Text acquisition: getting Japanese off the screen and deciding which
//! character the cursor is on.

pub mod layout;
pub mod capture;

use crate::geom::{PhysPoint, PhysRect};
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
}

/// A way of obtaining text at a screen position. M2 provides an OCR-backed
/// implementation; M4 adds a UI Automation one in front of it.
pub trait TextSource {
    fn at(&self, p: PhysPoint) -> Result<Option<TextSpan>>;
}

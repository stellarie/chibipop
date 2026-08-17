//! Screen text acquisition.

pub mod layout;
pub mod capture;
pub mod ocr;
pub mod provider;

use crate::geom::PhysRect;
use crate::text::layout::TextGeom;

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

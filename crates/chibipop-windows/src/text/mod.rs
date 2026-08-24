//! Windows screen-text acquisition: the DXGI/GDI capture and WinRT OCR
//! backends behind core's `TextSource`.

pub mod capture;
pub mod ocr;

// Core owns the vocabulary and the layout/hit-scan logic (ADR-0001);
// re-exported so the modules above keep addressing them as `crate::text::…`,
// unchanged by the workspace split.
pub use chibipop::text::{layout, TextSource, TextSpan};

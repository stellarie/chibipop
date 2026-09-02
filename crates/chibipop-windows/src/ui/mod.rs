//! The popup and its content: Win32 windows and Direct2D/DirectWrite paint output.

pub mod audit;
pub mod console;
pub mod editor;
pub mod media;
pub mod overlay;
pub mod render;
pub mod settings_window;
pub mod static_overlay;
pub mod tray;
pub mod window;

// `theme` and the popup `layout` are core vocabulary
// (ARCHITECTURE.md#workspace-and-seams). Re-export them so these modules
// keep the `crate::ui::…` paths after the workspace split.
// `css` styles that vocabulary, so keep it beside them.
pub use chibipop::ui::{css, layout, theme};

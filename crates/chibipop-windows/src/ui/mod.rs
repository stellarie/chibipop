//! The popup and its content: Win32 windows, Direct2D/DirectWrite painting.

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

// The theme and the popup's layout are core vocabulary (ADR-0001, ADR-0004);
// re-exported so the modules above keep addressing them as `crate::ui::…`,
// unchanged by the workspace split. `css` styles that vocabulary, so it
// lives beside them.
pub use chibipop::ui::{css, layout, theme};

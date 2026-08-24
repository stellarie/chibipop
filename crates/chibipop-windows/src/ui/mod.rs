//! The popup and its content: Win32 windows, Direct2D/DirectWrite painting.

pub mod console;
pub mod overlay;
pub mod render;
pub mod settings_window;
pub mod tray;
pub mod window;

// The theme is core vocabulary (ADR-0001); re-exported so the modules above
// keep addressing it as `crate::ui::theme`, unchanged by the workspace split.
pub use chibipop::ui::theme;

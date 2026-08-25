//! Channel-aware hotkey controls (ADR-0005): the UI never lies about
//! who owns the trigger binding.
//!
//! Today only the wlr-native channel exists — the compositor bind is
//! the truth and the config chord is advisory. Ticket 36's
//! GlobalShortcuts session will construct `Portal` and render the
//! rebind flow; the enum is shaped so that lands without reshaping the
//! view.

use super::snippets::{self, Compositor};

/// Who owns the trigger binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyChannel {
    /// The compositor bind invokes `chibipop ctl`; we can only show the
    /// snippet to paste.
    Native,
    /// The XDG GlobalShortcuts portal owns the binding and reports it.
    // Constructed by the portal session, ticket 36.
    #[allow(dead_code)]
    Portal { current_binding: Option<String> },
}

/// What the hotkey section renders. One control per channel; `view`
/// matches on this and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyControl {
    /// Copyable native-bind lines plus the advisory note.
    Snippet { text: String },
    /// The portal's configure/rebind flow (ticket 36).
    #[allow(dead_code)]
    Rebind { current: Option<String> },
}

impl HotkeyChannel {
    pub fn control(&self, compositor: Compositor, chord: &str) -> HotkeyControl {
        match self {
            HotkeyChannel::Native => {
                HotkeyControl::Snippet { text: snippets::bind_snippet(compositor, chord) }
            }
            HotkeyChannel::Portal { current_binding } => {
                HotkeyControl::Rebind { current: current_binding.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_channel_renders_the_snippet() {
        let control = HotkeyChannel::Native.control(Compositor::Hyprland, "ALT+F");
        let HotkeyControl::Snippet { text } = control else {
            panic!("native must render a snippet, got {control:?}");
        };
        assert!(text.contains("chibipop ctl trigger-down"));
    }

    #[test]
    fn portal_channel_renders_the_rebind_flow() {
        let channel = HotkeyChannel::Portal { current_binding: Some("ALT+F".into()) };
        assert_eq!(
            channel.control(Compositor::Hyprland, "ALT+F"),
            HotkeyControl::Rebind { current: Some("ALT+F".into()) }
        );
    }
}

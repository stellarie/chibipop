//! Channel-aware hotkey controls (ADR-0005): the UI never lies about
//! who owns the trigger binding.
//!
//! Both rungs of ADR-0003's trigger ladder are real here. On the native
//! rung the compositor bind is the truth and the config chord is
//! advisory, so the only honest control is the snippet to paste. On the
//! portal rung the GlobalShortcuts session owns the binding and reports
//! it, so the control names the key the portal gave and points at the
//! desktop's own editor — the daemon publishes which of the two it
//! resolved (`shortcuts::state`), because a bus probe cannot tell them
//! apart.

use super::snippets::{self, Compositor};
use std::path::Path;

/// Who owns the trigger binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyChannel {
    /// The compositor bind invokes `chibipop ctl`; we can only show the
    /// snippet to paste.
    Native,
    /// The XDG GlobalShortcuts portal owns the binding and reports it.
    /// `current_binding` is the portal's own `trigger_description`, and
    /// `None` where the implementation reports no key (xdph does not:
    /// on Hyprland the key lives in the compositor's config).
    Portal { current_binding: Option<String> },
}

/// What the hotkey section renders. One control per channel; `view`
/// matches on this and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyControl {
    /// Copyable native-bind lines plus the advisory note.
    Snippet { text: String },
    /// The portal's binding, and where the user changes it.
    Rebind { current: Option<String> },
}

impl HotkeyChannel {
    /// `exe` is the binary a pasted bind must exec, resolved by the
    /// caller (`paths::exec_name`) rather than assumed to be on PATH
    /// (ticket 51).
    pub fn control(&self, compositor: Compositor, chord: &str, exe: &Path) -> HotkeyControl {
        match self {
            HotkeyChannel::Native => {
                HotkeyControl::Snippet { text: snippets::bind_snippet(compositor, chord, exe) }
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

    /// The snippet must name the running binary, not the bare command:
    /// under `cargo run` there is no `chibipop` on PATH to exec.
    #[test]
    fn native_channel_renders_the_snippet_for_the_running_binary() {
        let exe = Path::new("/home/u/chibipop/target/debug/chibipop");
        let control = HotkeyChannel::Native.control(Compositor::Hyprland, "ALT+F", exe);
        let HotkeyControl::Snippet { text } = control else {
            panic!("native must render a snippet, got {control:?}");
        };
        assert!(text.contains("/home/u/chibipop/target/debug/chibipop ctl trigger-down"), "{text}");
        assert!(!text.contains(", chibipop ctl"), "the bare command name must not survive: {text}");
    }

    #[test]
    fn portal_channel_renders_the_rebind_flow() {
        let channel = HotkeyChannel::Portal { current_binding: Some("ALT+F".into()) };
        assert_eq!(
            channel.control(Compositor::Hyprland, "ALT+F", Path::new("chibipop")),
            HotkeyControl::Rebind { current: Some("ALT+F".into()) }
        );
    }
}

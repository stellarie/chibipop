//! Channel-aware hotkey controls
//! (ARCHITECTURE.md#settings-and-config).
//! The interface shows the true owner of each global binding.
//!
//! Both rungs of the trigger ladder (ARCHITECTURE.md#input-ladders) operate here.
//! On the native rung, the compositor binding is the authority and the configuration
//! chord is advisory. The control shows the snippet to copy. On the portal rung,
//! the GlobalShortcuts session owns and reports the binding. The control shows
//! the key from the portal and points to the desktop editor. The daemon publishes
//! the resolved channel (`shortcuts::state`), because bus inspection cannot
//! differentiate the two rungs.
//!
//! The same two rungs serve every global action. Each action has a control-socket
//! verb. One row structure serves all actions. The native rung selects the
//! corresponding [`Bind`] variant.

use super::snippets::{self, Bind, Compositor};
use std::path::Path;

/// The owner of the trigger binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyChannel {
    /// The compositor binding executes `chibipop ctl`. The control displays
    /// the snippet to copy.
    Native,
    /// The XDG GlobalShortcuts portal owns and reports the binding.
    /// `current_binding` contains the portal description for this action
    /// (`shortcuts::state::Published::description`). The field is `None` when
    /// the backend reports no key or when the portal did not register the identifier.
    Portal { current_binding: Option<String> },
}

/// The rendered hotkey control. The `view` function matches on this enum.
/// Each channel provides one control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyControl {
    /// Copyable native binding lines and the advisory note.
    Snippet { text: String },
    /// The portal binding and the change target for the user.
    Rebind { current: Option<String> },
    /// The chord field is empty, so no binding exists.
    /// A snippet for an empty chord is invalid syntax. The row displays this state.
    NoChord,
}

impl HotkeyChannel {
    /// Return the control for one chord row. `bind` is the configuration for
    /// the native rung: a press and release pair for the trigger, or one press
    /// for an action.
    ///
    /// `exe` is the binary path that the binding executes. The caller resolves
    /// this path with `paths::exec_name`.
    pub fn control(
        &self,
        compositor: Compositor,
        chord: &str,
        exe: &Path,
        bind: Bind,
    ) -> HotkeyControl {
        if chord.trim().is_empty() {
            return HotkeyControl::NoChord;
        }
        match self {
            HotkeyChannel::Native => HotkeyControl::Snippet {
                text: snippets::bind_snippet(compositor, chord, exe, bind),
            },
            // `current_binding` is the published key for this action.
            // The channel resolves for each portal identifier (`hotkey_channel`).
            // The row shows only its assigned key.
            HotkeyChannel::Portal { current_binding } => {
                HotkeyControl::Rebind { current: current_binding.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The snippet must contain the running binary path instead of a bare name.
    /// Under `cargo run`, the binary is not in PATH.
    #[test]
    fn native_channel_renders_the_snippet_for_the_running_binary() {
        let exe = Path::new("/home/u/chibipop/target/debug/chibipop");
        let control = HotkeyChannel::Native.control(Compositor::Hyprland, "ALT+F", exe, Bind::Hold);
        let HotkeyControl::Snippet { text } = control else {
            panic!("native must render a snippet, got {control:?}");
        };
        assert!(text.contains("/home/u/chibipop/target/debug/chibipop ctl trigger-down"), "{text}");
        assert!(!text.contains(", chibipop ctl"), "the bare command name must not survive: {text}");
    }

    /// The add-card row uses the same structure. On the native rung, this snippet
    /// is the only method to bind the chord (ARCHITECTURE.md#input-ladders, rung 2).
    #[test]
    fn native_channel_renders_a_press_snippet_for_a_one_shot_action() {
        let control = HotkeyChannel::Native.control(
            Compositor::Sway,
            "ALT+A",
            Path::new("/opt/cp/chibipop"),
            Bind::Press(crate::control::Verb::AnkiAdd),
        );
        let HotkeyControl::Snippet { text } = control else {
            panic!("native must render a snippet, got {control:?}");
        };
        assert!(text.contains("bindsym --no-repeat ALT+A exec /opt/cp/chibipop ctl anki-add"), "{text}");
        assert!(!text.contains("--release"), "one press, one verb: {text}");
    }

    #[test]
    fn portal_channel_renders_the_rebind_flow() {
        let channel = HotkeyChannel::Portal { current_binding: Some("ALT+F".into()) };
        assert_eq!(
            channel.control(Compositor::Hyprland, "ALT+F", Path::new("chibipop"), Bind::Hold),
            HotkeyControl::Rebind { current: Some("ALT+F".into()) }
        );
    }

    /// Each row receives the channel for its portal identifier.
    /// The add-card row shows the add-card key. An unregistered identifier
    /// shows no key. The type prevents using keys from other actions.
    #[test]
    fn a_one_shot_row_names_the_key_published_for_its_own_action() {
        let add = Bind::Press(crate::control::Verb::AnkiAdd);
        let bound = HotkeyChannel::Portal { current_binding: Some("ALT+A".into()) };
        assert_eq!(
            HotkeyControl::Rebind { current: Some("ALT+A".into()) },
            bound.control(Compositor::Hyprland, "ALT+A", Path::new("chibipop"), add),
        );
        let unnamed = HotkeyChannel::Portal { current_binding: None };
        assert_eq!(
            HotkeyControl::Rebind { current: None },
            unnamed.control(Compositor::Hyprland, "ALT+A", Path::new("chibipop"), add),
        );
    }

    /// An empty chord provides no binding on either rung.
    /// Whitespace strings also count as empty.
    #[test]
    fn a_blank_chord_offers_no_bind_on_either_channel() {
        let add = Bind::Press(crate::control::Verb::AnkiAdd);
        for channel in [
            HotkeyChannel::Native,
            HotkeyChannel::Portal { current_binding: Some("ALT+F".into()) },
        ] {
            for chord in ["", "   "] {
                assert_eq!(
                    HotkeyControl::NoChord,
                    channel.control(Compositor::Hyprland, chord, Path::new("chibipop"), add),
                    "{channel:?} / {chord:?}"
                );
            }
        }
    }
}

//! Channel-aware hotkey controls
//! (ARCHITECTURE.md#settings-and-config).
//! The interface shows the true owner of each global binding.
//!
//! Both rungs of the trigger ladder (ARCHITECTURE.md#input-ladders) operate here.
//! On the native rung, the compositor binding is the authority and the configuration
//! chord is advisory. The control shows the snippet to copy. On the portal rung,
//! the GlobalShortcuts session owns and reports the portal shortcut. The control reports
//! the portal key. The settings window then gives the correct desktop-specific
//! change path. The daemon publishes the resolved channel (`shortcuts::state`)
//! because bus inspection cannot distinguish the two rungs.
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
    /// The compositor cannot load this bind or preserve its trigger mode.
    Unsupported { reason: String },
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
        bind: Bind<'_>,
    ) -> HotkeyControl {
        if chord.trim().is_empty() {
            return HotkeyControl::NoChord;
        }
        if let HotkeyChannel::Portal { current_binding } = self {
            return HotkeyControl::Rebind { current: current_binding.clone() };
        }
        let text = snippets::bind_snippet(compositor, chord, exe, bind);
        if compositor.supports_bind(chord, bind) {
            HotkeyControl::Snippet { text }
        } else {
            HotkeyControl::Unsupported { reason: text }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::config::TriggerMode;


    #[test]
    fn niri_does_not_offer_a_copy_button_for_unsupported_modifiers() {
        let control = HotkeyChannel::Native.control(
            Compositor::Niri,
            "CAPS+F2",
            Path::new("chibipop"),
            Bind { id: "action", mode: TriggerMode::Press },
        );
        assert!(matches!(control, HotkeyControl::Unsupported { .. }), "{control:?}");
    }


    /// An empty chord provides no binding on either rung.
    /// Whitespace strings also count as empty.
    #[test]
    fn a_blank_chord_offers_no_bind_on_either_channel() {
        let add = Bind { id: "action", mode: TriggerMode::Press };
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

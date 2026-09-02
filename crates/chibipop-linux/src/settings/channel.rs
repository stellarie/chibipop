//! Channel-aware hotkey controls
//! (ARCHITECTURE.md#settings-and-config): the UI never lies about who
//! owns a global binding.
//!
//! Both rungs of the trigger ladder (ARCHITECTURE.md#input-ladders) are
//! real here. On the native rung the compositor bind is the truth and
//! the config chord is advisory, so the only honest control is the
//! snippet to paste. On the portal rung the GlobalShortcuts session
//! owns the binding and reports it, so the control names the key the
//! portal gave and points at the desktop's own editor — the daemon
//! publishes which of the two it resolved (`shortcuts::state`), because
//! a bus probe cannot tell them apart.
//!
//! The same two rungs answer for every global action, not just the
//! trigger: since the 2026-08-26 addendum each one has its own
//! control-socket verb, so one row shape serves them all and the only
//! difference is which [`Bind`] the native rung pastes.

use super::snippets::{self, Bind, Compositor};
use std::path::Path;

/// Who owns the trigger binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyChannel {
    /// The compositor bind invokes `chibipop ctl`; we can only show the
    /// snippet to paste.
    Native,
    /// The XDG GlobalShortcuts portal owns the binding and reports it.
    /// `current_binding` is the portal's own `trigger_description` for
    /// *this row's* action (`shortcuts::state::Published::description`),
    /// and `None` where the implementation reports no key (xdph does
    /// not: on Hyprland the key lives in the compositor's config) or
    /// where the portal never answered for that id at all.
    Portal { current_binding: Option<String> },
}

/// What a hotkey row renders. One control per channel; `view` matches on
/// this and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyControl {
    /// Copyable native-bind lines plus the advisory note.
    Snippet { text: String },
    /// The portal's binding, and where the user changes it.
    Rebind { current: Option<String> },
    /// The chord field is blank, so there is no bind to build: a
    /// snippet for it would be a syntactically invalid `bind = , F, …`
    /// line, which is exactly the kind of thing this window must never
    /// hand a user. The row says so instead.
    NoChord,
}

impl HotkeyChannel {
    /// The control for one chord row. `bind` is the shape the native
    /// rung would paste — the trigger's press/release pair, or one
    /// press of one verb.
    ///
    /// `exe` is the binary a pasted bind must exec, resolved by the
    /// caller (`paths::exec_name`) rather than assumed to be on PATH
    /// (ticket 51).
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
            // `current_binding` is this action's own published key: a
            // channel is resolved per portal id (`hotkey_channel`), so
            // a row can only ever name the key it was told. Borrowing
            // another row's key is the one thing this window must never
            // do, and the type is what prevents it.
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
        let control = HotkeyChannel::Native.control(Compositor::Hyprland, "ALT+F", exe, Bind::Hold);
        let HotkeyControl::Snippet { text } = control else {
            panic!("native must render a snippet, got {control:?}");
        };
        assert!(text.contains("/home/u/chibipop/target/debug/chibipop ctl trigger-down"), "{text}");
        assert!(!text.contains(", chibipop ctl"), "the bare command name must not survive: {text}");
    }

    /// The add-card row is the same row: on the native rung it is the
    /// only way that chord can be bound at all
    /// (ARCHITECTURE.md#input-ladders, rung 2).
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

    /// Each row is handed the channel resolved for its *own* portal id
    /// (ticket 09), so the add-card row names the add-card key and an
    /// id the portal never answered for names nothing at all. The row
    /// shape cannot borrow another action's key because it never sees
    /// one.
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

    /// An unset chord has no bind on either rung. Whitespace is unset
    /// too: a text entry a user cleared often keeps a space.
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

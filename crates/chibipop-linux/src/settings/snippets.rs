//! This module provides copyable compositor snippets
//! (ARCHITECTURE.md#settings-and-config, ARCHITECTURE.md#capture-and-masking).
//!
//! On the wlr-native channel, the compositor bind is authoritative.
//! The settings window does not claim the trigger.
//! It shows bind lines that call `chibipop ctl` and provides a copy button.
//! Capture exclusion uses the same approach.
//! No Wayland client can hide its surface from third-party capture.
//! The window offers the compositor rule when one exists.
//! It states when no rule exists.

use crate::control::Verb;
use crate::paths;
use chibipop::config::TriggerMode;
use std::path::Path;

/// Identify the compositor family that the snippets target.
/// Detection uses environment values and can choose the wrong family.
/// A wrong choice still produces a valid snippet for *some* compositor.
/// Some generated snippets include a comment that names the syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Hyprland,
    Sway,
    Kde,
    Other,
}

impl Compositor {
    pub fn detect() -> Compositor {
        classify(
            std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
            std::env::var_os("SWAYSOCK").is_some(),
            std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        )
    }
}

/// Classify the compositor from supplied signals.
/// The function stays pure so tests can supply those signals.
pub fn classify(hyprland: bool, sway: bool, desktop: Option<&str>) -> Compositor {
    if hyprland {
        Compositor::Hyprland
    } else if sway {
        Compositor::Sway
    } else if desktop.is_some_and(|d| d.to_ascii_lowercase().contains("kde")) {
        Compositor::Kde
    } else {
        Compositor::Other
    }
}

/// Split a chord into the Hyprland and Sway forms.
///
/// `trigger_key_linux` holds the XDG GlobalShortcuts preferred-binding
/// syntax (`ALT+F`). The native snippet spells it for each compositor.
fn split_chord(chord: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = chord.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let key = parts.pop().unwrap_or("F");
    (parts, key)
}

/// Select the bind shape that a chord needs.
///
/// The caller does not build the verb text.
/// [`Verb::as_str`] supplies it, so a verb rename cannot leave a snippet
/// with a word that the socket no longer accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    /// The trigger. A press sends `trigger-down`, and a release sends `trigger-up`.
    /// The pair carries the Hyprland release caveat.
    Hold,
    /// A one-shot global action. One press sends one verb and no release line.
    Press(Verb),
}

/// Select the native bind verb for a trigger mode.
///
/// The mode picks which verb the native bind sends.
/// Toggle mode gets a one-line press bind with no release line, so the Hyprland
/// modifier-first release defect already documented on [`Bind::Hold`] cannot wedge it.
/// Press mode also gets one press bind, and it sends `lookup` for one lookup per key press.
pub fn trigger_bind(mode: TriggerMode) -> Bind {
    match mode {
        TriggerMode::Toggle => Bind::Press(Verb::Toggle),
        TriggerMode::Press => Bind::Press(Verb::Lookup),
        _ => Bind::Hold,
    }
}

/// Build the native-bind snippet for one chord.
/// The snippet contains exactly the verbs that the control socket accepts.
///
/// The caller resolves `exe` with `paths::exec_name`.
/// This function does not look up `exe`.
/// A pasted bind must execute the daemon that the user runs.
/// Under `cargo run`, that daemon is `target/debug/chibipop` and is not on PATH.
/// The external lookup keeps this function pure.
pub fn bind_snippet(compositor: Compositor, chord: &str, exe: &Path, bind: Bind) -> String {
    let (mods, key) = split_chord(chord);
    let exe = paths::shell_quote(exe);
    match compositor {
        Compositor::Hyprland => {
            let mask = mods.join(" ");
            match bind {
                // Keep these caveat lines in the snippet.
                // Hyprland (≤ 0.55.4, verified in source and live) can fire no release bind
                // when a chord modifier goes up before its key.
                // Release checks require the bind's mod mask to remain active at release.
                // KeybindManager.cpp calls this condition "Gate A".
                // When the user presses another key during the hold, that key shadows a
                // modifier-keyed `bindr` (hyprwm/Hyprland#5032, #7675).
                // We tried and measured every alternative:
                // a modifier `bindr`/`bindir`, an empty-mask `bindr` on the key,
                // and a submap-scoped `bindri`, which wedges the whole keymap.
                // The code ships the pair unchanged.
                // The snippet states one habit and one recovery:
                // release the key before the modifier, then repeat the chord if needed.
                // The GlobalShortcuts `global` dispatcher is immune by design.
                // This supports the portal rung as rung 1
                // (ARCHITECTURE.md#input-ladders).
                Bind::Hold => format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {down}\n\
                     bindr = {mask}, {key}, exec, {exe} ctl {up}\n\
                     # Release {key} before {mask} - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
                     # If the popup sticks, tap the chord again (release {key} first), or bind `ctl toggle` instead.",
                    down = Verb::TriggerDown.as_str(),
                    up = Verb::TriggerUp.as_str(),
                ),
                // This bind has no release line.
                // A lost release cannot wedge a press-only bind.
                Bind::Press(verb) => format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {verb}",
                    verb = verb.as_str(),
                ),
            }
        }
        _ => {
            // Use Sway syntax.
            // Every other wlr compositor documents equivalent syntax.
            // The generated comment names this dialect.
            let chord = mods
                .iter()
                .chain(std::iter::once(&key))
                .cloned()
                .collect::<Vec<_>>()
                .join("+");
            match bind {
                Bind::Hold => format!(
                    "# sway syntax - adapt to your compositor\n\
                     bindsym --no-repeat {chord} exec {exe} ctl {down}\n\
                     bindsym --release {chord} exec {exe} ctl {up}",
                    down = Verb::TriggerDown.as_str(),
                    up = Verb::TriggerUp.as_str(),
                ),
                Bind::Press(verb) => format!(
                    "# sway syntax - adapt to your compositor\n\
                     bindsym --no-repeat {chord} exec {exe} ctl {verb}",
                    verb = verb.as_str(),
                ),
            }
        }
    }
}

/// Provide screen-share exclusion guidance
/// (ARCHITECTURE.md#capture-and-masking).
/// Hyprland gets a copyable rule.
/// KDE gets a manual instruction.
/// Other compositors get an honest "not available" message.
/// `None` means that no text exists to copy.
pub fn capture_rule(compositor: Compositor) -> (String, Option<String>) {
    match compositor {
        Compositor::Hyprland => (
            "Hide the popup from screen sharing (hyprland.conf):".to_string(),
            Some("layerrule = no_screen_share, chibipop".to_string()),
        ),
        Compositor::Kde => (
            "KDE: right-click the popup's entry in the screen-share picker \
             and enable \"Hide from Screen Sharing\" - there is no config \
             snippet."
                .to_string(),
            None,
        ),
        Compositor::Sway | Compositor::Other => (
            "Hiding the popup from screen sharing is not available on this \
             compositor."
                .to_string(),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A development build uses this path, which is not on PATH.
    /// The path is a bare word, so the snippet must show it verbatim.
    const DEV_EXE: &str = "/home/u/chibipop/target/debug/chibipop";

    #[test]
    fn hyprland_bind_for_the_default_chord() {
        let snippet = bind_snippet(Compositor::Hyprland, "ALT+F", Path::new(DEV_EXE), Bind::Hold);
        assert_eq!(
            snippet,
            "bind = ALT, F, exec, /home/u/chibipop/target/debug/chibipop ctl trigger-down\n\
             bindr = ALT, F, exec, /home/u/chibipop/target/debug/chibipop ctl trigger-up\n\
             # Release F before ALT - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
             # If the popup sticks, tap the chord again (release F first), or bind `ctl toggle` instead."
        );
    }

    /// The wedge caveat names the user's chord, not the default chord.
    /// A CTRL+SHIFT+K user must release K first.
    #[test]
    fn hyprland_bind_for_a_two_modifier_chord() {
        let snippet =
            bind_snippet(Compositor::Hyprland, "CTRL+SHIFT+K", Path::new("chibipop"), Bind::Hold);
        assert_eq!(
            snippet,
            "bind = CTRL SHIFT, K, exec, chibipop ctl trigger-down\n\
             bindr = CTRL SHIFT, K, exec, chibipop ctl trigger-up\n\
             # Release K before CTRL SHIFT - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
             # If the popup sticks, tap the chord again (release K first), or bind `ctl toggle` instead."
        );
    }

    /// A one-shot action uses one line.
    /// It has no release bind, so it needs no hold-release caveat.
    #[test]
    fn hyprland_press_bind_is_one_line_with_no_release_caveat() {
        let snippet = bind_snippet(
            Compositor::Hyprland,
            "ALT+A",
            Path::new(DEV_EXE),
            Bind::Press(Verb::AnkiAdd),
        );
        assert_eq!(
            snippet,
            "bind = ALT, A, exec, /home/u/chibipop/target/debug/chibipop ctl anki-add"
        );
    }

    #[test]
    fn trigger_modes_select_toggle_or_hold_bind_shape() {
        for compositor in [Compositor::Hyprland, Compositor::Sway] {
            let toggle =
                bind_snippet(compositor, "ALT+F", Path::new(DEV_EXE), trigger_bind(TriggerMode::Toggle));
            assert!(toggle.ends_with("ctl toggle"), "{toggle}");
            assert!(!toggle.contains("trigger-up"), "{toggle}");

            let press =
                bind_snippet(compositor, "ALT+F", Path::new(DEV_EXE), trigger_bind(TriggerMode::Press));
            assert!(press.ends_with("ctl lookup"), "{press}");
            assert!(!press.contains("trigger-up"), "{press}");

            for mode in [TriggerMode::Live, TriggerMode::HoldKey, TriggerMode::HoldShift] {
                let hold =
                    bind_snippet(compositor, "ALT+F", Path::new(DEV_EXE), trigger_bind(mode));
                assert!(hold.contains("trigger-down"), "{hold}");
                assert!(hold.contains("trigger-up"), "{hold}");
            }
        }
    }

    #[test]
    fn sway_bind_keeps_the_chord_spelling() {
        let snippet = bind_snippet(Compositor::Sway, "ALT+F", Path::new(DEV_EXE), Bind::Hold);
        assert!(snippet.contains(&format!(
            "bindsym --no-repeat ALT+F exec {DEV_EXE} ctl trigger-down"
        )));
        assert!(
            snippet.contains(&format!("bindsym --release ALT+F exec {DEV_EXE} ctl trigger-up"))
        );
    }

    #[test]
    fn sway_press_bind_is_the_labelled_dialect_with_no_release_line() {
        let snippet = bind_snippet(
            Compositor::Sway,
            "ALT+A",
            Path::new(DEV_EXE),
            Bind::Press(Verb::AnkiAdd),
        );
        assert_eq!(
            snippet,
            format!(
                "# sway syntax - adapt to your compositor\n\
                 bindsym --no-repeat ALT+A exec {DEV_EXE} ctl anki-add"
            )
        );
        assert!(!snippet.contains("--release"), "a press bind has no release line: {snippet}");
    }

    /// The snippet must name a verb that the socket accepts.
    /// It must not use a string that the caller builds.
    /// A verb rename must change the snippet.
    #[test]
    fn every_press_bind_names_the_verbs_own_wire_word() {
        for verb in crate::control::VERBS {
            for compositor in [Compositor::Hyprland, Compositor::Sway] {
                let snippet =
                    bind_snippet(compositor, "ALT+A", Path::new("chibipop"), Bind::Press(verb));
                assert!(
                    snippet.ends_with(&format!("chibipop ctl {}", verb.as_str())),
                    "{snippet}"
                );
            }
        }
        let hold = bind_snippet(Compositor::Hyprland, "ALT+F", Path::new("chibipop"), Bind::Hold);
        assert!(hold.contains(&format!("ctl {}", Verb::TriggerDown.as_str())), "{hold}");
        assert!(hold.contains(&format!("ctl {}", Verb::TriggerUp.as_str())), "{hold}");
    }

    #[test]
    fn unknown_compositor_gets_the_labelled_sway_dialect() {
        assert!(bind_snippet(Compositor::Other, "ALT+F", Path::new(DEV_EXE), Bind::Hold)
            .starts_with("# sway syntax"));
        assert!(bind_snippet(
            Compositor::Other,
            "ALT+A",
            Path::new(DEV_EXE),
            Bind::Press(Verb::AnkiAdd)
        )
        .starts_with("# sway syntax"));
    }

    /// Paths can contain spaces, for example `~/My Builds/...` or a user-named
    /// checkout directory.
    /// An unquoted path can execute the wrong word.
    /// Both dialects must quote such a path.
    #[test]
    fn a_path_with_a_space_is_quoted_for_both_dialects() {
        let exe = Path::new("/home/u/my builds/chibipop");
        let hypr = bind_snippet(Compositor::Hyprland, "ALT+F", exe, Bind::Hold);
        assert!(
            hypr.contains("bind = ALT, F, exec, '/home/u/my builds/chibipop' ctl trigger-down"),
            "{hypr}"
        );
        assert!(
            hypr.contains("bindr = ALT, F, exec, '/home/u/my builds/chibipop' ctl trigger-up"),
            "{hypr}"
        );
        let sway = bind_snippet(Compositor::Sway, "ALT+F", exe, Bind::Hold);
        assert!(
            sway.contains(
                "bindsym --no-repeat ALT+F exec '/home/u/my builds/chibipop' ctl trigger-down"
            ),
            "{sway}"
        );
        assert!(
            sway.contains(
                "bindsym --release ALT+F exec '/home/u/my builds/chibipop' ctl trigger-up"
            ),
            "{sway}"
        );
        let press = bind_snippet(Compositor::Hyprland, "ALT+A", exe, Bind::Press(Verb::AnkiAdd));
        assert_eq!(
            "bind = ALT, A, exec, '/home/u/my builds/chibipop' ctl anki-add",
            press
        );
    }

    #[test]
    fn capture_rule_is_copyable_only_on_hyprland() {
        let (_, rule) = capture_rule(Compositor::Hyprland);
        assert_eq!(rule.as_deref(), Some("layerrule = no_screen_share, chibipop"));
        assert_eq!(capture_rule(Compositor::Kde).1, None);
        assert_eq!(capture_rule(Compositor::Other).1, None);
    }

    #[test]
    fn detection_prefers_the_specific_signals() {
        assert_eq!(classify(true, true, Some("KDE")), Compositor::Hyprland);
        assert_eq!(classify(false, true, None), Compositor::Sway);
        assert_eq!(classify(false, false, Some("KDE")), Compositor::Kde);
        assert_eq!(classify(false, false, Some("GNOME")), Compositor::Other);
        assert_eq!(classify(false, false, None), Compositor::Other);
    }
}

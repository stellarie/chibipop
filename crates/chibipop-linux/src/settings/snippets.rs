//! Copyable compositor snippets (ADR-0005/0008).
//!
//! On the wlr-native channel the compositor bind is the truth, so the
//! settings window never pretends to own the trigger: it shows the bind
//! lines that shell out to `chibipop ctl`, with a copy button. Capture
//! exclusion is the same shape — no Wayland client can hide its surface
//! from third-party capture, so the window offers the compositor's own
//! rule where one exists and says so where none does.

use crate::control::Verb;
use crate::paths;
use std::path::Path;

/// Which compositor family the snippets target. Detection is
/// best-effort env sniffing: a wrong guess still yields a valid snippet
/// for *some* compositor, clearly labelled.
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

/// The pure decision, injectable for tests.
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

/// A chord split into its Hyprland/sway halves.
///
/// `trigger_key_linux` holds XDG GlobalShortcuts preferred-binding
/// syntax (`ALT+F`); the native snippet re-spells it per compositor.
fn split_chord(chord: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = chord.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let key = parts.pop().unwrap_or("F");
    (parts, key)
}

/// Which bind shape a chord needs.
///
/// The verb text is never a caller-built string: it comes from
/// [`Verb::as_str`], so renaming a verb cannot leave a snippet naming a
/// word the socket no longer answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    /// The trigger: press runs `trigger-down`, release `trigger-up`, and
    /// the pair carries the Hyprland release caveat.
    Hold,
    /// A one-shot global action: one press, one verb, no release line.
    Press(Verb),
}

/// The native-bind snippet for one chord: exactly the verbs the control
/// socket speaks.
///
/// `exe` is the binary to name, resolved by the caller
/// (`paths::exec_name`) and never looked up here: a pasted bind must
/// exec the daemon the user is actually running, which under
/// `cargo run` is `target/debug/chibipop` and is not on PATH
/// (ticket 51). Keeping the lookup outside keeps this function pure.
pub fn bind_snippet(compositor: Compositor, chord: &str, exe: &Path, bind: Bind) -> String {
    let (mods, key) = split_chord(chord);
    let exe = paths::shell_quote(exe);
    match compositor {
        Compositor::Hyprland => {
            let mask = mods.join(" ");
            match bind {
                // The caveat lines are part of the snippet on purpose.
                // Hyprland (≤ 0.55.4, verified in source and live) can
                // fire NO release bind when a chord's modifier goes up
                // before its key: release matching requires the bind's
                // mod mask to hold at the release instant
                // (KeybindManager.cpp "Gate A"), and a modifier-keyed
                // `bindr` is shadowed the moment another key is pressed
                // during the hold (hyprwm/Hyprland#5032, #7675). Every
                // rescue was tried and measured — a modifier
                // `bindr`/`bindir`, an empty-mask `bindr` on the key,
                // and a submap-scoped `bindri` (which wedges the whole
                // keymap) — see ticket 53. So the pair is shipped as-is
                // and the user is told the one habit and the one
                // recovery that exist. The GlobalShortcuts `global`
                // dispatcher is immune by design, which is one more
                // reason the portal rung is rung 1 (ADR-0003).
                Bind::Hold => format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {down}\n\
                     bindr = {mask}, {key}, exec, {exe} ctl {up}\n\
                     # Release {key} before {mask} - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
                     # If the popup sticks, tap the chord again (release {key} first), or bind `ctl toggle` instead.",
                    down = Verb::TriggerDown.as_str(),
                    up = Verb::TriggerUp.as_str(),
                ),
                // No release line, so none of the above applies: a
                // press-only bind cannot be wedged by a lost release.
                Bind::Press(verb) => format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {verb}",
                    verb = verb.as_str(),
                ),
            }
        }
        _ => {
            // sway syntax; every other wlr compositor documents an
            // equivalent, and the comment says which dialect this is.
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

/// Hide-from-screen-share, per ADR-0008: a copyable rule on Hyprland, a
/// pointer on KDE, the honest "not available" elsewhere. `None` means
/// there is nothing to copy.
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

    /// The path a real dev build hands out: not on PATH, and a bare
    /// word, so it must appear verbatim.
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

    /// The wedge caveat names the user's own chord, not the default's:
    /// a CTRL+SHIFT+K user must be told to release K first.
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

    /// A one-shot action is one line: no release bind, and therefore
    /// none of the release caveat the hold has to carry.
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

    /// The snippet must name the verb the socket answers, not a string
    /// the caller composed: a renamed verb has to change the snippet.
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

    /// A path with a space is the everyday case (`~/My Builds/...`, and
    /// any checkout under a directory a human named), and an unquoted
    /// one silently execs the wrong word. Both dialects must survive it.
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

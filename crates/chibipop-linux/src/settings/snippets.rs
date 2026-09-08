//! This module provides copyable compositor snippets
//! (ARCHITECTURE.md#settings-and-config, ARCHITECTURE.md#capture-and-masking).
//!
//! On the wlr-native channel, the compositor bind is authoritative.
//! The settings window does not claim the trigger.
//! It shows bind lines that call `chibipop ctl` and provides a copy button.
//! KDE, GNOME, and unknown desktops receive the same command for a press
//! shortcut. Their shortcut editors ask for a command, not a text-config bind.
//! Capture exclusion uses the same approach.
//! No Wayland client can hide its surface from third-party capture.
//! The window offers the compositor rule when one exists.
//! It states when no rule exists.

use crate::control::Verb;
use crate::paths;
use chibipop::config::TriggerMode;
use std::fmt;
use std::path::Path;

/// Identify the compositor family that the snippets target.
///
/// The first three families have a native text configuration syntax.
/// KDE and GNOME manage shortcuts in a desktop settings editor.
/// `Other` means that no family-specific syntax is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Hyprland,
    Sway,
    Niri,
    Kde,
    Gnome,
    Other,
}

impl Compositor {
    /// Every family that the settings syntax selector can show.
    pub const ALL: [Compositor; 6] = [
        Compositor::Hyprland,
        Compositor::Sway,
        Compositor::Niri,
        Compositor::Kde,
        Compositor::Gnome,
        Compositor::Other,
    ];

    pub fn detect() -> Compositor {
        let hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
        let sway = std::env::var_os("SWAYSOCK").is_some();
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
        if !hyprland && !sway && std::env::var_os("NIRI_SOCKET").is_some() {
            return Compositor::Niri;
        }
        classify(hyprland, sway, desktop.as_deref())
    }

    /// Name the file or editor that accepts the generated bind.
    ///
    /// The row shows this text above the snippet. A bind line without its
    /// destination is not actionable. The GNOME text also states the repeat
    /// caveat, because a command shortcut cannot ask GNOME to ignore key
    /// repeat.
    pub fn bind_help(self) -> &'static str {
        match self {
            Compositor::Hyprland => {
                "Add this line to ~/.config/hypr/hyprland.conf or an included Hyprland file."
            }
            Compositor::Sway => "Add this line to ~/.config/sway/config.",
            Compositor::Niri => "Add this line inside the existing binds block in ~/.config/niri/config.kdl.",
            Compositor::Kde => {
                "Set this shortcut in KDE System Settings with `systemsettings kcm_keys`."
            }
            // gnome-settings-daemon registers a custom shortcut with
            // META_KEY_BINDING_NONE (plugins/media-keys/gsd-media-keys-manager.c),
            // and Mutter drops repeats only for IGNORE_AUTOREPEAT
            // (src/core/keybindings.c). A command shortcut cannot ask for that
            // flag, so a held chord runs the verb at the repeat rate. Hyprland,
            // Sway, and niri binds suppress repeat in the snippet instead.
            Compositor::Gnome => {
                "Set this shortcut in GNOME Settings with `gnome-control-center keyboard`. GNOME repeats a held custom shortcut, so tap the chord."
            }
            Compositor::Other => {
                "Use the desktop's shortcut editor. No known native text bind syntax exists."
            }
        }
    }

    /// The copy button must not expose a bind that the compositor rejects.
    ///
    /// Niri has no release-bind syntax. Desktop shortcut editors accept a
    /// press command but cannot represent the two events in a hold bind.
    pub fn supports_bind(self, chord: &str, bind: Bind) -> bool {
        match self {
            Compositor::Hyprland | Compositor::Sway => true,
            Compositor::Niri => matches!(bind, Bind::Press(_))
                && chord.rsplit('+').map(str::trim).filter(|part| !part.is_empty()).skip(1).all(|name| {
                    ["SUPER", "META", "MOD4", "LOGO", "WIN", "CONTROL", "CTRL",
                     "ALT", "MOD1", "SHIFT", "ALTGR", "ISO_LEVEL3_SHIFT", "MOD5",
                     "MOD", "ISO_LEVEL5_SHIFT", "MOD3"]
                        .iter().any(|modifier| name.eq_ignore_ascii_case(modifier))
                }),
            Compositor::Kde | Compositor::Gnome | Compositor::Other => matches!(bind, Bind::Press(_)),
        }
    }
}

impl fmt::Display for Compositor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Compositor::Hyprland => "Hyprland",
            Compositor::Sway => "Sway",
            Compositor::Niri => "niri",
            Compositor::Kde => "KDE",
            Compositor::Gnome => "GNOME",
            Compositor::Other => "Unknown",
        };
        f.write_str(name)
    }
}

/// Classify the compositor from supplied signals.
/// The function stays pure so tests can supply those signals.
pub fn classify(hyprland: bool, sway: bool, desktop: Option<&str>) -> Compositor {
    let family = desktop.map(desktop_family);
    if hyprland || family == Some(Compositor::Hyprland) {
        Compositor::Hyprland
    } else if sway || family == Some(Compositor::Sway) {
        Compositor::Sway
    } else {
        family.unwrap_or(Compositor::Other)
    }
}

fn desktop_family(desktop: &str) -> Compositor {
    let mut tokens = desktop
        .split([':', ';', ',', ' ', '-', '_'])
        .map(|part| part.to_ascii_lowercase());
    if tokens.clone().any(|part| part == "hyprland" || part == "hypr") {
        Compositor::Hyprland
    } else if tokens.clone().any(|part| part == "sway") {
        Compositor::Sway
    } else if tokens.clone().any(|part| part == "niri") {
        Compositor::Niri
    } else if tokens.clone().any(|part| part == "kde" || part == "plasma") {
        Compositor::Kde
    } else if tokens.any(|part| part == "gnome") {
        Compositor::Gnome
    } else {
        Compositor::Other
    }
}

/// Split a chord into modifiers and its key.
///
/// `trigger_key_linux` holds the XDG GlobalShortcuts preferred-binding
/// syntax (`ALT+F`). The native snippet spells each modifier for its
/// compositor.
fn split_chord(chord: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> =
        chord.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let key = parts.pop().unwrap_or("F");
    (parts, key)
}

fn modifier(compositor: Compositor, name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    let value = match compositor {
        Compositor::Hyprland => match upper.as_str() {
            "SUPER" | "META" | "MOD4" | "LOGO" | "WIN" => "SUPER",
            "CONTROL" | "CTRL" => "CTRL",
            "ALT" | "MOD1" => "ALT",
            "SHIFT" => "SHIFT",
            "CAPS" => "CAPS",
            "NUM" | "NUMLOCK" | "MOD2" => "MOD2",
            "MOD3" => "MOD3",
            "ALTGR" | "MOD5" => "MOD5",
            _ => return name.trim().to_string(),
        },
        Compositor::Sway => match upper.as_str() {
            "SUPER" | "META" | "MOD4" | "LOGO" | "WIN" => "Mod4",
            "CONTROL" | "CTRL" => "Control",
            "ALT" | "MOD1" => "Mod1",
            "SHIFT" => "Shift",
            "CAPS" | "LOCK" => "Lock",
            "NUM" | "NUMLOCK" | "MOD2" => "Mod2",
            "MOD3" => "Mod3",
            "ALTGR" | "MOD5" => "Mod5",
            _ => return name.trim().to_string(),
        },
        Compositor::Niri => match upper.as_str() {
            "SUPER" | "META" | "MOD4" | "LOGO" | "WIN" => "Super",
            "CONTROL" | "CTRL" => "Ctrl",
            "ALT" | "MOD1" => "Alt",
            "SHIFT" => "Shift",
            "ALTGR" | "ISO_LEVEL3_SHIFT" | "MOD5" => "Mod5",
            "MOD" => "Mod",
            _ => return name.trim().to_string(),
        },
        Compositor::Kde | Compositor::Gnome | Compositor::Other => {
            return name.trim().to_string()
        }
    };
    value.to_string()
}

fn native_chord(compositor: Compositor, chord: &str, separator: &str) -> String {
    let (mods, key) = split_chord(chord);
    let key = match compositor {
        Compositor::Sway | Compositor::Niri
            if key.chars().count() == 1 && key.chars().all(|ch| ch.is_ascii_alphabetic()) =>
        {
            key.to_ascii_lowercase()
        }
        _ => key.to_string(),
    };
    mods.iter()
        .map(|name| modifier(compositor, name))
        .chain(std::iter::once(key))
        .collect::<Vec<_>>()
        .join(separator)
}

fn kdl_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            ch if ch.is_control() => {
                quoted.push_str(&format!("\\u{{{:x}}}", ch as u32));
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
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
///
/// The snippet contains exactly the verbs that the control socket accepts.
///
/// The caller resolves `exe` with `paths::exec_name`.
/// This function does not look up `exe`.
/// A pasted bind must execute the daemon that the user runs.
/// Under `cargo run`, that daemon is `target/debug/chibipop` and is not on PATH.
/// The external lookup keeps this function pure.
pub fn bind_snippet(compositor: Compositor, chord: &str, exe: &Path, bind: Bind) -> String {
    match compositor {
        Compositor::Hyprland => {
            let (mods, key) = split_chord(chord);
            let mask = mods
                .iter()
                .map(|name| modifier(Compositor::Hyprland, name))
                .collect::<Vec<_>>()
                .join(" ");
            let exe = paths::shell_quote(exe);
            match bind {
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
                Bind::Hold => format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {down}\n\
                     bindr = {mask}, {key}, exec, {exe} ctl {up}\n\
                     # Release {key} before {mask} - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
                     # If the popup sticks, tap the chord again (release {key} first), or bind `ctl toggle` instead.",
                    down = Verb::TriggerDown.as_str(),
                    up = Verb::TriggerUp.as_str(),
                ),
                Bind::Press(verb) => format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {verb}",
                    verb = verb.as_str(),
                ),
            }
        }
        Compositor::Sway => {
            let chord = native_chord(Compositor::Sway, chord, "+");
            let exe = paths::shell_quote(exe);
            match bind {
                Bind::Hold => format!(
                    "bindsym --no-repeat {chord} exec {exe} ctl {down}\n\
                     bindsym --release {chord} exec {exe} ctl {up}",
                    down = Verb::TriggerDown.as_str(),
                    up = Verb::TriggerUp.as_str(),
                ),
                Bind::Press(verb) => format!(
                    "bindsym --no-repeat {chord} exec {exe} ctl {verb}",
                    verb = verb.as_str(),
                ),
            }
        }
        Compositor::Niri => {
            if !compositor.supports_bind(chord, bind) {
                return unsupported_bind(compositor, bind);
            }
            let chord = kdl_quote(&native_chord(Compositor::Niri, chord, "+"));
            let exe = kdl_quote(&exe.to_string_lossy());
            let Bind::Press(verb) = bind else {
                unreachable!("unsupported niri hold bind returned above");
            };
            format!(
                "{chord} repeat=false {{ spawn {exe} \"ctl\" \"{verb}\"; }};",
                verb = verb.as_str(),
            )
        }
        Compositor::Kde | Compositor::Gnome | Compositor::Other => match bind {
            Bind::Press(verb) => {
                format!("{} ctl {}", paths::shell_quote(exe), verb.as_str())
            }
            Bind::Hold => unsupported_bind(compositor, bind),
        },
    }
}

fn unsupported_bind(compositor: Compositor, bind: Bind) -> String {
    match compositor {
        Compositor::Niri => match bind {
            Bind::Hold => "Niri has no key-release bind. Select Press or Toggle mode for a native trigger bind.".to_string(),
            Bind::Press(_) => {
                "For Niri, use Ctrl, Alt, Shift, Super, Mod, Mod3, or Mod5. Niri does not support the requested modifiers.".to_string()
            }
        },
        Compositor::Kde | Compositor::Gnome | Compositor::Other => {
            assert!(matches!(bind, Bind::Hold));
            format!(
                "# {compositor} has no native text bind syntax for a hold action.\n\
                 # {help}\n\
                 # A desktop shortcut editor can run a press command, but it cannot send key release.",
                help = compositor.bind_help(),
            )
        }
        Compositor::Hyprland | Compositor::Sway => {
            unreachable!("native compositor supports every bind shape")
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
        Compositor::Sway | Compositor::Niri | Compositor::Gnome | Compositor::Other => (
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
            snippet.lines().filter(|line| !line.starts_with('#')).collect::<Vec<_>>(),
            [
                "bind = ALT, F, exec, /home/u/chibipop/target/debug/chibipop ctl trigger-down",
                "bindr = ALT, F, exec, /home/u/chibipop/target/debug/chibipop ctl trigger-up",
            ]
        );
    }

    #[test]
    fn hyprland_bind_for_a_two_modifier_chord() {
        let snippet =
            bind_snippet(Compositor::Hyprland, "CTRL+SHIFT+K", Path::new("chibipop"), Bind::Hold);
        assert_eq!(
            snippet.lines().filter(|line| !line.starts_with('#')).collect::<Vec<_>>(),
            [
                "bind = CTRL SHIFT, K, exec, chibipop ctl trigger-down",
                "bindr = CTRL SHIFT, K, exec, chibipop ctl trigger-up",
            ]
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
    fn sway_bind_normalizes_portal_modifiers() {
        let snippet = bind_snippet(Compositor::Sway, "ALT+SUPER+CTRL+SHIFT+F", Path::new(DEV_EXE), Bind::Hold);
        assert!(snippet.contains(&format!(
            "bindsym --no-repeat Mod1+Mod4+Control+Shift+f exec {DEV_EXE} ctl trigger-down"
        )));
        assert!(snippet.contains(&format!(
            "bindsym --release Mod1+Mod4+Control+Shift+f exec {DEV_EXE} ctl trigger-up"
        )));
    }

    #[test]
    fn sway_press_bind_has_no_release_line() {
        let snippet = bind_snippet(
            Compositor::Sway,
            "SUPER+A",
            Path::new(DEV_EXE),
            Bind::Press(Verb::AnkiAdd),
        );
        assert_eq!(
            snippet,
            format!("bindsym --no-repeat Mod4+a exec {DEV_EXE} ctl anki-add")
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
    fn unknown_compositor_gets_explicit_guidance_instead_of_sway_syntax() {
        let hold = bind_snippet(Compositor::Other, "ALT+F", Path::new(DEV_EXE), Bind::Hold);
        assert!(!hold.contains("bindsym"), "{hold}");
        assert!(Compositor::Other.supports_bind("ALT+A", Bind::Press(Verb::AnkiAdd)));
        assert_eq!(
            bind_snippet(
                Compositor::Other,
                "ALT+A",
                Path::new(DEV_EXE),
                Bind::Press(Verb::AnkiAdd),
            ),
            format!("{DEV_EXE} ctl anki-add")
        );
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
                "bindsym --no-repeat Mod1+f exec '/home/u/my builds/chibipop' ctl trigger-down"
            ),
            "{sway}"
        );
        assert!(
            sway.contains(
                "bindsym --release Mod1+f exec '/home/u/my builds/chibipop' ctl trigger-up"
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
    fn niri_emits_a_real_press_bind_and_rejects_hold() {
        let press = bind_snippet(
            Compositor::Niri,
            "SUPER+CTRL+A",
            Path::new("/home/u/my builds/chibipop"),
            Bind::Press(Verb::AnkiAdd),
        );
        assert_eq!(
            press,
            "\"Super+Ctrl+a\" repeat=false { spawn \"/home/u/my builds/chibipop\" \"ctl\" \"anki-add\"; };"
        );
        assert!(!Compositor::Niri.supports_bind("ALT+F", Bind::Hold));
    }

    #[test]
    fn niri_quotes_a_digit_key_as_a_node_name() {
        let snippet = bind_snippet(Compositor::Niri, "1", Path::new("chibipop"), Bind::Press(Verb::Search));
        assert_eq!(snippet, "\"1\" repeat=false { spawn \"chibipop\" \"ctl\" \"search\"; };");
    }

    #[test]
    fn detects_compositor_from_desktop_names() {
        assert_eq!(classify(false, false, Some("GNOME")), Compositor::Gnome);
        assert_eq!(classify(false, false, Some("KDE;Plasma")), Compositor::Kde);
        assert_eq!(classify(false, false, Some("niri")), Compositor::Niri);
        assert_eq!(classify(false, false, Some("Hyprland")), Compositor::Hyprland);
        assert_eq!(classify(false, false, Some("Sway")), Compositor::Sway);
    }

    /// KDE and GNOME editors take one command for a press action. A hold
    /// needs a key release that no editor can send. GNOME cannot suppress
    /// key repeat for a command, so its help states the caveat.
    #[test]
    fn desktop_editors_get_commands_for_press_and_guidance_for_hold() {
        let add = Bind::Press(Verb::AnkiAdd);
        let press = bind_snippet(Compositor::Kde, "ALT+A", Path::new(DEV_EXE), add);
        assert_eq!(format!("{DEV_EXE} ctl anki-add"), press);
        assert_eq!(press, bind_snippet(Compositor::Gnome, "ALT+A", Path::new(DEV_EXE), add));
        assert!(Compositor::Kde.supports_bind("ALT+A", add));

        let gnome_help = Compositor::Gnome.bind_help();
        assert!(gnome_help.contains("repeats a held custom shortcut"), "{gnome_help}");
        assert!(!Compositor::Kde.bind_help().contains("repeats"));

        assert!(!Compositor::Kde.supports_bind("ALT+A", Bind::Hold));
        assert!(!Compositor::Gnome.supports_bind("ALT+A", Bind::Hold));
        let hold = bind_snippet(Compositor::Other, "ALT+A", Path::new(DEV_EXE), Bind::Hold);
        assert!(hold.contains("cannot send key release"), "{hold}");
        assert!(!hold.contains("ctl trigger-down"), "{hold}");
    }

    #[test]
    fn hyprland_normalizes_aliases_without_a_portal_dispatcher() {
        let snippet = bind_snippet(
            Compositor::Hyprland,
            "meta+control+alt+shift+F",
            Path::new(DEV_EXE),
            Bind::Press(Verb::Lookup),
        );
        assert_eq!(
            snippet,
            format!(
                "bind = SUPER CTRL ALT SHIFT, F, exec, {DEV_EXE} ctl lookup"
            )
        );
    }

    #[test]
    fn detection_prefers_the_specific_signals() {
        assert_eq!(classify(true, true, Some("KDE")), Compositor::Hyprland);
        assert_eq!(classify(false, true, Some("niri")), Compositor::Sway);
        assert_eq!(classify(false, false, Some("KDE")), Compositor::Kde);
        assert_eq!(classify(false, false, None), Compositor::Other);
    }
}

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
    pub fn supports_bind(self, chord: &str, bind: Bind<'_>) -> bool {
        if !crate::shortcuts::ShortcutId::is_valid(bind.id) {
            return false;
        }
        match self {
            Compositor::Hyprland | Compositor::Sway => true,
            Compositor::Niri => !bind.requires_release()
                && chord.rsplit('+').map(str::trim).filter(|part| !part.is_empty()).skip(1).all(|name| {
                    ["SUPER", "META", "MOD4", "LOGO", "WIN", "CONTROL", "CTRL",
                     "ALT", "MOD1", "SHIFT", "ALTGR", "ISO_LEVEL3_SHIFT", "MOD5",
                     "MOD", "ISO_LEVEL5_SHIFT", "MOD3"]
                        .iter().any(|modifier| name.eq_ignore_ascii_case(modifier))
                }),
            Compositor::Kde | Compositor::Gnome | Compositor::Other => !bind.requires_release(),
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

/// Select the native bind shape and request ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind<'a> {
    pub id: &'a str,
    pub mode: TriggerMode,
}

impl<'a> Bind<'a> {
    fn requires_release(self) -> bool {
        self.mode == TriggerMode::HoldKey
    }
}

#[derive(Clone, Copy)]
struct CtlCommand<'a> {
    id: &'a str,
    activated: bool,
}

impl fmt::Display for CtlCommand<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", if self.activated { "bind-down" } else { "bind-up" }, self.id)
    }
}

impl<'a> Bind<'a> {
    fn down(self) -> CtlCommand<'a> {
        CtlCommand { id: self.id, activated: true }
    }

    fn up(self) -> CtlCommand<'a> {
        CtlCommand { id: self.id, activated: false }
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
pub fn bind_snippet(compositor: Compositor, chord: &str, exe: &Path, bind: Bind<'_>) -> String {
    if !compositor.supports_bind(chord, bind) {
        return unsupported_bind(compositor, bind);
    }

    match compositor {
        Compositor::Hyprland => {
            let (mods, key) = split_chord(chord);
            let mask = mods
                .iter()
                .map(|name| modifier(Compositor::Hyprland, name))
                .collect::<Vec<_>>()
                .join(" ");
            let exe = paths::shell_quote(exe);
            if bind.requires_release() {
                format!(
                    "bind = {mask}, {key}, exec, {exe} ctl {down}\n\
                     bindr = {mask}, {key}, exec, {exe} ctl {up}\n\
                     # Release {key} before {mask} - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
                     # If the popup stays open, change this bind to Press or Toggle mode in chibipop settings.",
                    down = bind.down(),
                    up = bind.up(),
                )
            } else {
                format!("bind = {mask}, {key}, exec, {exe} ctl {down}", down = bind.down())
            }
        }
        Compositor::Sway => {
            let chord = native_chord(Compositor::Sway, chord, "+");
            let exe = paths::shell_quote(exe);
            if bind.requires_release() {
                format!(
                    "bindsym --no-repeat {chord} exec {exe} ctl {down}\n\
                     bindsym --release {chord} exec {exe} ctl {up}",
                    down = bind.down(),
                    up = bind.up(),
                )
            } else {
                format!("bindsym --no-repeat {chord} exec {exe} ctl {down}", down = bind.down())
            }
        }
        Compositor::Niri => {
            let chord = kdl_quote(&native_chord(Compositor::Niri, chord, "+"));
            let exe = kdl_quote(&exe.to_string_lossy());
            format!(
                "{chord} repeat=false {{ spawn {exe} {}; }};",
                NiriArgs(bind.down())
            )
        }
        Compositor::Kde | Compositor::Gnome | Compositor::Other => {
            format!("{} ctl {}", paths::shell_quote(exe), bind.down())
        }
    }
}

struct NiriArgs<'a>(CtlCommand<'a>);

impl fmt::Display for NiriArgs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"ctl\" \"{}\" \"{}\"",
            if self.0.activated { "bind-down" } else { "bind-up" }, self.0.id)
    }
}

fn unsupported_bind(compositor: Compositor, bind: Bind<'_>) -> String {
    if !crate::shortcuts::ShortcutId::is_valid(bind.id) {
        return "The configured bind ID is invalid.".to_string();
    }
    match compositor {
        Compositor::Niri if bind.requires_release() => {
            "Niri has no key-release bind. Select Press or Toggle mode for this lookup bind.".to_string()
        }
        Compositor::Niri => {
            "For Niri, use Ctrl, Alt, Shift, Super, Mod, Mod3, or Mod5. Niri does not support the requested modifiers.".to_string()
        }
        Compositor::Kde | Compositor::Gnome | Compositor::Other => {
            assert!(bind.requires_release());
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
        let snippet = bind_snippet(Compositor::Hyprland, "ALT+F", Path::new(DEV_EXE), Bind { id: "lookup", mode: TriggerMode::HoldKey });
        assert_eq!(
            snippet.lines().filter(|line| !line.starts_with('#')).collect::<Vec<_>>(),
            [
                "bind = ALT, F, exec, /home/u/chibipop/target/debug/chibipop ctl bind-down lookup",
                "bindr = ALT, F, exec, /home/u/chibipop/target/debug/chibipop ctl bind-up lookup",
            ]
        );
    }

    #[test]
    fn hyprland_bind_for_a_two_modifier_chord() {
        let snippet =
            bind_snippet(Compositor::Hyprland, "CTRL+SHIFT+K", Path::new("chibipop"), Bind { id: "lookup", mode: TriggerMode::HoldKey });
        assert_eq!(
            snippet.lines().filter(|line| !line.starts_with('#')).collect::<Vec<_>>(),
            [
                "bind = CTRL SHIFT, K, exec, chibipop ctl bind-down lookup",
                "bindr = CTRL SHIFT, K, exec, chibipop ctl bind-up lookup",
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
            Bind { id: "action", mode: TriggerMode::Press },
        );
        assert_eq!(
            snippet,
            "bind = ALT, A, exec, /home/u/chibipop/target/debug/chibipop ctl bind-down action"
        );
    }

    #[test]
    fn configured_lookup_modes_emit_the_required_bind_events() {
        for compositor in [Compositor::Hyprland, Compositor::Sway] {
            let toggle = bind_snippet(
                compositor,
                "ALT+F",
                Path::new(DEV_EXE),
                Bind { id: "bind-7", mode: TriggerMode::Toggle },
            );
            assert!(toggle.contains("ctl bind-down bind-7"), "{toggle}");
            assert!(!toggle.contains("bind-up"), "{toggle}");

            let press = bind_snippet(
                compositor,
                "ALT+F",
                Path::new(DEV_EXE),
                Bind { id: "bind-7", mode: TriggerMode::Press },
            );
            assert!(press.contains("ctl bind-down bind-7"), "{press}");
            assert!(!press.contains("bind-up"), "{press}");

            let hold = bind_snippet(
                compositor,
                "ALT+F",
                Path::new(DEV_EXE),
                Bind { id: "bind-7", mode: TriggerMode::HoldKey },
            );
            assert!(hold.contains("ctl bind-down bind-7"), "{hold}");
            assert!(hold.contains("ctl bind-up bind-7"), "{hold}");
        }
    }

    #[test]
    fn sway_bind_normalizes_portal_modifiers() {
        let snippet = bind_snippet(Compositor::Sway, "ALT+SUPER+CTRL+SHIFT+F", Path::new(DEV_EXE), Bind { id: "lookup", mode: TriggerMode::HoldKey });
        assert!(snippet.contains(&format!(
            "bindsym --no-repeat Mod1+Mod4+Control+Shift+f exec {DEV_EXE} ctl bind-down lookup"
        )));
        assert!(snippet.contains(&format!(
            "bindsym --release Mod1+Mod4+Control+Shift+f exec {DEV_EXE} ctl bind-up lookup"
        )));
    }

    #[test]
    fn sway_press_bind_has_no_release_line() {
        let snippet = bind_snippet(
            Compositor::Sway,
            "SUPER+A",
            Path::new(DEV_EXE),
            Bind { id: "action", mode: TriggerMode::Press },
        );
        assert_eq!(
            snippet,
            format!("bindsym --no-repeat Mod4+a exec {DEV_EXE} ctl bind-down action")
        );
        assert!(!snippet.contains("--release"), "a press bind has no release line: {snippet}");
    }



    #[test]
    fn unknown_compositor_gets_explicit_guidance_instead_of_sway_syntax() {
        let hold = bind_snippet(Compositor::Other, "ALT+F", Path::new(DEV_EXE), Bind { id: "lookup", mode: TriggerMode::HoldKey });
        assert!(!hold.contains("bindsym"), "{hold}");
        assert!(Compositor::Other.supports_bind("ALT+A", Bind { id: "action", mode: TriggerMode::Press }));
        assert_eq!(
            bind_snippet(
                Compositor::Other,
                "ALT+A",
                Path::new(DEV_EXE),
                Bind { id: "action", mode: TriggerMode::Press },
            ),
            format!("{DEV_EXE} ctl bind-down action")
        );
    }

    /// Paths can contain spaces, for example `~/My Builds/...` or a user-named
    /// checkout directory.
    /// An unquoted path can execute the wrong word.
    /// Both dialects must quote such a path.
    #[test]
    fn a_path_with_a_space_is_quoted_for_both_dialects() {
        let exe = Path::new("/home/u/my builds/chibipop");
        let hypr = bind_snippet(Compositor::Hyprland, "ALT+F", exe, Bind { id: "lookup", mode: TriggerMode::HoldKey });
        assert!(
            hypr.contains("bind = ALT, F, exec, '/home/u/my builds/chibipop' ctl bind-down lookup"),
            "{hypr}"
        );
        assert!(
            hypr.contains("bindr = ALT, F, exec, '/home/u/my builds/chibipop' ctl bind-up lookup"),
            "{hypr}"
        );
        let sway = bind_snippet(Compositor::Sway, "ALT+F", exe, Bind { id: "lookup", mode: TriggerMode::HoldKey });
        assert!(
            sway.contains(
                "bindsym --no-repeat Mod1+f exec '/home/u/my builds/chibipop' ctl bind-down lookup"
            ),
            "{sway}"
        );
        assert!(
            sway.contains(
                "bindsym --release Mod1+f exec '/home/u/my builds/chibipop' ctl bind-up lookup"
            ),
            "{sway}"
        );
        let press = bind_snippet(Compositor::Hyprland, "ALT+A", exe, Bind { id: "action", mode: TriggerMode::Press });
        assert_eq!(
            "bind = ALT, A, exec, '/home/u/my builds/chibipop' ctl bind-down action",
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
            Bind { id: "action", mode: TriggerMode::Press },
        );
        assert_eq!(
            press,
            "\"Super+Ctrl+a\" repeat=false { spawn \"/home/u/my builds/chibipop\" \"ctl\" \"bind-down\" \"action\"; };"
        );
        assert!(!Compositor::Niri.supports_bind("ALT+F", Bind { id: "lookup", mode: TriggerMode::HoldKey }));
    }

    #[test]
    fn niri_quotes_a_digit_key_as_a_node_name() {
        let snippet = bind_snippet(Compositor::Niri, "1", Path::new("chibipop"), Bind { id: "action", mode: TriggerMode::Press });
        assert_eq!(snippet, "\"1\" repeat=false { spawn \"chibipop\" \"ctl\" \"bind-down\" \"action\"; };");
    }

    #[test]
    fn detects_compositor_from_desktop_names() {
        assert_eq!(classify(false, false, Some("GNOME")), Compositor::Gnome);
        assert_eq!(classify(false, false, Some("KDE;Plasma")), Compositor::Kde);
        assert_eq!(classify(false, false, Some("niri")), Compositor::Niri);
        assert_eq!(classify(false, false, Some("Hyprland")), Compositor::Hyprland);
        assert_eq!(classify(false, false, Some("Sway")), Compositor::Sway);
    }

    #[test]
    fn desktop_editors_get_commands_for_press_and_guidance_for_hold() {
        let add = Bind { id: "action", mode: TriggerMode::Press };
        let press = bind_snippet(Compositor::Kde, "ALT+A", Path::new(DEV_EXE), add);
        assert_eq!(format!("{DEV_EXE} ctl bind-down action"), press);
        assert_eq!(press, bind_snippet(Compositor::Gnome, "ALT+A", Path::new(DEV_EXE), add));
        assert!(Compositor::Kde.supports_bind("ALT+A", add));

        let hold = Bind { id: "lookup", mode: TriggerMode::HoldKey };
        assert!(!Compositor::Kde.supports_bind("ALT+A", hold));
        assert!(!Compositor::Gnome.supports_bind("ALT+A", hold));
        assert!(!Compositor::Other.supports_bind("ALT+A", hold));
    }

    #[test]
    fn hyprland_normalizes_aliases_without_a_portal_dispatcher() {
        let snippet = bind_snippet(
            Compositor::Hyprland,
            "meta+control+alt+shift+F",
            Path::new(DEV_EXE),
            Bind { id: "action", mode: TriggerMode::Press },
        );
        assert_eq!(
            snippet,
            format!(
                "bind = SUPER CTRL ALT SHIFT, F, exec, {DEV_EXE} ctl bind-down action"
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

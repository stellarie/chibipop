//! Copyable compositor snippets (ADR-0005/0008).
//!
//! On the wlr-native channel the compositor bind is the truth, so the
//! settings window never pretends to own the trigger: it shows the bind
//! lines that shell out to `chibipop ctl`, with a copy button. Capture
//! exclusion is the same shape — no Wayland client can hide its surface
//! from third-party capture, so the window offers the compositor's own
//! rule where one exists and says so where none does.

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

/// The native-bind snippet: press runs `trigger-down`, release
/// `trigger-up`, exactly the verbs the control socket speaks.
pub fn bind_snippet(compositor: Compositor, chord: &str) -> String {
    let (mods, key) = split_chord(chord);
    match compositor {
        Compositor::Hyprland => {
            let mods = mods.join(" ");
            format!(
                "bind = {mods}, {key}, exec, chibipop ctl trigger-down\n\
                 bindr = {mods}, {key}, exec, chibipop ctl trigger-up"
            )
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
            format!(
                "# sway syntax - adapt to your compositor\n\
                 bindsym --no-repeat {chord} exec chibipop ctl trigger-down\n\
                 bindsym --release {chord} exec chibipop ctl trigger-up"
            )
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

    #[test]
    fn hyprland_bind_for_the_default_chord() {
        let snippet = bind_snippet(Compositor::Hyprland, "ALT+F");
        assert_eq!(
            snippet,
            "bind = ALT, F, exec, chibipop ctl trigger-down\n\
             bindr = ALT, F, exec, chibipop ctl trigger-up"
        );
    }

    #[test]
    fn hyprland_bind_for_a_two_modifier_chord() {
        let snippet = bind_snippet(Compositor::Hyprland, "CTRL+SHIFT+K");
        assert_eq!(
            snippet,
            "bind = CTRL SHIFT, K, exec, chibipop ctl trigger-down\n\
             bindr = CTRL SHIFT, K, exec, chibipop ctl trigger-up"
        );
    }

    #[test]
    fn sway_bind_keeps_the_chord_spelling() {
        let snippet = bind_snippet(Compositor::Sway, "ALT+F");
        assert!(snippet.contains("bindsym --no-repeat ALT+F exec chibipop ctl trigger-down"));
        assert!(snippet.contains("bindsym --release ALT+F exec chibipop ctl trigger-up"));
    }

    #[test]
    fn unknown_compositor_gets_the_labelled_sway_dialect() {
        assert!(bind_snippet(Compositor::Other, "ALT+F").starts_with("# sway syntax"));
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

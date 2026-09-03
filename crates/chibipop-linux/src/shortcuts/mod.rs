//! The trigger channel has two rungs (ARCHITECTURE.md#input-ladders).
//! The GlobalShortcuts portal provides the first rung.
//! A compositor keybind to the control socket provides the second rung.
//! Both rungs convert shortcut events to the same control-socket verb.
//!
//! **The native rung always works.** The order selects the rung that asks
//! the user for a binding. It does not disable the other transport.
//! Each session binds `chibipop ctl trigger-down|trigger-up|toggle` at
//! startup. The control socket accepts requests when the portal is active.
//! The portal then supplies another source for the same press and release
//! events. If the portal is absent or refuses the request, the control socket
//! remains the only source. The trigger row is never `Down` because the
//! daemon always keeps the trigger channel available.
//!
//! **Exactly two ids** ([`ShortcutId`]): `trigger` and
//! `anki-add`. The consent dialog lists each requested shortcut.
//! Users can reject a long list. An enum fixes the set instead of config data.
//! The compiler and a test enforce the two-identifier limit.
//!
//! **Portal interface facts.** The local interface XML confirms these facts:
//!
//! * `org.freedesktop.portal.GlobalShortcuts` interface version 2 has no
//!   restore token or persist mode. ScreenCast has a token, which
//!   `capture/portal/token.rs` stores. `BindShortcuts` runs once for each
//!   portal session. `ListShortcuts` returns active shortcuts after
//!   `BindShortcuts`. Without that call, it returns shortcuts that a previous
//!   portal session bound for this application. The interface definition is
//!   `/usr/share/dbus-1/interfaces/org.freedesktop.portal.GlobalShortcuts.xml`.
//! * A trigger is a chord, not a bare modifier. The shortcuts spec draft 0.1
//!   uses XKB modifier names (`CTRL`, `ALT`, `SHIFT`, `NUM`, `LOGO`).
//!   Each chord also contains one keysym from `xkbcommon-keysyms.h` without
//!   the `XKB_KEY_` prefix. Use `+` between parts, and use only the base layer.
//!   `ALT+F` is the Linux default. [`normalize_trigger`] converts a user's
//!   chord to the required form before transport.
//!
//! **An app id is mandatory.** xdg-desktop-portal rejects `CreateSession`
//!   with `NotAllowed`/"An app id is required" when it cannot name the caller.
//!   For a non-sandboxed process, it derives the name from the systemd user
//!   unit (`app[-<launcher>]-<ApplicationID>-<RANDOM>.scope|.slice|.service`)
//!   and requires a matching `<ApplicationID>.desktop` file. A daemon started
//!   from a shell has neither. This rung is unreachable, even with a new
//!   portal. [`explain`] reports this condition. The user can launch from the
//!   desktop entry or autostart unit. The control socket carries the trigger
//!   in the meantime.

pub mod portal;
pub mod state;

use crate::control::Verb;
use std::path::Path;

/// The fixed set of shortcuts that chibipop registers. It keeps the consent
/// dialog small (ARCHITECTURE.md#input-ladders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutId {
    /// Hold this shortcut to read text. A press freezes a grab and starts a
    /// lookup. A release retracts the popup. The portal sends both
    /// `Activated`/`Deactivated` events, so the daemon needs no keyboard
    /// access.
    Trigger,
    /// Start the Anki add action with this shortcut. The popup never takes
    /// focus, so this action needs a global shortcut on Wayland.
    AnkiAdd,
}

impl ShortcutId {
    /// The complete set in the order that the daemon registers it. The
    /// fixed-size array makes the identifier set part of the application.
    pub const ALL: [ShortcutId; 2] = [ShortcutId::Trigger, ShortcutId::AnkiAdd];

    /// The stable identifier on the wire. Hyprland prefixes this value with
    /// the portal app ID, which can depend on the process that launched the
    /// daemon. A rename still breaks portal bindings without an error.
    pub fn as_str(self) -> &'static str {
        match self {
            ShortcutId::Trigger => "trigger",
            ShortcutId::AnkiAdd => "anki-add",
        }
    }

    /// Parse a known identifier. Return `None` for a foreign portal session
    /// or a stale binding.
    pub fn parse(id: &str) -> Option<ShortcutId> {
        ShortcutId::ALL.into_iter().find(|known| known.as_str() == id)
    }

    /// Return the `description` that the portal shows to the user. The text
    /// explains the key action because it describes a system-wide key grab.
    pub fn description(self) -> &'static str {
        match self {
            ShortcutId::Trigger => "Hold to look up the Japanese text under the cursor",
            ShortcutId::AnkiAdd => "Add the word shown in the popup to Anki",
        }
    }
}

/// One binding from `BindShortcuts` or `ListShortcuts`.
///
/// `trigger` contains the portal's `trigger_description` for display.
/// Some implementations report no key. For example,
/// xdg-desktop-portal-hyprland returns `trigger_description: ""` because
/// Hyprland stores the key in the user's configuration. A binding can exist
/// when `trigger` is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: ShortcutId,
    pub trigger: Option<String>,
}

impl Binding {
    /// Format one binding for a status row. Show the reported key or state
    /// that the portal did not report it.
    pub fn describe(&self) -> String {
        match &self.trigger {
            Some(trigger) => format!("{} {trigger}", self.id.as_str()),
            None => format!("{} (key not reported)", self.id.as_str()),
        }
    }
}

/// Messages from the portal thread to the calloop pump. The thread sends
/// each D-Bus result here and does not own the log or tray
/// (ARCHITECTURE.md#workspace-and-seams). The calloop pump stays
/// synchronous and single-threaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `BindShortcuts` succeeded. The bindings contain the portal's current
    /// result. An empty list means that the user approved no shortcuts.
    Bound(Vec<Binding>),
    /// The desktop UI reports a binding change through `ShortcutsChanged`.
    /// The payload has the same form as `Bound`.
    Changed(Vec<Binding>),
    /// A shortcut fired. `true` means `Activated`. `false` means `Deactivated`.
    Fired { id: ShortcutId, activated: bool },
    /// The portal rung cannot serve shortcuts. `reason` fits a status row.
    /// `advice` gives a log action when one exists. The control socket
    /// continues to carry trigger actions.
    Unavailable { reason: String, advice: Option<String> },
    /// A diagnostic that the calloop pump writes.
    Note(String),
}

/// The `CHIBIPOP_TRIGGER_CHANNEL` test hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOverride {
    /// Use the documented ladder order.
    Auto,
    /// Select the portal rung when the interface exists. If it is absent, the
    /// daemon reports that condition. Use this value to test the same portal
    /// rung that automatic selection uses.
    Portal,
    /// Do not use the portal. The control socket is the only trigger source.
    /// This matches a sway or wlr session that also has a portal.
    Native,
}

impl ChannelOverride {
    pub const ENV: &'static str = "CHIBIPOP_TRIGGER_CHANNEL";

    /// Parse `auto|portal|native`. Return `None` for any other value.
    pub fn parse(value: &str) -> Option<ChannelOverride> {
        match value {
            "auto" => Some(ChannelOverride::Auto),
            "portal" => Some(ChannelOverride::Portal),
            "native" => Some(ChannelOverride::Native),
            _ => None,
        }
    }

    /// Read the override and return a diagnostic for an unknown value.
    pub fn from_env() -> (ChannelOverride, Option<String>) {
        match std::env::var(Self::ENV) {
            Err(_) => (ChannelOverride::Auto, None),
            Ok(v) => match Self::parse(&v) {
                Some(ov) => (ov, None),
                None => (
                    ChannelOverride::Auto,
                    Some(format!(
                        "trigger: ignoring {}={v:?}; expected auto|portal|native",
                        Self::ENV
                    )),
                ),
            },
        }
    }
}

/// Reasons that cause the native rung to ask the user for a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeReason {
    /// The session bus lacks
    /// `org.freedesktop.portal.GlobalShortcuts`. This includes sway, generic
    /// wlr, and GNOME below the supported floor.
    NoPortal,
    /// The user set `CHIBIPOP_TRIGGER_CHANNEL=native`.
    Forced,
    /// The user set `CHIBIPOP_TRIGGER_CHANNEL=portal`, but the session has no
    /// interface. The daemon reports this condition.
    ForcedButAbsent,
}

/// The rung that requests a binding. The control socket serves both selections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Rung 1 registers the two ids with the portal and keeps the socket.
    Portal,
    /// Rung 2 uses the compositor keybind to reach the control socket.
    Native(NativeReason),
}

impl Selection {
    /// Build the daemon's startup log line.
    ///
    /// `exe` identifies the binary in advice for the native rung. The caller
    /// resolves it through `paths::exec_name`. A bare command does not resolve
    /// under `cargo run` when PATH does not contain it.
    pub fn startup_line(self, exe: &Path) -> String {
        match self {
            Selection::Portal => format!(
                "trigger: {} portal (ladder rung 1) - registering {} and {}; the control socket keeps serving too",
                portal::SHORTCUTS_INTERFACE,
                ShortcutId::Trigger.as_str(),
                ShortcutId::AnkiAdd.as_str()
            ),
            Selection::Native(NativeReason::NoPortal) => format!(
                "trigger: control socket only (ladder rung 2) - no {} on the session bus; bind `{} ctl trigger-down|trigger-up` in your compositor",
                portal::SHORTCUTS_INTERFACE,
                crate::paths::shell_quote(exe)
            ),
            Selection::Native(NativeReason::Forced) => format!(
                "trigger: control socket only - {}=native override active (test hook)",
                ChannelOverride::ENV
            ),
            Selection::Native(NativeReason::ForcedButAbsent) => format!(
                "trigger: control socket only - {}=portal was asked for but {} is not on the session bus",
                ChannelOverride::ENV,
                portal::SHORTCUTS_INTERFACE
            ),
        }
    }
}

/// Select the trigger rung (ARCHITECTURE.md#input-ladders). The `portal`
/// argument is the result of the caller's D-Bus probe.
pub fn select(portal: bool, ov: ChannelOverride) -> Selection {
    match (ov, portal) {
        (ChannelOverride::Auto | ChannelOverride::Portal, true) => Selection::Portal,
        (ChannelOverride::Auto, false) => Selection::Native(NativeReason::NoPortal),
        (ChannelOverride::Portal, false) => Selection::Native(NativeReason::ForcedButAbsent),
        (ChannelOverride::Native, _) => Selection::Native(NativeReason::Forced),
    }
}

/// Build the two shortcuts from the configured chords. Each chord uses the
/// form that the shortcuts spec defines.
///
/// The fixed-size array enforces exactly two ids.
pub fn preferred(config: &chibipop::config::Config) -> [(ShortcutId, String); 2] {
    [
        (ShortcutId::Trigger, normalize_trigger(&config.trigger.trigger_key_linux)),
        (ShortcutId::AnkiAdd, normalize_trigger(&config.anki.add_key_linux)),
    ]
}

/// Convert a user's chord to the form that the shortcuts spec defines.
/// The result uses uppercase XKB modifier names and a key name from
/// `xkbcommon-keysyms.h`.
///
/// Two conversions affect portal acceptance. `SUPER` is a common user term,
/// but the shortcuts spec requires `LOGO`. The function converts a
/// one-letter key to lowercase. `F` names the shifted keysym `XKB_KEY_F`,
/// but the shortcuts spec requires the base layer. Therefore, the default
/// `ALT+F` means Alt with the F key. Long names such as `Return`, `F1`, and
/// `space` keep their original form. These values are keysym names, and
/// case is significant.
pub fn normalize_trigger(chord: &str) -> String {
    let parts: Vec<&str> = chord.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return String::new();
    };
    let mut out = String::with_capacity(chord.len());
    for modifier in modifiers {
        out.push_str(&spec_modifier(modifier));
        out.push('+');
    }
    if key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphabetic()) {
        out.push(key.to_ascii_lowercase().chars().next().expect("one char"));
    } else {
        out.push_str(key);
    }
    out
}

/// Convert one modifier to the shortcuts spec form. Convert an unknown
/// modifier to uppercase without other changes. The portal can then report
/// the user's error.
fn spec_modifier(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "SUPER" | "META" | "MOD4" | "LOGO" => "LOGO".to_string(),
        "CONTROL" | "CTRL" => "CTRL".to_string(),
        _ => upper,
    }
}

/// The action that one portal signal causes in the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Run the same control socket verb as every other rung. One code path
    /// prevents differences between a portal shortcut and
    /// `chibipop ctl <verb>`. Every global action has a verb. A portal signal
    /// therefore produces a verb or no action.
    Verb(Verb),
    /// No action is necessary.
    Nothing,
}

/// Map one `Activated`/`Deactivated` signal to the daemon's action.
pub fn action(id: ShortcutId, activated: bool) -> Action {
    match (id, activated) {
        // A hold needs both events. Both rungs send the press and release.
        (ShortcutId::Trigger, true) => Action::Verb(Verb::TriggerDown),
        (ShortcutId::Trigger, false) => Action::Verb(Verb::TriggerUp),
        (ShortcutId::AnkiAdd, true) => Action::Verb(Verb::AnkiAdd),
        // A release cannot reverse an Anki add action.
        (ShortcutId::AnkiAdd, false) => Action::Nothing,
    }
}

/// Return the trigger detail while the portal has not answered the
/// binding request. On KDE, the user reads the dialog during this period.
pub fn pending_detail() -> String {
    "GlobalShortcuts portal - binding requested; control socket serving meanwhile".to_string()
}

/// Return the trigger detail after the portal answers. Name each binding.
pub fn portal_detail(bindings: &[Binding]) -> String {
    if bindings.is_empty() {
        return "GlobalShortcuts portal bound nothing - control socket is the only trigger"
            .to_string();
    }
    let described: Vec<String> = bindings.iter().map(Binding::describe).collect();
    format!("GlobalShortcuts portal - {}", described.join(", "))
}

/// Return the trigger detail when the portal rung cannot serve. The
/// control socket still serves, and `why` gives an action.
///
/// A status row has one short line
/// (ARCHITECTURE.md#platform-integration). It names verbs instead of the
/// binary. A bare `chibipop` command does not resolve under `cargo run`.
/// The settings window gives binding snippets that name the binary.
pub fn native_detail(why: &str) -> String {
    format!("control socket (`ctl trigger-down`) - {why}")
}

/// Format the native rung reason for a status row.
pub fn native_reason(reason: NativeReason) -> String {
    match reason {
        NativeReason::NoPortal => {
            format!("no {} on this session", portal::SHORTCUTS_INTERFACE)
        }
        NativeReason::Forced => format!("{}=native", ChannelOverride::ENV),
        NativeReason::ForcedButAbsent => {
            format!("{}=portal asked for, interface absent", ChannelOverride::ENV)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test fixes the consent request at two ids. A new variant requires
    /// a deliberate test change.
    #[test]
    fn exactly_two_ids_exist_and_they_round_trip() {
        assert_eq!(2, ShortcutId::ALL.len());
        assert_eq!(["trigger", "anki-add"], ShortcutId::ALL.map(ShortcutId::as_str));
        for id in ShortcutId::ALL {
            assert_eq!(Some(id), ShortcutId::parse(id.as_str()));
            assert!(!id.description().is_empty(), "{id:?} needs dialog text");
        }
        assert_eq!(None, ShortcutId::parse("toggle"), "no third id is recognised");
        assert_eq!(None, ShortcutId::parse(""));
    }

    /// The configuration supplies both shortcut chords. The fixed result
    /// contains exactly two ids.
    #[test]
    fn the_registered_set_is_the_config_chords_and_only_two() {
        let mut cfg = chibipop::config::Config::default();
        cfg.trigger.trigger_key_linux = "ALT+F".to_string();
        cfg.anki.add_key_linux = "SUPER+A".to_string();
        let asked = preferred(&cfg);
        assert_eq!(
            [(ShortcutId::Trigger, "ALT+f".to_string()), (ShortcutId::AnkiAdd, "LOGO+a".to_string())],
            asked
        );
    }

    /// This test checks the shortcuts spec form instead of the user's form.
    #[test]
    fn a_chord_is_spelled_the_way_the_spec_wants() {
        assert_eq!("ALT+f", normalize_trigger("ALT+F"));
        assert_eq!("ALT+f", normalize_trigger("alt + f"));
        assert_eq!("CTRL+SHIFT+k", normalize_trigger("Ctrl+Shift+K"));
        assert_eq!("LOGO+j", normalize_trigger("SUPER+J"));
        // Multi-character keysym names keep their case.
        assert_eq!("CTRL+ALT+Return", normalize_trigger("ctrl+alt+Return"));
        assert_eq!("ALT+F1", normalize_trigger("alt+F1"));
        assert_eq!("ALT+space", normalize_trigger("ALT+space"));
        // A bare key reaches the portal without a modifier. The portal
        // can refuse it and report the user's invalid value.
        assert_eq!("f", normalize_trigger("F"));
        assert_eq!("", normalize_trigger(""));
    }

    /// This test covers every ladder selection.
    #[test]
    fn the_ladder_prefers_the_portal_and_falls_back_to_the_socket() {
        assert_eq!(Selection::Portal, select(true, ChannelOverride::Auto));
        assert_eq!(
            Selection::Native(NativeReason::NoPortal),
            select(false, ChannelOverride::Auto)
        );
        assert_eq!(Selection::Portal, select(true, ChannelOverride::Portal));
        assert_eq!(
            Selection::Native(NativeReason::ForcedButAbsent),
            select(false, ChannelOverride::Portal)
        );
        assert_eq!(Selection::Native(NativeReason::Forced), select(true, ChannelOverride::Native));
        assert_eq!(Selection::Native(NativeReason::Forced), select(false, ChannelOverride::Native));
    }

    /// Each startup line names the active trigger source. Native lines name
    /// the control socket, so the user knows that the trigger works.
    /// The rung-2 line also gives binding instructions and names the active
    /// binary. A bare command name can be absent from PATH.
    #[test]
    fn every_startup_line_names_what_serves_the_trigger() {
        let exe = Path::new("/home/u/chibipop/target/debug/chibipop");
        assert!(Selection::Portal.startup_line(exe).contains("GlobalShortcuts"));
        assert!(Selection::Portal.startup_line(exe).contains("control socket"));
        for reason in
            [NativeReason::NoPortal, NativeReason::Forced, NativeReason::ForcedButAbsent]
        {
            let line = Selection::Native(reason).startup_line(exe);
            assert!(line.contains("control socket"), "{line}");
            assert!(!line.contains('\n'), "{line}");
        }

        let advice = Selection::Native(NativeReason::NoPortal).startup_line(exe);
        assert!(
            advice.contains("bind `/home/u/chibipop/target/debug/chibipop ctl trigger-down"),
            "{advice}"
        );
        let spaced = Selection::Native(NativeReason::NoPortal)
            .startup_line(Path::new("/home/u/my builds/chibipop"));
        assert!(spaced.contains("bind `'/home/u/my builds/chibipop' ctl"), "{spaced}");
    }

    #[test]
    fn the_override_reads_three_words_and_complains_about_anything_else() {
        assert_eq!(Some(ChannelOverride::Auto), ChannelOverride::parse("auto"));
        assert_eq!(Some(ChannelOverride::Portal), ChannelOverride::parse("portal"));
        assert_eq!(Some(ChannelOverride::Native), ChannelOverride::parse("native"));
        assert_eq!(None, ChannelOverride::parse("Portal"));
        assert_eq!(None, ChannelOverride::parse("evdev"));
    }

    /// A hold needs a press and a release. Without the release, the frozen grab
    /// remains active.
    #[test]
    fn the_trigger_id_maps_to_both_halves_of_the_hold() {
        assert_eq!(Action::Verb(Verb::TriggerDown), action(ShortcutId::Trigger, true));
        assert_eq!(Action::Verb(Verb::TriggerUp), action(ShortcutId::Trigger, false));
    }

    /// Anki adds only on a press. The release does not create a second
    /// event. The press maps to the verb. A portal press and
    /// `chibipop ctl anki-add` therefore use the same code path.
    #[test]
    fn the_add_id_adds_once_per_press() {
        assert_eq!(Action::Verb(Verb::AnkiAdd), action(ShortcutId::AnkiAdd, true));
        assert_eq!(Action::Nothing, action(ShortcutId::AnkiAdd, false));
    }

    /// A status row names the channel that owns the binding. A portal row can
    /// also show that the portal did not report a key.
    #[test]
    fn status_details_name_the_owner_of_the_binding() {
        let named = vec![
            Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
            Binding { id: ShortcutId::AnkiAdd, trigger: Some("Alt+A".into()) },
        ];
        let detail = portal_detail(&named);
        assert!(detail.contains("GlobalShortcuts portal"), "{detail}");
        assert!(detail.contains("trigger Alt+F"), "{detail}");
        assert!(detail.contains("anki-add Alt+A"), "{detail}");

        let unnamed = vec![Binding { id: ShortcutId::Trigger, trigger: None }];
        assert!(portal_detail(&unnamed).contains("key not reported"));
        assert!(portal_detail(&[]).contains("bound nothing"));

        let native = native_detail(&native_reason(NativeReason::NoPortal));
        assert!(native.contains("control socket"), "{native}");
        assert!(native.contains(portal::SHORTCUTS_INTERFACE), "{native}");
    }
}

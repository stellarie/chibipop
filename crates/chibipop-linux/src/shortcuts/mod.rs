//! The trigger channel's ladder (ARCHITECTURE.md#input-ladders): the
//! GlobalShortcuts portal rung, the ids it may ever register, and how
//! one portal signal turns into the same effect a control-socket verb
//! has.
//!
//! **The native rung never stops working.** The ladder lists the portal
//! first and the compositor-keybind-into-control-socket rung second, but
//! that order is about *who the product asks to bind a key*, not about
//! transport exclusivity: `chibipop ctl trigger-down|trigger-up|toggle`
//! is bound at startup on every session and keeps answering whatever the
//! portal does. So the portal is an *additional* source of the same
//! press/release, and a portal that is absent, refuses, or has no key
//! assigned to it degrades to "the socket is the only source" — which is
//! a working product, never a dead trigger. That is also why the trigger
//! row is never `Down`: the channel is up as long as the daemon runs.
//!
//! **Exactly two ids, forever** ([`ShortcutId`]): `trigger` and
//! `anki-add`. The consent dialog is a list of everything an app claims
//! from the user's keyboard, and a long list is a dialog people dismiss.
//! The set is an enum rather than a config-driven list so "only the two
//! shortcut ids are ever registered" is a compile-time property with a
//! test on top, not a review habit.
//!
//! **Two facts about the portal that shape everything here**, both read
//! off this machine rather than remembered:
//!
//! * `org.freedesktop.portal.GlobalShortcuts` (interface version 2, per
//!   `/usr/share/dbus-1/interfaces/org.freedesktop.portal.GlobalShortcuts.xml`)
//!   has **no restore token and no persist mode** — unlike ScreenCast,
//!   whose token this tree does store (`capture/portal/token.rs`). There
//!   is nothing to persist across launches: `BindShortcuts` is called
//!   once per session, and the portal's own memory is what makes the
//!   second launch quiet. `ListShortcuts` is the read-back — "if
//!   `BindShortcuts` was called for `session` all active shortcuts for
//!   `session` are returned. Otherwise returns the shortcuts that were
//!   successfully bound in a previous session by this application."
//! * The trigger is a **chord, never a bare modifier**: the shortcuts
//!   spec (freedesktop, draft 0.1) defines a shortcut as XKB modifier
//!   names (`CTRL`, `ALT`, `SHIFT`, `NUM`, `LOGO`) plus one keysym
//!   identifier from `xkbcommon-keysyms.h` minus the `XKB_KEY_` prefix,
//!   joined by `+`, limited to the base layer. Hence the Linux default
//!   `ALT+F`, and hence [`normalize_trigger`], which spells a user's
//!   chord the way that spec wants before it goes on the wire.

pub mod portal;
pub mod state;

use crate::control::Verb;
use std::path::Path;

/// Every shortcut chibipop will ever register, and nothing else
/// (keep the consent dialog small - ARCHITECTURE.md#input-ladders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutId {
    /// Hold to read: press freezes and looks up, release retracts. The
    /// portal delivers both halves (`Activated`/`Deactivated`), which is
    /// what makes a *hold* expressible without observing the keyboard.
    Trigger,
    /// The Anki add affordance's keyboard path — the one contextual
    /// interaction that keeps a global key on Wayland, since the popup
    /// itself never takes focus.
    AnkiAdd,
}

impl ShortcutId {
    /// The whole set, in registration order. Fixed-size: the id set is
    /// the app's shape, not data.
    pub const ALL: [ShortcutId; 2] = [ShortcutId::Trigger, ShortcutId::AnkiAdd];

    /// The id on the wire. Stable forever: a compositor keybind names it
    /// (Hyprland `bind = ALT, F, global, chibipop:trigger`), so renaming
    /// one would silently break every user's config.
    pub fn as_str(self) -> &'static str {
        match self {
            ShortcutId::Trigger => "trigger",
            ShortcutId::AnkiAdd => "anki-add",
        }
    }

    /// The id back, or `None` for anything else — which is what a
    /// signal for a foreign session or a stale registration looks like.
    pub fn parse(id: &str) -> Option<ShortcutId> {
        ShortcutId::ALL.into_iter().find(|known| known.as_str() == id)
    }

    /// The `description` the portal shows the user. It is the entire
    /// explanation they get for a system-wide key grab, so it says what
    /// the key does, not what the program is.
    pub fn description(self) -> &'static str {
        match self {
            ShortcutId::Trigger => "Hold to look up the Japanese text under the cursor",
            ShortcutId::AnkiAdd => "Add the word shown in the popup to Anki",
        }
    }
}

/// One shortcut as the portal reports it back (`BindShortcuts` results
/// and `ListShortcuts`).
///
/// `trigger` is the portal's `trigger_description` — *its* spelling of
/// the key, for display only, and `None` when the implementation does
/// not report one. xdg-desktop-portal-hyprland is exactly that case: it
/// answers `trigger_description: ""` because the key lives in the
/// user's Hyprland config, not in the portal. So "bound" and "we can
/// name the key" are two different facts and are stored as such.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: ShortcutId,
    pub trigger: Option<String>,
}

impl Binding {
    /// How one binding reads in a status row: the key when the portal
    /// names it, and honesty when it does not.
    pub fn describe(&self) -> String {
        match &self.trigger {
            Some(trigger) => format!("{} {trigger}", self.id.as_str()),
            None => format!("{} (key not reported)", self.id.as_str()),
        }
    }
}

/// What the portal thread tells the pump. Everything the D-Bus session
/// learns arrives here; the thread owns no log and no tray
/// (ARCHITECTURE.md#workspace-and-seams — the pump stays sync and
/// single-threaded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `BindShortcuts` succeeded; the payload is what the portal says is
    /// bound now. An empty list is a legitimate answer: the user may
    /// approve none of them.
    Bound(Vec<Binding>),
    /// `ShortcutsChanged`: the user re-bound something in the desktop's
    /// own UI. Same payload, different line in the log.
    Changed(Vec<Binding>),
    /// A shortcut fired. `true` is `Activated`, `false` `Deactivated`.
    Fired { id: ShortcutId, activated: bool },
    /// The rung is not serving. `reason` is short enough for a status
    /// row; `advice` is the longer "and here is what to do", for the
    /// log, when there is something to do. The control socket carries
    /// the trigger from here either way.
    Unavailable { reason: String, advice: Option<String> },
    /// A diagnostic from the portal thread, written by the pump.
    Note(String),
}

/// The `CHIBIPOP_TRIGGER_CHANNEL` test hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOverride {
    /// Walk the ladder in its documented order.
    Auto,
    /// Take the portal rung when the interface is there, and say so
    /// loudly when it is not — the documented way to test the rung on a
    /// session where it would be picked anyway.
    Portal,
    /// Skip the portal entirely: the control socket is the only trigger
    /// source, which is what a sway/wlr session looks like on a box that
    /// happens to run a portal.
    Native,
}

impl ChannelOverride {
    pub const ENV: &'static str = "CHIBIPOP_TRIGGER_CHANNEL";

    /// One of `auto|portal|native`, or `None` for anything else.
    pub fn parse(value: &str) -> Option<ChannelOverride> {
        match value {
            "auto" => Some(ChannelOverride::Auto),
            "portal" => Some(ChannelOverride::Portal),
            "native" => Some(ChannelOverride::Native),
            _ => None,
        }
    }

    /// The override and, when the value was unrecognized, a diagnostic.
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

/// Why the native rung is the one asking the user to bind a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeReason {
    /// No `org.freedesktop.portal.GlobalShortcuts` on the session bus —
    /// sway, generic wlr, GNOME below the supported floor.
    NoPortal,
    /// `CHIBIPOP_TRIGGER_CHANNEL=native`.
    Forced,
    /// `CHIBIPOP_TRIGGER_CHANNEL=portal` on a session with no such
    /// interface: honour the ladder, but never silently.
    ForcedButAbsent,
}

/// Which rung asks for the binding. The socket serves either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Rung 1: register the two ids with the portal, and keep the socket.
    Portal,
    /// Rung 2 only: the compositor keybind into the control socket.
    Native(NativeReason),
}

impl Selection {
    /// The one startup line the daemon logs for the trigger channel.
    ///
    /// `exe` is the binary the native rung's advice must name, resolved
    /// by the caller (`paths::exec_name`): the line tells the user what
    /// to bind, and under `cargo run` the bare command name is not on
    /// PATH, so a snippet built from it execs nothing (ticket 51).
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

/// Walk the trigger ladder (ARCHITECTURE.md#input-ladders). `portal`
/// is the D-Bus probe the caller already ran.
pub fn select(portal: bool, ov: ChannelOverride) -> Selection {
    match (ov, portal) {
        (ChannelOverride::Auto | ChannelOverride::Portal, true) => Selection::Portal,
        (ChannelOverride::Auto, false) => Selection::Native(NativeReason::NoPortal),
        (ChannelOverride::Portal, false) => Selection::Native(NativeReason::ForcedButAbsent),
        (ChannelOverride::Native, _) => Selection::Native(NativeReason::Forced),
    }
}

/// The two shortcuts to register, with the chords the config asks for,
/// spelled the way the shortcuts spec wants them.
///
/// A fixed-size array, so "exactly two ids" is the type.
pub fn preferred(config: &chibipop::config::Config) -> [(ShortcutId, String); 2] {
    [
        (ShortcutId::Trigger, normalize_trigger(&config.trigger.trigger_key_linux)),
        (ShortcutId::AnkiAdd, normalize_trigger(&config.anki.add_key_linux)),
    ]
}

/// A user's chord as the shortcuts spec spells it: XKB modifier names
/// upper-case, the key an `xkbcommon-keysyms.h` identifier.
///
/// Two real corrections, not cosmetics. `SUPER` is what users type and
/// what every compositor config calls that key, but the spec's name for
/// it is `LOGO` — the portal is entitled to reject the other spelling.
/// And a single letter is lower-cased, because `F` is the keysym
/// `XKB_KEY_F` (i.e. shifted) while the spec asks for the base layer:
/// the `ALT+F` default means Alt plus the F *key*. Longer identifiers
/// (`Return`, `F1`, `space`) are passed through verbatim: they are
/// keysym names already, and case is part of the name.
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

/// One modifier, spec-spelled. Anything unrecognized is upper-cased and
/// passed on: an unknown modifier is the user's business, and mangling
/// it further would only hide their typo.
fn spec_modifier(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "SUPER" | "META" | "MOD4" | "LOGO" => "LOGO".to_string(),
        "CONTROL" | "CTRL" => "CTRL".to_string(),
        _ => upper,
    }
}

/// What one portal signal does to the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Exactly what the control socket's verb does — one code path for
    /// every rung, so a portal press and a `chibipop ctl <verb>` cannot
    /// drift apart. There is no second kind of effect on purpose: every
    /// global action has a verb, so a portal signal is only ever a verb
    /// or nothing.
    Verb(Verb),
    /// Nothing to do.
    Nothing,
}

/// One `Activated`/`Deactivated` mapped onto the daemon's vocabulary.
pub fn action(id: ShortcutId, activated: bool) -> Action {
    match (id, activated) {
        // The hold, both halves: press *and* release arrive on both
        // rungs.
        (ShortcutId::Trigger, true) => Action::Verb(Verb::TriggerDown),
        (ShortcutId::Trigger, false) => Action::Verb(Verb::TriggerUp),
        (ShortcutId::AnkiAdd, true) => Action::Verb(Verb::AnkiAdd),
        // Releasing the add key cannot un-add a card.
        (ShortcutId::AnkiAdd, false) => Action::Nothing,
    }
}

/// The trigger row while the portal has been asked but has not answered.
/// On KDE that wait is a dialog the user is reading.
pub fn pending_detail() -> String {
    "GlobalShortcuts portal - binding requested; control socket serving meanwhile".to_string()
}

/// The trigger row once the portal answered, naming what it bound.
pub fn portal_detail(bindings: &[Binding]) -> String {
    if bindings.is_empty() {
        return "GlobalShortcuts portal bound nothing - control socket is the only trigger"
            .to_string();
    }
    let described: Vec<String> = bindings.iter().map(Binding::describe).collect();
    format!("GlobalShortcuts portal - {}", described.join(", "))
}

/// The trigger row when the portal rung is not serving: the socket is,
/// and `why` is the part a user can act on.
///
/// Names the verbs, not the binary: a status row has one short line
/// (ARCHITECTURE.md#platform-integration), and a bare `chibipop` in it
/// would be a command that does not resolve under `cargo run`
/// (ticket 51). The bind lines that do name the running exe are the
/// settings window's snippet.
pub fn native_detail(why: &str) -> String {
    format!("control socket (`ctl trigger-down`) - {why}")
}

/// The native rung's own reason, phrased for a status row.
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

    /// The whole consent argument: two ids, no more, ever. A new
    /// variant has to break this test on purpose.
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

    /// What gets registered comes from the config and is still exactly
    /// the two ids, whatever the user typed.
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

    /// The shortcuts spec's spelling, not the user's.
    #[test]
    fn a_chord_is_spelled_the_way_the_spec_wants() {
        assert_eq!("ALT+f", normalize_trigger("ALT+F"));
        assert_eq!("ALT+f", normalize_trigger("alt + f"));
        assert_eq!("CTRL+SHIFT+k", normalize_trigger("Ctrl+Shift+K"));
        assert_eq!("LOGO+j", normalize_trigger("SUPER+J"));
        // Multi-character keysym names are names: case is theirs.
        assert_eq!("CTRL+ALT+Return", normalize_trigger("ctrl+alt+Return"));
        assert_eq!("ALT+F1", normalize_trigger("alt+F1"));
        assert_eq!("ALT+space", normalize_trigger("ALT+space"));
        // A bare key is passed through: the portal will refuse it, and
        // that refusal is the honest answer to a user who insisted.
        assert_eq!("f", normalize_trigger("F"));
        assert_eq!("", normalize_trigger(""));
    }

    /// The ladder, whole truth table.
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

    /// Every rung's startup line names the mechanism, and the native
    /// ones name the socket so a reader knows the trigger still works.
    /// The rung-2 line is also the user's instruction, so it must name
    /// the running binary rather than a bare command name that PATH may
    /// not have (ticket 51).
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

    /// The hold is the pair: a press without its release would leave a
    /// frozen grab up forever.
    #[test]
    fn the_trigger_id_maps_to_both_halves_of_the_hold() {
        assert_eq!(Action::Verb(Verb::TriggerDown), action(ShortcutId::Trigger, true));
        assert_eq!(Action::Verb(Verb::TriggerUp), action(ShortcutId::Trigger, false));
    }

    /// Anki adds on press only, and the release is not a second event.
    /// The press resolves to the *verb*, which is what makes a portal
    /// press and `chibipop ctl anki-add` the same code path.
    #[test]
    fn the_add_id_adds_once_per_press() {
        assert_eq!(Action::Verb(Verb::AnkiAdd), action(ShortcutId::AnkiAdd, true));
        assert_eq!(Action::Nothing, action(ShortcutId::AnkiAdd, false));
    }

    /// A status row has to say which channel owns the binding, and the
    /// portal rows have to survive a portal that reports no key.
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

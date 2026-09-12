//! The trigger channel has two rungs (ARCHITECTURE.md#input-ladders).
//! The GlobalShortcuts portal provides the first rung.
//! A compositor keybind to the control socket provides the second rung.
//! Both rungs convert shortcut events to the same control-socket verb.
//!
//! **The native rung always works.** The order selects the rung that asks
//! the user for a binding. It does not disable the other transport.
//! Each session binds configured global actions. The control socket accepts
//! requests when the portal is active. The portal then supplies another source
//! for the same press and release events. If the portal is absent or refuses the
//! request, the control socket remains the only source.
//!
//! The portal can assign keys directly on desktops such as KDE and GNOME.
//! Hyprland advertises the portal but stores its keys in compositor config, so
//! automatic Hyprland sessions use native binds instead.
//!
//! **Portal interface facts.** The local interface XML confirms these facts:
//!
//! * `org.freedesktop.portal.GlobalShortcuts` interface version 2 has no
//!   restore token or persist mode. `BindShortcuts` runs once for each portal
//!   session. `ListShortcuts` returns active shortcuts after `BindShortcuts`.
//!   Without that call, it returns shortcuts that a previous portal session
//!   bound for this application. The interface definition is
//!   `/usr/share/dbus-1/interfaces/org.freedesktop.portal.GlobalShortcuts.xml`.
//! * A trigger is a chord, not a bare modifier. The shortcuts spec draft 0.1
//!   uses XKB modifier names (`CTRL`, `ALT`, `SHIFT`, `NUM`, `LOGO`). Each chord
//!   also contains one keysym from `xkbcommon-keysyms.h` without the `XKB_KEY_`
//!   prefix. Use `+` between parts, and use only the base layer.
//!   [`normalize_trigger`] converts a user's chord to the required form.
//!
//! **An app id is mandatory.** xdg-desktop-portal rejects `CreateSession`
//! with `NotAllowed`/"An app id is required" when it cannot name the caller.
//! For a non-sandboxed process, it derives the name from the systemd user
//! unit (`app[-<launcher>]-<ApplicationID>-<RANDOM>.scope|.slice|.service`)
//! and requires a matching `<ApplicationID>.desktop` file. A daemon started
//! from a shell has neither. This rung is unreachable, even with a new
//! portal. [`explain`] reports this condition. The user can launch from the
//! desktop entry or autostart unit. The control socket carries every configured
//! action in the meantime.
//!

pub mod portal;
pub mod state;

use crate::control::Verb;
use chibipop::config::TriggerMode;
use std::path::Path;

/// Every configured global action has one stable portal identifier. The
/// registration list is filtered from this set by [`preferred`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutId {
    /// Hold this shortcut to read text. A press freezes a grab and starts a
    /// lookup. A release retracts the popup. The portal sends both
    /// `Activated`/`Deactivated` events, so the daemon needs no keyboard
    /// access.
    Trigger,
    /// Start the Anki add action with this shortcut. The popup never takes
    /// focus, so this action needs a global shortcut on Wayland.
    AnkiAdd,
    /// Open dictionary search.
    Search,
    /// Open sentence search.
    SentenceSearch,
    /// Read the application's PRIMARY selection.
    SelectedText,
    /// OCR a selected region onto the clipboard.
    OcrClipboard,
    /// Select the static sentence region.
    StaticRegion,
}

impl ShortcutId {
    /// The complete set in the order that the daemon registers it. The
    /// fixed-size array makes the identifier set part of the application.
    pub const ALL: [ShortcutId; 7] = [
        ShortcutId::Trigger,
        ShortcutId::AnkiAdd,
        ShortcutId::Search,
        ShortcutId::SentenceSearch,
        ShortcutId::SelectedText,
        ShortcutId::OcrClipboard,
        ShortcutId::StaticRegion,
    ];

    /// This function returns the stable identifier on the wire. Hyprland prefixes
    /// this value with the portal app ID. The ID can depend on the process that
    /// starts the daemon. A rename breaks portal registrations without an error.
    pub fn as_str(self) -> &'static str {
        match self {
            ShortcutId::Trigger => "trigger",
            ShortcutId::AnkiAdd => "anki-add",
            ShortcutId::Search => "search",
            ShortcutId::SentenceSearch => "sentence-search",
            ShortcutId::SelectedText => "selected-text",
            ShortcutId::OcrClipboard => "ocr-clipboard",
            ShortcutId::StaticRegion => "static-region",
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
            ShortcutId::Search => "Open dictionary search",
            ShortcutId::SentenceSearch => "Open sentence search",
            ShortcutId::SelectedText => "Look up selected application text",
            ShortcutId::OcrClipboard => "Copy OCR text from a selected region",
            ShortcutId::StaticRegion => "Select the static sentence region",
        }
    }
}

/// One shortcut binding reported by the portal.
///
/// `trigger` can be absent when the portal confirms an id but does not return
/// a human-readable key. Keep the id so settings can distinguish that case
/// from an unregistered action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: ShortcutId,
    pub trigger: Option<String>,
}

impl Binding {
    /// Format one binding for a status row. Show the reported key or state
    /// that the portal did not report it, so a row without a key does not
    /// look like a missing registration.
    pub fn describe(&self) -> String {
        match &self.trigger {
            Some(trigger) => format!("{} {trigger}", self.id.as_str()),
            None => format!("{} (key not reported)", self.id.as_str()),
        }
    }
}

/// A portal session identifier. A replacement session gets a new value, so
/// queued events from an older session cannot reach the current configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Messages from the portal thread to the calloop pump. The thread sends
/// each D-Bus result here and does not own the log or tray
/// (ARCHITECTURE.md#workspace-and-seams). The calloop pump stays
/// synchronous and single-threaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `BindShortcuts` succeeded for this session.
    Bound { session: SessionId, bindings: Vec<Binding> },
    /// The desktop UI reports a binding change through `ShortcutsChanged`.
    Changed { session: SessionId, bindings: Vec<Binding> },
    /// A shortcut fired. `true` means `Activated`; `false` means `Deactivated`.
    Fired { session: SessionId, id: ShortcutId, activated: bool },
    /// The portal rung cannot serve shortcuts.
    Unavailable { session: SessionId, reason: String, advice: Option<String> },
    /// A diagnostic that the calloop pump writes.
    Note { session: SessionId, line: String },
}

/// A session that can be stopped when configuration changes.
///
/// The handle drops the setup signal queue before it closes the connection.
/// A full queue must not block a pending reply while stop joins the thread.
/// The handle joins the worker only after it closes a published connection.
/// See [`SessionHandle::stop`] for the handshake case.
pub struct SessionHandle {
    id: SessionId,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    connection: std::sync::Arc<std::sync::Mutex<ConnectionState>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Keep the setup queue reachable while the worker waits for portal replies.
/// Cancellation drops its stream without a synchronous RemoveMatch round trip.
#[derive(Default)]
pub(crate) struct ConnectionState {
    connection: Option<zbus::blocking::Connection>,
    setup_signals: Option<zbus::blocking::MessageIterator>,
}

impl SessionHandle {
    pub(crate) fn new(
        id: SessionId,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        connection: std::sync::Arc<std::sync::Mutex<ConnectionState>>,
        thread: std::thread::JoinHandle<()>,
    ) -> Self {
        Self { id, cancel, connection, thread: Some(thread) }
    }

    /// Return the daemon's generation for this session.
    ///
    /// The daemon compares this value with the tag on each event. A retired
    /// session's queued events therefore cannot act on the current configuration.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Stop the session. Join the thread only when the worker holds a connection.
    ///
    /// A closed connection fails every pending portal call, so that join returns
    /// at once. A worker without a published connection is still inside the bus
    /// handshake. zbus gives that handshake no timeout, so a join there would
    /// block the calloop pump on a stalled bus. That worker reads `cancel` after
    /// it connects and exits on its own, so the handle does not wait for it.
    pub fn stop(&mut self) {
        use std::sync::atomic::Ordering;
        self.cancel.store(true, Ordering::Release);
        let state = self.connection.lock().ok().map(|mut slot| {
            (slot.setup_signals.take(), slot.connection.take())
        });
        let Some((signals, Some(connection))) = state else {
            return;
        };
        drop(signals.map(zbus::blocking::MessageIterator::into_inner));
        let _ = connection.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.stop();
    }
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
    /// The session bus lacks `org.freedesktop.portal.GlobalShortcuts`.
    NoPortal,
    /// The user set `CHIBIPOP_TRIGGER_CHANNEL=native`.
    Forced,
    /// The user set `CHIBIPOP_TRIGGER_CHANNEL=portal`, but the session has no
    /// interface. The daemon reports this condition.
    ForcedButAbsent,
    /// Automatic Hyprland operation uses the compositor's native config syntax.
    /// The portal may advertise the interface, but it cannot assign the
    /// configured key without a Hyprland config edit.
    Hyprland,
}

/// The rung that requests a binding. The control socket serves both selections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Rung 1 registers configured shortcuts with the portal and keeps the socket.
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
                "trigger: {} portal (ladder rung 1) - registering configured shortcuts; the control socket keeps serving too",
                portal::SHORTCUTS_INTERFACE
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
            Selection::Native(NativeReason::Hyprland) => format!(
                "trigger: control socket only - automatic Hyprland mode uses native keybinds; add `bind = <KEY>, exec, {} ctl <verb>` in hyprland.conf",
                crate::paths::shell_quote(exe)
            ),
        }
    }
}

/// Select the trigger rung (ARCHITECTURE.md#input-ladders). The `portal`
/// argument is the result of the caller's D-Bus probe. Automatic Hyprland
/// operation always uses its native config syntax; an explicit portal
/// override still selects the portal.
pub fn select(portal: bool, ov: ChannelOverride, hyprland: bool) -> Selection {
    match ov {
        ChannelOverride::Portal if portal => Selection::Portal,
        ChannelOverride::Portal => Selection::Native(NativeReason::ForcedButAbsent),
        ChannelOverride::Native => Selection::Native(NativeReason::Forced),
        ChannelOverride::Auto if hyprland => Selection::Native(NativeReason::Hyprland),
        ChannelOverride::Auto if portal => Selection::Portal,
        ChannelOverride::Auto => Selection::Native(NativeReason::NoPortal),
    }
}

/// Build every configured shortcut whose action is enabled and whose chord is
/// non-empty. Each chord uses the form that the shortcuts spec defines.
pub fn preferred(config: &chibipop::config::Config) -> Vec<(ShortcutId, String)> {
    let mut shortcuts = Vec::new();
    let push = |shortcuts: &mut Vec<(ShortcutId, String)>, id, key: &str| {
        if key.trim().is_empty() {
            return;
        }
        let chord = normalize_trigger(key);
        if !chord.is_empty() {
            shortcuts.push((id, chord));
        }
    };

    push(&mut shortcuts, ShortcutId::Trigger, &config.trigger.trigger_key_linux);
    if config.anki.enabled {
        push(&mut shortcuts, ShortcutId::AnkiAdd, &config.anki.add_key_linux);
    }
    if config.actions.enabled {
        if let Some(key) = config.actions.search.hotkey_linux.as_deref() {
            push(&mut shortcuts, ShortcutId::Search, key);
        }
        if let Some(key) = config.actions.search.sentence_hotkey_linux.as_deref() {
            push(&mut shortcuts, ShortcutId::SentenceSearch, key);
        }
        if let Some(key) = config.actions.search.selected_hotkey_linux.as_deref() {
            push(&mut shortcuts, ShortcutId::SelectedText, key);
        }
        if let Some(ocr) = config.actions.ocr_clipboard.as_ref() {
            if let Some(key) = ocr.hotkey_linux.as_deref() {
                push(&mut shortcuts, ShortcutId::OcrClipboard, key);
            }
        }
        if config.anki.sentence_mode == chibipop::config::SentenceMode::Static {
            push(&mut shortcuts, ShortcutId::StaticRegion, &config.anki.static_region_key_linux);
        }
    }
    shortcuts
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

/// Select the verb for one portal signal.
///
/// The mode selects the verb that the trigger key sends. This keeps a portal
/// press and a native bind on one `App::apply_verb` path
/// (ARCHITECTURE.md#input-ladders). Toggle mode uses the existing `toggle`
/// verb, so the daemon's latch logic in `trigger.rs` remains the single owner
/// of the hold. Press mode sends one `lookup` verb per press and ignores the
/// release.
pub fn action(id: ShortcutId, activated: bool, mode: TriggerMode) -> Action {
    match (id, activated, mode) {
        (ShortcutId::Trigger, true, TriggerMode::Toggle) => Action::Verb(Verb::Toggle),
        // A release cannot reverse a toggle latch.
        (ShortcutId::Trigger, false, TriggerMode::Toggle) => Action::Nothing,
        (ShortcutId::Trigger, true, TriggerMode::Press) => Action::Verb(Verb::Lookup),
        // Press mode performs one lookup and ignores the release.
        (ShortcutId::Trigger, false, TriggerMode::Press) => Action::Nothing,
        // Every other trigger mode needs both events.
        (ShortcutId::Trigger, true, _) => Action::Verb(Verb::TriggerDown),
        (ShortcutId::Trigger, false, _) => Action::Verb(Verb::TriggerUp),
        (ShortcutId::AnkiAdd, true, _) => Action::Verb(Verb::AnkiAdd),
        // A release cannot reverse an Anki add action.
        (ShortcutId::AnkiAdd, false, _) => Action::Nothing,
        (ShortcutId::Search, true, _) => Action::Verb(Verb::Search),
        (ShortcutId::Search, false, _) => Action::Nothing,
        (ShortcutId::SentenceSearch, true, _) => Action::Verb(Verb::SentenceSearch),
        (ShortcutId::SentenceSearch, false, _) => Action::Nothing,
        (ShortcutId::SelectedText, true, _) => Action::Verb(Verb::SelectedText),
        (ShortcutId::SelectedText, false, _) => Action::Nothing,
        (ShortcutId::OcrClipboard, true, _) => Action::Verb(Verb::OcrClipboard),
        (ShortcutId::OcrClipboard, false, _) => Action::Nothing,
        (ShortcutId::StaticRegion, true, _) => Action::Verb(Verb::StaticRegion),
        (ShortcutId::StaticRegion, false, _) => Action::Nothing,
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
        NativeReason::Hyprland => {
            "automatic Hyprland mode uses native keybinds".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_ids_round_trip_and_keep_wire_names() {
        assert_eq!(7, ShortcutId::ALL.len());
        assert_eq!(
            [
                "trigger",
                "anki-add",
                "search",
                "sentence-search",
                "selected-text",
                "ocr-clipboard",
                "static-region",
            ],
            ShortcutId::ALL.map(ShortcutId::as_str)
        );
        for id in ShortcutId::ALL {
            assert_eq!(Some(id), ShortcutId::parse(id.as_str()));
            assert!(!id.description().is_empty(), "{id:?} needs dialog text");
        }
        assert_eq!(None, ShortcutId::parse("toggle"));
        assert_eq!(None, ShortcutId::parse(""));
    }

    #[test]
    fn preferred_contains_each_enabled_non_empty_linux_chord() {
        let mut config = chibipop::config::Config::default();
        config.trigger.trigger_key_linux = "ALT+F".into();
        config.anki.enabled = true;
        config.anki.sentence_mode = chibipop::config::SentenceMode::Static;
        config.anki.add_key_linux = "SUPER+A".into();
        config.anki.static_region_key_linux = "CTRL+R".into();
        config.actions.search.hotkey_linux = Some("SUPER+F5".into());
        config.actions.search.sentence_hotkey_linux = Some("SUPER+F6".into());
        config.actions.search.selected_hotkey_linux = Some("SUPER+F7".into());
        config.actions.ocr_clipboard = Some(chibipop::config::OcrClipboardConfig {
            hotkey_linux: Some("SUPER+O".into()),
            ..Default::default()
        });
        assert_eq!(
            vec![
                (ShortcutId::Trigger, "ALT+f".into()),
                (ShortcutId::AnkiAdd, "LOGO+a".into()),
                (ShortcutId::Search, "LOGO+F5".into()),
                (ShortcutId::SentenceSearch, "LOGO+F6".into()),
                (ShortcutId::SelectedText, "LOGO+F7".into()),
                (ShortcutId::OcrClipboard, "LOGO+o".into()),
                (ShortcutId::StaticRegion, "CTRL+r".into()),
            ],
            preferred(&config)
        );

        config.anki.enabled = false;
        config.actions.enabled = false;
        let ids: Vec<_> = preferred(&config).into_iter().map(|(id, _)| id).collect();
        assert_eq!(vec![ShortcutId::Trigger], ids);
    }

    #[test]
    fn static_region_preference_requires_static_sentence_mode() {
        let mut config = chibipop::config::Config::default();
        config.actions.enabled = true;
        config.anki.static_region_key_linux = "CTRL+R".into();

        assert!(preferred(&config)
            .iter()
            .all(|(id, _)| *id != ShortcutId::StaticRegion));

        config.anki.sentence_mode = chibipop::config::SentenceMode::Static;
        assert_eq!(
            Some(&(ShortcutId::StaticRegion, "CTRL+r".to_string())),
            preferred(&config)
                .iter()
                .find(|(id, _)| *id == ShortcutId::StaticRegion)
        );
    }

    #[test]
    fn an_empty_chord_is_not_registered() {
        let mut config = chibipop::config::Config::default();
        config.trigger.trigger_key_linux.clear();
        config.anki.enabled = true;
        config.anki.add_key_linux.clear();
        config.actions.search.hotkey_linux = Some(" ".into());
        config.anki.static_region_key_linux.clear();
        assert!(preferred(&config).is_empty());
    }

    #[test]
    fn a_chord_is_spelled_the_way_the_spec_wants() {
        assert_eq!("ALT+f", normalize_trigger("ALT+F"));
        assert_eq!("ALT+f", normalize_trigger("alt + f"));
        assert_eq!("CTRL+SHIFT+k", normalize_trigger("Ctrl+Shift+K"));
        assert_eq!("LOGO+j", normalize_trigger("SUPER+J"));
        assert_eq!("CTRL+ALT+Return", normalize_trigger("ctrl+alt+Return"));
        assert_eq!("ALT+F1", normalize_trigger("alt+F1"));
        assert_eq!("ALT+space", normalize_trigger("ALT+space"));
        assert_eq!("f", normalize_trigger("F"));
        assert_eq!("", normalize_trigger(""));
    }

    /// This test covers every ladder selection. The automatic portal case is
    /// the default KDE and GNOME path.
    #[test]
    fn automatic_hyprland_uses_native_binds_but_explicit_portal_wins() {
        assert_eq!(Selection::Portal, select(true, ChannelOverride::Auto, false));
        assert_eq!(
            Selection::Native(NativeReason::Hyprland),
            select(true, ChannelOverride::Auto, true)
        );
        assert_eq!(
            Selection::Portal,
            select(true, ChannelOverride::Portal, true)
        );
        assert_eq!(
            Selection::Native(NativeReason::NoPortal),
            select(false, ChannelOverride::Auto, false)
        );
        assert_eq!(
            Selection::Native(NativeReason::ForcedButAbsent),
            select(false, ChannelOverride::Portal, true)
        );
        assert_eq!(
            Selection::Native(NativeReason::Forced),
            select(true, ChannelOverride::Native, true)
        );
        assert_eq!(
            Selection::Native(NativeReason::Forced),
            select(false, ChannelOverride::Native, false)
        );
    }

    #[test]
    fn the_override_reads_three_words_and_complains_about_anything_else() {
        assert_eq!(Some(ChannelOverride::Auto), ChannelOverride::parse("auto"));
        assert_eq!(Some(ChannelOverride::Portal), ChannelOverride::parse("portal"));
        assert_eq!(Some(ChannelOverride::Native), ChannelOverride::parse("native"));
        assert_eq!(None, ChannelOverride::parse("Portal"));
        assert_eq!(None, ChannelOverride::parse("evdev"));
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
        for reason in [
            NativeReason::NoPortal,
            NativeReason::Forced,
            NativeReason::ForcedButAbsent,
            NativeReason::Hyprland,
        ] {
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
    fn native_hyprland_status_names_the_config_target() {
        let exe = Path::new("/home/u/chibipop");
        let line = Selection::Native(NativeReason::Hyprland).startup_line(exe);
        assert!(line.contains("Hyprland"), "{line}");
        assert!(line.contains("hyprland.conf"), "{line}");
        assert!(line.contains("/home/u/chibipop ctl"), "{line}");
    }

    #[test]
    fn every_action_id_dispatches_its_matching_press_verb() {
        for (id, verb) in [
            (ShortcutId::AnkiAdd, Verb::AnkiAdd),
            (ShortcutId::Search, Verb::Search),
            (ShortcutId::SentenceSearch, Verb::SentenceSearch),
            (ShortcutId::SelectedText, Verb::SelectedText),
            (ShortcutId::OcrClipboard, Verb::OcrClipboard),
            (ShortcutId::StaticRegion, Verb::StaticRegion),
        ] {
            assert_eq!(Action::Verb(verb), action(id, true, TriggerMode::HoldKey));
            assert_eq!(Action::Nothing, action(id, false, TriggerMode::HoldKey));
        }
    }

    #[test]
    fn trigger_modes_keep_their_press_and_release_contract() {
        assert_eq!(
            Action::Verb(Verb::TriggerDown),
            action(ShortcutId::Trigger, true, TriggerMode::HoldKey)
        );
        assert_eq!(
            Action::Verb(Verb::TriggerUp),
            action(ShortcutId::Trigger, false, TriggerMode::HoldKey)
        );
        assert_eq!(
            Action::Verb(Verb::Toggle),
            action(ShortcutId::Trigger, true, TriggerMode::Toggle)
        );
        assert_eq!(
            Action::Nothing,
            action(ShortcutId::Trigger, false, TriggerMode::Toggle)
        );
        assert_eq!(
            Action::Verb(Verb::Lookup),
            action(ShortcutId::Trigger, true, TriggerMode::Press)
        );
        assert_eq!(
            Action::Nothing,
            action(ShortcutId::Trigger, false, TriggerMode::Press)
        );

        // Every other mode is a hold and needs both events. Without the
        // release, the frozen grab remains active.
        for mode in [TriggerMode::Live, TriggerMode::HoldShift] {
            assert_eq!(
                Action::Verb(Verb::TriggerDown),
                action(ShortcutId::Trigger, true, mode)
            );
            assert_eq!(
                Action::Verb(Verb::TriggerUp),
                action(ShortcutId::Trigger, false, mode)
            );
        }
    }

    #[test]
    fn status_details_name_the_owner_of_the_binding() {
        let named = vec![
            Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
        ];
        let detail = portal_detail(&named);
        assert!(detail.contains("GlobalShortcuts portal"), "{detail}");
        assert!(detail.contains("trigger Alt+F"), "{detail}");
        assert!(portal_detail(&[Binding { id: ShortcutId::Trigger, trigger: None }])
            .contains("key not reported"));
        assert!(portal_detail(&[]).contains("bound nothing"));

        let native = native_detail(&native_reason(NativeReason::NoPortal));
        assert!(native.contains("control socket"), "{native}");
        assert!(native.contains(portal::SHORTCUTS_INTERFACE), "{native}");
    }

    /// A worker inside the bus handshake has no connection to close, and the
    /// handshake has no timeout. `stop` must not wait for it, because the
    /// caller is the calloop pump. The worker stands in for that handshake
    /// with a receive that only this test releases.
    #[test]
    fn stop_does_not_wait_for_a_worker_without_a_connection() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let cancel = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let _ = release_rx.recv();
        });
        let mut handle = SessionHandle::new(
            SessionId::new(1),
            cancel.clone(),
            Arc::new(Mutex::new(ConnectionState::default())),
            worker,
        );

        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            handle.stop();
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "stop joined a worker that never published a connection"
        );
        assert!(cancel.load(Ordering::Acquire));
        drop(release_tx);
    }
}

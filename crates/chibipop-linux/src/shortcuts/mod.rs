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

use chibipop::config::Config;
use std::path::Path;

/// A configured bind ID used by the portal and control socket.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShortcutId(String);

impl ShortcutId {
    /// Parse an ID from a portal signal or saved bind.
    pub fn parse(id: &str) -> Option<Self> {
        Self::is_valid(id).then(|| Self(id.to_string()))
    }

    pub(crate) fn is_valid(id: &str) -> bool {
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One enabled configured bind offered to the GlobalShortcuts portal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutSpec {
    pub id: ShortcutId,
    pub trigger: String,
    pub description: String,
}

/// One shortcut binding reported by the portal.
///
/// `trigger` can be absent when the portal confirms an ID but does not return
/// a human-readable key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: ShortcutId,
    pub trigger: Option<String>,
}

impl Binding {
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
                "trigger: control socket only (ladder rung 2) - no {} on the session bus; use `{} ctl bind-down <id>` and `{} ctl bind-up <id>` in compositor binds",
                portal::SHORTCUTS_INTERFACE,
                crate::paths::shell_quote(exe),
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
                "trigger: control socket only - automatic Hyprland mode uses native keybinds; use `{} ctl bind-down <id>` in hyprland.conf",
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

/// Build enabled configured binds that have a Linux chord.
pub fn preferred(config: &Config) -> Vec<ShortcutSpec> {
    config
        .binds
        .iter()
        .filter(|bind| bind.enabled)
        .filter_map(|bind| {
            let chord = normalize_trigger(&bind.linux);
            if chord.is_empty() {
                return None;
            }
            Some(ShortcutSpec {
                id: ShortcutId::parse(&bind.id)?,
                trigger: chord,
                description: format!("{} ({})", bind.action.name(), bind.id),
            })
        })
        .collect()
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

/// Return the trigger detail while the portal has not answered.
pub fn pending_detail() -> String {
    "GlobalShortcuts portal - binding requested; control socket serving meanwhile".to_string()
}

/// Return the trigger detail after the portal answers.
pub fn portal_detail(bindings: &[Binding]) -> String {
    if bindings.is_empty() {
        return "GlobalShortcuts portal bound nothing - control socket serves configured binds"
            .to_string();
    }
    let described: Vec<String> = bindings.iter().map(Binding::describe).collect();
    format!("GlobalShortcuts portal - {}", described.join(", "))
}

/// Return the trigger detail when the portal rung cannot serve.
pub fn native_detail(why: &str) -> String {
    format!("control socket (configured bind IDs) - {why}")
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
    use chibipop::config::BindAction;

use super::*;

    #[test]
    fn shortcut_ids_accept_only_saved_config_id_syntax() {
        let id = ShortcutId::parse("bind_17").unwrap();
        assert_eq!("bind_17", id.as_str());
        assert_eq!(None, ShortcutId::parse(""));
        assert_eq!(None, ShortcutId::parse("bad/id"));
        assert_eq!(None, ShortcutId::parse(&"x".repeat(65)));
    }

    #[test]
    fn preferred_registers_only_enabled_binds_with_linux_chords() {
        let mut config = Config::default();
        config.binds.clear();
        let mut lookup = chibipop::config::Bind::new("bind-1".into(), BindAction::Lookup);
        lookup.linux = "ALT+F".into();
        config.binds.push(lookup);
        let mut search = chibipop::config::Bind::new("bind-2".into(), BindAction::Search);
        search.linux = "SUPER+F5".into();
        config.binds.push(search);
        let mut disabled = chibipop::config::Bind::new("bind-3".into(), BindAction::AnkiAdd);
        disabled.enabled = false;
        disabled.linux = "ALT+A".into();
        config.binds.push(disabled);
        let mut unbound = chibipop::config::Bind::new("bind-4".into(), BindAction::SelectedText);
        unbound.linux = "  ".into();
        config.binds.push(unbound);

        let specs = preferred(&config);
        assert_eq!(2, specs.len());
        assert_eq!("bind-1", specs[0].id.as_str());
        assert_eq!("ALT+f", specs[0].trigger);
        assert_eq!("Screen lookup (bind-1)", specs[0].description);
        assert_eq!("bind-2", specs[1].id.as_str());
        assert_eq!("LOGO+F5", specs[1].trigger);
        assert_eq!("Search (bind-2)", specs[1].description);
    }

    #[test]
    fn an_empty_or_invalid_bind_is_not_registered() {
        let mut config = Config::default();
        config.binds.clear();
        let mut empty = chibipop::config::Bind::new("bind-1".into(), BindAction::Lookup);
        empty.linux = " ".into();
        config.binds.push(empty);
        let mut invalid_id = chibipop::config::Bind::new("bad/id".into(), BindAction::Search);
        invalid_id.linux = "ALT+F".into();
        config.binds.push(invalid_id);
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
        assert!(advice.contains("ctl bind-down <id>"), "{advice}");
        assert!(advice.contains("ctl bind-up <id>"), "{advice}");
        let spaced = Selection::Native(NativeReason::NoPortal)
            .startup_line(Path::new("/home/u/my builds/chibipop"));
        assert!(spaced.contains("'/home/u/my builds/chibipop' ctl bind-down"), "{spaced}");
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
    fn status_details_name_the_owner_and_dynamic_bind_id() {
        let id = ShortcutId::parse("bind-17").unwrap();
        let named = vec![Binding { id: id.clone(), trigger: Some("Alt+F".into()) }];
        let detail = portal_detail(&named);
        assert!(detail.contains("GlobalShortcuts portal"), "{detail}");
        assert!(detail.contains("bind-17 Alt+F"), "{detail}");
        assert!(portal_detail(&[Binding { id, trigger: None }]).contains("key not reported"));
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

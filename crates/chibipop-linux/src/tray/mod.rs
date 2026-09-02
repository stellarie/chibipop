//! This module provides the StatusNotifierItem tray
//! (ARCHITECTURE.md#platform-integration).
//! It shows Settings, Quit, and one disabled row for each input channel.
//! The rows show why a channel does not work.
//!
//! Three rules shape this module.
//!
//! **The daemon stays synchronous.** `ksni` owns its D-Bus thread.
//! Use `default-features = false, features = ["async-io", "blocking"]`
//! so this binary has no `tokio` runtime
//! (ARCHITECTURE.md#workspace-and-seams and ksni's documented
//! feature-unification hazard).
//! `blocking` wraps the same async-io runtime. It does not add another runtime.
//!
//! **The tray never causes a fatal error.**
//! If D-Bus, the watcher, or the host is absent, the app works without a tray.
//! `spawn`, `watcher_online`, and `watcher_offline` send diagnostics for the
//! states they report.
//! Windows treats tray creation failure as fatal, but this binary does not.
//! Stock GNOME has no tray host, and bare Hyprland has no bar.
//!
//! **The tray thread never touches daemon state.**
//! Menu activations and tray diagnostics travel as [`TrayRequest`] over a calloop channel.
//! The daemon thread handles them with the log, settings-child guard, and loop signal.
//! [`TrayHandle::set_channel`] sends a registry snapshot in the reverse direction.

pub mod icon;
pub mod status;

use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{MenuItem, Status};
use status::{ChannelState, ChannelStatuses, ChannelId};

/// Requests that the tray thread sends to the daemon thread.
#[derive(Debug, PartialEq, Eq)]
pub enum TrayRequest {
    /// The user activated the Settings menu item.
    OpenSettings,
    /// The user activated the Quit menu item.
    Quit,
    /// A tray diagnostic. The daemon writes it to the `Log`.
    Diagnostic(String),
}

/// The `ksni::Tray` implementation on the tray thread.
/// The daemon reaches it only through [`TrayHandle`].
struct ChibipopTray {
    statuses: ChannelStatuses,
    requests: calloop::channel::Sender<TrayRequest>,
}

impl ChibipopTray {
    /// Send a request to the daemon thread.
    /// The channel is unbounded and non-blocking because the `ksni` activation
    /// contract requires it.
    /// A closed channel means that the daemon has begun shutdown.
    /// The method can then drop the request.
    fn ask(&self, request: TrayRequest) {
        let _ = self.requests.send(request);
    }
}

impl ksni::Tray for ChibipopTray {
    /// A left click opens the menu. It does not call `activate`.
    /// The menu is the status tray's main purpose.
    /// A click with no visible result makes the icon seem broken.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "chibipop".into()
    }

    fn title(&self) -> String {
        "chibipop".into()
    }

    fn status(&self) -> Status {
        self.statuses.sni_status()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon::icons().pixmaps.clone()
    }

    /// Hosts read *this* pixmap when the status is `NeedsAttention`.
    /// Keep the list non-empty so the icon remains visible when the user needs it.
    /// The artwork stays the same. The bar emphasizes the status.
    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon::icons().pixmaps.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "chibipop".into(),
            description: self.statuses.rows().join("\n"),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = vec![
            StandardItem {
                label: "Settings".into(),
                activate: Box::new(|tray: &mut Self| tray.ask(TrayRequest::OpenSettings)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        // The status rows provide information and accept no clicks.
        // The user reads them to learn why a channel is down.
        // The settings window provides the fixes.
        items.extend(self.statuses.rows().into_iter().map(|label| {
            StandardItem { label, enabled: false, ..Default::default() }.into()
        }));
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.ask(TrayRequest::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }

    fn watcher_online(&self) {
        self.ask(TrayRequest::Diagnostic("tray: StatusNotifier host online - item shown".into()));
    }

    /// Return `true` so a bar that starts later or a shell that restarts can find
    /// the item.
    /// Keep the item published. Do not restart the daemon.
    /// This is the trayless path, not an error.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        self.ask(TrayRequest::Diagnostic(format!(
            "tray: no StatusNotifier host ({reason:?}); running trayless - every feature still works, \
             and a bar started later picks the item up"
        )));
        true
    }
}

/// The daemon-side tray handle.
/// It owns the authoritative channel registry and an optional tray mirror.
///
/// The registry stays here, not only in the tray thread.
/// The daemon can then track channel health without a tray.
/// In trayless mode, every method still works. Only the D-Bus push stops.
pub struct TrayHandle {
    statuses: ChannelStatuses,
    handle: Option<ksni::blocking::Handle<ChibipopTray>>,
}

impl TrayHandle {
    /// Create a registry with no tray.
    /// `spawn` returns this value when D-Bus is unavailable.
    /// The daemon uses the registry without changes.
    pub fn trayless(statuses: ChannelStatuses) -> TrayHandle {
        TrayHandle { statuses, handle: None }
    }

    /// Return whether a tray service remains active behind this handle.
    pub fn is_connected(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_closed())
    }

    pub fn statuses(&self) -> &ChannelStatuses {
        &self.statuses
    }

    /// Set a channel state and re-render the tray.
    /// Return `true` only when the state changes.
    /// Callers can then log transitions instead of each poll tick.
    ///
    /// Detect a dead tray service here and forget it.
    /// The handle then becomes trayless.
    /// It does not retry a dead service on each update.
    pub fn set_channel(&mut self, id: ChannelId, state: ChannelState) -> bool {
        if !self.statuses.set(id, state) {
            return false;
        }
        if let Some(handle) = &self.handle {
            let snapshot = self.statuses.clone();
            if handle.update(move |tray| tray.statuses = snapshot).is_none() {
                self.handle = None;
            }
        }
        true
    }
}

/// Publish a tray and return its handle and diagnostics.
/// This function does not fail. The daemon logs the second result.
/// The handle works with or without a tray.
///
/// `assume_sni_available(true)` (ARCHITECTURE.md#platform-integration)
/// converts "no watcher on the bus" and "nothing will show this" into soft errors.
/// It routes those errors to `watcher_offline`.
/// A daemon can start before the bar, which is normal with a session manager.
/// The bar then shows the item when it arrives.
pub fn spawn(
    statuses: ChannelStatuses,
    requests: calloop::channel::Sender<TrayRequest>,
) -> (TrayHandle, Vec<String>) {
    let mut diagnostics: Vec<String> =
        icon::icons().problems.iter().map(|p| format!("tray: icon asset {p} (icon will be blank)")).collect();

    let tray = ChibipopTray { statuses: statuses.clone(), requests };
    match tray.assume_sni_available(true).spawn() {
        Ok(handle) => {
            diagnostics.push(
                "tray: StatusNotifierItem published (Settings / channel status / Quit)".to_string(),
            );
            (TrayHandle { statuses, handle: Some(handle) }, diagnostics)
        }
        Err(e) => {
            diagnostics.push(format!(
                "tray: unavailable ({e}); running trayless - every feature still works"
            ));
            (TrayHandle::trayless(statuses), diagnostics)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::backend::{Backend, Selection as CaptureSelection};
    use crate::cursor::{Rung, Selection};
    use ksni::Tray;

    /// Build the startup registry for a wlr session.
    /// It has the promptless capture backend, the selected cursor rung, and a layer shell.
    /// The layer shell provides the draw surface.
    fn tray(selection: &Selection) -> (ChibipopTray, calloop::channel::Channel<TrayRequest>) {
        let (tx, rx) = calloop::channel::channel();
        let statuses = ChannelStatuses::startup(
            status::capture_state(&CaptureSelection::Backend(Backend::WlrScreencopy)),
            selection,
            status::popup_state(true),
        );
        (ChibipopTray { statuses, requests: tx }, rx)
    }

    /// Return each menu label in order with its clickable state.
    /// Separators return as `("-", false)`.
    fn labels(tray: &ChibipopTray) -> Vec<(String, bool)> {
        tray.menu()
            .into_iter()
            .map(|item| match item {
                MenuItem::Standard(i) => (i.label, i.enabled),
                MenuItem::Separator => ("-".to_string(), false),
                _ => ("?".to_string(), false),
            })
            .collect()
    }

    /// Activate the first menu item with the given label.
    fn activate(tray: &mut ChibipopTray, label: &str) {
        let found = tray
            .menu()
            .into_iter()
            .find_map(|item| match item {
                MenuItem::Standard(i) if i.label == label => Some(i.activate),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no menu item labelled {label:?}"));
        found(tray);
    }

    /// The menu has Settings, one disabled status row per channel, and Quit.
    /// The two actions remain enabled.
    #[test]
    fn menu_is_settings_then_disabled_status_rows_then_quit() {
        let (tray, _rx) = tray(&Selection::Rung(Rung::ImageCopyCapture));
        assert_eq!(
            vec![
                ("Settings".to_string(), true),
                ("-".to_string(), false),
                ("Capture: wlr-screencopy region capture".to_string(), false),
                ("Cursor: ext-image-copy-capture cursor session".to_string(), false),
                ("Trigger: control socket".to_string(), false),
                ("Popup: wlr-layer-shell overlay surface".to_string(), false),
                ("-".to_string(), false),
                ("Quit".to_string(), true),
            ],
            labels(&tray)
        );
    }

    /// Menu activation sends each request to the daemon thread.
    /// The tray thread does not execute the request.
    /// This keeps the menu responsive and the daemon synchronous.
    #[test]
    fn activating_settings_and_quit_asks_the_daemon() {
        let (mut tray, rx) = tray(&Selection::Rung(Rung::HyprctlPoll));

        activate(&mut tray, "Settings");
        assert_eq!(Ok(TrayRequest::OpenSettings), rx.try_recv());

        activate(&mut tray, "Quit");
        assert_eq!(Ok(TrayRequest::Quit), rx.try_recv());
    }

    /// A down channel appears in its row and in the SNI status.
    /// The bar emphasizes the SNI status.
    #[test]
    fn a_down_channel_shows_in_the_row_and_the_sni_status() {
        let unsupported = Selection::Unsupported {
            missing: vec!["ext_image_copy_capture_manager_v1".to_string()],
        };
        let (tray, _rx) = tray(&unsupported);

        assert_eq!(Status::NeedsAttention, tray.status());
        assert!(
            labels(&tray).iter().any(|(l, enabled)| l
                == "Cursor: unsupported - missing ext_image_copy_capture_manager_v1"
                && !enabled),
            "the reason must be readable in the menu: {:?}",
            labels(&tray)
        );
    }

    /// `NeedsAttention` must still have an icon.
    /// Hosts switch to the attention pixmap for this status.
    /// An empty list would blank the tray when the user needs to see it.
    #[test]
    fn the_attention_icon_is_not_empty() {
        let (tray, _rx) = tray(&Selection::Rung(Rung::HyprctlPoll));
        assert_eq!(tray.icon_pixmap().len(), tray.attention_icon_pixmap().len());
        assert!(!tray.attention_icon_pixmap().is_empty());
    }

    /// The tooltip carries the same rows.
    /// It answers "what is broken?" without a menu open.
    #[test]
    fn the_tooltip_lists_every_channel() {
        let (tray, _rx) = tray(&Selection::Rung(Rung::HyprctlPoll));
        let tip = tray.tool_tip();
        assert_eq!("chibipop", tip.title);
        assert_eq!(tray.statuses.rows().join("\n"), tip.description);
    }

    /// Trayless mode is normal on stock GNOME and bare Hyprland.
    /// The registry remains usable and reports a disconnected tray.
    /// A push with no destination does not panic.
    #[test]
    fn a_trayless_handle_still_tracks_channels() {
        let mut handle = TrayHandle::trayless(ChannelStatuses::startup(
            status::capture_state(&CaptureSelection::Backend(Backend::WlrScreencopy)),
            &Selection::Rung(Rung::HyprctlPoll),
            status::popup_state(true),
        ));
        assert!(!handle.is_connected());

        assert!(handle.set_channel(ChannelId::Cursor, ChannelState::down("hyprctl gone")));
        assert_eq!("Cursor: hyprctl gone", handle.statuses().row(ChannelId::Cursor));
        assert_eq!(Status::NeedsAttention, handle.statuses().sni_status());

        assert!(
            !handle.set_channel(ChannelId::Cursor, ChannelState::down("hyprctl gone")),
            "a repeat is not a transition, tray or no tray"
        );

        assert!(handle.set_channel(
            ChannelId::Cursor,
            ChannelState::up(status::rung_detail(Rung::HyprctlPoll))
        ));
        assert_eq!(Status::Active, handle.statuses().sni_status());
    }

    /// The tray renders the state that the daemon sends.
    /// A mirrored snapshot must produce the menu that the daemon registry describes.
    #[test]
    fn a_pushed_snapshot_renders_the_new_rows() {
        let (mut tray, _rx) = tray(&Selection::Rung(Rung::HyprctlPoll));
        let mut handle = TrayHandle::trayless(ChannelStatuses::startup(
            status::capture_state(&CaptureSelection::Backend(Backend::Portal)),
            &Selection::Rung(Rung::HyprctlPoll),
            status::popup_state(true),
        ));
        handle.set_channel(ChannelId::Capture, ChannelState::up("wlr-screencopy"));

        tray.statuses = handle.statuses().clone();
        assert!(labels(&tray).iter().any(|(l, _)| l == "Capture: wlr-screencopy"));
        assert_eq!(Status::Active, tray.status());
    }
}

//! The StatusNotifierItem tray (ARCHITECTURE.md#platform-integration):
//! Settings, Quit, and a disabled row per input channel so a user can
//! see at a glance why something is not working.
//!
//! Three rules shape this module.
//!
//! **The daemon stays sync.** ksni runs its own D-Bus thread; we take it
//! with `default-features = false, features = ["async-io", "blocking"]`
//! so no tokio reaches this binary (ARCHITECTURE.md#workspace-and-seams,
//! and ksni's documented feature-unification footgun). `blocking` is a
//! thin wrapper over that same async-io runtime, not a second one.
//!
//! **Nothing about the tray is fatal.** No D-Bus, no watcher, no host,
//! a wedged bar, a dead tray thread: every one of those degrades to
//! "trayless" — one diagnostic line and an app that works. Windows'
//! "failing to create the tray is fatal" deliberately flips here,
//! because stock GNOME has no tray host and bare Hyprland has no bar.
//!
//! **The tray thread never touches daemon state.** Menu activations and
//! the tray's own diagnostics travel as [`TrayRequest`] over a calloop
//! channel, so they land on the daemon thread where the log, the
//! settings-child guard and the loop signal live. The reverse direction
//! is [`TrayHandle::set_channel`], which pushes a registry snapshot into
//! the tray.

pub mod icon;
pub mod status;

use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{MenuItem, Status};
use status::{ChannelState, ChannelStatuses, ChannelId};

/// What the tray thread asks the daemon thread to do or say.
#[derive(Debug, PartialEq, Eq)]
pub enum TrayRequest {
    /// The Settings menu item was activated.
    OpenSettings,
    /// The Quit menu item was activated.
    Quit,
    /// The tray has something for the log; the daemon owns the `Log`.
    Diagnostic(String),
}

/// The `ksni::Tray` implementation. Lives on the tray thread; the daemon
/// only ever reaches it through [`TrayHandle`].
struct ChibipopTray {
    statuses: ChannelStatuses,
    requests: calloop::channel::Sender<TrayRequest>,
}

impl ChibipopTray {
    /// Hand a request to the daemon thread. Unbounded and non-blocking,
    /// as ksni's activation contract requires; a closed channel means
    /// the daemon is already shutting down, so dropping is correct.
    fn ask(&self, request: TrayRequest) {
        let _ = self.requests.send(request);
    }
}

impl ksni::Tray for ChibipopTray {
    /// Left-click opens the menu instead of firing `activate`. A status
    /// tray's whole purpose is the menu, and a click that does nothing
    /// visible reads as a broken icon.
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

    /// Hosts read *this* pixmap while the status is NeedsAttention;
    /// leaving it empty makes the icon vanish exactly when it matters.
    /// Same artwork — the status itself is what the bar emphasises.
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
        // The status rows: informational, never clickable. A user reads
        // them to find out why a channel is dead; there is nothing to
        // press, and the settings window is where fixes live.
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

    /// Return `true`: keep the item published so a bar that starts later
    /// (or a shell that restarts) picks it up without restarting the
    /// daemon. This is the trayless path, and it is not an error.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        self.ask(TrayRequest::Diagnostic(format!(
            "tray: no StatusNotifier host ({reason:?}); running trayless - every feature still works, \
             and a bar started later picks the item up"
        )));
        true
    }
}

/// The daemon's end of the tray: the authoritative channel registry plus
/// an optional live tray to mirror it into.
///
/// The registry is here rather than only inside the tray thread so the
/// daemon's view of channel health does not depend on a tray existing.
/// Trayless, every method below still works; only the D-Bus push is
/// skipped.
pub struct TrayHandle {
    statuses: ChannelStatuses,
    handle: Option<ksni::blocking::Handle<ChibipopTray>>,
}

impl TrayHandle {
    /// A registry with no tray behind it — what `spawn` returns when
    /// D-Bus is unavailable, and what the daemon uses unchanged.
    pub fn trayless(statuses: ChannelStatuses) -> TrayHandle {
        TrayHandle { statuses, handle: None }
    }

    /// Whether a tray service is still running behind this handle.
    pub fn is_connected(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_closed())
    }

    pub fn statuses(&self) -> &ChannelStatuses {
        &self.statuses
    }

    /// Record a channel's new state and re-render the tray. Returns
    /// whether anything actually changed, so callers log transitions
    /// instead of every poll tick.
    ///
    /// A dead tray service is detected here and forgotten: the handle
    /// degrades to trayless rather than retrying a corpse on every
    /// update.
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

/// Publish the tray. Never fails: the second element is the diagnostics
/// the daemon should log, and the handle works either way.
///
/// `assume_sni_available(true)` (ARCHITECTURE.md#platform-integration)
/// turns "no watcher on the bus" and "nothing will show this" into soft
/// errors routed to `watcher_offline`, so a daemon that starts before
/// the bar — the normal case under a session manager — still gets its
/// item shown when the bar arrives.
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

    /// The startup registry a wlr session produces: the promptless
    /// capture backend, whichever cursor rung was selected, and a layer
    /// shell to draw on.
    fn tray(selection: &Selection) -> (ChibipopTray, calloop::channel::Channel<TrayRequest>) {
        let (tx, rx) = calloop::channel::channel();
        let statuses = ChannelStatuses::startup(
            status::capture_state(&CaptureSelection::Backend(Backend::WlrScreencopy)),
            selection,
            status::popup_state(true),
        );
        (ChibipopTray { statuses, requests: tx }, rx)
    }

    /// Every menu label in order, paired with whether it is clickable.
    /// Separators read as `("-", false)`.
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

    /// Activate the first menu item with this label.
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

    /// The menu shape: Settings, one status row per channel, Quit — and
    /// the status rows are disabled while the two actions are not.
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

    /// Activation hands work to the daemon thread rather than doing it,
    /// which is what keeps the menu responsive and the daemon sync.
    #[test]
    fn activating_settings_and_quit_asks_the_daemon() {
        let (mut tray, rx) = tray(&Selection::Rung(Rung::HyprctlPoll));

        activate(&mut tray, "Settings");
        assert_eq!(Ok(TrayRequest::OpenSettings), rx.try_recv());

        activate(&mut tray, "Quit");
        assert_eq!(Ok(TrayRequest::Quit), rx.try_recv());
    }

    /// A down channel is visible twice over: in its row and in the SNI
    /// status the bar emphasises.
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

    /// NeedsAttention must still have an icon; hosts switch to the
    /// attention pixmap and an empty list would blank the tray exactly
    /// when the user needs to notice it.
    #[test]
    fn the_attention_icon_is_not_empty() {
        let (tray, _rx) = tray(&Selection::Rung(Rung::HyprctlPoll));
        assert_eq!(tray.icon_pixmap().len(), tray.attention_icon_pixmap().len());
        assert!(!tray.attention_icon_pixmap().is_empty());
    }

    /// The tooltip carries the same rows, so hovering answers "what is
    /// broken?" without opening the menu.
    #[test]
    fn the_tooltip_lists_every_channel() {
        let (tray, _rx) = tray(&Selection::Rung(Rung::HyprctlPoll));
        let tip = tray.tool_tip();
        assert_eq!("chibipop", tip.title);
        assert_eq!(tray.statuses.rows().join("\n"), tip.description);
    }

    /// Traylessness is the normal case on stock GNOME and bare Hyprland:
    /// the registry keeps working, reports itself disconnected, and
    /// never panics on a push that has nowhere to go.
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

    /// The tray renders from whatever the daemon pushed, so a mirrored
    /// snapshot produces the same menu the daemon's registry implies.
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

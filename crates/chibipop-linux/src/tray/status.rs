//! This module stores each channel state for the tray's disabled menu rows
//! (ARCHITECTURE.md#platform-integration). It maps each state to menu-row
//! text and the SNI `Status`.
//!
//! The daemon owns this registry, which works with or without a tray.
//! The daemon supplies the capture backend and its resolved consent, the
//! cursor rung and its live health, and the always-bound control socket.
//! Later channel code updates states when its backends become available.
//! This module does not use D-Bus, so tests can run without a tray host.

use crate::capture::backend::{Backend, Selection as CaptureSelection};
use crate::cursor::{Rung, Selection};

/// One monitored channel in menu order. The list has three input channels
/// and the popup surface that shows their output.
///
/// The popup has its own row for stock GNOME. A session without a layer shell
/// can have healthy input channels but cannot show a definition. An icon that
/// reports every channel as healthy would hide this failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelId {
    Capture,
    Cursor,
    Trigger,
    Popup,
}

impl ChannelId {
    pub const ALL: [ChannelId; 4] =
        [ChannelId::Capture, ChannelId::Cursor, ChannelId::Trigger, ChannelId::Popup];

    pub fn label(self) -> &'static str {
        match self {
            ChannelId::Capture => "Capture",
            ChannelId::Cursor => "Cursor",
            ChannelId::Trigger => "Trigger",
            ChannelId::Popup => "Popup",
        }
    }

    fn index(self) -> usize {
        match self {
            ChannelId::Capture => 0,
            ChannelId::Cursor => 1,
            ChannelId::Trigger => 2,
            ChannelId::Popup => 3,
        }
    }
}

/// State for one channel. `detail` supplies the human-readable part of the
/// menu row. It names the mechanism or the exact capability that is not available.
///
/// The daemon resolves two states at startup for every channel it tracks.
/// These states cover the capture ladder and the portal's eager consent, the
/// cursor ladder, and the always-bound control socket. The third state reports
/// a channel that supplies pixels with a known defect, such as a compositor
/// that paints the pointer into frames that OCR reads. `Up` would mislead the
/// user with wrong readings. `Down` would hide that lookups still work and
/// give the user no useful action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelState {
    /// The channel works. `detail` names its source, such as "control socket".
    Up { detail: String },
    /// The channel remains available but has a named defect that the user can fix.
    /// `detail` names the source and the required change.
    Degraded { detail: String },
    /// The channel is down. `detail` names the absent capability or denial. For
    /// example: "Cursor: portal denied — see settings".
    Down { detail: String },
}

impl ChannelState {
    pub fn up(detail: impl Into<String>) -> ChannelState {
        ChannelState::Up { detail: detail.into() }
    }

    pub fn down(detail: impl Into<String>) -> ChannelState {
        ChannelState::Down { detail: detail.into() }
    }

    /// Append `defect` to this state and change the row to
    /// [`ChannelState::Degraded`]. The row keeps the source and adds the defect.
    ///
    /// A channel that is down stays down. A capability that is not available has
    /// priority over a defect, so this method does not change `Down`.
    pub fn degraded_by(self, defect: &str) -> ChannelState {
        match self {
            ChannelState::Down { .. } => self,
            ChannelState::Up { detail } | ChannelState::Degraded { detail } => {
                ChannelState::Degraded { detail: format!("{detail}; {defect}") }
            }
        }
    }

    fn detail(&self) -> &str {
        match self {
            ChannelState::Up { detail }
            | ChannelState::Degraded { detail }
            | ChannelState::Down { detail } => detail,
        }
    }
}

/// Return the menu text for a live cursor rung. Keep this function separate
/// from `Rung`. The ladder startup diagnostic names protocol globals in a
/// paragraph (ARCHITECTURE.md#input-ladders), while a menu row has one line.
pub fn rung_detail(rung: Rung) -> &'static str {
    match rung {
        Rung::ImageCopyCapture => "ext-image-copy-capture cursor session",
        Rung::PortalMetadata => "portal ScreenCast cursor metadata",
        Rung::HyprctlPoll => "hyprctl cursorpos polling",
    }
}

/// Build the cursor channel state from the rung selection that the daemon
/// already made.
pub fn cursor_state(selection: &Selection) -> ChannelState {
    match selection {
        Selection::Rung(rung) => ChannelState::up(rung_detail(*rung)),
        Selection::Unsupported { missing } => {
            ChannelState::down(format!("unsupported - missing {}", missing.join(", ")))
        }
    }
}

/// Build the capture channel state from the backend selection. If the portal
/// backend has no consent result, do not report that interim state here. The
/// daemon replaces this row with the consent result before it publishes the
/// tray.
pub fn capture_state(selection: &CaptureSelection) -> ChannelState {
    match selection {
        CaptureSelection::Backend(Backend::WlrScreencopy) => {
            ChannelState::up("wlr-screencopy region capture")
        }
        CaptureSelection::Backend(Backend::Portal) => {
            ChannelState::up("portal ScreenCast + PipeWire")
        }
        CaptureSelection::Unsupported { missing } => {
            ChannelState::down(format!("unsupported - missing {}", missing.join(", ")))
        }
    }
}

/// Build the popup channel state. `advertised` says whether this session
/// advertises `zwlr_layer_shell_v1`. This startup result comes before a bind
/// can fail. [`ChannelStatuses::set`] replaces the row if the bind fails.
///
/// The down row names the global. On stock GNOME, this name explains why
/// nothing appears.
pub fn popup_state(advertised: bool) -> ChannelState {
    if advertised {
        ChannelState::up("wlr-layer-shell overlay surface")
    } else {
        ChannelState::down(format!("unsupported - missing {}", crate::wayland::LAYER_SHELL.interface))
    }
}

/// Store every channel in a fixed-size array. The channel set is part of the
/// application shape, not input data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelStatuses {
    states: [ChannelState; 4],
}

impl ChannelStatuses {
    /// Create channel states from the daemon's startup results. `capture`
    /// already includes the backend selection and eager consent for the
    /// portal backend. The daemon publishes the tray only after it resolves
    /// that consent. The daemon has selected the cursor rung. The trigger
    /// uses the always-bound control socket. `popup` says whether the
    /// session's advertised layer shell can carry a panel.
    pub fn startup(capture: ChannelState, cursor: &Selection, popup: ChannelState) -> ChannelStatuses {
        ChannelStatuses {
            states: [capture, cursor_state(cursor), ChannelState::up("control socket"), popup],
        }
    }

    /// Replace one channel state and report whether it changed. The tray
    /// re-renders after a change. Callers can log transitions and skip repeats.
    pub fn set(&mut self, id: ChannelId, state: ChannelState) -> bool {
        let slot = &mut self.states[id.index()];
        if *slot == state {
            return false;
        }
        *slot = state;
        true
    }

    pub fn get(&self, id: ChannelId) -> &ChannelState {
        &self.states[id.index()]
    }

    /// Return one disabled menu row, such as "Cursor: unsupported - missing …".
    pub fn row(&self, id: ChannelId) -> String {
        format!("{}: {}", id.label(), self.get(id).detail())
    }

    /// Return all rows in `ChannelId::ALL` order.
    pub fn rows(&self) -> Vec<String> {
        ChannelId::ALL.iter().map(|&id| self.row(id)).collect()
    }

    /// Return the SNI `Status`. Use `NeedsAttention` when a channel is down or
    /// has a known defect, such as a software cursor. Use `Active` for all other
    /// states. Users can ignore an icon that always shows `NeedsAttention`.
    /// Each attention row identifies the affected channel and its failure.
    pub fn sni_status(&self) -> ksni::Status {
        let wants_attention = |s: &ChannelState| {
            matches!(s, ChannelState::Down { .. } | ChannelState::Degraded { .. })
        };
        if self.states.iter().any(wants_attention) {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::software_cursor;

    /// The common case uses the promptless capture backend.
    fn screencopy() -> ChannelState {
        capture_state(&CaptureSelection::Backend(Backend::WlrScreencopy))
    }

    /// A wlr session provides the layer shell.
    fn shell() -> ChannelState {
        popup_state(true)
    }

    /// The rows show the daemon's startup state. The capture row names the
    /// resolved backend. The trigger row names the control socket. The cursor
    /// row names the selected rung mechanism. The popup row names the shell that
    /// carries the panel.
    #[test]
    fn startup_rows_name_what_the_daemon_knows() {
        let statuses =
            ChannelStatuses::startup(screencopy(), &Selection::Rung(Rung::HyprctlPoll), shell());
        assert_eq!(
            vec![
                "Capture: wlr-screencopy region capture".to_string(),
                "Cursor: hyprctl cursorpos polling".to_string(),
                "Trigger: control socket".to_string(),
                "Popup: wlr-layer-shell overlay surface".to_string(),
            ],
            statuses.rows()
        );
    }

    /// Stock GNOME with the AppIndicator extension can provide healthy capture,
    /// cursor, and trigger channels through the portals. It cannot draw the popup
    /// without a layer shell. An icon that shows `Active` for every channel would
    /// hide the problem. The row names the unavailable global, so the user can fix
    /// it without the log.
    #[test]
    fn a_session_without_the_layer_shell_shows_a_down_popup_row() {
        let statuses = ChannelStatuses::startup(
            capture_state(&CaptureSelection::Backend(Backend::Portal)),
            &Selection::Rung(Rung::PortalMetadata),
            popup_state(false),
        );
        assert_eq!(
            "Popup: unsupported - missing zwlr_layer_shell_v1",
            statuses.row(ChannelId::Popup)
        );
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status());
        assert_eq!("Capture: portal ScreenCast + PipeWire", statuses.row(ChannelId::Capture));
        assert_eq!("Cursor: portal ScreenCast cursor metadata", statuses.row(ChannelId::Cursor));
        assert_eq!("Trigger: control socket", statuses.row(ChannelId::Trigger));
    }

    /// The compositor paints the pointer into frames that the backend copies.
    /// Lookups still work. The row names the backend and the setting to change.
    /// The icon reports the defect.
    #[test]
    fn a_capture_row_can_serve_and_still_name_a_defect() {
        let defect = software_cursor::PointerInFrames::Always
            .row_defect()
            .expect("a known software cursor degrades the row");
        let mut statuses =
            ChannelStatuses::startup(screencopy(), &Selection::Rung(Rung::HyprctlPoll), shell());
        assert_eq!(ksni::Status::Active, statuses.sni_status(), "healthy before");

        statuses.set(ChannelId::Capture, screencopy().degraded_by(&defect));
        assert_eq!(
            "Capture: wlr-screencopy region capture; pointer painted into frames - set \
             cursor:no_hardware_cursors = false",
            statuses.row(ChannelId::Capture)
        );
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status(), "spoiled is not healthy");
    }

    /// Fix the capability that is not available first. A defect must not change a
    /// down channel into a channel that supplies data.
    #[test]
    fn a_down_channel_stays_down_when_a_defect_is_added() {
        let down = ChannelState::down("unsupported - missing zwlr_screencopy_manager_v1");
        assert_eq!(down.clone(), down.clone().degraded_by("pointer painted into frames"));
    }

    /// A down channel sets the SNI status to `NeedsAttention`. Recovery clears
    /// that status.
    #[test]
    fn any_down_channel_needs_attention() {
        let mut statuses = ChannelStatuses::startup(
            screencopy(),
            &Selection::Rung(Rung::ImageCopyCapture),
            shell(),
        );
        assert_eq!(ksni::Status::Active, statuses.sni_status(), "all-up must be Active");

        assert!(statuses.set(ChannelId::Trigger, ChannelState::down("socket gone")));
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status());
        assert_eq!("Trigger: socket gone", statuses.row(ChannelId::Trigger));

        assert!(statuses.set(ChannelId::Trigger, ChannelState::up("control socket")));
        assert_eq!(ksni::Status::Active, statuses.sni_status(), "recovery must clear it");
    }

    /// A refused portal must leave a retry path in the capture row. The icon
    /// must also report `NeedsAttention`.
    #[test]
    fn a_refused_capture_channel_shows_the_retry_and_needs_attention() {
        let mut statuses = ChannelStatuses::startup(
            screencopy(),
            &Selection::Rung(Rung::ImageCopyCapture),
            shell(),
        );
        assert_eq!(ksni::Status::Active, statuses.sni_status());
        assert!(statuses.set(
            ChannelId::Capture,
            ChannelState::down("screen-capture permission denied - retry with `chibipop ctl reload`")
        ));
        assert!(statuses.row(ChannelId::Capture).contains("retry"));
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status());
    }

    /// The cursor selection can return `Unsupported`. The row names the exact
    /// capability that is not available, so a compositor upgrade gives the user
    /// a clear fix.
    #[test]
    fn unsupported_cursor_selection_maps_to_a_down_row() {
        let selection =
            Selection::Unsupported { missing: vec!["ext_image_copy_capture_manager_v1".to_string()] };
        let statuses = ChannelStatuses::startup(screencopy(), &selection, shell());
        assert_eq!(
            "Cursor: unsupported - missing ext_image_copy_capture_manager_v1",
            statuses.row(ChannelId::Cursor)
        );
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status());
    }

    /// `set` replaces only the addressed channel and reports whether the state
    /// changed. The daemon can log transitions without repeat entries.
    #[test]
    fn set_replaces_only_the_addressed_channel_and_reports_change() {
        let mut statuses =
            ChannelStatuses::startup(screencopy(), &Selection::Rung(Rung::HyprctlPoll), shell());
        let before_cursor = statuses.row(ChannelId::Cursor);

        assert!(statuses.set(ChannelId::Capture, ChannelState::up("wlr-screencopy")));
        assert_eq!("Capture: wlr-screencopy", statuses.row(ChannelId::Capture));
        assert_eq!(before_cursor, statuses.row(ChannelId::Cursor));
        assert_eq!("Trigger: control socket", statuses.row(ChannelId::Trigger));

        assert!(
            !statuses.set(ChannelId::Capture, ChannelState::up("wlr-screencopy")),
            "re-setting the same state is not a transition"
        );
    }

    /// Give each rung its own one-line row text. A new rung must not inherit
    /// another rung's description.
    #[test]
    fn every_rung_has_its_own_row_text() {
        let details =
            [Rung::ImageCopyCapture, Rung::PortalMetadata, Rung::HyprctlPoll].map(rung_detail);
        for (i, detail) in details.iter().enumerate() {
            assert!(!detail.is_empty());
            assert!(
                !details[..i].contains(detail),
                "{detail:?} is reused between rungs: {details:?}"
            );
        }
    }

    /// Map each backend selection to an honest row. Each backend names its
    /// mechanism. An unsupported selection names the capability that is not
    /// available.
    #[test]
    fn every_capture_selection_maps_to_its_own_row() {
        assert_eq!(ChannelState::up("wlr-screencopy region capture"), screencopy());
        assert_eq!(
            ChannelState::up("portal ScreenCast + PipeWire"),
            capture_state(&CaptureSelection::Backend(Backend::Portal))
        );
        assert_eq!(
            ChannelState::down(
                "unsupported - missing zwlr_screencopy_manager_v1, org.freedesktop.portal.ScreenCast"
            ),
            capture_state(&CaptureSelection::Unsupported {
                missing: vec![
                    "zwlr_screencopy_manager_v1".to_string(),
                    "org.freedesktop.portal.ScreenCast".to_string(),
                ],
            })
        );
    }

    /// No capture capability blocks hover. The icon must report the problem.
    #[test]
    fn an_unsupported_capture_selection_needs_attention() {
        let capture = capture_state(&CaptureSelection::Unsupported {
            missing: vec!["zwlr_screencopy_manager_v1".to_string()],
        });
        let statuses =
            ChannelStatuses::startup(capture, &Selection::Rung(Rung::ImageCopyCapture), shell());
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status());
        assert_eq!(
            "Capture: unsupported - missing zwlr_screencopy_manager_v1",
            statuses.row(ChannelId::Capture)
        );
    }

    /// A user reads this row in a menu. Keep it to one line without excess
    /// punctuation or a protocol dump.
    #[test]
    fn the_capture_row_reads_as_a_sentence() {
        for selection in [
            CaptureSelection::Backend(Backend::WlrScreencopy),
            CaptureSelection::Backend(Backend::Portal),
        ] {
            let statuses = ChannelStatuses::startup(
                capture_state(&selection),
                &Selection::Rung(Rung::HyprctlPoll),
                shell(),
            );
            let row = statuses.row(ChannelId::Capture);
            assert!(row.starts_with("Capture: "), "{row}");
            assert!(!row.contains('\n'), "{row}");
            assert!(row.split_whitespace().count() >= 3, "{row}");
            assert!(!row.ends_with('.'), "{row}");
        }
    }
}

//! The per-channel status registry behind the tray's disabled menu rows
//! (ARCHITECTURE.md#platform-integration): what each input channel is
//! doing right now, and the one mapping from those states to menu-row
//! text and the SNI `Status`.
//!
//! The registry is daemon-owned and works with or without a tray — it is
//! fed from what the daemon already knows (the ticket-34 capture backend
//! selection and the consent it resolved before publishing, the
//! ticket-33 cursor rung selection and its live health, the
//! always-bound control socket), and later channel tickets flip states
//! as their backends land. Nothing here touches D-Bus, so all of it is
//! testable without a tray host.

use crate::capture::backend::{Backend, Selection as CaptureSelection};
use crate::cursor::{Rung, Selection};

/// One monitored channel, in menu order: the three input channels, plus
/// the popup surface that shows what they produce. The popup earns a row
/// for the stock-GNOME case (ticket 49): a session with no layer shell
/// has three perfectly healthy input channels and still cannot show a
/// definition, and a tray that reads all-green there is the "silently
/// half-works" failure this app refuses.
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

/// What one channel is doing. `detail` is the human half of the menu
/// row — short, honest, and naming the mechanism or the exact gap.
///
/// Three states. Two of them resolve at startup for every channel the
/// daemon tracks (the capture ladder including the portal's eager
/// consent, the cursor ladder, the always-bound control socket), so
/// there is no channel left for a "not built yet" placeholder to
/// describe. The third is for the failure this app refuses to hide: a
/// channel that serves pixels *and* is known to serve them spoiled - a
/// compositor painting the pointer into the frames we OCR (ticket 52).
/// Reporting that as Up would be a lie the user pays for in wrong
/// readings; reporting it as Down would be a lie they could not act on,
/// because lookups do work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelState {
    /// Working; `detail` names what serves it ("control socket").
    Up { detail: String },
    /// Serving, with a named defect the user can fix; `detail` names
    /// what serves it *and* what to change.
    Degraded { detail: String },
    /// Down; `detail` names exactly what is missing or denied, per the
    /// example row "Cursor: portal denied — see settings".
    Down { detail: String },
}

impl ChannelState {
    pub fn up(detail: impl Into<String>) -> ChannelState {
        ChannelState::Up { detail: detail.into() }
    }

    pub fn down(detail: impl Into<String>) -> ChannelState {
        ChannelState::Down { detail: detail.into() }
    }

    /// This state, with `defect` appended and the row demoted to
    /// [`ChannelState::Degraded`]: the row keeps naming what serves the
    /// channel and gains what is wrong with it.
    ///
    /// A channel that is already down stays down - "unsupported, and
    /// also spoiled" is not a distinction a user can act on, and the
    /// missing capability is the thing to fix first.
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

/// How a live cursor rung reads in a menu row. Deliberately not a method
/// on `Rung`: the ladder's own startup diagnostic is a paragraph naming
/// protocol globals (ARCHITECTURE.md#input-ladders), and a menu row has
/// one line.
pub fn rung_detail(rung: Rung) -> &'static str {
    match rung {
        Rung::ImageCopyCapture => "ext-image-copy-capture cursor session",
        Rung::PortalMetadata => "portal ScreenCast cursor metadata",
        Rung::HyprctlPoll => "hyprctl cursorpos polling",
    }
}

/// The cursor channel's state, straight from the ticket-33 rung
/// selection the daemon already made.
pub fn cursor_state(selection: &Selection) -> ChannelState {
    match selection {
        Selection::Rung(rung) => ChannelState::up(rung_detail(*rung)),
        Selection::Unsupported { missing } => {
            ChannelState::down(format!("unsupported - missing {}", missing.join(", ")))
        }
    }
}

/// The capture channel's state, straight from the backend selection. A
/// portal backend that has been selected but whose consent has not been
/// answered yet is *not* reported here — the daemon overwrites this row
/// with the consent outcome before the tray is ever published.
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

/// The popup channel's state. `advertised` is whether this session
/// advertises `zwlr_layer_shell_v1` at all, which is the honest verdict
/// available at startup - a bind that then fails anyway overwrites this
/// row through [`ChannelStatuses::set`] with what actually went wrong.
/// The down row names the global, because on stock GNOME that name is
/// the whole answer to "why is nothing appearing".
pub fn popup_state(advertised: bool) -> ChannelState {
    if advertised {
        ChannelState::up("wlr-layer-shell overlay surface")
    } else {
        ChannelState::down(format!("unsupported - missing {}", crate::wayland::LAYER_SHELL.interface))
    }
}

/// Every channel. Fixed-size — the set of channels is the app's shape,
/// not data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelStatuses {
    states: [ChannelState; 4],
}

impl ChannelStatuses {
    /// What the daemon knows once startup is done: `capture` is the
    /// already-resolved capture state (the daemon runs the backend
    /// selection and, for the portal backend, its eager consent before
    /// publishing the tray), the cursor rung was just selected, the
    /// trigger is the always-bound control socket, and `popup` is
    /// whether the layer shell this session advertises can carry a panel
    /// at all.
    pub fn startup(capture: ChannelState, cursor: &Selection, popup: ChannelState) -> ChannelStatuses {
        ChannelStatuses {
            states: [capture, cursor_state(cursor), ChannelState::up("control socket"), popup],
        }
    }

    /// A channel came up or went down; the tray re-renders from here.
    /// Returns whether this actually changed anything, so callers can
    /// log transitions rather than repeats.
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

    /// One disabled menu row: "Cursor: unsupported - missing …".
    pub fn row(&self, id: ChannelId) -> String {
        format!("{}: {}", id.label(), self.get(id).detail())
    }

    /// All rows, in `ChannelId::ALL` order.
    pub fn rows(&self) -> Vec<String> {
        ChannelId::ALL.iter().map(|&id| self.row(id)).collect()
    }

    /// The SNI `Status`: NeedsAttention exactly when a channel is down
    /// or serving spoiled (ticket 52's software cursor). Nothing else
    /// raises it - an icon parked on NeedsAttention teaches the user to
    /// ignore it, and both of those states name a fix in their row.
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

    /// The common case: the promptless capture backend was selected.
    fn screencopy() -> ChannelState {
        capture_state(&CaptureSelection::Backend(Backend::WlrScreencopy))
    }

    /// A wlr session: the layer shell is right there.
    fn shell() -> ChannelState {
        popup_state(true)
    }

    /// The daemon's startup knowledge, verbatim: the capture row names
    /// the resolved backend, the trigger is the control socket, the
    /// cursor row names the selected rung's mechanism, and the popup row
    /// names the shell that will carry the panel.
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

    /// Stock GNOME, as a tray host with the AppIndicator extension sees
    /// it (ticket 49): capture, cursor and trigger can all be perfectly
    /// healthy through the portals while the popup has nowhere to be
    /// drawn, and an all-Active icon there would be a lie. The row names
    /// the missing global, so the fix is legible without the log.
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

    /// Ticket 52: the compositor paints the pointer into the frames the
    /// backend copies. Lookups work, so the row still names the
    /// backend - and it names the option to change, and the icon asks
    /// to be looked at.
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

    /// A missing capability is the thing to fix first, so a defect
    /// cannot promote a dead channel into a serving one.
    #[test]
    fn a_down_channel_stays_down_when_a_defect_is_added() {
        let down = ChannelState::down("unsupported - missing zwlr_screencopy_manager_v1");
        assert_eq!(down.clone(), down.clone().degraded_by("pointer painted into frames"));
    }

    /// The ticket's headline contract: a down channel flips the SNI
    /// status to NeedsAttention, and recovery clears it.
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

    /// The denial path as the tray sees it: a refused portal is a
    /// capture row with the way back in it, and the icon says so.
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

    /// Today's real down case: the ticket-33 selection came back
    /// Unsupported, and the row names the exact missing capability so a
    /// compositor upgrade is an obvious fix.
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

    /// `set` replaces exactly the addressed channel, and reports whether
    /// anything moved so the daemon logs transitions only.
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

    /// Every rung has a one-line row text; a new rung must not silently
    /// inherit another's description.
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

    /// The backend selection maps onto three honest rows: either backend
    /// names its mechanism, and no backend names the gap.
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

    /// A session with no capture at all is a real alarm: hover cannot
    /// work, so the icon must say so.
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

    /// The row is read by a human in a menu, so it has to parse as one:
    /// no punctuation soup, no protocol dump.
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

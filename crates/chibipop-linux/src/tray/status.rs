//! The per-channel status registry behind the tray's disabled menu rows
//! (ADR-0006): what each input channel is doing right now, and the one
//! mapping from those states to menu-row text and the SNI `Status`.
//!
//! The registry is daemon-owned and works with or without a tray — it is
//! fed from what the daemon already knows (the ticket-33 cursor rung
//! selection and its live health, the always-bound control socket), and
//! later channel tickets flip states as their backends land. Nothing here
//! touches D-Bus, so all of it is testable without a tray host.

use crate::cursor::{Rung, Selection};

/// One monitored input channel, in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelId {
    Capture,
    Cursor,
    Trigger,
}

impl ChannelId {
    pub const ALL: [ChannelId; 3] = [ChannelId::Capture, ChannelId::Cursor, ChannelId::Trigger];

    pub fn label(self) -> &'static str {
        match self {
            ChannelId::Capture => "Capture",
            ChannelId::Cursor => "Cursor",
            ChannelId::Trigger => "Trigger",
        }
    }

    fn index(self) -> usize {
        match self {
            ChannelId::Capture => 0,
            ChannelId::Cursor => 1,
            ChannelId::Trigger => 2,
        }
    }
}

/// What one channel is doing. `detail` is the human half of the menu
/// row — short, honest, and naming the mechanism or the exact gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelState {
    /// Working; `detail` names what serves it ("control socket").
    Up { detail: String },
    /// Not implemented yet — an honest placeholder, not an error, so it
    /// must not raise NeedsAttention while the port is incomplete.
    NotBuilt { detail: String },
    /// Down; `detail` names exactly what is missing or denied, per the
    /// ADR-0006 example row "Cursor: portal denied — see settings".
    Down { detail: String },
}

impl ChannelState {
    pub fn up(detail: impl Into<String>) -> ChannelState {
        ChannelState::Up { detail: detail.into() }
    }

    pub fn not_built(detail: impl Into<String>) -> ChannelState {
        ChannelState::NotBuilt { detail: detail.into() }
    }

    pub fn down(detail: impl Into<String>) -> ChannelState {
        ChannelState::Down { detail: detail.into() }
    }

    fn detail(&self) -> &str {
        match self {
            ChannelState::Up { detail }
            | ChannelState::NotBuilt { detail }
            | ChannelState::Down { detail } => detail,
        }
    }
}

/// How a live cursor rung reads in a menu row. Deliberately not a method
/// on `Rung`: the ladder's own startup diagnostic is a paragraph naming
/// protocol globals (ADR-0003), and a menu row has one line.
pub fn rung_detail(rung: Rung) -> &'static str {
    match rung {
        Rung::ImageCopyCapture => "ext-image-copy-capture cursor session",
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

/// All three channels. Fixed-size — the set of channels is the app's
/// shape, not data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelStatuses {
    states: [ChannelState; 3],
}

impl ChannelStatuses {
    /// What the daemon knows at startup: capture has no backend wired
    /// into the loop yet, the cursor rung was just selected, and the
    /// trigger is the always-bound control socket.
    pub fn startup(cursor: &Selection) -> ChannelStatuses {
        ChannelStatuses {
            states: [
                ChannelState::not_built("not built yet"),
                cursor_state(cursor),
                ChannelState::up("control socket"),
            ],
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

    /// The SNI `Status`: NeedsAttention when any channel is down.
    /// NotBuilt stays Active — a feature that has not landed yet is not
    /// an alarm, and an icon parked on NeedsAttention teaches the user
    /// to ignore it.
    pub fn sni_status(&self) -> ksni::Status {
        if self.states.iter().any(|s| matches!(s, ChannelState::Down { .. })) {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon's startup knowledge, verbatim: capture is honestly
    /// "not built yet", the trigger is the control socket, and the
    /// cursor row names the selected rung's mechanism.
    #[test]
    fn startup_rows_name_what_the_daemon_knows() {
        let statuses = ChannelStatuses::startup(&Selection::Rung(Rung::HyprctlPoll));
        assert_eq!(
            vec![
                "Capture: not built yet".to_string(),
                "Cursor: hyprctl cursorpos polling".to_string(),
                "Trigger: control socket".to_string(),
            ],
            statuses.rows()
        );
    }

    /// The ticket's headline contract: a down channel flips the SNI
    /// status to NeedsAttention, and recovery clears it.
    #[test]
    fn any_down_channel_needs_attention() {
        let mut statuses = ChannelStatuses::startup(&Selection::Rung(Rung::ImageCopyCapture));
        assert_eq!(ksni::Status::Active, statuses.sni_status(), "all-up must be Active");

        assert!(statuses.set(ChannelId::Trigger, ChannelState::down("socket gone")));
        assert_eq!(ksni::Status::NeedsAttention, statuses.sni_status());
        assert_eq!("Trigger: socket gone", statuses.row(ChannelId::Trigger));

        assert!(statuses.set(ChannelId::Trigger, ChannelState::up("control socket")));
        assert_eq!(ksni::Status::Active, statuses.sni_status(), "recovery must clear it");
    }

    /// NotBuilt is informational: an unfinished channel must not park
    /// the icon on NeedsAttention forever.
    #[test]
    fn not_built_is_not_an_alarm() {
        let statuses = ChannelStatuses::startup(&Selection::Rung(Rung::ImageCopyCapture));
        assert!(matches!(statuses.get(ChannelId::Capture), ChannelState::NotBuilt { .. }));
        assert_eq!(ksni::Status::Active, statuses.sni_status());
    }

    /// Today's real down case: the ticket-33 selection came back
    /// Unsupported, and the row names the exact missing capability so a
    /// compositor upgrade is an obvious fix.
    #[test]
    fn unsupported_cursor_selection_maps_to_a_down_row() {
        let selection =
            Selection::Unsupported { missing: vec!["ext_image_copy_capture_manager_v1".to_string()] };
        let statuses = ChannelStatuses::startup(&selection);
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
        let mut statuses = ChannelStatuses::startup(&Selection::Rung(Rung::HyprctlPoll));
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
        let details = [Rung::ImageCopyCapture, Rung::HyprctlPoll].map(rung_detail);
        assert_ne!(details[0], details[1]);
        for detail in details {
            assert!(!detail.is_empty());
        }
    }
}

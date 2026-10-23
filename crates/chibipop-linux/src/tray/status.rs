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
}

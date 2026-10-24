//! This module reports whether this session paints the pointer into
//! the pixels that chibipop OCRs. It also holds the diagnostic that
//! says so out loud.
//!
//! Every capture request of chibipop asks for pixels *without* a
//! cursor. The wlr rung passes `overlay_cursor = 0` on both slots
//! (`session::WITHOUT_CURSOR`). The portal rung never sends
//! `cursor_mode = EMBEDDED`. Neither request helps when the
//! compositor drew the pointer into the framebuffer *before* the
//! copy. With a software cursor there is no separate pointer to omit.
//! There are only pixels that already contain one. The copy is then
//! faithful, and the frame holds an arrow. The arrow sits exactly on
//! the text of the lookup, and OCR reads the arrow as part of the
//! glyph.
//!
//! The author measured this effect on one desk (2026-08-26, Hyprland
//! 0.55.4, NVIDIA RTX 5080, `cursor:no_hardware_cursors = 1`). A
//! `chibipop capture-dump` of a 220x220 box around the pointer
//! contains the arrow. A change of the option to `false`, and a dump
//! of the same box with the pointer in the same place, contains no
//! arrow. One option gives the whole effect.
//!
//! Therefore the absence of the pointer is a *compositor* property,
//! and not a property that a client can request. The only honest act
//! for chibipop is to name the property. It prints a startup line and
//! a degraded Capture row that name the option to change. Silence
//! would give the user an app that reads the wrong character under
//! the cursor, and no way to know why.
//!
//! This probe is Hyprland-gated, like `cursor::hyprctl` (the one
//! identity exception). Hyprland is the compositor whose option the
//! author knows, whose default many NVIDIA setups override, and whose
//! `hyprctl` answers questions. On every other compositor the verdict
//! is [`PointerInFrames::Unknown`], and the probe prints nothing. A
//! guess would be worse than silence.

use crate::cursor::hyprctl;
use std::process::Command;

/// The Hyprland option that decides the answer.
pub const OPTION: &str = "cursor:no_hardware_cursors";

/// What the configuration of the compositor says about the pointer in
/// captured frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerInFrames {
    /// Hardware cursor planes. The display engine overlays the
    /// pointer after it composes the framebuffer. Therefore the
    /// pixels that screencopy copies never contain the pointer. There
    /// is nothing to say.
    Never,
    /// `no_hardware_cursors = 1`. The compositor renders the pointer
    /// into every frame. Every capture holds the pointer.
    Always,
    /// `no_hardware_cursors = 2` (auto, NVIDIA only). If the driver
    /// cannot give the compositor a plane, the compositor uses a
    /// software cursor instead. Therefore this session can hold the
    /// pointer in its frames, and the config alone cannot tell you.
    Maybe,
    /// Not a Hyprland session, or `hyprctl` did not answer.
    Unknown,
}

impl PointerInFrames {
    /// The startup line, when there is something to say.
    ///
    /// The line is exact and it names an action: the option, the
    /// value to set, and the one-line command that tries the value
    /// with no edit and no reload. The `Maybe` line gives information
    /// only. It names the check, and not a fix. `auto` is a problem
    /// only on the hardware that uses a software cursor. Many auto
    /// sessions do not need the change. A diagnostic that tells every
    /// auto session to change the setting teaches people to ignore
    /// the diagnostic.
    pub fn startup_line(self) -> Option<String> {
        match self {
            PointerInFrames::Always => Some(format!(
                "capture: {OPTION} = true, so this compositor paints the pointer into every frame \
                 chibipop captures - no capture request can remove it, and OCR of the text under \
                 the pointer will read the pointer too. Fix: set `{OPTION} = false` in \
                 hyprland.conf (try it now with `hyprctl keyword {OPTION} false`), or verify with \
                 `chibipop capture-dump --region X,Y,W,H` around the pointer"
            )),
            PointerInFrames::Maybe => Some(format!(
                "capture: {OPTION} = 2 (auto) - on a driver with no cursor plane the compositor \
                 paints the pointer into the frames chibipop OCRs. Check with `chibipop \
                 capture-dump --region X,Y,W,H` around the pointer; if the arrow is in the PNG, \
                 set `{OPTION} = false`"
            )),
            PointerInFrames::Never | PointerInFrames::Unknown => None,
        }
    }

    /// The text that the Capture status row adds when the pointer is
    /// certainly in the pixels. The backend still serves. Therefore
    /// the row keeps the backend name, and it adds the defect and the
    /// fix ([`crate::tray::status::ChannelState::degraded_by`]).
    ///
    /// Only the certain case degrades a row. `auto` gets the startup
    /// line and no permanent attention flag. An icon that stays on
    /// NeedsAttention for a maybe is an icon that nobody reads.
    pub fn row_defect(self) -> Option<String> {
        match self {
            PointerInFrames::Always => {
                Some(format!("pointer painted into frames - set {OPTION} = false"))
            }
            _ => None,
        }
    }
}

/// `hyprctl getoption <int option>` prints two lines:
///
/// ```text
/// int: 1
/// set: true
/// ```
///
/// The value matters. `set` only tells you whether the user wrote the
/// option in the config. A default has the same effect as a setting.
pub fn parse_getoption_int(out: &str) -> Option<i64> {
    out.lines()
        .find_map(|line| line.trim().strip_prefix("int:"))
        .and_then(|value| value.trim().parse().ok())
}

/// Reads the value of the option: `0` off, `1` on, `2` auto (NVIDIA only).
///
/// An unknown number gives `Unknown`, and not a guess. A future
/// Hyprland can add a value. A verdict invented for that value would
/// print a fix that the user does not need.
pub fn verdict(value: Option<i64>) -> PointerInFrames {
    match value {
        Some(0) => PointerInFrames::Never,
        Some(1) => PointerInFrames::Always,
        Some(2) => PointerInFrames::Maybe,
        _ => PointerInFrames::Unknown,
    }
}

/// Asks this session one time at startup. Every unexpected condition
/// gives `Unknown`. A missing binary, a session without Hyprland, or
/// an answer that does not parse must never cost a startup.
pub fn probe() -> PointerInFrames {
    if !hyprctl::available() {
        return PointerInFrames::Unknown;
    }
    verdict(read_option(OPTION))
}

fn read_option(option: &str) -> Option<i64> {
    // The reason is the same as in `hyprctl::sample`. No child
    // inherits the shutdown mask of the daemon.
    let out = crate::signals::unmasked(&mut Command::new("hyprctl"))
        .args(["getoption", option])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_getoption_int(std::str::from_utf8(&out.stdout).ok()?)
}

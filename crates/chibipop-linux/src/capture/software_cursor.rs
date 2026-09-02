//! Whether this session paints the pointer into the pixels we OCR, and
//! the diagnostic that says so out loud.
//!
//! Every capture request chibipop makes asks for pixels *without* a
//! cursor: the wlr rung passes `overlay_cursor = 0` on both slots
//! (`session::WITHOUT_CURSOR`) and the portal rung never sends
//! `cursor_mode = EMBEDDED`. Neither request can help when the
//! compositor drew the pointer into the framebuffer *before* the copy
//! happened: with a software cursor there is no separate pointer to
//! leave out, only pixels that already contain one. The copy is then
//! faithful and the frame has an arrow in it, sitting exactly on the
//! text being looked up, and OCR reads the arrow as part of the glyph.
//!
//! Measured on this desk (2026-08-26, Hyprland 0.55.4, NVIDIA RTX 5080,
//! `cursor:no_hardware_cursors = 1`): `chibipop capture-dump` of a
//! 220x220 box around the pointer contains the arrow; flipping the
//! option to `false` and dumping the same box with the pointer in the
//! same place contains no arrow. One option, the whole effect.
//!
//! So the pointer's absence is a *compositor* property, not something a
//! client can request, and the only honest thing chibipop can do is name
//! it: a startup line and a degraded Capture row that say which option
//! to change. Silence would leave the user with an app that reads the
//! wrong character under the cursor and no way to know why.
//!
//! Hyprland-gated, like `cursor::hyprctl` (the one identity exception):
//! it is the compositor whose option is known, whose default many
//! NVIDIA setups override, and whose `hyprctl` can be asked. Elsewhere
//! the verdict is [`PointerInFrames::Unknown`] and nothing is printed -
//! a guess would be worse than silence.

use crate::cursor::hyprctl;
use std::process::Command;

/// The Hyprland option that decides it.
pub const OPTION: &str = "cursor:no_hardware_cursors";

/// What the compositor's own configuration says about the pointer being
/// in captured frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerInFrames {
    /// Hardware cursor planes: the display engine overlays the pointer
    /// after the framebuffer is composed, so the pixels screencopy
    /// copies never contain it. Nothing to say.
    Never,
    /// `no_hardware_cursors = 1`: the compositor renders the pointer
    /// into every frame. Every capture we take has it.
    Always,
    /// `no_hardware_cursors = 2` (auto, NVIDIA only): the compositor
    /// falls back to a software cursor when the driver cannot give it a
    /// plane, so this session may or may not have the pointer in its
    /// frames and the config alone cannot say which.
    Maybe,
    /// Not a Hyprland session, or `hyprctl` did not answer.
    Unknown,
}

impl PointerInFrames {
    /// The startup line, when there is something to say.
    ///
    /// Actionable and exact: the option, the value to set, and the
    /// one-liner that tries it without an edit or a reload. The
    /// `Maybe` line is informational and names the check rather than a
    /// fix, because `auto` is only a problem on the hardware where it
    /// falls back - telling every auto session to change a setting it
    /// may not need is how a diagnostic teaches people to ignore it.
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

    /// What the Capture status row appends when the pointer is known to
    /// be in the pixels: the backend still serves, so the row keeps
    /// naming it and adds the defect and its fix
    /// ([`crate::tray::status::ChannelState::degraded_by`]).
    ///
    /// Only the certain case degrades a row. `auto` gets the startup
    /// line and no permanent attention flag: an icon parked on
    /// NeedsAttention for a maybe is an icon nobody reads.
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
/// The value is what matters; `set` only says whether the user wrote it
/// down, and a default is as effective as a setting.
pub fn parse_getoption_int(out: &str) -> Option<i64> {
    out.lines()
        .find_map(|line| line.trim().strip_prefix("int:"))
        .and_then(|value| value.trim().parse().ok())
}

/// Read the option's value: `0` off, `1` on, `2` auto (NVIDIA only).
///
/// An unknown number is `Unknown`, not a guess - a future Hyprland may
/// add a value, and inventing a verdict for it would print a fix the
/// user does not need.
pub fn verdict(value: Option<i64>) -> PointerInFrames {
    match value {
        Some(0) => PointerInFrames::Never,
        Some(1) => PointerInFrames::Always,
        Some(2) => PointerInFrames::Maybe,
        _ => PointerInFrames::Unknown,
    }
}

/// Ask this session, once, at startup. `Unknown` on anything unexpected:
/// a missing binary, a non-Hyprland session, or an answer that does not
/// parse must never cost a startup.
pub fn probe() -> PointerInFrames {
    if !hyprctl::available() {
        return PointerInFrames::Unknown;
    }
    verdict(read_option(OPTION))
}

fn read_option(option: &str) -> Option<i64> {
    // Same reason as `hyprctl::sample`: no child inherits the daemon's
    // shutdown mask.
    let out = crate::signals::unmasked(&mut Command::new("hyprctl"))
        .args(["getoption", option])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_getoption_int(std::str::from_utf8(&out.stdout).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `hyprctl` printed on the diagnosed session, verbatim.
    #[test]
    fn getoption_output_parses() {
        assert_eq!(Some(1), parse_getoption_int("int: 1\nset: true\n"));
        assert_eq!(Some(0), parse_getoption_int("int: 0\nset: true\n"));
        assert_eq!(Some(2), parse_getoption_int("int: 2\nset: false\n"));
        assert_eq!(None, parse_getoption_int("unknown option\n"));
        assert_eq!(None, parse_getoption_int(""));
        assert_eq!(None, parse_getoption_int("int: nonsense\n"), "a non-number is not a value");
    }

    #[test]
    fn each_option_value_maps_to_its_verdict() {
        assert_eq!(PointerInFrames::Never, verdict(Some(0)));
        assert_eq!(PointerInFrames::Always, verdict(Some(1)));
        assert_eq!(PointerInFrames::Maybe, verdict(Some(2)));
        assert_eq!(PointerInFrames::Unknown, verdict(None), "no answer is not a verdict");
        assert_eq!(PointerInFrames::Unknown, verdict(Some(7)), "nor is a value we do not know");
    }

    /// The measured case: the diagnostic must name the option, the value
    /// to set, and a way to see it for yourself.
    #[test]
    fn a_software_cursor_session_is_told_exactly_what_to_change() {
        let line = PointerInFrames::Always.startup_line().expect("a loud line");
        assert!(line.contains(OPTION), "names the option");
        assert!(line.contains("= false"), "names the value to set");
        assert!(line.contains("capture-dump"), "names how to see it");
        let defect = PointerInFrames::Always.row_defect().expect("a degraded row");
        assert!(defect.contains(OPTION));
    }

    /// Auto is a check, not a verdict: it says something, and it does
    /// not park the tray on NeedsAttention.
    #[test]
    fn an_auto_session_is_told_how_to_check_but_does_not_degrade_a_row() {
        let line = PointerInFrames::Maybe.startup_line().expect("an informational line");
        assert!(line.contains("capture-dump"), "names the check");
        assert_eq!(None, PointerInFrames::Maybe.row_defect());
    }

    /// Silence where there is nothing to report - including sessions
    /// this probe cannot read at all.
    #[test]
    fn hardware_cursors_and_unreadable_sessions_say_nothing() {
        for quiet in [PointerInFrames::Never, PointerInFrames::Unknown] {
            assert_eq!(None, quiet.startup_line(), "{quiet:?}");
            assert_eq!(None, quiet.row_defect(), "{quiet:?}");
        }
    }

    /// Off a Hyprland session there is no option to read, and the probe
    /// must not shell out looking for one.
    #[test]
    fn a_session_without_hyprland_probes_to_unknown() {
        if !hyprctl::available() {
            assert_eq!(PointerInFrames::Unknown, probe());
        }
    }
}

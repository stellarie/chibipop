//! Rung 3: `hyprctl cursorpos` adaptive polling (ADR-0003).
//!
//! Gated on HYPRLAND_INSTANCE_SIGNATURE — the one deliberate
//! identity-based exception to ADR-0002's "never compositor identity"
//! rule, documented in ADR-0003: ~50 lines, zero permissions, and it
//! covers Hyprland versions below the ext-image-copy-capture floor.
//!
//! Cadence per ADR-0010 (`cursor::budget`): 20 ms while the cursor
//! moves, decaying to ~150 ms after 5 s of quiet, bursting back to
//! 20 ms on the first observed move.

use super::budget;
use std::process::Command;
use std::time::Duration;

/// The identity gate. Set (non-empty) only inside a Hyprland session.
pub fn available() -> bool {
    std::env::var(HYPRLAND_ENV).is_ok_and(|v| !v.is_empty())
}

pub const HYPRLAND_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";

/// The adaptive cadence, pure so it is testable: how long until the
/// next poll, given how long the cursor has been still.
pub fn next_interval(since_last_move: Duration) -> Duration {
    if since_last_move >= budget::POLL_DECAY_AFTER {
        budget::POLL_IDLE
    } else {
        budget::POLL_ACTIVE
    }
}

/// `hyprctl cursorpos` prints `x, y` in logical layout coordinates.
pub fn parse_cursorpos(out: &str) -> Option<(i32, i32)> {
    let (x, y) = out.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// One sample, logical layout coordinates. `None` on any failure —
/// a missing binary or a hiccup must never kill the daemon.
pub fn sample() -> Option<(i32, i32)> {
    // `unmasked`: the daemon's blocked SIGINT/SIGTERM would otherwise be
    // this child's too (ticket 13), and a `hyprctl` that ever wedged
    // would then outlive the group kill meant to clear it.
    let out = crate::signals::unmasked(&mut Command::new("hyprctl")).arg("cursorpos").output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_cursorpos(std::str::from_utf8(&out.stdout).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cadence_decays_after_the_quiet_window() {
        assert_eq!(budget::POLL_ACTIVE, next_interval(Duration::ZERO));
        assert_eq!(
            budget::POLL_ACTIVE,
            next_interval(budget::POLL_DECAY_AFTER - Duration::from_millis(1))
        );
        assert_eq!(budget::POLL_IDLE, next_interval(budget::POLL_DECAY_AFTER));
        assert_eq!(budget::POLL_IDLE, next_interval(Duration::from_secs(60)));
    }

    #[test]
    fn cursorpos_output_parses() {
        assert_eq!(Some((646, 873)), parse_cursorpos("646, 873\n"));
        assert_eq!(Some((0, 0)), parse_cursorpos("0,0"));
        assert_eq!(Some((-5, 12)), parse_cursorpos("-5, 12"));
        assert_eq!(None, parse_cursorpos("unknown request"));
        assert_eq!(None, parse_cursorpos(""));
    }
}

//! Rung 3 of the cursor ladder (ARCHITECTURE.md#input-ladders) uses
//! an adaptive interval with `hyprctl cursorpos`.
//!
//! The code reads `HYPRLAND_INSTANCE_SIGNATURE` as a Hyprland signal.
//! This is the deliberate identity-based exception to the
//! "never compositor identity" rule
//! (ARCHITECTURE.md#capture-and-masking). A non-empty value does not
//! prove the current compositor identity.
//! The module has about 50 lines, needs zero permissions, and supports
//! Hyprland versions below the ext-image-copy-capture floor.
//!
//! `cursor::budget` defines this cadence (ARCHITECTURE.md#hover-cadence).
//! Poll every 20 ms while the cursor moves. After 5 s without a move,
//! poll about every 150 ms. Return to 20 ms after the first observed move.

use super::budget;
use std::process::Command;
use std::time::Duration;

/// Treat a non-empty value as a Hyprland signal.
/// It does not prove that the current compositor is Hyprland.
pub fn available() -> bool {
    std::env::var(HYPRLAND_ENV).is_ok_and(|v| !v.is_empty())
}

pub const HYPRLAND_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";

/// Return the next poll interval from the time since the last cursor move.
/// This pure function keeps cadence tests deterministic.
pub fn next_interval(since_last_move: Duration) -> Duration {
    if since_last_move >= budget::POLL_DECAY_AFTER {
        budget::POLL_IDLE
    } else {
        budget::POLL_ACTIVE
    }
}

/// Parse `hyprctl cursorpos` output as a logical layout coordinate pair.
pub fn parse_cursorpos(out: &str) -> Option<(i32, i32)> {
    let (x, y) = out.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Read one logical layout coordinate pair from `hyprctl cursorpos`.
/// Return `None` for every failure. A missing binary or command fault must
/// not stop the daemon.
pub fn sample() -> Option<(i32, i32)> {
    // Call `unmasked` so the child does not inherit the daemon's blocked
    // SIGINT/SIGTERM. A stuck `hyprctl` must not outlive the group kill
    // that clears the daemon.
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

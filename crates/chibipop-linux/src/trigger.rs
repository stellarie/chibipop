//! This module defines trigger hold transitions.
//!
//! The control socket has three trigger verbs.
//! See ARCHITECTURE.md#input-ladders.
//! A key hold freezes one press-time grab because the popup can cover the word
//! during the short hold, and the frozen grab reads through the popup.
//! A toggle latch reads live grabs with the popup masked, like Live mode.
//! A latch can last minutes while the screen changes under it, so it must not
//! keep stale pixels.
//! See ARCHITECTURE.md#hover-cadence.
//!
//! This module returns decisions and causes no platform effect.

use crate::capture::geometry;
use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{PhysPoint, PhysRect};

/// One active trigger hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hold {
    /// This output matters only for a frozen hold.
    /// A cursor on another output needs a new frozen grab.
    /// See ARCHITECTURE.md#hover-cadence.
    pub output: PhysRect,
    /// A latch reads live grabs with the popup masked.
    /// It survives `trigger-up` until `toggle` releases it.
    pub latched: bool,
}

/// One change to the trigger hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Get a press-time frozen grab of the output under the cursor.
    Freeze,
    /// Start or keep a live-grab latch until `toggle` releases it.
    Latch,
    /// Drop the frozen grab or release the live latch, and retract the popup.
    Release,
    /// Keep the hold unchanged. The value gives the log reason.
    Nothing(&'static str),
}

/// `trigger-down` freezes a new hold, unless a latch already exists.
pub fn down(hold: Option<Hold>) -> Step {
    if hold.is_some_and(|h| h.latched) { Step::Latch } else { Step::Freeze }
}

/// `trigger-up` ends an unlatched hold.
///
/// It does not end a hold that `toggle` latched.
/// The latch lets the user release the key while live grabs continue.
pub fn up(hold: Option<Hold>) -> Step {
    match hold {
        Some(h) if h.latched => Step::Nothing("a toggle holds the live grab; toggle again to end it"),
        Some(_) => Step::Release,
        None => Step::Nothing("nothing is held"),
    }
}

/// `toggle` starts a live latch or releases the current hold.
///
/// A latch can last minutes while the screen changes under it, so it reads live
/// grabs with the popup masked, like Live mode. A key hold still freezes because
/// the popup can cover the word for the short hold, and the frozen grab reads through it.
pub fn toggle(hold: Option<Hold>) -> Step {
    match hold {
        Some(_) => Step::Release,
        None => Step::Latch,
    }
}

/// Return a new output when the cursor leaves the output of the frozen grab.
///
/// A different output needs one fresh full frozen grab while the hold lasts.
/// See ARCHITECTURE.md#hover-cadence.
/// This rule lets the user hold the trigger and inspect another monitor.
/// The current frozen grab remains when the cursor stays on the same output.
pub fn regrab(hold: Hold, geoms: &[OutputGeometry], pos: PhysPoint) -> Option<PhysRect> {
    if hold.output.contains(pos) {
        return None;
    }
    // `bounds_containing` returns the nearest output for a cursor outside all outputs.
    // Therefore, a cursor in a monitor gap can stay on the output of the frozen grab.
    let entered = geometry::bounds_containing(geoms, pos);
    (entered != hold.output).then_some(entered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: PhysRect = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };
    const RIGHT: PhysRect = PhysRect { x: 1920, y: 0, w: 1920, h: 1080 };

    fn held(output: PhysRect, latched: bool) -> Option<Hold> {
        Some(Hold { output, latched })
    }

    /// Return two adjacent 1920x1080 outputs at scale 1.
    /// This test data checks output changes without a second physical monitor.
    fn two_outputs() -> Vec<OutputGeometry> {
        [0, 1920]
            .into_iter()
            .map(|x| OutputGeometry {
                logical_x: x,
                logical_y: 0,
                logical_w: 1920,
                logical_h: 1080,
                mode_w: 1920,
                mode_h: 1080,
                transform_swaps: false,
            })
            .collect()
    }

    #[test]
    fn a_press_freezes_and_a_release_ends_it() {
        assert_eq!(Step::Freeze, down(None));
        assert_eq!(Step::Release, up(held(LEFT, false)));
    }

    /// A press while a latched hold must preserve the latch.
    #[test]
    fn a_press_during_a_latched_hold_keeps_the_latch() {
        assert_eq!(Step::Latch, down(held(LEFT, true)));
        assert_eq!(Step::Freeze, down(held(LEFT, false)));
    }

    #[test]
    fn a_release_under_a_toggle_latch_changes_nothing() {
        assert!(matches!(up(held(LEFT, true)), Step::Nothing(_)));
    }

    #[test]
    fn a_release_with_nothing_held_changes_nothing() {
        assert!(matches!(up(None), Step::Nothing(_)));
    }

    /// A toggle starts a live latch and ends it at toggle-off.
    #[test]
    fn toggle_latches_live_until_it_is_toggled_off() {
        assert_eq!(Step::Latch, toggle(None));
        assert_eq!(Step::Release, toggle(held(LEFT, true)));
    }

    /// `toggle` also ends an unlatched hold, so one verb has one visible result.
    #[test]
    fn toggle_ends_a_hold_a_press_started() {
        assert_eq!(Step::Release, toggle(held(LEFT, false)));
    }

    #[test]
    fn a_move_within_the_frozen_output_takes_no_new_grab() {
        let geoms = two_outputs();
        let hold = Hold { output: LEFT, latched: false };
        assert_eq!(None, regrab(hold, &geoms, PhysPoint { x: 1919, y: 1079 }));
    }

    #[test]
    fn crossing_onto_the_other_output_grabs_it() {
        let geoms = two_outputs();
        let hold = Hold { output: LEFT, latched: false };
        assert_eq!(Some(RIGHT), regrab(hold, &geoms, PhysPoint { x: 1920, y: 500 }));
    }

    /// A cursor outside every output resolves to the nearest output.
    /// A monitor gap does not need another frozen grab.
    #[test]
    fn a_cursor_off_every_output_keeps_the_grab_it_has() {
        let geoms = two_outputs();
        let hold = Hold { output: LEFT, latched: false };
        assert_eq!(None, regrab(hold, &geoms, PhysPoint { x: 40, y: 4000 }));
    }
}

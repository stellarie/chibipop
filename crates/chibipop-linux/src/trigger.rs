//! This module defines trigger hold transitions and requests for a new frozen grab.
//!
//! The control socket has three trigger verbs.
//! See ARCHITECTURE.md#input-ladders.
//! Trigger mode gets its frozen grab at key press.
//! See ARCHITECTURE.md#hover-cadence.
//!
//! This module returns decisions and causes no platform effect.
//! The daemon owns the cursor, the Worker, and the popup.
//! Tests can check the transition rules on a system with one output.

use crate::capture::geometry;
use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{PhysPoint, PhysRect};

/// One active trigger hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hold {
    /// The press-time frozen grab covers this output.
    /// A cursor on another output needs a new frozen grab.
    /// See ARCHITECTURE.md#hover-cadence.
    pub output: PhysRect,
    /// A `toggle` started this hold, so `trigger-up` must not end it.
    pub latched: bool,
}

/// One change to the trigger hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Get a new frozen grab of the output under the cursor.
    /// A press while a latched hold keeps the latch.
    Freeze { latched: bool },
    /// Drop the frozen grab and retract the popup.
    Release,
    /// Keep the hold unchanged. The value gives the log reason.
    Nothing(&'static str),
}

/// `trigger-down` always requests a new frozen grab.
pub fn down(hold: Option<Hold>) -> Step {
    Step::Freeze { latched: hold.is_some_and(|h| h.latched) }
}

/// `trigger-up` ends an unlatched hold.
///
/// It does not end a hold that `toggle` latched.
/// The latch lets the user release the key and keep the frozen grab.
pub fn up(hold: Option<Hold>) -> Step {
    match hold {
        Some(h) if h.latched => Step::Nothing("a toggle holds the freeze; toggle again to end it"),
        Some(_) => Step::Release,
        None => Step::Nothing("nothing is held"),
    }
}

/// `toggle` gets a frozen grab at toggle-on and drops it at toggle-off.
pub fn toggle(hold: Option<Hold>) -> Step {
    match hold {
        Some(_) => Step::Release,
        None => Step::Freeze { latched: true },
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
        assert_eq!(Step::Freeze { latched: false }, down(None));
        assert_eq!(Step::Release, up(held(LEFT, false)));
    }

    /// Each press requests a new frozen grab.
    /// A press while a latched hold must preserve the latch.
    #[test]
    fn a_press_during_a_latched_hold_keeps_the_latch() {
        assert_eq!(Step::Freeze { latched: true }, down(held(LEFT, true)));
        assert_eq!(Step::Freeze { latched: false }, down(held(LEFT, false)));
    }

    #[test]
    fn a_release_under_a_toggle_latch_changes_nothing() {
        assert!(matches!(up(held(LEFT, true)), Step::Nothing(_)));
    }

    #[test]
    fn a_release_with_nothing_held_changes_nothing() {
        assert!(matches!(up(None), Step::Nothing(_)));
    }

    /// A latch starts at toggle-on and ends at toggle-off.
    #[test]
    fn toggle_freezes_until_it_is_toggled_off() {
        assert_eq!(Step::Freeze { latched: true }, toggle(None));
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

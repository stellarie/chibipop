//! Trigger mode's hold: what the control socket's three trigger verbs do
//! to it, and when it needs a fresh press-time grab. The verb set lives
//! at ARCHITECTURE.md#input-ladders, the freeze at
//! ARCHITECTURE.md#hover-cadence.
//!
//! All decision, no effect: the daemon owns the cursor, the Worker and the
//! popup, and this owns the rules about them. That is what makes "a
//! release must not undo a toggle" and "crossing outputs re-grabs" things
//! a test can pin on a single-head machine.

use crate::capture::geometry;
use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{PhysPoint, PhysRect};

/// One trigger hold, while it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hold {
    /// The box the press-time grab covers. A cursor leaving it has
    /// crossed onto another output, which is the one thing mid-hold that
    /// takes a fresh grab (ARCHITECTURE.md#hover-cadence).
    pub output: PhysRect,
    /// Started by `toggle`, so a key release must not end it.
    pub latched: bool,
}

/// What a verb does to the hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Grab the output under the cursor and hold it. `latched` carries
    /// through: a press during a toggle-hold re-grabs without dropping
    /// the latch.
    Freeze { latched: bool },
    /// Drop the frozen frame and retract the popup.
    Release,
    /// Nothing, and why - the log says which.
    Nothing(&'static str),
}

/// `trigger-down`: every press grabs fresh.
pub fn down(hold: Option<Hold>) -> Step {
    Step::Freeze { latched: hold.is_some_and(|h| h.latched) }
}

/// `trigger-up`: ends the hold, unless a `toggle` latched it.
///
/// A latched freeze outliving the key is the whole point of `toggle`:
/// the user pressed the bind once to stop holding it.
pub fn up(hold: Option<Hold>) -> Step {
    match hold {
        Some(h) if h.latched => Step::Nothing("a toggle holds the freeze; toggle again to end it"),
        Some(_) => Step::Release,
        None => Step::Nothing("nothing is held"),
    }
}

/// `toggle`: freeze at toggle-on, stay frozen until toggle-off.
pub fn toggle(hold: Option<Hold>) -> Step {
    match hold {
        Some(_) => Step::Release,
        None => Step::Freeze { latched: true },
    }
}

/// The fresh grab a moved cursor needs, or `None` while it is still on
/// the output the hold already froze.
///
/// Crossing outputs mid-hold takes one fresh full grab of the entered
/// output (ARCHITECTURE.md#hover-cadence) - "hold and glance at the
/// other monitor" works, and a dead second monitor would read as a bug.
/// Staying put costs nothing, which is what keeps a hold at one copy.
pub fn regrab(hold: Hold, geoms: &[OutputGeometry], pos: PhysPoint) -> Option<PhysRect> {
    if hold.output.contains(pos) {
        return None;
    }
    // Off every output: `bounds_containing` answers with the nearest, so
    // a cursor in a gap between monitors is still the output we hold.
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

    /// Two 1920x1080 outputs side by side at scale 1. This box has one
    /// monitor, so the crossing rule is pinned here rather than live.
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

    /// Each press grabs fresh, so a press during a hold is a new grab -
    /// but it must not silently cancel a toggle.
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

    /// The latch: toggle on, stay frozen, toggle off.
    #[test]
    fn toggle_freezes_until_it_is_toggled_off() {
        assert_eq!(Step::Freeze { latched: true }, toggle(None));
        assert_eq!(Step::Release, toggle(held(LEFT, true)));
    }

    /// A toggle also ends a plain hold: one verb, one visible state.
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

    /// A cursor off every output resolves to the nearest one, so a gap
    /// between monitors is not a reason to re-copy what we hold.
    #[test]
    fn a_cursor_off_every_output_keeps_the_grab_it_has() {
        let geoms = two_outputs();
        let hold = Hold { output: LEFT, latched: false };
        assert_eq!(None, regrab(hold, &geoms, PhysPoint { x: 40, y: 4000 }));
    }
}

//! Hybrid damage pacing, as a state machine with no Wayland in it. See
//! ARCHITECTURE.md#capture-and-masking and
//! ARCHITECTURE.md#hover-cadence.
//!
//! The problem: a plain `copy` forces the compositor to repaint, and
//! `copy_with_damage` answers only when something changed - which on a
//! still desktop is never, and `RegionCapture::grab` may never block on
//! damage. So the backend runs one `copy_with_damage` as a *race*
//! against a deadline and treats the timeout as information: nothing
//! changed.
//!
//! The race is armed on the union of the regions a read grabbed, at
//! `end_read`, and is deliberately *retained* across reads while the
//! screen stays still. That is what makes a dwelling hover cost one
//! deadline wakeup per period and zero copies (the power budget): the
//! next read waits on the race already in flight instead of asking
//! for anything.
//!
//! One verdict serves a whole read. The first cached region settles the
//! race; every later pass in that read trusts the answer, so a
//! four-tile read on a static screen waits once, not four times. The
//! verdict is honest for the whole read because the race watches the
//! union of exactly the boxes that read looks at.
//!
//! A read that never repeats a region - the cursor moved - never
//! touches the race at all and pays no deadline: plain copies only, as
//! before.
//!
//! **What the two compositors actually do** (measured, ticket 30):
//! wlroots scopes damage to the frame's box, so a static screen really
//! does answer at the deadline and cost zero copies - and real damage
//! wins the race early, at 108 ms of a 250 ms deadline in the smoke
//! run. Hyprland 0.55.4 fires `copy_with_damage` on essentially every
//! output commit, so on a live desktop the verdict is `Damaged` at
//! about one frame's latency and the dwell degrades to a plain copy per
//! read - never stale, never blocked, just no saving. The invariants
//! hold either way; the power win is a wlroots-and-sway win today.

use chibipop::geom::PhysRect;
use std::time::Duration;

/// The dwell deadline: at most four wakeups a second while dwelling
/// on a static screen.
pub const DWELL_DEADLINE: Duration = Duration::from_millis(250);

/// A plain copy waits for one repaint, not for damage. This only
/// bounds a compositor that never answers at all, so it is far longer
/// than a frame and still short of a hover feeling hung.
pub const COPY_DEADLINE: Duration = Duration::from_millis(400);

/// Arming the race is pure saving, so it may never cost a hover:
/// enumerating a frame's buffer takes microseconds, and a compositor
/// slower than this loses its damage pacing rather than delaying the
/// popup that has already been read.
pub const ARM_DEADLINE: Duration = Duration::from_millis(50);

/// What the damage race said about the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The deadline passed with no damage: the last pixels still hold.
    Static,
    /// Damage arrived (or there was no race to ask).
    Damaged,
}

/// What one `grab` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Ask the compositor for these pixels now.
    Copy,
    /// Settle the race first, then step again.
    Settle,
    /// Serve the pixels already held; the screen has not changed.
    Serve,
}

/// What `end_read` should do with the race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// The race in flight already watches this read: cost nothing.
    Keep,
    /// Arm a fresh race on this box.
    Rearm(PhysRect),
    /// Nothing was read; drop the race.
    Disarm,
}

/// The pacing state: what this read has touched, and what the race in
/// flight is watching.
#[derive(Debug, Default)]
pub struct Pacer {
    /// This read's verdict, once one region has settled it.
    verdict: Option<Verdict>,
    /// Union of every region this read asked for.
    touched: Option<PhysRect>,
    /// The box the race in flight watches, if one is armed.
    watching: Option<PhysRect>,
}

impl Pacer {
    /// Start a read: last read's verdict no longer applies.
    pub fn begin_read(&mut self) {
        self.verdict = None;
        self.touched = None;
    }

    /// What to do for `region`, given whether its pixels are held.
    pub fn step(&mut self, region: PhysRect, cached: bool) -> Step {
        self.touched = Some(match self.touched {
            Some(u) => super::geometry::cover(u, region),
            None => region,
        });
        if !cached {
            return Step::Copy;
        }
        match self.verdict {
            None => Step::Settle,
            Some(Verdict::Static) => Step::Serve,
            Some(Verdict::Damaged) => Step::Copy,
        }
    }

    /// Record what the race answered; it holds for this whole read.
    pub fn settled(&mut self, verdict: Verdict) {
        self.verdict = Some(verdict);
    }

    /// The race is gone - fired, failed, or never armed.
    pub fn disarmed(&mut self) {
        self.watching = None;
    }

    /// End a read: keep the race in flight, or arm one on what this
    /// read actually looked at.
    pub fn end_read(&mut self, armed: bool) -> Arm {
        let verdict = self.verdict.take();
        let Some(union) = self.touched.take() else {
            self.watching = None;
            return Arm::Disarm;
        };
        // A still-pending race on exactly this box is the whole point:
        // re-arming it would cost a copy for nothing.
        if armed && self.watching == Some(union) && verdict != Some(Verdict::Damaged) {
            return Arm::Keep;
        }
        self.watching = Some(union);
        Arm::Rearm(union)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PhysRect {
        PhysRect { x, y, w, h }
    }

    /// The three regions one tiled read asks for.
    const PASS1: PhysRect = PhysRect { x: 100, y: 100, w: 400, h: 200 };
    const TILE1: PhysRect = PhysRect { x: 500, y: 120, w: 300, h: 160 };
    const TILE2: PhysRect = PhysRect { x: 800, y: 120, w: 300, h: 160 };

    #[test]
    fn the_first_read_copies_everything_and_arms_the_union() {
        let mut p = Pacer::default();
        p.begin_read();
        assert_eq!(p.step(PASS1, false), Step::Copy);
        assert_eq!(p.step(TILE1, false), Step::Copy);
        assert_eq!(p.step(TILE2, false), Step::Copy);
        let union = super::super::geometry::cover(super::super::geometry::cover(PASS1, TILE1), TILE2);
        assert_eq!(p.end_read(false), Arm::Rearm(union));
    }

    #[test]
    fn a_static_second_read_settles_once_then_serves() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.step(TILE1, false);
        p.end_read(false);

        p.begin_read();
        assert_eq!(p.step(PASS1, true), Step::Settle, "the first cached region settles the race");
        p.settled(Verdict::Static);
        assert_eq!(p.step(PASS1, true), Step::Serve);
        assert_eq!(p.step(TILE1, true), Step::Serve, "one verdict serves the whole read");
    }

    /// The power-budget claim: a dwell on a static screen asks the
    /// compositor for nothing at all.
    #[test]
    fn a_static_dwell_keeps_the_race_and_never_copies() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.step(TILE1, false);
        assert!(matches!(p.end_read(false), Arm::Rearm(_)));

        for _ in 0..5 {
            p.begin_read();
            assert_eq!(p.step(PASS1, true), Step::Settle);
            p.settled(Verdict::Static);
            assert_eq!(p.step(PASS1, true), Step::Serve);
            assert_eq!(p.step(TILE1, true), Step::Serve);
            assert_eq!(p.end_read(true), Arm::Keep, "a pending race must not be re-armed");
        }
    }

    #[test]
    fn damage_makes_the_read_copy_again_and_rearm() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.step(TILE1, false);
        p.end_read(false);

        p.begin_read();
        assert_eq!(p.step(PASS1, true), Step::Settle);
        p.settled(Verdict::Damaged);
        assert_eq!(p.step(PASS1, true), Step::Copy);
        assert_eq!(p.step(TILE1, true), Step::Copy, "damage anywhere restages every pass");
        // The race fired, so nothing is in flight to keep.
        assert!(matches!(p.end_read(false), Arm::Rearm(_)));
    }

    /// A fired race must never be kept even if the caller still claims
    /// one is armed: its pixels are already spent.
    #[test]
    fn a_damaged_verdict_never_keeps_the_race() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.end_read(false);
        p.begin_read();
        p.step(PASS1, true);
        p.settled(Verdict::Damaged);
        assert_eq!(p.end_read(true), Arm::Rearm(PASS1));
    }

    #[test]
    fn an_uncached_region_is_copied_whatever_the_verdict() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.settled(Verdict::Static);
        assert_eq!(p.step(TILE2, false), Step::Copy, "pixels we do not hold cannot be served");
    }

    #[test]
    fn a_moved_cursor_pays_no_deadline_and_rearms_elsewhere() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.end_read(false);

        // The cursor moved: nothing cached matches, so nothing settles.
        p.begin_read();
        let moved = rect(2000, 400, 400, 200);
        assert_eq!(p.step(moved, false), Step::Copy);
        assert_eq!(p.end_read(true), Arm::Rearm(moved), "the race must follow the eyes");
    }

    #[test]
    fn an_empty_read_disarms() {
        let mut p = Pacer::default();
        p.begin_read();
        assert_eq!(p.end_read(true), Arm::Disarm);
    }

    #[test]
    fn a_disarmed_race_is_armed_again_next_read() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        assert_eq!(p.end_read(false), Arm::Rearm(PASS1));
        p.disarmed();
        p.begin_read();
        p.step(PASS1, true);
        p.settled(Verdict::Static);
        assert_eq!(p.end_read(false), Arm::Rearm(PASS1));
    }

    /// Grabs outside a read bracket (the `probe`-style single shot)
    /// still work: they just never use the race.
    #[test]
    fn unbracketed_grabs_settle_and_copy() {
        let mut p = Pacer::default();
        assert_eq!(p.step(PASS1, false), Step::Copy);
        assert_eq!(p.step(PASS1, true), Step::Settle);
        // No race exists, so the backend reports damage and copies.
        p.settled(Verdict::Damaged);
        assert_eq!(p.step(PASS1, true), Step::Copy);
    }

    #[test]
    fn the_watched_box_grows_to_cover_served_regions_too() {
        let mut p = Pacer::default();
        p.begin_read();
        p.step(PASS1, false);
        p.end_read(false);

        p.begin_read();
        p.step(PASS1, true);
        p.settled(Verdict::Static);
        p.step(PASS1, true);
        // A tile that was served must still be watched next period.
        p.step(TILE2, false);
        let union = super::super::geometry::cover(PASS1, TILE2);
        assert_eq!(p.end_read(true), Arm::Rearm(union));
    }
}

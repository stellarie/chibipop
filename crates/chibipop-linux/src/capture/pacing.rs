//! This module implements hybrid damage pacing as a state machine without Wayland.
//! See ARCHITECTURE.md#capture-and-masking and
//! ARCHITECTURE.md#hover-cadence.
//!
//! A plain `copy` makes the compositor repaint. `copy_with_damage` answers only
//! after a change. A still desktop therefore gives no answer, and
//! `RegionCapture::grab` must not block on damage. The backend races one
//! `copy_with_damage` against a deadline. A timeout means that nothing changed.
//!
//! The backend arms the race on the union of regions that a read grabs, at
//! `end_read`. It keeps the race across reads while the screen stays still.
//! A hover then needs one deadline wakeup per period and zero copies.
//! The next read waits on the active race instead of a new copy request.
//!
//! One verdict serves one whole read. The first cached region settles the
//! race. Each later pass in that read uses the same verdict. A four-tile
//! read on a static screen then waits once, not four times. The race watches
//! the union of all boxes that the read touches, so the verdict covers the read.
//!
//! A read with no repeated region means that the cursor moved. It does not
//! use the race or pay a deadline. It uses plain copies only, as before.
//!
//! **What the two compositors actually do** (measured):
//! wlroots limits damage to the frame box. A static screen therefore answers
//! at the deadline and costs zero copies. Real damage wins early, at 108 ms
//! of the 250 ms deadline in the smoke run. Hyprland 0.55.4 fires
//! `copy_with_damage` on essentially every output commit. A live desktop then gives
//! `Damaged` at about one frame's latency, and a dwell uses one plain copy per
//! read. It never returns stale pixels or blocks, but it gives no power benefit.
//! The invariants hold in both cases. The power win applies to wlroots and sway
//! today.

use chibipop::geom::PhysRect;
use std::time::Duration;

/// The dwell deadline. A static screen causes at most four wakeups per second.
pub const DWELL_DEADLINE: Duration = Duration::from_millis(250);

/// The plain-copy deadline. A plain copy waits for one repaint, not for damage.
/// This bounds a compositor that gives no answer. It lasts longer than a frame but
/// remains short enough to avoid a hover that feels hung.
pub const COPY_DEADLINE: Duration = Duration::from_millis(400);

/// The arm deadline. Frame buffer enumeration takes microseconds, so this setup
/// must not delay a popup that already has a result. A slower compositor loses
/// damage pacing instead.
pub const ARM_DEADLINE: Duration = Duration::from_millis(50);

/// The result from the damage race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The deadline passed without damage. The held pixels still match the screen.
    Static,
    /// Damage arrived, or no race was available.
    Damaged,
}

/// The operation for one `grab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Ask the compositor for these pixels.
    Copy,
    /// Settle the active damage race, then choose the next step.
    Settle,
    /// Serve the held pixels. The screen has not changed.
    Serve,
}

/// The operation for `end_read` to apply to the damage race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// The active race already watches this read. Keep it without a copy.
    Keep,
    /// Arm a new race on this box.
    Rearm(PhysRect),
    /// No region was read. Drop the race.
    Disarm,
}

/// Pacing state for this read and for the box that the active race watches.
#[derive(Debug, Default)]
pub struct Pacer {
    /// Verdict for this read after one region settles the race.
    verdict: Option<Verdict>,
    /// Union of every region that this read asks for.
    touched: Option<PhysRect>,
    /// Box that the active race watches, if one exists.
    watching: Option<PhysRect>,
}

impl Pacer {
    /// Start a read. Clear the verdict from the prior read.
    pub fn begin_read(&mut self) {
        self.verdict = None;
        self.touched = None;
    }

    /// Choose the operation for `region` based on whether its pixels are held.
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

    /// Store the race verdict for this whole read.
    pub fn settled(&mut self, verdict: Verdict) {
        self.verdict = Some(verdict);
    }

    /// Mark the race as absent after it fires, fails, or never arms.
    pub fn disarmed(&mut self) {
        self.watching = None;
    }

    /// End a read. Keep the active race or arm one for the regions that it read.
    pub fn end_read(&mut self, armed: bool) -> Arm {
        let verdict = self.verdict.take();
        let Some(union) = self.touched.take() else {
            self.watching = None;
            return Arm::Disarm;
        };
        // Keep an active race when it watches this exact box. Re-arm it only
        // when the box changes or damage reaches it.
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

    /// Three regions that one tiled read asks for.
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

    /// The power budget: a dwell on a static screen sends no request to the compositor.
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
        // The race fired. Nothing remains in flight to keep.
        assert!(matches!(p.end_read(false), Arm::Rearm(_)));
    }

    /// Never keep a fired race, even when the caller reports that one remains armed.
    /// Its pixels are already spent.
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

        // The cursor moved. No held region matches, so no race can settle.
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

    /// A grab outside a read bracket, such as a single `probe`, still works.
    /// It does not use the race.
    #[test]
    fn unbracketed_grabs_settle_and_copy() {
        let mut p = Pacer::default();
        assert_eq!(p.step(PASS1, false), Step::Copy);
        assert_eq!(p.step(PASS1, true), Step::Settle);
        // No race exists. Report damage and copy.
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
        // Keep a served tile in the watched box for the next period.
        p.step(TILE2, false);
        let union = super::super::geometry::cover(PASS1, TILE2);
        assert_eq!(p.end_read(true), Arm::Rearm(union));
    }
}

//! The press-time frame trigger mode reads (ARCHITECTURE.md#hover-cadence).
//!
//! At trigger press one full grab of the output under the cursor is taken,
//! before any popup exists, and every lookup in the hold is served out of
//! it: no capture, no mask, and text the popup later covers stays
//! readable. That is the read-through property Windows gets from its
//! capture guard and a live Wayland grab cannot have
//! (ARCHITECTURE.md#capture-and-masking). Release drops the frame; each
//! press grabs fresh.
//!
//! Nothing here decides *when* to freeze. The bin owns the trigger and the
//! output-crossing rule; this owns the pixels and answers boxes out of
//! them.

use crate::geom::{PhysPoint, PhysRect};
use crate::text::{Frame, RegionCapture};
use anyhow::{Context, Result};

/// One trigger hold's frozen pixels.
pub(crate) struct FrozenFrame {
    /// The output box these pixels cover, physical.
    region: PhysRect,
    /// BGRA8, `region.w * region.h * 4`, top-down: [`Frame`]'s format.
    buf: Vec<u8>,
    /// Which backend path produced them, and why it was not the
    /// preferred one: provenance survives the freeze, so a crop taken
    /// twenty lookups later still says where its pixels came from.
    source: &'static str,
    fallback: Option<String>,
    /// Boxes already served out of this frame. A repeat is the same
    /// pixels by construction, so it can say `unchanged` honestly and
    /// the pipeline skips an OCR pass it has already paid for.
    served: Vec<PhysRect>,
}

impl FrozenFrame {
    /// One full grab of the output holding `at`.
    ///
    /// Bracketed as one read like any other, so a backend that hides
    /// its own surfaces or holds an expensive session open does it here
    /// too. The grab is the whole output on purpose: presses arrive at
    /// human cadence, so the copy is free and no out-of-region edge
    /// case exists (ARCHITECTURE.md#hover-cadence).
    pub(crate) fn take(capture: &mut dyn RegionCapture, at: PhysPoint) -> Result<FrozenFrame> {
        let region = capture.bounds_containing(at);
        capture.begin_read();
        let grabbed = capture.grab(region);
        capture.end_read();
        let frame = grabbed.with_context(|| {
            format!(
                "the trigger-press grab of {}x{} at {},{} failed",
                region.w, region.h, region.x, region.y
            )
        })?;
        let need = usize::try_from(i64::from(region.w) * i64::from(region.h) * 4)
            .ok()
            .with_context(|| format!("a {}x{} output is too large to freeze", region.w, region.h))?;
        anyhow::ensure!(
            frame.w == region.w && frame.h == region.h && frame.buf.len() >= need,
            "the trigger-press grab answered {}x{} in {} bytes, wanted {}x{} in {need}",
            frame.w,
            frame.h,
            frame.buf.len(),
            region.w,
            region.h,
        );
        Ok(FrozenFrame {
            region,
            buf: frame.buf,
            source: frame.source,
            fallback: frame.fallback,
            served: Vec::new(),
        })
    }

    /// The box these pixels cover.
    pub(crate) fn region(&self) -> PhysRect {
        self.region
    }

    /// `region`'s pixels out of the frozen frame.
    ///
    /// Black where the frame does not reach: a frozen output has no
    /// pixels past its own edge, and a blank margin costs one lookup
    /// nothing where failing the grab would cost every lookup near an
    /// edge - the same answer the portal rung gives off-monitor.
    pub(crate) fn crop(&mut self, region: PhysRect) -> Frame {
        let (w, h) = (region.w.max(0) as usize, region.h.max(0) as usize);
        let mut buf = vec![0u8; w * h * 4];
        if let Some(hit) = self.region.intersection(region) {
            let src_stride = self.region.w as usize * 4;
            let dst_stride = w * 4;
            let bytes = hit.w as usize * 4;
            let src_x = (hit.x - self.region.x) as usize * 4;
            let dst_x = (hit.x - region.x) as usize * 4;
            for row in 0..hit.h as usize {
                let s = ((hit.y - self.region.y) as usize + row) * src_stride + src_x;
                let d = ((hit.y - region.y) as usize + row) * dst_stride + dst_x;
                buf[d..d + bytes].copy_from_slice(&self.buf[s..s + bytes]);
            }
        }
        let unchanged = self.served.contains(&region);
        if !unchanged {
            self.served.push(region);
        }
        Frame {
            buf,
            w: region.w,
            h: region.h,
            source: self.source,
            fallback: self.fallback.clone(),
            unchanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: PhysRect = PhysRect { x: 100, y: 200, w: 4, h: 3 };

    /// One byte per pixel's blue channel, so a crop is readable by eye.
    struct OneOutput;

    impl RegionCapture for OneOutput {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            assert_eq!(OUT, region, "a freeze grabs the whole output");
            let mut buf = vec![0u8; (region.w * region.h * 4) as usize];
            for i in 0..(region.w * region.h) as usize {
                buf[i * 4] = i as u8;
            }
            Ok(Frame {
                buf,
                w: region.w,
                h: region.h,
                source: "fake",
                fallback: Some("rung 1 was busy".to_string()),
                unchanged: false,
            })
        }

        fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
            OUT
        }
    }

    /// Pixels short of the promised box are refused: a frozen frame is
    /// indexed for the whole hold, so a lie here would be a panic later.
    struct ShortGrab;

    impl RegionCapture for ShortGrab {
        fn grab(&mut self, region: PhysRect) -> Result<Frame> {
            Ok(Frame {
                buf: vec![0u8; 8],
                w: region.w,
                h: region.h,
                source: "fake",
                fallback: None,
                unchanged: false,
            })
        }

        fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
            OUT
        }
    }

    fn blues(frame: &Frame) -> Vec<u8> {
        frame.buf.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
    }

    fn frozen() -> FrozenFrame {
        FrozenFrame::take(&mut OneOutput, PhysPoint { x: 101, y: 201 })
            .expect("a healthy backend freezes")
    }

    #[test]
    fn a_freeze_grabs_the_output_holding_the_point() {
        let f = frozen();
        assert_eq!(OUT, f.region());
    }

    #[test]
    fn a_crop_answers_the_frozen_pixels_of_that_box() {
        let mut f = frozen();
        let got = f.crop(PhysRect { x: 101, y: 201, w: 2, h: 2 });
        assert_eq!(2, got.w);
        assert_eq!(2, got.h);
        // Rows 1 and 2 of a 4-wide frame, second column onward.
        assert_eq!(vec![5, 6, 9, 10], blues(&got));
        assert_eq!("fake", got.source);
        assert_eq!(Some("rung 1 was busy".to_string()), got.fallback);
    }

    #[test]
    fn a_box_past_the_output_edge_is_black_where_the_frame_ends() {
        let mut f = frozen();
        let got = f.crop(PhysRect { x: 103, y: 202, w: 2, h: 2 });
        // Only the frame's last pixel is real; the rest is off-output.
        assert_eq!(vec![11, 0, 0, 0], blues(&got));
    }

    #[test]
    fn a_box_wholly_off_the_output_is_all_black() {
        let mut f = frozen();
        let got = f.crop(PhysRect { x: 900, y: 900, w: 2, h: 1 });
        assert_eq!(vec![0, 0], blues(&got));
    }

    /// The pixels cannot change during a hold, so the second serve of a
    /// box is `unchanged` - which is what lets the OCR cache answer it.
    #[test]
    fn the_second_serve_of_the_same_box_is_unchanged() {
        let mut f = frozen();
        let box_ = PhysRect { x: 100, y: 200, w: 2, h: 2 };
        assert!(!f.crop(box_).unchanged, "the first serve of a box is new");
        assert!(f.crop(box_).unchanged, "the same box is the same pixels");
        assert!(
            !f.crop(PhysRect { x: 101, y: 200, w: 2, h: 2 }).unchanged,
            "a different box is a different question"
        );
    }

    #[test]
    fn a_short_press_time_grab_is_refused_rather_than_indexed() {
        let Err(e) = FrozenFrame::take(&mut ShortGrab, PhysPoint { x: 101, y: 201 }) else {
            panic!("a short buffer must not become a frozen frame");
        };
        assert!(format!("{e:#}").contains("wanted 4x3"), "{e:#}");
    }
}

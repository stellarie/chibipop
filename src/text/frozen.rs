//! This module stores the frozen frame that trigger mode reads after a key press.
//! (ARCHITECTURE.md#hover-cadence).
//!
//! Trigger mode captures the full output that contains the cursor before it creates a popup.
//! Each lookup in the key hold reads that frozen frame.
//! The hold does not capture or mask pixels.
//! The text stays readable when the popup covers it.
//! The capture guard gives Windows the same result.
//! A live Wayland grab cannot give this result.
//! (ARCHITECTURE.md#capture-and-masking).
//! A release drops the frozen frame.
//! Each press creates a new grab.
//!
//! The platform bin decides when to freeze the frame and how to handle output changes.
//! This module stores the pixels and returns requested regions.

use crate::geom::{PhysPoint, PhysRect};
use crate::text::{Frame, RegionCapture};
use anyhow::{Context, Result};

/// A `FrozenFrame` stores the pixels for one trigger hold.
pub(crate) struct FrozenFrame {
    /// This region uses physical pixels and covers every stored pixel.
    region: PhysRect,
    /// This buffer uses [`Frame`]'s top-down BGRA8 format. It contains `region.w * region.h * 4` bytes.
    buf: Vec<u8>,
    /// This source names the capture backend path.
    /// The fallback records why the code did not use the preferred path.
    /// Both values stay with the frozen frame, so later crops report the original path.
    source: &'static str,
    fallback: Option<String>,
    /// This list records each region that the frozen frame served.
    /// A repeated region has identical pixels, so the pipeline marks it `unchanged` and skips OCR.
    served: Vec<PhysRect>,
}

impl FrozenFrame {
    /// Capture the full output that contains `at`.
    ///
    /// The function pairs `begin_read` with `end_read`, as it does for a live read.
    /// A capture backend can hide its surfaces or keep a session open for this read.
    /// The full-output grab prevents a lookup outside the stored region.
    /// A trigger press occurs at human speed, so the copy cost stays limited.
    /// (ARCHITECTURE.md#hover-cadence).
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

    /// Return the box that these pixels cover.
    pub(crate) fn region(&self) -> PhysRect {
        self.region
    }

    /// Copy `region` from the frozen frame.
    ///
    /// Pixels beyond the output edge stay black. This blank margin lets edge lookups continue.
    /// Without this margin, each lookup near the edge can fail.
    /// The portal rung gives the same result outside a monitor.
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


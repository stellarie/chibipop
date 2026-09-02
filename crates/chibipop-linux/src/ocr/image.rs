//! Provide the pixel operations that the meikiocr port needs.
//!
//! [`Bgr::resize`] matches OpenCV's `INTER_LINEAR` for 8-bit images
//! *bit-for-bit*. It uses half-pixel centre mapping, fixed-point tap weights
//! at 1/2048, and horizontal-then-vertical accumulation with OpenCV's exact
//! shift schedule.
//!
//! This exact match matters. The Python harness in `tools/ocr-bench` is the
//! parity reference and uses `cv2`. The gate allows this port to differ by
//! +-3 pp from the corpus numbers. The corpus has one 16-glyph vertical slice,
//! so one changed character costs 6.25 pp. Plain float bilinear resize differs
//! from `cv2` by one least-significant bit for many pixels. That change can
//! flip a borderline glyph.

/// An 8-bit BGR image with packed pixels.
///
/// The image stores blue first because meikiocr uses this order. Its Python
/// entry point receives `cv2.imread` output and passes the channels to the
/// model in memory order. The "B" plane is channel 0 of the tensor.
pub struct Bgr {
    pub w: usize,
    pub h: usize,
    /// `w * h * 3` bytes in row-major order.
    pub px: Vec<u8>,
}

impl Bgr {
    /// Drop alpha from a capture [`Frame`](chibipop::text::Frame).
    ///
    /// `cv2.imread(..., IMREAD_COLOR)` gives the harness three channels in the
    /// same order. Alpha has no use in either path.
    pub fn from_bgra(bgra: &[u8], w: usize, h: usize) -> Self {
        let mut px = Vec::with_capacity(w * h * 3);
        let (pixels, _) = bgra.as_chunks::<4>();
        for p in pixels.iter().take(w * h) {
            px.extend_from_slice(&p[..3]);
        }
        Bgr { w, h, px }
    }

    /// Return `image[y0:y1, x0:x1]`, with the bounds clamped to the image.
    ///
    /// Return an empty image for a degenerate box. The caller then skips the
    /// crop. Numpy returns a zero-size array for the same input.
    pub fn crop(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Bgr {
        let x1 = x1.min(self.w);
        let y1 = y1.min(self.h);
        let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        let mut px = Vec::with_capacity(w * h * 3);
        for y in y0..y0 + h {
            let row = (y * self.w + x0) * 3;
            px.extend_from_slice(&self.px[row..row + w * 3]);
        }
        Bgr { w, h, px }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Resize an image with `cv2.resize(img, (dw, dh), interpolation=cv2.INTER_LINEAR)`.
    pub fn resize(&self, dw: usize, dh: usize) -> Bgr {
        // Compute the forward scale as OpenCV does. OpenCV takes the reciprocal of
        // the ratio that it just computed, not `src/dst`. The two values differ in
        // the last bit for most sizes, and that bit affects the tap weights.
        let scale_x = 1.0 / (dw as f64 / self.w as f64);
        let scale_y = 1.0 / (dh as f64 / self.h as f64);

        // Precompute horizontal taps for each output column, as `cv::resize` does.
        let mut xmap = Vec::with_capacity(dw);
        for dx in 0..dw {
            let (mut sx, mut fx) = source_of(dx, scale_x);
            if sx < 0 {
                sx = 0;
                fx = 0.0;
            }
            if sx >= self.w as i32 - 1 {
                sx = self.w as i32 - 1;
                fx = 0.0;
            }
            let sx = sx.max(0) as usize;
            // The far tap has zero weight after the clamp. Fold it onto the near
            // column to avoid a read outside the row.
            let (a0, a1) = taps(fx);
            xmap.push((sx * 3, (sx + 1).min(self.w - 1) * 3, a0, a1));
        }

        let mut out = vec![0u8; dw * dh * 3];
        // Keep two horizontally resized rows in the cache, keyed by source row.
        // `cv::resize` keeps the same two rows and reuses a row that remains needed.
        // `usize::MAX` means "no row is cached yet". No real row index can reach it.
        let mut cache: [(usize, Vec<i32>); 2] = [(usize::MAX, Vec::new()), (usize::MAX, Vec::new())];
        for dy in 0..dh {
            let (sy0, fy) = source_of(dy, scale_y);
            let (b0, b1) = taps(fy);
            // Clamp rows when you fetch them, not when you set up taps. At either
            // border, both taps use the same row. Their weights still sum to 2048, so
            // the border value stays unchanged.
            let rows = [clip_row(sy0, self.h), clip_row(sy0 + 1, self.h)];

            for (slot, &sy) in rows.iter().enumerate() {
                if cache[slot].0 == sy {
                    continue;
                }
                if cache[1 - slot].0 == sy {
                    let other = cache[1 - slot].1.clone();
                    cache[slot] = (sy, other);
                    continue;
                }
                let mut buf = std::mem::take(&mut cache[slot].1);
                buf.clear();
                buf.reserve(dw * 3);
                let src = &self.px[sy * self.w * 3..(sy + 1) * self.w * 3];
                for &(near, far, a0, a1) in &xmap {
                    for c in 0..3 {
                        buf.push(src[near + c] as i32 * a0 + src[far + c] as i32 * a1);
                    }
                }
                cache[slot] = (sy, buf);
            }

            let (s0, s1) = (&cache[0].1, &cache[1].1);
            let dst = &mut out[dy * dw * 3..(dy + 1) * dw * 3];
            for (i, o) in dst.iter_mut().enumerate() {
                // Use OpenCV's uchar specialization of VResizeLinear exactly.
                *o = ((((b0 * (s0[i] >> 4)) >> 16) + ((b1 * (s1[i] >> 4)) >> 16) + 2) >> 2) as u8;
            }
        }

        Bgr { w: dw, h: dh, px: out }
    }
}

/// Return OpenCV's half-pixel source coordinate as an integer part and a
/// fraction.
///
/// Subtract in `double`, then narrow to `float` before the split. This order
/// matches `resize_` exactly.
fn source_of(d: usize, scale: f64) -> (i32, f32) {
    let f = ((d as f64 + 0.5) * scale - 0.5) as f32;
    let s = f.floor() as i32;
    (s, f - s as f32)
}

/// Return the two fixed-point interpolation weights at 1/2048.
///
/// `saturate_cast<short>` rounds half to even, and this function does the same.
/// The pair always sums to 2048. This sum keeps the shift schedule below in
/// 32-bit range.
fn taps(f: f32) -> (i32, i32) {
    const COEF_SCALE: f32 = 2048.0;
    let a0 = ((1.0 - f) * COEF_SCALE).round_ties_even() as i32;
    let a1 = (f * COEF_SCALE).round_ties_even() as i32;
    (a0, a1)
}

/// Apply OpenCV's `clip(x, 0, h)`. Values below the range become 0.
/// Values at or above the range become `h - 1`.
fn clip_row(y: i32, h: usize) -> usize {
    if y < 0 {
        0
    } else if (y as usize) < h {
        y as usize
    } else {
        h - 1
    }
}

/// Match Python's `round`: break half ties to even, then truncate to an
/// integer.
///
/// The harness sizes every recognition crop with `int(round(...))`. On a
/// 32-pixel-tall line, this difference changes one whole pixel column of
/// content.
pub fn py_round(v: f64) -> i32 {
    v.round_ties_even() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Bgr {
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                px.push((x * 7 + y * 13) as u8);
                px.push((x * 3 + y * 5) as u8);
                px.push((x * 11 + y * 2) as u8);
            }
        }
        Bgr { w, h, px }
    }

    #[test]
    fn bgra_loses_only_its_alpha() {
        let bgra = [1, 2, 3, 255, 4, 5, 6, 0];
        let bgr = Bgr::from_bgra(&bgra, 2, 1);
        assert_eq!(vec![1, 2, 3, 4, 5, 6], bgr.px);
    }

    #[test]
    fn a_crop_takes_the_named_window() {
        let img = ramp(4, 4);
        let c = img.crop(1, 2, 3, 4);
        assert_eq!((2, 2), (c.w, c.h));
        assert_eq!(&img.px[(2 * 4 + 1) * 3..(2 * 4 + 3) * 3], &c.px[..6]);
    }

    /// A degenerate box gives the caller an empty image. The caller can skip the
    /// crop without a panic.
    #[test]
    fn a_crop_past_the_edge_comes_back_empty() {
        let img = ramp(4, 4);
        assert!(img.crop(3, 3, 3, 4).is_empty());
        assert!(img.crop(0, 0, 9, 0).is_empty());
    }

    /// An identity resize must return the same pixels. Each tap must use its own
    /// pixel with weight 2048, and the shift schedule must restore the byte.
    #[test]
    fn resizing_to_the_same_size_returns_the_same_pixels() {
        let img = ramp(9, 7);
        let same = img.resize(9, 7);
        assert_eq!(img.px, same.px);
    }

    /// Compare a 4x3 gradient after an upsample and a downsample with values from
    /// `cv2.resize(..., interpolation=cv2.INTER_LINEAR)` (OpenCV 5.0.0).
    /// These bytes protect the module's contract. If they change, the gate spends
    /// its +-3 pp parity band on arithmetic instead of the port.
    #[test]
    fn resizing_matches_opencv_byte_for_byte() {
        let src = Bgr { w: 4, h: 3, px: (0..36u8).map(|v| v.wrapping_mul(7)).collect() };

        #[rustfmt::skip]
        let up_7x5: [u8; 105] = [
            0, 7, 14, 7, 14, 21, 19, 26, 33, 31, 38, 45, 43, 50, 57, 55, 62, 69, 63, 70, 77,
            34, 40, 47, 41, 48, 55, 53, 60, 67, 65, 72, 79, 77, 84, 91, 89, 96, 103, 97, 104, 110,
            84, 91, 98, 91, 98, 105, 104, 111, 118, 116, 123, 130, 127, 134, 141, 140, 147, 154,
            147, 154, 161,
            134, 141, 148, 142, 149, 156, 154, 161, 168, 166, 173, 180, 178, 185, 192, 190, 197, 204,
            197, 204, 211,
            168, 175, 182, 175, 182, 189, 187, 194, 201, 199, 206, 213, 211, 218, 225, 223, 230, 237,
            231, 238, 245,
        ];
        assert_eq!(&up_7x5[..], &src.resize(7, 5).px[..]);

        let down_2x2: [u8; 12] = [31, 38, 45, 73, 80, 87, 157, 164, 171, 199, 206, 213];
        assert_eq!(&down_2x2[..], &src.resize(2, 2).px[..]);
    }

    /// A flat image must keep one value after every resize. Both passes use
    /// weights that sum to 2048, so a constant image stays constant.
    #[test]
    fn a_flat_image_stays_flat_through_any_resize() {
        let img = Bgr { w: 13, h: 5, px: vec![200; 13 * 5 * 3] };
        for (w, h) in [(1, 1), (32, 480), (960, 32), (7, 3), (13, 5)] {
            let out = img.resize(w, h);
            assert!(out.px.iter().all(|&p| p == 200), "{w}x{h} drifted");
        }
    }

    #[test]
    fn python_round_breaks_ties_to_even() {
        assert_eq!(2, py_round(2.5));
        assert_eq!(4, py_round(3.5));
        assert_eq!(3, py_round(3.4999));
        assert_eq!(-2, py_round(-2.5));
    }
}

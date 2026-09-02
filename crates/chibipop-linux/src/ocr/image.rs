//! The pixel plumbing the meikiocr port needs, and nothing beyond it.
//!
//! [`Bgr::resize`] reproduces OpenCV's `INTER_LINEAR` for 8-bit images
//! *bit-for-bit*: the half-pixel centre mapping, fixed-point tap weights at
//! 1/2048, and the horizontal-then-vertical accumulation with OpenCV's
//! exact shift schedule. That fidelity is not fussiness. The Python
//! harness in `tools/ocr-bench` is the parity reference, it resizes with
//! `cv2`, and the gate holds this port to +-3 pp of its numbers on a corpus
//! whose vertical slice is a single 16-glyph crop - where one flipped
//! character is 6.25 pp. A plain float bilinear differs from cv2 by a
//! least-significant bit on a good fraction of pixels, which is exactly the
//! size of perturbation that flips a borderline glyph.

/// A tightly packed 8-bit BGR image.
///
/// Blue first because that is what meikiocr was trained on: its Python
/// entry point takes `cv2.imread` output and feeds the channels to the
/// model in memory order, so the "B" plane is channel 0 of the tensor.
pub struct Bgr {
    pub w: usize,
    pub h: usize,
    /// `w * h * 3`, row-major.
    pub px: Vec<u8>,
}

impl Bgr {
    /// Drops the alpha from a capture [`Frame`](chibipop::text::Frame).
    ///
    /// `cv2.imread(..., IMREAD_COLOR)` hands the harness three channels the
    /// same way; alpha is junk in both.
    pub fn from_bgra(bgra: &[u8], w: usize, h: usize) -> Self {
        let mut px = Vec::with_capacity(w * h * 3);
        let (pixels, _) = bgra.as_chunks::<4>();
        for p in pixels.iter().take(w * h) {
            px.extend_from_slice(&p[..3]);
        }
        Bgr { w, h, px }
    }

    /// `image[y0:y1, x0:x1]`, clamped to the image.
    ///
    /// Empty when the box is degenerate, which is the caller's cue to skip
    /// the crop - numpy hands back a zero-size array in the same case.
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

    /// `cv2.resize(img, (dw, dh), interpolation=cv2.INTER_LINEAR)`.
    pub fn resize(&self, dw: usize, dh: usize) -> Bgr {
        // cv::resize derives the forward scale as the reciprocal of the
        // ratio it just computed, not as src/dst. The two differ in the
        // last bit for most sizes, and that bit reaches the tap weights.
        let scale_x = 1.0 / (dw as f64 / self.w as f64);
        let scale_y = 1.0 / (dh as f64 / self.h as f64);

        // Horizontal taps, precomputed once per column as cv::resize does.
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
            // The far tap weighs zero once clamped, so folding it onto the
            // near column only avoids reading off the end of the row.
            let (a0, a1) = taps(fx);
            xmap.push((sx * 3, (sx + 1).min(self.w - 1) * 3, a0, a1));
        }

        let mut out = vec![0u8; dw * dh * 3];
        // Two horizontally resized rows in flight, keyed by source row.
        // cv::resize keeps the same two and reuses whichever carries over.
        // `usize::MAX` is the "nothing cached yet" key; no real row index
        // can reach it.
        let mut cache: [(usize, Vec<i32>); 2] = [(usize::MAX, Vec::new()), (usize::MAX, Vec::new())];
        for dy in 0..dh {
            let (sy0, fy) = source_of(dy, scale_y);
            let (b0, b1) = taps(fy);
            // Row clamping happens at fetch time, not in the tap setup:
            // at either border both taps land on the same row and the
            // weights, which still sum to 2048, cancel out.
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
                // OpenCV's uchar specialisation of VResizeLinear, verbatim.
                *o = ((((b0 * (s0[i] >> 4)) >> 16) + ((b1 * (s1[i] >> 4)) >> 16) + 2) >> 2) as u8;
            }
        }

        Bgr { w: dw, h: dh, px: out }
    }
}

/// OpenCV's half-pixel source coordinate: integer part and fraction.
///
/// The subtraction happens in `double` and the result is narrowed to
/// `float` before the split, exactly as `resize_` does it.
fn source_of(d: usize, scale: f64) -> (i32, f32) {
    let f = ((d as f64 + 0.5) * scale - 0.5) as f32;
    let s = f.floor() as i32;
    (s, f - s as f32)
}

/// The two fixed-point interpolation weights at 1/2048.
///
/// `saturate_cast<short>` rounds half to even; so does this. The pair
/// always sums to 2048, which is what lets the shift schedule below stay
/// in 32-bit range.
fn taps(f: f32) -> (i32, i32) {
    const COEF_SCALE: f32 = 2048.0;
    let a0 = ((1.0 - f) * COEF_SCALE).round_ties_even() as i32;
    let a1 = (f * COEF_SCALE).round_ties_even() as i32;
    (a0, a1)
}

/// OpenCV's `clip(x, 0, h)`: below the range snaps to 0, at or above it
/// snaps to `h - 1`.
fn clip_row(y: i32, h: usize) -> usize {
    if y < 0 {
        0
    } else if (y as usize) < h {
        y as usize
    } else {
        h - 1
    }
}

/// Python's `round`: half to even, then truncated to an integer.
///
/// The harness sizes every recognition crop with `int(round(...))`, and on
/// a 32-pixel-tall line the difference between half-up and half-even is a
/// whole pixel column of content.
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

    /// A degenerate box is the caller's cue to skip, not a panic.
    #[test]
    fn a_crop_past_the_edge_comes_back_empty() {
        let img = ramp(4, 4);
        assert!(img.crop(3, 3, 3, 4).is_empty());
        assert!(img.crop(0, 0, 9, 0).is_empty());
    }

    /// The identity resize must be exact: every tap lands on its own pixel
    /// with weight 2048, and the shift schedule has to give the byte back.
    #[test]
    fn resizing_to_the_same_size_returns_the_same_pixels() {
        let img = ramp(9, 7);
        let same = img.resize(9, 7);
        assert_eq!(img.px, same.px);
    }

    /// A 4x3 gradient, up- and downsampled, against values produced by
    /// `cv2.resize(..., interpolation=cv2.INTER_LINEAR)` (OpenCV 5.0.0).
    /// These bytes are the whole point of the module: if this drifts, the
    /// gate's +-3 pp parity band is being spent on arithmetic rather than
    /// on the port.
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

    /// A flat image survives any resize without drift: the weights sum to
    /// 2048 in both passes, so a constant must come back constant.
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

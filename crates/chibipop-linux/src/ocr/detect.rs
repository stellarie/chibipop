//! Detect text lines for the 960x544 stage of the meikiocr pipeline.
//!
//! The detector uses an RT-DETR export, not a segmentation model. It returns
//! `(labels, boxes, scores)` with the scale from `orig_target_sizes`.
//! This stage has no probability-map threshold or unclip step to reproduce.
//! `postprocess` applies the detector confidence threshold. `preprocess` applies
//! the letterbox and its implied transform.

use super::image::Bgr;

pub const INPUT_W: usize = 960;
pub const INPUT_H: usize = 544;

/// One detected line in source-image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetBox {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl DetBox {
    pub fn w(&self) -> i32 {
        self.x2 - self.x1
    }

    pub fn h(&self) -> i32 {
        self.y2 - self.y1
    }

    /// Apply meikiocr's routing rule. This function is the only place that
    /// decides verticality. It stays inside the engine. Core's `orientation_of`
    /// remains the orientation authority for the rest of chibipop
    /// (ARCHITECTURE.md#ocr-engine).
    pub fn is_vertical(&self) -> bool {
        self.h() > self.w()
    }
}

/// Letterbox an image into the fixed detector input.
///
/// Return the `1x3x544x960` tensor and the scale factor. The caller uses the
/// factor to build `orig_target_sizes`. Fit the image to the box and place it
/// at the top-left corner. Keep the unused area black.
///
/// This step scales small crops up. That scale comes from meikiocr's fixed
/// input geometry, not from the Linux adapter. The adapter does not upscale
/// crops. This difference explains why 1x measures better than 2x
/// (ARCHITECTURE.md#ocr-engine).
pub fn preprocess(img: &Bgr) -> (Vec<f32>, f64) {
    let scale = (INPUT_W as f64 / img.w as f64).min(INPUT_H as f64 / img.h as f64);
    // `int(...)` truncates, exactly as the harness sizes it.
    let rw = ((img.w as f64 * scale) as usize).clamp(1, INPUT_W);
    let rh = ((img.h as f64 * scale) as usize).clamp(1, INPUT_H);
    let resized = img.resize(rw, rh);

    let plane = INPUT_W * INPUT_H;
    let mut tensor = vec![0.0f32; 3 * plane];
    for y in 0..rh {
        for x in 0..rw {
            let src = (y * rw + x) * 3;
            for c in 0..3 {
                tensor[c * plane + y * INPUT_W + x] = f32::from(resized.px[src + c]) / 255.0;
            }
        }
    }
    (tensor, scale)
}

/// Return the source-image size that the detector uses for its normalized
/// boxes.
///
/// The letterboxed canvas covers `INPUT_W/scale` by `INPUT_H/scale` source
/// pixels. This size maps boxes directly to source coordinates, so this
/// module does not unletterbox them.
pub fn target_size(scale: f64) -> [i64; 2] {
    [(INPUT_W as f64 / scale) as i64, (INPUT_H as f64 / scale) as i64]
}

/// Keep confident boxes, clamp them to the image, and sort them from top to
/// bottom.
///
/// `boxes` has `n * 4` values in `x1,y1,x2,y2` order. `scores` has `n` values.
/// Both slices come from the first and only batch row.
pub fn postprocess(boxes: &[f32], scores: &[f32], w: usize, h: usize, threshold: f32) -> Vec<DetBox> {
    let bound = |v: f32, max: usize| -> i32 { (f64::from(v).clamp(0.0, max as f64)) as i32 };
    let mut out: Vec<DetBox> = scores
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s > threshold)
        .map(|(i, _)| {
            let b = &boxes[i * 4..i * 4 + 4];
            DetBox {
                x1: bound(b[0], w),
                y1: bound(b[1], h),
                x2: bound(b[2], w),
                y2: bound(b[3], h),
            }
        })
        .collect();
    // Keep the sort stable, so equal top edges keep the model's order.
    out.sort_by_key(|b| b.y1);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, v: u8) -> Bgr {
        Bgr { w, h, px: vec![v; w * h * 3] }
    }

    #[test]
    fn the_letterbox_fills_only_the_scaled_corner() {
        // 500x100 fits by width: 960/500 = 1.92 is less than 544/100 = 5.44.
        let (tensor, scale) = preprocess(&solid(500, 100, 255));
        assert!((scale - 1.92).abs() < 1e-12, "{scale}");
        let plane = INPUT_W * INPUT_H;
        assert_eq!(3 * plane, tensor.len());
        // 100 * 1.92 = 192 rows contain data. The other rows stay black.
        assert_eq!(1.0, tensor[191 * INPUT_W + 959]);
        assert_eq!(0.0, tensor[192 * INPUT_W]);
        assert_eq!(0.0, tensor[plane + 200 * INPUT_W]);
    }

    #[test]
    fn a_tall_crop_fits_by_height() {
        let (_, scale) = preprocess(&solid(40, 400, 128));
        assert!((scale - 544.0 / 400.0).abs() < 1e-12, "{scale}");
    }

    /// The canvas size in source pixels.
    #[test]
    fn the_target_size_describes_the_whole_canvas() {
        assert_eq!([500, 283], target_size(1.92));
    }

    #[test]
    fn only_confident_boxes_survive() {
        let boxes = [0.0, 10.0, 5.0, 20.0, 1.0, 2.0, 3.0, 4.0];
        let scores = [0.9, 0.4];
        let got = postprocess(&boxes, &scores, 100, 100, 0.5);
        assert_eq!(vec![DetBox { x1: 0, y1: 10, x2: 5, y2: 20 }], got);
    }

    #[test]
    fn boxes_are_clamped_to_the_image_and_truncated() {
        let boxes = [-3.7, 0.0, 120.9, 40.6];
        let got = postprocess(&boxes, &[1.0], 100, 30, 0.5);
        assert_eq!(vec![DetBox { x1: 0, y1: 0, x2: 100, y2: 30 }], got);
    }

    #[test]
    fn boxes_come_back_top_to_bottom() {
        let boxes = [0.0, 50.0, 10.0, 60.0, 0.0, 5.0, 10.0, 15.0];
        let got = postprocess(&boxes, &[1.0, 1.0], 100, 100, 0.5);
        assert_eq!(vec![5, 50], got.iter().map(|b| b.y1).collect::<Vec<_>>());
    }

    #[test]
    fn verticality_is_the_taller_than_wide_test() {
        assert!(DetBox { x1: 0, y1: 0, x2: 10, y2: 40 }.is_vertical());
        assert!(!DetBox { x1: 0, y1: 0, x2: 40, y2: 10 }.is_vertical());
        // A square is horizontal because the harness uses `h > w`.
        assert!(!DetBox { x1: 0, y1: 0, x2: 10, y2: 10 }.is_vertical());
    }
}

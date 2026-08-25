//! Character recognition: the two RT-DETR recognisers and the geometry
//! that puts their output back on the source image.
//!
//! Both recognisers detect *characters* inside a line crop rather than
//! emitting a sequence, so there is no CTC decode here: each model answers
//! with `(char_codes, boxes, scores)` in its own input space, and the work
//! is mapping those boxes back through the crop transform and resolving the
//! overlaps a detector inevitably produces. Horizontal lines resolve
//! overlaps along x, vertical columns along y.
//!
//! A tall column does not fit the 480-pixel vertical input, so it is cut
//! into overlapping segments and every segment's characters land in the
//! same candidate pool - which is the other reason the overlap pass exists.

use super::detect::DetBox;
use super::image::{py_round, Bgr};

pub const REC_W: usize = 960;
pub const REC_H: usize = 32;
pub const VREC_W: usize = 32;
pub const VREC_H: usize = 480;
/// Segment height once a column has to be split, in scaled pixels. Smaller
/// than the input so every segment carries padding.
const VREC_MAX_CONTENT_H: i32 = 420;
/// How much consecutive segments share, in scaled pixels.
const VREC_OVERLAP: i32 = 64;
/// Fraction of the shorter interval two characters may share before the
/// lower-confidence one is dropped.
const OVERLAP_THRESHOLD: f64 = 0.3;
/// Keeps a zero-length interval from dividing by zero.
const EPSILON: f64 = 1e-6;

/// Upstream ships these eight reversed compounds as a known model quirk and
/// corrects them by name; the harness inherits the correction, so the gate
/// numbers include it.
const SWAPPED_PAIRS: [(&str, &str); 8] = [
    ("儡傀", "傀儡"),
    ("談冗", "冗談"),
    ("汰淘", "淘汰"),
    ("沱滂", "滂沱"),
    ("攣痙", "痙攣"),
    ("酊酩", "酩酊"),
    ("麭麺", "麺麭"),
    ("哭慟", "慟哭"),
];

/// One recognised character with its box in source-image pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct CharBox {
    pub ch: char,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub conf: f32,
}

/// One line crop, ready for the recogniser.
pub struct Crop {
    /// `3 x H x W` for the relevant recogniser, normalised to 0..1.
    pub tensor: Vec<f32>,
    /// Which detected line this belongs to. Several segments of one tall
    /// column share it.
    pub line: usize,
    bbox: [i32; 4],
    eff_w: i32,
    eff_h: i32,
}

/// Crops, resized and padded into the recogniser's fixed input.
///
/// `indices` names which of `boxes` to take - the caller has already split
/// them by orientation, because the two recognisers are separate models
/// and each wants its own batch.
pub fn preprocess(img: &Bgr, boxes: &[DetBox], indices: &[usize], vertical: bool) -> Vec<Crop> {
    let mut out = Vec::with_capacity(indices.len());
    for &i in indices {
        let b = boxes[i];
        let crop = img.crop(b.x1 as usize, b.y1 as usize, b.x2 as usize, b.y2 as usize);
        if crop.is_empty() {
            continue;
        }
        if vertical {
            push_vertical(&mut out, img, b, &crop, i);
        } else {
            push_horizontal(&mut out, &crop, b, i);
        }
    }
    out
}

fn push_horizontal(out: &mut Vec<Crop>, crop: &Bgr, b: DetBox, line: usize) {
    let scale = REC_H as f64 / crop.h as f64;
    let mut new_h = REC_H as i32;
    let mut new_w = py_round(crop.w as f64 * scale);
    if new_w > REC_W as i32 {
        let scale_w = REC_W as f64 / f64::from(new_w);
        new_w = REC_W as i32;
        new_h = py_round(REC_H as f64 * scale_w);
    }
    let (new_w, new_h) = (new_w.max(1), new_h.max(1));
    let resized = crop.resize(new_w as usize, new_h as usize);
    out.push(Crop {
        tensor: pad_into(&resized, REC_W, REC_H),
        line,
        bbox: [b.x1, b.y1, b.x2, b.y2],
        eff_w: new_w,
        eff_h: new_h,
    });
}

fn push_vertical(out: &mut Vec<Crop>, img: &Bgr, b: DetBox, crop: &Bgr, line: usize) {
    let scale = VREC_W as f64 / crop.w as f64;
    let full = crop.h as f64 * scale;

    // Only split when the column genuinely overruns the input; a 478-pixel
    // column is processed whole rather than cut in two.
    let (max_content, segment_h, starts) = if full > VREC_H as f64 {
        let segment_h = f64::from(VREC_MAX_CONTENT_H) / scale;
        let stride = f64::from(VREC_MAX_CONTENT_H - VREC_OVERLAP) / scale;
        let mut starts = Vec::new();
        let mut y = f64::from(b.y1);
        while y + segment_h < f64::from(b.y2) {
            starts.push(y);
            y += stride;
        }
        // The tail segment is pinned to the bottom edge rather than left
        // hanging, unless the walk already ended there.
        let last = f64::from(b.y2) - segment_h;
        if starts.last().is_none_or(|&prev| last > prev + 1.0) {
            starts.push(last);
        }
        (VREC_MAX_CONTENT_H, segment_h, starts)
    } else {
        (VREC_H as i32, f64::from(b.h()), vec![f64::from(b.y1)])
    };

    for start in starts {
        let sy1 = py_round(start);
        let sy2 = py_round(start + segment_h).min(b.y2);
        let seg = img.crop(b.x1 as usize, sy1 as usize, b.x2 as usize, sy2 as usize);
        if seg.h == 0 {
            continue;
        }
        let seg_h = py_round(seg.h as f64 * scale).min(max_content).max(1);
        let resized = seg.resize(VREC_W, seg_h as usize);
        out.push(Crop {
            tensor: pad_into(&resized, VREC_W, VREC_H),
            line,
            bbox: [b.x1, sy1, b.x2, sy2],
            eff_w: VREC_W as i32,
            eff_h: seg_h,
        });
    }
}

/// Pins the image to the top-left of a `3 x h x w` tensor; the rest is
/// black, which is what the recognisers were trained to ignore.
fn pad_into(img: &Bgr, w: usize, h: usize) -> Vec<f32> {
    let plane = w * h;
    let mut tensor = vec![0.0f32; 3 * plane];
    for y in 0..img.h.min(h) {
        for x in 0..img.w.min(w) {
            let src = (y * img.w + x) * 3;
            for c in 0..3 {
                tensor[c * plane + y * w + x] = f32::from(img.px[src + c]) / 255.0;
            }
        }
    }
    tensor
}

/// A character the recogniser proposed, before overlaps are resolved.
struct Candidate {
    ch: char,
    bbox: [i32; 4],
    conf: f32,
    /// Extent along the reading axis: x for a line, y for a column.
    i1: i32,
    i2: i32,
}

/// One recogniser batch's raw output, as the model hands it over.
///
/// `queries` is the model's fixed query count, so row `r` of the batch owns
/// `codes[r*queries..]`, `boxes[r*queries*4..]` and `scores[r*queries..]`.
pub struct Batch<'a> {
    pub codes: &'a [i32],
    pub boxes: &'a [f32],
    pub scores: &'a [f32],
    pub queries: usize,
}

/// Collects a recogniser's characters per detected line.
///
/// Several batches feed one collector: a tall column arrives as separate
/// segments, and the two recognisers run as two passes over the same set
/// of lines.
pub struct Collector {
    per_line: Vec<Vec<Candidate>>,
}

impl Collector {
    pub fn new(lines: usize) -> Self {
        Collector { per_line: (0..lines).map(|_| Vec::new()).collect() }
    }

    /// Maps one batch back onto the source image.
    pub fn add(&mut self, crops: &[Crop], batch: Batch<'_>, vertical: bool, threshold: f32) {
        let Batch { codes, boxes, scores, queries: k } = batch;
        for (r, crop) in crops.iter().enumerate() {
            let [gx1, gy1, gx2, gy2] = crop.bbox;
            let (crop_w, crop_h) = ((gx2 - gx1) as f32, (gy2 - gy1) as f32);
            for q in 0..k {
                let conf = scores[r * k + q];
                if conf < threshold {
                    continue;
                }
                let Some(ch) = char::from_u32(codes[r * k + q] as u32) else {
                    continue;
                };
                let b = &boxes[(r * k + q) * 4..(r * k + q) * 4 + 4];
                let (rx1, ry1, rx2, ry2) = (b[0], b[1], b[2], b[3]);

                // Everything past the padding boundary is the model
                // reading black, so it is dropped rather than clamped in.
                let (cx1, cy1, cx2, cy2) = if vertical {
                    let eff_h = crop.eff_h as f32;
                    if ry1 >= eff_h {
                        continue;
                    }
                    (
                        (rx1 / VREC_W as f32) * crop_w,
                        (ry1 / eff_h) * crop_h,
                        (rx2 / VREC_W as f32) * crop_w,
                        (ry2.min(eff_h) / eff_h) * crop_h,
                    )
                } else {
                    let eff_w = crop.eff_w as f32;
                    if rx1 >= eff_w {
                        continue;
                    }
                    (
                        (rx1 / eff_w) * crop_w,
                        (ry1 / REC_H as f32) * crop_h,
                        (rx2.min(eff_w) / eff_w) * crop_w,
                        (ry2 / REC_H as f32) * crop_h,
                    )
                };

                let bbox = [gx1 + cx1 as i32, gy1 + cy1 as i32, gx1 + cx2 as i32, gy1 + cy2 as i32];
                if vertical && bbox[3] <= bbox[1] {
                    continue;
                }
                let (i1, i2) = if vertical { (bbox[1], bbox[3]) } else { (bbox[0], bbox[2]) };
                self.per_line[crop.line].push(Candidate { ch, bbox, conf, i1, i2 });
            }
        }
    }

    /// Resolves overlaps and hands back each line's characters in reading
    /// order. Lines nobody proposed a character for come back empty.
    pub fn finish(self) -> Vec<Vec<CharBox>> {
        self.per_line.into_iter().map(resolve_line).collect()
    }
}

fn resolve_line(mut candidates: Vec<Candidate>) -> Vec<CharBox> {
    // Confidence first, and stably: equal scores keep the model's order.
    candidates.sort_by(|a, b| b.conf.total_cmp(&a.conf));

    let mut accepted: Vec<Candidate> = Vec::new();
    for c in candidates {
        let len_c = f64::from(c.i2 - c.i1) + EPSILON;
        let clashes = accepted.iter().any(|a| {
            if c.i1 >= a.i2 || a.i1 >= c.i2 {
                return false;
            }
            let inter = f64::from((c.i2.min(a.i2) - c.i1.max(a.i1)).max(0));
            let len_a = f64::from(a.i2 - a.i1) + EPSILON;
            inter / len_c.min(len_a) > OVERLAP_THRESHOLD
        });
        if !clashes {
            accepted.push(c);
        }
    }

    accepted.sort_by_key(|c| c.i1);
    let mut chars: Vec<CharBox> = accepted
        .into_iter()
        .map(|c| CharBox {
            ch: c.ch,
            x1: c.bbox[0],
            y1: c.bbox[1],
            x2: c.bbox[2],
            y2: c.bbox[3],
            conf: c.conf,
        })
        .collect();
    fix_swapped_pairs(&mut chars);
    chars
}

/// Un-reverses the known compounds, in place and boxes untouched: the
/// characters were read in the right places, only their identities were
/// exchanged.
fn fix_swapped_pairs(chars: &mut [CharBox]) {
    for (wrong, _) in SWAPPED_PAIRS {
        let mut w = wrong.chars();
        let (a, b) = (w.next().expect("pair"), w.next().expect("pair"));
        if let Some(i) = chars.windows(2).position(|p| p[0].ch == a && p[1].ch == b) {
            let (first, second) = chars.split_at_mut(i + 1);
            std::mem::swap(&mut first[i].ch, &mut second[0].ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize) -> Bgr {
        Bgr { w, h, px: vec![128; w * h * 3] }
    }

    fn cb(ch: char, x1: i32, x2: i32, conf: f32) -> Candidate {
        Candidate { ch, bbox: [x1, 0, x2, 10], conf, i1: x1, i2: x2 }
    }

    #[test]
    fn a_horizontal_crop_is_scaled_to_the_line_height_and_padded() {
        let img = solid(200, 50);
        let b = DetBox { x1: 0, y1: 0, x2: 200, y2: 50 };
        let crops = preprocess(&img, &[b], &[0], false);
        assert_eq!(1, crops.len());
        assert_eq!(3 * REC_W * REC_H, crops[0].tensor.len());
        // 200 * (32/50) = 128.
        assert_eq!(128, crops[0].eff_w);
        assert_eq!(32, crops[0].eff_h);
        // Past the content width the tensor is black.
        assert_eq!(0.0, crops[0].tensor[130]);
    }

    /// An extremely wide line hits the 960 ceiling and loses height for it.
    #[test]
    fn an_overlong_line_is_capped_at_the_input_width() {
        let img = solid(4000, 32);
        let b = DetBox { x1: 0, y1: 0, x2: 4000, y2: 32 };
        let crops = preprocess(&img, &[b], &[0], false);
        assert_eq!(960, crops[0].eff_w);
        // 32 * (960/4000) = 7.68 -> 8.
        assert_eq!(8, crops[0].eff_h);
    }

    #[test]
    fn a_short_column_is_not_split() {
        let img = solid(30, 400);
        let b = DetBox { x1: 0, y1: 0, x2: 30, y2: 400 };
        let crops = preprocess(&img, &[b], &[0], true);
        assert_eq!(1, crops.len());
        assert_eq!(3 * VREC_W * VREC_H, crops[0].tensor.len());
        // 400 * (32/30) = 426.67 -> 427, under the 480 input.
        assert_eq!(427, crops[0].eff_h);
    }

    /// The boundary case the harness calls out: 478 scaled pixels is
    /// processed whole, not cut into two padded halves.
    #[test]
    fn a_column_just_under_the_input_is_processed_whole() {
        let img = solid(32, 478);
        let b = DetBox { x1: 0, y1: 0, x2: 32, y2: 478 };
        let crops = preprocess(&img, &[b], &[0], true);
        assert_eq!(1, crops.len());
        assert_eq!(478, crops[0].eff_h);
    }

    #[test]
    fn a_tall_column_is_split_into_overlapping_segments() {
        let img = solid(32, 1200);
        let b = DetBox { x1: 0, y1: 0, x2: 32, y2: 1200 };
        let crops = preprocess(&img, &[b], &[0], true);
        assert!(crops.len() > 1, "1200px at 1x scale must split");
        assert!(crops.iter().all(|c| c.line == 0), "every segment stays on its line");
        assert!(crops.iter().all(|c| c.eff_h <= VREC_MAX_CONTENT_H));
        // Consecutive segments share ground.
        assert!(crops[1].bbox[1] < crops[0].bbox[3]);
        // The last one is pinned to the bottom edge.
        assert_eq!(1200, crops.last().unwrap().bbox[3]);
    }

    #[test]
    fn a_degenerate_box_is_skipped_rather_than_panicking() {
        let img = solid(50, 50);
        let b = DetBox { x1: 10, y1: 10, x2: 10, y2: 20 };
        assert!(preprocess(&img, &[b], &[0], false).is_empty());
        assert!(preprocess(&img, &[b], &[0], true).is_empty());
    }

    #[test]
    fn the_higher_confidence_character_wins_an_overlap() {
        let got = resolve_line(vec![cb('X', 0, 20, 0.4), cb('O', 2, 22, 0.9)]);
        assert_eq!(vec!['O'], got.iter().map(|c| c.ch).collect::<Vec<_>>());
    }

    #[test]
    fn characters_that_merely_touch_both_survive() {
        let got = resolve_line(vec![cb('A', 0, 20, 0.9), cb('B', 20, 40, 0.8)]);
        assert_eq!(vec!['A', 'B'], got.iter().map(|c| c.ch).collect::<Vec<_>>());
    }

    /// Under the threshold the pair is a genuine neighbour pair, not a
    /// duplicate: 30 % of the shorter interval is the line.
    #[test]
    fn a_small_overlap_is_tolerated() {
        let got = resolve_line(vec![cb('A', 0, 20, 0.9), cb('B', 15, 40, 0.8)]);
        assert_eq!(2, got.len(), "5/20 = 25 % is under the 30 % threshold");
    }

    #[test]
    fn survivors_come_back_in_reading_order_not_confidence_order() {
        let got = resolve_line(vec![cb('C', 40, 60, 0.99), cb('A', 0, 20, 0.5), cb('B', 20, 40, 0.7)]);
        assert_eq!(vec!['A', 'B', 'C'], got.iter().map(|c| c.ch).collect::<Vec<_>>());
    }

    #[test]
    fn a_known_reversed_compound_is_put_back_in_order() {
        let got = resolve_line(vec![cb('談', 0, 20, 0.9), cb('冗', 20, 40, 0.9)]);
        assert_eq!("冗談", got.iter().map(|c| c.ch).collect::<String>());
        // Boxes stay where they were read.
        assert_eq!(0, got[0].x1);
    }

    #[test]
    fn a_compound_that_is_already_right_is_left_alone() {
        let got = resolve_line(vec![cb('冗', 0, 20, 0.9), cb('談', 20, 40, 0.9)]);
        assert_eq!("冗談", got.iter().map(|c| c.ch).collect::<String>());
    }

    #[test]
    fn a_line_nobody_proposed_anything_for_is_empty() {
        assert!(resolve_line(Vec::new()).is_empty());
    }
}

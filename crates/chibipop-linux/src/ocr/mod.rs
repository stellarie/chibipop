//! The Linux OCR engine: meikiocr's pipeline, ported to Rust over `ort`.
//!
//! ADR-0009 picked meikiocr because it is the only benchmarked candidate
//! that clears every hard requirement at once - per-character geometry,
//! ~22 ms warm, and it still reads the sparse three-glyph crop. This module
//! is that pipeline with the Python removed: three ONNX sessions and the
//! pre/post-processing around them, held to the harness in
//! `tools/ocr-bench` by the fixture gate in `tests/ocr_gate.rs`.
//!
//! Two contracts shape the port:
//!
//! - **No upscaling.** The adapter feeds native-resolution pixels in every
//!   orientation (ADR-0009's 2026-08-24 amendment: meiki measured 1x better
//!   than 2x on every slice, because its fixed 960x544 detector letterbox
//!   undoes an upscale). The letterbox itself may scale a small crop up -
//!   that is the model's own input geometry, not a capture-side decision.
//! - **Verticality stays in here.** meiki routes each line to a horizontal
//!   or a vertical recogniser by its aspect ratio, and that decision never
//!   leaves the module: core's `orientation_of` remains the sole
//!   orientation authority for everything downstream.
//!
//! The engine emits one [`OcrWord`] per *character*, which is finer than
//! Windows' word rects and is exactly what `hit_scan` resolves against.

pub mod detect;
pub mod image;
pub mod models;
pub mod recognise;

use anyhow::{anyhow, Context, Result};
use chibipop::geom::PhysRect;
use chibipop::text::layout::{OcrLine, OcrWord};
use image::Bgr;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use recognise::{CharBox, Collector};
use std::cell::RefCell;
use std::path::Path;

/// Below this a detected line is not a line. Upstream's default.
const DET_THRESHOLD: f32 = 0.5;
/// Below this a proposed character is noise. Upstream's default.
const REC_THRESHOLD: f32 = 0.1;
/// Batch ceiling for the recogniser models, upstream's default: it bounds
/// peak memory on a page-sized capture without costing throughput.
const MAX_BATCH: usize = 8;

/// One detected line and the characters read out of it.
pub struct Line {
    pub chars: Vec<CharBox>,
    /// Which recogniser read it. Engine-internal by ADR-0009; it is not
    /// part of anything this module hands out.
    vertical: bool,
}

/// meikiocr over `ort`.
///
/// The sessions live behind [`RefCell`] because [`chibipop::text::OcrEngine`]
/// recognises through `&self` while `ort` runs through `&mut Session`. That
/// is sound rather than a workaround: the worker builds its backends inside
/// its own thread and never shares them (`src/worker.rs`), so there is no
/// second borrower to race.
pub struct MeikiOcr {
    detector: RefCell<Session>,
    recogniser: RefCell<Session>,
    vertical_recogniser: RefCell<Session>,
}

impl MeikiOcr {
    /// Opens the bundled models, wherever this install keeps them.
    pub fn new() -> Result<Self> {
        let dir = models::locate()?;
        Self::open(&dir)
    }

    /// Opens a named model directory, digests checked first.
    ///
    /// The check is what makes the gate's numbers mean anything: they were
    /// measured against these exact bytes, so a different file is refused
    /// rather than silently recognised with.
    pub fn open(dir: &Path) -> Result<Self> {
        models::verify(dir).context("verifying the bundled OCR models")?;
        Ok(MeikiOcr {
            detector: RefCell::new(session(&dir.join(models::DETECT.0))?),
            recogniser: RefCell::new(session(&dir.join(models::RECOGNISE.0))?),
            vertical_recogniser: RefCell::new(session(&dir.join(models::RECOGNISE_VERTICAL.0))?),
        })
    }

    /// Detect, then recognise: the whole pipeline over one image.
    pub fn read(&self, img: &Bgr) -> Result<Vec<Line>> {
        if img.is_empty() {
            return Ok(Vec::new());
        }
        let boxes = self.detect(img)?;
        if boxes.is_empty() {
            return Ok(Vec::new());
        }

        // Degenerate boxes have no orientation and nothing to read.
        let live = |vertical: bool| -> Vec<usize> {
            boxes
                .iter()
                .enumerate()
                .filter(|(_, b)| b.w() > 0 && b.h() > 0 && b.is_vertical() == vertical)
                .map(|(i, _)| i)
                .collect()
        };

        let mut collector = Collector::new(boxes.len());
        self.recognise_group(&mut collector, img, &boxes, &live(false), false)?;
        self.recognise_group(&mut collector, img, &boxes, &live(true), true)?;

        Ok(collector
            .finish()
            .into_iter()
            .zip(&boxes)
            .map(|(chars, b)| Line { chars, vertical: b.is_vertical() })
            .collect())
    }

    fn detect(&self, img: &Bgr) -> Result<Vec<detect::DetBox>> {
        let (tensor, scale) = detect::preprocess(img);
        let shape = [1i64, 3, detect::INPUT_H as i64, detect::INPUT_W as i64];
        let mut detector = self.detector.borrow_mut();
        let outputs = detector
            .run(ort::inputs![
                "images" => Tensor::from_array((shape, tensor))?,
                "orig_target_sizes" => Tensor::from_array(([1i64, 2], detect::target_size(scale).to_vec()))?,
            ])
            .context("running text detection")?;
        let (_, boxes) = outputs["boxes"].try_extract_tensor::<f32>()?;
        let (_, scores) = outputs["scores"].try_extract_tensor::<f32>()?;
        Ok(detect::postprocess(boxes, scores, img.w, img.h, DET_THRESHOLD))
    }

    fn recognise_group(
        &self,
        collector: &mut Collector,
        img: &Bgr,
        boxes: &[detect::DetBox],
        indices: &[usize],
        vertical: bool,
    ) -> Result<()> {
        if indices.is_empty() {
            return Ok(());
        }
        let crops = recognise::preprocess(img, boxes, indices, vertical);
        if crops.is_empty() {
            return Ok(());
        }

        let (w, h) = if vertical {
            (recognise::VREC_W, recognise::VREC_H)
        } else {
            (recognise::REC_W, recognise::REC_H)
        };
        let cell = if vertical { &self.vertical_recogniser } else { &self.recogniser };
        let mut session = cell.borrow_mut();

        for chunk in crops.chunks(MAX_BATCH) {
            let mut pixels: Vec<f32> = Vec::with_capacity(chunk.len() * 3 * w * h);
            for crop in chunk {
                pixels.extend_from_slice(&crop.tensor);
            }
            let shape = [chunk.len() as i64, 3, h as i64, w as i64];
            let outputs = session
                .run(ort::inputs![
                    "images" => Tensor::from_array((shape, pixels))?,
                    "orig_target_sizes" => Tensor::from_array(([1i64, 2], vec![w as i64, h as i64]))?,
                ])
                .context("running character recognition")?;
            let (codes_shape, codes) = outputs["char_codes"].try_extract_tensor::<i32>()?;
            let (_, char_boxes) = outputs["boxes"].try_extract_tensor::<f32>()?;
            let (_, scores) = outputs["scores"].try_extract_tensor::<f32>()?;
            let batch = recognise::Batch { codes, boxes: char_boxes, scores, queries: codes_shape[1] as usize };
            collector.add(chunk, batch, vertical, REC_THRESHOLD);
        }
        Ok(())
    }
}

/// One session, configured the way the benchmark configured its Python
/// equivalent - full graph optimisation, and no spin-waiting, which is
/// wasted heat on a machine that is idle between hovers.
fn session(path: &Path) -> Result<Session> {
    let build = || -> std::result::Result<Session, String> {
        Session::builder()
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_config_entry("session.intra_op.allow_spinning", "0")
            .map_err(|e| e.to_string())?
            .with_config_entry("session.inter_op.allow_spinning", "0")
            .map_err(|e| e.to_string())?
            .commit_from_file(path)
            .map_err(|e| e.to_string())
    };
    build().map_err(|e| anyhow!("loading {}: {e}", path.display()))
}

/// Reading order across lines: horizontal lines top to bottom (the
/// detector already sorted them), then vertical columns right to left.
/// Empty lines are dropped, so an empty result means nothing was read.
fn to_ocr_lines(lines: Vec<Line>) -> Vec<OcrLine> {
    let (vertical, horizontal): (Vec<Line>, Vec<Line>) =
        lines.into_iter().filter(|l| !l.chars.is_empty()).partition(|l| l.vertical);
    let mut vertical = vertical;
    vertical.sort_by_key(|l| -l.chars[0].x1);

    horizontal
        .into_iter()
        .chain(vertical)
        .map(|l| OcrLine {
            words: l
                .chars
                .into_iter()
                .map(|c| OcrWord {
                    text: c.ch.to_string(),
                    rect: PhysRect { x: c.x1, y: c.y1, w: c.x2 - c.x1, h: c.y2 - c.y1 },
                })
                .collect(),
        })
        .collect()
}

/// meikiocr reads one script pair - Japanese, with the Latin and digits
/// that sit inside Japanese text - and has no second charset to swap to.
///
/// The one caller left is [`chibipop::text::OcrEngine::set_language`]: a
/// config carrying some other `ocr.language` (ADR-0012 hides the field on
/// Linux, it does not clear it) gets read by meikiocr regardless, and the
/// user is told so rather than silently handed nothing.
pub fn serves_language(tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    tag == "ja" || tag.starts_with("ja-")
}

impl chibipop::text::OcrEngine for MeikiOcr {
    fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        if w <= 0 || h <= 0 {
            return Ok(Vec::new());
        }
        let (w, h) = (w as usize, h as usize);
        anyhow::ensure!(
            bgra.len() >= w * h * 4,
            "capture is {} bytes, short of the {} a {w}x{h} BGRA frame needs",
            bgra.len(),
            w * h * 4
        );
        let img = Bgr::from_bgra(bgra, w, h);
        Ok(to_ocr_lines(self.read(&img)?))
    }

    fn set_language(&mut self, tag: &str) {
        if !serves_language(tag) {
            eprintln!("chibipop: the Linux OCR engine reads ja only; keeping it rather than {tag}");
        }
    }

    fn name(&self) -> &str {
        "meiki-ocr"
    }

    /// Per-character boxes, finer than a word rect (ADR-0009).
    fn provides_geometry(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(vertical: bool, chars: &[(char, i32, i32)]) -> Line {
        Line {
            vertical,
            chars: chars
                .iter()
                .map(|&(ch, x, y)| CharBox { ch, x1: x, y1: y, x2: x + 20, y2: y + 20, conf: 0.9 })
                .collect(),
        }
    }

    #[test]
    fn every_character_becomes_its_own_word_box() {
        let got = to_ocr_lines(vec![line(false, &[('昨', 10, 5), ('日', 30, 5)])]);
        assert_eq!(1, got.len());
        assert_eq!(
            vec![
                OcrWord { text: "昨".into(), rect: PhysRect { x: 10, y: 5, w: 20, h: 20 } },
                OcrWord { text: "日".into(), rect: PhysRect { x: 30, y: 5, w: 20, h: 20 } },
            ],
            got[0].words
        );
    }

    #[test]
    fn empty_lines_are_dropped_rather_than_returned_wordless() {
        let got = to_ocr_lines(vec![line(false, &[]), line(false, &[('あ', 0, 0)])]);
        assert_eq!(1, got.len());
    }

    #[test]
    fn horizontal_lines_come_before_vertical_columns() {
        let got = to_ocr_lines(vec![line(true, &[('縦', 0, 0)]), line(false, &[('横', 0, 0)])]);
        assert_eq!("横", got[0].words[0].text);
        assert_eq!("縦", got[1].words[0].text);
    }

    #[test]
    fn vertical_columns_are_ordered_right_to_left() {
        let got = to_ocr_lines(vec![line(true, &[('左', 10, 0)]), line(true, &[('右', 90, 0)])]);
        assert_eq!("右", got[0].words[0].text);
        assert_eq!("左", got[1].words[0].text);
    }

    /// The per-character boxes core's `orientation_of` sees must still read
    /// as a column, or the whole point of routing verticals is lost.
    #[test]
    fn a_vertical_columns_geometry_reads_as_vertical_to_core() {
        use chibipop::text::layout::{orientation_of, Orientation};
        let got = to_ocr_lines(vec![line(true, &[('上', 100, 10), ('下', 100, 40)])]);
        assert_eq!(Orientation::Vertical, orientation_of(&got[0]));
    }

    #[test]
    fn japanese_tags_are_served_and_others_are_not() {
        assert!(serves_language("ja"));
        assert!(serves_language("JA"));
        assert!(serves_language("ja-JP"));
        assert!(!serves_language("ko"));
        assert!(!serves_language("java"));
    }
}

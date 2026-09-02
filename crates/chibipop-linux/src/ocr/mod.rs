//! The Linux OCR engine runs meikiocr through `ort`.
//!
//! meikiocr is the only tested candidate that meets all hard requirements:
//! per-character geometry, about 22 ms warm latency, and recognition of the
//! sparse three-glyph crop. This module ports that pipeline to Rust without
//! Python. It uses three ONNX sessions and the steps before and after each
//! model call. The fixture gate in `tests/ocr_gate.rs` checks it against the
//! `tools/ocr-bench` harness.
//!
//! Two contracts define this port:
//!
//! - **No upscale.** The adapter sends native-resolution pixels for every
//!   orientation (ARCHITECTURE.md#ocr-engine). meiki measured better at 1x
//!   than at 2x on every slice because its fixed 960x544 detector letterbox
//!   reverses an upscale. The letterbox can enlarge a small crop. That scale
//!   belongs to the model input, not to the capture decision.
//! - **Keep verticality here.** meiki sends each line to a horizontal or
//!   vertical recogniser from its aspect ratio. This module keeps that choice.
//!   Core's `orientation_of` remains the only orientation authority for later
//!   stages.
//!
//! The engine emits one [`OcrWord`] for each *character*. This unit is finer
//! than Windows' word rects. `hit_scan` resolves against these boxes.

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

/// A detected line below this score is not a line. This is the upstream default.
const DET_THRESHOLD: f32 = 0.5;
/// A proposed character below this score is noise. This is the upstream default.
const REC_THRESHOLD: f32 = 0.1;
/// The recogniser uses at most this many inputs in one batch. This upstream
/// default limits peak memory for a page-sized capture without a throughput loss.
const MAX_BATCH: usize = 8;

/// A detected line and the characters that the engine reads from it.
pub struct Line {
    pub chars: Vec<CharBox>,
    /// The recogniser that read the line. This field stays inside the engine.
    /// The module does not return this field.
    vertical: bool,
}

/// The meikiocr engine uses `ort`.
///
/// The sessions use [`RefCell`] because
/// [`chibipop::text::OcrEngine`] calls recognition through `&self`, while
/// `ort` calls `Session` methods through `&mut Session`. This is safe because
/// the worker creates each backend inside its own thread and never shares it
/// (`src/worker.rs`). No second borrower can race.
pub struct MeikiOcr {
    detector: RefCell<Session>,
    recogniser: RefCell<Session>,
    vertical_recogniser: RefCell<Session>,
}

impl MeikiOcr {
    /// Open the bundled models from the install directory.
    pub fn new() -> Result<Self> {
        let dir = models::locate()?;
        Self::open(&dir)
    }

    /// Open a named model directory after the digest check.
    ///
    /// The check ties the gate values to these exact bytes. The method rejects
    /// other bytes. It does not silently use another model.
    pub fn open(dir: &Path) -> Result<Self> {
        models::verify(dir).context("verifying the bundled OCR models")?;
        Ok(MeikiOcr {
            detector: RefCell::new(session(&dir.join(models::DETECT.0))?),
            recogniser: RefCell::new(session(&dir.join(models::RECOGNISE.0))?),
            vertical_recogniser: RefCell::new(session(&dir.join(models::RECOGNISE_VERTICAL.0))?),
        })
    }

    /// Run the complete detection and recognition pipeline for one image.
    pub fn read(&self, img: &Bgr) -> Result<Vec<Line>> {
        if img.is_empty() {
            return Ok(Vec::new());
        }
        let boxes = self.detect(img)?;
        if boxes.is_empty() {
            return Ok(Vec::new());
        }

        // A box with no area has no orientation, so the engine skips it.
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

/// Create one session with the benchmark settings.
///
/// Use full graph optimization and disable spin waits. Spin waits waste heat
/// because the machine stays idle between hovers.
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

/// Set line order across lines. The detector already sorts horizontal lines
/// from top to bottom. Sort vertical columns from right to left. Drop empty
/// lines. An empty result means that the engine read nothing.
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

/// meikiocr reads one script pair: Japanese plus the Latin letters and digits
/// that occur in Japanese text. It has no second character set.
///
/// [`chibipop::text::OcrEngine::set_language`] is the only caller. A
/// configuration with another `ocr.language` can reach this method because the
/// Linux settings UI hides the field but does not clear it. meikiocr still reads
/// that language. The caller tells the user. It does not hide the lack of text.
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

    /// Return one box for each character. This is finer than a word rect
    /// (ARCHITECTURE.md#ocr-engine).
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

    /// Keep the per-character boxes that core's `orientation_of` reads as a
    /// column. Otherwise, vertical selection loses its purpose.
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

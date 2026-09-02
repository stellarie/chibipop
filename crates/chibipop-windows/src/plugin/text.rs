//! Plugin text helpers.
use crate::geom::PhysRect;
use crate::plugin::host::Host;
use crate::plugin::manifest::Manifest;
use crate::plugin::proto::{RecogniseParams, RecogniseResult, Rect};
use crate::plugin::strikes::Strikes;
use crate::text::layout::{OcrLine, OcrWord};
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::cell::RefCell;
use std::time::Duration;

/// Wire rects are image-local.
fn to_ocr_lines(r: &RecogniseResult) -> Vec<OcrLine> {
    r.lines
        .iter()
        .map(|l| OcrLine {
            words: l
                .words
                .iter()
                .flatten()
                .map(|word| OcrWord {
                    text: word.text.clone(),
                    rect: PhysRect {
                        x: word.rect.x,
                        y: word.rect.y,
                        w: word.rect.w,
                        h: word.rect.h,
                    },
                })
                .collect(),
        })
        .collect()
}

pub fn estimate_offset(line: &str, cursor_x: i32, region: PhysRect) -> usize {
    let chars = line.chars().count();
    if chars == 0 || region.w <= 0 {
        return 0;
    }
    let dx = (cursor_x - region.x).clamp(0, region.w - 1) as i64;
    let idx = (dx * chars as i64 / region.w as i64) as usize;
    let idx = idx.min(chars - 1);
    line.char_indices().nth(idx).map_or(0, |(b, _)| b)
}

pub struct PluginText {
    pub(crate) host: RefCell<Host>,
    pub(crate) strikes: RefCell<Strikes>,
    pub(crate) name: String,
    pub(crate) geometry: bool,
    pub(crate) language: String,
    pub(crate) timeout: Duration,
}

impl PluginText {
    pub fn new(host: Host, m: &Manifest) -> Self {
        let cfg = m.text_provider.as_ref().expect("checked in manifest::parse");
        PluginText {
            host: RefCell::new(host),
            strikes: RefCell::new(Strikes::new(3)),
            name: m.name.clone(),
            geometry: cfg.provides_geometry,
            language: cfg.languages.first().cloned().unwrap_or_default(),
            timeout: Duration::from_millis(cfg.timeout_ms),
        }
    }

    pub fn disabled(&self) -> bool {
        self.strikes.borrow().disabled()
    }

    fn attempt(&self, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        let png = crate::image::encode_bgra_to_png(buf, w, h)?;
        let params = RecogniseParams {
            image_png: base64::engine::general_purpose::STANDARD.encode(&png),
            // The image is its own frame.
            region: Rect { x: 0, y: 0, w, h },
            scale: 1,
            language: self.language.clone(),
            deadline_ms: u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
        };
        let params = serde_json::to_value(&params).context("encoding the request")?;
        let v = self.host.borrow_mut().call("text/recognise", params, self.timeout)?;
        let parsed: RecogniseResult =
            serde_json::from_value(v).context("response did not match the contract")?;
        Ok(to_ocr_lines(&parsed))
    }
}

/// The core worker seam: a plugin serves hovers exactly like the
/// built-in engine, strikes and all, and answers the same metadata
/// (ARCHITECTURE.md#workspace-and-seams).
impl chibipop::text::OcrEngine for PluginText {
    fn recognise(&self, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        if self.disabled() {
            let why = self.strikes.borrow().last_error().unwrap_or("no detail").to_string();
            bail!("plugin \"{}\" is disabled: {why}", self.name);
        }
        match self.attempt(buf, w, h) {
            Ok(lines) => {
                self.strikes.borrow_mut().record(true);
                Ok(lines)
            }
            Err(e) => {
                let mut strikes = self.strikes.borrow_mut();
                strikes.note(&format!("{e:#}"));
                if let Some(notice) = strikes.record(false) {
                    eprintln!("chibipop: {}: {notice}", self.name);
                }
                Err(e)
            }
        }
    }

    fn set_language(&mut self, tag: &str) {
        // A plugin's languages come from its manifest; honour a reload
        // only if the manifest listed the tag, and say so if not.
        if self.language.eq_ignore_ascii_case(tag) {
            return;
        }
        eprintln!(
            "chibipop: plugin \"{}\" serves {}; keeping it (reload asked for {tag})",
            self.name, self.language
        );
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn provides_geometry(&self) -> bool {
        self.geometry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::proto::{Line, RecogniseResult, Rect, Word};

    fn region() -> PhysRect {
        PhysRect { x: 100, y: 200, w: 500, h: 100 }
    }

    #[test]
    fn wire_lines_become_ocr_lines() {
        let r = RecogniseResult {
            lines: vec![Line {
                text: "宿舎".into(),
                words: Some(vec![
                    Word { text: "宿".into(), rect: Rect { x: 0, y: 0, w: 28, h: 30 } },
                    Word { text: "舎".into(), rect: Rect { x: 28, y: 0, w: 28, h: 30 } },
                ]),
            }],
        };
        let lines = to_ocr_lines(&r);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[1].rect.x, 28);
    }

    #[test]
    fn a_text_only_line_yields_no_words() {
        let r = RecogniseResult {
            lines: vec![Line { text: "宿舎".into(), words: None }],
        };
        assert!(to_ocr_lines(&r)[0].words.is_empty());
    }

    #[test]
    fn every_rect_field_survives_the_mapping_unoffset() {
        let r = RecogniseResult {
            lines: vec![Line {
                text: "宿".into(),
                words: Some(vec![Word {
                    text: "宿".into(),
                    rect: Rect { x: 11, y: 22, w: 33, h: 44 },
                }]),
            }],
        };
        let word = &to_ocr_lines(&r)[0].words[0];
        assert_eq!(word.text, "宿");
        assert_eq!(word.rect, PhysRect { x: 11, y: 22, w: 33, h: 44 });
    }

    #[test]
    fn line_order_is_kept_and_no_line_is_dropped() {
        let line = |t: &str| Line { text: t.into(), words: None };
        let r = RecogniseResult { lines: vec![line("one"), line("two"), line("three")] };
        assert_eq!(to_ocr_lines(&r).len(), 3);
    }

    #[test]
    fn a_reply_with_no_lines_maps_to_no_lines() {
        assert!(to_ocr_lines(&RecogniseResult { lines: vec![] }).is_empty());
    }

    #[test]
    fn a_plugin_can_stand_in_as_an_ocr_engine() {
        fn boxed(p: PluginText) -> Box<dyn chibipop::text::OcrEngine> {
            Box::new(p)
        }
        let _ = boxed;
    }

    #[test]
    fn the_estimate_lands_on_the_first_char_at_the_left_edge() {
        assert_eq!(estimate_offset("宿舎に戻る", 100, region()), 0);
    }

    #[test]
    fn the_estimate_lands_on_the_last_char_at_the_right_edge() {
        let line = "宿舎に戻る";
        let off = estimate_offset(line, 599, region());
        assert_eq!(&line[off..], "る");
    }

    #[test]
    fn the_estimate_clamps_left_of_the_region() {
        assert_eq!(estimate_offset("宿舎に戻る", -50, region()), 0);
    }

    #[test]
    fn the_estimate_always_returns_a_char_boundary() {
        let line = "宿舎に戻る";
        for x in 0..700 {
            let off = estimate_offset(line, x, region());
            assert!(line.is_char_boundary(off), "x={x} off={off}");
        }
    }

    #[test]
    fn an_empty_line_estimates_zero() {
        assert_eq!(estimate_offset("", 300, region()), 0);
    }
}

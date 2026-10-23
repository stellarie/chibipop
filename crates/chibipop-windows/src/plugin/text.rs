//! Adapt plugin text providers to the core OCR seam.
//!
//! This module keeps plugin OCR on the same path as the built-in engine.
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

/// Map wire rectangles to OCR words. Do not add an offset.
///
/// The wire rectangles already use image-local coordinates.
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
    monitor: Option<crate::text::runtime::OcrMonitor>,
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
            monitor: None,
        }
    }

    pub fn disabled(&self) -> bool {
        self.strikes.borrow().disabled()
    }

    pub fn with_monitor(mut self, monitor: crate::text::runtime::OcrMonitor) -> Self {
        self.monitor = Some(monitor);
        self
    }

    fn attempt(&self, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        let png = crate::image::encode_bgra_to_png_fast(buf, w, h)?;
        let params = RecogniseParams {
            image_png: base64::engine::general_purpose::STANDARD.encode(&png),
            // The request image defines its own frame, so the region starts at the origin.
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

/// `PluginText` supplies plugin OCR through the core `OcrEngine` seam.
///
/// The downstream handling applies the normal hover and strike rules.
impl chibipop::text::OcrEngine for PluginText {
    fn recognise(&self, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        if self.disabled() {
            self.host.borrow_mut().shutdown();
            if let Some(monitor) = &self.monitor {
                monitor.publish(&self.name, &self.language, false);
            }
            let why = self.strikes.borrow().last_error().unwrap_or("no detail").to_string();
            bail!("plugin \"{}\" is disabled: {why}", self.name);
        }
        match self.attempt(buf, w, h) {
            Ok(lines) => {
                self.strikes.borrow_mut().record(true);
                if let Some(monitor) = &self.monitor {
                    monitor.publish(&self.name, &self.language, true);
                }
                Ok(lines)
            }
            Err(e) => {
                let mut strikes = self.strikes.borrow_mut();
                strikes.note(&format!("{e:#}"));
                if let Some(notice) = strikes.record(false) {
                    eprintln!("chibipop: {}: {notice}", self.name);
                }
                let disabled = strikes.disabled();
                drop(strikes);
                if disabled {
                    self.host.borrow_mut().shutdown();
                }
                if let Some(monitor) = &self.monitor {
                    monitor.publish(&self.name, &self.language, !disabled);
                }
                Err(e)
            }
        }
    }

    fn set_language(&mut self, tag: &str) {
        if let Some(monitor) = &self.monitor {
            monitor.publish(&self.name, &self.language, !self.disabled());
        }
        // Keep the first language from the manifest. Report a reload request when
        // its tag differs because this adapter does not change the manifest choice.
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
    fn a_plugin_can_stand_in_as_an_ocr_engine() {
        fn boxed(p: PluginText) -> Box<dyn chibipop::text::OcrEngine> {
            Box::new(p)
        }
        let _ = boxed;
    }
}

//! Plugin text becomes a span.
use crate::geom::{PhysPoint, PhysRect};
use crate::plugin::proto::{RecogniseResult, Rect};
use crate::text::layout::TextGeom;
use crate::text::TextSpan;

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

fn to_screen(r: Rect, region: PhysRect, scale: i32) -> PhysRect {
    let s = scale.max(1);
    PhysRect {
        x: region.x + r.x / s,
        y: region.y + r.y / s,
        w: (r.w / s).max(1),
        h: (r.h / s).max(1),
    }
}

pub fn span_from_lines(
    r: &RecogniseResult,
    cursor: PhysPoint,
    region: PhysRect,
    scale: i32,
) -> Option<TextSpan> {
    let line = r.lines.first()?;
    if line.text.is_empty() {
        return None;
    }
    let Some(words) = line.words.as_ref() else {
        let off = estimate_offset(&line.text, cursor.x, region);
        return Some(TextSpan {
            text: line.text.clone(),
            cursor_byte_offset: off,
            anchor: region,
            geom: vec![],
        });
    };
    let geom: Vec<TextGeom> = words
        .iter()
        .map(|w| TextGeom {
            char_count: w.text.chars().count(),
            rect: to_screen(w.rect, region, scale),
        })
        .collect();
    let mut off = 0usize;
    let mut anchor = region;
    for (w, g) in words.iter().zip(&geom) {
        if cursor.x >= g.rect.x && cursor.x < g.rect.x + g.rect.w {
            anchor = g.rect;
            break;
        }
        off += w.text.len();
    }
    let off = off.min(line.text.len());
    Some(TextSpan {
        text: line.text.clone(),
        cursor_byte_offset: off,
        anchor,
        geom,
    })
}

use crate::plugin::host::Host;
use crate::plugin::manifest::Manifest;
use crate::plugin::strikes::Strikes;
use std::cell::RefCell;
use std::time::Duration;

pub struct PluginText {
    pub host: RefCell<Host>,
    pub(crate) strikes: RefCell<Strikes>,
    pub name: String,
    pub geometry: bool,
    pub language: String,
    pub timeout: Duration,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::proto::{Line, Word};

    fn region() -> PhysRect {
        PhysRect { x: 100, y: 200, w: 500, h: 100 }
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

    #[test]
    fn geometry_maps_image_pixels_back_to_the_screen() {
        let r = RecogniseResult {
            lines: vec![Line {
                text: "宿舎に".into(),
                words: Some(vec![Word {
                    text: "宿舎".into(),
                    rect: Rect { x: 0, y: 0, w: 112, h: 60 },
                }]),
            }],
        };
        let span =
            span_from_lines(&r, PhysPoint { x: 110, y: 210 }, region(), 2).unwrap();
        assert_eq!(span.geom[0].rect, PhysRect { x: 100, y: 200, w: 56, h: 30 });
        assert_eq!(span.geom[0].char_count, 2);
        assert_eq!(span.anchor, span.geom[0].rect);
    }

    #[test]
    fn a_text_only_line_yields_empty_geometry() {
        let r = RecogniseResult {
            lines: vec![Line { text: "宿舎に戻る".into(), words: None }],
        };
        let span =
            span_from_lines(&r, PhysPoint { x: 350, y: 210 }, region(), 2).unwrap();
        assert!(span.geom.is_empty());
        assert_eq!(span.anchor, region());
    }
}

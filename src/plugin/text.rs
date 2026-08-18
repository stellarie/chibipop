//! Plugin text helpers.
use crate::geom::PhysRect;

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
}

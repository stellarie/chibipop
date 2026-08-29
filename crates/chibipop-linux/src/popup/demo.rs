//! The canned popup: a real surface, real layout, real glyphs, with no
//! capture and no OCR behind it.
//!
//! The pipeline that produces a `Presentation` from the screen is
//! ticket 35's; this exists so the surface, the placement round-trip
//! and the JP text stack can be driven - and smoke-tested on a live
//! compositor - before it lands. It is env-gated
//! (`CHIBIPOP_POPUP_DEMO=1`) and drives off the existing trigger verbs,
//! so the forever verb set on the control socket stays untouched.
//!
//! The content is chosen for the one thing a screenshot can prove:
//! every kanji on the "variants" line is drawn differently by Noto Sans
//! CJK JP and by CJK SC, so a wrong `FontSystem` locale is visible
//! rather than silent.

use chibipop::geom::PhysRect;
use chibipop::present::{Card, CollapsedRow, GlossBlock, Presentation};

/// The env gate and its optional fixed anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Demo {
    pub armed: bool,
    /// `CHIBIPOP_POPUP_DEMO_ANCHOR=x,y,w,h`, global physical pixels.
    /// A fixed anchor is what makes a smoke test reproducible without
    /// synthesizing any seat input.
    pub anchor: Option<PhysRect>,
}

impl Demo {
    pub const ENV: &'static str = "CHIBIPOP_POPUP_DEMO";
    pub const ANCHOR_ENV: &'static str = "CHIBIPOP_POPUP_DEMO_ANCHOR";

    pub fn from_env() -> Demo {
        let armed = std::env::var(Demo::ENV).is_ok_and(|v| v == "1");
        let anchor = std::env::var(Demo::ANCHOR_ENV).ok().as_deref().and_then(parse_anchor);
        Demo { armed, anchor }
    }
}

/// `x,y,w,h` in global physical pixels.
pub fn parse_anchor(text: &str) -> Option<PhysRect> {
    let mut it = text.split(',').map(str::trim).map(str::parse::<i32>);
    let x = it.next()?.ok()?;
    let y = it.next()?.ok()?;
    let w = it.next()?.ok()?;
    let h = it.next()?.ok()?;
    if it.next().is_some() || w <= 0 || h <= 0 {
        return None;
    }
    Some(PhysRect { x, y, w, h })
}

/// One popup's worth of view model.
///
/// Deliberately shaped like a real hover: a top card with a reading, a
/// frequency corner, two dictionaries, and collapsed rows below - so
/// the scene exercises every element kind the painter can draw.
pub fn canned() -> Presentation {
    Presentation {
        top: Some(Card {
            written: Some("\u{6f22}\u{5b57}".to_string()),
            reading: Some("\u{304b}\u{3093}\u{3058}".to_string()),
            pos: vec!["n".to_string()],
            freq: Some(1042),
            blocks: vec![
                GlossBlock::parse(
                    "JMdict",
                    r#"["Chinese character; Chinese characters","kanji"]"#,
                ),
                // Every one of these is drawn differently by the JP and the
                // SC faces of Noto Sans CJK: 骨 直 今 雪 兵 令 具 単 説 対
                GlossBlock::parse(
                    "variants",
                    "[\"\u{9aa8} \u{76f4} \u{4eca} \u{96ea} \u{5175} \u{4ee4} \u{5177} \
                      \u{5358} \u{8aac} \u{5bfe}\"]",
                ),
            ],
            match_len: 2,
        }),
        collapsed: vec![
            CollapsedRow {
                written: Some("\u{6f22}\u{6587}".to_string()),
                reading: Some("\u{304b}\u{3093}\u{3076}\u{3093}".to_string()),
                summary: "classical Chinese writing".to_string(),
            },
            CollapsedRow {
                written: Some("\u{6f22}\u{65b9}".to_string()),
                reading: Some("\u{304b}\u{3093}\u{307d}\u{3046}".to_string()),
                summary: "traditional Chinese medicine".to_string(),
            },
        ],
        all_cards: Vec::new(),
        sentence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_anchor_parses_as_four_physical_pixels() {
        assert_eq!(Some(PhysRect { x: 600, y: 400, w: 120, h: 32 }), parse_anchor("600,400,120,32"));
        assert_eq!(Some(PhysRect { x: -20, y: 0, w: 1, h: 1 }), parse_anchor(" -20 , 0 , 1 , 1 "));
    }

    #[test]
    fn a_malformed_anchor_is_refused_rather_than_guessed() {
        for text in ["", "1,2,3", "1,2,3,4,5", "1,2,0,4", "1,2,3,-4", "a,b,c,d"] {
            assert_eq!(None, parse_anchor(text), "{text:?} must not parse");
        }
    }

    /// The demo exists to prove JP glyph variants on screen, so the
    /// scene must actually contain kanji that differ between the JP and
    /// SC faces.
    #[test]
    fn the_canned_presentation_carries_variant_kanji() {
        let p = canned();
        let card = p.top.expect("a top card");
        let glosses: String = card.blocks.iter().flat_map(|b| b.glosses.clone()).collect();
        for ch in ['\u{9aa8}', '\u{76f4}', '\u{4eca}', '\u{96ea}'] {
            assert!(glosses.contains(ch), "{ch} is missing from the demo scene");
        }
        assert_eq!(Some("\u{6f22}\u{5b57}"), card.written.as_deref());
        assert_eq!(2, p.collapsed.len(), "collapsed rows exercise the side panel");
    }
}

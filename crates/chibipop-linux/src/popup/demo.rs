//! This demo checks the popup surface, layout, and glyph selection on a live compositor.
//! It does not use capture or OCR.
//!
//! It bypasses the screen pipeline that creates a `Presentation`.
//! Developers can use it to test popup placement and the Japanese text stack.
//! Set `CHIBIPOP_POPUP_DEMO=1` to enable the demo.
//! Current trigger verbs control the demo, so it does not add a control socket verb.
//!
//! A screenshot shows whether the text stack selects the required glyph variants.
//! Noto Sans CJK JP and Noto Sans CJK SC select different glyphs for each
//! kanji on the "variants" line.
//! An incorrect `FontSystem` locale therefore produces a visible error.

use chibipop::geom::PhysRect;
use chibipop::present::{Card, CollapsedRow, GlossBlock, Presentation};

/// Stores whether the demo is active and its optional fixed anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Demo {
    pub armed: bool,
    /// `CHIBIPOP_POPUP_DEMO_ANCHOR=x,y,w,h` sets an anchor in global physical pixels.
    /// A fixed anchor makes the smoke test repeatable without seat input.
    pub anchor: Option<PhysRect>,
    /// The demo stores the last lookup that it answered.
    /// A demo `trigger-up` uses this id with the Controller to hide the popup
    /// in every trigger mode. Live mode ignores a key release.
    pub request: Option<chibipop::controller::RequestId>,
}

impl Demo {
    pub const ENV: &'static str = "CHIBIPOP_POPUP_DEMO";
    pub const ANCHOR_ENV: &'static str = "CHIBIPOP_POPUP_DEMO_ANCHOR";

    pub fn from_env() -> Demo {
        let armed = std::env::var(Demo::ENV).is_ok_and(|v| v == "1");
        let anchor = std::env::var(Demo::ANCHOR_ENV).ok().as_deref().and_then(parse_anchor);
        Demo { armed, anchor, request: None }
    }
}

/// Parses `x,y,w,h` as a rectangle in global physical pixels.
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

/// Creates a view model for one popup.
///
/// The model matches a real hover.
/// It has a top card, a kana form, a frequency corner, two Dictionaries,
/// and collapsed rows.
/// The scene therefore covers each element kind that the paint code supports.
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
                // Noto Sans CJK JP and Noto Sans CJK SC select different glyphs
                // for each character:
                GlossBlock::parse(
                    "variants",
                    "[\"\u{9aa8} \u{76f4} \u{4eca} \u{96ea} \u{5175} \u{4ee4} \u{5177} \
                      \u{5358} \u{8aac} \u{5bfe}\"]",
                ),
            ],
            match_len: 2,
            pitch: Vec::new(),
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

    /// The demo must include kanji that differ between the Noto Sans CJK JP
    /// and Noto Sans CJK SC faces.
    /// This difference makes locale errors visible.
    #[test]
    fn the_canned_presentation_carries_variant_kanji() {
        let p = canned();
        let card = p.top.expect("a top card");
        let glosses: String = card.blocks.iter().flat_map(GlossBlock::glosses).collect();
        for ch in ['\u{9aa8}', '\u{76f4}', '\u{4eca}', '\u{96ea}'] {
            assert!(glosses.contains(ch), "{ch} is missing from the demo scene");
        }
        assert_eq!(Some("\u{6f22}\u{5b57}"), card.written.as_deref());
        assert_eq!(2, p.collapsed.len(), "collapsed rows exercise the side panel");
    }
}

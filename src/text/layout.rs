//! Pure reasoning over recognised text: which character is under the cursor,
//! which way the line runs, and what string to hand the lookup engine.
//!
//! Nothing here touches a Windows API. The OCR engine's output is converted to
//! these plain types at the boundary, so every decision below is unit-testable
//! without a screen.

use crate::geom::{PhysPoint, PhysRect};
use crate::text::TextSpan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrWord {
    pub text: String,
    pub rect: PhysRect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrLine {
    pub words: Vec<OcrWord>,
}

/// Find the word under `cursor`, returning `(line_index, word_index)`.
///
/// Direct containment wins. Otherwise the nearest word is accepted only if the
/// cursor is within half that word's own height of it — hovering the gap
/// between two characters is normal and should not fail, but hovering blank
/// space well away from text should. Ties break by document order.
pub fn hit_scan(lines: &[OcrLine], cursor: PhysPoint) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, f64)> = None;

    for (li, line) in lines.iter().enumerate() {
        for (wi, word) in line.words.iter().enumerate() {
            if word.rect.contains(cursor) {
                return Some((li, wi));
            }
            let d = word.rect.edge_distance_to(cursor);
            if d > (word.rect.h as f64) / 2.0 {
                continue;
            }
            // Strictly-less keeps the first-encountered word on a tie, which
            // is document order because of the iteration order above.
            match best {
                Some((_, _, bd)) if d >= bd => {}
                _ => best = Some((li, wi, d)),
            }
        }
    }

    best.map(|(li, wi, _)| (li, wi))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Debug)]
pub struct Resolved {
    pub span: TextSpan,
    pub orientation: Orientation,
}

/// Decide which way a line runs by comparing the spread of its word centres.
///
/// Fewer than two words carries no directional information, so `Horizontal` is
/// returned by convention — with one word the reading order is the same either
/// way, so the choice cannot affect the result.
pub fn orientation_of(line: &OcrLine) -> Orientation {
    if line.words.len() < 2 {
        return Orientation::Horizontal;
    }
    let centres: Vec<PhysPoint> = line.words.iter().map(|w| w.rect.center()).collect();
    let spread = |vals: Vec<i32>| -> i32 {
        let max = vals.iter().copied().max().unwrap_or(0);
        let min = vals.iter().copied().min().unwrap_or(0);
        max - min
    };
    let x_spread = spread(centres.iter().map(|c| c.x).collect());
    let y_spread = spread(centres.iter().map(|c| c.y).collect());
    if y_spread > x_spread { Orientation::Vertical } else { Orientation::Horizontal }
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

/// Repair OCR artefacts that would otherwise break lookup.
///
/// Windows OCR reads the long-vowel mark `ー` as a hyphen. Replacing it only
/// when the preceding character is kana leaves ordinary hyphenated Latin text
/// (`e-mail`, `UTF-8`) untouched.
pub fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    for c in text.chars() {
        let replaced = match c {
            '-' | '\u{2010}' | '\u{2013}' | '\u{2014}' if prev.is_some_and(is_kana) => 'ー',
            other => other,
        };
        out.push(replaced);
        prev = Some(replaced);
    }
    out
}

/// Map a rect from upscaled-image space back into virtual-desktop space.
///
/// The upscale factor exists only between capture and recognition; nothing
/// downstream of this function ever learns that upscaling happened.
pub fn map_from_upscaled(rect: PhysRect, origin: PhysPoint, factor: i32) -> PhysRect {
    rect.scaled_down(factor).translated(origin.x, origin.y)
}

/// Resolve a cursor position against recognised text.
pub fn resolve(lines: &[OcrLine], cursor: PhysPoint) -> Option<Resolved> {
    let (li, wi) = hit_scan(lines, cursor)?;
    let line = &lines[li];
    let orientation = orientation_of(line);

    // Reading order along the line's own axis. The OCR engine usually supplies
    // words in order already, but sorting makes that an assumption we don't
    // have to rely on.
    let mut ordered: Vec<&OcrWord> = line.words.iter().collect();
    match orientation {
        Orientation::Horizontal => ordered.sort_by_key(|w| w.rect.x),
        Orientation::Vertical => ordered.sort_by_key(|w| w.rect.y),
    }

    let hit = &line.words[wi];
    let mut text = String::new();
    let mut cursor_byte_offset = 0usize;
    for w in &ordered {
        if std::ptr::eq(*w, hit) {
            cursor_byte_offset = text.len();
        }
        text.push_str(&w.text);
    }

    // Normalisation can only substitute one char for another of the same
    // char-count, never insert or remove, so byte offsets computed above stay
    // valid only if the replacement is the same length. 'ー' is 3 bytes, as are
    // the dashes it can replace except ASCII '-', which is 1 — so recompute the
    // offset by normalising the prefix rather than assuming it is unchanged.
    let prefix_len = normalise(&text[..cursor_byte_offset]).len();
    let text = normalise(&text);

    Some(Resolved {
        span: TextSpan {
            text,
            cursor_byte_offset: prefix_len,
            anchor: hit.rect,
        },
        orientation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x: i32, y: i32, ww: i32, h: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y, w: ww, h } }
    }
    fn p(x: i32, y: i32) -> PhysPoint { PhysPoint { x, y } }

    /// Three 20x20 characters in a row at y=100, starting at x=100,
    /// with a 10px gap between each.
    fn horizontal_line() -> Vec<OcrLine> {
        vec![OcrLine {
            words: vec![
                w("昨", 100, 100, 20, 20),
                w("日", 130, 100, 20, 20),
                w("は", 160, 100, 20, 20),
            ],
        }]
    }

    #[test]
    fn direct_containment_wins() {
        assert_eq!(Some((0, 1)), hit_scan(&horizontal_line(), p(135, 105)));
    }

    #[test]
    fn near_miss_within_half_a_character_height_is_accepted() {
        // 5px above the top edge; half of a 20px height is 10, so accepted.
        assert_eq!(Some((0, 0)), hit_scan(&horizontal_line(), p(105, 95)));
    }

    #[test]
    fn beyond_tolerance_returns_none() {
        // 40px above — far beyond half a character height.
        assert_eq!(None, hit_scan(&horizontal_line(), p(105, 60)));
    }

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(None, hit_scan(&[], p(105, 105)));
        assert_eq!(None, hit_scan(&[OcrLine { words: vec![] }], p(105, 105)));
    }

    #[test]
    fn ties_break_by_document_order() {
        // A genuine tie needs its own geometry. With `edge_distance_to` using
        // the last *occupied* pixel, a word at x=100 w=20 ends at 119; for an
        // integer midpoint to exist the next word must start at 131, giving
        // distance 6 to each from x=125.
        let lines = vec![OcrLine {
            words: vec![
                w("昨", 100, 100, 20, 20), // occupies x 100..=119
                w("日", 131, 100, 20, 20), // occupies x 131..=150
            ],
        }];
        assert_eq!(6.0, lines[0].words[0].rect.edge_distance_to(p(125, 105)));
        assert_eq!(6.0, lines[0].words[1].rect.edge_distance_to(p(125, 105)));
        // Genuinely tied - the lower word index must win, every time.
        for _ in 0..20 {
            assert_eq!(Some((0, 0)), hit_scan(&lines, p(125, 105)));
        }
    }

    #[test]
    fn scans_across_multiple_lines() {
        let lines = vec![
            OcrLine { words: vec![w("上", 100, 100, 20, 20)] },
            OcrLine { words: vec![w("下", 100, 200, 20, 20)] },
        ];
        assert_eq!(Some((1, 0)), hit_scan(&lines, p(105, 205)));
    }

    fn vertical_line() -> Vec<OcrLine> {
        vec![OcrLine {
            words: vec![
                w("昨", 100, 100, 20, 20),
                w("日", 100, 130, 20, 20),
                w("は", 100, 160, 20, 20),
            ],
        }]
    }

    #[test]
    fn orientation_detects_horizontal() {
        assert_eq!(Orientation::Horizontal, orientation_of(&horizontal_line()[0]));
    }

    #[test]
    fn orientation_detects_vertical() {
        assert_eq!(Orientation::Vertical, orientation_of(&vertical_line()[0]));
    }

    #[test]
    fn single_word_line_is_horizontal_by_convention() {
        let line = OcrLine { words: vec![w("食", 100, 100, 20, 20)] };
        assert_eq!(Orientation::Horizontal, orientation_of(&line));
    }

    #[test]
    fn empty_line_is_horizontal_by_convention() {
        assert_eq!(Orientation::Horizontal, orientation_of(&OcrLine { words: vec![] }));
    }

    #[test]
    fn assembly_orders_horizontally_even_if_words_arrive_shuffled() {
        let line = OcrLine {
            words: vec![
                w("は", 160, 100, 20, 20),
                w("昨", 100, 100, 20, 20),
                w("日", 130, 100, 20, 20),
            ],
        };
        let got = resolve(&[line], p(105, 105)).unwrap();
        assert_eq!("昨日は", got.span.text);
        assert_eq!(0, got.span.cursor_byte_offset);
    }

    #[test]
    fn assembly_orders_vertically_top_to_bottom() {
        let got = resolve(&vertical_line(), p(105, 165)).unwrap();
        assert_eq!("昨日は", got.span.text);
        assert_eq!(Orientation::Vertical, got.orientation);
        // "は" is the third character; 昨 and 日 are 3 bytes each in UTF-8.
        assert_eq!(6, got.span.cursor_byte_offset);
    }

    #[test]
    fn cursor_byte_offset_lands_on_a_char_boundary() {
        let got = resolve(&horizontal_line(), p(165, 105)).unwrap();
        // Slicing at the offset must not panic and must start at the hit char.
        assert!(got.span.text[got.span.cursor_byte_offset..].starts_with('は'));
    }

    #[test]
    fn anchor_is_the_hit_characters_own_rect() {
        let got = resolve(&horizontal_line(), p(135, 105)).unwrap();
        assert_eq!(PhysRect { x: 130, y: 100, w: 20, h: 20 }, got.span.anchor);
    }

    #[test]
    fn resolve_returns_none_when_nothing_is_near() {
        assert_eq!(None, resolve(&horizontal_line(), p(500, 500)).map(|r| r.span.text));
    }

    #[test]
    fn normalise_converts_hyphen_after_kana_to_long_vowel() {
        assert_eq!("ツール", normalise("ツ-ル"));
        assert_eq!("ツール", normalise("ツ‐ル"));
        assert_eq!("ツール", normalise("ツ–ル"));
        assert_eq!("ツール", normalise("ツ—ル"));
    }

    #[test]
    fn normalise_converts_after_hiragana_too() {
        assert_eq!("あー", normalise("あ-"));
    }

    #[test]
    fn normalise_leaves_latin_hyphens_alone() {
        assert_eq!("e-mail", normalise("e-mail"));
        assert_eq!("UTF-8", normalise("UTF-8"));
    }

    #[test]
    fn normalise_leaves_a_hyphen_after_kanji_alone() {
        // Only kana takes a long-vowel mark; a dash after kanji is a dash.
        assert_eq!("日-本", normalise("日-本"));
    }

    #[test]
    fn coordinates_survive_a_negative_origin() {
        // A monitor positioned left of the primary gives the virtual desktop
        // negative coordinates. This must not panic and must stay coherent.
        let got = map_from_upscaled(
            PhysRect { x: 40, y: 20, w: 40, h: 40 },
            PhysPoint { x: -1920, y: -100 },
            2,
        );
        assert_eq!(PhysRect { x: -1900, y: -90, w: 20, h: 20 }, got);
        // And a hit-scan against negative-space boxes still resolves.
        let line = OcrLine {
            words: vec![OcrWord { text: "食".to_string(), rect: got }],
        };
        assert_eq!(Some((0, 0)), hit_scan(&[line], p(-1895, -85)));
    }

    #[test]
    fn map_from_upscaled_divides_then_translates() {
        // A rect at (200,100) in a 2x-upscaled image of a region whose
        // top-left sits at (1000,500) on the virtual desktop.
        let got = map_from_upscaled(
            PhysRect { x: 200, y: 100, w: 40, h: 40 },
            PhysPoint { x: 1000, y: 500 },
            2,
        );
        assert_eq!(PhysRect { x: 1100, y: 550, w: 20, h: 20 }, got);
    }
}

//! Pure reasoning over recognised text: which character is under the cursor,
//! which way the line runs, and what string to hand the lookup engine.
//!
//! Nothing here touches a Windows API. The OCR engine's output is converted to
//! these plain types at the boundary, so every decision below is unit-testable
//! without a screen.

use crate::geom::{PhysPoint, PhysRect};

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
}

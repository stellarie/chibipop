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

/// The capture box: 500×100 physical pixels centred on the cursor.
///
/// **Smaller is more accurate, and this was measured, not assumed.** Windows'
/// OCR segments a whole captured image at once rather than reading character
/// by character, so everything else in the box competes for its attention.
/// Against a visual novel at ~40px text, hovering ten characters of one line
/// and comparing the character handed to lookup:
///
/// | box | correct |
/// |---|---|
/// | 900×300 (the original) | 6/10 — `通`→`過`, `に`→`0`, `風` and `私` dropped |
/// | 500×100 | **10/10** |
/// | 400×100 | 10/10 on that line, but `ロ`→`回` on the next one |
///
/// The wide box was not buying reading distance either: it returned *six*
/// forward characters of `…を過抜ける` against the narrow box's seven of
/// `…を通り抜ける。制`, because the extra width was spent on mangling rather
/// than on more text. A 300-tall box also swallows the neighbouring line and
/// lets the recogniser blend the two.
///
/// The height is not independently risky: `hit_scan` already refuses a cursor
/// further than half a word's height from the text, so a hover this box would
/// clip is one that would not have resolved anyway.
///
/// Tuned against one game at one text size. `probe --region W,H` exists to
/// re-measure this rather than trusting it — see `OcrTextSource::resolve_in_region`.
pub const REGION_W: i32 = 500;
pub const REGION_H: i32 = 100;

/// How far a tile reads along the line, in physical pixels.
///
/// The same measured limit as `REGION_W`: Windows' OCR degrades as more text
/// enters one captured image, and 400-500px is the band where a visual novel
/// at ~40px text recognises correctly. See the two-pass spec section 1.1.
pub const TILE_LEN: i32 = 500;

/// A tile's perpendicular extent, as a multiple of the hovered word's own
/// perpendicular size. Margin for ascenders and slight line drift.
/// `band_of` also floors this at `REGION_H`; see its doc comment.
pub const BAND_FACTOR: f32 = 3.0;

/// The line's perpendicular extent, derived from the hovered word.
///
/// Floored at `REGION_H`: a 44px-tall capture measured on real on-screen
/// text recognised nothing at all, and the threshold sat between 60 and
/// 66px, so the tile is never thinner than the capture pass 1 already
/// proves works.
///
/// The parallel axis is left at the word's own position and size; it carries
/// no meaning here, and `tile_after` replaces it.
pub fn band_of(word: PhysRect, orientation: Orientation) -> PhysRect {
    let c = word.center();
    match orientation {
        Orientation::Horizontal => {
            let h = (((word.h as f32) * BAND_FACTOR).round() as i32).max(REGION_H);
            PhysRect { x: word.x, y: c.y - h / 2, w: word.w, h }
        }
        Orientation::Vertical => {
            let w = (((word.w as f32) * BAND_FACTOR).round() as i32).max(REGION_H);
            PhysRect { x: c.x - w / 2, y: word.y, w, h: word.h }
        }
    }
}

/// A capture box `len` long down the reading direction from `start`, carrying
/// `band`'s perpendicular extent.
pub fn tile_after(band: PhysRect, start: i32, orientation: Orientation, len: i32) -> PhysRect {
    match orientation {
        Orientation::Horizontal => PhysRect { x: start, y: band.y, w: len, h: band.h },
        Orientation::Vertical => PhysRect { x: band.x, y: start, w: band.w, h: len },
    }
}

/// Selects the OCR line nearest `perpendicular_centre` on the perpendicular
/// axis — y for `Horizontal`, x for `Vertical`, `band_of`'s own convention.
///
/// Windows' OCR reports furigana as its own line, separate from the text it
/// annotates, even when both sit inside one tall capture tile. Picking the
/// line actually centred near the hover — instead of flattening every line
/// the tile saw into one word list — drops the ruby with no pixel threshold
/// to tune, and stays correct if the ruby sits below the text rather than
/// above it.
///
/// A line's centre is the mean of its own words' centres on that axis, not
/// its bounding box's midpoint, so one unusually wide or short word can't
/// drag it. Ties keep the first line in `lines`, matching `hit_scan`'s
/// document-order rule. `None` when `lines` is empty or every line has no
/// words — callers turn that into an empty `Vec`, same as no lines at all.
pub fn nearest_line(
    lines: &[OcrLine],
    perpendicular_centre: i32,
    orientation: Orientation,
) -> Option<&OcrLine> {
    let axis = |p: PhysPoint| match orientation {
        Orientation::Horizontal => p.y,
        Orientation::Vertical => p.x,
    };

    let mut best: Option<(&OcrLine, i32)> = None;
    for line in lines {
        if line.words.is_empty() {
            continue;
        }
        let sum: i32 = line.words.iter().map(|w| axis(w.rect.center())).sum();
        let centre = sum / line.words.len() as i32;
        let d = (centre - perpendicular_centre).abs();
        match best {
            Some((_, bd)) if d >= bd => {}
            _ => best = Some((line, d)),
        }
    }
    best.map(|(line, _)| line)
}

/// How close a word's trailing edge may come to a tile's trailing edge before
/// it is treated as possibly clipped.
pub const EDGE_MARGIN: i32 = 4;

/// Maps a rect to a position along the reading axis.
type AxisEdge = fn(&PhysRect) -> i32;

/// Split a tile's words into those safely inside it and the start of the next
/// tile (spec D3).
///
/// A word whose trailing edge lies within [`EDGE_MARGIN`] of the tile's own
/// trailing edge may have been cut by the capture boundary, and a cut glyph is
/// what turns 通 into 過. Such a word is discarded and its leading edge becomes
/// the next tile's start, so it is re-read whole.
///
/// Returns the tile's trailing edge when nothing was clipped, and the
/// clipped word's own leading edge otherwise, so the next tile re-reads
/// it whole. That edge only equals `tile`'s own start when the clipped
/// word began there too - the caller still treats a start that does not
/// advance as a stop.
pub fn split_at_clipped(
    words: &[OcrWord],
    tile: PhysRect,
    orientation: Orientation,
) -> (Vec<&OcrWord>, i32) {
    let (end, lead, trail): (i32, AxisEdge, AxisEdge) = match orientation {
        Orientation::Horizontal => (tile.x + tile.w, |r| r.x, |r| r.x + r.w),
        Orientation::Vertical => (tile.y + tile.h, |r| r.y, |r| r.y + r.h),
    };

    let mut ordered: Vec<&OcrWord> = words.iter().collect();
    ordered.sort_by_key(|word| lead(&word.rect));

    let mut kept = Vec::new();
    for word in ordered {
        if trail(&word.rect) > end - EDGE_MARGIN {
            return (kept, lead(&word.rect));
        }
        kept.push(word);
    }
    (kept, end)
}

/// Read forward along a line in tiles, returning the concatenated text.
///
/// `read` performs one capture-and-recognise for a tile and returns its words
/// in virtual-desktop coordinates; injecting it keeps this loop testable with
/// no screen. The `for _ in 0..max_tiles` bound is what guarantees this loop
/// ends; the other stops - `max_chars` reached, a tile recognising nothing,
/// or a start that does not advance (spec D5) - just end it early, saving up
/// to `max_tiles` real OCR round-trips (each one a screen capture) once a
/// tile can no longer make progress.
///
/// The result is normalised once at the end rather than per tile, so a
/// sequence split across a seam still normalises as one string.
pub fn tile_forward<F>(
    band: PhysRect,
    start: i32,
    orientation: Orientation,
    max_tiles: usize,
    max_chars: usize,
    mut read: F,
) -> String
where
    F: FnMut(PhysRect) -> Vec<OcrWord>,
{
    let mut out = String::new();
    let mut start = start;

    for _ in 0..max_tiles {
        if out.chars().count() >= max_chars {
            break;
        }
        let tile = tile_after(band, start, orientation, TILE_LEN);
        let words = read(tile);
        if words.is_empty() {
            break;
        }
        let (kept, next) = split_at_clipped(&words, tile, orientation);
        for word in kept {
            out.push_str(&word.text);
        }
        if next <= start {
            break;
        }
        start = next;
    }

    normalise(&out)
}

pub fn region_around(cursor: PhysPoint) -> PhysRect {
    PhysRect {
        x: cursor.x - REGION_W / 2,
        y: cursor.y - REGION_H / 2,
        w: REGION_W,
        h: REGION_H,
    }
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

    /// Gap: the byte offset is computed once while assembling the *raw*
    /// text, then recomputed by normalising the prefix, because
    /// normalisation can change byte length (ASCII '-' is 1 byte; the 'ー'
    /// it can become is 3). This line hits that head-on: the hyphen sits
    /// before the hit character, so the raw offset would land mid-character
    /// if the recompute were skipped.
    #[test]
    fn cursor_byte_offset_is_recomputed_after_normalisation_changes_length() {
        let line = OcrLine {
            words: vec![
                w("ツ", 100, 100, 20, 20),
                w("-", 130, 100, 20, 20),
                w("ル", 160, 100, 20, 20),
                w("箱", 190, 100, 20, 20),
            ],
        };
        let got = resolve(&[line], p(195, 105)).unwrap();
        assert_eq!("ツール箱", got.span.text);
        assert!(got.span.text[got.span.cursor_byte_offset..].starts_with('箱'));
        // "ツール" is 3 characters, 3 bytes each in UTF-8 = 9.
        assert_eq!(9, got.span.cursor_byte_offset);
    }

    /// Gap: word identity during assembly is resolved with `std::ptr::eq`,
    /// not by comparing `text`, specifically because Japanese lines repeat
    /// characters. "々" appears three times here, twice *after* the hit.
    /// The assembly loop has no early-exit — it overwrites the offset on
    /// every match it sees — so a text-equality comparison would settle on
    /// the *last* same-text word rather than the hit itself, landing on the
    /// trailing "々" instead of the hit at index 2. (A line with only two
    /// occurrences, with the hit on the last one, can't tell the two
    /// comparisons apart: there is nothing after the hit left to wrongly
    /// match, so both approaches land on the same offset by coincidence.)
    #[test]
    fn repeated_character_resolves_to_the_hit_occurrence() {
        let line = OcrLine {
            words: vec![
                w("々", 100, 100, 20, 20),
                w("人", 130, 100, 20, 20),
                w("々", 160, 100, 20, 20), // the hit: second "々", third word
                w("々", 190, 100, 20, 20), // trailing repeat, see doc comment
            ],
        };
        let got = resolve(&[line], p(170, 110)).unwrap();
        assert_eq!("々人々々", got.span.text);
        // "々人" is 2 characters, 3 bytes each in UTF-8 = 6.
        assert_eq!(6, got.span.cursor_byte_offset);
        assert_eq!("々々", &got.span.text[got.span.cursor_byte_offset..]);
    }

    /// The centring is the contract; the size is a measured tuning that may
    /// move again (see `REGION_W`'s table), so this asserts centring against
    /// whatever the constants currently are rather than restating them.
    #[test]
    fn the_region_is_centred_on_the_cursor() {
        let r = region_around(p(1000, 500));
        assert_eq!(REGION_W, r.w);
        assert_eq!(REGION_H, r.h);
        assert_eq!(1000 - REGION_W / 2, r.x);
        assert_eq!(500 - REGION_H / 2, r.y);
    }

    /// The box must stay wide enough to out-reach `hit_scan`'s own tolerance
    /// vertically - otherwise a hover that resolves would capture a text row
    /// the box has clipped, which is the one shape that makes OCR return
    /// nothing at all rather than something imperfect.
    #[test]
    fn the_region_is_taller_than_a_resolvable_hover_can_be_off_by() {
        let typical_word_h = 40;
        let worst_offset = typical_word_h / 2;
        let needed = worst_offset + typical_word_h / 2;
        assert!(
            REGION_H / 2 >= needed,
            "REGION_H/2 = {} must cover {needed}px of hit-scan slack plus half a glyph",
            REGION_H / 2
        );
    }

    #[test]
    fn band_expands_perpendicular_and_keeps_the_word_centred() {
        let word = PhysRect { x: 1499, y: 870, w: 35, h: 34 };
        let b = band_of(word, Orientation::Horizontal);
        assert_eq!(102, b.h, "max(3.0x the word height, REGION_H)");
        assert_eq!(word.center().y, b.center().y, "same centre line");
        assert_eq!(word.x, b.x, "parallel axis untouched");
    }

    #[test]
    fn band_swaps_axes_for_vertical_text() {
        let word = PhysRect { x: 1438, y: 344, w: 64, h: 70 };
        let b = band_of(word, Orientation::Vertical);
        assert_eq!(192, b.w, "max(3.0x the word width, REGION_H)");
        assert_eq!(word.center().x, b.center().x);
        assert_eq!(word.y, b.y, "parallel axis untouched");
    }

    /// A 44px band (2.0 x 22px, the old factor) measured on real on-screen
    /// text recognised nothing at all - REGION_H must floor small text.
    #[test]
    fn band_floors_small_text_at_region_h() {
        let word = PhysRect { x: 1499, y: 870, w: 20, h: 22 };
        let b = band_of(word, Orientation::Horizontal);
        assert_eq!(REGION_H, b.h, "3.0x22=66 must be floored to REGION_H");
        assert_eq!(word.center().y, b.center().y, "same centre line");
    }

    #[test]
    fn band_factor_still_applies_above_the_floor() {
        let word = PhysRect { x: 1499, y: 870, w: 20, h: 60 };
        let b = band_of(word, Orientation::Horizontal);
        assert_eq!(180, b.h, "3.0x60=180 must not be clamped down to REGION_H");
        assert_eq!(word.center().y, b.center().y, "same centre line");
    }

    #[test]
    fn tile_runs_along_the_reading_direction() {
        let band = PhysRect { x: 100, y: 200, w: 40, h: 80 };
        let h = tile_after(band, 500, Orientation::Horizontal, TILE_LEN);
        assert_eq!(PhysRect { x: 500, y: 200, w: TILE_LEN, h: 80 }, h);

        let v = tile_after(band, 500, Orientation::Vertical, TILE_LEN);
        assert_eq!(PhysRect { x: 100, y: 500, w: 40, h: TILE_LEN }, v);
    }

    /// Furigana-fix measurement: furigana at y=1345 h=11 (centre 1350)
    /// vs. the real line at y=1362 h=22 (mean centre 1373 - ~1px from
    /// the spec's own bounding-box midpoint of 1372; see `nearest_line`
    /// on mean vs. midpoint). Hovering 1372 must pick the real line.
    #[test]
    fn nearest_line_picks_the_real_line_over_furigana() {
        let furigana = OcrLine {
            words: (0..3).map(|i| w("furi", 3320 + i * 13, 1345, 13, 11)).collect(),
        };
        let real_line = OcrLine {
            words: (0..18).map(|i| w("real", 2873 + i * 22, 1362, 22, 22)).collect(),
        };
        let lines = vec![furigana, real_line.clone()];
        assert_eq!(Some(&real_line), nearest_line(&lines, 1372, Orientation::Horizontal));
    }

    /// Same measurement, rotated: a vertical line's perpendicular axis
    /// is x, so the same two centres (1350, 1373) now come from
    /// word.rect.x/w instead of y/h.
    #[test]
    fn nearest_line_mirrors_axes_for_vertical_text() {
        let furigana = OcrLine {
            words: (0..3).map(|i| w("furi", 1345, 3320 + i * 13, 11, 13)).collect(),
        };
        let real_line = OcrLine {
            words: (0..18).map(|i| w("real", 1362, 2873 + i * 22, 22, 22)).collect(),
        };
        let lines = vec![furigana, real_line.clone()];
        assert_eq!(Some(&real_line), nearest_line(&lines, 1372, Orientation::Vertical));
    }

    #[test]
    fn nearest_line_empty_input_returns_none() {
        assert_eq!(None, nearest_line(&[], 1372, Orientation::Horizontal));
    }

    #[test]
    fn nearest_line_skips_a_line_with_no_words() {
        let empty = OcrLine { words: vec![] };
        let real_line = OcrLine { words: vec![w("real", 100, 1372, 20, 20)] };
        let lines = vec![empty, real_line.clone()];
        assert_eq!(Some(&real_line), nearest_line(&lines, 1372, Orientation::Horizontal));
    }

    /// A genuine tie: both centres (110, 150) sit 20px from target 130,
    /// so document order alone must decide.
    #[test]
    fn nearest_line_ties_break_by_document_order() {
        let first_line = OcrLine { words: vec![w("a", 0, 100, 20, 20)] };
        let second_line = OcrLine { words: vec![w("b", 0, 140, 20, 20)] };
        let lines = vec![first_line.clone(), second_line];
        assert_eq!(Some(&first_line), nearest_line(&lines, 130, Orientation::Horizontal));
    }

    fn hword(text: &str, x: i32, width: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y: 100, w: width, h: 40 } }
    }

    #[test]
    fn a_word_touching_the_trailing_edge_is_dropped_and_becomes_the_next_start() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![hword("あ", 10, 40), hword("い", 60, 40), hword("う", 470, 40)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(vec!["あ", "い"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
        assert_eq!(470, next, "the clipped word's leading edge starts the next tile");
    }

    #[test]
    fn with_nothing_clipped_the_next_start_is_the_tiles_own_edge() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![hword("あ", 10, 40), hword("い", 60, 40)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(2, kept.len());
        assert_eq!(500, next);
    }

    /// end=500, threshold=496 (500-4). Landing exactly on the threshold
    /// must be kept, since `trail > threshold` is false when equal - the
    /// one fact the fixtures elsewhere in this file never pin, because
    /// they all clip 10px past `end` rather than 1px past the threshold.
    #[test]
    fn trailing_edge_exactly_at_the_margin_boundary_is_kept() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![hword("あ", 456, 40)]; // trail = 456+40 = 496
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(1, kept.len(), "496 must not be treated as clipped");
        assert_eq!(500, next);
    }

    /// Same tile, same word, one pixel further right: trail becomes 497
    /// and must flip to discarded. The only difference from the previous
    /// test is that single pixel.
    #[test]
    fn trailing_edge_one_pixel_past_the_margin_boundary_is_discarded() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![hword("あ", 457, 40)]; // trail = 457+40 = 497
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(457, next, "must return the clipped word's own leading edge");
    }

    /// Spec D3: a tile too narrow to hold one whole word keeps nothing and
    /// returns its own start, which D5's strict-advance rule turns into a stop.
    #[test]
    fn a_first_word_already_clipped_keeps_nothing_and_does_not_advance() {
        let tile = PhysRect { x: 400, y: 90, w: 100, h: 60 };
        let words = vec![hword("あ", 400, 120)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(400, next, "must not advance past the tile's own start");
    }

    /// The general case the doc comment describes: a clipped word does
    /// not always begin where the tile does, and the returned edge is
    /// the word's own - here strictly greater than the tile's start,
    /// unlike the coincidental case above where the two are equal.
    #[test]
    fn a_clipped_first_word_starting_mid_tile_returns_its_own_leading_edge() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![hword("あ", 200, 400)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(200, next);
        assert!(next > tile.x, "must be the word's edge, not the tile's start");
    }

    #[test]
    fn the_split_reads_top_to_bottom_for_vertical_text() {
        let tile = PhysRect { x: 90, y: 0, w: 60, h: 500 };
        let words = vec![
            OcrWord { text: "上".into(), rect: PhysRect { x: 100, y: 10, w: 40, h: 40 } },
            OcrWord { text: "下".into(), rect: PhysRect { x: 100, y: 470, w: 40, h: 40 } },
        ];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Vertical);
        assert_eq!(vec!["上"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
        assert_eq!(470, next);
    }

    /// OCR does not promise reading order, so the split must not rely on it.
    #[test]
    fn words_arriving_out_of_order_are_sorted_before_splitting() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = vec![hword("い", 60, 40), hword("あ", 10, 40)];
        let (kept, _) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(vec!["あ", "い"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
    }

    #[test]
    fn tiling_concatenates_successive_tiles() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, |tile| {
            calls += 1;
            vec![hword("あ", tile.x + 10, 40), hword("い", tile.x + 60, 40)]
        });
        assert_eq!(3, calls, "runs up to max_tiles");
        assert_eq!("あいあいあい", text);
    }

    #[test]
    fn each_tile_reads_the_region_after_the_last_one() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut seen = Vec::new();
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, |tile| {
            seen.push(tile.x);
            let label = (tile.x / TILE_LEN).to_string();
            vec![hword(&label, tile.x + 10, 40)]
        });
        assert_eq!(vec![0, TILE_LEN, TILE_LEN * 2], seen, "each tile starts where the last ended");
        assert_eq!("012", text, "a loop that never advances would repeat one region");
    }

    #[test]
    fn tiling_stops_once_max_chars_is_reached() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        tile_forward(band, 0, Orientation::Horizontal, 5, 2, |tile| {
            calls += 1;
            vec![hword("あ", tile.x + 10, 40), hword("い", tile.x + 60, 40)]
        });
        assert_eq!(1, calls, "two characters already meet the budget");
    }

    #[test]
    fn tiling_stops_when_a_tile_recognises_nothing() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, |_| {
            calls += 1;
            Vec::new()
        });
        assert_eq!(1, calls);
        assert_eq!("", text);
    }

    /// Spec D5. Without the strict-advance guard this loops until max_tiles,
    /// and with a larger cap it would not terminate at all. The reader here
    /// always returns one word that fills its whole tile, so every split
    /// reports the tile's own start back.
    #[test]
    fn tiling_stops_when_the_next_start_does_not_advance() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        tile_forward(band, 0, Orientation::Horizontal, 100, 25, |tile| {
            calls += 1;
            vec![hword("あ", tile.x, tile.w)]
        });
        assert_eq!(1, calls, "a start that cannot advance must stop the loop");
    }

    #[test]
    fn tiling_normalises_across_a_seam() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut first = true;
        let text = tile_forward(band, 0, Orientation::Horizontal, 2, 25, |tile| {
            let out = if first {
                vec![hword("ツ", tile.x + 10, 40)]
            } else {
                vec![hword("-ル", tile.x + 10, 40)]
            };
            first = false;
            out
        });
        assert_eq!("ツール", text, "the hyphen after kana normalises across the join");
    }
}

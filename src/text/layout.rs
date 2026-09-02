//! This module performs layout with physical pixels.
//! It has no Win32 code.

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

/// Join recognized lines into one plain-text block.
///
/// The function joins words without spaces because Japanese OCR splits words for recognition, not because the source has spaces.
/// A space adds text that does not exist on the screen.
/// The function keeps line breaks because they appear on the screen.
///
/// Core owns this rule because both platform bins copy region text to the clipboard (`ocr-clipboard`).
/// One shared rule prevents two implementations (ARCHITECTURE.md#workspace-and-seams).
/// The result matches the Windows action byte for byte.
/// It uses one `String` instead of a `Vec<String>` and a `join`.
/// The code builds and reads the clipboard payload once.
pub fn join_lines(lines: &[OcrLine]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        // A line index matters, not line emptiness.
        // A wordless line still ends the line above it.
        if i > 0 {
            out.push('\n');
        }
        for word in &line.words {
            out.push_str(&word.text);
        }
    }
    out
}

/// Return the word at `cursor`.
pub fn hit_scan(lines: &[OcrLine], cursor: PhysPoint, scan_alnum: bool) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, f64)> = None;

    for (li, line) in lines.iter().enumerate() {
        for (wi, word) in line.words.iter().enumerate() {
            if !scan_alnum && !has_scannable_script(&word.text) {
                continue;
            }
            if word.rect.contains(cursor) {
                return Some((li, wi));
            }
            let d = word.rect.edge_distance_to(cursor);
            if d > (word.rect.h as f64) / 2.0 {
                continue;
            }
            // The first word with the same distance wins.
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

/// Return the orientation of the line.
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

/// Convert an OCR hyphen after kana to `ー`.
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

/// Map an upscaled rect into desktop physical pixels.
pub fn map_from_upscaled(rect: PhysRect, origin: PhysPoint, factor: i32) -> PhysRect {
    rect.scaled_down(factor).translated(origin.x, origin.y)
}

/// A smaller region gives more accurate OCR.
pub const REGION_W: i32 = 500;
pub const REGION_H: i32 = 100;

/// A 400-500px tile gives correct OCR.
pub const TILE_LEN: i32 = 500;

/// This factor leaves room for ascenders and position drift.
pub const BAND_FACTOR: f32 = 3.0;

/// Build a band around a word and floor its short axis.
pub fn band_of(word: PhysRect, orientation: Orientation, short_floor: i32) -> PhysRect {
    let c = word.center();
    match orientation {
        Orientation::Horizontal => {
            let h = (((word.h as f32) * BAND_FACTOR).round() as i32).max(short_floor);
            PhysRect { x: word.x, y: c.y - h / 2, w: word.w, h }
        }
        Orientation::Vertical => {
            let w = (((word.w as f32) * BAND_FACTOR).round() as i32).max(short_floor);
            PhysRect { x: c.x - w / 2, y: word.y, w, h: word.h }
        }
    }
}

/// Build one tile with length `len`.
pub fn tile_after(band: PhysRect, start: i32, orientation: Orientation, len: i32) -> PhysRect {
    match orientation {
        Orientation::Horizontal => PhysRect { x: start, y: band.y, w: len, h: band.h },
        Orientation::Vertical => PhysRect { x: band.x, y: start, w: band.w, h: len },
    }
}

/// Return `None` when the tile has zero extent.
pub fn clamp_tile(tile: PhysRect, bounds: PhysRect) -> Option<PhysRect> {
    let x0 = tile.x.max(bounds.x);
    let y0 = tile.y.max(bounds.y);
    let x1 = (tile.x + tile.w).min(bounds.x + bounds.w);
    let y1 = (tile.y + tile.h).min(bounds.y + bounds.h);
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 { None } else { Some(PhysRect { x: x0, y: y0, w, h }) }
}

/// Return the OCR line nearest to the hover point.
pub fn nearest_line(
    lines: &[OcrLine],
    perpendicular_centre: i32,
    orientation: Orientation,
    tolerance: i32,
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
        if d > tolerance {
            continue;
        }
        match best {
            Some((_, bd)) if d >= bd => {}
            _ => best = Some((line, d)),
        }
    }
    best.map(|(line, _)| line)
}

/// This margin protects words near a tile edge.
pub const EDGE_MARGIN: i32 = 4;

/// This function returns an edge on the scan axis.
type AxisEdge = fn(&PhysRect) -> i32;

/// Keep complete words and return the next tile start.
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

/// Return words whose start edge is near or after `start`.
pub fn drop_leading(words: &[OcrWord], start: i32, orientation: Orientation) -> Vec<OcrWord> {
    let lead: AxisEdge = match orientation {
        Orientation::Horizontal => |r| r.x,
        Orientation::Vertical => |r| r.y,
    };
    words
        .iter()
        .filter(|w| lead(&w.rect) >= start - EDGE_MARGIN)
        .cloned()
        .collect()
}

/// Read forward through a sequence of tiles.
pub fn tile_forward<F>(
    band: PhysRect,
    start: i32,
    orientation: Orientation,
    max_tiles: usize,
    max_chars: usize,
    bounds: PhysRect,
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
        let Some(tile) = clamp_tile(tile, bounds) else {
            break;
        };
        let words = read(tile);
        let words = drop_leading(&words, start, orientation);
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

/// A text geometry entry stores a character count and its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextGeom {
    pub char_count: usize,
    pub rect: PhysRect,
}

/// Expand the union outward by `pad` and saturate its bounds.
pub fn union_chars(geom: &[TextGeom], from: usize, len: usize, pad: i32) -> Option<PhysRect> {
    if len == 0 {
        return None;
    }
    let end = from.saturating_add(len);
    let mut at = 0usize;
    let mut acc: Option<PhysRect> = None;

    for entry in geom {
        let next = at + entry.char_count;
        if next > from && at < end {
            acc = Some(match acc {
                None => entry.rect,
                Some(a) => {
                    let l = a.x.min(entry.rect.x);
                    let t = a.y.min(entry.rect.y);
                    let r = (a.x + a.w).max(entry.rect.x + entry.rect.w);
                    let b = (a.y + a.h).max(entry.rect.y + entry.rect.h);
                    PhysRect { x: l, y: t, w: r - l, h: b - t }
                }
            });
        }
        at = next;
    }

    acc.map(|r| PhysRect { x: r.x - pad, y: r.y - pad, w: r.w + 2 * pad, h: r.h + 2 * pad })
}

pub fn region_around(cursor: PhysPoint, prefer_vertical: bool, size: CaptureSize) -> PhysRect {
    let (w, h) = if prefer_vertical {
        (size.h, size.w)
    } else {
        (size.w, size.h)
    };
    PhysRect {
        x: cursor.x - w / 2,
        y: cursor.y - h / 2,
        w,
        h,
    }
}

/// Return the gap when `next` continues `current`.
fn continuation_gap(current: &OcrLine, next: &OcrLine, orientation: Orientation) -> Option<i32> {
    if current.words.is_empty() || next.words.is_empty() {
        return None;
    }

    let axis = |p: PhysPoint| match orientation {
        Orientation::Horizontal => p.y,
        Orientation::Vertical => p.x,
    };
    let (cross, edge): (AxisEdge, AxisEdge) = match orientation {
        Orientation::Horizontal => (|r| r.h, |r| r.x),
        Orientation::Vertical => (|r| r.w, |r| r.y),
    };

    let perp = |l: &OcrLine| -> i32 {
        let sum: i32 = l.words.iter().map(|w| axis(w.rect.center())).sum();
        sum / l.words.len() as i32
    };
    let thick = |l: &OcrLine| -> i32 {
        let sum: i32 = l.words.iter().map(|w| cross(&w.rect)).sum();
        sum / l.words.len() as i32
    };
    let start = |l: &OcrLine| l.words.iter().map(|w| edge(&w.rect)).min().unwrap_or(0);

    let line_h = thick(current).max(thick(next));
    if line_h <= 0 {
        return None;
    }

    let gap = match orientation {
        Orientation::Horizontal => perp(next) - perp(current),
        Orientation::Vertical => perp(current) - perp(next),
    };
    if gap <= 0 || gap > line_h * 3 / 2 {
        return None;
    }
    if start(next) > start(current) + line_h / 2 {
        return None;
    }
    Some(gap)
}

/// Return whether text continues on the next line.
#[cfg(test)]
fn continues_on_next_line(current: &OcrLine, next: &OcrLine, orientation: Orientation) -> bool {
    continuation_gap(current, next, orientation).is_some()
}

/// Find the nearest valid line that continues the current line.
fn find_continuation(lines: &[OcrLine], li: usize, orientation: Orientation) -> Option<&OcrLine> {
    let current = &lines[li];
    lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != li)
        .filter_map(|(_, candidate)| {
            continuation_gap(current, candidate, orientation).map(|gap| (gap, candidate))
        })
        .min_by_key(|(gap, _)| *gap)
        .map(|(_, candidate)| candidate)
}

/// Return pass 1's tail after edge trim.
pub fn head_and_tail(
    lines: &[OcrLine],
    cursor: PhysPoint,
    region: PhysRect,
    scan_alnum: bool,
) -> Option<(String, i32, Orientation)> {
    let (li, wi) = hit_scan(lines, cursor, scan_alnum)?;
    let line = &lines[li];
    let orientation = orientation_of(line);

    let mut ordered: Vec<&OcrWord> = line.words.iter().collect();
    match orientation {
        Orientation::Horizontal => ordered.sort_by_key(|w| w.rect.x),
        Orientation::Vertical => ordered.sort_by_key(|w| w.rect.y),
    }

    let hit = &line.words[wi];
    let hit_pos = ordered.iter().position(|w| std::ptr::eq(*w, hit))?;
    let tail_words: Vec<OcrWord> = ordered[hit_pos..].iter().map(|w| (**w).clone()).collect();

    let (kept, next) = split_at_clipped(&tail_words, region, orientation);
    let mut text = String::new();
    for w in &kept {
        text.push_str(&w.text);
    }
    Some((text, next, orientation))
}

/// Resolve the text under the hover.
pub fn resolve(lines: &[OcrLine], cursor: PhysPoint, scan_alnum: bool) -> Option<Resolved> {
    let (li, wi) = hit_scan(lines, cursor, scan_alnum)?;
    let line = &lines[li];
    let orientation = orientation_of(line);

    // OCR does not promise an order.
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

    // Append the wrapped tail when one exists.
    if let Some(next_line) = find_continuation(lines, li, orientation) {
        let mut tail: Vec<&OcrWord> = next_line.words.iter().collect();
        match orientation {
            Orientation::Horizontal => tail.sort_by_key(|w| w.rect.x),
            Orientation::Vertical => tail.sort_by_key(|w| w.rect.y),
        }
        for w in &tail {
            text.push_str(&w.text);
        }
        ordered.extend(tail);
    }

    // Store geometry for each OCR word, not each text span.
    let geom: Vec<TextGeom> = ordered
        .iter()
        .map(|w| TextGeom {
            char_count: w.text.chars().count(),
            rect: w.rect,
        })
        .collect();

    // '-' uses 1 byte, and 'ー' uses 3 bytes.
    let prefix_len = normalise(&text[..cursor_byte_offset]).len();
    let text = normalise(&text);

    Some(Resolved {
        span: TextSpan {
            text,
            cursor_byte_offset: prefix_len,
            anchor: hit.rect,
            geom,
        },
        orientation,
    })
}

/// `CaptureSize` stores the capture box size in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CaptureSize {
    pub w: i32,
    pub h: i32,
}

impl Default for CaptureSize {
    fn default() -> Self {
        CaptureSize { w: REGION_W, h: REGION_H }
    }
}

impl CaptureSize {
    /// Return the extent on the perpendicular axis.
    pub fn short(self) -> i32 {
        self.w.min(self.h)
    }
}

/// Return whether text contains kana or a CJK ideograph.
pub fn has_scannable_script(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{3040}'..='\u{309F}'   // hiragana
            | '\u{30A0}'..='\u{30FF}' // katakana
            | '\u{FF66}'..='\u{FF9D}' // halfwidth katakana
            | '\u{4E00}'..='\u{9FFF}' // CJK
            | '\u{3400}'..='\u{4DBF}' // CJK ext A
            | '\u{3005}'              // 々
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x: i32, y: i32, ww: i32, h: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y, w: ww, h } }
    }
    fn p(x: i32, y: i32) -> PhysPoint { PhysPoint { x, y } }

    /// This fixture contains three 20x20 characters in one row.
    fn horizontal_line() -> Vec<OcrLine> {
        vec![OcrLine {
            words: vec![
                w("昨", 100, 100, 20, 20),
                w("日", 130, 100, 20, 20),
                w("は", 160, 100, 20, 20),
            ],
        }]
    }

    /// The function joins adjacent words without spaces and keeps each line break.
    /// OCR word splits do not add spaces, but line breaks appear on the screen.
    #[test]
    fn recognised_lines_join_without_spaces_and_keep_one_break_each() {
        let lines = vec![
            OcrLine { words: vec![w("これは", 0, 0, 1, 1), w("テスト", 0, 0, 1, 1)] },
            OcrLine { words: vec![w("二行目", 0, 0, 1, 1)] },
        ];
        assert_eq!("これはテスト\n二行目", join_lines(&lines));
    }

    /// No recognized lines produce an empty string, not a stray newline.
    /// The caller uses emptiness to decide whether it has text to copy.
    #[test]
    fn nothing_recognised_joins_to_the_empty_string() {
        assert_eq!("", join_lines(&[]));
        assert_eq!("", join_lines(&[OcrLine { words: Vec::new() }]));
    }

    /// The OCR engine can return a wordless line.
    /// The function still keeps its line break after the line above.
    #[test]
    fn a_wordless_line_between_two_others_keeps_both_breaks() {
        let lines = vec![
            OcrLine { words: vec![w("上", 0, 0, 1, 1)] },
            OcrLine { words: Vec::new() },
            OcrLine { words: vec![w("下", 0, 0, 1, 1)] },
        ];
        assert_eq!("上\n\n下", join_lines(&lines));
    }

    #[test]
    fn direct_containment_wins() {
        assert_eq!(Some((0, 1)), hit_scan(&horizontal_line(), p(135, 105), true));
    }

    #[test]
    fn near_miss_within_half_a_character_height_is_accepted() {
        // Half of 20 equals 10.
        assert_eq!(Some((0, 0)), hit_scan(&horizontal_line(), p(105, 95), true));
    }

    #[test]
    fn beyond_tolerance_returns_none() {
        // This point lies far beyond half a word height.
        assert_eq!(None, hit_scan(&horizontal_line(), p(105, 60), true));
    }

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(None, hit_scan(&[], p(105, 105), true));
        assert_eq!(None, hit_scan(&[OcrLine { words: vec![] }], p(105, 105), true));
    }

    #[test]
    fn ties_break_by_document_order() {
        // Exact geometry creates the tie.
        let lines = vec![OcrLine {
            words: vec![
                w("昨", 100, 100, 20, 20), // occupies x 100..=119
                w("日", 131, 100, 20, 20), // occupies x 131..=150
            ],
        }];
        assert_eq!(6.0, lines[0].words[0].rect.edge_distance_to(p(125, 105)));
        assert_eq!(6.0, lines[0].words[1].rect.edge_distance_to(p(125, 105)));
        // The lower index wins.
        for _ in 0..20 {
            assert_eq!(Some((0, 0)), hit_scan(&lines, p(125, 105), true));
        }
    }

    #[test]
    fn scans_across_multiple_lines() {
        let lines = vec![
            OcrLine { words: vec![w("上", 100, 100, 20, 20)] },
            OcrLine { words: vec![w("下", 100, 200, 20, 20)] },
        ];
        assert_eq!(Some((1, 0)), hit_scan(&lines, p(105, 205), true));
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
        let got = resolve(&[line], p(105, 105), true).unwrap();
        assert_eq!("昨日は", got.span.text);
        assert_eq!(0, got.span.cursor_byte_offset);
    }

    #[test]
    fn assembly_orders_vertically_top_to_bottom() {
        let got = resolve(&vertical_line(), p(105, 165), true).unwrap();
        assert_eq!("昨日は", got.span.text);
        assert_eq!(Orientation::Vertical, got.orientation);
        // 昨 and 日 use 3 bytes each.
        assert_eq!(6, got.span.cursor_byte_offset);
    }

    #[test]
    fn cursor_byte_offset_lands_on_a_char_boundary() {
        let got = resolve(&horizontal_line(), p(165, 105), true).unwrap();
        // The offset must never split a character.
        assert!(got.span.text[got.span.cursor_byte_offset..].starts_with('は'));
    }

    #[test]
    fn anchor_is_the_hit_characters_own_rect() {
        let got = resolve(&horizontal_line(), p(135, 105), true).unwrap();
        assert_eq!(PhysRect { x: 130, y: 100, w: 20, h: 20 }, got.span.anchor);
    }

    #[test]
    fn resolve_returns_none_when_nothing_is_near() {
        assert_eq!(None, resolve(&horizontal_line(), p(500, 500), true).map(|r| r.span.text));
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
        // A hyphen after kanji stays a dash.
        assert_eq!("日-本", normalise("日-本"));
    }

    #[test]
    fn coordinates_survive_a_negative_origin() {
        // The monitor sits left of the primary monitor.
        let got = map_from_upscaled(
            PhysRect { x: 40, y: 20, w: 40, h: 40 },
            PhysPoint { x: -1920, y: -100 },
            2,
        );
        assert_eq!(PhysRect { x: -1900, y: -90, w: 20, h: 20 }, got);
        // Hit-scan still resolves the word.
        let line = OcrLine {
            words: vec![OcrWord { text: "食".to_string(), rect: got }],
        };
        assert_eq!(Some((0, 0)), hit_scan(&[line], p(-1895, -85), true));
    }

    #[test]
    fn map_from_upscaled_divides_then_translates() {
        // The region origin is (1000,500).
        let got = map_from_upscaled(
            PhysRect { x: 200, y: 100, w: 40, h: 40 },
            PhysPoint { x: 1000, y: 500 },
            2,
        );
        assert_eq!(PhysRect { x: 1100, y: 550, w: 20, h: 20 }, got);
    }

    /// The input has a hyphen before the hovered word.
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
        let got = resolve(&[line], p(195, 105), true).unwrap();
        assert_eq!("ツール箱", got.span.text);
        assert!(got.span.text[got.span.cursor_byte_offset..].starts_with('箱'));
        // Three characters use 3 bytes each, for 9 bytes.
        assert_eq!(9, got.span.cursor_byte_offset);
    }

    /// Use `ptr::eq`, not text equality.
    #[test]
    fn repeated_character_resolves_to_the_hit_occurrence() {
        let line = OcrLine {
            words: vec![
                w("々", 100, 100, 20, 20),
                w("人", 130, 100, 20, 20),
                w("々", 160, 100, 20, 20), // The hit is the second "々", the third word.
                w("々", 190, 100, 20, 20), // This final repeat follows the hit repeat.
            ],
        };
        let got = resolve(&[line], p(170, 110), true).unwrap();
        assert_eq!("々人々々", got.span.text);
        // 2 chars x 3 bytes = 6.
        assert_eq!(6, got.span.cursor_byte_offset);
        assert_eq!("々々", &got.span.text[got.span.cursor_byte_offset..]);
    }

    /// The region must center on the cursor.
    #[test]
    fn the_region_is_centred_on_the_cursor() {
        let r = region_around(p(1000, 500), false, CaptureSize::default());
        assert_eq!(REGION_W, r.w);
        assert_eq!(REGION_H, r.h);
        assert_eq!(1000 - REGION_W / 2, r.x);
        assert_eq!(500 - REGION_H / 2, r.y);
    }

    #[test]
    fn region_around_horizontal_is_wider_than_tall() {
        let r = region_around(p(500, 500), false, CaptureSize::default());
        assert!(r.w > r.h);
    }

    #[test]
    fn region_around_vertical_is_taller_than_wide() {
        let r = region_around(p(500, 500), true, CaptureSize::default());
        assert!(r.h > r.w);
    }

    #[test]
    fn region_around_uses_the_configured_size() {
        let size = CaptureSize { w: 800, h: 120 };
        let r = region_around(p(1000, 500), false, size);
        assert_eq!(800, r.w);
        assert_eq!(120, r.h);
        assert_eq!(1000 - 400, r.x);
        assert_eq!(500 - 60, r.y);
    }

    #[test]
    fn prefer_vertical_transposes_the_configured_size() {
        let size = CaptureSize { w: 800, h: 120 };
        let r = region_around(p(1000, 500), true, size);
        assert_eq!(120, r.w, "vertical swaps the two numbers");
        assert_eq!(800, r.h);
    }

    /// The capture floor must exceed the slack that `hit_scan` accepts.
    #[test]
    fn the_capture_floor_covers_hit_scan_slack() {
        let typical_word_h = 40;
        let needed = typical_word_h / 2 + typical_word_h / 2;
        assert!(
            crate::config::CAPTURE_H_RANGE.0 / 2 >= needed,
            "CAPTURE_H_RANGE.0/2 = {} must cover {needed}px",
            crate::config::CAPTURE_H_RANGE.0 / 2
        );
    }

    #[test]
    fn band_expands_perpendicular_and_keeps_the_word_centred() {
        let word = PhysRect { x: 1499, y: 870, w: 35, h: 34 };
        let b = band_of(word, Orientation::Horizontal, REGION_H);
        assert_eq!(102, b.h, "max(3.0x the word height, REGION_H)");
        assert_eq!(word.center().y, b.center().y, "same centre line");
        assert_eq!(word.x, b.x, "parallel axis untouched");
    }

    #[test]
    fn band_swaps_axes_for_vertical_text() {
        let word = PhysRect { x: 1438, y: 344, w: 64, h: 70 };
        let b = band_of(word, Orientation::Vertical, REGION_H);
        assert_eq!(192, b.w, "max(3.0x the word width, REGION_H)");
        assert_eq!(word.center().x, b.center().x);
        assert_eq!(word.y, b.y, "parallel axis untouched");
    }

    /// A 44px band cannot read the text.
    #[test]
    fn band_floors_small_text_at_region_h() {
        let word = PhysRect { x: 1499, y: 870, w: 20, h: 22 };
        let b = band_of(word, Orientation::Horizontal, REGION_H);
        assert_eq!(REGION_H, b.h, "3.0x22=66 must be floored to REGION_H");
        assert_eq!(word.center().y, b.center().y, "same centre line");
    }

    #[test]
    fn band_factor_still_applies_above_the_floor() {
        let word = PhysRect { x: 1499, y: 870, w: 20, h: 60 };
        let b = band_of(word, Orientation::Horizontal, REGION_H);
        assert_eq!(180, b.h, "3.0x60=180 must not be clamped down to REGION_H");
        assert_eq!(word.center().y, b.center().y, "same centre line");
    }

    #[test]
    fn band_of_floors_on_the_configured_short_axis() {
        let word = PhysRect { x: 1499, y: 870, w: 35, h: 34 };
        let b = band_of(word, Orientation::Horizontal, 200);
        assert_eq!(200, b.h, "floored by the configured short axis");
    }

    #[test]
    fn tile_runs_along_the_reading_direction() {
        let band = PhysRect { x: 100, y: 200, w: 40, h: 80 };
        let h = tile_after(band, 500, Orientation::Horizontal, TILE_LEN);
        assert_eq!(PhysRect { x: 500, y: 200, w: TILE_LEN, h: 80 }, h);

        let v = tile_after(band, 500, Orientation::Vertical, TILE_LEN);
        assert_eq!(PhysRect { x: 100, y: 500, w: 40, h: TILE_LEN }, v);
    }

    /// Measured furigana forms a separate line.
    #[test]
    fn nearest_line_picks_the_real_line_over_furigana() {
        let furigana = OcrLine {
            words: (0..3).map(|i| w("furi", 3320 + i * 13, 1345, 13, 11)).collect(),
        };
        let real_line = OcrLine {
            words: (0..18).map(|i| w("real", 2873 + i * 22, 1362, 22, 22)).collect(),
        };
        let lines = vec![furigana, real_line.clone()];
        assert_eq!(Some(&real_line), nearest_line(&lines, 1372, Orientation::Horizontal, 11));
    }

    /// The same rule applies after the axes rotate.
    #[test]
    fn nearest_line_mirrors_axes_for_vertical_text() {
        let furigana = OcrLine {
            words: (0..3).map(|i| w("furi", 1345, 3320 + i * 13, 11, 13)).collect(),
        };
        let real_line = OcrLine {
            words: (0..18).map(|i| w("real", 1362, 2873 + i * 22, 22, 22)).collect(),
        };
        let lines = vec![furigana, real_line.clone()];
        assert_eq!(Some(&real_line), nearest_line(&lines, 1372, Orientation::Vertical, 11));
    }

    #[test]
    fn nearest_line_empty_input_returns_none() {
        assert_eq!(None, nearest_line(&[], 1372, Orientation::Horizontal, 20));
    }

    #[test]
    fn nearest_line_skips_a_line_with_no_words() {
        let empty = OcrLine { words: vec![] };
        let real_line = OcrLine { words: vec![w("real", 100, 1372, 20, 20)] };
        let lines = vec![empty, real_line.clone()];
        assert_eq!(Some(&real_line), nearest_line(&lines, 1372, Orientation::Horizontal, 15));
    }

    /// A tie places both lines 20px from the target.
    #[test]
    fn nearest_line_ties_break_by_document_order() {
        let first_line = OcrLine { words: vec![w("a", 0, 100, 20, 20)] };
        let second_line = OcrLine { words: vec![w("b", 0, 140, 20, 20)] };
        let lines = vec![first_line.clone(), second_line];
        assert_eq!(Some(&first_line), nearest_line(&lines, 130, Orientation::Horizontal, 20));
    }

    /// I3: The line lies inside the tolerance.
    #[test]
    fn nearest_line_within_the_bound_is_selected() {
        let line = OcrLine { words: vec![w("a", 100, 1360, 20, 20)] };
        let lines = vec![line.clone()];
        assert_eq!(Some(&line), nearest_line(&lines, 1372, Orientation::Horizontal, 10));
    }

    /// I3: The line lies outside the tolerance.
    #[test]
    fn nearest_line_beyond_the_bound_returns_none() {
        let far_line = OcrLine { words: vec![w("a", 100, 1300, 20, 20)] };
        assert_eq!(None, nearest_line(&[far_line], 1372, Orientation::Horizontal, 10));
    }

    fn hword(text: &str, x: i32, width: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y: 100, w: width, h: 40 } }
    }

    /// This helper returns a large bound, so no clamp applies.
    fn unbounded() -> PhysRect {
        PhysRect { x: -1_000_000, y: -1_000_000, w: 3_000_000, h: 3_000_000 }
    }

    #[test]
    fn clamp_tile_is_unchanged_when_fully_inside_bounds() {
        let tile = PhysRect { x: 100, y: 100, w: 50, h: 50 };
        let bounds = PhysRect { x: 0, y: 0, w: 1000, h: 1000 };
        assert_eq!(Some(tile), clamp_tile(tile, bounds));
    }

    /// A wide, short tile tests horizontal text.
    #[test]
    fn clamp_tile_shrinks_a_horizontal_tile_past_the_right_edge() {
        let tile = PhysRect { x: 2300, y: 500, w: 500, h: 80 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1080 };
        assert_eq!(Some(PhysRect { x: 2300, y: 500, w: 260, h: 80 }), clamp_tile(tile, bounds));
    }

    /// A tall, narrow tile tests vertical text.
    #[test]
    fn clamp_tile_shrinks_a_vertical_tile_past_the_bottom_edge() {
        let tile = PhysRect { x: 500, y: 900, w: 80, h: 500 };
        let bounds = PhysRect { x: 0, y: 0, w: 1080, h: 1200 };
        assert_eq!(Some(PhysRect { x: 500, y: 900, w: 80, h: 300 }), clamp_tile(tile, bounds));
    }

    #[test]
    fn clamp_tile_returns_none_when_the_tile_starts_beyond_bounds() {
        let tile = PhysRect { x: 3000, y: 0, w: 500, h: 80 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1080 };
        assert_eq!(None, clamp_tile(tile, bounds));
    }

    #[test]
    fn clamp_tile_returns_none_when_clamped_to_exactly_zero_extent() {
        let tile = PhysRect { x: 2560, y: 0, w: 500, h: 80 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1080 };
        assert_eq!(None, clamp_tile(tile, bounds));
    }

    #[test]
    fn clamp_tile_also_clamps_the_perpendicular_axis() {
        let tile = PhysRect { x: 100, y: -20, w: 500, h: 80 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1080 };
        assert_eq!(Some(PhysRect { x: 100, y: 0, w: 500, h: 60 }), clamp_tile(tile, bounds));
    }

    #[test]
    fn a_word_touching_the_trailing_edge_is_dropped_and_becomes_the_next_start() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 10, 40), hword("い", 60, 40), hword("う", 470, 40)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(vec!["あ", "い"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
        assert_eq!(470, next, "the clipped word's leading edge starts the next tile");
    }

    #[test]
    fn with_nothing_clipped_the_next_start_is_the_tiles_own_edge() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 10, 40), hword("い", 60, 40)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(2, kept.len());
        assert_eq!(500, next);
    }

    /// Equal edge distance does not count as clipped.
    #[test]
    fn trailing_edge_exactly_at_the_margin_boundary_is_kept() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 456, 40)]; // trail = 456+40 = 496
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(1, kept.len(), "496 must not be treated as clipped");
        assert_eq!(500, next);
    }

    /// One pixel farther reaches 497.
    #[test]
    fn trailing_edge_one_pixel_past_the_margin_boundary_is_discarded() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 457, 40)]; // trail = 457+40 = 497
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(457, next, "must return the clipped word's own leading edge");
    }

    /// D3: The first word is too wide to advance.
    #[test]
    fn a_first_word_already_clipped_keeps_nothing_and_does_not_advance() {
        let tile = PhysRect { x: 400, y: 90, w: 100, h: 60 };
        let words = [hword("あ", 400, 120)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(400, next, "must not advance past the tile's own start");
    }

    /// Return the word edge, not the tile start.
    #[test]
    fn a_clipped_first_word_starting_mid_tile_returns_its_own_leading_edge() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 200, 400)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(200, next);
        assert!(next > tile.x, "must be the word's edge, not the tile's start");
    }

    #[test]
    fn the_split_reads_top_to_bottom_for_vertical_text() {
        let tile = PhysRect { x: 90, y: 0, w: 60, h: 500 };
        let words = [
            OcrWord { text: "上".into(), rect: PhysRect { x: 100, y: 10, w: 40, h: 40 } },
            OcrWord { text: "下".into(), rect: PhysRect { x: 100, y: 470, w: 40, h: 40 } },
        ];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Vertical);
        assert_eq!(vec!["上"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
        assert_eq!(470, next);
    }

    /// OCR does not promise word order.
    #[test]
    fn words_arriving_out_of_order_are_sorted_before_splitting() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("い", 60, 40), hword("あ", 10, 40)];
        let (kept, _) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(vec!["あ", "い"], kept.iter().map(|k| k.text.as_str()).collect::<Vec<_>>());
    }

    // -- drop_leading --

    #[test]
    fn drop_leading_keeps_words_at_or_after_start() {
        let words = [hword("あ", 100, 40), hword("い", 150, 40)];
        let kept = drop_leading(&words, 100, Orientation::Horizontal);
        assert_eq!(2, kept.len());
    }

    /// This boundary keeps a deferred glyph.
    #[test]
    fn drop_leading_keeps_a_word_exactly_at_the_margin_boundary() {
        let words = [hword("あ", 96, 40)]; // start=100, margin=4 -> 96
        let kept = drop_leading(&words, 100, Orientation::Horizontal);
        assert_eq!(1, kept.len(), "96 must not be treated as spillover");
    }

    /// One pixel farther back reaches 95.
    #[test]
    fn drop_leading_discards_a_word_one_pixel_past_the_margin_boundary() {
        let words = [hword("あ", 95, 40)];
        let kept = drop_leading(&words, 100, Orientation::Horizontal);
        assert!(kept.is_empty());
    }

    #[test]
    fn drop_leading_reads_the_perpendicular_axis_for_vertical_text() {
        let words = [OcrWord { text: "上".into(), rect: PhysRect { x: 100, y: 96, w: 40, h: 40 } }];
        let kept = drop_leading(&words, 100, Orientation::Vertical);
        assert_eq!(1, kept.len());
    }

    #[test]
    fn drop_leading_empty_input_returns_empty() {
        assert!(drop_leading(&[], 100, Orientation::Horizontal).is_empty());
    }

    #[test]
    fn tiling_concatenates_successive_tiles() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, unbounded(), |tile| {
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
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, unbounded(), |tile| {
            seen.push(tile.x);
            let label = (tile.x / TILE_LEN).to_string();
            vec![hword(&label, tile.x + 10, 40)]
        });
        assert_eq!(vec![0, TILE_LEN, TILE_LEN * 2], seen, "each tile starts where the last ended");
        assert_eq!("012", text, "a loop that never advances would repeat one region");
    }

    /// D5: Restart at 470.
    #[test]
    fn tile_forward_restarts_from_the_clipped_words_own_leading_edge() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut seen_starts = Vec::new();
        let mut call = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 2, 25, unbounded(), |tile| {
            seen_starts.push(tile.x);
            call += 1;
            if call == 1 {
                vec![hword("あ", 470, 40)]
            } else {
                vec![hword("い", tile.x + 10, 40)]
            }
        });
        assert_eq!(vec![0, 470], seen_starts, "tile 2 must open at the clipped word's own edge");
        assert_eq!("い", text, "tile 1's clipped word contributes nothing");
    }

    #[test]
    fn tiling_stops_once_max_chars_is_reached() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        tile_forward(band, 0, Orientation::Horizontal, 5, 2, unbounded(), |tile| {
            calls += 1;
            vec![hword("あ", tile.x + 10, 40), hword("い", tile.x + 60, 40)]
        });
        assert_eq!(1, calls, "two characters already meet the budget");
    }

    #[test]
    fn tiling_stops_when_a_tile_recognises_nothing() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, unbounded(), |_| {
            calls += 1;
            Vec::new()
        });
        assert_eq!(1, calls);
        assert_eq!("", text);
    }

    /// D5: Stop when the next start cannot advance.
    #[test]
    fn tiling_stops_when_the_next_start_does_not_advance() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut calls = 0;
        tile_forward(band, 0, Orientation::Horizontal, 100, 25, unbounded(), |tile| {
            calls += 1;
            vec![hword("あ", tile.x, tile.w)]
        });
        assert_eq!(1, calls, "a start that cannot advance must stop the loop");
    }

    #[test]
    fn tiling_normalises_across_a_seam() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let mut first = true;
        let text = tile_forward(band, 0, Orientation::Horizontal, 2, 25, unbounded(), |tile| {
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

    /// I2: Tile 2 clamps to 20px.
    #[test]
    fn tiling_clamps_a_tile_that_would_cross_a_monitor_edge() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let bounds = PhysRect { x: 0, y: 0, w: 520, h: 1000 };
        let mut widths = Vec::new();
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, bounds, |tile| {
            widths.push(tile.w);
            vec![hword("あ", tile.x, 10)]
        });
        assert_eq!(vec![TILE_LEN, 20], widths, "tile 2 is clamped to what the monitor has left");
        assert_eq!("ああ", text, "the clamped tile is still read, not skipped");
    }

    /// I2: A zero-extent tile requires no read.
    #[test]
    fn tiling_stops_without_reading_when_the_first_tile_clamps_to_nothing() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let bounds = PhysRect { x: -500, y: 0, w: 500, h: 1000 };
        let mut calls = 0;
        let text = tile_forward(band, 0, Orientation::Horizontal, 3, 25, bounds, |_| {
            calls += 1;
            Vec::new()
        });
        assert_eq!(0, calls, "a tile clamped to zero extent must never be read");
        assert_eq!("", text);
    }

    /// I2: Clamp a vertical tile that extends downward.
    #[test]
    fn tiling_clamps_a_tile_that_would_cross_a_monitor_edge_for_vertical_text() {
        let band = PhysRect { x: 90, y: 0, w: 60, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 1000, h: 520 };
        let mut heights = Vec::new();
        let text = tile_forward(band, 0, Orientation::Vertical, 3, 25, bounds, |tile| {
            heights.push(tile.h);
            vec![OcrWord { text: "上".into(), rect: PhysRect { x: tile.x, y: tile.y, w: tile.w, h: 10 } }]
        });
        assert_eq!(vec![TILE_LEN, 20], heights, "tile 2 is clamped to what the monitor has left");
        assert_eq!("上上", text);
    }

    /// A tile must not prepend words from the previous tile.
    #[test]
    fn tile_forward_drops_a_tiles_own_leading_spillover() {
        let band = PhysRect { x: 0, y: 90, w: 40, h: 60 };
        let text = tile_forward(band, 500, Orientation::Horizontal, 1, 25, unbounded(), |tile| {
            vec![hword("よ", tile.x - 20, 40), hword("あ", tile.x + 10, 40)]
        });
        assert_eq!("あ", text, "よ starts before the tile and must be dropped");
    }

    fn g(n: usize, x: i32, w: i32) -> TextGeom {
        TextGeom { char_count: n, rect: PhysRect { x, y: 100, w, h: 40 } }
    }

    #[test]
    fn union_covers_exactly_the_matched_run_padded() {
        let geom = vec![g(1, 100, 30), g(1, 130, 30), g(1, 160, 30)];
        let r = union_chars(&geom, 0, 2, 3).unwrap();
        assert_eq!(PhysRect { x: 97, y: 97, w: 66, h: 46 }, r);
    }

    #[test]
    fn union_starts_at_the_requested_offset_not_the_beginning() {
        let geom = vec![g(1, 100, 30), g(1, 130, 30), g(1, 160, 30)];
        let r = union_chars(&geom, 2, 1, 0).unwrap();
        assert_eq!(PhysRect { x: 160, y: 100, w: 30, h: 40 }, r);
    }

    /// The OCR returned "338" in one box.
    #[test]
    fn a_match_ending_inside_a_multi_char_entry_rounds_outward() {
        let geom = vec![g(1, 100, 30), g(3, 130, 90)];
        let r = union_chars(&geom, 0, 2, 0).unwrap();
        assert_eq!(PhysRect { x: 100, y: 100, w: 120, h: 40 }, r);
    }

    #[test]
    fn union_of_nothing_is_none() {
        assert!(union_chars(&[], 0, 3, 0).is_none());
        assert!(union_chars(&[g(1, 100, 30)], 5, 1, 0).is_none());
    }

    /// Saturation must prevent wraparound.
    #[test]
    fn an_absurd_match_length_covers_the_rest_rather_than_wrapping() {
        let geom = vec![g(1, 100, 30), g(1, 130, 30), g(1, 160, 30)];
        let r = union_chars(&geom, 1, usize::MAX, 0).unwrap();
        assert_eq!(PhysRect { x: 130, y: 100, w: 60, h: 40 }, r);
    }

    /// The character count must stay unchanged.
    #[test]
    fn normalise_preserves_character_count() {
        for s in ["ツ-ル", "ツ‐ル", "ツ–ル", "ツ—ル", "abc", "日本語", ""] {
            assert_eq!(s.chars().count(), normalise(s).chars().count(), "input {s:?}");
        }
    }

    #[test]
    fn resolve_carries_one_geom_entry_per_word_aligned_to_the_text() {
        let line = OcrLine {
            words: vec![w("可", 100, 100, 30, 40), w("哀", 130, 100, 30, 40),
                        w("想", 160, 100, 30, 40)],
        };
        let r = resolve(&[line], p(145, 120), true).unwrap();
        assert_eq!(3, r.span.geom.len());
        assert_eq!(r.span.text.chars().count(),
                   r.span.geom.iter().map(|g| g.char_count).sum::<usize>());
    }

    /// Start the highlight at the hover, not at the line start.
    #[test]
    fn the_highlight_boxes_the_hovered_word_not_the_lines_start() {
        let line = OcrLine {
            words: vec![w("そ", 100, 100, 30, 40), w("の", 130, 100, 30, 40),
                        w("可", 160, 100, 30, 40), w("哀", 190, 100, 30, 40),
                        w("想", 220, 100, 30, 40)],
        };
        let r = resolve(&[line], p(175, 120), true).unwrap();
        assert_ne!(0, r.span.cursor_byte_offset, "the fixture must exercise a non-zero offset");

        let from = r.span.text[..r.span.cursor_byte_offset].chars().count();
        let boxed = union_chars(&r.span.geom, from, 3, 0).unwrap();
        assert_eq!(PhysRect { x: 160, y: 100, w: 90, h: 40 }, boxed);
    }

    /// Absent geometry must produce no highlight, not a wrong highlight.
    #[test]
    fn no_geometry_yields_no_highlight_rather_than_a_wrong_one() {
        assert_eq!(None, union_chars(&[], 0, 3, 3));
    }

    /// The runtime uses a 26px pitch.
    #[test]
    fn a_spaced_run_boxes_only_the_matched_chars() {
        let line = OcrLine {
            words: vec![w("学", 100, 100, 23, 24), w("生", 126, 100, 23, 24),
                        w("は", 152, 100, 23, 24), w("宿", 178, 100, 23, 24),
                        w("舎", 204, 100, 23, 24)],
        };
        let r = resolve(&[line], p(185, 112), true).unwrap();
        assert_eq!(5, r.span.geom.len(), "gaps must not fold the run into one entry");

        let from = r.span.text[..r.span.cursor_byte_offset].chars().count();
        let boxed = union_chars(&r.span.geom, from, 2, 3).unwrap();
        assert_eq!(PhysRect { x: 175, y: 97, w: 55, h: 30 }, boxed);
    }

    /// Cover gaps inside a matched text span.
    #[test]
    fn a_spaced_match_is_continuous_across_its_gaps() {
        let line = OcrLine {
            words: vec![w("学", 100, 100, 23, 24), w("生", 126, 100, 23, 24),
                        w("は", 152, 100, 23, 24)],
        };
        let r = resolve(&[line], p(110, 112), true).unwrap();
        let boxed = union_chars(&r.span.geom, 0, 3, 0).unwrap();
        assert_eq!(PhysRect { x: 100, y: 100, w: 75, h: 24 }, boxed);
    }

    // -- resolve with spaced text --

    /// Keep geometry per character when words have spaces.
    #[test]
    fn resolve_keeps_spaced_geom_per_char() {
        let line = OcrLine {
            words: vec![
                w("食", 100, 100, 20, 20),
                w("べ", 150, 100, 20, 20),
                w("物", 200, 100, 20, 20),
            ],
        };
        let got = resolve(&[line], p(155, 105), true).unwrap();
        assert_eq!("食べ物", got.span.text);
        assert_eq!(3, got.span.geom.len());
        assert!(got.span.geom.iter().all(|g| g.char_count == 1));

        // The whole match covers its internal gaps.
        assert_eq!(
            PhysRect { x: 100, y: 100, w: 120, h: 20 },
            union_chars(&got.span.geom, 0, 3, 0).unwrap()
        );
        // One character covers only its own box.
        assert_eq!(
            PhysRect { x: 150, y: 100, w: 20, h: 20 },
            union_chars(&got.span.geom, 1, 1, 0).unwrap()
        );
    }

    /// The cursor offset stays correct after the merge.
    #[test]
    fn resolve_spaced_cursor_offset_correct() {
        let line = OcrLine {
            words: vec![
                w("食", 100, 100, 20, 20),
                w("べ", 150, 100, 20, 20),
                w("物", 200, 100, 20, 20),
            ],
        };
        let got = resolve(&[line], p(205, 105), true).unwrap();
        assert_eq!(6, got.span.cursor_byte_offset);
        assert!(got.span.text[6..].starts_with('物'));
    }

    /// Keep vertical geometry per character when words have spaces.
    #[test]
    fn resolve_keeps_spaced_vertical_geom_per_char() {
        let line = OcrLine {
            words: vec![
                w("食", 100, 100, 20, 20),
                w("べ", 100, 150, 20, 20),
                w("物", 100, 200, 20, 20),
            ],
        };
        let got = resolve(&[line], p(105, 155), true).unwrap();
        assert_eq!(Orientation::Vertical, got.orientation);
        assert_eq!(3, got.span.geom.len());
        assert_eq!(
            PhysRect { x: 100, y: 150, w: 20, h: 20 },
            union_chars(&got.span.geom, 1, 1, 0).unwrap()
        );
    }

    /// Keep geometry per character when words touch.
    #[test]
    fn resolve_touching_chars_keep_per_char_geom() {
        let line = OcrLine {
            words: vec![
                w("可", 100, 100, 30, 40),
                w("哀", 130, 100, 30, 40),
                w("想", 160, 100, 30, 40),
            ],
        };
        let got = resolve(&[line], p(145, 120), true).unwrap();
        assert_eq!(3, got.span.geom.len());
    }

    // -- line wrap continuation --

    #[test]
    fn wrap_within_one_line_height_continues() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 100, 156, 40, 40)] };
        assert!(continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    /// Accept a gap equal to 1.5 times the line thickness.
    #[test]
    fn wrap_gap_exactly_at_the_ceiling_is_accepted() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 100, 160, 40, 40)] };
        assert!(continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    /// Reject a gap one pixel above the ceiling at 61px.
    #[test]
    fn wrap_gap_one_past_the_ceiling_is_rejected() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 100, 161, 40, 40)] };
        assert!(!continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    #[test]
    fn wrap_does_not_look_backward_to_the_previous_line() {
        let upper = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let lower = OcrLine { words: vec![w("b", 100, 156, 40, 40)] };
        assert!(!continues_on_next_line(&lower, &upper, Orientation::Horizontal));
    }

    /// Allow half a line of slack at the start.
    #[test]
    fn wrap_start_within_tolerance_of_the_margin_continues() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 120, 156, 40, 40)] };
        assert!(continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    /// Reject a start one pixel beyond the tolerance at 121.
    #[test]
    fn wrap_start_one_past_the_margin_tolerance_is_rejected() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 121, 156, 40, 40)] };
        assert!(!continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    #[test]
    fn wrap_far_from_the_margin_reads_as_a_column_not_a_wrap() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 300, 156, 40, 40)] };
        assert!(!continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    #[test]
    fn vertical_wrap_continues_into_the_column_on_the_left() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 44, 100, 40, 40)] };
        assert!(continues_on_next_line(&current, &next, Orientation::Vertical));
    }

    #[test]
    fn vertical_wrap_does_not_look_to_the_column_on_the_right() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![w("b", 156, 100, 40, 40)] };
        assert!(!continues_on_next_line(&current, &next, Orientation::Vertical));
    }

    #[test]
    fn wrap_with_an_empty_current_line_is_false() {
        let current = OcrLine { words: vec![] };
        let next = OcrLine { words: vec![w("b", 100, 156, 40, 40)] };
        assert!(!continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    #[test]
    fn wrap_with_an_empty_next_line_is_false() {
        let current = OcrLine { words: vec![w("a", 100, 100, 40, 40)] };
        let next = OcrLine { words: vec![] };
        assert!(!continues_on_next_line(&current, &next, Orientation::Horizontal));
    }

    // -- resolve across a wrap --

    /// This fixture reproduces the reported wrap bug.
    fn wrapped_lines() -> Vec<OcrLine> {
        vec![
            OcrLine {
                words: vec![
                    w("新", 100, 100, 40, 40),
                    w("し", 140, 100, 40, 40),
                    w("い", 180, 100, 40, 40),
                    w("冒", 220, 100, 40, 40),
                    w("険", 260, 100, 40, 40),
                    w("が", 300, 100, 40, 40),
                ],
            },
            OcrLine {
                words: vec![
                    w("始", 100, 156, 40, 40),
                    w("ま", 140, 156, 40, 40),
                    w("る", 180, 156, 40, 40),
                ],
            },
        ]
    }

    #[test]
    fn resolve_appends_the_wrapped_next_line() {
        let got = resolve(&wrapped_lines(), p(190, 110), true).unwrap();
        assert_eq!("新しい冒険が始まる", got.span.text);
    }

    /// The hover reaches the third character after two earlier characters.
    #[test]
    fn resolve_wrap_keeps_the_hit_words_own_cursor_offset() {
        let got = resolve(&wrapped_lines(), p(190, 110), true).unwrap();
        assert_eq!(6, got.span.cursor_byte_offset);
        assert!(got.span.text[6..].starts_with('い'));
    }

    #[test]
    fn resolve_geom_spans_both_lines_of_a_wrap() {
        let got = resolve(&wrapped_lines(), p(190, 110), true).unwrap();
        assert_eq!(9, got.span.geom.len(), "one entry per touching char");
        let chars: usize = got.span.geom.iter().map(|g| g.char_count).sum();
        assert_eq!(got.span.text.chars().count(), chars);
    }

    #[test]
    fn resolve_does_not_merge_a_distant_second_line() {
        let lines = vec![
            OcrLine { words: vec![w("上", 100, 100, 40, 40)] },
            OcrLine { words: vec![w("下", 100, 400, 40, 40)] },
        ];
        let got = resolve(&lines, p(110, 110), true).unwrap();
        assert_eq!("上", got.span.text);
    }

    #[test]
    fn resolve_only_looks_forward_not_backward_for_a_wrap() {
        let got = resolve(&wrapped_lines(), p(110, 166), true).unwrap();
        assert_eq!("始まる", got.span.text);
    }

    #[test]
    fn resolve_does_not_recursively_merge_a_third_line() {
        let mut lines = wrapped_lines();
        lines.push(OcrLine { words: vec![w("末", 100, 212, 40, 40)] });
        let got = resolve(&lines, p(190, 110), true).unwrap();
        assert_eq!("新しい冒険が始まる", got.span.text);
    }

    /// The nearest valid line must win even when the vector lists a farther line first.
    #[test]
    fn resolve_picks_the_nearest_continuation_not_the_first_in_vec_order() {
        let lines = vec![
            OcrLine { words: vec![w("新", 100, 100, 40, 80)] },
            // Decoy: This line has a gap of 110 and appears before the true wrap.
            OcrLine { words: vec![w("夏", 100, 210, 40, 80)] },
            // True wrap: This line has a gap of 60 and appears second.
            OcrLine { words: vec![w("始", 100, 160, 40, 80)] },
        ];
        let got = resolve(&lines, p(110, 130), true).unwrap();
        assert_eq!(
            "新始", got.span.text,
            "the nearer line must win regardless of Vec order"
        );
    }

    #[test]
    fn resolve_merges_a_vertical_wrap_into_the_next_column() {
        let lines = vec![
            OcrLine {
                words: vec![w("上", 200, 100, 40, 40), w("下", 200, 140, 40, 40)],
            },
            OcrLine {
                words: vec![w("左", 144, 100, 40, 40), w("右", 144, 140, 40, 40)],
            },
        ];
        let got = resolve(&lines, p(210, 110), true).unwrap();
        assert_eq!(Orientation::Vertical, got.orientation);
        assert_eq!("上下左右", got.span.text);
    }

    // -- head_and_tail --

    #[test]
    fn head_and_tail_keeps_the_hit_word_and_what_follows() {
        let line = OcrLine {
            words: vec![
                w("そ", 100, 100, 20, 20),
                w("の", 130, 100, 20, 20),
                w("感", 160, 100, 20, 20),
                w("想", 190, 100, 20, 20),
            ],
        };
        let region = PhysRect { x: 50, y: 80, w: 500, h: 60 };
        let (head, next, ori) = head_and_tail(&[line], p(165, 105), region, true).unwrap();
        assert_eq!("感想", head, "text before the hit word is not head");
        assert_eq!(Orientation::Horizontal, ori);
        assert_eq!(550, next, "region's own trailing edge: 50+500");
    }

    #[test]
    fn head_and_tail_excludes_a_word_clipped_at_the_regions_edge() {
        let line = OcrLine {
            words: vec![
                w("感", 460, 100, 20, 20),
                w("想", 490, 100, 20, 20),
            ],
        };
        let region = PhysRect { x: 0, y: 80, w: 500, h: 60 };
        let (head, next, _) = head_and_tail(&[line], p(465, 105), region, true).unwrap();
        assert_eq!("感", head, "想 is clipped by the region's own edge");
        assert_eq!(490, next, "tiling resumes at the clipped word's own edge");
    }

    #[test]
    fn head_and_tail_empty_head_when_the_hit_word_itself_is_clipped() {
        let line = OcrLine { words: vec![w("想", 490, 100, 20, 20)] };
        let region = PhysRect { x: 0, y: 80, w: 500, h: 60 };
        let (head, next, _) = head_and_tail(&[line], p(495, 105), region, true).unwrap();
        assert_eq!("", head, "even the hit word is clipped by the region edge");
        assert_eq!(490, next, "falls back to the hit word's own leading edge");
    }

    #[test]
    fn head_and_tail_returns_none_when_nothing_is_hit() {
        let line = OcrLine { words: vec![w("食", 100, 100, 20, 20)] };
        let region = PhysRect { x: 0, y: 0, w: 500, h: 100 };
        assert_eq!(None, head_and_tail(&[line], p(900, 900), region, true));
    }

    #[test]
    fn head_and_tail_mirrors_for_vertical_text() {
        let line = OcrLine {
            words: vec![
                w("上", 100, 100, 20, 20),
                w("下", 100, 130, 20, 20),
            ],
        };
        let region = PhysRect { x: 80, y: 50, w: 60, h: 500 };
        let (head, _, ori) = head_and_tail(&[line], p(105, 105), region, true).unwrap();
        assert_eq!("上下", head);
        assert_eq!(Orientation::Vertical, ori);
    }

    /// The flag reaches `hit_scan`.
    #[test]
    fn head_and_tail_skips_latin_words_when_alnum_is_off() {
        let line = OcrLine {
            words: vec![w("Edit", 100, 100, 40, 20), w("語", 150, 100, 20, 20)],
        };
        let region = PhysRect { x: 50, y: 80, w: 500, h: 60 };
        let lines = std::slice::from_ref(&line);
        assert!(head_and_tail(lines, p(110, 105), region, true).is_some());
        assert_eq!(None, head_and_tail(lines, p(110, 105), region, false));
    }

    // -- hit-scan eligibility --

    #[test]
    fn kana_and_kanji_count_as_scannable_script() {
        assert!(has_scannable_script("ひ"));
        assert!(has_scannable_script("カ"));
        assert!(has_scannable_script("漢"));
        assert!(has_scannable_script("々"));
        assert!(has_scannable_script("ｶ"), "halfwidth katakana");
    }

    #[test]
    fn latin_digits_and_fullwidth_latin_are_not_scannable_script() {
        assert!(!has_scannable_script("Edit"));
        assert!(!has_scannable_script("3"));
        assert!(!has_scannable_script("Ａ"), "fullwidth Latin is Latin");
        assert!(!has_scannable_script("１"), "fullwidth digit is a digit");
        assert!(!has_scannable_script("。"), "punctuation alone is not a word");
    }

    #[test]
    fn mixed_words_count_as_scannable_script() {
        assert!(has_scannable_script("3人"));
        assert!(has_scannable_script("Aランク"));
        assert!(has_scannable_script("PCを使う"));
    }

    /// Hit-scan ignores Latin words when alphanumeric scans are off.
    #[test]
    fn hit_scan_skips_latin_words_when_alnum_is_off() {
        let lines = vec![OcrLine {
            words: vec![w("Edit", 100, 100, 40, 30), w("語", 150, 100, 40, 30)],
        }];
        assert_eq!(Some((0, 0)), hit_scan(&lines, p(110, 110), true));
        assert_eq!(None, hit_scan(&lines, p(110, 110), false));
    }

    /// Resolve keeps line text unchanged.
    #[test]
    fn a_japanese_word_still_resolves_the_whole_line() {
        let lines = vec![OcrLine {
            words: vec![w("PC", 100, 100, 40, 30), w("語", 150, 100, 40, 30)],
        }];
        let got = resolve(&lines, p(160, 110), false).unwrap();
        assert_eq!("PC語", got.span.text, "PC stays in the text");
    }
}

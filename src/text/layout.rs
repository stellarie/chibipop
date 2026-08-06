//! Pure layout; no Win32 here.

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

/// The word under `cursor`.
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
            // First on a tie wins.
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

/// Which way the line runs.
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

/// OCR reads `ー` as a hyphen.
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

/// Upscaled space -> desktop.
pub fn map_from_upscaled(rect: PhysRect, origin: PhysPoint, factor: i32) -> PhysRect {
    rect.scaled_down(factor).translated(origin.x, origin.y)
}

/// Smaller is more accurate.
pub const REGION_W: i32 = 500;
pub const REGION_H: i32 = 100;

/// 400-500px reads correctly.
pub const TILE_LEN: i32 = 500;

/// Room for ascenders, drift.
pub const BAND_FACTOR: f32 = 3.0;

/// Floored at `REGION_H`.
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

/// One tile, `len` long.
pub fn tile_after(band: PhysRect, start: i32, orientation: Orientation, len: i32) -> PhysRect {
    match orientation {
        Orientation::Horizontal => PhysRect { x: start, y: band.y, w: len, h: band.h },
        Orientation::Vertical => PhysRect { x: band.x, y: start, w: band.w, h: len },
    }
}

/// `None` on zero extent.
pub fn clamp_tile(tile: PhysRect, bounds: PhysRect) -> Option<PhysRect> {
    let x0 = tile.x.max(bounds.x);
    let y0 = tile.y.max(bounds.y);
    let x1 = (tile.x + tile.w).min(bounds.x + bounds.w);
    let y1 = (tile.y + tile.h).min(bounds.y + bounds.h);
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 { None } else { Some(PhysRect { x: x0, y: y0, w, h }) }
}

/// The line under the hover.
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

/// Clip slack at a tile edge.
pub const EDGE_MARGIN: i32 = 4;

/// Edge on the reading axis.
type AxisEdge = fn(&PhysRect) -> i32;

/// Kept words + next start.
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

/// Drops words before `start`.
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

/// Reads forward in tiles.
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

/// A box and its char count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextGeom {
    pub char_count: usize,
    pub rect: PhysRect,
}

/// Rounds outward; saturates.
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

pub fn region_around(cursor: PhysPoint, prefer_vertical: bool) -> PhysRect {
    let (w, h) = if prefer_vertical {
        (REGION_H, REGION_W)
    } else {
        (REGION_W, REGION_H)
    };
    PhysRect {
        x: cursor.x - w / 2,
        y: cursor.y - h / 2,
        w,
        h,
    }
}

/// Union of two rects.
fn union_rect(a: PhysRect, b: PhysRect) -> PhysRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let r = (a.x + a.w).max(b.x + b.w);
    let bot = (a.y + a.h).max(b.y + b.h);
    PhysRect { x, y, w: r - x, h: bot - y }
}

/// Spaced chars on same line?
fn spaced_on_line(prev: PhysRect, next: PhysRect, ori: Orientation) -> bool {
    let (gap, sz, cross, lim) = match ori {
        Orientation::Horizontal => (
            next.x - (prev.x + prev.w),
            (prev.w + next.w) / 2,
            (prev.center().y - next.center().y).abs(),
            (prev.h + next.h) / 4,
        ),
        Orientation::Vertical => (
            next.y - (prev.y + prev.h),
            (prev.h + next.h) / 2,
            (prev.center().x - next.center().x).abs(),
            (prev.w + next.w) / 4,
        ),
    };
    gap > 0 && gap < 2 * sz && cross <= lim
}

/// Merge spaced single-chars.
pub fn merge_spaced_words(words: &[&OcrWord], ori: Orientation) -> Vec<OcrWord> {
    if words.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<OcrWord> = Vec::new();
    let mut text = words[0].text.clone();
    let mut rect = words[0].rect;
    let mut ok = words[0].text.chars().count() == 1;
    let mut prev = words[0].rect;
    for w in words.iter().skip(1) {
        let single = w.text.chars().count() == 1;
        if ok && single && spaced_on_line(prev, w.rect, ori) {
            text.push_str(&w.text);
            rect = union_rect(rect, w.rect);
        } else {
            out.push(OcrWord { text, rect });
            text = w.text.clone();
            rect = w.rect;
            ok = single;
        }
        prev = w.rect;
    }
    out.push(OcrWord { text, rect });
    out
}

/// Pass 1's own tail, edge-trimmed.
pub fn head_and_tail(
    lines: &[OcrLine],
    cursor: PhysPoint,
    region: PhysRect,
) -> Option<(String, i32, Orientation)> {
    let (li, wi) = hit_scan(lines, cursor)?;
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

/// Resolve a hover.
pub fn resolve(lines: &[OcrLine], cursor: PhysPoint) -> Option<Resolved> {
    let (li, wi) = hit_scan(lines, cursor)?;
    let line = &lines[li];
    let orientation = orientation_of(line);

    // OCR order is not promised.
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

    // Geom from merged words.
    let merged = merge_spaced_words(&ordered, orientation);
    let geom: Vec<TextGeom> = merged
        .iter()
        .map(|mw| TextGeom {
            char_count: mw.text.chars().count(),
            rect: mw.rect,
        })
        .collect();

    // '-' is 1 byte, 'ー' is 3.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x: i32, y: i32, ww: i32, h: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y, w: ww, h } }
    }
    fn p(x: i32, y: i32) -> PhysPoint { PhysPoint { x, y } }

    /// Three 20x20 chars in a row.
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
        // Half of 20 is 10.
        assert_eq!(Some((0, 0)), hit_scan(&horizontal_line(), p(105, 95)));
    }

    #[test]
    fn beyond_tolerance_returns_none() {
        // Far beyond half a height.
        assert_eq!(None, hit_scan(&horizontal_line(), p(105, 60)));
    }

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(None, hit_scan(&[], p(105, 105)));
        assert_eq!(None, hit_scan(&[OcrLine { words: vec![] }], p(105, 105)));
    }

    #[test]
    fn ties_break_by_document_order() {
        // A tie needs exact geometry.
        let lines = vec![OcrLine {
            words: vec![
                w("昨", 100, 100, 20, 20), // occupies x 100..=119
                w("日", 131, 100, 20, 20), // occupies x 131..=150
            ],
        }];
        assert_eq!(6.0, lines[0].words[0].rect.edge_distance_to(p(125, 105)));
        assert_eq!(6.0, lines[0].words[1].rect.edge_distance_to(p(125, 105)));
        // The lower index must win.
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
        // 昨 and 日 are 3 bytes each.
        assert_eq!(6, got.span.cursor_byte_offset);
    }

    #[test]
    fn cursor_byte_offset_lands_on_a_char_boundary() {
        let got = resolve(&horizontal_line(), p(165, 105)).unwrap();
        // Must not panic mid-char.
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
        // After kanji it stays a dash.
        assert_eq!("日-本", normalise("日-本"));
    }

    #[test]
    fn coordinates_survive_a_negative_origin() {
        // A monitor left of primary.
        let got = map_from_upscaled(
            PhysRect { x: 40, y: 20, w: 40, h: 40 },
            PhysPoint { x: -1920, y: -100 },
            2,
        );
        assert_eq!(PhysRect { x: -1900, y: -90, w: 20, h: 20 }, got);
        // And hit-scan still resolves.
        let line = OcrLine {
            words: vec![OcrWord { text: "食".to_string(), rect: got }],
        };
        assert_eq!(Some((0, 0)), hit_scan(&[line], p(-1895, -85)));
    }

    #[test]
    fn map_from_upscaled_divides_then_translates() {
        // Region origin (1000,500).
        let got = map_from_upscaled(
            PhysRect { x: 200, y: 100, w: 40, h: 40 },
            PhysPoint { x: 1000, y: 500 },
            2,
        );
        assert_eq!(PhysRect { x: 1100, y: 550, w: 20, h: 20 }, got);
    }

    /// A hyphen before the hit.
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
        // 3 chars x 3 bytes = 9.
        assert_eq!(9, got.span.cursor_byte_offset);
    }

    /// ptr::eq, not text equality.
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
        // 2 chars x 3 bytes = 6.
        assert_eq!(6, got.span.cursor_byte_offset);
        assert_eq!("々々", &got.span.text[got.span.cursor_byte_offset..]);
    }

    /// Centring is the contract.
    #[test]
    fn the_region_is_centred_on_the_cursor() {
        let r = region_around(p(1000, 500), false);
        assert_eq!(REGION_W, r.w);
        assert_eq!(REGION_H, r.h);
        assert_eq!(1000 - REGION_W / 2, r.x);
        assert_eq!(500 - REGION_H / 2, r.y);
    }

    #[test]
    fn region_around_horizontal_is_wider_than_tall() {
        let r = region_around(p(500, 500), false);
        assert!(r.w > r.h);
    }

    #[test]
    fn region_around_vertical_is_taller_than_wide() {
        let r = region_around(p(500, 500), true);
        assert!(r.h > r.w);
    }

    /// Must out-reach hit_scan.
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

    /// 44px read nothing at all.
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

    /// Measured furigana geometry.
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

    /// The same, rotated.
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

    /// A tie: both 20px away.
    #[test]
    fn nearest_line_ties_break_by_document_order() {
        let first_line = OcrLine { words: vec![w("a", 0, 100, 20, 20)] };
        let second_line = OcrLine { words: vec![w("b", 0, 140, 20, 20)] };
        let lines = vec![first_line.clone(), second_line];
        assert_eq!(Some(&first_line), nearest_line(&lines, 130, Orientation::Horizontal, 20));
    }

    /// I3: inside the tolerance.
    #[test]
    fn nearest_line_within_the_bound_is_selected() {
        let line = OcrLine { words: vec![w("a", 100, 1360, 20, 20)] };
        let lines = vec![line.clone()];
        assert_eq!(Some(&line), nearest_line(&lines, 1372, Orientation::Horizontal, 10));
    }

    /// I3: past the tolerance.
    #[test]
    fn nearest_line_beyond_the_bound_returns_none() {
        let far_line = OcrLine { words: vec![w("a", 100, 1300, 20, 20)] };
        assert_eq!(None, nearest_line(&[far_line], 1372, Orientation::Horizontal, 10));
    }

    fn hword(text: &str, x: i32, width: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y: 100, w: width, h: 40 } }
    }

    /// No clamp ever applies.
    fn unbounded() -> PhysRect {
        PhysRect { x: -1_000_000, y: -1_000_000, w: 3_000_000, h: 3_000_000 }
    }

    #[test]
    fn clamp_tile_is_unchanged_when_fully_inside_bounds() {
        let tile = PhysRect { x: 100, y: 100, w: 50, h: 50 };
        let bounds = PhysRect { x: 0, y: 0, w: 1000, h: 1000 };
        assert_eq!(Some(tile), clamp_tile(tile, bounds));
    }

    /// Wide, short: horizontal.
    #[test]
    fn clamp_tile_shrinks_a_horizontal_tile_past_the_right_edge() {
        let tile = PhysRect { x: 2300, y: 500, w: 500, h: 80 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1080 };
        assert_eq!(Some(PhysRect { x: 2300, y: 500, w: 260, h: 80 }), clamp_tile(tile, bounds));
    }

    /// Tall, narrow: vertical.
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

    /// Equal is not clipped.
    #[test]
    fn trailing_edge_exactly_at_the_margin_boundary_is_kept() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 456, 40)]; // trail = 456+40 = 496
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert_eq!(1, kept.len(), "496 must not be treated as clipped");
        assert_eq!(500, next);
    }

    /// One pixel further: 497.
    #[test]
    fn trailing_edge_one_pixel_past_the_margin_boundary_is_discarded() {
        let tile = PhysRect { x: 0, y: 90, w: 500, h: 60 };
        let words = [hword("あ", 457, 40)]; // trail = 457+40 = 497
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(457, next, "must return the clipped word's own leading edge");
    }

    /// D3: too narrow to advance.
    #[test]
    fn a_first_word_already_clipped_keeps_nothing_and_does_not_advance() {
        let tile = PhysRect { x: 400, y: 90, w: 100, h: 60 };
        let words = [hword("あ", 400, 120)];
        let (kept, next) = split_at_clipped(&words, tile, Orientation::Horizontal);
        assert!(kept.is_empty());
        assert_eq!(400, next, "must not advance past the tile's own start");
    }

    /// Word edge, not tile start.
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

    /// OCR does not promise order.
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

    /// Load-bearing: keeps a deferred glyph.
    #[test]
    fn drop_leading_keeps_a_word_exactly_at_the_margin_boundary() {
        let words = [hword("あ", 96, 40)]; // start=100, margin=4 -> 96
        let kept = drop_leading(&words, 100, Orientation::Horizontal);
        assert_eq!(1, kept.len(), "96 must not be treated as spillover");
    }

    /// One pixel further back: 95.
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

    /// D5: restarts at 470.
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

    /// D5: strict-advance stop.
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

    /// I2: tile 2 clamps to 20px.
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

    /// I2: zero extent, no read.
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

    /// I2, reading downward.
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

    /// A tile must not prepend spillover.
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

    /// "338" arrived in one box.
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

    /// Must saturate, not wrap.
    #[test]
    fn an_absurd_match_length_covers_the_rest_rather_than_wrapping() {
        let geom = vec![g(1, 100, 30), g(1, 130, 30), g(1, 160, 30)];
        let r = union_chars(&geom, 1, usize::MAX, 0).unwrap();
        assert_eq!(PhysRect { x: 130, y: 100, w: 60, h: 40 }, r);
    }

    /// Count must not change.
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
        let r = resolve(&[line], p(145, 120)).unwrap();
        assert_eq!(3, r.span.geom.len());
        assert_eq!(r.span.text.chars().count(),
                   r.span.geom.iter().map(|g| g.char_count).sum::<usize>());
    }

    /// From the hover, not start.
    #[test]
    fn the_highlight_boxes_the_hovered_word_not_the_lines_start() {
        let line = OcrLine {
            words: vec![w("そ", 100, 100, 30, 40), w("の", 130, 100, 30, 40),
                        w("可", 160, 100, 30, 40), w("哀", 190, 100, 30, 40),
                        w("想", 220, 100, 30, 40)],
        };
        let r = resolve(&[line], p(175, 120)).unwrap();
        assert_ne!(0, r.span.cursor_byte_offset, "the fixture must exercise a non-zero offset");

        let from = r.span.text[..r.span.cursor_byte_offset].chars().count();
        let boxed = union_chars(&r.span.geom, from, 3, 0).unwrap();
        assert_eq!(PhysRect { x: 160, y: 100, w: 90, h: 40 }, boxed);
    }

    /// Absent, never wrong.
    #[test]
    fn no_geometry_yields_no_highlight_rather_than_a_wrong_one() {
        assert_eq!(None, union_chars(&[], 0, 3, 3));
    }

    // -- merge_spaced_words --

    #[test]
    fn merge_empty_returns_empty() {
        let r: Vec<&OcrWord> = Vec::new();
        assert!(merge_spaced_words(&r, Orientation::Horizontal).is_empty());
    }

    #[test]
    fn merge_single_word_unchanged() {
        let words = [w("食", 100, 100, 20, 20)];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(1, m.len());
        assert_eq!("食", m[0].text);
    }

    /// gap = 0: no merge needed.
    #[test]
    fn merge_touching_chars_stay_separate() {
        let words = [
            w("可", 100, 100, 30, 40),
            w("哀", 130, 100, 30, 40),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(2, m.len());
    }

    /// gap = 1.5x: merge.
    #[test]
    fn merge_spaced_chars_are_joined() {
        let words = [
            w("食", 100, 100, 20, 20),
            w("べ", 150, 100, 20, 20),
            w("物", 200, 100, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(1, m.len());
        assert_eq!("食べ物", m[0].text);
    }

    /// gap = 4x: no merge.
    #[test]
    fn merge_far_apart_chars_stay_separate() {
        let words = [
            w("左", 100, 100, 20, 20),
            w("右", 200, 100, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(2, m.len());
    }

    /// Multi-char blocks merging.
    #[test]
    fn merge_skips_multi_char_words() {
        let words = [
            w("食", 100, 100, 20, 20),
            w("ab", 150, 100, 40, 20),
            w("物", 220, 100, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(3, m.len());
    }

    /// Cross-axis offset blocks it.
    #[test]
    fn merge_different_y_stays_separate() {
        let words = [
            w("上", 100, 100, 20, 20),
            w("下", 150, 200, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(2, m.len());
    }

    /// Vertical spaced chars merge.
    #[test]
    fn merge_vertical_spaced_chars() {
        let words = [
            w("食", 100, 100, 20, 20),
            w("べ", 100, 150, 20, 20),
            w("物", 100, 200, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Vertical);
        assert_eq!(1, m.len());
        assert_eq!("食べ物", m[0].text);
    }

    /// Union rect spans the gap.
    #[test]
    fn merge_rect_covers_the_gap() {
        let words = [
            w("食", 100, 100, 20, 20),
            w("物", 150, 100, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(1, m.len());
        assert_eq!(PhysRect { x: 100, y: 100, w: 70, h: 20 }, m[0].rect);
    }

    /// Large gap breaks the chain.
    #[test]
    fn merge_chain_broken_by_large_gap() {
        let words = [
            w("食", 100, 100, 20, 20),
            w("べ", 150, 100, 20, 20),
            w("物", 300, 100, 20, 20),
        ];
        let r: Vec<&OcrWord> = words.iter().collect();
        let m = merge_spaced_words(&r, Orientation::Horizontal);
        assert_eq!(2, m.len());
        assert_eq!("食べ", m[0].text);
        assert_eq!("物", m[1].text);
    }

    // -- resolve with spaced text --

    /// Spaced geom is merged.
    #[test]
    fn resolve_merges_spaced_geom() {
        let line = OcrLine {
            words: vec![
                w("食", 100, 100, 20, 20),
                w("べ", 150, 100, 20, 20),
                w("物", 200, 100, 20, 20),
            ],
        };
        let got = resolve(&[line], p(155, 105)).unwrap();
        assert_eq!("食べ物", got.span.text);
        assert_eq!(1, got.span.geom.len());
        assert_eq!(3, got.span.geom[0].char_count);
    }

    /// Cursor offset survives merge.
    #[test]
    fn resolve_spaced_cursor_offset_correct() {
        let line = OcrLine {
            words: vec![
                w("食", 100, 100, 20, 20),
                w("べ", 150, 100, 20, 20),
                w("物", 200, 100, 20, 20),
            ],
        };
        let got = resolve(&[line], p(205, 105)).unwrap();
        assert_eq!(6, got.span.cursor_byte_offset);
        assert!(got.span.text[6..].starts_with('物'));
    }

    /// Vertical spaced chars merge.
    #[test]
    fn resolve_spaced_vertical_merges() {
        let line = OcrLine {
            words: vec![
                w("食", 100, 100, 20, 20),
                w("べ", 100, 150, 20, 20),
                w("物", 100, 200, 20, 20),
            ],
        };
        let got = resolve(&[line], p(105, 155)).unwrap();
        assert_eq!(Orientation::Vertical, got.orientation);
        assert_eq!(1, got.span.geom.len());
    }

    /// Touching keeps per-char geom.
    #[test]
    fn resolve_touching_chars_keep_per_char_geom() {
        let line = OcrLine {
            words: vec![
                w("可", 100, 100, 30, 40),
                w("哀", 130, 100, 30, 40),
                w("想", 160, 100, 30, 40),
            ],
        };
        let got = resolve(&[line], p(145, 120)).unwrap();
        assert_eq!(3, got.span.geom.len());
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
        let (head, next, ori) = head_and_tail(&[line], p(165, 105), region).unwrap();
        assert_eq!("感想", head, "text before the hit word is not head");
        assert_eq!(Orientation::Horizontal, ori);
        assert_eq!(550, next, "region's own trailing edge: 50+500");
    }

    /// The last kept word's edge, not pass 1's edge.
    #[test]
    fn head_and_tail_excludes_a_word_clipped_at_the_regions_edge() {
        let line = OcrLine {
            words: vec![
                w("感", 460, 100, 20, 20),
                w("想", 490, 100, 20, 20), // trail = 510, region ends at 500
            ],
        };
        let region = PhysRect { x: 0, y: 80, w: 500, h: 60 };
        let (head, next, _) = head_and_tail(&[line], p(465, 105), region).unwrap();
        assert_eq!("感", head, "想 is clipped by the region's own edge");
        assert_eq!(490, next, "tiling resumes at the clipped word's own edge");
    }

    /// Pass 1 may distrust its own hit.
    #[test]
    fn head_and_tail_empty_head_when_the_hit_word_itself_is_clipped() {
        let line = OcrLine { words: vec![w("想", 490, 100, 20, 20)] }; // trail=510
        let region = PhysRect { x: 0, y: 80, w: 500, h: 60 }; // end=500
        let (head, next, _) = head_and_tail(&[line], p(495, 105), region).unwrap();
        assert_eq!("", head, "even the hit word is clipped by the region edge");
        assert_eq!(490, next, "falls back to the hit word's own leading edge");
    }

    #[test]
    fn head_and_tail_returns_none_when_nothing_is_hit() {
        let line = OcrLine { words: vec![w("食", 100, 100, 20, 20)] };
        let region = PhysRect { x: 0, y: 0, w: 500, h: 100 };
        assert_eq!(None, head_and_tail(&[line], p(900, 900), region));
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
        let (head, _, ori) = head_and_tail(&[line], p(105, 105), region).unwrap();
        assert_eq!("上下", head);
        assert_eq!(Orientation::Vertical, ori);
    }
}

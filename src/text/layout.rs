//! This module performs layout with physical pixels.
//! It has no Win32 code.

use crate::geom::{PhysPoint, PhysRect};
use crate::lookup::engine::runs_out;
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

fn line_rect(line: &OcrLine) -> Option<PhysRect> {
    let mut words = line.words.iter();
    let first = words.next()?.rect;
    Some(words.fold(first, |all, word| {
        let right = all.x.saturating_add(all.w).max(word.rect.x.saturating_add(word.rect.w));
        let bottom = all.y.saturating_add(all.h).max(word.rect.y.saturating_add(word.rect.h));
        let x = all.x.min(word.rect.x);
        let y = all.y.min(word.rect.y);
        PhysRect { x, y, w: right.saturating_sub(x), h: bottom.saturating_sub(y) }
    }))
}

fn interval_overlap(a: i32, a_len: i32, b: i32, b_len: i32) -> i32 {
    a.saturating_add(a_len)
        .min(b.saturating_add(b_len))
        .saturating_sub(a.max(b))
}

fn interval_gap(a: i32, a_len: i32, b: i32, b_len: i32) -> i32 {
    a.max(b)
        .saturating_sub(a.saturating_add(a_len).min(b.saturating_add(b_len)))
        .max(0)
}

fn contains_kanji(line: &OcrLine) -> bool {
    line.words.iter().any(|word| {
        word.text.chars().any(|c| {
            matches!(c, '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}')
        })
    })
}

fn contains_only_kana(line: &OcrLine) -> bool {
    let mut saw_character = false;
    let only_kana = line.words.iter().flat_map(|word| word.text.chars()).all(|c| {
        saw_character = true;
        is_kana(c)
    });
    saw_character && only_kana
}

fn is_ruby_pair(ruby: PhysRect, base: PhysRect) -> bool {
    let horizontal_overlap = interval_overlap(ruby.x, ruby.w, base.x, base.w);
    let vertical_overlap = interval_overlap(ruby.y, ruby.h, base.y, base.h);
    let horizontal = ruby.w >= ruby.h
        && base.w >= base.h
        && i64::from(ruby.h) * 3 <= i64::from(base.h) * 2
        && horizontal_overlap >= ruby.w.saturating_add(1) / 2
        && interval_gap(ruby.y, ruby.h, base.y, base.h) <= base.h;
    let vertical = ruby.h >= ruby.w
        && base.h >= base.w
        && i64::from(ruby.w) * 3 <= i64::from(base.w) * 2
        && vertical_overlap >= ruby.h.saturating_add(1) / 2
        && interval_gap(ruby.x, ruby.w, base.x, base.w) <= base.w;
    horizontal || vertical
}

/// Removes geometric ruby lines.
pub fn discard_furigana(lines: Vec<OcrLine>) -> Vec<OcrLine> {
    let rects: Vec<Option<PhysRect>> = lines.iter().map(line_rect).collect();
    let discard: Vec<bool> = lines
        .iter()
        .enumerate()
        .map(|(ruby_index, line)| {
            let Some(ruby) = rects[ruby_index] else { return false };
            contains_only_kana(line)
                && rects.iter().enumerate().any(|(base_index, base)| {
                    base_index != ruby_index
                        && contains_kanji(&lines[base_index])
                        && base.is_some_and(|base| is_ruby_pair(ruby, base))
                })
        })
        .collect();
    lines
        .into_iter()
        .enumerate()
        .filter_map(|(index, line)| (!discard[index]).then_some(line))
        .collect()
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
impl Orientation {
    /// The reading-axis start edge: `x` for a row, `y` for a column.
    pub(crate) fn lead(self, r: PhysRect) -> i32 {
        match self {
            Self::Horizontal => r.x,
            Self::Vertical => r.y,
        }
    }

    /// The reading-axis end edge, saturating.
    pub(crate) fn trail(self, r: PhysRect) -> i32 {
        match self {
            Self::Horizontal => r.x.saturating_add(r.w),
            Self::Vertical => r.y.saturating_add(r.h),
        }
    }

    /// The reading-axis extent: `w` for a row, `h` for a column.
    pub(crate) fn len(self, r: PhysRect) -> i32 {
        match self {
            Self::Horizontal => r.w,
            Self::Vertical => r.h,
        }
    }

    /// The cross-axis extent: `h` for a row, `w` for a column.
    pub(crate) fn thick(self, r: PhysRect) -> i32 {
        match self {
            Self::Horizontal => r.h,
            Self::Vertical => r.w,
        }
    }

    /// The cross-axis coordinate of a point: `y` for a row, `x` for a column.
    pub(crate) fn cross(self, p: PhysPoint) -> i32 {
        match self {
            Self::Horizontal => p.y,
            Self::Vertical => p.x,
        }
    }

    /// A rectangle from reading-axis (`lead`, `len`) and cross-axis (`cross_start`, `thick`) parts.
    pub(crate) fn rect(self, lead: i32, len: i32, cross_start: i32, thick: i32) -> PhysRect {
        match self {
            Self::Horizontal => PhysRect { x: lead, y: cross_start, w: len, h: thick },
            Self::Vertical => PhysRect { x: cross_start, y: lead, w: thick, h: len },
        }
    }
}

#[derive(Debug)]
pub struct Resolved {
    pub span: TextSpan,
    pub orientation: Orientation,
}

/// A line can override the capture box only when it has at least this many words.
///
/// Issue #92: an engine boxed the components of one large `新` as `立` over `木`, two
/// words stacked inside a horizontal box. Their center spread made the line vertical.
/// The wrap probe then started at the output top edge, and the forward tile ran below
/// the text. Two words cannot override the box. Three words that fill the short side
/// of the box form a real line.
pub const OVERRIDE_WORDS: usize = 3;

/// Return the line orientation. `prior` is the orientation of the capture box.
///
/// The user chose the box shape with `prefer_vertical`, so the box is the prior. A
/// line overrides it only with strong evidence: at least [`OVERRIDE_WORDS`] words
/// whose centers spread more along the other axis, and a union at least twice as long
/// as it is thick on that axis. Three stacked components of one glyph (`意` as `立`,
/// `日`, `心`) form a square union, not a column. A column of three 40 px glyphs in a
/// 100 px box forms a union 100 tall and 40 wide, which passes.
pub fn orientation_of(line: &OcrLine, prior: Orientation) -> Orientation {
    if line.words.len() < OVERRIDE_WORDS {
        return prior;
    }
    let centres: Vec<PhysPoint> = line.words.iter().map(|w| w.rect.center()).collect();
    let (x_min, x_max) = centres.iter().fold((i32::MAX, i32::MIN), |(min, max), c| {
        (min.min(c.x), max.max(c.x))
    });
    let (y_min, y_max) = centres.iter().fold((i32::MAX, i32::MIN), |(min, max), c| {
        (min.min(c.y), max.max(c.y))
    });
    let x_spread = x_max - x_min;
    let y_spread = y_max - y_min;
    let by_spread = match y_spread.cmp(&x_spread) {
        std::cmp::Ordering::Greater => Orientation::Vertical,
        std::cmp::Ordering::Less => Orientation::Horizontal,
        std::cmp::Ordering::Equal => return prior,
    };
    if by_spread == prior {
        return prior;
    }
    let Some(union) = line_rect(line) else { return prior };
    if by_spread.len(union) >= 2 * by_spread.thick(union) { by_spread } else { prior }
}

/// Return the orientation that a capture box implies: a box taller than it is wide
/// captures a column.
pub fn box_orientation(region: PhysRect) -> Orientation {
    if region.h > region.w { Orientation::Vertical } else { Orientation::Horizontal }
}

/// The capture box can grow at most this many times.
///
/// Each step doubles both sides: 500 x 100 becomes 1000 x 200, then 2000 x 400. Issue
/// #92 hovered text at about the box height. The Linux engine scales a crop to its
/// detector size, and a 500 px wide crop is always scaled up by 1.92. A 120 px glyph
/// in that crop is too large to detect, and no taller 500 px wide box changed that.
/// The engine scales a 1000 px wide box by 0.96. Every measured case returned text from
/// that box. A glyph above 400 px is not popup text.
pub const GROWTH_STEPS: usize = 2;

/// Return true when a line spans the short side of `region`.
///
/// A glyph at least as tall as the box touches both of its long edges, within
/// [`EDGE_MARGIN`]. The engine then sees a cut glyph and returns a fragment, an
/// incorrect result, or no text. The union of a line covers a stack of fragments
/// and one cut word. The check uses every line. The cursor sits inside the box, and
/// a line that spans the box passes through the cursor's row.
pub fn spans_short_side(lines: &[OcrLine], region: PhysRect) -> bool {
    let orientation = box_orientation(region);
    let start = orientation.cross(PhysPoint { x: region.x, y: region.y });
    let end = start + orientation.thick(region);
    lines.iter().filter_map(line_rect).any(|line| {
        let lead = orientation.cross(PhysPoint { x: line.x, y: line.y });
        lead <= start + EDGE_MARGIN && lead + orientation.thick(line) >= end - EDGE_MARGIN
    })
}

/// Return true when the word under the cursor is cut by one edge of `region`.
///
/// A cursor near the top or the bottom of a large glyph puts one box edge through
/// the row. The visible part fits the box, so no line spans it, and the engine
/// returns incorrect text in boxes that touch that edge (issue #92: `サ千子ペナ`
/// for the top half of `活発な`). The word is cut when it touches an edge within
/// [`EDGE_MARGIN`] and is at least half the box thick. A body line under a cursor
/// just below it touches the top edge too, but at 40 px in a 100 px box it is not a
/// large glyph, and the engine reads it.
pub fn hit_cut_by_edge(lines: &[OcrLine], cursor: PhysPoint, scan_alnum: bool, region: PhysRect) -> bool {
    let Some((li, wi)) = hit_scan(lines, cursor, scan_alnum) else { return false };
    let word = lines[li].words[wi].rect;
    let orientation = box_orientation(region);
    let start = orientation.cross(PhysPoint { x: region.x, y: region.y });
    let end = start + orientation.thick(region);
    let lead = orientation.cross(PhysPoint { x: word.x, y: word.y });
    let thick = orientation.thick(word);
    thick * 2 >= orientation.thick(region)
        && (lead <= start + EDGE_MARGIN || lead + thick >= end - EDGE_MARGIN)
}

/// Return `region` doubled on both sides around the same center, inside `bounds`.
///
/// Both sides double because the engine's scale depends on both. A box that grows
/// on its short side alone keeps the reading axis at 500 px and the scale at 1.92.
/// The grown box moves inside the output when it can, and it shrinks to the output
/// only when it is larger. A box that starts outside its monitor fails the Windows
/// DXGI grab and costs a one second BitBlt fallback. A box that shrinks on the
/// reading axis raises the scale: the engine reads a bold 125 px line in a 500 px wide
/// box, but not in a 432 px wide one.
pub fn grow(region: PhysRect, bounds: PhysRect) -> PhysRect {
    let grown = region.inflated(region.w / 2, region.h / 2);
    let fit = |start: i32, len: i32, bound_start: i32, bound_len: i32| {
        if len >= bound_len {
            (bound_start, bound_len)
        } else {
            (start.max(bound_start).min(bound_start + bound_len - len), len)
        }
    };
    let (x, w) = fit(grown.x, grown.w, bounds.x, bounds.w);
    let (y, h) = fit(grown.y, grown.h, bounds.y, bounds.h);
    PhysRect { x, y, w, h }
}

/// A word box thinner than this fraction of its thickness on the reading axis is a
/// sliver, not a glyph.
///
/// The Linux engine returned a `」` in a 4 x 92 box at the right edge of a 100 px `規`.
/// No glyph is that thin: a `l` or a vertical `ー` is about one fifth of its height.
/// A word box can be long, so the rule reads only the reading axis.
pub const SLIVER_RATIO: i32 = 16;

/// Drop sliver words and lines that then have no words. `orientation` is the capture
/// box orientation.
pub fn drop_slivers(lines: Vec<OcrLine>, orientation: Orientation) -> Vec<OcrLine> {
    lines
        .into_iter()
        .map(|mut line| {
            line.words.retain(|word| {
                orientation.len(word.rect) * SLIVER_RATIO >= orientation.thick(word.rect)
            });
            line
        })
        .filter(|line| !line.words.is_empty())
        .collect()
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
    let t = (((orientation.thick(word) as f32) * BAND_FACTOR).round() as i32).max(short_floor);
    orientation.rect(
        orientation.lead(word),
        orientation.len(word),
        orientation.cross(c) - t / 2,
        t,
    )
}

/// Build one tile with length `len`.
pub fn tile_after(band: PhysRect, start: i32, orientation: Orientation, len: i32) -> PhysRect {
    orientation.rect(
        start,
        len,
        orientation.cross(PhysPoint { x: band.x, y: band.y }),
        orientation.thick(band),
    )
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
    let mut best: Option<(&OcrLine, i32)> = None;
    for line in lines {
        if line.words.is_empty() {
            continue;
        }
        let sum: i32 = line.words.iter().map(|w| orientation.cross(w.rect.center())).sum();
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


/// Keep complete words and return the next tile start.
pub fn split_at_clipped(
    words: &[OcrWord],
    tile: PhysRect,
    orientation: Orientation,
) -> (Vec<&OcrWord>, i32) {
    let end = orientation.trail(tile);
    let mut ordered: Vec<&OcrWord> = words.iter().collect();
    ordered.sort_by_key(|word| orientation.lead(word.rect));

    let mut kept = Vec::new();
    for word in ordered {
        if orientation.trail(word.rect) > end - EDGE_MARGIN {
            return (kept, orientation.lead(word.rect));
        }
        kept.push(word);
    }
    (kept, end)
}

/// Return words whose start edge is near or after `start`.
pub fn drop_leading(words: &[OcrWord], start: i32, orientation: Orientation) -> Vec<OcrWord> {
    words
        .iter()
        .filter(|w| orientation.lead(w.rect) >= start - EDGE_MARGIN)
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

/// Return the mean center across a line: `y` for a row and `x` for a column.
pub(crate) fn perp_centre(line: &OcrLine, orientation: Orientation) -> i32 {
    let sum: i32 = line.words.iter().map(|w| orientation.cross(w.rect.center())).sum();
    sum / line.words.len() as i32
}

/// Return the mean thickness of a line's words: `h` for a row and `w` for a column.
pub(crate) fn thickness(line: &OcrLine, orientation: Orientation) -> i32 {
    let sum: i32 = line.words.iter().map(|w| orientation.thick(w.rect)).sum();
    sum / line.words.len() as i32
}

/// Return the first edge of a line on the reading axis.
pub(crate) fn reading_start(line: &OcrLine, orientation: Orientation) -> Option<i32> {
    line.words.iter().map(|w| orientation.lead(w.rect)).min()
}

/// Return the gap from `a` to `b` on the reading axis.
/// A row's next line lies below it. A column's next line lies to its left.
pub(crate) fn forward_gap(a: &OcrLine, b: &OcrLine, orientation: Orientation) -> i32 {
    match orientation {
        Orientation::Horizontal => perp_centre(b, orientation) - perp_centre(a, orientation),
        Orientation::Vertical => perp_centre(a, orientation) - perp_centre(b, orientation),
    }
}

/// Return true when `thick` is near `reference`.
///
/// Ruby above a base line is half its height. A heading is larger. Neither one forms
/// a continuation.
pub(crate) fn same_size(reference: i32, thick: i32) -> bool {
    thick * 3 >= reference * 2 && thick * 2 <= reference * 3
}

/// Set the widest continuation gap to 4.5 thicknesses, or nine half-thicknesses.
///
/// Tight text wraps within 1.5 thicknesses. An NHK Easy page with ruby places the next
/// line 3 to 3.5 thicknesses below. A loose `line-height` falls between these values.
/// One fixed reach covers all cases. An earlier design measured pitch from nearby lines
/// and failed on a two-line paragraph because the tested pair had no pitch beside it.
/// The fixed reach can join across a paragraph gap when the last line has no 。 or 」.
/// [`wrap_probe`] rejects that case before it reads anything. Only a line without
/// punctuation pays this cost. [`wrap_probe`] sizes its probes from this value.
pub(crate) const WRAP_GAP_HALVES: i32 = 9;

/// Return the center gap when `next` forms a continuation of `current`.
///
/// The gap ranges from half to 4.5 thicknesses. The start of `next` is no more than
/// half a thickness after `current`.
fn continuation_gap(current: &OcrLine, next: &OcrLine, orientation: Orientation) -> Option<i32> {
    if current.words.is_empty() || next.words.is_empty() {
        return None;
    }
    let line_h = thickness(current, orientation).max(thickness(next, orientation));
    if line_h <= 0 {
        return None;
    }

    let gap = forward_gap(current, next, orientation);
    if gap < line_h / 2 || gap > line_h * WRAP_GAP_HALVES / 2 {
        return None;
    }
    let current_start = reading_start(current, orientation)?;
    let next_start = reading_start(next, orientation)?;
    if next_start > current_start + line_h / 2 {
        return None;
    }
    Some(gap)
}

/// Return the text that the wrap probe read.
///
/// The probe exists only to find a wrap.
/// A probe with no continuation returns no answer.
/// Pass 1's answer then stands.
/// `orientation` comes from pass 1. A clipped probe can contain one word and cannot
/// reliably identify a vertical column.
pub fn resolve_wrap(
    probes: &[(PhysRect, Vec<OcrLine>)],
    cursor: PhysPoint,
    scan_alnum: bool,
    orientation: Orientation,
    bounds: PhysRect,
) -> Option<Resolved> {
    let line_count: usize = probes.iter().map(|(_, lines)| lines.len()).sum();
    let mut trimmed_lines = Vec::with_capacity(line_count);
    for (_, lines) in probes {
        trimmed_lines.extend(lines.iter().cloned());
    }
    hit_scan(&trimmed_lines, cursor, scan_alnum)?;

    let mut offset = 0;
    for (probe, lines) in probes {
        let end = offset + lines.len();
        trim_probe_edges(&mut trimmed_lines[offset..end], *probe, bounds, orientation);
        offset = end;
    }

    let combined = combine_probe_lines(&trimmed_lines, orientation);
    resolve_wrapped(&combined, cursor, scan_alnum, orientation)
        .and_then(|(resolved, wrapped)| wrapped.then_some(resolved))
}

/// Trim words at probe seams before code merges overlaps.
///
/// OCR can join half a glyph at a probe seam to its neighbor.
/// Half-tile overlap gives a multi-character word a full interior copy in one
/// probe, while this trim drops clipped copies from both sides of the seam.
/// The tile path already applies this rule in [`split_at_clipped`] and [`drop_leading`].
/// An output edge is a real screen edge. It cannot cut a glyph, so this code does not
/// trim it.
pub(crate) fn trim_probe_edges(
    lines: &mut [OcrLine],
    probe: PhysRect,
    bounds: PhysRect,
    orientation: Orientation,
) {
    let probe_lead = orientation.lead(probe);
    let probe_end = orientation.trail(probe);
    let bounds_lead = orientation.lead(bounds);
    let bounds_end = orientation.trail(bounds);
    let trim_lead = probe_lead > bounds_lead;
    let trim_trail = probe_end < bounds_end;
    for line in lines {
        line.words.retain(|word| {
            (!trim_lead || orientation.lead(word.rect) >= probe_lead + EDGE_MARGIN)
                && (!trim_trail || orientation.trail(word.rect) <= probe_end - EDGE_MARGIN)
        });
    }
}

/// Merge fragments from one physical line. Two probes that overlap read those fragments.
fn combine_probe_lines(lines: &[OcrLine], orientation: Orientation) -> Vec<OcrLine> {
    let mut combined = Vec::new();
    for line in lines.iter().filter(|line| !line.words.is_empty()) {
        let thick = thickness(line, orientation);
        let existing = combined.iter_mut().position(|candidate: &mut OcrLine| {
            let candidate_thick = thickness(candidate, orientation);
            same_size(candidate_thick, thick)
                && (perp_centre(candidate, orientation) - perp_centre(line, orientation)).abs()
                    < candidate_thick.max(thick) / 2
        });
        if let Some(index) = existing {
            let candidate = &mut combined[index];
            let mut words = std::mem::take(&mut candidate.words);
            words.extend(line.words.iter().cloned());

            // Process larger boxes first. A complete OCR word can overlap every
            // fragment from another probe. Remove all smaller alternatives.
            let mut order: Vec<usize> = (0..words.len()).collect();
            order.sort_by(|a, b| {
                let area = |index: usize| {
                    let rect = words[index].rect;
                    i64::from(rect.w) * i64::from(rect.h)
                };
                area(*b).cmp(&area(*a))
            });
            let mut kept = Vec::with_capacity(words.len());
            let mut keep = vec![true; words.len()];
            for &index in &order {
                if !keep[index] {
                    continue;
                }
                if kept.iter().any(|&known| same_probe_word(&words[known], &words[index])) {
                    keep[index] = false;
                } else {
                    kept.push(index);
                }
            }

            candidate.words = words
                .into_iter()
                .enumerate()
                .filter_map(|(index, word)| keep[index].then_some(word))
                .collect();
        } else {
            combined.push(line.clone());
        }
    }
    combined
}

/// Find the nearest body line that continues the line at `li`.
fn find_continuation(lines: &[OcrLine], li: usize, orientation: Orientation) -> Option<&OcrLine> {
    let current = &lines[li];
    if current.words.is_empty() {
        return None;
    }
    let thick = thickness(current, orientation);
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| *i != li && !l.words.is_empty())
        .filter(|(_, l)| same_size(thick, thickness(l, orientation)))
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
    let orientation = orientation_of(line, box_orientation(region));

    let mut ordered: Vec<&OcrWord> = line.words.iter().collect();
    ordered.sort_by_key(|w| orientation.lead(w.rect));

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

/// Resolve the text under the hover. `region` is the capture box that `lines` came
/// from. Its shape is the orientation prior.
pub fn resolve(
    lines: &[OcrLine],
    cursor: PhysPoint,
    region: PhysRect,
    scan_alnum: bool,
) -> Option<Resolved> {
    let (li, _) = hit_scan(lines, cursor, scan_alnum)?;
    let orientation = orientation_of(&lines[li], box_orientation(region));
    resolve_wrapped(lines, cursor, scan_alnum, orientation).map(|(resolved, _)| resolved)
}

/// Return true when two OCR words represent the same physical word.
///
/// Probes that overlap can shift a box by a few pixels. The intersection must cover
/// at least half of the smaller box. Adjacent repeated characters with touching boxes
/// must remain separate.
fn same_probe_word(a: &OcrWord, b: &OcrWord) -> bool {
    let Some(overlap) = a.rect.intersection(b.rect) else {
        return false;
    };
    let overlap_area = i64::from(overlap.w) * i64::from(overlap.h);
    let a_area = i64::from(a.rect.w) * i64::from(a.rect.h);
    let b_area = i64::from(b.rect.w) * i64::from(b.rect.h);
    overlap_area * 2 >= a_area.min(b_area)
}

/// Resolve text and report whether a continuation line joined it.
fn resolve_wrapped(
    lines: &[OcrLine],
    cursor: PhysPoint,
    scan_alnum: bool,
    orientation: Orientation,
) -> Option<(Resolved, bool)> {
    let (li, wi) = hit_scan(lines, cursor, scan_alnum)?;
    let line = &lines[li];

    // OCR does not promise an order.
    let mut ordered: Vec<&OcrWord> = line.words.iter().collect();
    ordered.sort_by_key(|w| orientation.lead(w.rect));

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
    let continuation = find_continuation(lines, li, orientation);
    if let Some(next_line) = continuation {
        let mut tail: Vec<&OcrWord> = next_line.words.iter().collect();
        tail.sort_by_key(|w| orientation.lead(w.rect));
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

    Some((
        Resolved {
            span: TextSpan {
                text,
                cursor_byte_offset: prefix_len,
                anchor: hit.rect,
                geom,
            },
            orientation,
        },
        continuation.is_some(),
    ))
}

/// Return bounded probes when a wrap can hide outside pass 1's box.
///
/// A wrapped word ends one line and continues at the margin of the next line.
/// The hit lies near the hovered line's end, and the margin lies far to the left.
/// A box centered on the cursor rarely holds both.
///
/// A short output uses one probe. A wide output uses probes with half of the maximum
/// reading-axis length overlapped across the full span from the output margin to the
/// line end. Each probe is at most `2 * TILE_LEN` on its reading axis.
///
/// The probe costs one or more grabs and OCR passes. Four gates keep it rare:
/// - The lookup runs out at the line's end. A separator or the character cap between
///   the hit and the end stops the lookup first. No wrap can matter ([`runs_out`]).
/// - The box did not clip the line. A word at the box edge is the last word that the
///   box shows, not the last word on the line. [`split_at_clipped`] uses the same margin.
/// - Pass 1 found no continuation, unless that continuation lies near the probe's
///   leading edge and the box can clip it.
/// - The probe is skipped only when every probe lies inside pass 1's box. A probe
///   inside the box repeats a capture that found nothing.
///
/// The probes are clamped to `bounds` with [`clamp_tile`].
pub fn wrap_probe(
    lines: &[OcrLine],
    cursor: PhysPoint,
    region: PhysRect,
    scan_alnum: bool,
    bounds: PhysRect,
) -> Option<Vec<PhysRect>> {
    let (li, wi) = hit_scan(lines, cursor, scan_alnum)?;
    let line = &lines[li];
    let orientation = orientation_of(line, box_orientation(region));
    let hit = line.words[wi].rect;
    let region_lead = orientation.lead(region);
    let region_end = orientation.trail(region);
    let bounds_end = orientation.trail(bounds);
    let hit_lead = orientation.lead(hit);
    let mut tail_words: Vec<&OcrWord> = line
        .words
        .iter()
        .filter(|word| orientation.lead(word.rect) >= hit_lead)
        .collect();
    tail_words.sort_by_key(|word| orientation.lead(word.rect));
    if tail_words.is_empty() {
        return None;
    }
    let mut tail = String::new();
    let mut line_end = orientation.trail(hit);
    for word in tail_words {
        tail.push_str(&word.text);
        line_end = line_end.max(orientation.trail(word.rect));
    }
    if !runs_out(&tail) {
        return None;
    }
    if line_end > region_end.saturating_sub(EDGE_MARGIN) && region_end < bounds_end {
        return None;
    }
    if let Some(next_line) = find_continuation(lines, li, orientation) {
        let thick = thickness(line, orientation);
        let next_start = reading_start(next_line, orientation)?;
        let near_leading_edge = next_start >= region_lead.saturating_sub(thick)
            && next_start <= region_lead.saturating_add(thick);
        if !near_leading_edge {
            return None;
        }
    }

    let thick = thickness(line, orientation);
    if thick <= 0 {
        return None;
    }
    // The reach below covers six thicknesses: 4.5 thicknesses for the gap ceiling, half a
    // line for the continuation's glyphs, and one thickness of slack. The slack covers a
    // continuation that is somewhat thicker than the hovered line and mean-center error.
    // These terms sum to six.
    // At the `same_size` 1.5x limit and the full gap ceiling, a continuation lies beyond
    // this reach. We accept that case to keep the probe short.
    let above = thick * 3 / 4;
    let below = thick * (WRAP_GAP_HALVES + 3) / 2;
    let probe_start = orientation.lead(bounds);
    let probe_end = line_end.saturating_add(thick / 2).min(bounds_end);
    if probe_end <= probe_start {
        return None;
    }
    let cross_start = match orientation {
        Orientation::Horizontal => perp_centre(line, orientation).saturating_sub(above),
        Orientation::Vertical => perp_centre(line, orientation).saturating_sub(below),
    };
    let band = orientation.rect(probe_start, probe_end - probe_start, cross_start, above + below);
    const MAX_PROBE_LEN: i32 = 2 * TILE_LEN;
    // Overlap adjacent windows by half of the maximum length. This gives a
    // multi-character OCR word a full copy away from both trim seams.
    let overlap = MAX_PROBE_LEN / 2;
    let step = MAX_PROBE_LEN - overlap;
    let mut probes = Vec::new();
    let mut add = |start: i32, len: i32| {
        let probe = orientation.rect(
            start,
            len,
            orientation.cross(PhysPoint { x: band.x, y: band.y }),
            orientation.thick(band),
        );
        if let Some(probe) = clamp_tile(probe, bounds) {
            if !probes.contains(&probe) {
                probes.push(probe);
            }
        }
    };

    let mut start = probe_start;
    while start < probe_end {
        let remaining = probe_end.saturating_sub(start);
        let len = remaining.min(MAX_PROBE_LEN);
        add(start, len);
        if len == remaining {
            break;
        }
        let next = start.saturating_add(step);
        if next <= start {
            break;
        }
        start = next;
    }
    if probes.is_empty() || probes.iter().all(|probe| region.intersection(*probe) == Some(*probe)) {
        return None;
    }
    Some(probes)
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

    #[test]
    fn horizontal_ruby_is_removed() {
        let lines = vec![
            OcrLine { words: vec![w("かんじ", 105, 75, 45, 12)] },
            OcrLine { words: vec![w("漢字", 100, 90, 60, 30)] },
        ];
        let filtered = discard_furigana(lines);
        assert_eq!(1, filtered.len());
        assert_eq!("漢字", filtered[0].words[0].text);
    }

    fn w(text: &str, x: i32, y: i32, ww: i32, h: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y, w: ww, h } }
    }
    fn p(x: i32, y: i32) -> PhysPoint { PhysPoint { x, y } }
    /// A horizontal capture box that holds every fixture below.
    fn wide_box() -> PhysRect { PhysRect { x: 0, y: 0, w: 1000, h: 400 } }
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
        let got = resolve(&[line], p(170, 110), wide_box(), true).unwrap();
        assert_eq!("々人々々", got.span.text);
        // 2 chars x 3 bytes = 6.
        assert_eq!(6, got.span.cursor_byte_offset);
        assert_eq!("々々", &got.span.text[got.span.cursor_byte_offset..]);
    }

    fn hword(text: &str, x: i32, width: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y: 100, w: width, h: 40 } }
    }

    /// This helper returns a large bound, so no clamp applies.
    fn unbounded() -> PhysRect {
        PhysRect { x: -1_000_000, y: -1_000_000, w: 3_000_000, h: 3_000_000 }
    }

    // -- drop_leading --

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

    /// Start the highlight at the hover, not at the line start.
    #[test]
    fn the_highlight_boxes_the_hovered_word_not_the_lines_start() {
        let line = OcrLine {
            words: vec![w("そ", 100, 100, 30, 40), w("の", 130, 100, 30, 40),
                        w("可", 160, 100, 30, 40), w("哀", 190, 100, 30, 40),
                        w("想", 220, 100, 30, 40)],
        };
        let r = resolve(&[line], p(175, 120), wide_box(), true).unwrap();
        assert_ne!(0, r.span.cursor_byte_offset, "the fixture must exercise a non-zero offset");

        let from = r.span.text[..r.span.cursor_byte_offset].chars().count();
        let boxed = union_chars(&r.span.geom, from, 3, 0).unwrap();
        assert_eq!(PhysRect { x: 160, y: 100, w: 90, h: 40 }, boxed);
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
        let got = resolve(&[line], p(155, 105), wide_box(), true).unwrap();
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

    // -- line wrap continuation --

    // -- ruby and loose pitch --

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
        let got = resolve(&wrapped_lines(), p(190, 110), wide_box(), true).unwrap();
        assert_eq!("新しい冒険が始まる", got.span.text);
    }

    // -- the wrap probe --

    const PROBE_BOUNDS: PhysRect = PhysRect { x: 0, y: 0, w: 2000, h: 1500 };
    #[test]
    fn wrap_probe_splits_a_wide_output_into_overlapping_bounded_regions() {
        let lines = vec![OcrLine {
            words: vec![
                w("に", 1800, 480, 40, 40),
                w("も", 1840, 480, 40, 40),
                w("疎", 1880, 480, 40, 40),
            ],
        }];
        let probes = wrap_probe(
            &lines,
            p(1900, 500),
            PhysRect { x: 1750, y: 450, w: 500, h: 100 },
            true,
            PROBE_BOUNDS,
        )
        .expect("a wide output needs a probe");
        assert_eq!(3, probes.len());
        assert_eq!(PhysRect { x: 0, y: 470, w: 1000, h: 270 }, probes[0]);
        assert_eq!(PhysRect { x: 500, y: 470, w: 1000, h: 270 }, probes[1]);
        assert_eq!(PhysRect { x: 1000, y: 470, w: 940, h: 270 }, probes[2]);
        assert!(probes.iter().all(|probe| probe.w <= 2 * TILE_LEN));
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

    // -- hit-scan eligibility --

}

//! Read a larger OCR strip when the user adds a word to Anki.
//!
//! The hover box is small for speed. Hover-time sentence text is only a fragment.
//! The probe runs only on add. It can use several OCR passes.
//! The design hides the popup instead of masking the screen. The popup covers the next lines.
//! The design uses a fixed reach instead of paragraph detection. OCR gives no paragraph labels.

use crate::geom::PhysRect;
use crate::text::layout::{self, OcrLine, OcrWord, Orientation};

/// Reach above and below the anchor line, in line thicknesses.
pub const SENTENCE_REACH_LINES: i32 = 6;

const SENTENCE_ENDS: [char; 5] = ['。', '！', '？', '!', '?'];
const SENTENCE_CLOSERS: [char; 4] = ['」', '』', '）', ')'];

/// Return the tiles that a sentence probe reads.
///
/// The function returns no tiles when the anchor has no thickness or lies outside the `bounds`.
/// It makes a perpendicular band of thirteen thicknesses around the anchor center.
/// It reads the full `bounds` on the reading axis.
/// It splits that axis into tiles no longer than `2 * layout::TILE_LEN`.
/// Consecutive tiles overlap by half of the maximum tile length.
/// It clamps every tile to the `bounds`.
/// The sentence probe uses a fixed reach. This limits its cost.
/// It does not use one large tile. OCR accuracy falls when the capture is larger.
///
/// The function uses physical pixels for every coordinate.
pub fn probe_regions(anchor: PhysRect, orientation: Orientation, bounds: PhysRect) -> Vec<PhysRect> {
    let thickness = match orientation {
        Orientation::Horizontal => anchor.h,
        Orientation::Vertical => anchor.w,
    };
    if thickness <= 0 || bounds.w <= 0 || bounds.h <= 0 || anchor.intersection(bounds).is_none() {
        return Vec::new();
    }

    let band_extent = thickness.saturating_mul(SENTENCE_REACH_LINES * 2 + 1);
    let centre = crate::geom::PhysPoint {
        x: anchor.x.saturating_add(anchor.w / 2),
        y: anchor.y.saturating_add(anchor.h / 2),
    };
    let (perpendicular_start, axis_start, axis_end) = match orientation {
        Orientation::Horizontal => (
            centre.y.saturating_sub(band_extent / 2),
            bounds.x,
            bounds.x.saturating_add(bounds.w),
        ),
        Orientation::Vertical => (
            centre.x.saturating_sub(band_extent / 2),
            bounds.y,
            bounds.y.saturating_add(bounds.h),
        ),
    };

    let tile_len = 2 * layout::TILE_LEN;
    let overlap = tile_len / 2;
    let stride = tile_len - overlap;
    if tile_len <= 0 || stride <= 0 || axis_end <= axis_start {
        return Vec::new();
    }

    let mut regions = Vec::new();
    let mut start = axis_start;
    while start < axis_end {
        let remaining = axis_end.saturating_sub(start);
        let len = remaining.min(tile_len);
        let tile = match orientation {
            Orientation::Horizontal => PhysRect {
                x: start,
                y: perpendicular_start,
                w: len,
                h: band_extent,
            },
            Orientation::Vertical => PhysRect {
                x: perpendicular_start,
                y: start,
                w: band_extent,
                h: len,
            },
        };
        if let Some(tile) = layout::clamp_tile(tile, bounds) {
            regions.push(tile);
        }
        if len == remaining {
            break;
        }
        let next = start.saturating_add(stride);
        if next <= start {
            break;
        }
        start = next;
    }
    regions
}

/// Return the sentence that contains the word at `anchor`.
/// The function groups words by thickness and perpendicular center.
/// It removes high-overlap duplicates, preferring a representative that covers half the
/// anchor and then the larger box.
/// It finds the anchor word when its overlap covers at least half of the anchor area.
/// It keeps rows with matching thickness and a half-extent overlap in the same column.
/// It orders rows from top to bottom for horizontal text and from right to left for vertical text.
/// It stops at paragraph gaps wider than 4.5 times the larger adjacent row thickness.
/// It joins words without separators because Japanese does not need inserted spaces.
/// It cuts at sentence terminators and keeps the listed closing brackets.
/// It normalizes the cut before it returns it.
/// It returns `None` when no word covers the anchor or the cut is empty.
/// It does not use paragraph labels because OCR does not provide them.
/// It does not use a fixed character count because sentence lengths vary.
pub fn sentence_at(
    lines: &[OcrLine],
    anchor: PhysRect,
    orientation: Orientation,
) -> Option<String> {
    let anchor_area = i64::from(anchor.w) * i64::from(anchor.h);
    if anchor_area <= 0 {
        return None;
    }
    let mut rows = group_rows(lines, orientation);
    dedupe_rows(&mut rows, orientation, anchor, anchor_area);

    let mut anchor_word = None;
    for row in &rows {
        for word in row {
            if covers_anchor(word, anchor, anchor_area) {
                anchor_word = Some(*word);
                break;
            }
        }
        if anchor_word.is_some() {
            break;
        }
    }
    let anchor_word = anchor_word?;
    let anchor_row_index = rows
        .iter()
        .position(|row| row.iter().any(|word| std::ptr::eq(*word, anchor_word)))?;
    let anchor_thickness = row_thickness(&rows[anchor_row_index], orientation);
    let anchor_extent = row_extent(&rows[anchor_row_index], orientation)?;

    rows.retain(|row| {
        layout::same_size(anchor_thickness, row_thickness(row, orientation))
            && extent_overlap(anchor_extent, row_extent(row, orientation))
    });
    rows.sort_by(|a, b| {
        let ordering = row_perp_centre(a, orientation).cmp(&row_perp_centre(b, orientation));
        match orientation {
            Orientation::Horizontal => ordering,
            Orientation::Vertical => ordering.reverse(),
        }
    });

    let anchor_row_index = rows
        .iter()
        .position(|row| row.iter().any(|word| std::ptr::eq(*word, anchor_word)))?;
    let mut first = anchor_row_index;
    while first > 0 {
        let gap = row_forward_gap(&rows[first - 1], &rows[first], orientation);
        let gap_limit = row_thickness(&rows[first - 1], orientation)
            .max(row_thickness(&rows[first], orientation))
            * layout::WRAP_GAP_HALVES
            / 2;
        if gap > gap_limit {
            break;
        }
        first -= 1;
    }
    let mut last = anchor_row_index;
    while last + 1 < rows.len() {
        let gap = row_forward_gap(&rows[last], &rows[last + 1], orientation);
        let gap_limit = row_thickness(&rows[last], orientation)
            .max(row_thickness(&rows[last + 1], orientation))
            * layout::WRAP_GAP_HALVES
            / 2;
        if gap > gap_limit {
            break;
        }
        last += 1;
    }

    let mut text = String::new();
    let mut anchor_offset = None;
    for row in &rows[first..=last] {
        for word in row {
            if anchor_offset.is_none() && std::ptr::eq(*word, anchor_word) {
                anchor_offset = Some(text.len());
            }
            text.push_str(&word.text);
        }
    }
    let anchor_offset = anchor_offset?;
    let result = layout::normalise(cut_sentence(&text, anchor_offset));
    (!result.trim().is_empty()).then_some(result)
}

fn group_rows(lines: &[OcrLine], orientation: Orientation) -> Vec<Vec<&OcrWord>> {
    let mut rows: Vec<Vec<&OcrWord>> = Vec::new();
    for line in lines {
        for word in &line.words {
            let word_thickness = word_thickness(word, orientation);
            let word_centre = word_perp_centre(word, orientation);
            let existing = rows.iter().position(|row| {
                let row_thickness = row_thickness(row, orientation);
                layout::same_size(row_thickness, word_thickness)
                    && (row_perp_centre(row, orientation) - word_centre).abs()
                        < row_thickness.max(word_thickness) / 2
            });
            if let Some(index) = existing {
                rows[index].push(word);
            } else {
                rows.push(vec![word]);
            }
        }
    }

    for row in &mut rows {
        row.sort_by_key(|word| word_lead(word, orientation));
    }

    let mut split_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let row_thickness = row_thickness(&row, orientation);
        let mut current = Vec::new();
        let mut previous = None;
        for word in row {
            if let Some(previous) = previous {
                let gap = word_lead(word, orientation) - word_trail(previous, orientation);
                if gap > row_thickness * 2 {
                    if !current.is_empty() {
                        split_rows.push(current);
                    }
                    current = Vec::new();
                }
            }
            previous = Some(word);
            current.push(word);
        }
        if !current.is_empty() {
            split_rows.push(current);
        }
    }
    split_rows
}

fn dedupe_rows(
    rows: &mut [Vec<&OcrWord>],
    orientation: Orientation,
    anchor: PhysRect,
    anchor_area: i64,
) {
    for row in rows {
        let mut unique: Vec<&OcrWord> = Vec::with_capacity(row.len());
        for word in row.iter().copied() {
            if let Some(index) = unique
                .iter()
                .position(|known| same_physical_word(known, word))
            {
                let known = unique[index];
                let word_covers_anchor = covers_anchor(word, anchor, anchor_area);
                let known_covers_anchor = covers_anchor(known, anchor, anchor_area);
                if (word_covers_anchor && !known_covers_anchor)
                    || (word_covers_anchor == known_covers_anchor
                        && word_area(word) > word_area(known))
                {
                    unique[index] = word;
                }
            } else {
                unique.push(word);
            }
        }
        unique.sort_by_key(|word| word_lead(word, orientation));
        *row = unique;
    }
}

fn covers_anchor(word: &OcrWord, anchor: PhysRect, anchor_area: i64) -> bool {
    let Some(overlap) = word.rect.intersection(anchor) else {
        return false;
    };
    let overlap_area = i64::from(overlap.w) * i64::from(overlap.h);
    overlap_area * 2 >= anchor_area
}

fn same_physical_word(a: &OcrWord, b: &OcrWord) -> bool {
    let Some(overlap) = a.rect.intersection(b.rect) else {
        return false;
    };
    let overlap_area = i64::from(overlap.w) * i64::from(overlap.h);
    overlap_area * 2 >= word_area(a).min(word_area(b))
}

fn word_area(word: &OcrWord) -> i64 {
    i64::from(word.rect.w) * i64::from(word.rect.h)
}

fn word_perp_centre(word: &OcrWord, orientation: Orientation) -> i32 {
    let centre = word.rect.center();
    match orientation {
        Orientation::Horizontal => centre.y,
        Orientation::Vertical => centre.x,
    }
}

fn word_thickness(word: &OcrWord, orientation: Orientation) -> i32 {
    match orientation {
        Orientation::Horizontal => word.rect.h,
        Orientation::Vertical => word.rect.w,
    }
}

fn word_lead(word: &OcrWord, orientation: Orientation) -> i32 {
    match orientation {
        Orientation::Horizontal => word.rect.x,
        Orientation::Vertical => word.rect.y,
    }
}

fn word_trail(word: &OcrWord, orientation: Orientation) -> i32 {
    match orientation {
        Orientation::Horizontal => word.rect.x + word.rect.w,
        Orientation::Vertical => word.rect.y + word.rect.h,
    }
}

fn row_perp_centre(row: &[&OcrWord], orientation: Orientation) -> i32 {
    let sum: i32 = row.iter().map(|word| word_perp_centre(word, orientation)).sum();
    sum / row.len() as i32
}

fn row_thickness(row: &[&OcrWord], orientation: Orientation) -> i32 {
    let sum: i32 = row.iter().map(|word| word_thickness(word, orientation)).sum();
    sum / row.len() as i32
}

fn row_extent(row: &[&OcrWord], orientation: Orientation) -> Option<(i32, i32)> {
    let first = row.first()?;
    let mut start = word_lead(first, orientation);
    let mut end = word_trail(first, orientation);
    for word in &row[1..] {
        start = start.min(word_lead(word, orientation));
        end = end.max(word_trail(word, orientation));
    }
    Some((start, end))
}

fn extent_overlap(a: (i32, i32), b: Option<(i32, i32)>) -> bool {
    let Some(b) = b else {
        return false;
    };
    let shorter = (a.1 - a.0).min(b.1 - b.0);
    let overlap = (a.1.min(b.1) - a.0.max(b.0)).max(0);
    shorter > 0 && overlap * 2 >= shorter
}

fn row_forward_gap(
    previous: &[&OcrWord],
    next: &[&OcrWord],
    orientation: Orientation,
) -> i32 {
    match orientation {
        Orientation::Horizontal => row_perp_centre(next, orientation) - row_perp_centre(previous, orientation),
        Orientation::Vertical => row_perp_centre(previous, orientation) - row_perp_centre(next, orientation),
    }
}

/// Return the sentence that holds the character at `anchor_offset`.
///
/// The cut starts after the last terminator before the anchor. It keeps an opening
/// bracket directly before that start. It ends after the first terminator at or
/// after the anchor and any closing brackets behind it. The hover-time line and
/// the add-time probe both use this cut. One line that holds three sentences
/// gives one sentence on both paths.
pub fn cut_sentence(text: &str, anchor_offset: usize) -> &str {
    let (start, end) = sentence_bounds(text, anchor_offset);
    &text[start..end]
}

fn sentence_bounds(text: &str, anchor_offset: usize) -> (usize, usize) {
    let mut start = 0;
    let mut previous_end = None;
    for (index, character) in text.char_indices() {
        if index >= anchor_offset {
            break;
        }
        if SENTENCE_ENDS.contains(&character) {
            previous_end = Some(index + character.len_utf8());
        }
    }
    if let Some(mut end) = previous_end {
        end = skip_closers(text, end);
        start = end;
    }

    if start > 0 {
        if let Some((index, character)) = text[..start].char_indices().next_back() {
            if matches!(character, '「' | '『') {
                start = index;
            }
        }
    }

    let mut end = text.len();
    for (index, character) in text.char_indices() {
        if index < anchor_offset {
            continue;
        }
        if SENTENCE_ENDS.contains(&character) {
            end = skip_closers(text, index + character.len_utf8());
            break;
        }
    }
    (start, end)
}

/// Return the end after any closing brackets that follow a terminator.
fn skip_closers(text: &str, end: usize) -> usize {
    let tail = &text[end..];
    let kept = tail.chars().take_while(|c| SENTENCE_CLOSERS.contains(c)).map(char::len_utf8).sum::<usize>();
    end + kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x: i32, y: i32, ww: i32, h: i32) -> OcrWord {
        OcrWord { text: text.to_string(), rect: PhysRect { x, y, w: ww, h } }
    }

    fn p(x: i32, y: i32) -> crate::geom::PhysPoint {
        crate::geom::PhysPoint { x, y }
    }

    fn row(text: &str, x: i32, y: i32, ww: i32, h: i32) -> OcrLine {
        OcrLine {
            words: text
                .chars()
                .enumerate()
                .map(|(index, character)| w(&character.to_string(), x + index as i32 * ww, y, ww, h))
                .collect(),
        }
    }

    #[test]
    fn probe_regions_centers_a_thirteen_line_band_on_the_anchor() {
        let anchor = PhysRect { x: 980, y: 480, w: 40, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1440 };
        assert_eq!(
            vec![
                PhysRect { x: 0, y: 240, w: 1000, h: 520 },
                PhysRect { x: 500, y: 240, w: 1000, h: 520 },
                PhysRect { x: 1000, y: 240, w: 1000, h: 520 },
                PhysRect { x: 1500, y: 240, w: 1000, h: 520 },
                PhysRect { x: 2000, y: 240, w: 560, h: 520 },
            ],
            probe_regions(anchor, Orientation::Horizontal, bounds)
        );
    }

    #[test]
    fn probe_regions_reads_columns_for_vertical_text() {
        let anchor = PhysRect { x: 500, y: 180, w: 40, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 1440, h: 900 };
        assert_eq!(
            vec![PhysRect { x: 260, y: 0, w: 520, h: 900 }],
            probe_regions(anchor, Orientation::Vertical, bounds)
        );
    }

    #[test]
    fn probe_regions_is_empty_for_a_flat_anchor() {
        let anchor = PhysRect { x: 980, y: 480, w: 40, h: 0 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 1440 };
        assert!(probe_regions(anchor, Orientation::Horizontal, bounds).is_empty());
    }

    #[test]
    fn probe_regions_clamps_at_the_output_edge() {
        let anchor = PhysRect { x: 980, y: 0, w: 40, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 2560, h: 400 };
        assert_eq!(
            Some(PhysRect { x: 0, y: 0, w: 1000, h: 280 }),
            probe_regions(anchor, Orientation::Horizontal, bounds).first().copied()
        );
    }

    #[test]
    fn sentence_at_joins_three_rows_and_cuts_at_the_terminators() {
        let lines = vec![
            row("前の文。今日は", 100, 100, 40, 40),
            row("いい天気で", 100, 160, 40, 40),
            row("すね。次の文", 100, 220, 40, 40),
        ];
        let anchor = PhysRect { x: 180, y: 160, w: 40, h: 40 };
        assert_eq!(Some("今日はいい天気ですね。".to_string()), sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn sentence_at_keeps_a_closing_bracket_after_the_terminator() {
        let lines = vec![row("「行こう。」次", 100, 100, 40, 40)];
        let anchor_origin = p(140, 100);
        let anchor = PhysRect { x: anchor_origin.x, y: anchor_origin.y, w: 40, h: 40 };
        assert_eq!(Some("「行こう。」".to_string()), sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn sentence_at_stops_at_a_paragraph_gap() {
        let lines = vec![
            row("前の文。今日は", 100, 100, 40, 40),
            row("いい天気で", 100, 160, 40, 40),
            row("すね。次の文", 100, 400, 40, 40),
        ];
        let anchor = PhysRect { x: 180, y: 160, w: 40, h: 40 };
        assert_eq!(Some("今日はいい天気で".to_string()), sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn sentence_at_skips_ruby_between_rows() {
        let lines = vec![
            row("前の文。今日は", 100, 100, 40, 40),
            row("るび", 100, 145, 40, 12),
            row("いい天気。", 100, 160, 40, 40),
        ];
        let anchor = PhysRect { x: 180, y: 160, w: 40, h: 40 };
        assert_eq!(Some("今日はいい天気。".to_string()), sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn sentence_at_ignores_a_side_column() {
        let mut first = row("前の文。今日は", 100, 100, 40, 40);
        first.words.push(w("別", 900, 100, 40, 40));
        first.words.push(w("列", 940, 100, 40, 40));
        let lines = vec![first, row("いい天気。", 100, 160, 40, 40)];
        let anchor = PhysRect { x: 180, y: 160, w: 40, h: 40 };
        assert_eq!(Some("今日はいい天気。".to_string()), sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn sentence_at_reads_vertical_columns_right_to_left() {
        let lines = vec![
            OcrLine { words: vec![w("右", 500, 100, 40, 40)] },
            OcrLine { words: vec![w("中", 440, 100, 40, 40)] },
            OcrLine { words: vec![w("左。", 380, 100, 40, 40)] },
        ];
        let anchor = PhysRect { x: 440, y: 100, w: 40, h: 40 };
        assert_eq!(Some("右中左。".to_string()), sentence_at(&lines, anchor, Orientation::Vertical));
    }

    #[test]
    fn sentence_at_is_none_when_no_word_covers_the_anchor() {
        let lines = vec![row("文字", 100, 100, 40, 40)];
        let origin = p(500, 500);
        let anchor = PhysRect { x: origin.x, y: origin.y, w: 40, h: 40 };
        assert_eq!(None, sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn sentence_at_normalises_the_cut() {
        let lines = vec![row("カ-。", 100, 100, 40, 40)];
        let anchor = PhysRect { x: 100, y: 100, w: 40, h: 40 };
        assert_eq!(Some("カー。".to_string()), sentence_at(&lines, anchor, Orientation::Horizontal));
    }

    #[test]
    fn probe_seam_keeps_an_ordinary_word_in_at_least_one_trimmed_tile() {
        let anchor = PhysRect { x: 958, y: 100, w: 40, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 1960, h: 400 };
        let word = w("語", 958, 100, 40, 40);

        let retained = probe_regions(anchor, Orientation::Horizontal, bounds)
            .into_iter()
            .any(|tile| {
                let mut lines = vec![OcrLine { words: vec![word.clone()] }];
                layout::trim_probe_edges(
                    &mut lines,
                    tile,
                    bounds,
                    Orientation::Horizontal,
                );
                lines.iter().any(|line| !line.words.is_empty())
            });

        assert!(retained);
    }

    #[test]
    fn sentence_at_deduplicates_a_narrow_word_overlapping_two_tiles() {
        let anchor = PhysRect { x: 970, y: 100, w: 40, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 1960, h: 400 };
        let mut all_lines = Vec::new();

        for tile in probe_regions(anchor, Orientation::Horizontal, bounds) {
            let mut lines = vec![OcrLine {
                words: vec![w("語", 970, 100, 20, 40)],
            }];
            layout::trim_probe_edges(
                &mut lines,
                tile,
                bounds,
                Orientation::Horizontal,
            );
            all_lines.extend(lines);
        }

        assert_eq!(
            Some("語".to_string()),
            sentence_at(&all_lines, anchor, Orientation::Horizontal)
        );
    }

    #[test]
    fn sentence_at_keeps_a_thicker_continuation_at_a_three_and_a_half_thickness_gap() {
        let lines = vec![
            OcrLine {
                words: vec![w("前", 100, 100, 40, 40)],
            },
            OcrLine {
                words: vec![w("続。", 100, 300, 120, 60)],
            },
        ];
        let anchor = PhysRect { x: 100, y: 100, w: 40, h: 40 };

        assert_eq!(
            Some("前続。".to_string()),
            sentence_at(&lines, anchor, Orientation::Horizontal)
        );
    }

    #[test]
    fn sentence_at_keeps_a_wide_repeated_word_crossing_an_internal_tile_seam_once() {
        let anchor = PhysRect { x: 970, y: 100, w: 40, h: 40 };
        let bounds = PhysRect { x: 0, y: 0, w: 1960, h: 400 };
        let words = vec![
            w("語", 910, 100, 40, 40),
            w("語語", 950, 100, 80, 40),
            w("。", 1030, 100, 40, 40),
        ];
        let mut all_lines = Vec::new();

        for tile in probe_regions(anchor, Orientation::Horizontal, bounds) {
            let mut lines = vec![OcrLine { words: words.clone() }];
            layout::trim_probe_edges(
                &mut lines,
                tile,
                bounds,
                Orientation::Horizontal,
            );
            all_lines.extend(lines);
        }

        assert_eq!(
            Some("語語語。".to_string()),
            sentence_at(&all_lines, anchor, Orientation::Horizontal)
        );
    }

    #[test]
    fn sentence_at_keeps_an_anchor_covering_word_from_a_jittered_duplicate_cluster() {
        let lines = vec![OcrLine {
            words: vec![
                w("語", 60, 100, 40, 40),
                w("語", 100, 100, 60, 40),
                w("語", 130, 100, 100, 40),
                w("。", 230, 100, 40, 40),
            ],
        }];
        let anchor = PhysRect { x: 100, y: 100, w: 40, h: 40 };

        assert_eq!(
            Some("語語。".to_string()),
            sentence_at(&lines, anchor, Orientation::Horizontal)
        );
    }
}

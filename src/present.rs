//! What the popup shows.

use crate::geom::PhysRect;
use crate::lookup::model::Hit;
use crate::text::layout::union_chars;
use crate::text::TextSpan;

/// One hover's popup content.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    pub top: Option<Card>,
    pub collapsed: Vec<CollapsedRow>,
}

/// The top group, in full.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub written: Option<String>,
    pub reading: Option<String>,
    pub pos: Vec<String>,
    pub freq: Option<i64>,
    /// In display order.
    pub blocks: Vec<GlossBlock>,
    /// Input chars, not bytes.
    pub match_len: usize,
}

/// One dict's glosses, flat.
#[derive(Debug, Clone, PartialEq)]
pub struct GlossBlock {
    pub dict_name: String,
    pub glosses: Vec<String>,
}

/// A non-top group, one line.
#[derive(Debug, Clone, PartialEq)]
pub struct CollapsedRow {
    pub written: Option<String>,
    pub reading: Option<String>,
    /// First gloss, truncated.
    pub summary: String,
}

/// A dictionary's identity.
#[derive(Debug, Clone, PartialEq)]
pub struct DictInfo {
    pub dict_id: i64,
    pub name: String,
}

/// Presentation knobs.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentConfig {
    /// Name substrings, in order.
    pub dict_order: Vec<String>,
    /// Summary cap, in chars.
    pub summary_chars: usize,
}

/// One headword group.
struct Group<'a> {
    written: Option<String>,
    reading: Option<String>,
    hits: Vec<&'a Hit>,
}

/// `hits` must be rank-ordered.
pub fn build(hits: &[Hit], dicts: &[DictInfo], cfg: &PresentConfig) -> Presentation {
    let mut groups: Vec<Group> = Vec::new();
    for hit in hits {
        match groups
            .iter_mut()
            .find(|g| g.written == hit.written && g.reading == hit.reading)
        {
            Some(g) => g.hits.push(hit),
            None => groups.push(Group {
                written: hit.written.clone(),
                reading: hit.reading.clone(),
                hits: vec![hit],
            }),
        }
    }

    let mut groups = groups.into_iter();
    let top = groups.next().map(|g| card_from_group(g, dicts, cfg));
    let collapsed = groups.map(|g| collapsed_from_group(g, dicts, cfg)).collect();

    Presentation { top, collapsed }
}

/// Ink-to-outline pad, in px.
pub const HIGHLIGHT_PAD: i32 = 3;

/// Indexed from the cursor.
pub fn match_highlight(span: &TextSpan, top: Option<&Card>) -> Option<PhysRect> {
    let top = top?;
    let after_cursor = span.text.get(span.cursor_byte_offset..)?;
    let skipped = after_cursor.len() - after_cursor.trim_start().len();
    let from = span.text[..span.cursor_byte_offset + skipped].chars().count();
    union_chars(&span.geom, from, top.match_len, HIGHLIGHT_PAD)
}

fn card_from_group(group: Group, dicts: &[DictInfo], cfg: &PresentConfig) -> Card {
    // Pre-ranked: first is best.
    let best = group.hits[0];
    Card {
        written: group.written,
        reading: group.reading,
        pos: best.entry.senses.first().map(|s| s.pos.clone()).unwrap_or_default(),
        freq: best.freq,
        blocks: ordered_blocks(&group.hits, dicts, cfg),
        match_len: best.match_len,
    }
}

fn collapsed_from_group(group: Group, dicts: &[DictInfo], cfg: &PresentConfig) -> CollapsedRow {
    let blocks = ordered_blocks(&group.hits, dicts, cfg);
    let first_gloss = blocks
        .first()
        .and_then(|b| b.glosses.first())
        .map(String::as_str)
        .unwrap_or("");
    CollapsedRow {
        written: group.written,
        reading: group.reading,
        summary: truncate_chars(first_gloss, cfg.summary_chars),
    }
}

/// Per hit, not per dict.
fn ordered_blocks(hits: &[&Hit], dicts: &[DictInfo], cfg: &PresentConfig) -> Vec<GlossBlock> {
    let mut ranked: Vec<(usize, i64, GlossBlock)> = hits
        .iter()
        .map(|hit| {
            let dict_id = hit.entry.dict_id;
            let dict_name = dict_name_for(dict_id, dicts);
            let rank = dict_order_rank(&dict_name, &cfg.dict_order).unwrap_or(usize::MAX);
            let glosses = hit.entry.senses.iter().flat_map(|s| s.glosses.clone()).collect();
            (rank, dict_id, GlossBlock { dict_name, glosses })
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, block)| block).collect()
}

/// Unknown id: named by id.
fn dict_name_for(dict_id: i64, dicts: &[DictInfo]) -> String {
    dicts
        .iter()
        .find(|d| d.dict_id == dict_id)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| format!("dict {dict_id}"))
}

/// `None` sorts last.
pub fn dict_order_rank(dict_name: &str, dict_order: &[String]) -> Option<usize> {
    let lower = dict_name.to_lowercase();
    // Blank matches everything.
    dict_order
        .iter()
        .position(|s| !s.trim().is_empty() && lower.contains(&s.to_lowercase()))
}

/// Chars, not bytes.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::{Entry, Hit, Sense};

    fn dicts() -> Vec<DictInfo> {
        vec![
            DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ]
    }

    fn cfg() -> PresentConfig {
        PresentConfig {
            dict_order: vec!["大辞林".into(), "Jitendex".into()],
            summary_chars: 40,
        }
    }

    fn hit(written: &str, reading: &str, dict_id: i64, gloss: &str) -> Hit {
        Hit {
            written: Some(written.to_string()),
            reading: Some(reading.to_string()),
            match_len: 2,
            freq: Some(365),
            score: 7.7,
            process: vec![],
            entry: Entry {
                entry_id: dict_id * 100,
                dict_id,
                senses: vec![Sense {
                    glosses: vec![gloss.to_string()],
                    pos: vec!["noun".into()],
                    misc: vec![],
                }],
            },
        }
    }

    #[test]
    fn empty_hits_yield_nothing() {
        let p = build(&[], &dicts(), &cfg());
        assert!(p.top.is_none());
        assert!(p.collapsed.is_empty());
    }

    #[test]
    fn a_single_hit_becomes_the_top_card_with_no_rows() {
        let p = build(&[hit("昨日", "きのう", 1, "yesterday")], &dicts(), &cfg());
        assert_eq!(Some("昨日".to_string()), p.top.as_ref().unwrap().written);
        assert!(p.collapsed.is_empty());
    }

    #[test]
    fn same_word_from_two_dictionaries_merges_into_one_card() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        assert!(p.collapsed.is_empty(), "the two must merge, not collapse");
        assert_eq!(2, p.top.as_ref().unwrap().blocks.len());
    }

    #[test]
    fn daijirin_orders_before_jitendex_regardless_of_dict_id() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        let blocks = &p.top.as_ref().unwrap().blocks;
        assert!(blocks[0].dict_name.contains("大辞林"), "got {:?}", blocks[0].dict_name);
        assert!(blocks[1].dict_name.contains("Jitendex"));
    }

    #[test]
    fn a_different_reading_is_a_different_card() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨日", "さくじつ", 1, "yesterday"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        assert_eq!(1, p.collapsed.len());
        assert_eq!(Some("さくじつ".to_string()), p.collapsed[0].reading);
    }

    #[test]
    fn group_order_follows_the_best_hits_rank() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "last (year)"),
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        assert_eq!(Some("昨日".to_string()), p.top.as_ref().unwrap().written);
        assert_eq!(1, p.collapsed.len());
        assert_eq!(Some("昨".to_string()), p.collapsed[0].written);
    }

    #[test]
    fn summary_truncates_on_a_char_boundary_and_marks_the_cut() {
        let long = "あ".repeat(80);
        let hits = vec![
            hit("A", "あ", 1, "short"),
            hit("B", "い", 1, &long),
        ];
        let p = build(&hits, &dicts(), &cfg());
        let s = &p.collapsed[0].summary;
        assert!(s.ends_with('…'));
        assert_eq!(41, s.chars().count(), "40 chars plus the ellipsis");
    }

    #[test]
    fn a_short_summary_is_not_marked() {
        let hits = vec![hit("A", "あ", 1, "short"), hit("B", "い", 1, "also short")];
        let p = build(&hits, &dicts(), &cfg());
        assert_eq!("also short", p.collapsed[0].summary);
    }

    #[test]
    fn a_blank_order_entry_ranks_nothing() {
        let blank = vec![String::new()];
        assert_eq!(None, dict_order_rank("Jitendex.org", &blank));
        let with_blank = vec![String::new(), "Jitendex".to_string()];
        assert_eq!(Some(1), dict_order_rank("Jitendex.org", &with_blank));
    }

    #[test]
    fn an_unknown_dictionary_still_produces_a_block() {
        let hits = vec![hit("猫", "ねこ", 99, "cat")];
        let p = build(&hits, &dicts(), &cfg());
        assert_eq!(1, p.top.as_ref().unwrap().blocks.len());
    }

    fn bare_card(match_len: usize) -> Card {
        Card {
            written: None,
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len,
        }
    }

    /// 30px boxes from x=100.
    fn span_of(text: &str, cursor_byte_offset: usize) -> TextSpan {
        let geom = (0..text.chars().count())
            .map(|i| crate::text::layout::TextGeom {
                char_count: 1,
                rect: PhysRect { x: 100 + 30 * i as i32, y: 200, w: 30, h: 40 },
            })
            .collect();
        TextSpan {
            text: text.to_string(),
            cursor_byte_offset,
            anchor: PhysRect { x: 100, y: 200, w: 30, h: 40 },
            geom,
        }
    }

    #[test]
    fn the_highlight_starts_at_the_hovered_character_not_the_line_start() {
        let span = span_of("その可哀想", "その".len());
        let r = match_highlight(&span, Some(&bare_card(3))).unwrap();
        assert_eq!(PhysRect { x: 157, y: 197, w: 96, h: 46 }, r,
                   "three 30px boxes from x=160, padded 3px");
    }

    #[test]
    fn leading_whitespace_at_the_cursor_is_skipped_by_the_highlight() {
        let span = span_of("あ 猫", "あ".len());
        let r = match_highlight(&span, Some(&bare_card(1))).unwrap();
        assert_eq!(160, r.x + HIGHLIGHT_PAD, "the box must start at 猫, not at the space");
    }

    /// The tiled path has none.
    #[test]
    fn a_span_without_geometry_draws_no_highlight() {
        let mut span = span_of("可哀想", 0);
        span.geom.clear();
        assert_eq!(None, match_highlight(&span, Some(&bare_card(3))));
    }

    #[test]
    fn no_top_card_draws_no_highlight() {
        assert_eq!(None, match_highlight(&span_of("可哀想", 0), None));
    }

    #[test]
    fn a_match_longer_than_the_known_geometry_boxes_what_is_known() {
        let span = span_of("猫", 0);
        let r = match_highlight(&span, Some(&bare_card(9))).unwrap();
        assert_eq!(PhysRect { x: 97, y: 197, w: 36, h: 46 }, r);
    }

    #[test]
    fn the_card_carries_its_best_hits_match_len() {
        let mut long = hit("可哀想", "かわいそう", 1, "pitiable");
        long.match_len = 3;
        let mut short = hit("可哀想", "かわいそう", 2, "気の毒なさま。");
        short.match_len = 3;
        let p = build(&[long, short], &dicts(), &cfg());
        assert_eq!(3, p.top.as_ref().unwrap().match_len);
    }
}

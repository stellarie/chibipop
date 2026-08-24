//! What the popup shows.

use std::collections::HashSet;

use crate::geom::PhysRect;
use crate::lookup::model::Hit;
use crate::text::layout::union_chars;
use crate::text::TextSpan;

/// One hover's popup content.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    pub top: Option<Card>,
    pub collapsed: Vec<CollapsedRow>,
    /// Every group as a full card.
    pub all_cards: Vec<Card>,
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
    /// Same glosses, HTML-formatted. Empty wherever `glosses` is empty.
    pub glosses_html: Vec<String>,
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

/// Anki state for this popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnkiPopupState {
    pub dupes: HashSet<String>,
    pub added: HashSet<String>,
    pub enabled: bool,
    pub adding: bool,
    /// Dupe check in flight.
    pub checking: bool,
    /// AnkiConnect reachable.
    pub connected: bool,
    /// Last add-note failed.
    pub failed: bool,
}

impl AnkiPopupState {
    /// Disabled, no markers.
    pub fn disabled() -> Self {
        Self {
            dupes: HashSet::new(),
            added: HashSet::new(),
            enabled: false,
            adding: false,
            checking: false,
            connected: false,
            failed: false,
        }
    }

    /// A brand-new popup's state:
    /// checking iff Anki is on.
    pub fn fresh(enabled: bool) -> Self {
        Self {
            enabled,
            checking: enabled,
            connected: enabled,
            ..Self::disabled()
        }
    }
}

/// Presentation knobs.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentConfig {
    /// Name substrings, in order.
    pub dict_order: Vec<String>,
    /// Summary cap, in chars.
    pub summary_chars: usize,
    /// Order also excludes?
    pub restrict_to_order: bool,
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
        let dict_name = dict_name_for(hit.entry.dict_id, dicts);
        if !keeps_dict(&dict_name, &cfg.dict_order, cfg.restrict_to_order) {
            continue;
        }
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

    let all_cards: Vec<Card> = groups
        .into_iter()
        .map(|g| card_from_group(g, dicts, cfg))
        .collect();
    let top = all_cards.first().cloned();
    let collapsed = all_cards
        .iter()
        .skip(1)
        .map(|c| collapsed_from_card(c, cfg.summary_chars))
        .collect();

    Presentation { top, collapsed, all_cards }
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

/// Card back to a one-liner.
pub fn collapsed_from_card(card: &Card, summary_chars: usize) -> CollapsedRow {
    let first_gloss = card
        .blocks
        .first()
        .and_then(|b| b.glosses.first())
        .map(String::as_str)
        .unwrap_or("");
    CollapsedRow {
        written: card.written.clone(),
        reading: card.reading.clone(),
        summary: truncate_chars(first_gloss, summary_chars),
    }
}

/// Promotes a collapsed entry.
pub fn swap_top(p: &mut Presentation, collapsed_index: usize, summary_chars: usize) {
    let card_index = collapsed_index + 1;
    if card_index >= p.all_cards.len() {
        return;
    }
    p.all_cards.swap(0, card_index);
    p.top = Some(p.all_cards[0].clone());
    p.collapsed = p
        .all_cards
        .iter()
        .skip(1)
        .map(|c| collapsed_from_card(c, summary_chars))
        .collect();
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
            let glosses_html =
                hit.entry.senses.iter().flat_map(|s| s.glosses_html.clone()).collect();
            (rank, dict_id, GlossBlock { dict_name, glosses, glosses_html })
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

/// Does the list name one?
pub fn any_listed<'a>(names: impl IntoIterator<Item = &'a str>, list: &[String]) -> bool {
    names.into_iter().any(|n| dict_order_rank(n, list).is_some())
}

/// Searched for this language?
pub fn keeps_dict(dict_name: &str, dict_order: &[String], restrict: bool) -> bool {
    if !restrict || dict_order.is_empty() {
        return true;
    }
    dict_order_rank(dict_name, dict_order).is_some()
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
            restrict_to_order: false,
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
                    glosses_html: vec![],
                    pos: vec!["noun".into()],
                    misc: vec![],
                }],
            },
        }
    }

    #[test]
    fn restrict_drops_a_dictionary_outside_the_order() {
        assert!(!keeps_dict("Jitendex.org", &["大辞林".to_string()], true));
        assert!(keeps_dict("大辞林　第四版", &["大辞林".to_string()], true));
    }

    #[test]
    fn without_restrict_an_unranked_dictionary_is_kept() {
        assert!(keeps_dict("Jitendex.org", &["大辞林".to_string()], false));
    }

    #[test]
    fn restrict_with_an_empty_order_keeps_everything() {
        assert!(keeps_dict("Jitendex.org", &[], true));
    }

    #[test]
    fn an_excluded_dictionary_contributes_no_card() {
        let hits = vec![hit("猫", "ねこ", 1, "cat"), hit("犬", "いぬ", 2, "dog")];
        let cfg = PresentConfig {
            dict_order: vec!["大辞林".to_string()],
            summary_chars: 40,
            restrict_to_order: true,
        };
        let p = build(&hits, &dicts(), &cfg);
        assert_eq!(1, p.all_cards.len(), "the excluded dictionary makes no card");
        assert!(p.all_cards[0].blocks.iter().all(|b| b.dict_name.contains("大辞林")));
        assert!(!p.all_cards[0].blocks.is_empty(), "and the card is not hollow");
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

    /// One predicate, two readers.
    #[test]
    fn any_listed_answers_for_the_whole_library() {
        let all = dicts();
        let names = || all.iter().map(|d| d.name.as_str());
        assert!(any_listed(names(), &["大辞林".to_string()]));
        assert!(!any_listed(names(), &["Daijirin".to_string()]));
        assert!(!any_listed(names(), &[]));
        assert!(!any_listed(std::iter::empty(), &["大辞林".to_string()]));
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

    #[test]
    fn build_populates_all_cards_for_every_group() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "last (year)"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        assert_eq!(2, p.all_cards.len());
        assert_eq!(Some("昨日".to_string()), p.all_cards[0].written);
        assert_eq!(Some("昨".to_string()), p.all_cards[1].written);
    }

    #[test]
    fn collapsed_from_card_takes_the_first_gloss() {
        let card = Card {
            written: Some("猫".into()),
            reading: Some("ねこ".into()),
            pos: vec!["noun".into()],
            freq: Some(100),
            blocks: vec![GlossBlock {
                dict_name: "Test".into(),
                glosses: vec!["cat".into(), "feline".into()],
                glosses_html: vec![],
            }],
            match_len: 1,
        };
        let row = collapsed_from_card(&card, 40);
        assert_eq!(Some("猫".to_string()), row.written);
        assert_eq!(Some("ねこ".to_string()), row.reading);
        assert_eq!("cat", row.summary);
    }

    #[test]
    fn collapsed_from_card_truncates_long_glosses() {
        let long = "あ".repeat(80);
        let card = Card {
            written: None,
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![GlossBlock {
                dict_name: "D".into(),
                glosses: vec![long],
                glosses_html: vec![],
            }],
            match_len: 1,
        };
        let row = collapsed_from_card(&card, 40);
        assert!(row.summary.ends_with('…'));
        assert_eq!(41, row.summary.chars().count());
    }

    #[test]
    fn collapsed_from_card_with_no_blocks_yields_an_empty_summary() {
        let card = bare_card(1);
        let row = collapsed_from_card(&card, 40);
        assert_eq!("", row.summary);
    }

    #[test]
    fn swap_top_promotes_the_selected_entry() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "last (year)"),
            hit("日", "にち", 1, "day"),
        ];
        let mut p = build(&hits, &dicts(), &cfg());
        assert_eq!(Some("昨日".to_string()), p.top.as_ref().unwrap().written);

        swap_top(&mut p, 0, 40);
        assert_eq!(Some("昨".to_string()), p.top.as_ref().unwrap().written);
        assert_eq!(2, p.collapsed.len());
        assert_eq!(Some("昨日".to_string()), p.collapsed[0].written);
        assert_eq!(Some("日".to_string()), p.collapsed[1].written);
    }

    #[test]
    fn swap_top_out_of_range_is_a_no_op() {
        let hits = vec![hit("猫", "ねこ", 1, "cat")];
        let mut p = build(&hits, &dicts(), &cfg());
        let before = p.clone();
        swap_top(&mut p, 0, 40);
        assert_eq!(before, p);
    }

    #[test]
    fn swap_top_preserves_all_cards_content() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "last (year)"),
        ];
        let mut p = build(&hits, &dicts(), &cfg());
        let originals: Vec<String> = p
            .all_cards
            .iter()
            .filter_map(|c| c.written.clone())
            .collect();

        swap_top(&mut p, 0, 40);

        let mut after: Vec<String> = p
            .all_cards
            .iter()
            .filter_map(|c| c.written.clone())
            .collect();
        after.sort();
        let mut expected = originals;
        expected.sort();
        assert_eq!(expected, after);
    }

    #[test]
    fn swap_top_then_swap_back_restores_the_original() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "last (year)"),
        ];
        let mut p = build(&hits, &dicts(), &cfg());
        let original_top = p.top.clone();

        swap_top(&mut p, 0, 40);
        assert_ne!(original_top, p.top);
        swap_top(&mut p, 0, 40);
        assert_eq!(original_top, p.top);
    }
}

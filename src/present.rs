//! What the popup shows.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use crate::dict::gloss::GlossDoc;
use crate::dict::media::Intrinsic;

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
    /// The OCR line; set by worker.rs.
    pub sentence: Option<String>,
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

/// One dict's contribution to a card, plus the trees it came from.
///
/// One dictionary, one block - not one matched term-bank row, one block.
/// The census found 6 220 大辞林 headwords with more than one row and a
/// worst case of eleven, so the panel used to repeat that dictionary's
/// name up to eleven times with one gloss under each.
#[derive(Debug, Clone, PartialEq)]
pub struct GlossBlock {
    pub dict_name: String,
    /// The dictionary's own row id, which is what identifies it - the name
    /// is what a reader sees and two libraries can spell it differently.
    /// Half of the stable identity a scene element carries (stories 45 and
    /// 46); the other half is the row's `entry_id` and the node path inside
    /// its tree.
    pub dict_id: i64,
    /// One per matched term-bank row, in the order the rows ranked.
    pub entries: Vec<GlossEntry>,
}

/// One matched term-bank row.
#[derive(Debug, Clone, PartialEq)]
pub struct GlossEntry {
    /// The Entry this row is, as the database numbers it.
    ///
    /// Carried so that a scene element built from this row names the row it
    /// came from and not just the text it drew: "sense 3 of 大辞林" rather
    /// than a character range. [`NO_ROW`] when no stored row is behind it.
    pub entry_id: i64,
    /// The plain-text render, one string per glossary item. Precomputed
    /// because `layout::scene` runs per frame and the panel needs a string.
    pub glosses: Vec<String>,
    /// This row's part-of-speech set, ready to print: numeric tags dropped,
    /// and **empty when the row above already printed the same set**. Not
    /// "this row has no tags" - a reader scanning eleven 大辞林 rows wants
    /// the tags where they change, and Yomitan and Hoshi Reader both dedupe
    /// them the same way.
    pub tags: Vec<String>,
    /// The parsed tree the glosses were rendered from, shared with the
    /// parsed-tree cache. Every other view of this gloss is a renderer over
    /// it - the Anki HTML field today, the popup scene once ticket 08 lands -
    /// so the card and the panel cannot drift apart again.
    pub doc: Arc<GlossDoc>,
    /// The recorded size of every image asset this row's tree names and the
    /// media store has bytes for, by the `path` the node declared.
    ///
    /// Carried rather than looked up while the panel is laid out:
    /// `layout::scene` runs with a measurer and no database (ADR-0004), and
    /// this is what lets it resolve an image's rect without decoding a pixel
    /// (`lookup::model::Entry::media`). An absent path is what makes the
    /// `alt`-text fallback fire.
    pub media: Vec<(String, Intrinsic)>,
}

/// The id content with no database row behind it carries.
///
/// SQLite numbers rows from one, so zero names no dictionary and no
/// term-bank row. What a demo, a geometry fixture, or a test builds: its
/// scene elements are still addressable *within their own tree*, and
/// identify no stored Entry - which is the truth about them, where any
/// plausible-looking id would be a lie a sense picker would go looking for.
pub const NO_ROW: i64 = 0;

impl GlossBlock {
    /// A one-row block from one raw glossary payload, in the form the
    /// record stores.
    ///
    /// The hover path goes through `Hit`, which already carries a parsed
    /// tree and the ids behind it; this is for callers that hold the stored
    /// text and nothing else - the popup demo, the geometry fixtures, and
    /// tests - so the block it builds carries [`NO_ROW`].
    pub fn parse(dict_name: &str, glossary: &str) -> GlossBlock {
        let doc = Arc::new(GlossDoc::parse(glossary));
        GlossBlock {
            dict_name: dict_name.to_string(),
            dict_id: NO_ROW,
            entries: vec![GlossEntry {
                entry_id: NO_ROW,
                glosses: crate::dict::gloss::plain_items(&doc),
                tags: Vec::new(),
                doc,
                // No store behind a tree parsed from a string, so its
                // images size from what they declare and fall back to
                // their `alt` text.
                media: Vec::new(),
            }],
        }
    }

    /// Every gloss under this dictionary, rows flattened.
    pub fn glosses(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().flat_map(|e| e.glosses.iter().map(String::as_str))
    }
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

    Presentation { top, collapsed, all_cards, sentence: None }
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
    let pos = definition_tags(&best.entry.pos);
    let mut blocks = ordered_blocks(&group.hits, dicts, cfg);
    // Seeded with the card's own tag line, which sits directly above the
    // first dictionary heading: printing the same set again one line later
    // tells a reader nothing.
    dedupe_tags(&mut blocks, pos.clone());
    Card {
        written: group.written,
        reading: group.reading,
        pos,
        freq: best.freq,
        blocks,
        match_len: best.match_len,
    }
}

/// Card back to a one-liner.
///
/// A gloss now carries the dictionary's own line breaks, and a collapsed
/// row is one line by construction, so those breaks fold back into the
/// inline separator the panel still uses between glosses. Folding before
/// truncating is deliberate: the cap counts the characters a reader sees.
pub fn collapsed_from_card(card: &Card, summary_chars: usize) -> CollapsedRow {
    let first_gloss = card
        .blocks
        .first()
        .and_then(|b| b.glosses().next())
        .unwrap_or("");
    CollapsedRow {
        written: card.written.clone(),
        reading: card.reading.clone(),
        summary: truncate_chars(&one_line(first_gloss), summary_chars),
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

/// Per dict, not per hit.
///
/// Grouping is by `dict_id`, the dictionary's identity, and a group's rank
/// comes from the first row that named it, so the existing name-substring
/// ordering and the `dict_id` tie-break are untouched: only the number of
/// blocks changes. Rows keep their arrival order inside a group because the
/// sort is stable and `hits` arrives rank-ordered.
fn ordered_blocks(hits: &[&Hit], dicts: &[DictInfo], cfg: &PresentConfig) -> Vec<GlossBlock> {
    let mut ranked: Vec<(usize, i64, GlossBlock)> = Vec::new();
    for hit in hits {
        let dict_id = hit.entry.dict_id;
        let entry = GlossEntry {
            entry_id: hit.entry.entry_id,
            glosses: hit.entry.glosses(),
            tags: definition_tags(&hit.entry.pos),
            media: hit.entry.media.clone(),
            doc: Arc::clone(&hit.entry.gloss),
        };
        match ranked.iter_mut().find(|(_, id, _)| *id == dict_id) {
            Some((_, _, block)) => block.entries.push(entry),
            None => {
                let dict_name = dict_name_for(dict_id, dicts);
                let rank = dict_order_rank(&dict_name, &cfg.dict_order).unwrap_or(usize::MAX);
                ranked.push((
                    rank,
                    dict_id,
                    GlossBlock { dict_name, dict_id, entries: vec![entry] },
                ));
            }
        }
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, block)| block).collect()
}

/// The tags a row prints, as Yomitan and Hoshi Reader print them.
///
/// A tag that is only digits is the term-bank row's own sense number, not a
/// part of speech. 大辞林 draws its `①②③` inside the tree, so printing that
/// number again as a tag double-numbers the row.
fn definition_tags(pos: &[String]) -> Vec<String> {
    pos.iter().filter(|t| !is_number(t)).cloned().collect()
}

/// Digits only, in any script: `1`, `１`, and `①` are all sense numbers.
fn is_number(tag: &str) -> bool {
    let tag = tag.trim();
    !tag.is_empty() && tag.chars().all(char::is_numeric)
}

/// Consecutive rows print one tag set once.
///
/// Walks the rows in display order across the whole card, so the run of
/// identical sets an eleven-row 大辞林 headword produces collapses to one
/// printed line. `printed` seeds the walk with whatever the caller has
/// already put on screen.
fn dedupe_tags(blocks: &mut [GlossBlock], mut printed: Vec<String>) {
    for block in blocks {
        for entry in &mut block.entries {
            if entry.tags == printed {
                entry.tags.clear();
            } else {
                printed = entry.tags.clone();
            }
        }
    }
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

/// A hard break folded into the inline separator, borrowed when there is
/// nothing to fold.
fn one_line(s: &str) -> Cow<'_, str> {
    if s.contains('\n') {
        Cow::Owned(s.split('\n').collect::<Vec<_>>().join("; "))
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::{Entry, Hit};

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

    /// A block whose glosses arrive as a plain-string glossary - the shape
    /// 20 of the census's 72 dictionaries emit, and the one that round-trips
    /// a literal string unchanged.
    fn strings(dict: &str, glosses: &[&str]) -> GlossBlock {
        GlossBlock::parse(dict, &serde_json::json!(glosses).to_string())
    }

    /// One hit whose gloss arrives the way a record stores it: a
    /// structured-content item with a part-of-speech pill and one block.
    fn hit(written: &str, reading: &str, dict_id: i64, gloss: &str) -> Hit {
        hit_tagged(written, reading, dict_id, gloss, &["noun"])
    }

    /// The same, with the row's part-of-speech set chosen by the caller -
    /// the field the tag dedupe reads. Pills, not a hand-built `pos` vec, so
    /// the labels travel the route a real record's do: through the tree and
    /// out of `pos_labels`.
    fn hit_tagged(
        written: &str,
        reading: &str,
        dict_id: i64,
        gloss: &str,
        pos: &[&str],
    ) -> Hit {
        let mut content: Vec<serde_json::Value> = pos
            .iter()
            .map(|p| {
                serde_json::json!({
                    "tag": "span",
                    "data": {"content": "part-of-speech-info"},
                    "content": p,
                })
            })
            .collect();
        content.push(serde_json::json!({"tag": "div", "content": gloss}));
        let glossary =
            serde_json::json!([{"type": "structured-content", "content": content}]).to_string();
        Hit {
            written: Some(written.to_string()),
            reading: Some(reading.to_string()),
            match_len: 2,
            freq: Some(365),
            score: 7.7,
            process: vec![],
            entry: Entry::parse(dict_id * 100, dict_id, &glossary),
        }
    }

    /// The defect ticket 16 fixes. The census found 6 220 大辞林 headwords
    /// with more than one term-bank row, the worst eleven, and each row used
    /// to bring its own copy of the dictionary's name.
    #[test]
    fn three_rows_from_one_dictionary_become_one_block_with_three_entries() {
        let hits = vec![
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
            hit("昨日", "きのう", 2, "過ぎ去った日。"),
            hit("昨日", "きのう", 2, "近い過去。"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        let blocks = &p.top.as_ref().unwrap().blocks;
        assert_eq!(1, blocks.len(), "one dictionary, one block");
        assert_eq!(3, blocks[0].entries.len(), "one entry per matched row");
        assert_eq!(
            vec!["今日の一日前の日。", "過ぎ去った日。", "近い過去。"],
            blocks[0].glosses().collect::<Vec<_>>(),
            "in the order the rows ranked"
        );
    }

    /// Grouping must not disturb the ordering configuration: 大辞林 leads
    /// even though its rows arrive after Jitendex's and its id is higher.
    #[test]
    fn a_multi_row_dictionary_still_orders_by_the_configuration() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
            hit("昨日", "きのう", 2, "過ぎ去った日。"),
        ];
        let p = build(&hits, &dicts(), &cfg());
        let blocks = &p.top.as_ref().unwrap().blocks;
        assert_eq!(2, blocks.len(), "two dictionaries, two blocks");
        assert!(blocks[0].dict_name.contains("大辞林"), "got {:?}", blocks[0].dict_name);
        assert_eq!(2, blocks[0].entries.len());
        assert!(blocks[1].dict_name.contains("Jitendex"));
        assert_eq!(1, blocks[1].entries.len());
    }

    /// Consecutive rows carrying one tag set print it once. Without this,
    /// the eleven-row headword repeats its tag row eleven times under the
    /// one heading grouping just merged.
    #[test]
    fn consecutive_rows_with_one_tag_set_print_it_once() {
        let hits = vec![
            hit_tagged("昨日", "きのう", 2, "a", &["noun"]),
            hit_tagged("昨日", "きのう", 2, "b", &["noun"]),
            hit_tagged("昨日", "きのう", 2, "c", &["adverb"]),
            hit_tagged("昨日", "きのう", 2, "d", &["adverb"]),
        ];
        let card = build(&hits, &dicts(), &cfg()).top.expect("a top card");
        assert_eq!(vec!["noun".to_string()], card.pos, "the card's own tag line");
        let printed: Vec<Vec<String>> =
            card.blocks[0].entries.iter().map(|e| e.tags.clone()).collect();
        assert_eq!(
            vec![vec![], vec![], vec!["adverb".to_string()], vec![]],
            printed,
            "the card line already said noun, so only the change prints"
        );
    }

    /// A digits-only tag is the row's own sense number in any script. 大辞林
    /// draws its ①②③ inside the tree, so the number must not also arrive as
    /// a tag and double-number the row.
    #[test]
    fn a_numeric_tag_never_reaches_the_panel() {
        let hits = vec![
            hit_tagged("昨日", "きのう", 2, "a", &["1", "noun"]),
            hit_tagged("昨日", "きのう", 2, "b", &["\u{ff12}", "adverb"]),
            hit_tagged("昨日", "きのう", 2, "c", &["\u{2462}"]),
        ];
        let card = build(&hits, &dicts(), &cfg()).top.expect("a top card");
        assert_eq!(vec!["noun".to_string()], card.pos, "not \"1 · noun\"");
        let printed: Vec<Vec<String>> =
            card.blocks[0].entries.iter().map(|e| e.tags.clone()).collect();
        assert_eq!(vec![vec![], vec!["adverb".to_string()], vec![]], printed);
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
            blocks: vec![strings("Test", &["cat", "feline"])],
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
            blocks: vec![strings("D", &[&long])],
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

    /// A gloss carries the dictionary's line breaks now; a collapsed row
    /// is one line, so the row must not grow a second one.
    #[test]
    fn collapsed_from_card_folds_a_multiline_gloss_onto_one_line() {
        let card = Card {
            written: Some("走る".into()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![strings("D", &["to run\nto flow"])],
            match_len: 1,
        };
        let row = collapsed_from_card(&card, 40);
        assert_eq!("to run; to flow", row.summary);
    }

    /// The cap counts what a reader sees, so folding happens first: a
    /// truncation that ran before the fold would cut at 40 characters of
    /// the raw string and leave a stray break behind.
    #[test]
    fn a_folded_summary_is_truncated_after_folding() {
        let card = Card {
            written: None,
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![strings("D", &[&format!("{}\n{}", "あ".repeat(30), "い".repeat(30))])],
            match_len: 1,
        };
        let row = collapsed_from_card(&card, 40);
        assert!(!row.summary.contains('\n'));
        assert_eq!(41, row.summary.chars().count());
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

    #[test]
    fn build_leaves_sentence_unset() {
        let p = build(&[hit("猫", "ねこ", 1, "cat")], &dicts(), &cfg());
        assert!(p.sentence.is_none());
    }

    /// Guards the payload path, not a
    /// hand-rolled copy of it.
    #[test]
    fn sentence_source_appears_in_fields_when_set() {
        let mut p = build(&[hit("猫", "ねこ", 1, "cat")], &dicts(), &cfg());
        p.sentence = Some("その猫はかわいい。".to_string());

        let (_, fields) = crate::controller::note_payload(&p, false);

        assert_eq!(Some(&"その猫はかわいい。".to_string()), fields.get("sentence"));
    }
}

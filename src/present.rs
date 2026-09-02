//! What the popup shows.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use crate::dict::gloss::GlossDoc;
use crate::dict::media::Intrinsic;
use crate::dict::pitch::Accent;

use crate::geom::PhysRect;
use crate::lookup::model::{Dictionary, Hit};
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
    /// The Reported frequency of this card's headword: the number the
    /// highest-ordered enabled frequency dictionary that has it published,
    /// drawn as `freq {n}` in the card's corner.
    ///
    /// One dictionary's own claim, never the reduced Frequency rank that
    /// ordered these results (ARCHITECTURE.md#dictionary-and-lookup): the
    /// figure on screen is always something a real dictionary said, so a
    /// reader can look it up and check it, and switching ranking strategy
    /// does not change it.
    pub freq: Option<i64>,
    /// In display order.
    pub blocks: Vec<GlossBlock>,
    /// Input chars, not bytes.
    pub match_len: usize,
    /// The Pitch patterns the enabled pitch dictionaries give this card's
    /// reading, deduplicated, in the pitch list's order.
    ///
    /// One row per *distinct* accent and not one per dictionary: the
    /// census found that over the readings two or more pitch dictionaries
    /// both know, 379 288 accent claims reduce to 123 140 - a 67.5% collapse -
    /// so without the dedup a three-dictionary user would read the same accent
    /// three times on nearly every card.
    ///
    /// Empty when no enabled pitch dictionary has the reading, and the card
    /// then draws no pitch row at all rather than an empty one.
    pub pitch: Vec<PitchRow>,
}

/// One accent a card draws, and the dictionaries that gave it.
///
/// The accents are compared by their **position** alone, because the
/// position is the whole of what this card draws: the nasal and devoiced
/// moras ride along unread until later work draws them, and comparing with
/// them would put NHK's marked heiban on a second row beside 三省堂's
/// unmarked one - two rows that look identical, which is the failure the
/// dedup exists to prevent (`docs/research/pitch-accent-shapes.md`).
///
/// So `accent` is the one the highest-ordered dictionary that claimed this
/// position published, markers and tags included - priority-first-wins,
/// exactly as [`Card::freq`] reports one real dictionary's number rather
/// than a computed one.
#[derive(Debug, Clone, PartialEq)]
pub struct PitchRow {
    pub accent: Accent,
    /// The dictionaries that gave this accent, in the pitch list's order,
    /// each named once.
    pub dicts: Vec<String>,
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
    /// Half of the stable identity a scene element carries; the other half
    /// is the row's `entry_id` and the node path inside its tree.
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
    /// it - the Anki HTML field today, the popup scene once it lands -
    /// so the card and the panel cannot drift apart again.
    pub doc: Arc<GlossDoc>,
    /// The recorded size of every image asset this row's tree names and the
    /// media store has bytes for, by the `path` the node declared.
    ///
    /// Carried rather than looked up while the panel is laid out:
    /// `layout::scene` runs with a measurer and no database, and this is
    /// what lets it resolve an image's rect without decoding a pixel
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
///
/// Identity only, and deliberately no roles: what an archive supplies is
/// read from its banks and lives on its library entry
/// ([`crate::library::Roles`]), while the database has no index that could
/// answer "does this dictionary have term rows" without scanning `entry`.
#[derive(Debug, Clone, PartialEq)]
pub struct DictInfo {
    pub dict_id: i64,
    /// The exact name every Dictionary list names it by
    /// (ARCHITECTURE.md#dictionary-and-lookup).
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
    /// The enabled terms Dictionaries, by exact name, highest priority
    /// first.
    ///
    /// Both the filter and the order, and there is nothing behind it: a
    /// name either names an installed Dictionary or does not, and an empty
    /// list is a user who turned every one of them off, not a typo to
    /// second-guess (ARCHITECTURE.md#dictionary-and-lookup).
    pub terms: Vec<String>,
    /// The enabled pitch Dictionaries, by exact name, highest priority
    /// first. The same filter and the same order, over accents.
    pub pitch: Vec<String>,
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
///
/// `dict` is read once per card, for that card's own reading, and for
/// nothing else here: a Pitch pattern is per reading, so it is neither on
/// the term path nor on a `Hit`.
pub fn build(
    hits: &[Hit],
    dicts: &[DictInfo],
    cfg: &PresentConfig,
    dict: &dyn Dictionary,
) -> Presentation {
    let mut groups: Vec<Group> = Vec::new();
    for hit in hits {
        if !enabled_for(hit.entry.dict_id, dicts, &cfg.terms) {
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
        .map(|g| card_from_group(g, dicts, cfg, dict))
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

fn card_from_group(
    group: Group,
    dicts: &[DictInfo],
    cfg: &PresentConfig,
    dict: &dyn Dictionary,
) -> Card {
    // Pre-ranked: first is best.
    let best = group.hits[0];
    let pos = definition_tags(&best.entry.pos);
    let mut blocks = ordered_blocks(&group.hits, dicts, cfg);
    // Seeded with the card's own tag line, which sits directly above the
    // first dictionary heading: printing the same set again one line later
    // tells a reader nothing.
    dedupe_tags(&mut blocks, pos.clone());
    let pitch = pitch_rows(&group, dicts, cfg, dict);
    Card {
        written: group.written,
        reading: group.reading,
        pos,
        freq: best.entry.reported_freq,
        blocks,
        match_len: best.match_len,
        pitch,
    }
}

/// The pitch rows one card draws: every enabled pitch dictionary's accents
/// for this card's reading, in the pitch list's order, identical accents
/// collapsed onto one row that names all of them.
///
/// One read, for one (headword, reading) pair - `term` is the kanji
/// headword or, where there is none, the reading itself, which is the
/// expression a Yomitan pitch archive names. A card with no reading has no
/// accent to draw and runs no query at all.
///
/// Ordering and enabling are the **pitch** list's, spent here the same way
/// [`ordered_blocks`] spends the terms list over dictionaries: a dictionary
/// the list does not name contributes nothing, and one it names ranks where
/// it names it, `dict_id` breaking a tie so two dictionaries at the same
/// position still order the same way twice. The two lists are independent,
/// so a dictionary may lead the pitch list while sitting disabled in terms.
fn pitch_rows(
    group: &Group,
    dicts: &[DictInfo],
    cfg: &PresentConfig,
    dict: &dyn Dictionary,
) -> Vec<PitchRow> {
    let Some(reading) = group.reading.as_deref().filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    let term = group.written.as_deref().unwrap_or(reading);

    let mut ranked: Vec<(usize, i64, String, Accent)> = Vec::new();
    for claim in dict.pitch_for(term, reading) {
        if !enabled_for(claim.dict_id, dicts, &cfg.pitch) {
            continue;
        }
        let name = dict_name_for(claim.dict_id, dicts);
        let rank = list_rank(&name, &cfg.pitch).unwrap_or(usize::MAX);
        ranked.push((rank, claim.dict_id, name, claim.accent));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut out: Vec<PitchRow> = Vec::new();
    for (_, _, name, accent) in ranked {
        // By position alone, which is the whole of what a row draws (see
        // `PitchRow`). The first claimant's accent is the one kept, so its
        // markers are the highest-ordered dictionary's.
        match out.iter_mut().find(|row| row.accent.position == accent.position) {
            Some(row) => {
                if !row.dicts.contains(&name) {
                    row.dicts.push(name);
                }
            }
            None => out.push(PitchRow { accent, dicts: vec![name] }),
        }
    }
    out
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
/// is its dictionary's position in the enabled terms list, `dict_id`
/// breaking a tie. Rows keep their arrival order inside a group because the
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
                let rank = list_rank(&dict_name, &cfg.terms).unwrap_or(usize::MAX);
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

/// Does the list enable the dictionary this row came from?
///
/// A `dict_id` no [`DictInfo`] names is **kept**. The identities are a
/// snapshot the caller took, and a row from a dictionary that snapshot
/// predates is a reload the pipeline has not seen yet, not a dictionary the
/// user switched off - a list cannot have an opinion about a name it has
/// never been told. Nothing a user can type reaches this arm, which is what
/// makes it different in kind from the deleted substring guards.
fn enabled_for(dict_id: i64, dicts: &[DictInfo], list: &[String]) -> bool {
    match dicts.iter().find(|d| d.dict_id == dict_id) {
        Some(known) => keeps_dict(&known.name, list),
        None => true,
    }
}

/// This Dictionary's position in one Dictionary list, or `None` when the
/// list does not name it.
///
/// Equality on the exact name, which is the whole of identity now
/// (ARCHITECTURE.md#dictionary-and-lookup): the substring `contains`
/// this replaced could not tell two editions of one dictionary apart,
/// so ordering or excluding one of them silently did the same to the
/// other.
pub fn list_rank(dict_name: &str, list: &[String]) -> Option<usize> {
    list.iter().position(|listed| listed == dict_name)
}

/// Does this list enable the Dictionary?
///
/// Membership and nothing else. A list that names none of the installed
/// dictionaries enables none of them, and an empty list enables nothing at
/// all - "search nothing in this role" is a state the user can reach on
/// purpose, and the guards that used to talk it out of that were insurance
/// against a substring matching nothing.
pub fn keeps_dict(dict_name: &str, list: &[String]) -> bool {
    list.iter().any(|listed| listed == dict_name)
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
    use crate::dict::pitch::Position;
    use crate::lookup::model::{Entry, FakeDictionary, Hit};

    /// A library with no pitch dictionary in it, which is what every test
    /// below but the pitch ones is about.
    fn no_pitch() -> FakeDictionary {
        FakeDictionary::new()
    }

    /// A library whose pitch dictionaries claim what the caller says, in the
    /// order the caller lists them - which is the order the stored rows come
    /// back in, not the order the pitch list puts them in.
    fn with_pitch(claims: &[(i64, &str, &str, Accent)]) -> FakeDictionary {
        let mut dict = FakeDictionary::new();
        for (dict_id, term, reading, accent) in claims {
            dict.add_pitch(term, reading, *dict_id, accent.clone());
        }
        dict
    }

    /// One accent with no markers and no tags, which is 96% of the corpus.
    fn downstep(fall: u32) -> Accent {
        Accent {
            position: Position::Downstep(fall),
            nasal: Vec::new(),
            devoice: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn dicts() -> Vec<DictInfo> {
        vec![
            DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ]
    }

    fn cfg() -> PresentConfig {
        PresentConfig {
            terms: vec!["大辞林　第四版".into(), "Jitendex.org [2026-07-09]".into()],
            pitch: vec!["大辞林　第四版".into(), "Jitendex.org [2026-07-09]".into()],
            summary_chars: 40,
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

    /// The defect grouping fixes. The census found 6 220 大辞林 headwords
    /// with more than one term-bank row, the worst eleven, and each row used
    /// to bring its own copy of the dictionary's name.
    #[test]
    fn three_rows_from_one_dictionary_become_one_block_with_three_entries() {
        let hits = vec![
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
            hit("昨日", "きのう", 2, "過ぎ去った日。"),
            hit("昨日", "きのう", 2, "近い過去。"),
        ];
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let card = build(&hits, &dicts(), &cfg(), &no_pitch()).top.expect("a top card");
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
        let card = build(&hits, &dicts(), &cfg(), &no_pitch()).top.expect("a top card");
        assert_eq!(vec!["noun".to_string()], card.pos, "not \"1 · noun\"");
        let printed: Vec<Vec<String>> =
            card.blocks[0].entries.iter().map(|e| e.tags.clone()).collect();
        assert_eq!(vec![vec![], vec!["adverb".to_string()], vec![]], printed);
    }

    /// The trap the substring model could not escape: two editions sharing
    /// a name can be enabled independently, because the list names one of
    /// them and equality answers about that one only.
    #[test]
    fn two_dictionaries_sharing_a_substring_are_enabled_independently() {
        let list = vec!["大辞林　第四版".to_string()];
        assert!(keeps_dict("大辞林　第四版", &list));
        assert!(!keeps_dict("大辞林　第三版", &list));
        assert!(!keeps_dict("大辞林", &list), "the shared prefix names nothing on its own");
    }

    /// The guard that used to stand here read an empty list as "no
    /// restriction" and searched everything. There is nothing to guard
    /// against now: an empty enabled list is a user who unchecked every row.
    #[test]
    fn an_empty_terms_list_searches_nothing() {
        let hits = vec![hit("猫", "ねこ", 1, "cat"), hit("犬", "いぬ", 2, "dog")];
        let cfg = PresentConfig { terms: Vec::new(), pitch: Vec::new(), summary_chars: 40 };
        let p = build(&hits, &dicts(), &cfg, &no_pitch());
        assert!(p.all_cards.is_empty(), "no dictionary is enabled, so nothing is searched");
        assert!(p.top.is_none());
    }

    #[test]
    fn a_disabled_dictionary_contributes_no_card() {
        let hits = vec![hit("猫", "ねこ", 1, "cat"), hit("犬", "いぬ", 2, "dog")];
        let cfg = PresentConfig {
            terms: vec!["大辞林　第四版".to_string()],
            pitch: Vec::new(),
            summary_chars: 40,
        };
        let p = build(&hits, &dicts(), &cfg, &no_pitch());
        assert_eq!(1, p.all_cards.len(), "the disabled dictionary makes no card");
        assert!(p.all_cards[0].blocks.iter().all(|b| b.dict_name == "大辞林　第四版"));
        assert!(!p.all_cards[0].blocks.is_empty(), "and the card is not hollow");
    }

    #[test]
    fn empty_hits_yield_nothing() {
        let p = build(&[], &dicts(), &cfg(), &no_pitch());
        assert!(p.top.is_none());
        assert!(p.collapsed.is_empty());
    }

    #[test]
    fn a_single_hit_becomes_the_top_card_with_no_rows() {
        let p = build(&[hit("昨日", "きのう", 1, "yesterday")], &dicts(), &cfg(), &no_pitch());
        assert_eq!(Some("昨日".to_string()), p.top.as_ref().unwrap().written);
        assert!(p.collapsed.is_empty());
    }

    #[test]
    fn same_word_from_two_dictionaries_merges_into_one_card() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
        ];
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
        assert!(p.collapsed.is_empty(), "the two must merge, not collapse");
        assert_eq!(2, p.top.as_ref().unwrap().blocks.len());
    }

    #[test]
    fn daijirin_orders_before_jitendex_regardless_of_dict_id() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨日", "きのう", 2, "今日の一日前の日。"),
        ];
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
        let s = &p.collapsed[0].summary;
        assert!(s.ends_with('…'));
        assert_eq!(41, s.chars().count(), "40 chars plus the ellipsis");
    }

    #[test]
    fn a_short_summary_is_not_marked() {
        let hits = vec![hit("A", "あ", 1, "short"), hit("B", "い", 1, "also short")];
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
        assert_eq!("also short", p.collapsed[0].summary);
    }

    /// Position in the list is priority, and a name the list is silent
    /// about has none - which is what sorts it last.
    #[test]
    fn a_list_ranks_the_names_it_holds_and_no_others() {
        let list = vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()];
        assert_eq!(Some(0), list_rank("大辞林　第四版", &list));
        assert_eq!(Some(1), list_rank("Jitendex.org [2026-07-09]", &list));
        assert_eq!(None, list_rank("Jitendex", &list), "a prefix is not the name");
        assert_eq!(None, list_rank("大辞林　第四版", &[]));
    }

    /// The blank entry that used to match everything is now just a name no
    /// dictionary has.
    #[test]
    fn a_blank_list_entry_names_nothing() {
        let with_blank = vec![String::new(), "Jitendex.org".to_string()];
        assert!(!keeps_dict("", &["Jitendex.org".to_string()]));
        assert_eq!(Some(1), list_rank("Jitendex.org", &with_blank));
    }

    /// A stale identity snapshot must not blank the popup: the list cannot
    /// have an opinion about a dictionary it has never been told exists.
    #[test]
    fn a_row_from_a_dictionary_no_identity_names_still_produces_a_block() {
        let hits = vec![hit("猫", "ねこ", 99, "cat")];
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
            pitch: Vec::new(),
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
        let p = build(&[long, short], &dicts(), &cfg(), &no_pitch());
        assert_eq!(3, p.top.as_ref().unwrap().match_len);
    }

    #[test]
    fn build_populates_all_cards_for_every_group() {
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "last (year)"),
        ];
        let p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
            pitch: Vec::new(),
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
            pitch: Vec::new(),
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
            pitch: Vec::new(),
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
            pitch: Vec::new(),
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
        let mut p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let mut p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let mut p = build(&hits, &dicts(), &cfg(), &no_pitch());
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
        let mut p = build(&hits, &dicts(), &cfg(), &no_pitch());
        let original_top = p.top.clone();

        swap_top(&mut p, 0, 40);
        assert_ne!(original_top, p.top);
        swap_top(&mut p, 0, 40);
        assert_eq!(original_top, p.top);
    }

    #[test]
    fn build_leaves_sentence_unset() {
        let p = build(&[hit("猫", "ねこ", 1, "cat")], &dicts(), &cfg(), &no_pitch());
        assert!(p.sentence.is_none());
    }

    /// Guards the payload path, not a
    /// hand-rolled copy of it.
    #[test]
    fn sentence_source_appears_in_fields_when_set() {
        let mut p = build(&[hit("猫", "ねこ", 1, "cat")], &dicts(), &cfg(), &no_pitch());
        p.sentence = Some("その猫はかわいい。".to_string());

        let (_, fields) = crate::controller::note_payload(&p, false);

        assert_eq!(Some(&"その猫はかわいい。".to_string()), fields.get("sentence"));
    }

    // ---- pitch

    /// The dedup, which the census makes the visible part of pitch
    /// rather than a refinement: 81.4% of the readings two or more pitch
    /// dictionaries both know get an identical accent set from all of them.
    #[test]
    fn two_dictionaries_giving_one_accent_draw_one_row_naming_both() {
        let dict = with_pitch(&[
            (1, "昨日", "きのう", downstep(1)),
            (2, "昨日", "きのう", downstep(1)),
        ]);

        let card = build(&[hit("昨日", "きのう", 1, "yesterday")], &dicts(), &cfg(), &dict)
            .top
            .expect("a top card");

        assert_eq!(1, card.pitch.len(), "one accent, one row: {:?}", card.pitch);
        assert_eq!(Position::Downstep(1), card.pitch[0].accent.position);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            card.pitch[0].dicts,
            "both names against the one row, in the pitch list's order"
        );
    }

    /// The 18.7% of readings that are not unanimous, which the user
    /// asks to see rather than hide.
    #[test]
    fn two_dictionaries_giving_different_accents_draw_two_rows_in_list_order() {
        // Seeded lowest-priority first, so a reader that kept the stored
        // order rather than the pitch list's fails here.
        let dict = with_pitch(&[
            (1, "昨日", "きのう", downstep(0)),
            (2, "昨日", "きのう", downstep(1)),
        ]);

        let card = build(&[hit("昨日", "きのう", 1, "yesterday")], &dicts(), &cfg(), &dict)
            .top
            .expect("a top card");

        assert_eq!(
            vec![Position::Downstep(1), Position::Downstep(0)],
            card.pitch.iter().map(|r| r.accent.position.clone()).collect::<Vec<_>>(),
            "大辞林 leads the configured order, so its accent leads"
        );
        assert_eq!(vec!["大辞林　第四版".to_string()], card.pitch[0].dicts);
        assert_eq!(vec!["Jitendex.org [2026-07-09]".to_string()], card.pitch[1].dicts);
    }

    /// The census's `合縁奇縁`: three dictionaries share one accent and one
    /// dissents, so the header draws two rows and names three against one.
    #[test]
    fn a_majority_and_a_dissenter_draw_two_rows_with_the_names_split_between_them() {
        let mut dict = FakeDictionary::new();
        for id in [1, 2, 3] {
            dict.add_pitch("合縁奇縁", "あいえんきえん", id, downstep(5));
        }
        dict.add_pitch("合縁奇縁", "あいえんきえん", 4, downstep(0));
        let named = vec![
            DictInfo { dict_id: 1, name: "NHK".into() },
            DictInfo { dict_id: 2, name: "三省堂".into() },
            DictInfo { dict_id: 3, name: "新明解".into() },
            DictInfo { dict_id: 4, name: "大辞泉".into() },
        ];
        let hits = vec![hit("合縁奇縁", "あいえんきえん", 1, "a happy match")];
        let listed: Vec<String> = named.iter().map(|d| d.name.clone()).collect();
        let cfg = PresentConfig {
            terms: listed.clone(),
            pitch: listed,
            summary_chars: 40,
        };

        let card = build(&hits, &named, &cfg, &dict).top.expect("a top card");

        assert_eq!(2, card.pitch.len());
        assert_eq!(3, card.pitch[0].dicts.len(), "{:?}", card.pitch[0].dicts);
        assert_eq!(vec!["大辞泉".to_string()], card.pitch[1].dicts);
    }

    /// The comparison is the position alone, because the position is the
    /// whole of what a card draws. NHK is the only dictionary in the corpus
    /// with markers, so comparing with them would put its marked heiban on a
    /// second row that looks exactly like the unmarked one beside it.
    #[test]
    fn one_accent_marked_by_one_dictionary_and_bare_by_another_is_still_one_row() {
        let marked = Accent {
            position: Position::Downstep(0),
            nasal: vec![4],
            devoice: vec![2],
            tags: Vec::new(),
        };
        let dict = with_pitch(&[
            (2, "合鍵", "あいかぎ", marked.clone()),
            (1, "合鍵", "あいかぎ", downstep(0)),
        ]);

        let card = build(&[hit("合鍵", "あいかぎ", 1, "spare key")], &dicts(), &cfg(), &dict)
            .top
            .expect("a top card");

        assert_eq!(1, card.pitch.len(), "{:?}", card.pitch);
        assert_eq!(
            marked, card.pitch[0].accent,
            "the markers kept are the highest-ordered dictionary's"
        );
        assert_eq!(2, card.pitch[0].dicts.len());
    }

    #[test]
    fn a_reading_no_pitch_dictionary_has_draws_nothing() {
        let dict = with_pitch(&[(1, "昨日", "きのう", downstep(1))]);

        let card = build(&[hit("猫", "ねこ", 1, "cat")], &dicts(), &cfg(), &dict)
            .top
            .expect("a top card");

        assert!(card.pitch.is_empty(), "no row, and not an empty one");
    }

    /// A kana-only headword has no `written`, and the expression a pitch
    /// archive names it by is the reading itself.
    #[test]
    fn a_kana_only_headword_is_looked_up_by_its_reading() {
        let mut dict = FakeDictionary::new();
        dict.add_pitch("ねこ", "ねこ", 1, downstep(1));
        let hits = vec![Hit {
            written: None,
            reading: Some("ねこ".to_string()),
            ..hit("猫", "ねこ", 1, "cat")
        }];

        let card = build(&hits, &dicts(), &cfg(), &dict).top.expect("a top card");

        assert_eq!(1, card.pitch.len());
        assert_eq!(Position::Downstep(1), card.pitch[0].accent.position);
    }

    /// A disabled dictionary is not a source, and the pitch list is the
    /// only list that answers: the terms list here still names both, so
    /// what silences one of them is its absence from `pitch` and nothing
    /// else.
    #[test]
    fn a_dictionary_the_pitch_list_excludes_contributes_no_accent() {
        let dict = with_pitch(&[
            (1, "昨日", "きのう", downstep(0)),
            (2, "昨日", "きのう", downstep(1)),
        ]);
        let cfg = PresentConfig {
            pitch: vec!["大辞林　第四版".to_string()],
            ..cfg()
        };

        let card = build(&[hit("昨日", "きのう", 2, "yesterday")], &dicts(), &cfg, &dict)
            .top
            .expect("a top card");

        assert_eq!(1, card.pitch.len(), "{:?}", card.pitch);
        assert_eq!(vec!["大辞林　第四版".to_string()], card.pitch[0].dicts);
    }

    /// A card with no reading has no moras to mark, so it runs no query at
    /// all rather than one that cannot match.
    #[test]
    fn a_card_with_no_reading_draws_no_pitch() {
        let mut dict = FakeDictionary::new();
        dict.add_pitch("昨日", "きのう", 1, downstep(1));
        let hits = vec![Hit { reading: None, ..hit("昨日", "きのう", 1, "yesterday") }];

        let card = build(&hits, &dicts(), &cfg(), &dict).top.expect("a top card");

        assert!(card.pitch.is_empty());
    }

    /// Every card gets its own accents and not just the top one, because
    /// [`swap_top`] promotes a collapsed row without a store to ask.
    #[test]
    fn a_promoted_card_already_carries_its_own_accents() {
        let dict = with_pitch(&[
            (1, "昨日", "きのう", downstep(1)),
            (1, "昨", "さく", downstep(0)),
        ]);
        let hits = vec![
            hit("昨日", "きのう", 1, "yesterday"),
            hit("昨", "さく", 1, "previous"),
        ];

        let mut p = build(&hits, &dicts(), &cfg(), &dict);
        swap_top(&mut p, 0, 40);

        let promoted = p.top.expect("a promoted card");
        assert_eq!(Some("昨".to_string()), promoted.written);
        assert_eq!(vec![Position::Downstep(0)], promoted.pitch.iter().map(|r| r.accent.position.clone()).collect::<Vec<_>>());
    }
}

//! The presentation module builds the popup view model from Dictionary hits.
//!
//! It groups hits, orders Dictionaries, and builds summaries and pitch rows before the layout pass.
//! The platform bins then measure and paint this model.

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

/// The popup view model for one hover.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    pub top: Option<Card>,
    pub collapsed: Vec<CollapsedRow>,
    /// A complete [`Card`] for each group.
    pub all_cards: Vec<Card>,
    /// The OCR sentence line that `worker.rs` sets for note payloads.
    pub sentence: Option<String>,
    /// The on-screen text that the top Card matched, as OCR read it.
    ///
    /// The note bolds this text inside `sentence`. It is the surface form, not
    /// the headword: a card for 崩れる bolds 崩れたり. `worker.rs` sets it
    /// from the hovered span and `match_len`.
    pub surface: Option<String>,
}

/// The complete top [`Card`].
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub written: Option<String>,
    pub reading: Option<String>,
    pub pos: Vec<String>,
    /// The Reported frequency for this Card's headword.
    ///
    /// The highest-priority enabled frequency Dictionary supplies this value when it reports the headword.
    /// The Card shows `freq {n}` in its corner.
    ///
    /// This value is the Dictionary's Reported frequency, not the Frequency rank that orders the results
    /// (ARCHITECTURE.md#dictionary-and-lookup). A reader can check the reported number.
    /// A change to the Ranking strategy does not change this value.
    pub freq: Option<i64>,
    /// The GlossBlock values in display order.
    pub blocks: Vec<GlossBlock>,
    /// The match length uses input characters, not bytes.
    pub match_len: usize,
    /// The Pitch patterns from enabled pitch Dictionaries for this Card's reading.
    ///
    /// This list removes duplicate Accent positions and uses Pitch list order.
    /// Each [`PitchRow`] holds one position, not one Dictionary.
    /// The census reduced 379 288 Accent claims to 123 140 rows. This result
    /// gives a 67.5% reduction. Without it, a user with three Dictionaries sees
    /// the same Accent three times on almost every Card.
    ///
    /// The vector is empty when no enabled pitch Dictionary reports a Pitch
    /// pattern for the reading. The Card then shows no pitch row.
    pub pitch: Vec<PitchRow>,
}

/// One Accent that a Card shows, with the Dictionaries that report it.
///
/// This type compares Accents by `position` only because the Card shows only
/// this value. The Card does not show nasal or devoiced moras. A comparison
/// that includes those moras puts marked NHK heiban and unmarked 三省堂 heiban
/// on separate rows. Those rows look identical. This comparison prevents that
/// result (`docs/research/pitch-accent-shapes.md`).
///
/// The `accent` field holds the Accent from the highest-priority Dictionary
/// for each position. This Accent includes its markers and tags.
/// [`Card::freq`] follows the same priority rule for one Reported frequency
/// instead of a computed Frequency rank.
#[derive(Debug, Clone, PartialEq)]
pub struct PitchRow {
    pub accent: Accent,
    /// Names of the Dictionaries that report this Accent, in Pitch list order.
    /// Each name appears once.
    pub dicts: Vec<String>,
}

/// One Dictionary contribution to a Card, with its GlossDoc trees.
///
/// Each Dictionary supplies one block. A matched term-bank row does not
/// create a separate block. The census found 6 220 大辞林 headwords with
/// multiple rows, and the largest group had eleven rows. The old panel
/// repeated the Dictionary name for every row and placed one Gloss under it.
#[derive(Debug, Clone, PartialEq)]
pub struct GlossBlock {
    pub dict_name: String,
    /// The database row ID that identifies the Dictionary.
    ///
    /// A reader sees the name, but two libraries can use different spellings.
    /// This ID provides half of a scene element's stable identity. The `entry_id`
    /// and the node path provide the other half.
    pub dict_id: i64,
    /// One Entry for each matched term-bank row, in rank order.
    pub entries: Vec<GlossEntry>,
}

/// One matched term-bank row.
#[derive(Debug, Clone, PartialEq)]
pub struct GlossEntry {
    /// The database ID of the Entry in this row.
    ///
    /// The Entry stores this ID so a scene element identifies its source row,
    /// not only its text. The element can identify "sense 3 of 大辞林" instead
    /// of a character range. [`NO_ROW`] identifies content with no stored row.
    pub entry_id: i64,
    /// The plain-text form, with one string for each Gloss.
    ///
    /// The code computes this list before `layout::scene` runs. The panel needs
    /// one string for each frame.
    pub glosses: Vec<String>,
    /// The part-of-speech set that the panel can show.
    ///
    /// The list has no numeric tags. It is **empty when the prior row showed
    /// the same set**. An empty list does not mean that this row has no tags.
    /// A reader needs tags only when they change across consecutive 大辞林 rows.
    /// Yomitan and Hoshi Reader use the same rule.
    pub tags: Vec<String>,
    /// The GlossDoc tree shared with the parsed-tree cache.
    ///
    /// Every Gloss view renders this tree. The Anki HTML field and the PopupScene
    /// use the same tree. The shared tree keeps the Card and the panel consistent.
    pub doc: Arc<GlossDoc>,
    /// The intrinsic size of each image asset that this row names.
    ///
    /// The media store must contain bytes for the asset. The declared `path` is
    /// its key. The Entry stores these sizes so the panel layout needs no lookup.
    /// `layout::scene` has a TextMeasure but no database. These sizes let it
    /// resolve an image rect without a pixel decode
    /// (`lookup::model::Entry::media`). If a path is absent, the renderer shows
    /// the `alt` text.
    pub media: Vec<(String, Intrinsic)>,
}

/// The ID for content that has no database row.
///
/// SQLite row numbers start at one. Zero therefore identifies no Dictionary
/// and no term-bank row. A demo, a Geometry snapshot fixture, or a test can
/// create such content. Scene elements remain addressable within their tree,
/// but they identify no stored Entry. A made-up nonzero ID would identify the
/// wrong row. A Sense picker would then search for that row.
pub const NO_ROW: i64 = 0;

impl GlossBlock {
    /// Builds one GlossBlock from one stored glossary payload.
    ///
    /// The hover path uses `Hit`, which already contains a GlossDoc tree and its
    /// IDs. The popup demo, Geometry snapshot fixtures, and tests call this
    /// function with stored text only. The result uses [`NO_ROW`].
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
                // A tree from a string has no Media store. Each image therefore uses its
                // declared size. An image without a declared size shows its `alt` text.
                media: Vec::new(),
            }],
        }
    }

    /// Returns all Gloss values for this Dictionary, without Entry groups.
    pub fn glosses(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().flat_map(|e| e.glosses.iter().map(String::as_str))
    }
}

/// One non-top group in one line.
#[derive(Debug, Clone, PartialEq)]
pub struct CollapsedRow {
    pub written: Option<String>,
    pub reading: Option<String>,
    /// The first Gloss, limited to the configured length.
    pub summary: String,
}

/// The identity of a Dictionary.
///
/// This type stores identity only and no Dictionary roles. The Library reads
/// each role from the archive banks and stores it on the Library Entry
/// ([`crate::library::Roles`]). The database has no index that identifies term
/// rows without a full scan of `entry`.
#[derive(Debug, Clone, PartialEq)]
pub struct DictInfo {
    pub dict_id: i64,
    /// The exact name that all Dictionary lists use
    /// (ARCHITECTURE.md#dictionary-and-lookup).
    pub name: String,
}

/// The Anki state for this popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnkiPopupState {
    pub dupes: HashSet<String>,
    pub added: HashSet<String>,
    pub enabled: bool,
    pub adding: bool,
    /// True while the popup checks for duplicates.
    pub checking: bool,
    /// True until AnkiConnect gives an answer.
    pub connected: bool,
    /// True if the last add-note call failed.
    pub failed: bool,
}

impl AnkiPopupState {
    /// Returns a disabled state with no operation in progress.
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

    /// Returns the initial state of a new popup.
    /// The popup checks for duplicates when Anki is enabled.
    pub fn fresh(enabled: bool) -> Self {
        Self {
            enabled,
            checking: enabled,
            connected: enabled,
            ..Self::disabled()
        }
    }
}

/// The configuration that controls presentation.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentConfig {
    /// The enabled Dictionaries in the terms list, in exact-name and priority order.
    ///
    /// This list controls selection and order. No other value controls these rules.
    /// Each name identifies an installed Dictionary or identifies nothing.
    /// An empty list means that the user disabled every Dictionary
    /// (ARCHITECTURE.md#dictionary-and-lookup).
    pub terms: Vec<String>,
    /// The enabled pitch Dictionaries, in exact-name and priority order.
    /// This list controls Accent selection and order.
    pub pitch: Vec<String>,
    /// The maximum character count of a collapsed summary.
    pub summary_chars: usize,
}
/// One headword group that this module uses to build a Presentation.
struct Group<'a> {
    written: Option<String>,
    reading: Option<String>,
    hits: Vec<&'a Hit>,
}

/// Returns whether a character is a CJK ideograph.
pub fn is_kanji(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
    )
}

/// Returns the written form and the reading that one hit claims.
///
/// A term bank can leave the reading empty. Babylon does so on 9 990 of its 10 000
/// rows, and 大辞泉 does so on alias rows. The build then stores the term as its own
/// reading with no written form, the shape of a kana-only headword
/// (`dict::build::PreparedRow::same`). A reading is kana, so a "reading" with a kanji
/// in it is the term echoed back. This function reads that shape as a written form
/// with no reading. The Card can then borrow the reading from a Dictionary that has
/// one, and a mined note does not put the kanji string in its reading field.
///
/// This rule lives here and not in the build. A build change needs a schema bump and
/// a rebuild of every library. This rule reads the stored shape as it is.
fn claim(hit: &Hit) -> (Option<&str>, Option<&str>) {
    match (hit.written.as_deref(), hit.reading.as_deref()) {
        (None, Some(reading)) if reading.chars().any(is_kanji) => (Some(reading), None),
        (written, reading) => (written, reading),
    }
}

/// Builds the Presentation from Dictionary hits that already have rank order.
///
/// The `hits` slice must use rank order. The function reads `dict` once for
/// each Card reading. A Pitch pattern belongs to a reading, not to a term
/// path or a `Hit`.
///
/// A hit joins the group with its written form and reading. A hit with no reading
/// ([`claim`]) joins the first group with its written form, and a group with no
/// reading takes the reading of the first hit that supplies one. 銀 has three
/// readings, so a reading-less 銀 joins the reading that ranks highest.
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
        let (written, reading) = claim(hit);
        match groups.iter_mut().find(|g| {
            g.written.as_deref() == written
                && (g.reading.as_deref() == reading
                    || g.reading.is_none()
                    || reading.is_none())
        }) {
            Some(g) => {
                if g.reading.is_none() {
                    g.reading = reading.map(str::to_string);
                }
                g.hits.push(hit);
            }
            None => groups.push(Group {
                written: written.map(str::to_string),
                reading: reading.map(str::to_string),
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

    Presentation { top, collapsed, all_cards, sentence: None, surface: None }
}

/// The physical-pixel space between the text and its outline.
pub const HIGHLIGHT_PAD: i32 = 3;

/// Computes the highlight rect for matched text from the cursor position.
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
    // The hits use rank order, so the first hit has the highest rank.
    let best = group.hits[0];
    let pos = definition_tags(&best.entry.pos);
    let mut blocks = ordered_blocks(&group.hits, dicts, cfg);
    // Start duplicate removal with the Card tag line above the first Dictionary
    // heading. The next line adds no information when it has the same set.
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

/// Collects PitchRow values for one Card.
///
/// The function gets Accents from each enabled pitch Dictionary for the Card's
/// reading. It follows Pitch list order and combines equal Accent positions
/// into one row that names all source Dictionaries.
///
/// The function makes one read for one `(headword, reading)` pair. `term` is
/// the kanji headword. If no kanji headword exists, `term` is the reading. A
/// Yomitan pitch archive uses this expression. A Card without a reading has
/// no Accent, so the function makes no query.
///
/// The **pitch** list controls selection and order. [`ordered_blocks`] follows
/// the terms list in the same way. An unlisted Dictionary contributes nothing.
/// A listed Dictionary uses its list position. `dict_id` resolves equal
/// positions and gives a stable order. The two lists are independent. A
/// Dictionary can lead the pitch list while the terms list disables it.
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
        // Compare only `position` because the row shows only this value. Keep the
        // Accent from the first source, so the highest-priority Dictionary supplies
        // the markers.
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

/// Converts a Card to a one-line CollapsedRow.
///
/// A Gloss can contain Dictionary line breaks, but a CollapsedRow has one
/// line. The function replaces each break with the panel separator before it
/// limits the text. The limit counts visible characters.
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

/// Moves a selected CollapsedRow to the top.
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

/// Builds one GlossBlock per Dictionary rather than per Hit.
///
/// The function groups hits by `dict_id`, which identifies the Dictionary.
/// The enabled terms list gives each group its rank. `dict_id` resolves equal
/// ranks. A stable sort keeps Hit order within each group.
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

/// Selects part-of-speech tags for display.
///
/// The function removes tags that contain only digits because numeric tags
/// identify Sense numbers, not grammatical parts of speech.
fn definition_tags(pos: &[String]) -> Vec<String> {
    pos.iter().filter(|t| !is_number(t)).cloned().collect()
}

/// Returns true if the trimmed tag contains only numeric digits.
fn is_number(tag: &str) -> bool {
    let tag = tag.trim();
    !tag.is_empty() && tag.chars().all(char::is_numeric)
}

/// Removes duplicate part-of-speech tags from consecutive Entries.
///
/// When an Entry has the same tags as the prior Entry, the function clears
/// the second Entry's tags.
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

/// Gets a Dictionary name from its ID.
fn dict_name_for(dict_id: i64, dicts: &[DictInfo]) -> String {
    dicts
        .iter()
        .find(|d| d.dict_id == dict_id)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| format!("dict {dict_id}"))
}

/// Tests whether an enabled Dictionary name list contains a Dictionary.
///
/// If `dicts` has no ID for the Dictionary, the function returns true.
/// This rule keeps a new Dictionary visible.
fn enabled_for(dict_id: i64, dicts: &[DictInfo], list: &[String]) -> bool {
    match dicts.iter().find(|d| d.dict_id == dict_id) {
        Some(known) => keeps_dict(&known.name, list),
        None => true,
    }
}

/// Returns the priority index of a Dictionary name in a Config list.
///
/// A match requires complete-name equality. The function returns `None` when
/// no list name equals `dict_name`.
pub fn list_rank(dict_name: &str, list: &[String]) -> Option<usize> {
    list.iter().position(|listed| listed == dict_name)
}

/// Tests whether a Config list contains a Dictionary name.
///
/// Exact string equality decides a match. An empty list enables no Dictionaries.
pub fn keeps_dict(dict_name: &str, list: &[String]) -> bool {
    list.iter().any(|listed| listed == dict_name)
}

/// Limits a string to a maximum number of characters.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Replaces newline characters with semicolons.
/// Returns a borrowed slice when the string has no newline.
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

    /// Returns a mock dictionary without Pitch accent data.
    fn no_pitch() -> FakeDictionary {
        FakeDictionary::new()
    }

    /// Creates a mock dictionary with the specified Pitch accent claims.
    fn with_pitch(claims: &[(i64, &str, &str, Accent)]) -> FakeDictionary {
        let mut dict = FakeDictionary::new();
        for (dict_id, term, reading, accent) in claims {
            dict.add_pitch(term, reading, *dict_id, accent.clone());
        }
        dict
    }

    /// Creates an Accent with a downstep position and no tags.
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

    /// Creates a GlossBlock from plain string slices.
    fn strings(dict: &str, glosses: &[&str]) -> GlossBlock {
        GlossBlock::parse(dict, &serde_json::json!(glosses).to_string())
    }

    /// Creates a Hit with a default noun tag.
    fn hit(written: &str, reading: &str, dict_id: i64, gloss: &str) -> Hit {
        hit_tagged(written, reading, dict_id, gloss, &["noun"])
    }

    /// Creates a Hit with specified part-of-speech tags.
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

    /// Verifies that multiple rows from one dictionary combine into one block.
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

    /// Verifies that dictionary configuration order determines block sequence.
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

    /// A term bank can leave the reading empty. Babylon does so on 9 990 of its 10 000
    /// rows, and 大辞泉 does so on alias rows. The build then stores the term as its own
    /// reading with no written form, the shape of a kana-only headword.
    ///
    /// Such a row must not open a Card of its own above the real one. It joins the Card
    /// with the same written form, and that Card keeps the reading and pitch that the
    /// other Dictionary supplies. Here the reading-less row ranks first, as it did for
    /// 銀の匙 in the library.
    #[test]
    fn a_row_with_no_reading_joins_the_card_that_has_one() {
        let mut orphan = hit("銀の匙", "銀の匙", 1, "n. silver spoon");
        orphan.written = None;
        let hits = vec![orphan, hit("銀の匙", "ぎんのさじ", 2, "銀で作ったさじ。")];
        let p = build(&hits, &dicts(), &cfg(), &with_pitch(&[(2, "銀の匙", "ぎんのさじ", downstep(0))]));
        assert_eq!(1, p.all_cards.len(), "one word, one card: {:?}", p.all_cards);
        let card = p.top.expect("a top card");
        assert_eq!(Some("銀の匙"), card.written.as_deref());
        assert_eq!(Some("ぎんのさじ"), card.reading.as_deref(), "borrowed from 大辞林");
        assert_eq!(1, card.pitch.len(), "and the pitch of that reading");
        assert_eq!(2, card.blocks.len(), "both Dictionaries gloss the one card");
    }

    /// A reading-less row with no Dictionary to borrow from keeps its own Card. The Card
    /// has the written form and no reading. The old shape claimed the kanji string as a
    /// reading, so a mined note put 銀の in its reading field.
    #[test]
    fn a_row_with_no_reading_and_no_sibling_has_a_written_form_and_no_reading() {
        let mut orphan = hit("銀の", "銀の", 1, "adj. silver");
        orphan.written = None;
        let p = build(&[orphan], &dicts(), &cfg(), &no_pitch());
        let card = p.top.expect("a top card");
        assert_eq!(Some("銀の"), card.written.as_deref());
        assert_eq!(None, card.reading.as_deref());
    }

    /// A kana-only headword keeps its shape. Its reading is the headword, and the popup
    /// shows it once.
    #[test]
    fn a_kana_only_headword_keeps_its_reading_as_its_headword() {
        let mut cat = hit("ねこ", "ねこ", 1, "cat");
        cat.written = None;
        let p = build(&[cat], &dicts(), &cfg(), &no_pitch());
        let card = p.top.expect("a top card");
        assert_eq!(None, card.written.as_deref());
        assert_eq!(Some("ねこ"), card.reading.as_deref());
    }

    /// Verifies that consecutive entries suppress duplicate tag displays.
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

    /// Verifies that numeric tags do not appear in output tag lists.
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

    /// Verifies that exact names keep Dictionaries with shared prefixes separate.
    #[test]
    fn two_dictionaries_sharing_a_substring_are_enabled_independently() {
        let list = vec!["大辞林　第四版".to_string()];
        assert!(keeps_dict("大辞林　第四版", &list));
        assert!(!keeps_dict("大辞林　第三版", &list));
        assert!(!keeps_dict("大辞林", &list), "the shared prefix names nothing on its own");
    }

    /// Verifies that an empty terms list produces no output cards.
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

    /// Verifies that list rank orders listed names and rejects unlisted names.
    #[test]
    fn a_list_ranks_the_names_it_holds_and_no_others() {
        let list = vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()];
        assert_eq!(Some(0), list_rank("大辞林　第四版", &list));
        assert_eq!(Some(1), list_rank("Jitendex.org [2026-07-09]", &list));
        assert_eq!(None, list_rank("Jitendex", &list), "a prefix is not the name");
        assert_eq!(None, list_rank("大辞林　第四版", &[]));
    }

    /// Verifies that an empty string in a configuration list matches nothing.
    #[test]
    fn a_blank_list_entry_names_nothing() {
        let with_blank = vec![String::new(), "Jitendex.org".to_string()];
        assert!(!keeps_dict("", &["Jitendex.org".to_string()]));
        assert_eq!(Some(1), list_rank("Jitendex.org", &with_blank));
    }

    /// Verifies that an unlisted Dictionary ID still produces output.
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

    /// Creates a mock text span with 30-pixel character geometries.
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

    /// Verifies that spans without character geometry produce no highlight rectangle.
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

    /// Verifies that multi-line glosses convert into single-line summaries.
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

    /// Verifies that truncation occurs after newline replacement.
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

    /// Verifies that note payloads include sentence text when present.
    #[test]
    fn sentence_source_appears_in_fields_when_set() {
        let mut p = build(&[hit("猫", "ねこ", 1, "cat")], &dicts(), &cfg(), &no_pitch());
        p.sentence = Some("その猫はかわいい。".to_string());

        let none = crate::select::CardSelection::default();
        let (_, fields) = crate::controller::note_payload(
            &p,
            false,
            true,
            &none,
            crate::dict::gloss::Separator::Ellipsis,
        );

        assert_eq!(Some(&"その猫はかわいい。".to_string()), fields.get("sentence"));
    }

    // ---- pitch

    /// Verifies that identical pitch accents collapse into a single row.
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

    /// Verifies that distinct pitch accents produce separate rows in priority order.
    #[test]
    fn two_dictionaries_giving_different_accents_draw_two_rows_in_list_order() {
        // The test seeds the lower-priority Dictionary first. This order catches code
        // that keeps stored order instead of using Pitch list order.
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

    /// Verifies that multiple Dictionaries that report one Accent share a row.
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

    /// Verifies that accent position determines equality regardless of mora markers.
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

    /// Verifies that kana-only terms query pitch data using reading text.
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

    /// Verifies that exclusion from the pitch list removes accent data.
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

    /// Verifies that cards without readings do not query pitch data.
    #[test]
    fn a_card_with_no_reading_draws_no_pitch() {
        let mut dict = FakeDictionary::new();
        dict.add_pitch("昨日", "きのう", 1, downstep(1));
        let hits = vec![Hit { reading: None, ..hit("昨日", "きのう", 1, "yesterday") }];

        let card = build(&hits, &dicts(), &cfg(), &dict).top.expect("a top card");

        assert!(card.pitch.is_empty());
    }

    /// Verifies that promoted cards retain their pitch accent data.
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

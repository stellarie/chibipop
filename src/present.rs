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

/// The OCR source and match length for the current top Card.
///
/// The source keeps enough text to derive a new surface when `swap_top`
/// promotes a Card with a different match length.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrSurface {
    source: String,
    match_len: usize,
}

impl OcrSurface {
    /// Keep the trimmed OCR source and the match length of the current Card.
    pub fn new(source: &str, match_len: usize) -> Option<Self> {
        let source = source.trim_start();
        (!source.is_empty()).then(|| Self { source: source.to_string(), match_len })
    }

    /// Return the surface for the current Card without allocating.
    pub fn as_str(&self) -> Option<&str> {
        let end = self
            .source
            .char_indices()
            .nth(self.match_len)
            .map_or(self.source.len(), |(at, _)| at);
        (!self.source[..end].is_empty()).then_some(&self.source[..end])
    }

    fn set_match_len(&mut self, match_len: usize) {
        self.match_len = match_len;
    }
}

/// The popup view model for one hover.
///
/// `surface` keeps the OCR source in a small value so all Presentation
/// literals can keep using `surface: None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    pub top: Option<Card>,
    pub collapsed: Vec<CollapsedRow>,
    /// A complete [`Card`] for each group.
    pub all_cards: Vec<Card>,
    /// The OCR sentence line that `worker.rs` sets for note payloads.
    pub sentence: Option<String>,
    /// The OCR source and current top-Card surface for note payloads.
    pub surface: Option<OcrSurface>,
}

/// The top card's expression.
pub fn top_expr(p: &Presentation) -> &str {
    p.top
        .as_ref()
        .and_then(|c| c.written.as_deref().or(c.reading.as_deref()))
        .unwrap_or("")
}

/// The complete top [`Card`].
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub written: Option<String>,
    pub reading: Option<String>,
    pub pos: Vec<String>,
    /// The Inflection chain: the conjugation and auxiliary steps between the headword
    /// and the hovered text, in build order from the headword outward.
    ///
    /// `inflection_chain` has already filtered this list for display.
    /// The chain lives on the Card, not on a GlossBlock or GlossEntry.
    /// It belongs to the match, the same Hit that supplies `match_len`, not to one Dictionary.
    pub inflections: Vec<String>,
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
    pub updated: HashSet<String>,
    /// True if an add may overwrite a known duplicate.
    pub update_dupes: bool,
    pub enabled: bool,
    pub adding: bool,
    pub saving: bool,
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
            updated: HashSet::new(),
            update_dupes: false,
            enabled: false,
            adding: false,
            saving: false,
            checking: false,
            connected: false,
            failed: false,
        }
    }

    /// Returns the initial state of a new popup.
    /// The popup checks for duplicates when Anki is enabled.
    pub fn fresh(enabled: bool, update_dupes: bool) -> Self {
        Self {
            enabled,
            update_dupes,
            checking: enabled,
            connected: enabled,
            ..Self::disabled()
        }
    }

    /// True if an add for `expr` must do nothing.
    pub fn blocks_add(&self, expr: &str) -> bool {
        self.dupes.contains(expr) && !self.update_dupes
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
        inflections: inflection_chain(&best.process),
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
    if let Some(surface) = p.surface.as_mut() {
        surface.set_match_len(p.all_cards[0].match_len);
    }
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

/// Builds the Inflection chain in build order from the headword outward.
///
/// `Hit::process` stores the steps outermost first, so this function reverses them.
/// It drops empty steps. It drops a parenthesized stem such as `(unstressed infinitive)`
/// unless the stem is `process[0]`, the outermost step. That step stays because a bare
/// te-form such as 風邪をひいて still explains its match with `(te form)`. Nazeka uses
/// this filter.
///
/// The design rejects a relabel table in code. The labels are the `detail` strings of
/// `data/deconjugator.json`, so a relabel is a change to the rule file.
pub fn inflection_chain(process: &[String]) -> Vec<String> {
    process
        .iter()
        .enumerate()
        .rev()
        .filter(|&(i, step)| {
            !step.is_empty()
                && !(i != 0 && step.starts_with('(') && step.ends_with(')'))
        })
        .map(|(_, step)| step.clone())
        .collect()
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

    // ---- pitch

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

}

//! Core data types and the `Dictionary` interface.

use crate::dict::gloss::{plain_items, pos_labels, GlossDoc};
use crate::dict::media::Intrinsic;
use crate::dict::pitch::{Accent, PitchClaim};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct TermRow {
    pub surface: String,
    pub written: Option<String>,
    pub reading: Option<String>,
    /// Yomitan `rules` field, with values separated by spaces. Valid values are
    /// `v1`, `v5`, `vs`, `vz`, `vk`, `adj-i`, and the empty string for an unknown
    /// part of speech. The deconjugator uses finer JMdict-style tags, such as
    /// `v5k`. `engine.rs`'s `dict_pos_for` maps those tags to this set before it
    /// filters rows.
    pub pos: String,
    /// Frequency rank. A lower value means that the term is more common. `None`
    /// means no rank.
    pub freq: Option<i64>,
    pub entry_id: i64,
    /// The build copies this value from `entry.dict_id` for the same reason as
    /// `pos`. The lookup path groups rows and ranks Dictionaries without a join.
    pub dict_id: i64,
}

/// One Dictionary record for one headword. A lookup returns this unit.
///
/// The previous Dictionary builder used a `Vec<Sense>` here. It left the data
/// unchanged. The builder always wrote one item. Its gloss vector held the
/// whole flattened glossary from the term-bank row. No code wrote its `misc` field.
/// Every caller therefore flattened a one-item vector.
/// `CONTEXT.md` reserves `Sense` for one distinct meaning inside the tree as a
/// sibling block.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub entry_id: i64,
    pub dict_id: i64,
    /// The Entry's gloss content, parsed from the raw structured content that the
    /// record stores. The value uses `Arc` because `SqliteDictionary`'s parsed-tree
    /// cache returns clones of one parse. A clone of an `Arc` keeps that cache useful.
    pub gloss: Arc<GlossDoc>,
    /// Part-of-speech labels from the tree. The popup renders them in the card's
    /// own field, not in the gloss.
    pub pos: Vec<String>,
    /// Intrinsic size for each image asset that this row's tree names and the media
    /// store has bytes for. The map key is the `path` that the node declares.
    ///
    /// The code resolves this data beside the parse, not during layout.
    /// `layout::scene` only measures and computes. It has no database. A size
    /// query for each image on each frame would put SQLite on the paint path. A
    /// row names a few assets. 字通 averages more than four, so a short association
    /// list and a linear scan are enough.
    ///
    /// An absent path means that the build stored no bytes or could not read an
    /// intrinsic size. The `alt`-text fallback uses this state (`dict::media`).
    pub media: Vec<(String, Intrinsic)>,
    /// Reported frequency for this record's headword. It is the number that the
    /// highest-priority enabled frequency Dictionary has published for the
    /// headword.
    ///
    /// The popup prints this value, not the reduced Frequency rank that
    /// [`TermRow::freq`] carries (ARCHITECTURE.md#dictionary-and-lookup). The
    /// frequency list selects the first claim under every Ranking strategy. The
    /// displayed number therefore comes from one real Dictionary. It is not a
    /// value computed from several Dictionaries.
    ///
    /// `None` means either that no enabled frequency Dictionary ranks the headword
    /// or that the `Entry` has no store.
    pub reported_freq: Option<i64>,
}

impl Entry {
    /// Wrap an already parsed tree and copy its labels to the `Entry`.
    pub fn new(
        entry_id: i64,
        dict_id: i64,
        gloss: Arc<GlossDoc>,
        media: Vec<(String, Intrinsic)>,
        reported_freq: Option<i64>,
    ) -> Entry {
        let pos = pos_labels(&gloss);
        Entry { entry_id, dict_id, gloss, pos, media, reported_freq }
    }

    /// Parse a raw glossary payload. The hover path uses `SqliteDictionary`'s
    /// cache. Tests use this path for fixtures.
    ///
    /// A tree parsed from a string has no store. Therefore, this `Entry` has no
    /// media or Reported frequency. Its images use their declared size and then
    /// fall back to their `alt` text.
    pub fn parse(entry_id: i64, dict_id: i64, glossary: &str) -> Entry {
        Entry::new(entry_id, dict_id, Arc::new(GlossDoc::parse(glossary)), Vec::new(), None)
    }

    /// Return one plain-text gloss for each glossary item.
    pub fn glosses(&self) -> Vec<String> {
        plain_items(&self.gloss)
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub written: Option<String>,
    pub reading: Option<String>,
    /// Number of input characters that this match consumes.
    pub match_len: usize,
    pub freq: Option<i64>,
    pub score: f64,
    /// Deconjugation steps, with the outermost step first.
    pub process: Vec<String>,
    pub entry: Entry,
}

pub trait Dictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>>;
    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>>;

    /// Dictionary identities. The `dict` table has few rows. This is not a hot-path
    /// call.
    fn dicts(&self) -> Result<Vec<crate::present::DictInfo>>;

    /// Every stored Pitch pattern for one headword and reading, from each Dictionary
    /// that made a claim.
    ///
    /// `term` is the headword that the archive names. It uses the kanji spelling,
    /// or the kana spelling when no kanji exists. `reading` limits the result to
    /// one reading. The pitch list in config decides which Dictionaries are
    /// enabled and how it orders them. This method returns all claims without
    /// filters or order (ARCHITECTURE.md#dictionary-and-lookup).
    ///
    /// The popup calls this method once for each card. It does not call it on the
    /// term path because a Pitch pattern belongs to a reading, not a `term` row.
    ///
    /// **Total.** A store fault returns no Pitch pattern, not an error for the
    /// hover. This matches the missing-image path in
    /// `SqliteDictionary::media_sizes`. A Pitch pattern appears in one card
    /// header, so the whole hover can continue.
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim>;
}

/// In-memory `Dictionary` for tests.
#[derive(Default)]
pub struct FakeDictionary {
    terms: HashMap<String, Vec<TermRow>>,
    entries: HashMap<i64, Entry>,
    dicts: Vec<crate::present::DictInfo>,
    /// Headword and reading mapped to their claims. Claims keep seed order, which
    /// matches the order of stored rows.
    pitch: HashMap<(String, String), Vec<PitchClaim>>,
}

impl FakeDictionary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one `term` row directly. The arguments are the row's columns and nothing
    /// else. A struct here would repeat `TermRow`.
    pub fn add_term(
        &mut self,
        surface: &str,
        written: Option<&str>,
        reading: Option<&str>,
        pos: &str,
        freq: Option<i64>,
        entry_id: i64,
        dict_id: i64,
    ) {
        self.terms.entry(surface.to_string()).or_default().push(TermRow {
            surface: surface.to_string(),
            written: written.map(str::to_string),
            reading: reading.map(str::to_string),
            pos: pos.to_string(),
            freq,
            entry_id,
            dict_id,
        });
    }

    /// Seed one Entry from a raw glossary payload exactly as the record stores it.
    /// This fixture uses the real parser instead of a hand-built tree that cannot
    /// occur in a stored Entry.
    pub fn add_entry(&mut self, entry_id: i64, dict_id: i64, glossary: &str) {
        self.entries.insert(entry_id, Entry::parse(entry_id, dict_id, glossary));
    }

    pub fn add_dict(&mut self, dict_id: i64, name: &str) {
        self.dicts.push(crate::present::DictInfo { dict_id, name: name.to_string() });
    }

    /// Add one Dictionary's claim for one headword and reading.
    pub fn add_pitch(&mut self, term: &str, reading: &str, dict_id: i64, accent: Accent) {
        self.pitch
            .entry((term.to_string(), reading.to_string()))
            .or_default()
            .push(PitchClaim { dict_id, accent });
    }
}

impl Dictionary for FakeDictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        Ok(self.terms.get(surface).cloned().unwrap_or_default())
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        Ok(ids.iter().filter_map(|i| self.entries.get(i).cloned()).collect())
    }

    fn dicts(&self) -> Result<Vec<crate::present::DictInfo>> {
        Ok(self.dicts.clone())
    }

    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        self.pitch
            .get(&(term.to_string(), reading.to_string()))
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structured-content glossary with one part-of-speech pill and one gloss.
    /// The census reports one glossary item in 64 of 72 Dictionaries. It does not
    /// report this exact node shape.
    fn one_sense(gloss: &str, pos: &str) -> String {
        serde_json::json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": pos},
            {"tag": "div", "content": gloss}
        ]}])
        .to_string()
    }

    #[test]
    fn an_entry_carries_its_glosses_and_its_labels_directly() {
        let e = Entry::parse(1, 1, &one_sense("to eat", "v1"));
        assert_eq!(vec!["to eat".to_string()], e.glosses());
        assert_eq!(vec!["v1".to_string()], e.pos);
    }

    #[test]
    fn a_plain_string_glossary_needs_no_structured_content() {
        let e = Entry::parse(1, 1, r#"["cat","feline"]"#);
        assert_eq!(vec!["cat".to_string(), "feline".to_string()], e.glosses());
        assert!(e.pos.is_empty());
    }

    /// An unreadable record becomes an Entry with no glosses, not a failed lookup.
    /// A hover has nothing useful to do with this error.
    #[test]
    fn an_unreadable_record_yields_an_empty_entry() {
        let e = Entry::parse(1, 1, "not json");
        assert!(e.glosses().is_empty());
        assert!(e.pos.is_empty());
    }

    #[test]
    fn fake_dictionary_returns_seeded_rows() {
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v1", Some(7), 1, 1);
        assert_eq!(1, d.terms_for("食べる").unwrap().len());
        assert!(d.terms_for("猫").unwrap().is_empty());
    }

    #[test]
    fn fake_dictionary_returns_seeded_entries() {
        let mut d = FakeDictionary::new();
        d.add_entry(1, 1, &one_sense("to eat", "v1"));
        assert_eq!(1, d.entries(&[1]).unwrap().len());
    }
}

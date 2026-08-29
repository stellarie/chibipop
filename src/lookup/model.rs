//! Core data types and the `Dictionary` abstraction.

use crate::dict::gloss::{plain_items, pos_labels, GlossDoc};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct TermRow {
    pub surface: String,
    pub written: Option<String>,
    pub reading: Option<String>,
    /// Yomitan `rules` field, space-separated. The real vocabulary is
    /// exactly `v1`, `v5`, `vs`, `vz`, `vk`, `adj-i`, and the empty string
    /// (unknown part of speech) - never the deconjugator's fine-grained
    /// JMdict-style subtypes (e.g. `v5k`). `engine.rs`'s `dict_pos_for` maps
    /// those fine-grained deconjugator tags onto this coarse vocabulary
    /// before filtering.
    pub pos: String,
    /// Rank; lower is more common. `None` means unranked.
    pub freq: Option<i64>,
    pub entry_id: i64,
    /// Denormalised from `entry.dict_id` (same reason as `pos`): grouping
    /// and dictionary-priority ranking cost no join on the hot path.
    pub dict_id: i64,
}

/// One dictionary's record for one headword - the unit a lookup returns.
///
/// There used to be a `Vec<Sense>` here. It was a pass-through: the builder
/// always wrote exactly one, its gloss vec held the whole term-bank row's
/// flattened glossary, and its `misc` field was never populated anywhere in
/// the codebase - so every call site flat-mapped over a one-element vec.
/// `CONTEXT.md` reserves the word Sense for what it means: one distinct
/// meaning, living inside the tree as a sibling block.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub entry_id: i64,
    pub dict_id: i64,
    /// The Entry's gloss content, parsed from the raw structured content the
    /// record stores. Shared rather than owned because
    /// `SqliteDictionary`'s parsed-tree cache hands out clones of one parse,
    /// and cloning an `Arc` is what makes that cache worth having.
    pub gloss: Arc<GlossDoc>,
    /// Part-of-speech labels lifted out of the tree, which the popup renders
    /// as the card's own field rather than inline.
    pub pos: Vec<String>,
}

impl Entry {
    /// Wraps an already-parsed tree, lifting its labels out.
    pub fn new(entry_id: i64, dict_id: i64, gloss: Arc<GlossDoc>) -> Entry {
        let pos = pos_labels(&gloss);
        Entry { entry_id, dict_id, gloss, pos }
    }

    /// Parses a raw glossary payload. The hover path goes through
    /// `SqliteDictionary`'s cache instead; this is the fixture path.
    pub fn parse(entry_id: i64, dict_id: i64, glossary: &str) -> Entry {
        Entry::new(entry_id, dict_id, Arc::new(GlossDoc::parse(glossary)))
    }

    /// The plain-text glosses, one per glossary item.
    pub fn glosses(&self) -> Vec<String> {
        plain_items(&self.gloss)
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub written: Option<String>,
    pub reading: Option<String>,
    /// Characters of input consumed by this match.
    pub match_len: usize,
    pub freq: Option<i64>,
    pub score: f64,
    /// Deconjugation trace, outermost step first.
    pub process: Vec<String>,
    pub entry: Entry,
}

pub trait Dictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>>;
    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>>;

    /// Dictionary identities, read once at startup. The `dict` table has a
    /// handful of rows; this is not a hot-path call.
    fn dicts(&self) -> Result<Vec<crate::present::DictInfo>>;
}

/// In-memory `Dictionary` for tests.
#[derive(Default)]
pub struct FakeDictionary {
    terms: HashMap<String, Vec<TermRow>>,
    entries: HashMap<i64, Entry>,
    dicts: Vec<crate::present::DictInfo>,
}

impl FakeDictionary {
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Seeds one entry from a raw glossary payload, exactly as the record
    /// stores it - so a fixture exercises the real parser instead of a
    /// hand-built tree that could not occur.
    pub fn add_entry(&mut self, entry_id: i64, dict_id: i64, glossary: &str) {
        self.entries.insert(entry_id, Entry::parse(entry_id, dict_id, glossary));
    }

    pub fn add_dict(&mut self, dict_id: i64, name: &str) {
        self.dicts.push(crate::present::DictInfo { dict_id, name: name.to_string() });
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structured-content glossary with one part-of-speech pill and one
    /// gloss - the shape 64 of 72 census dictionaries emit.
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

    /// An unreadable record is an entry with no glosses, never a failed
    /// lookup: a hover has nothing useful to do with an error here.
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

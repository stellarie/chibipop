//! Core data types and the `Dictionary` abstraction.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sense {
    pub glosses: Vec<String>,
    /// Same glosses, HTML-formatted, for callers that want formatting (the
    /// Anki `glossary_html` field) instead of flattened text. Absent in
    /// databases built before this field existed - `serde(default)` reads
    /// those as empty rather than failing, though `SqliteDictionary::open`'s
    /// schema_version check means that path is only reachable in tests, not
    /// in a real chibipop.sqlite.
    #[serde(default)]
    pub glosses_html: Vec<String>,
    #[serde(default)]
    pub pos: Vec<String>,
    #[serde(default)]
    pub misc: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub entry_id: i64,
    pub dict_id: i64,
    pub senses: Vec<Sense>,
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

    pub fn add_entry(&mut self, entry_id: i64, dict_id: i64, senses: Vec<Sense>) {
        self.entries.insert(entry_id, Entry { entry_id, dict_id, senses });
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

    #[test]
    fn sense_roundtrips_through_json() {
        let json = r#"[{"glosses":["to eat"],"pos":["v1"],"misc":[]}]"#;
        let senses: Vec<Sense> = serde_json::from_str(json).unwrap();
        assert_eq!(vec!["to eat".to_string()], senses[0].glosses);
        assert_eq!(vec!["v1".to_string()], senses[0].pos);
    }

    #[test]
    fn sense_tolerates_missing_optional_fields() {
        let senses: Vec<Sense> =
            serde_json::from_str(r#"[{"glosses":["cat"]}]"#).unwrap();
        assert!(senses[0].pos.is_empty());
        assert!(senses[0].misc.is_empty());
        assert!(senses[0].glosses_html.is_empty());
    }

    #[test]
    fn sense_roundtrips_glosses_html() {
        let json = r#"[{"glosses":["to eat"],"glosses_html":["<b>to eat</b>"]}]"#;
        let senses: Vec<Sense> = serde_json::from_str(json).unwrap();
        assert_eq!(vec!["<b>to eat</b>".to_string()], senses[0].glosses_html);
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
        d.add_entry(1, 1, vec![Sense {
            glosses: vec!["to eat".into()],
            glosses_html: vec![],
            pos: vec!["v1".into()],
            misc: vec![],
        }]);
        assert_eq!(1, d.entries(&[1]).unwrap().len());
    }
}

//! This module filters a Dictionary to enabled dictionaries.

use crate::dict::pitch::PitchClaim;
use crate::lookup::model::{Dictionary, Entry, TermRow};
use crate::present::{DictInfo, PresentConfig};
use anyhow::Result;

pub struct EnabledDictionary<'a> {
    dictionary: &'a dyn Dictionary,
    ids: Vec<i64>,
}

impl<'a> EnabledDictionary<'a> {
    pub fn new(dictionary: &'a dyn Dictionary, dicts: &[DictInfo], cfg: &PresentConfig) -> Self {
        Self {
            dictionary,
            ids: dicts.iter().filter(|dict| cfg.terms.contains(&dict.name))
                .map(|dict| dict.dict_id).collect(),
        }
    }
}

impl Dictionary for EnabledDictionary<'_> {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        Ok(self.dictionary.terms_for(surface)?.into_iter()
            .filter(|row| self.ids.contains(&row.dict_id)).collect())
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> { self.dictionary.entries(ids) }
    fn dicts(&self) -> Result<Vec<DictInfo>> { self.dictionary.dicts() }
    fn tagged_dicts(&self) -> Result<Vec<i64>> { self.dictionary.tagged_dicts() }
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        self.dictionary.pitch_for(term, reading)
    }
}

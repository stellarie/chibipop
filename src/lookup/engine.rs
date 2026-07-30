//! The lookup engine.

use crate::lookup::deconj::{Deconjugator, Form};
use crate::lookup::model::{Dictionary, Entry, Hit, TermRow};
use anyhow::Result;
use std::cmp::Reverse;
use std::collections::HashMap;

pub const MAX_LOOKUP_CHARS: usize = 25;
pub const MAX_RESULTS: usize = 10;
const DEFAULT_FREQ: f64 = 999_999.0;
const SEPARATORS: &[char] = &['、', '。', '「', '」', '！', '？', '…', '\n'];

/// Trim, cut, truncate.
pub fn clean_input(text: &str) -> String {
    let trimmed = text.trim();
    let cut = match trimmed.find(SEPARATORS) {
        Some(i) => &trimmed[..i],
        None => trimmed,
    };
    cut.chars().take(MAX_LOOKUP_CHARS).collect()
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

/// From weikipop's priority.
fn score(match_len: usize, freq: Option<i64>, kana_bonus: bool, steps: usize) -> f64 {
    let f = freq.map(|v| v as f64).unwrap_or(DEFAULT_FREQ).max(1.0);
    let mut s = match_len as f64;
    s += 10.0 * (1.0 - f.ln() / DEFAULT_FREQ.ln());
    if kana_bonus {
        s += 3.0;
    }
    s -= steps as f64;
    s
}

/// Fine dec_tag to coarse pos.
fn dict_pos_for(dec_tag: &str) -> Option<Option<&'static str>> {
    match dec_tag {
        "v5aru" | "v5b" | "v5g" | "v5k" | "v5k-s" | "v5m" | "v5n" | "v5r"
        | "v5r-i" | "v5s" | "v5t" | "v5u" | "v5u-s" => Some(Some("v5")),
        "vs-i" => Some(Some("vs")),
        "adj-i" => Some(Some("adj-i")),
        "v1" => Some(Some("v1")),
        "vk" => Some(Some("vk")),
        // No POS signal.
        "exp" | "topic-condition" | "uninflectable" => Some(None),
        // Unrecognised: fail open.
        _ => None,
    }
}

struct Candidate {
    row: TermRow,
    match_len: usize,
    steps: usize,
    process: Vec<String>,
}

/// One result slot.
type GroupKey = (Option<String>, Option<String>, i64);

/// Deterministic total order.
fn is_better(new: &Candidate, existing: &Candidate) -> bool {
    fn rank(c: &Candidate) -> (usize, Reverse<usize>, Reverse<&Vec<String>>, Reverse<i64>) {
        (c.match_len, Reverse(c.steps), Reverse(&c.process), Reverse(c.row.entry_id))
    }
    rank(new) > rank(existing)
}

pub struct LookupEngine {
    deconjugator: Deconjugator,
}

impl LookupEngine {
    pub fn new(deconjugator: Deconjugator) -> Self {
        LookupEngine { deconjugator }
    }

    pub fn run<D: Dictionary>(&self, dict: &D, text: &str) -> Result<Vec<Hit>> {
        let cleaned = clean_input(text);
        if cleaned.is_empty() {
            return Ok(Vec::new());
        }

        let chars: Vec<char> = cleaned.chars().collect();
        let mut best: HashMap<GroupKey, Candidate> = HashMap::new();

        // No early exit: forms differ.
        for prefix_len in (1..=chars.len()).rev() {
            let prefix: String = chars[..prefix_len].iter().collect();

            // It seeds the plain form.
            let forms: Vec<Form> = self.deconjugator.deconjugate(&prefix)
                .into_iter()
                .collect();

            for form in &forms {
                let required_pos =
                    form.tags.last().map(String::as_str).and_then(dict_pos_for).flatten();
                for row in dict.terms_for(&form.text)? {
                    if let Some(need) = required_pos {
                        if !row.pos.is_empty()
                            && !row.pos.split_whitespace().any(|p| p == need)
                        {
                            continue;
                        }
                    }
                    let key: GroupKey = (row.written.clone(), row.reading.clone(), row.dict_id);
                    let candidate = Candidate {
                        row,
                        match_len: prefix_len,
                        steps: form.process.len(),
                        process: form.process.clone(),
                    };
                    let should_replace = match best.get(&key) {
                        Some(existing) => is_better(&candidate, existing),
                        None => true,
                    };
                    if should_replace {
                        best.insert(key, candidate);
                    }
                }
            }
        }

        let mut ranked: Vec<Candidate> = best.into_values().collect();

        // Unconjugated all-kana only.
        ranked.sort_by(|a, b| {
            let sa = score(
                a.match_len,
                a.row.freq,
                a.steps == 0 && a.row.surface.chars().all(is_kana),
                a.steps,
            );
            let sb = score(
                b.match_len,
                b.row.freq,
                b.steps == 0 && b.row.surface.chars().all(is_kana),
                b.steps,
            );
            b.match_len
                .cmp(&a.match_len)
                .then(sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal))
                // dict_id ascends by priority.
                .then(a.row.dict_id.cmp(&b.row.dict_id))
                .then(a.row.entry_id.cmp(&b.row.entry_id))
        });
        ranked.truncate(MAX_RESULTS);

        let ids: Vec<i64> = ranked.iter().map(|c| c.row.entry_id).collect();
        let entries: HashMap<i64, Entry> = dict
            .entries(&ids)?
            .into_iter()
            .map(|e| (e.entry_id, e))
            .collect();

        Ok(ranked
            .into_iter()
            .filter_map(|c| {
                let entry = entries.get(&c.row.entry_id)?.clone();
                let kana_bonus =
                    c.steps == 0 && c.row.surface.chars().all(is_kana);
                Some(Hit {
                    score: score(c.match_len, c.row.freq, kana_bonus, c.steps),
                    written: c.row.written.clone(),
                    reading: c.row.reading.clone(),
                    match_len: c.match_len,
                    freq: c.row.freq,
                    process: c.process,
                    entry,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::{FakeDictionary, Sense};
    use crate::lookup::rules::load_rules;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn engine() -> LookupEngine {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/deconjugator.json");
        LookupEngine::new(Deconjugator::new(load_rules(&p).unwrap()))
    }

    fn sense(gloss: &str, pos: &str) -> Sense {
        Sense {
            glosses: vec![gloss.to_string()],
            pos: vec![pos.to_string()],
            misc: vec![],
        }
    }

    fn taberu_dict() -> FakeDictionary {
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v1", Some(500), 1, 1);
        d.add_entry(1, 1, vec![sense("to eat", "v1")]);
        d
    }

    #[test]
    fn exact_match_found() {
        let hits = engine().run(&taberu_dict(), "食べる").unwrap();
        assert_eq!(1, hits.len());
        assert_eq!(vec!["to eat".to_string()], hits[0].entry.senses[0].glosses);
    }

    #[test]
    fn conjugated_form_deconjugates_to_headword() {
        let hits = engine().run(&taberu_dict(), "食べさせられた").unwrap();
        assert_eq!(1, hits.len());
        assert_eq!(Some("食べる".to_string()), hits[0].written);
    }

    #[test]
    fn trailing_context_is_ignored_by_prefix_scan() {
        let hits = engine().run(&taberu_dict(), "食べるのが好き").unwrap();
        assert!(!hits.is_empty());
        assert_eq!(Some("食べる".to_string()), hits[0].written);
    }

    #[test]
    fn input_cut_at_japanese_separator() {
        assert_eq!("食べる", clean_input("食べる。おいしい"));
    }

    #[test]
    fn input_truncated_by_character_not_byte_count() {
        let long = "あ".repeat(40);
        assert_eq!(MAX_LOOKUP_CHARS, clean_input(&long).chars().count());
    }

    #[test]
    fn empty_input_returns_no_hits() {
        assert!(engine().run(&taberu_dict(), "   ").unwrap().is_empty());
    }

    #[test]
    fn pos_filter_rejects_mismatched_part_of_speech() {
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v5r", Some(500), 1, 1);
        d.add_entry(1, 1, vec![sense("wrong pos", "v5r")]);
        let hits = engine().run(&d, "食べさせられた").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn dict_pos_for_maps_fine_grained_godan_tags_to_v5() {
        for tag in [
            "v5aru", "v5b", "v5g", "v5k", "v5k-s", "v5m", "v5n", "v5r",
            "v5r-i", "v5s", "v5t", "v5u", "v5u-s",
        ] {
            assert_eq!(Some(Some("v5")), dict_pos_for(tag), "tag {tag}");
        }
    }

    #[test]
    fn dict_pos_for_maps_vs_i_to_vs() {
        assert_eq!(Some(Some("vs")), dict_pos_for("vs-i"));
    }

    #[test]
    fn dict_pos_for_identity_mappings_are_explicit() {
        assert_eq!(Some(Some("adj-i")), dict_pos_for("adj-i"));
        assert_eq!(Some(Some("v1")), dict_pos_for("v1"));
        assert_eq!(Some(Some("vk")), dict_pos_for("vk"));
    }

    #[test]
    fn dict_pos_for_context_tags_impose_no_constraint() {
        assert_eq!(Some(None), dict_pos_for("exp"));
        assert_eq!(Some(None), dict_pos_for("topic-condition"));
        assert_eq!(Some(None), dict_pos_for("uninflectable"));
    }

    #[test]
    fn dict_pos_for_unrecognised_tag_imposes_no_constraint() {
        // Must fail open, not closed.
        assert_eq!(None, dict_pos_for("some-future-tag-not-yet-mapped"));
    }

    #[test]
    fn v5k_tagged_form_matches_v5_row() {
        // v5k form, v5 row.
        let mut d = FakeDictionary::new();
        d.add_term("行く", Some("行く"), Some("いく"), "v5", Some(50), 1, 1);
        d.add_entry(1, 1, vec![sense("to go", "v5")]);
        let hits = engine().run(&d, "行かなかった").unwrap();
        assert!(
            hits.iter().any(|h| h.written.as_deref() == Some("行く")),
            "expected 行く among hits, got {:?}",
            hits.iter().map(|h| h.written.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn vs_i_tagged_form_matches_vs_row() {
        // vs-i form, vs row.
        let mut d = FakeDictionary::new();
        d.add_term("する", Some("する"), Some("する"), "vs", Some(10), 1, 1);
        d.add_entry(1, 1, vec![sense("to do", "vs")]);
        let hits = engine().run(&d, "してしまった").unwrap();
        assert!(
            hits.iter().any(|h| h.written.as_deref() == Some("する")),
            "expected する among hits, got {:?}",
            hits.iter().map(|h| h.written.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn v1_tagged_form_does_not_match_v5_only_row() {
        // The filter must still bite.
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v5", Some(500), 1, 1);
        d.add_entry(1, 1, vec![sense("wrong pos", "v5")]);
        let hits = engine().run(&d, "食べさせられた").unwrap();
        assert!(hits.is_empty());
    }

    /// Guards vocabulary drift.
    #[test]
    fn all_terminal_dec_tags_are_handled_by_the_mapping() {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/deconjugator.json");
        let rules = load_rules(&p).unwrap();

        let mut terminal_tags: HashSet<String> = HashSet::new();
        for rule in &rules {
            if let Some(tags) = &rule.dec_tag {
                for t in tags.as_slice() {
                    if !t.starts_with("stem-") {
                        terminal_tags.insert(t.clone());
                    }
                }
            }
        }

        // Else the guard is vacuous.
        assert!(
            !terminal_tags.is_empty(),
            "expected at least one terminal dec_tag from the real rule file"
        );

        let mut unhandled: Vec<&String> = terminal_tags
            .iter()
            .filter(|t| dict_pos_for(t).is_none())
            .collect();
        unhandled.sort();
        assert!(
            unhandled.is_empty(),
            "dict_pos_for has no explicit arm for real terminal dec_tag(s) {unhandled:?} - \
             the rule vocabulary moved and the mapping table in engine.rs must be updated"
        );
    }

    #[test]
    fn empty_pos_column_is_never_filtered_out() {
        let mut d = FakeDictionary::new();
        d.add_term("ねこ", None, Some("ねこ"), "", None, 1, 1);
        d.add_entry(1, 1, vec![sense("cat", "")]);
        assert_eq!(1, engine().run(&d, "ねこ").unwrap().len());
    }

    #[test]
    fn more_common_word_ranks_first() {
        // Distinct ids: no grouping.
        let mut d = FakeDictionary::new();
        d.add_term("はし", None, Some("はし"), "", Some(50), 1, 1);
        d.add_entry(1, 1, vec![sense("chopsticks", "")]);
        d.add_term("はし", None, Some("はし"), "", Some(9000), 2, 2);
        d.add_entry(2, 2, vec![sense("bridge", "")]);
        let hits = engine().run(&d, "はし").unwrap();
        assert_eq!(vec!["chopsticks".to_string()], hits[0].entry.senses[0].glosses);
    }

    #[test]
    fn longer_match_outranks_shorter() {
        let mut d = FakeDictionary::new();
        d.add_term("日本", Some("日本"), Some("にほん"), "", Some(100), 1, 1);
        d.add_entry(1, 1, vec![sense("Japan", "")]);
        d.add_term("日本語", Some("日本語"), Some("にほんご"), "", Some(900), 2, 1);
        d.add_entry(2, 1, vec![sense("Japanese language", "")]);
        let hits = engine().run(&d, "日本語").unwrap();
        assert_eq!(Some("日本語".to_string()), hits[0].written);
    }

    #[test]
    fn results_truncated_to_max() {
        let mut d = FakeDictionary::new();
        for i in 1..=25 {
            // Distinct ids, else grouped.
            d.add_term("あ", None, Some("あ"), "", Some(i), i, i);
            d.add_entry(i, i, vec![sense("x", "")]);
        }
        assert_eq!(MAX_RESULTS, engine().run(&d, "あ").unwrap().len());
    }

    #[test]
    fn each_entry_appears_at_most_once() {
        let hits = engine().run(&taberu_dict(), "食べさせられた").unwrap();
        let mut ids: Vec<i64> = hits.iter().map(|h| h.entry.entry_id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len());
    }
}

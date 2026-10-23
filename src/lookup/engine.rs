//! The lookup engine resolves input against Dictionary terms.

use crate::lookup::deconj::{Deconjugator, Form};
use crate::lookup::model::{Dictionary, Entry, Hit, TermRow};
use anyhow::Result;
use std::cmp::Reverse;
use std::collections::HashMap;

pub const MAX_LOOKUP_CHARS: usize = 25;
pub const MAX_RESULTS: usize = 10;
const DEFAULT_FREQ: f64 = 999_999.0;
const SEPARATORS: &[char] = &['、', '。', '「', '」', '！', '？', '…', '\n'];

/// Trim input, cut it at a separator, and truncate it by character count.
pub fn clean_input(text: &str) -> String {
    let trimmed = text.trim();
    let cut = match trimmed.find(SEPARATORS) {
        Some(i) => &trimmed[..i],
        None => trimmed,
    };
    cut.chars().take(MAX_LOOKUP_CHARS).collect()
}

/// Return true when `clean_input` keeps the text and the character cap has room.
///
/// Text without a separator and shorter than `MAX_LOOKUP_CHARS` passes.
/// The tail of such a line can hold part of a word that continues below.
/// The wrap probe asks this question (`text::layout::wrap_probe`).
pub fn runs_out(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.contains(SEPARATORS) && trimmed.chars().count() < MAX_LOOKUP_CHARS
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

/// This score ports weikipop's priority formula. It omits `steps` by design.
/// See `Candidate::whole`.
fn score(match_len: usize, freq: Option<i64>, kana_bonus: bool) -> f64 {
    let f = freq.map(|v| v as f64).unwrap_or(DEFAULT_FREQ).max(1.0);
    let mut s = match_len as f64;
    s += 10.0 * (1.0 - f.ln() / DEFAULT_FREQ.ln());
    if kana_bonus {
        s += 3.0;
    }
    s
}

/// Map a fine `dec_tag` to a coarse POS. A mapped tag is *terminal*.
/// `stem-` tags use the fail-open arm. The
/// `all_terminal_dec_tags_are_handled_by_the_mapping` test guards this rule.
fn dict_pos_for(dec_tag: &str) -> Option<Option<&'static str>> {
    match dec_tag {
        "v5aru" | "v5b" | "v5g" | "v5k" | "v5k-s" | "v5m" | "v5n" | "v5r"
        | "v5r-i" | "v5s" | "v5t" | "v5u" | "v5u-s" => Some(Some("v5")),
        "vs-i" => Some(Some("vs")),
        "adj-i" => Some(Some("adj-i")),
        "v1" => Some(Some("v1")),
        "vk" => Some(Some("vk")),
        // These tags have no POS signal.
        "exp" | "topic-condition" | "uninflectable" => Some(None),
        // Unrecognized tags use the fail-open path.
        _ => None,
    }
}

struct Candidate {
    row: TermRow,
    match_len: usize,
    steps: usize,
    /// This value is `true` when the deconjugator reaches a complete form. A chain that ends
    /// with a `stem-` tag stops inside morphology. Its row matches a stem by text.
    /// The input is not a form of that word. An unconjugated form needs no chain, so it is
    /// whole by definition.
    whole: bool,
    process: Vec<String>,
}

/// Return `true` only for an unconjugated row with kana in every character.
fn kana_bonus(c: &Candidate) -> bool {
    c.steps == 0 && c.row.surface.chars().all(is_kana)
}

/// `GroupKey` identifies a term-bank row for a headword form. It uses the written
/// form, the reading, and `entry_id`.
///
/// The key does not use one slot per (headword, Dictionary). If a Dictionary
/// matches a headword with several term-bank rows, the engine keeps every row.
/// A census found 6 220 大辞林 headwords with more than one row. The worst
/// case had eleven rows. A key based on `dict_id` made ten rows unreachable
/// from the panel, regardless of renderer behavior. The rows share the ten
/// `MAX_RESULTS` slots in the sort order below. The engine applies the cap
/// before it calls `entries`. Therefore, one hover parses at most ten glossaries
/// (docs/research/hover-parse-cost.md).
///
/// Use `entry_id`, not `dict_id`. `entry.dict_id` identifies the Dictionary, so
/// the third key component still separates Dictionaries. One Jitendex Entry
/// can use two surfaces, つま and 妻. Both surfaces still map to one row.
type GroupKey = (Option<String>, Option<String>, i64);

/// This function defines a deterministic total order. It mirrors the sort order
/// below through `steps`. The group keeps the representative that the order
/// ranks first. The order has no `entry_id` term because the key already holds
/// it. Every candidate in one group shares that value.
fn is_better(new: &Candidate, existing: &Candidate) -> bool {
    fn rank(c: &Candidate) -> (usize, bool, Reverse<usize>, Reverse<&Vec<String>>) {
        (c.match_len, c.whole, Reverse(c.steps), Reverse(&c.process))
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

    pub fn run<D: Dictionary + ?Sized>(&self, dict: &D, text: &str) -> Result<Vec<Hit>> {
        let cleaned = clean_input(text);
        if cleaned.is_empty() {
            return Ok(Vec::new());
        }

        let chars: Vec<char> = cleaned.chars().collect();
        let mut best: HashMap<GroupKey, Candidate> = HashMap::new();

        // Do not exit early. Each prefix has different forms.
        for prefix_len in (1..=chars.len()).rev() {
            let prefix: String = chars[..prefix_len].iter().collect();

            // This call also adds the plain form.
            let forms: Vec<Form> = self.deconjugator.deconjugate(&prefix)
                .into_iter()
                .collect();

            for form in &forms {
                // Get the POS that the chain requires. Mark a form as whole only when it has
                // no tag or a terminal tag.
                let (whole, required_pos) = match form.tags.last() {
                    None => (true, None),
                    Some(tag) => match dict_pos_for(tag) {
                        Some(pos) => (true, pos),
                        None => (false, None),
                    },
                };
                for row in dict.terms_for(&form.text)? {
                    if let Some(need) = required_pos {
                        if !row.pos.is_empty()
                            && !row.pos.split_whitespace().any(|p| p == need)
                        {
                            continue;
                        }
                    }
                    let key: GroupKey =
                        (row.written.clone(), row.reading.clone(), row.entry_id);
                    let candidate = Candidate {
                        row,
                        match_len: prefix_len,
                        steps: form.process.len(),
                        whole,
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

        ranked.sort_by(|a, b| {
            let sa = score(a.match_len, a.row.freq, kana_bonus(a));
            let sb = score(b.match_len, b.row.freq, kana_bonus(b));
            b.match_len
                .cmp(&a.match_len)
                // For equal spans, prefer a form that the deconjugator resolves to a word
                // class. A stem-only result loses even if its homograph is common. Rule count
                // does not make a parse worse.
                .then(b.whole.cmp(&a.whole))
                .then(sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal))
                // After all higher keys tie, use `steps` as a nudge, not as the main
                // factor in the decision.
                .then(a.steps.cmp(&b.steps))
                // Use raw `dict_id` only as a deterministic tie-break.
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
                Some(Hit {
                    score: score(c.match_len, c.row.freq, kana_bonus(&c)),
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
    use crate::lookup::model::FakeDictionary;
    use crate::lookup::rules::load_rules;
    
    use std::path::PathBuf;

    fn engine() -> LookupEngine {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/deconjugator.json");
        LookupEngine::new(Deconjugator::new(load_rules(&p).unwrap()))
    }

    /// A glossary payload in the record format. It has one part-of-speech pill and
    /// one gloss block.
    fn glossary(gloss: &str, pos: &str) -> String {
        serde_json::json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": pos},
            {"tag": "div", "content": gloss}
        ]}])
        .to_string()
    }

    fn taberu_dict() -> FakeDictionary {
        let mut d = FakeDictionary::new();
        d.add_term("食べる", Some("食べる"), Some("たべる"), "v1", Some(500), 1, 1);
        d.add_entry(1, 1, &glossary("to eat", "v1"));
        d
    }

    #[test]
    fn exact_match_found() {
        let hits = engine().run(&taberu_dict(), "食べる").unwrap();
        assert_eq!(1, hits.len());
        assert_eq!(vec!["to eat".to_string()], hits[0].entry.glosses());
    }

    #[test]
    fn conjugated_form_deconjugates_to_headword() {
        let hits = engine().run(&taberu_dict(), "食べさせられた").unwrap();
        assert_eq!(1, hits.len());
        assert_eq!(Some("食べる".to_string()), hits[0].written);
        // The group keeps the shortest derivation, outermost step first. The Card's
        // Inflection chain reverses this order, so the order is a contract.
        assert_eq!(
            hits[0].process,
            ["past", "(unstressed infinitive)", "passive/potential", "causative"]
        );
    }

    #[test]
    fn v5k_tagged_form_matches_v5_row() {
        // The form has tag `v5k`, and the row has POS `v5`.
        let mut d = FakeDictionary::new();
        d.add_term("行く", Some("行く"), Some("いく"), "v5", Some(50), 1, 1);
        d.add_entry(1, 1, &glossary("to go", "v5"));
        let hits = engine().run(&d, "行かなかった").unwrap();
        assert!(
            hits.iter().any(|h| h.written.as_deref() == Some("行く")),
            "expected 行く among hits, got {:?}",
            hits.iter().map(|h| h.written.clone()).collect::<Vec<_>>()
        );
    }

    /// The previous test cannot expose a defect in rank order. With one term,
    /// する wins by default. This test adds many candidates like a real
    /// frequency-less library for してしまった.
    ///
    /// Every row consumes the full input and has no frequency. The only form that
    /// the deconjugator resolves to a word class also uses the most rules. The
    /// nouns are homographs of the stems: し and して. That text match makes
    /// them candidates.
    #[test]
    fn whole_parse_survives_a_crowd_of_stem_homographs() {
        const SHI: &[&str] = &[
            "梓", "仕", "刺", "市", "死", "誌", "巳", "四", "肆", "士", "子",
            "師", "詩", "私",
        ];
        let mut d = FakeDictionary::new();
        for (i, written) in SHI.iter().enumerate() {
            let id = i as i64 + 1;
            d.add_term("し", Some(written), Some("し"), "", None, id, 1);
            d.add_entry(id, 1, &glossary("a noun read し", ""));
        }
        d.add_term("して", Some("仕手"), Some("して"), "", None, 90, 1);
        d.add_entry(90, 1, &glossary("speculator", ""));
        d.add_term("する", Some("為る"), Some("する"), "vs", None, 91, 1);
        d.add_entry(91, 1, &glossary("to do", "vs"));

        let hits = engine().run(&d, "してしまった").unwrap();
        let shown: Vec<Option<String>> = hits.iter().map(|h| h.written.clone()).collect();
        // The extra candidates fill `MAX_RESULTS` and expose the rank order.
        assert_eq!(MAX_RESULTS, hits.len(), "expected a full window, got {shown:?}");
        assert_eq!(
            Some(&Some("為る".to_string())),
            shown.first(),
            "the verb してしまった is a form of must lead its own homographs, got {shown:?}"
        );
    }

    #[test]
    fn more_common_word_ranks_first() {
        // The Dictionary has two Entries and two term-bank rows.
        let mut d = FakeDictionary::new();
        d.add_term("はし", None, Some("はし"), "", Some(50), 1, 1);
        d.add_entry(1, 1, &glossary("chopsticks", ""));
        d.add_term("はし", None, Some("はし"), "", Some(9000), 2, 2);
        d.add_entry(2, 2, &glossary("bridge", ""));
        let hits = engine().run(&d, "はし").unwrap();
        assert_eq!(vec!["chopsticks".to_string()], hits[0].entry.glosses());
    }

    #[test]
    fn longer_match_outranks_shorter() {
        let mut d = FakeDictionary::new();
        d.add_term("日本", Some("日本"), Some("にほん"), "", Some(100), 1, 1);
        d.add_entry(1, 1, &glossary("Japan", ""));
        d.add_term("日本語", Some("日本語"), Some("にほんご"), "", Some(900), 2, 1);
        d.add_entry(2, 1, &glossary("Japanese language", ""));
        let hits = engine().run(&d, "日本語").unwrap();
        assert_eq!(Some("日本語".to_string()), hits[0].written);
    }

}

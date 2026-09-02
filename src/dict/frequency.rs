//! Reported frequencies, and the rule that reduces them to one rank.

use std::collections::HashMap;
use std::path::Path;

/// Term+reading key to rank.
pub type FreqTable = HashMap<(String, Option<String>), i64>;

/// One frequency dictionary's claims, under the name it goes by.
///
/// The pair `dict::build::load_freqs` hands on: a reduction needs the tables
/// in frequency-list order, and storing them needs to know which dictionary
/// each one came from.
pub struct FreqSource {
    pub name: String,
    pub table: FreqTable,
}

/// Does this archive supply the frequency role?
///
/// Its own `term_meta_bank_` rows and never its filename or its
/// `index.json`: the heuristic this replaced asked whether the file was
/// called something containing `Freq` and whether the index set
/// `frequencyMode`, and neither can say what an archive contains
/// (ARCHITECTURE.md#dictionary-and-lookup). The same shape
/// [`crate::dict::pitch::supplies_pitch`] and
/// [`crate::dict::archive::supplies_terms`] take.
///
/// Stops at the first `"freq"` row, so a frequency archive answers from the
/// first row of its first bank and only one with meta banks holding no
/// frequency at all is read whole.
///
/// `false` for an archive this build cannot open or whose banks it cannot
/// parse - unreadable supplies no role.
pub fn supplies_frequency(archive: &Path) -> bool {
    crate::dict::archive::any_meta_row(archive, is_freq_row).unwrap_or(false)
}

/// Is this row a frequency row?
fn is_freq_row(row: &serde_json::Value) -> bool {
    row.as_array().is_some_and(|row| row.len() >= 3 && row[1].as_str() == Some("freq"))
}

/// Rows to a rank table.
pub fn parse_freq_rows(rows: &[serde_json::Value]) -> FreqTable {
    let mut table = FreqTable::new();
    for row in rows {
        merge_freq_row(&mut table, row);
    }
    table
}

/// One row into a table.
pub fn merge_freq_row(table: &mut FreqTable, row: &serde_json::Value) {
    if !is_freq_row(row) {
        return;
    }
    let row = row.as_array().expect("a freq row is an array");
    let Some(term) = row[0].as_str() else { return };
    let (reading, rank) = extract_reading_and_rank(&row[2]);
    let Some(rank) = rank else { return };

    let key = (term.to_string(), reading);
    let should_insert = match table.get(&key) {
        Some(&prev) => rank < prev,
        None => true,
    };
    if should_insert {
        table.insert(key, rank);
    }
}

/// Rank for a term/reading.
pub fn lookup_freq(table: &FreqTable, term: &str, reading: Option<&str>) -> Option<i64> {
    if let Some(r) = reading.filter(|r| !r.is_empty()) {
        if let Some(&rank) = table.get(&(term.to_string(), Some(r.to_string()))) {
            return Some(rank);
        }
    }
    table.get(&(term.to_string(), None)).copied()
}

/// Reading and rank from a row.
fn extract_reading_and_rank(payload: &serde_json::Value) -> (Option<String>, Option<i64>) {
    if let Some(n) = payload.as_i64() {
        return (None, Some(n));
    }
    let Some(obj) = payload.as_object() else { return (None, None) };

    let reading = obj.get("reading").and_then(|v| v.as_str()).map(String::from);
    let rank = match obj.get("frequency") {
        Some(inner) if inner.is_i64() => inner.as_i64(),
        Some(inner) if inner.is_object() => inner.get("value").and_then(|v| v.as_i64()),
        _ => obj.get("value").and_then(|v| v.as_i64()),
    };
    (reading, rank)
}

// ---- many Reported frequencies into one Frequency rank ----

/// The rule that reduces many Reported frequencies to one Frequency rank.
///
/// Applied over the ranks the **enabled** frequency dictionaries report for
/// one headword, in the order the frequency list puts them in. A dictionary
/// that does not have the headword contributes nothing at all - not a large
/// rank, not a zero and not a vote - so a strategy is only ever handed the
/// ranks that exist, and a headword no enabled dictionary ranks reduces to
/// `None`. That `None` is the `NULL` in `term.freq` that leaves `score` on
/// its `DEFAULT_FREQ` fallback (ARCHITECTURE.md#dictionary-and-lookup).
///
/// The three kebab-case names are the *one* spelling: `meta.frequency_strategy`
/// records them through [`RankingStrategy::as_str`], `[dictionaries]` writes
/// them through this derive, and
/// `the_toml_spelling_is_the_one_the_database_records` pins the two together
/// so a rename cannot part them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RankingStrategy {
    /// The lowest rank any enabled dictionary reports.
    ///
    /// The default, and the only one of the three that never reads the
    /// order: it is [`merge_freq_row`]'s within-archive "lowest rank wins"
    /// rule extended across archives, so installing a dictionary can make a
    /// word look commoner and never rarer.
    #[default]
    BestRank,
    /// The rank from the highest-ordered enabled dictionary that has the word.
    Priority,
    /// The median of the ranks the enabled dictionaries report.
    Median,
}

impl RankingStrategy {
    /// The name `meta.frequency_strategy` records.
    pub fn as_str(self) -> &'static str {
        match self {
            RankingStrategy::BestRank => "best-rank",
            RankingStrategy::Priority => "priority",
            RankingStrategy::Median => "median",
        }
    }

    /// One headword's reported ranks as one rank.
    ///
    /// `reported` holds the rank of every enabled dictionary that has this
    /// headword, in frequency-list order, and nothing else.
    ///
    /// Taken mutably because [`RankingStrategy::Median`] sorts in place: a
    /// reduction runs once per headword over a whole corpus, and a `Vec` per
    /// word would be hundreds of thousands of allocations for a slice of
    /// three numbers.
    pub fn apply(self, reported: &mut [i64]) -> Option<i64> {
        match self {
            RankingStrategy::BestRank => best_rank(reported),
            RankingStrategy::Priority => priority(reported),
            RankingStrategy::Median => median(reported),
        }
    }
}

impl std::str::FromStr for RankingStrategy {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<RankingStrategy, anyhow::Error> {
        match name {
            "best-rank" => Ok(RankingStrategy::BestRank),
            "priority" => Ok(RankingStrategy::Priority),
            "median" => Ok(RankingStrategy::Median),
            other => anyhow::bail!(
                "{other:?} is not a ranking strategy - expected best-rank, priority or median"
            ),
        }
    }
}

/// The lowest rank any enabled dictionary reports.
fn best_rank(reported: &[i64]) -> Option<i64> {
    reported.iter().copied().min()
}

/// The rank from the highest-ordered enabled dictionary that has the word.
///
/// The front of the slice, because the ranks arrive in frequency-list order
/// and a dictionary without the word was never put in it: the highest-ordered
/// dictionary that has the word is therefore the first entry, whether or not
/// its rank is the lowest one there.
fn priority(reported: &[i64]) -> Option<i64> {
    reported.first().copied()
}

/// The median of the ranks the enabled dictionaries report.
///
/// An even count has two middle ranks and nothing reported between them, so
/// they are averaged - `low + (high - low) / 2` rather than
/// `(low + high) / 2`, which cannot overflow and which rounds toward the
/// commoner of the two. The direction is deliberate: a reduction may not
/// invent a rarity its sources do not carry.
fn median(reported: &mut [i64]) -> Option<i64> {
    if reported.is_empty() {
        return None;
    }
    reported.sort_unstable();
    let middle = reported.len() / 2;
    if reported.len() % 2 == 1 {
        return Some(reported[middle]);
    }
    let (low, high) = (reported[middle - 1], reported[middle]);
    Some(low + (high - low) / 2)
}

/// Every enabled frequency dictionary's claims, in frequency-list order,
/// reduced to the one table `term.freq` is stamped from.
///
/// Reduced over the union of the sources' keys, each key answered by running
/// [`lookup_freq`] against every source - not by merging the maps, and not
/// row by row. It is the same answer a term row would get if the strategy
/// ran per row, and here is why: a term row is looked up by (term, reading),
/// and `lookup_freq` on this result reads key (term, reading) when the union
/// holds it and key (term, `None`) otherwise. The union holds (term, reading)
/// exactly when some source does, and what went in under that key is every
/// source's own `lookup_freq` answer for it - which for a source that lacks
/// it is that source's (term, `None`), the very fallback it would take row by
/// row. So no term row can be reduced two ways, and a build pays one pass
/// over the sources rather than one per term row.
pub fn reduce(sources: &[FreqTable], strategy: RankingStrategy) -> FreqTable {
    // One dictionary reduces to itself under every strategy, and one is the
    // overwhelmingly common case.
    if let [only] = sources {
        return only.clone();
    }
    let mut reduced = FreqTable::new();
    let mut reported: Vec<i64> = Vec::with_capacity(sources.len());
    for source in sources {
        for key in source.keys() {
            if reduced.contains_key(key) {
                continue;
            }
            reported.clear();
            reported
                .extend(sources.iter().filter_map(|s| lookup_freq(s, &key.0, key.1.as_deref())));
            if let Some(rank) = strategy.apply(&mut reported) {
                reduced.insert(key.clone(), rank);
            }
        }
    }
    reduced
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows(v: serde_json::Value) -> Vec<serde_json::Value> {
        v.as_array().unwrap().clone()
    }

    #[test]
    fn a_reading_agnostic_row_is_keyed_with_no_reading() {
        let t = parse_freq_rows(&rows(json!([["の", "freq", {"value": 1}]])));
        assert_eq!(Some(1), t.get(&("の".to_string(), None)).copied());
    }

    #[test]
    fn a_reading_scoped_row_nests_value_one_level_deeper() {
        let t = parse_freq_rows(&rows(json!([
            ["乃", "freq", {"reading": "の", "frequency": {"value": 7}}]
        ])));
        assert_eq!(Some(7), t.get(&("乃".to_string(), Some("の".to_string()))).copied());
    }

    #[test]
    fn the_lowest_rank_wins_for_one_key() {
        let t = parse_freq_rows(&rows(json!([
            ["猫", "freq", {"value": 900}],
            ["猫", "freq", {"value": 40}]
        ])));
        assert_eq!(Some(40), t.get(&("猫".to_string(), None)).copied());
    }

    #[test]
    fn a_reading_specific_rank_beats_a_reading_agnostic_one() {
        let t = parse_freq_rows(&rows(json!([
            ["猫", "freq", {"value": 9999}],
            ["猫", "freq", {"reading": "ねこ", "frequency": {"value": 42}}]
        ])));
        assert_eq!(Some(42), lookup_freq(&t, "猫", Some("ねこ")));
        assert_eq!(Some(9999), lookup_freq(&t, "猫", Some("びょう")));
        assert_eq!(None, lookup_freq(&t, "犬", None));
    }

    #[test]
    fn an_empty_reading_falls_back_like_a_missing_one() {
        let t = parse_freq_rows(&rows(json!([
            ["猫", "freq", {"value": 9999}],
            ["猫", "freq", {"reading": "ねこ", "frequency": {"value": 42}}]
        ])));
        assert_eq!(Some(9999), lookup_freq(&t, "猫", Some("")));
        assert_eq!(Some(9999), lookup_freq(&t, "猫", None));
    }

    #[test]
    fn rows_that_are_not_freq_rows_are_skipped() {
        let t = parse_freq_rows(&rows(json!([
            ["x", "pitch", {"value": 1}],
            ["y", "freq"],
            ["z", "freq", {"no_value_here": true}]
        ])));
        assert!(t.is_empty());
    }

    #[test]
    fn flat_shape_ignores_extra_display_value_field() {
        let t = parse_freq_rows(&rows(json!([
            ["の", "freq", {"value": 1, "displayValue": "1"}]
        ])));
        let expected: FreqTable = HashMap::from([(("の".to_string(), None), 1)]);
        assert_eq!(expected, t);
    }

    #[test]
    fn reading_scoped_shape_ignores_extra_display_value_field() {
        let t = parse_freq_rows(&rows(json!([
            ["乃", "freq", {"reading": "の", "frequency": {"value": 1, "displayValue": "1"}}]
        ])));
        let expected: FreqTable =
            HashMap::from([(("乃".to_string(), Some("の".to_string())), 1)]);
        assert_eq!(expected, t);
    }

    #[test]
    fn a_bare_integer_payload_is_used_directly_as_the_rank() {
        let t = parse_freq_rows(&rows(json!([["猫", "freq", 42]])));
        let expected: FreqTable = HashMap::from([(("猫".to_string(), None), 42)]);
        assert_eq!(expected, t);
    }

    #[test]
    fn a_lone_pitch_tagged_row_produces_an_empty_table() {
        let t = parse_freq_rows(&rows(json!([["x", "pitch", {"value": 1}]])));
        assert!(t.is_empty());
    }

    #[test]
    fn the_lowest_rank_wins_checked_against_the_full_table() {
        let t = parse_freq_rows(&rows(json!([
            ["猫", "freq", {"value": 90}],
            ["猫", "freq", {"value": 5}]
        ])));
        let expected: FreqTable = HashMap::from([(("猫".to_string(), None), 5)]);
        assert_eq!(expected, t);
    }

    #[test]
    fn lookup_prefers_reading_specific_on_a_hand_built_table() {
        let t: FreqTable = HashMap::from([
            (("乃".to_string(), None), 900),
            (("乃".to_string(), Some("の".to_string())), 1),
        ]);
        assert_eq!(Some(1), lookup_freq(&t, "乃", Some("の")));
    }

    #[test]
    fn lookup_falls_back_when_no_reading_specific_entry_exists_at_all() {
        let t: FreqTable = HashMap::from([(("乃".to_string(), None), 900)]);
        assert_eq!(Some(900), lookup_freq(&t, "乃", Some("の")));
    }

    #[test]
    fn lookup_on_a_completely_empty_table_returns_none() {
        let t = FreqTable::new();
        assert_eq!(None, lookup_freq(&t, "猫", Some("ねこ")));
    }

    // ---- the ranking strategies ----

    /// One reading-agnostic claim per term.
    fn table(claims: &[(&str, i64)]) -> FreqTable {
        claims.iter().map(|(term, rank)| ((term.to_string(), None), *rank)).collect()
    }

    /// Three frequency dictionaries and one fixed set of claims, in
    /// frequency-list order.
    ///
    /// 猫 is in all three, 犬 in the first and the last only, and 鼠 in none
    /// of them. No dictionary's ranks are the lowest, so a strategy that
    /// reads the order and one that sorts cannot agree by accident.
    fn three_dictionaries() -> Vec<FreqTable> {
        vec![
            table(&[("猫", 400), ("犬", 9)]),
            table(&[("猫", 20)]),
            table(&[("猫", 100), ("犬", 5)]),
        ]
    }

    #[test]
    fn best_rank_takes_the_lowest_rank_reported() {
        assert_eq!(Some(20), RankingStrategy::BestRank.apply(&mut [400, 20, 100]));
    }

    #[test]
    fn priority_takes_the_first_dictionarys_rank_even_when_it_is_not_the_lowest() {
        assert_eq!(Some(400), RankingStrategy::Priority.apply(&mut [400, 20, 100]));
    }

    #[test]
    fn the_median_of_an_odd_number_of_ranks_is_the_middle_one() {
        assert_eq!(Some(100), RankingStrategy::Median.apply(&mut [400, 20, 100]));
    }

    #[test]
    fn an_even_number_of_ranks_averages_the_two_middle_ones() {
        assert_eq!(Some(7), RankingStrategy::Median.apply(&mut [9, 5]));
        assert_eq!(Some(30), RankingStrategy::Median.apply(&mut [40, 10, 60, 20]));
    }

    #[test]
    fn every_strategy_reduces_no_reported_ranks_to_nothing() {
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            assert_eq!(None, strategy.apply(&mut []), "{strategy:?}");
        }
    }

    #[test]
    fn a_word_two_of_three_dictionaries_have_is_reduced_from_those_two_alone() {
        let sources = three_dictionaries();
        // 犬 is 9 in the first dictionary and 5 in the third; the second
        // does not have it and must not weigh on any of the three answers.
        let dog = |strategy| lookup_freq(&reduce(&sources, strategy), "犬", None);
        assert_eq!(Some(5), dog(RankingStrategy::BestRank));
        assert_eq!(Some(9), dog(RankingStrategy::Priority));
        assert_eq!(Some(7), dog(RankingStrategy::Median));
    }

    #[test]
    fn a_word_no_dictionary_has_is_reduced_to_nothing() {
        let sources = three_dictionaries();
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            let reduced = reduce(&sources, strategy);
            assert_eq!(None, lookup_freq(&reduced, "鼠", None), "{strategy:?}");
        }
    }

    #[test]
    fn each_strategy_reduces_a_word_every_dictionary_has_by_its_own_rule() {
        let sources = three_dictionaries();
        let cat = |strategy| lookup_freq(&reduce(&sources, strategy), "猫", None);
        assert_eq!(Some(20), cat(RankingStrategy::BestRank));
        assert_eq!(Some(400), cat(RankingStrategy::Priority));
        assert_eq!(Some(100), cat(RankingStrategy::Median));
    }

    #[test]
    fn one_frequency_dictionary_reduces_to_its_own_claims() {
        let only = table(&[("猫", 400)]);
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            assert_eq!(only, reduce(std::slice::from_ref(&only), strategy), "{strategy:?}");
        }
    }

    #[test]
    fn no_frequency_dictionary_reduces_to_an_empty_table() {
        assert!(reduce(&[], RankingStrategy::BestRank).is_empty());
    }

    /// The equivalence [`reduce`]'s comment claims: reducing the union of
    /// the sources' keys answers every (term, reading) pair exactly as
    /// running the strategy over that pair's own per-source lookups would.
    #[test]
    fn reducing_the_key_union_answers_a_row_as_a_per_row_reduction_would() {
        let sources = vec![
            HashMap::from([(("猫".to_string(), Some("ねこ".to_string())), 5)]),
            HashMap::from([(("猫".to_string(), None), 9)]),
        ];
        let reduced = reduce(&sources, RankingStrategy::Median);
        for reading in [Some("ねこ"), Some("びょう"), None] {
            let mut per_row: Vec<i64> =
                sources.iter().filter_map(|s| lookup_freq(s, "猫", reading)).collect();
            assert_eq!(
                RankingStrategy::Median.apply(&mut per_row),
                lookup_freq(&reduced, "猫", reading),
                "reading {reading:?}"
            );
        }
    }

    #[test]
    fn a_strategy_round_trips_through_the_name_meta_records() {
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            assert_eq!(Ok(strategy), strategy.as_str().parse::<RankingStrategy>().map_err(|_| ()));
        }
        assert!("mean".parse::<RankingStrategy>().is_err());
    }
}

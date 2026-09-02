//! Reported frequencies and the rule that reduces them to one Frequency rank.

use std::collections::HashMap;
use std::path::Path;

/// Maps a term and reading key to a Frequency rank.
pub type FreqTable = HashMap<(String, Option<String>), i64>;

/// One Dictionary's Reported frequencies and its name.
///
/// `dict::build::load_freqs` returns this pair. A reduction needs tables in
/// frequency-list order. Storage also needs the name of the Dictionary that
/// supplied each table.
pub struct FreqSource {
    pub name: String,
    pub table: FreqTable,
}

/// Reports whether an archive supplies the frequency role.
///
/// Read `term_meta_bank_` rows. Do not read the filename or `index.json`.
/// The old heuristic checked for `Freq` in the filename and `frequencyMode`
/// in the index. Neither field identifies an archive role
/// (ARCHITECTURE.md#dictionary-and-lookup).
/// [`crate::dict::pitch::supplies_pitch`] and
/// [`crate::dict::archive::supplies_terms`] use the same scan.
///
/// Stop at the first `"freq"` row. Read all meta banks only when no such row
/// exists.
///
/// Return `false` when this build cannot open or parse the archive. An
/// unreadable archive supplies no role.
pub fn supplies_frequency(archive: &Path) -> bool {
    crate::dict::archive::any_meta_row(archive, is_freq_row).unwrap_or(false)
}

/// Returns true for a frequency row.
fn is_freq_row(row: &serde_json::Value) -> bool {
    row.as_array().is_some_and(|row| row.len() >= 3 && row[1].as_str() == Some("freq"))
}

/// Builds a `FreqTable` from rows.
pub fn parse_freq_rows(rows: &[serde_json::Value]) -> FreqTable {
    let mut table = FreqTable::new();
    for row in rows {
        merge_freq_row(&mut table, row);
    }
    table
}

/// Merges one row into a `FreqTable`. The lowest rank for one key wins.
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

/// Returns the Frequency rank for a term and reading.
pub fn lookup_freq(table: &FreqTable, term: &str, reading: Option<&str>) -> Option<i64> {
    if let Some(r) = reading.filter(|r| !r.is_empty()) {
        if let Some(&rank) = table.get(&(term.to_string(), Some(r.to_string()))) {
            return Some(rank);
        }
    }
    table.get(&(term.to_string(), None)).copied()
}

/// Gets the reading and rank from a row payload.
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

// ---- reduce Reported frequencies to one Frequency rank ----

/// Reduces the Reported frequencies for one headword to one Frequency rank.
///
/// The strategy uses ranks from **enabled** Dictionaries. The Dictionary list
/// sets their order. A Dictionary without the headword contributes nothing.
/// The strategy uses only ranks that exist. If no **enabled** Dictionary ranks
/// the headword, the result is `None`. That `None` becomes `NULL` in
/// `term.freq`, and `score` keeps `DEFAULT_FREQ`
/// (ARCHITECTURE.md#dictionary-and-lookup).
///
/// Use these three kebab-case names everywhere. `meta.frequency_strategy`
/// records them through [`RankingStrategy::as_str`]. `[dictionaries]` writes
/// them through this derive. The test
/// `the_toml_spelling_is_the_one_the_database_records` checks both forms.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RankingStrategy {
    /// Returns the lowest rank that any enabled Dictionary reports.
    ///
    /// This is the default strategy. It is the only strategy that ignores order.
    /// It extends the "lowest rank wins" rule from [`merge_freq_row`] across
    /// archives. A new Dictionary can make a headword more common, never more rare.
    #[default]
    BestRank,
    /// Returns the rank from the first enabled Dictionary that has the headword.
    Priority,
    /// Returns the median rank from enabled Dictionaries.
    Median,
}

impl RankingStrategy {
    /// Returns the name that `meta.frequency_strategy` records.
    pub fn as_str(self) -> &'static str {
        match self {
            RankingStrategy::BestRank => "best-rank",
            RankingStrategy::Priority => "priority",
            RankingStrategy::Median => "median",
        }
    }

    /// Reduces the Reported frequencies for one headword to one rank.
    ///
    /// `reported` contains one rank for each enabled Dictionary that has the
    /// headword, in frequency-list order. It contains no other ranks.
    ///
    /// The slice is mutable because [`RankingStrategy::Median`] sorts it in place.
    /// The reduction handles each headword in a full corpus. A new `Vec` per
    /// headword would create hundreds of thousands of allocations for three numbers.
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

/// Returns the lowest rank in `reported`.
fn best_rank(reported: &[i64]) -> Option<i64> {
    reported.iter().copied().min()
}

/// Returns the rank from the first enabled Dictionary with the headword.
///
/// The slice uses frequency-list order. A Dictionary without the headword
/// contributes no value, so the first rank belongs to the highest-priority
/// Dictionary that has the headword. It can differ from the lowest rank.
fn priority(reported: &[i64]) -> Option<i64> {
    reported.first().copied()
}

/// Returns the median of the reported ranks.
///
/// An even count has two middle ranks and no rank between them. Average those
/// two ranks with `low + (high - low) / 2`, not `(low + high) / 2`.
/// This form cannot overflow. It rounds toward the more common rank. The
/// direction avoids a rarer value that no source reports.
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

/// Reduces the claims from enabled Dictionaries into the table that stamps
/// `term.freq`.
///
/// Build the union of source keys. For each key, call [`lookup_freq`] for every
/// source and apply the strategy. Do not merge the maps or process each term row.
///
/// For a term row, [`lookup_freq`] reads the `(term, reading)` key when the
/// union contains it. Otherwise, it reads the `(term, None)` key.
/// A source without `(term, reading)` supplies its `(term, None)` value.
/// Therefore, the reduced table gives the same result as a strategy on each
/// term row.
///
/// One pass over the sources also avoids one pass for every term row.
pub fn reduce(sources: &[FreqTable], strategy: RankingStrategy) -> FreqTable {
    // One Dictionary reduces to itself under every strategy. This path also
    // covers the most common case.
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
// ---- Frequency rank strategies ----

    /// Builds a `FreqTable` with one reading-agnostic claim per term.
    fn table(claims: &[(&str, i64)]) -> FreqTable {
        claims.iter().map(|(term, rank)| ((term.to_string(), None), *rank)).collect()
    }

    /// Three Dictionaries and one fixed set of claims, in frequency-list order.
    ///
    /// Every Dictionary has 猫. Only the first and last have 犬. No Dictionary
    /// has 鼠. No Dictionary has the lowest rank for every headword. The data
    /// makes order-based and sort-based strategies return different values.
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
        // 犬 has ranks 9 and 5 in the first and third Dictionaries. The second
        // Dictionary has no 犬, so it does not affect any strategy result.
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

    /// Checks the equivalence described on [`reduce`]. A reduction over the
    /// union of source keys gives each `(term, reading)` pair the same result
    /// as a strategy over per-source [`lookup_freq`] results.
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

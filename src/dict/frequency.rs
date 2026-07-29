//! Frequency-rank parsing.

use std::collections::HashMap;

/// Term+reading key to rank.
pub type FreqTable = HashMap<(String, Option<String>), i64>;

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
    let Some(row) = row.as_array() else { return };
    if row.len() < 3 || row[1].as_str() != Some("freq") {
        return;
    }
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
}

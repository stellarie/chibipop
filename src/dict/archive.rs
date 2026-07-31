//! Yomitan archive reading.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

/// One row from a term bank.
pub struct TermEntry {
    pub term: String,
    pub reading: String,
    pub rules: String,
    pub glossary: Value,
}

/// An archive's index.json.
pub fn read_index(zip: &Path) -> Result<Value> {
    let mut archive = open_archive(zip)?;
    read_json(&mut archive, "index.json")
}

/// Every term bank row.
pub fn iter_terms(zip: &Path) -> Result<Vec<TermEntry>> {
    let mut archive = open_archive(zip)?;
    let banks = sorted_banks(&archive_names(&archive), "term_bank_");
    let mut out = Vec::new();
    for bank in banks {
        for row in read_json_array(&mut archive, &bank)? {
            let Some(row) = row.as_array() else { continue };
            let Some(term) = row.first().and_then(Value::as_str) else { continue };
            out.push(TermEntry {
                term: term.to_string(),
                reading: str_at(row, 1),
                rules: str_at(row, 3),
                glossary: row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new())),
            });
        }
    }
    Ok(out)
}

/// Every frequency bank row.
pub fn iter_freq_rows(zip: &Path) -> Result<Vec<Value>> {
    let mut archive = open_archive(zip)?;
    let banks = sorted_banks(&archive_names(&archive), "term_meta_bank_");
    let mut out = Vec::new();
    for bank in banks {
        out.extend(read_json_array(&mut archive, &bank)?);
    }
    Ok(out)
}

/// Detects a frequency archive.
pub fn is_frequency_archive(zip: &Path) -> bool {
    let name_says_freq =
        zip.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("Freq"));
    let index_says_freq =
        read_index(zip).is_ok_and(|idx| idx.get("frequencyMode").is_some_and(is_truthy));
    name_says_freq || index_says_freq
}

/// A row field, or empty.
fn str_at(row: &[Value], i: usize) -> String {
    row.get(i).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Python-style truthiness.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Opens a zip for reading.
fn open_archive(zip: &Path) -> Result<ZipArchive<File>> {
    let file = File::open(zip).with_context(|| format!("opening {}", zip.display()))?;
    ZipArchive::new(file).with_context(|| format!("reading zip {}", zip.display()))
}

/// Every entry name in a zip.
fn archive_names(archive: &ZipArchive<File>) -> Vec<String> {
    archive.file_names().map(String::from).collect()
}

/// One zip entry as JSON.
fn read_json(archive: &mut ZipArchive<File>, name: &str) -> Result<Value> {
    let mut entry = archive.by_name(name).with_context(|| format!("reading {name}"))?;
    let mut text = String::new();
    entry.read_to_string(&mut text).with_context(|| format!("decoding {name}"))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {name}"))
}

/// A bank file's JSON rows.
fn read_json_array(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<Value>> {
    match read_json(archive, name)? {
        Value::Array(rows) => Ok(rows),
        _ => anyhow::bail!("{name}: not a JSON array"),
    }
}

/// Matching banks, sorted.
fn sorted_banks(names: &[String], prefix: &str) -> Vec<String> {
    let mut picked: Vec<String> =
        names.iter().filter(|n| n.starts_with(prefix) && n.ends_with(".json")).cloned().collect();
    sort_banks(&mut picked, prefix);
    picked
}

/// Sorts bank names numerically.
fn sort_banks(names: &mut [String], prefix: &str) {
    names.sort_by_key(|n| bank_number(n, prefix));
}

/// A bank name's numeric key.
fn bank_number(name: &str, prefix: &str) -> u64 {
    let rest = name.strip_prefix(prefix).unwrap_or(name);
    first_digit_run(rest).unwrap_or(0)
}

/// First run of digits.
fn first_digit_run(s: &str) -> Option<u64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let digits: String = s[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    #[test]
    fn the_index_carries_the_dictionary_title() {
        let idx = read_index(&fixture("terms.zip")).unwrap();
        assert_eq!(Some("FixtureTerms"), idx["title"].as_str());
    }

    #[test]
    fn every_term_row_is_read_with_its_rules_field() {
        let terms = iter_terms(&fixture("terms.zip")).unwrap();
        assert_eq!(3, terms.len());
        let taberu = terms.iter().find(|t| t.term == "食べる").expect("食べる present");
        assert_eq!("たべる", taberu.reading);
        assert_eq!("v1", taberu.rules);
    }

    #[test]
    fn a_frequency_archive_is_detected_and_a_term_archive_is_not() {
        assert!(is_frequency_archive(&fixture("freq.zip")));
        assert!(!is_frequency_archive(&fixture("terms.zip")));
    }

    #[test]
    fn frequency_rows_come_back_raw_for_the_parser() {
        let rows = iter_freq_rows(&fixture("freq.zip")).unwrap();
        assert_eq!(3, rows.len());
    }

    #[test]
    fn banks_sort_numerically_so_bank_10_follows_bank_9() {
        let mut names = vec![
            "term_bank_10.json".to_string(),
            "term_bank_2.json".to_string(),
            "term_bank_1.json".to_string(),
        ];
        sort_banks(&mut names, "term_bank_");
        assert_eq!(
            vec!["term_bank_1.json", "term_bank_2.json", "term_bank_10.json"],
            names
        );
    }

    #[test]
    fn ragged_row_with_only_term_reads_defaults() {
        let row = vec![Value::String("word".to_string())];
        assert_eq!("", str_at(&row, 1));
        assert_eq!("", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn ragged_row_with_term_and_reading_only() {
        let row = serde_json::json!([
            "食べる",
            "たべる"
        ]).as_array().unwrap().clone();
        assert_eq!("食べる", str_at(&row, 0));
        assert_eq!("たべる", str_at(&row, 1));
        assert_eq!("", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn ragged_row_has_rules_but_no_glossary() {
        let row = serde_json::json!([
            "飲む",
            "のむ",
            "",
            "v5"
        ]).as_array().unwrap().clone();
        assert_eq!("飲む", str_at(&row, 0));
        assert_eq!("のむ", str_at(&row, 1));
        assert_eq!("v5", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn field_with_wrong_type_falls_back_to_default() {
        let row = serde_json::json!([
            "走る",
            123,
            "",
            999
        ]).as_array().unwrap().clone();
        assert_eq!("走る", str_at(&row, 0));
        assert_eq!("", str_at(&row, 1));
        assert_eq!("", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn glossary_with_wrong_type_falls_back_to_empty() {
        let row = serde_json::json!([
            "読む",
            "よむ",
            "",
            "v5",
            0,
            "not an array"
        ]).as_array().unwrap().clone();
        let glossary = row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        assert_eq!(Value::String("not an array".to_string()), glossary);
    }

    #[test]
    fn glossary_missing_falls_back_to_empty_array() {
        let row = serde_json::json!([
            "書く",
            "かく",
            "",
            "v5"
        ]).as_array().unwrap().clone();
        let glossary = row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        assert_eq!(Value::Array(Vec::new()), glossary);
    }
}

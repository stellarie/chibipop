//! AnkiConnect v6 client.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);
const VERSION: u8 = 6;

fn post(url: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    let mut resp = ureq::post(url)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .send_json(body)
        .context("AnkiConnect request")?;
    let json: serde_json::Value = resp
        .body_mut()
        .read_json()
        .context("reading AnkiConnect response")?;
    if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("AnkiConnect: {err}");
    }
    Ok(json)
}

/// True if Anki answers.
pub fn check_connection(url: &str) -> Result<bool> {
    let body = serde_json::json!({
        "action": "version",
        "version": VERSION,
    });
    match post(url, &body) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn string_list(resp: &serde_json::Value) -> Result<Vec<String>> {
    let arr = resp.get("result")
        .and_then(|r| r.as_array())
        .context("no result array")?;
    Ok(arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

/// All deck names.
pub fn deck_names(url: &str) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "action": "deckNames",
        "version": VERSION,
    });
    string_list(&post(url, &body)?)
}

/// All note-type names.
pub fn model_names(url: &str) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "action": "modelNames",
        "version": VERSION,
    });
    string_list(&post(url, &body)?)
}

/// A note-type's field names.
pub fn model_field_names(url: &str, model: &str) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "action": "modelFieldNames",
        "version": VERSION,
        "params": { "modelName": model },
    });
    string_list(&post(url, &body)?)
}

/// Expressions already in Anki.
pub fn find_duplicates(
    url: &str,
    deck: &str,
    model: &str,
    expressions: &[&str],
) -> Result<HashSet<String>> {
    if expressions.is_empty() {
        return Ok(HashSet::new());
    }
    let notes: Vec<_> = expressions
        .iter()
        .map(|&e| {
            serde_json::json!({
                "deckName": deck,
                "modelName": model,
                "fields": { "Expression": e },
                "options": {
                    "allowDuplicate": false,
                },
            })
        })
        .collect();
    let body = serde_json::json!({
        "action": "canAddNotes",
        "version": VERSION,
        "params": { "notes": notes },
    });
    let resp = post(url, &body)?;
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .context("canAddNotes: no result")?;
    Ok(collect_dupes(expressions, arr))
}

/// Inverts canAddNotes booleans.
fn collect_dupes(
    expressions: &[&str],
    can_add: &[serde_json::Value],
) -> HashSet<String> {
    expressions
        .iter()
        .zip(can_add)
        .filter(|(_, v)| {
            !v.as_bool().unwrap_or(true)
        })
        .map(|(&e, _)| e.to_string())
        .collect()
}

#[cfg(test)]
/// Escapes for Anki search.
fn quote_search(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('*', "\\*");
    format!("\"{escaped}\"")
}

/// Maps sources to Anki fields.
fn mapped_fields(
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for mapping in field_map {
        if let Some(value) = fields.get(&mapping.source) {
            out.insert(mapping.anki_field.clone(), serde_json::json!(value));
        }
    }
    out
}

/// Adds a note, returns its id.
pub fn add_note(
    url: &str,
    deck: &str,
    model: &str,
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
) -> Result<i64> {
    let body = serde_json::json!({
        "action": "addNote",
        "version": VERSION,
        "params": {
            "note": {
                "deckName": deck,
                "modelName": model,
                "fields": mapped_fields(fields, field_map),
                "options": {
                    "allowDuplicate": false,
                },
            },
        },
    });
    let resp = post(url, &body)?;
    resp.get("result")
        .and_then(|r| r.as_i64())
        .context("addNote did not return a note ID")
}

/// Fields from a card.
pub fn fields_from_card(
    card: &crate::present::Card,
    blocks: &[crate::present::GlossBlock],
) -> HashMap<String, String> {
    let expression = card.written.as_deref()
        .or(card.reading.as_deref())
        .unwrap_or("")
        .to_string();
    let reading = card.reading.as_deref()
        .unwrap_or("")
        .to_string();
    let glossary = blocks.iter()
        .map(|b| {
            let joined = b.glosses.join("; ");
            format!("{}: {joined}", b.dict_name)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut fields = HashMap::from([
        ("expression".to_string(), expression),
        ("reading".to_string(), reading),
        ("glossary".to_string(), glossary),
    ]);
    if let Some(freq) = card.freq {
        fields.insert("frequency".to_string(), freq.to_string());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_from_card_formats_glossary() {
        let card = crate::present::Card {
            written: Some("猫".into()),
            reading: Some("ねこ".into()),
            pos: vec![],
            freq: Some(42),
            blocks: vec![],
            match_len: 1,
        };
        let blocks = vec![
            crate::present::GlossBlock {
                dict_name: "大辞林".into(),
                glosses: vec!["ネコ科の哺乳類。".into()],
            },
            crate::present::GlossBlock {
                dict_name: "Jitendex".into(),
                glosses: vec!["cat".into(), "feline".into()],
            },
        ];
        let f = fields_from_card(&card, &blocks);
        assert_eq!(Some(&"猫".to_string()), f.get("expression"));
        assert_eq!(Some(&"ねこ".to_string()), f.get("reading"));
        assert_eq!(
            Some(&"大辞林: ネコ科の哺乳類。\nJitendex: cat; feline".to_string()),
            f.get("glossary"),
        );
        assert_eq!(Some(&"42".to_string()), f.get("frequency"));
    }

    #[test]
    fn fields_falls_back_to_reading() {
        let card = crate::present::Card {
            written: None,
            reading: Some("ねこ".into()),
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 1,
        };
        let f = fields_from_card(&card, &[]);
        assert_eq!(Some(&"ねこ".to_string()), f.get("expression"));
        assert_eq!(None, f.get("frequency"));
    }

    #[test]
    fn quote_search_wraps_a_plain_word() {
        assert_eq!("\"食べる\"", quote_search("食べる"));
    }

    #[test]
    fn quote_search_escapes_embedded_quotes() {
        assert_eq!("\"foo\\\"bar\"", quote_search("foo\"bar"));
    }

    #[test]
    fn quote_search_escapes_backslashes() {
        assert_eq!("\"a\\\\b\"", quote_search("a\\b"));
    }

    /// Bare spaces split into terms.
    #[test]
    fn quote_search_keeps_a_space_as_one_value() {
        assert_eq!("\"AT & T\"", quote_search("AT & T"));
    }

    #[test]
    fn quote_search_escapes_wildcard() {
        assert_eq!("\"a\\*b\"", quote_search("a*b"));
    }

    #[test]
    fn collect_dupes_picks_existing() {
        use serde_json::json;
        let exprs = ["食べる", "猫", "犬"];
        let resp = vec![json!(false), json!(true), json!(false)];
        let got = collect_dupes(&exprs, &resp);
        let want = HashSet::from(["食べる".into(), "犬".into()]);
        assert_eq!(want, got);
    }

    #[test]
    fn collect_dupes_all_new() {
        use serde_json::json;
        let exprs = ["食べる", "猫"];
        let resp = vec![json!(true), json!(true)];
        assert!(collect_dupes(&exprs, &resp).is_empty());
    }

    #[test]
    fn collect_dupes_all_existing() {
        use serde_json::json;
        let exprs = ["食べる", "猫"];
        let resp = vec![json!(false), json!(false)];
        let got = collect_dupes(&exprs, &resp);
        let want = HashSet::from(["食べる".into(), "猫".into()]);
        assert_eq!(want, got);
    }

    #[test]
    fn collect_dupes_empty_input() {
        let got = collect_dupes(&[], &[]);
        assert!(got.is_empty());
    }

    /// Shorter response: extras safe.
    #[test]
    fn collect_dupes_short_response() {
        use serde_json::json;
        let exprs = ["食べる", "猫", "犬"];
        let resp = vec![json!(false)];
        let got = collect_dupes(&exprs, &resp);
        assert_eq!(HashSet::from(["食べる".into()]), got);
    }

    #[test]
    fn mapped_fields_uses_the_configured_anki_field_name() {
        let fields = HashMap::from([("expression".to_string(), "猫".to_string())]);
        let map = vec![crate::config::FieldMapping {
            anki_field: "Front".into(),
            source: "expression".into(),
        }];
        let out = mapped_fields(&fields, &map);
        assert_eq!(Some(&serde_json::json!("猫")), out.get("Front"));
    }

    #[test]
    fn mapped_fields_skips_a_source_the_card_did_not_have() {
        let fields = HashMap::new();
        let map = vec![crate::config::FieldMapping {
            anki_field: "Frequency".into(),
            source: "frequency".into(),
        }];
        let out = mapped_fields(&fields, &map);
        assert!(out.is_empty());
    }

    #[test]
    fn mapped_fields_can_send_one_source_to_two_anki_fields() {
        let fields = HashMap::from([("frequency".to_string(), "42".to_string())]);
        let map = vec![
            crate::config::FieldMapping { anki_field: "Frequency".into(), source: "frequency".into() },
            crate::config::FieldMapping { anki_field: "FreqSort".into(), source: "frequency".into() },
        ];
        let out = mapped_fields(&fields, &map);
        assert_eq!(Some(&serde_json::json!("42")), out.get("Frequency"));
        assert_eq!(Some(&serde_json::json!("42")), out.get("FreqSort"));
    }

    #[test]
    fn mapped_fields_with_an_empty_map_sends_nothing() {
        let fields = HashMap::from([("expression".to_string(), "猫".to_string())]);
        let out = mapped_fields(&fields, &[]);
        assert!(out.is_empty());
    }
}

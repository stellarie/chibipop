//! AnkiConnect v6 client.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);
const VERSION: u8 = 6;

/// Fields sent to AnkiConnect.
pub struct AnkiFields {
    pub expression: String,
    pub reading: String,
    pub glossary: String,
    pub frequency: Option<String>,
}

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
pub fn find_duplicates(url: &str, expressions: &[&str]) -> Result<HashSet<String>> {
    let mut found = HashSet::new();
    for expr in expressions {
        let query = format!("Expression:{expr}");
        let body = serde_json::json!({
            "action": "findNotes",
            "version": VERSION,
            "params": { "query": query },
        });
        let resp = post(url, &body)?;
        let has_match = resp.get("result")
            .and_then(|r| r.as_array())
            .is_some_and(|a| !a.is_empty());
        if has_match {
            found.insert(expr.to_string());
        }
    }
    Ok(found)
}

/// Adds a note, returns its id.
pub fn add_note(url: &str, deck: &str, model: &str, fields: &AnkiFields) -> Result<i64> {
    let mut field_map = serde_json::json!({
        "Expression": fields.expression,
        "ExpressionReading": fields.reading,
        "Glossary": fields.glossary,
    });
    if let Some(freq) = &fields.frequency {
        field_map["Frequency"] = serde_json::json!(freq);
        field_map["FreqSort"] = serde_json::json!(freq);
    }
    let body = serde_json::json!({
        "action": "addNote",
        "version": VERSION,
        "params": {
            "note": {
                "deckName": deck,
                "modelName": model,
                "fields": field_map,
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
) -> AnkiFields {
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
    let frequency = card.freq.map(|f| f.to_string());
    AnkiFields { expression, reading, glossary, frequency }
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
        assert_eq!("猫", f.expression);
        assert_eq!("ねこ", f.reading);
        assert_eq!("大辞林: ネコ科の哺乳類。\nJitendex: cat; feline", f.glossary);
        assert_eq!(Some("42".into()), f.frequency);
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
        assert_eq!("ねこ", f.expression);
        assert_eq!(None, f.frequency);
    }
}

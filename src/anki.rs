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

/// True if some row sends source
fn field_map_routes(field_map: &[crate::config::FieldMapping], source: &str) -> bool {
    field_map.iter().any(|m| m.source == source)
}

/// Adds a note, returns its id.
pub fn add_note(
    url: &str,
    deck: &str,
    model: &str,
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
) -> Result<i64> {
    if !field_map_routes(field_map, "expression") {
        anyhow::bail!(
            "field_map has no entry mapping the \"expression\" source, so the \
             looked-up word would never reach Anki - fix the mapping in \
             Settings, Anki tab"
        );
    }
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

/// One image to attach.
pub struct NotePicture {
    pub data_base64: String,
    pub filename: String,
    /// Anki fields to embed it in.
    pub fields: Vec<String>,
}

/// Builds an addNote request.
fn build_add_note_body(
    deck: &str,
    model: &str,
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
    picture: Option<&NotePicture>,
) -> serde_json::Value {
    let mut note = serde_json::json!({
        "deckName": deck,
        "modelName": model,
        "fields": mapped_fields(fields, field_map),
        "options": { "allowDuplicate": false },
    });
    if let Some(pic) = picture {
        note["picture"] = serde_json::json!([{
            "data": pic.data_base64,
            "filename": pic.filename,
            "fields": pic.fields,
        }]);
    }
    serde_json::json!({
        "action": "addNote",
        "version": VERSION,
        "params": { "note": note },
    })
}

/// Adds a note with a picture.
pub fn add_note_with_picture(
    url: &str,
    deck: &str,
    model: &str,
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
    picture: Option<&NotePicture>,
) -> Result<i64> {
    if !field_map_routes(field_map, "expression") {
        anyhow::bail!(
            "field_map has no entry mapping the \"expression\" source, so the \
             looked-up word would never reach Anki - fix the mapping in \
             Settings, Anki tab"
        );
    }
    let body = build_add_note_body(deck, model, fields, field_map, picture);
    let resp = post(url, &body)?;
    resp.get("result")
        .and_then(|r| r.as_i64())
        .context("addNote did not return a note ID")
}

/// Minimal escaping for text placed into an HTML-destined Anki field
/// (currently just the dictionary name, which is arbitrary text pulled from
/// the Yomitan archive's index.json).
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One dict's glosses, numbered.
fn plain_dict_group(b: &crate::present::GlossBlock) -> String {
    let numbered = b.glosses.iter()
        .enumerate()
        .map(|(i, g)| format!("{}. {g}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    format!("[{}]\n{numbered}", b.dict_name)
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
        .map(plain_dict_group)
        .collect::<Vec<_>>()
        .join("\n\n");
    // \n is a no-op; use <li> tags.
    let glossary_html = blocks.iter()
        .map(|b| {
            let items: String = b.glosses_html.iter()
                .map(|g| format!("<li>{g}</li>"))
                .collect();
            format!(
                "<b>{}</b><ol style=\"margin:2px 0 2px 20px;padding:0\">{items}</ol>",
                escape_html(&b.dict_name)
            )
        })
        .collect::<Vec<_>>()
        .join("<hr style=\"border:none;border-top:1px solid #666;margin:4px 0\">");
    let mut fields = HashMap::from([
        ("expression".to_string(), expression),
        ("reading".to_string(), reading),
        ("glossary".to_string(), glossary),
        ("glossary_html".to_string(), glossary_html),
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
                glosses_html: vec!["ネコ科の<b>哺乳類</b>。".into()],
            },
            crate::present::GlossBlock {
                dict_name: "Jitendex".into(),
                glosses: vec!["cat".into(), "feline".into()],
                glosses_html: vec!["cat".into(), "<i>feline</i>".into()],
            },
        ];
        let f = fields_from_card(&card, &blocks);
        assert_eq!(Some(&"猫".to_string()), f.get("expression"));
        assert_eq!(Some(&"ねこ".to_string()), f.get("reading"));
        assert_eq!(
            Some(&"[大辞林]\n1. ネコ科の哺乳類。\n\n[Jitendex]\n1. cat\n2. feline".to_string()),
            f.get("glossary"),
        );
        assert_eq!(
            Some(&"<b>大辞林</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>ネコ科の<b>哺乳類</b>。</li></ol><hr style=\"border:none;border-top:1px solid #666;margin:4px 0\"><b>Jitendex</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>cat</li><li><i>feline</i></li></ol>".to_string()),
            f.get("glossary_html"),
        );
        assert_eq!(Some(&"42".to_string()), f.get("frequency"));
    }

    #[test]
    fn glossary_html_escapes_a_dictionary_name_with_markup_looking_characters() {
        let card = crate::present::Card {
            written: Some("猫".into()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 1,
        };
        let blocks = vec![crate::present::GlossBlock {
            dict_name: "A & B <dict>".into(),
            glosses: vec!["cat".into()],
            glosses_html: vec!["cat".into()],
        }];
        let f = fields_from_card(&card, &blocks);
        assert_eq!(
            Some(&"<b>A &amp; B &lt;dict&gt;</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>cat</li></ol>".to_string()),
            f.get("glossary_html"),
        );
    }

    #[test]
    fn glossary_html_wraps_one_dictionary_in_a_list_with_no_divider() {
        let card = crate::present::Card {
            written: Some("猫".into()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 1,
        };
        let blocks = vec![crate::present::GlossBlock {
            dict_name: "Wenlin".into(),
            glosses: vec!["supper".into(), "dinner".into()],
            glosses_html: vec!["supper".into(), "dinner".into()],
        }];
        let f = fields_from_card(&card, &blocks);
        let html = f.get("glossary_html").unwrap();
        assert_eq!(
            "<b>Wenlin</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>supper</li><li>dinner</li></ol>",
            html,
        );
        assert!(!html.contains("<hr"), "single dict has no divider");
    }

    #[test]
    fn glossary_html_separates_multiple_dictionaries_with_a_divider() {
        let card = crate::present::Card {
            written: Some("猫".into()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 1,
        };
        let blocks = vec![
            crate::present::GlossBlock {
                dict_name: "Wenlin".into(),
                glosses: vec!["supper".into()],
                glosses_html: vec!["supper".into()],
            },
            crate::present::GlossBlock {
                dict_name: "CC-CEDICT".into(),
                glosses: vec!["evening meal".into()],
                glosses_html: vec!["evening meal".into()],
            },
        ];
        let f = fields_from_card(&card, &blocks);
        let html = f.get("glossary_html").unwrap();
        assert_eq!(
            "<b>Wenlin</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>supper</li></ol><hr style=\"border:none;border-top:1px solid #666;margin:4px 0\"><b>CC-CEDICT</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>evening meal</li></ol>",
            html,
        );
        assert_eq!(1, html.matches("<hr").count(), "one divider, none trailing");
        assert!(html.ends_with("</ol>"), "no divider after last dict");
    }

    #[test]
    fn glossary_separates_dictionaries_with_headers_and_dividers() {
        let card = crate::present::Card {
            written: Some("犬".into()),
            reading: Some("いぬ".into()),
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 1,
        };
        let blocks = vec![
            crate::present::GlossBlock {
                dict_name: "大辞林".into(),
                glosses: vec!["イヌ科の哺乳類。".into()],
                glosses_html: vec!["イヌ科の哺乳類。".into()],
            },
            crate::present::GlossBlock {
                dict_name: "Jitendex".into(),
                glosses: vec!["dog".into()],
                glosses_html: vec!["dog".into()],
            },
        ];
        let f = fields_from_card(&card, &blocks);

        let glossary = f.get("glossary").unwrap();
        assert!(glossary.contains("[大辞林]"));
        assert!(glossary.contains("[Jitendex]"));
        assert!(glossary.contains("\n\n"), "blank line between groups");

        let html = f.get("glossary_html").unwrap();
        assert!(html.contains("<b>大辞林</b>"));
        assert!(html.contains("<b>Jitendex</b>"));
        assert!(html.contains("<hr"), "divider between groups");
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

    #[test]
    fn field_map_routes_finds_a_matching_source() {
        let map = vec![crate::config::FieldMapping {
            anki_field: "Expression".into(),
            source: "expression".into(),
        }];
        assert!(field_map_routes(&map, "expression"));
    }

    #[test]
    fn field_map_routes_is_false_without_a_match() {
        // Mirrors the live-config bug.
        let map = vec![crate::config::FieldMapping {
            anki_field: "Expression".into(),
            source: "frequency".into(),
        }];
        assert!(!field_map_routes(&map, "expression"));
    }

    #[test]
    fn field_map_routes_is_false_for_an_empty_map() {
        assert!(!field_map_routes(&[], "expression"));
    }

    /// No network before the check.
    #[test]
    fn add_note_rejects_a_field_map_that_drops_the_word() {
        let fields = HashMap::from([
            ("expression".to_string(), "猫".to_string()),
            ("reading".to_string(), "ねこ".to_string()),
        ]);
        let field_map = vec![
            crate::config::FieldMapping { anki_field: "Expression".into(), source: "frequency".into() },
            crate::config::FieldMapping { anki_field: "ExpressionReading".into(), source: "reading".into() },
        ];
        let err = add_note("not-a-url", "Default", "Lapis", &fields, &field_map).unwrap_err();
        assert!(format!("{err:#}").contains("expression"));
    }

    #[test]
    fn add_note_with_picture_includes_picture_array() {
        let fields = HashMap::from([("expression".to_string(), "宿舎".to_string())]);
        let field_map = vec![crate::config::FieldMapping {
            anki_field: "Expression".into(),
            source: "expression".into(),
        }];
        let pic = NotePicture {
            data_base64: "iVBOR...".to_string(),
            filename: "chibipop-screenshot-test.png".to_string(),
            fields: vec!["Context".to_string()],
        };
        let body = build_add_note_body("Default", "Lapis", &fields, &field_map, Some(&pic));
        let note = &body["params"]["note"];
        assert!(note["picture"].is_array());
        let pic_entry = &note["picture"][0];
        assert_eq!("iVBOR...", pic_entry["data"].as_str().unwrap());
        assert_eq!("chibipop-screenshot-test.png", pic_entry["filename"].as_str().unwrap());
        assert_eq!("Context", pic_entry["fields"][0].as_str().unwrap());
    }

    #[test]
    fn add_note_with_picture_none_omits_picture() {
        let fields = HashMap::from([("expression".to_string(), "宿舎".to_string())]);
        let field_map = vec![crate::config::FieldMapping {
            anki_field: "Expression".into(),
            source: "expression".into(),
        }];
        let body = build_add_note_body("Default", "Lapis", &fields, &field_map, None);
        let note = &body["params"]["note"];
        assert!(note.get("picture").is_none());
    }
}

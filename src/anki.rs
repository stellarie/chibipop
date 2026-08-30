//! AnkiConnect v6 client.

use crate::dict::gloss::{render_html, RoleFilter, Selection};
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
///
/// The probe notes carry the word routed through `field_map`, exactly
/// as an add would: `canAddNotes` answers `false` for *any* reason a
/// note cannot be filed, and a note built from field names the note
/// type does not have is empty, not a duplicate.
pub fn find_duplicates(
    url: &str,
    deck: &str,
    model: &str,
    expressions: &[&str],
    field_map: &[crate::config::FieldMapping],
) -> Result<HashSet<String>> {
    if expressions.is_empty() {
        return Ok(HashSet::new());
    }
    require_expression_route(field_map)?;
    let body = build_can_add_notes_body(deck, model, expressions, field_map);
    let resp = post(url, &body)?;
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .context("canAddNotes: no result")?;
    Ok(collect_dupes(expressions, arr))
}

/// Builds a canAddNotes probe.
///
/// One note per word, through the same `mapped_fields` router the add
/// uses, so Anki judges the note this word would really be filed as.
fn build_can_add_notes_body(
    deck: &str,
    model: &str,
    expressions: &[&str],
    field_map: &[crate::config::FieldMapping],
) -> serde_json::Value {
    let notes: Vec<_> = expressions
        .iter()
        .map(|&e| {
            let fields = HashMap::from([("expression".to_string(), e.to_string())]);
            serde_json::json!({
                "deckName": deck,
                "modelName": model,
                "fields": mapped_fields(&fields, field_map),
                "options": {
                    "allowDuplicate": false,
                },
            })
        })
        .collect();
    serde_json::json!({
        "action": "canAddNotes",
        "version": VERSION,
        "params": { "notes": notes },
    })
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

/// Refuses a map that never sends the word.
///
/// Every AnkiConnect call about a looked-up word - the add and the
/// dupe probe alike - is meaningless without it: an add would file a
/// note with no word in it, and a probe would ask Anki about an empty
/// note.
fn require_expression_route(field_map: &[crate::config::FieldMapping]) -> Result<()> {
    if field_map_routes(field_map, "expression") {
        return Ok(());
    }
    anyhow::bail!(
        "field_map has no entry mapping the \"expression\" source, so the \
         looked-up word would never reach Anki - fix the mapping in \
         Settings, Anki tab"
    )
}

/// Adds a note, returns its id.
pub fn add_note(
    url: &str,
    deck: &str,
    model: &str,
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
) -> Result<i64> {
    require_expression_route(field_map)?;
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
    require_expression_route(field_map)?;
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
///
/// Flattened across the dictionary's matched term-bank rows, so the field a
/// one-row block produces is unchanged.
fn plain_dict_group(b: &crate::present::GlossBlock) -> String {
    let numbered = b.glosses()
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
    // \n is a no-op; use <li> tags. Rendered here rather than carried on the
    // block: HTML is wanted when a card is mined, which is once, and every
    // hover was paying for it.
    let glossary_html = blocks.iter()
        .map(|b| {
            let items: String = b.entries.iter()
                .flat_map(|e| render_html(&e.doc, Selection::Whole, RoleFilter::CARD))
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
    use crate::present::GlossBlock;
    use serde_json::json;

    /// One block from a glossary payload in the form the record stores, so
    /// the field builder is fed by the real parser and the real renderers
    /// rather than by hand-written strings that no dictionary could produce.
    fn block(dict: &str, glossary: serde_json::Value) -> GlossBlock {
        GlossBlock::parse(dict, &glossary.to_string())
    }

    /// One dictionary's block over several matched term-bank rows, which is
    /// what a headword with more than one row now produces.
    fn rows(dict: &str, glossaries: [serde_json::Value; 2]) -> GlossBlock {
        let entries = glossaries
            .into_iter()
            .filter_map(|g| block(dict, g).entries.into_iter().next())
            .collect();
        GlossBlock {
            dict_name: dict.to_string(),
            dict_id: crate::present::NO_ROW,
            entries,
        }
    }

    /// The headword half of a card. The blocks are passed to
    /// `fields_from_card` separately, so the field builder never reads this
    /// one's own `blocks`.
    fn card(
        written: Option<&str>,
        reading: Option<&str>,
        freq: Option<i64>,
    ) -> crate::present::Card {
        crate::present::Card {
            written: written.map(str::to_string),
            reading: reading.map(str::to_string),
            pos: vec![],
            freq,
            blocks: vec![],
            match_len: 1,
        }
    }

    #[test]
    fn fields_from_card_formats_glossary() {
        let blocks = vec![
            block(
                "大辞林",
                json!([{"type": "structured-content", "content": [
                    "ネコ科の", {"tag": "b", "content": "哺乳類"}, "。"
                ]}]),
            ),
            block(
                "Jitendex",
                json!([
                    "cat",
                    {"type": "structured-content", "content": {"tag": "i", "content": "feline"}}
                ]),
            ),
        ];
        let f = fields_from_card(&card(Some("猫"), Some("ねこ"), Some(42)), &blocks);
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
        let blocks = vec![block("A & B <dict>", json!(["cat"]))];
        let f = fields_from_card(&card(Some("猫"), None, None), &blocks);
        assert_eq!(
            Some(&"<b>A &amp; B &lt;dict&gt;</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>cat</li></ol>".to_string()),
            f.get("glossary_html"),
        );
    }

    #[test]
    fn glossary_html_wraps_one_dictionary_in_a_list_with_no_divider() {
        let blocks = vec![block("Wenlin", json!(["supper", "dinner"]))];
        let f = fields_from_card(&card(Some("猫"), None, None), &blocks);
        let html = f.get("glossary_html").unwrap();
        assert_eq!(
            "<b>Wenlin</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>supper</li><li>dinner</li></ol>",
            html,
        );
        assert!(!html.contains("<hr"), "single dict has no divider");
    }

    #[test]
    fn glossary_html_separates_multiple_dictionaries_with_a_divider() {
        let blocks = vec![
            block("Wenlin", json!(["supper"])),
            block("CC-CEDICT", json!(["evening meal"])),
        ];
        let f = fields_from_card(&card(Some("猫"), None, None), &blocks);
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
        let blocks = vec![
            block("大辞林", json!(["イヌ科の哺乳類。"])),
            block("Jitendex", json!(["dog"])),
        ];
        let f = fields_from_card(&card(Some("犬"), Some("いぬ"), None), &blocks);

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
        let f = fields_from_card(&card(None, Some("ねこ"), None), &[]);
        assert_eq!(Some(&"ねこ".to_string()), f.get("expression"));
        assert_eq!(None, f.get("frequency"));
    }

    /// Story 42: popup density and card completeness are separate choices.
    /// Both fields come off one parsed tree in one call, so the disagreement
    /// here is two filters, not two configurations - the popup's plain-text
    /// summary still drops what the card keeps.
    #[test]
    fn an_example_the_popup_summary_drops_still_reaches_the_html_field() {
        let blocks = vec![block(
            "Jitendex",
            json!([{"type": "structured-content", "content": [
                {"tag": "span", "content": "to eat"},
                {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                    {"tag": "span", "content": "ご飯を食べる"}
                ]}
            ]}]),
        )];
        let f = fields_from_card(&card(Some("食べる"), None, None), &blocks);
        assert_eq!(Some(&"[Jitendex]\n1. to eat".to_string()), f.get("glossary"));
        let html = f.get("glossary_html").expect("the html field");
        assert!(html.contains("ご飯を食べる"), "the card keeps the example: {html}");
    }

    /// Anki has no copy of the dictionary's assets - `src/anki.rs` sends a
    /// screenshot and nothing else - so an image mines as the character it
    /// stands for rather than as a picture the note can never load.
    #[test]
    fn an_image_mines_as_its_alt_text_and_no_field_carries_a_src() {
        let blocks = vec![block(
            "字通",
            json!([{"type": "structured-content", "content": [
                {"tag": "img", "path": "gaiji/x.svg", "alt": "𠮟"},
                {"tag": "span", "content": "to scold"}
            ]}]),
        )];
        let f = fields_from_card(&card(Some("𠮟る"), None, None), &blocks);
        let html = f.get("glossary_html").expect("the html field");
        assert!(html.contains("𠮟<span>to scold</span>"), "the alt text stands in: {html}");
        for (name, value) in &f {
            assert!(!value.contains("<img"), "{name} carries an image tag: {value}");
            assert!(!value.contains("src="), "{name} carries an unresolvable source: {value}");
            assert!(!value.contains("gaiji/x.svg"), "{name} carries an archive path: {value}");
        }
    }

    /// A block is one dictionary, not one matched term-bank row, so both
    /// fields number across the rows it holds: a reader wants "sense 3 of
    /// 大辞林", never "sense 1 of row 2" under a repeated heading.
    #[test]
    fn both_fields_number_a_dictionarys_matched_rows_continuously() {
        let blocks = vec![rows("大辞林", [json!(["to run"]), json!(["to flow", "to stream"])])];
        let f = fields_from_card(&card(Some("走る"), None, None), &blocks);
        assert_eq!(
            Some(&"[大辞林]\n1. to run\n2. to flow\n3. to stream".to_string()),
            f.get("glossary"),
        );
        let html = f.get("glossary_html").expect("the html field");
        assert_eq!(
            concat!(
                "<b>大辞林</b><ol style=\"margin:2px 0 2px 20px;padding:0\">",
                "<li>to run</li><li>to flow</li><li>to stream</li></ol>",
            ),
            html,
        );
        assert!(!html.contains("<hr"), "one dictionary, one heading, no divider: {html}");
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

    // -- the dupe probe (upstream-merge-fallout 03) --

    /// The Lapis default, plus a JP Mining Note-shaped map: the same
    /// sources, none of the same Anki field names.
    fn mining_note_map() -> Vec<crate::config::FieldMapping> {
        vec![
            crate::config::FieldMapping {
                anki_field: "VocabKanji".into(),
                source: "expression".into(),
            },
            crate::config::FieldMapping {
                anki_field: "VocabFurigana".into(),
                source: "reading".into(),
            },
            crate::config::FieldMapping {
                anki_field: "SelectionText".into(),
                source: "glossary".into(),
            },
        ]
    }

    /// The probe must name the field the map names, or AnkiConnect
    /// judges a note the note type has no fields for - which it
    /// refuses as empty, and `collect_dupes` reads as a duplicate.
    #[test]
    fn the_dupe_probe_routes_the_word_through_the_field_map() {
        let body = build_can_add_notes_body(
            "Mining",
            "JP Mining Note",
            &["猫", "犬"],
            &mining_note_map(),
        );
        assert_eq!(Some("canAddNotes"), body["action"].as_str());
        let notes = body["params"]["notes"].as_array().expect("one note per word");
        assert_eq!(2, notes.len());
        assert_eq!(Some("猫"), notes[0]["fields"]["VocabKanji"].as_str());
        assert_eq!(Some("犬"), notes[1]["fields"]["VocabKanji"].as_str());
        assert!(
            notes[0]["fields"].get("Expression").is_none(),
            "a field the note type does not have makes the probe note empty: {}",
            notes[0],
        );
    }

    /// Only the word: the probe has no reading and no glossary to send,
    /// and a blank in a field would be a lie about the note an add
    /// would file.
    #[test]
    fn the_dupe_probe_sends_the_word_and_nothing_else() {
        let body = build_can_add_notes_body("Mining", "JP Mining Note", &["猫"], &mining_note_map());
        let fields = body["params"]["notes"][0]["fields"]
            .as_object()
            .expect("the mapped fields");
        assert_eq!(1, fields.len(), "one field, the word's: {fields:?}");
        assert_eq!(Some("猫"), fields["VocabKanji"].as_str());
    }

    /// One source, two Anki fields: the add sends both, so must the
    /// probe - Anki dupe-checks the note type's first field, and which
    /// of the two that is is not ours to guess.
    #[test]
    fn the_dupe_probe_fills_every_field_the_word_is_routed_to() {
        let map = vec![
            crate::config::FieldMapping { anki_field: "Word".into(), source: "expression".into() },
            crate::config::FieldMapping { anki_field: "Key".into(), source: "expression".into() },
        ];
        let body = build_can_add_notes_body("Default", "Custom", &["猫"], &map);
        let fields = &body["params"]["notes"][0]["fields"];
        assert_eq!(Some("猫"), fields["Word"].as_str());
        assert_eq!(Some("猫"), fields["Key"].as_str());
    }

    /// No network before the check, as with the add.
    #[test]
    fn find_duplicates_rejects_a_field_map_that_drops_the_word() {
        let field_map = vec![crate::config::FieldMapping {
            anki_field: "Expression".into(),
            source: "frequency".into(),
        }];
        let err = find_duplicates("not-a-url", "Default", "Lapis", &["猫"], &field_map)
            .expect_err("a map that never sends the word cannot answer for one");
        assert!(format!("{err:#}").contains("expression"));
    }

    #[test]
    fn find_duplicates_asks_nothing_for_no_expressions() {
        assert!(find_duplicates("not-a-url", "Default", "Lapis", &[], &[])
            .expect("no words, no question, no network")
            .is_empty());
    }

    /// A fake AnkiConnect that judges `canAddNotes` the way Anki does.
    ///
    /// Anki's own answer is `duplicate_or_empty` over the note type's
    /// **first field**: blank is Empty, a repeat of an existing note's
    /// is Duplicate, and `canAddNotes` reports both as a bare `false`.
    /// So the fake needs the first field's name and what is already in
    /// the collection, and nothing else - and a probe that names some
    /// other field gets `false` for every word, which is the bug this
    /// reproduces.
    struct FakeAnki {
        url: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    impl FakeAnki {
        /// Answers one request, then closes.
        fn start(first_field: &str, collection: &[&str]) -> FakeAnki {
            use std::io::Write;
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let url = format!("http://{}", listener.local_addr().expect("the bound address"));
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = seen.clone();
            let first_field = first_field.to_string();
            let held: Vec<String> = collection.iter().map(|s| s.to_string()).collect();
            std::thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let request: serde_json::Value =
                    serde_json::from_str(&read_body(&mut stream)).unwrap_or(serde_json::Value::Null);
                let reply = can_add_reply(&request, &first_field, &held);
                recorded.lock().expect("the request log").push(request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            });
            FakeAnki { url, seen }
        }

        fn seen(&self) -> Vec<serde_json::Value> {
            self.seen.lock().expect("the request log").clone()
        }
    }

    /// One HTTP request's body, by its `Content-Length`.
    fn read_body(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        // Headers first, one byte at a time, so the body is not
        // swallowed into a buffer this function cannot give back.
        while !raw.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => raw.push(byte[0]),
                _ => return String::new(),
            }
        }
        let headers = String::from_utf8_lossy(&raw).to_lowercase();
        let len = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&body).to_string()
    }

    /// `canAddNotes`, per note: false when the first field is blank
    /// (Anki's Empty) or already in the collection (its Duplicate).
    fn can_add_reply(
        request: &serde_json::Value,
        first_field: &str,
        collection: &[String],
    ) -> String {
        let notes = request["params"]["notes"].as_array().cloned().unwrap_or_default();
        let flags: Vec<&str> = notes
            .iter()
            .map(|note| {
                let word = note["fields"][first_field].as_str().unwrap_or("");
                let can_add = !word.is_empty() && !collection.iter().any(|held| held == word);
                if can_add { "true" } else { "false" }
            })
            .collect();
        format!("{{\"result\":[{}],\"error\":null}}", flags.join(","))
    }

    /// The bug, end to end: with a note type whose word field is not
    /// called `Expression`, every word came back a duplicate - the
    /// popup flagged words that were nowhere in the collection, while
    /// the add itself (routed through the same map) filed them fine.
    #[test]
    fn a_word_not_in_the_collection_is_no_dupe_for_a_note_type_of_its_own_naming() {
        let anki = FakeAnki::start("VocabKanji", &["猫"]);
        let dupes = find_duplicates(
            &anki.url,
            "Mining",
            "JP Mining Note",
            &["猫", "犬"],
            &mining_note_map(),
        )
        .expect("the fake answers");
        assert!(dupes.contains("猫"), "猫 is in the collection: {dupes:?}");
        assert!(
            !dupes.contains("犬"),
            "犬 is in no deck; a probe Anki refuses as empty is not a duplicate: {dupes:?}",
        );
        let sent = anki.seen();
        assert_eq!(
            Some("犬"),
            sent[0]["params"]["notes"][1]["fields"]["VocabKanji"].as_str(),
            "the wire carried the mapped field: {sent:?}",
        );
    }

    /// The Lapis default is unchanged: same wire, same answers.
    #[test]
    fn the_default_map_still_probes_the_expression_field() {
        let anki = FakeAnki::start("Expression", &["猫"]);
        let map = crate::config::AnkiConfig::default().field_map;
        let dupes = find_duplicates(&anki.url, "Mining", "Lapis", &["猫", "犬"], &map)
            .expect("the fake answers");
        assert_eq!(HashSet::from(["猫".to_string()]), dupes);
        assert_eq!(
            Some("猫"),
            anki.seen()[0]["params"]["notes"][0]["fields"]["Expression"].as_str(),
        );
    }
}

//! This module sends requests to AnkiConnect v6.

use crate::dict::pitch::marked_morae;
use crate::dict::gloss::{plain_selected, render_html, RoleFilter, Selection, Separator};
use crate::select::CardSelection;
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

/// Returns `true` when AnkiConnect answers the version request.
pub fn check_connection(url: &str) -> bool {
    let body = serde_json::json!({
        "action": "version",
        "version": VERSION,
    });
    post(url, &body).is_ok()
}

fn string_list(resp: &serde_json::Value) -> Result<Vec<String>> {
    let arr = resp.get("result")
        .and_then(|r| r.as_array())
        .context("no result array")?;
    Ok(arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

/// Returns all deck names from AnkiConnect.
pub fn deck_names(url: &str) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "action": "deckNames",
        "version": VERSION,
    });
    string_list(&post(url, &body)?)
}

/// Returns all note-type names from AnkiConnect.
pub fn model_names(url: &str) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "action": "modelNames",
        "version": VERSION,
    });
    string_list(&post(url, &body)?)
}

/// Returns the field names for a note type.
pub fn model_field_names(url: &str, model: &str) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "action": "modelFieldNames",
        "version": VERSION,
        "params": { "modelName": model },
    });
    string_list(&post(url, &body)?)
}

fn deck_id(url: &str, deck: &str) -> Result<i64> {
    let resp = post(url, &serde_json::json!({
        "action": "deckNamesAndIds",
        "version": VERSION,
    }))?;
    let decks = resp.get("result")
        .and_then(|result| result.as_object())
        .context("deckNamesAndIds: no result object")?;
    decks.get(deck)
        .with_context(|| format!("AnkiConnect: deck was not found: {deck}"))?
        .as_i64()
        .context("deckNamesAndIds returned a non-integer deck ID")
}

/// Identifies the note mutation that completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    Added(i64),
    Updated(i64),
}

/// Finds expressions that the selected deck already holds.
///
/// The probe routes each word through `field_map`, as the add operation does.
/// The `canAddNotes` result is `false` for *any* rejection reason.
/// A note with unknown field names is empty, not a duplicate.
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

/// Builds a `canAddNotes` probe.
///
/// Each note contains one word and uses the same `mapped_fields` route as an add.
/// Anki then judges the note that this word receives.
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
                    "duplicateScope": "deck",
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

/// Returns expressions for which `canAddNotes` returns `false`.
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

/// Escapes a value for an Anki search query.
fn escape_search(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace(':', "\\:")
}

/// Wraps a value in quotes.
fn quote_search(value: &str) -> String {
    format!("\"{}\"", escape_search(value))
}

fn build_find_notes_body(
    deck_id: i64,
    model: &str,
    expression: &str,
) -> serde_json::Value {
    let query = format!(
        "did:{deck_id} note:{} \"*:{}\"",
        quote_search(model),
        escape_search(expression)
    );
    serde_json::json!({
        "action": "findNotes",
        "version": VERSION,
        "params": { "query": query },
    })
}

fn note_ids(resp: &serde_json::Value) -> Result<Vec<i64>> {
    resp.get("result")
        .and_then(|r| r.as_array())
        .context("findNotes: no result array")?
        .iter()
        .map(|id| id.as_i64().context("findNotes returned a non-integer note ID"))
        .collect()
}

fn build_notes_info_body(ids: &[i64]) -> serde_json::Value {
    serde_json::json!({
        "action": "notesInfo",
        "version": VERSION,
        "params": { "notes": ids },
    })
}

fn exact_note_ids(
    url: &str,
    deck: &str,
    resp: &serde_json::Value,
    candidate_ids: &[i64],
    model: &str,
    first_field: &str,
    expression: &str,
) -> Result<Vec<i64>> {
    let records = resp
        .get("result")
        .and_then(|r| r.as_array())
        .context("notesInfo: no result array")?;
    let matches: Vec<_> = records
        .iter()
        .filter_map(|record| {
            let note_id = record.get("noteId").and_then(|id| id.as_i64())?;
            let note_model = record.get("modelName").and_then(|name| name.as_str())?;
            let value = record
                .get("fields")
                .and_then(|fields| fields.get(first_field))
                .and_then(|field| field.get("value"))
                .and_then(|value| value.as_str())?;
            let cards = record.get("cards").and_then(|cards| cards.as_array())?;
            (candidate_ids.contains(&note_id) && note_model == model && value == expression)
                .then_some((note_id, cards))
        })
        .collect();
    let cards: Vec<_> = matches.iter()
        .flat_map(|(_, cards)| cards.iter().filter_map(|id| id.as_i64()))
        .collect();
    if cards.is_empty() {
        return Ok(Vec::new());
    }
    let resp = post(url, &serde_json::json!({
        "action": "getDecks",
        "version": VERSION,
        "params": { "cards": cards },
    }))?;
    let decks = resp.get("result")
        .and_then(|result| result.as_object())
        .context("getDecks: no result object")?;
    let Some(deck_cards) = decks.get(deck) else {
        return Ok(Vec::new());
    };
    let deck_cards: HashSet<_> = deck_cards.as_array()
        .context("getDecks: no card ID array")?
        .iter()
        .map(|id| id.as_i64().context("getDecks returned a non-integer card ID"))
        .collect::<Result<_>>()?;
    Ok(matches.into_iter()
        .filter(|(_, cards)| cards.iter()
            .any(|id| id.as_i64().is_some_and(|id| deck_cards.contains(&id))))
        .map(|(note_id, _)| note_id)
        .collect())
}

fn canonical_field_name(model_fields: &[String], configured: &str) -> Option<String> {
    model_fields
        .iter()
        .find(|name| name.eq_ignore_ascii_case(configured))
        .cloned()
}

fn canonical_mapped_fields(
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
    model_fields: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for mapping in field_map {
        let Some(name) = canonical_field_name(model_fields, &mapping.anki_field) else {
            continue;
        };
        let value = fields.get(&mapping.source).cloned().unwrap_or_default();
        out.insert(name, serde_json::json!(value));
    }
    out
}

fn build_update_note_fields_body(
    note_id: i64,
    fields: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "action": "updateNoteFields",
        "version": VERSION,
        "params": { "note": { "id": note_id, "fields": fields } },
    })
}

fn store_media_file(url: &str, picture: &NotePicture) -> Result<String> {
    let body = serde_json::json!({
        "action": "storeMediaFile",
        "version": VERSION,
        "params": {
            "filename": picture.filename,
            "data": picture.data_base64,
            "deleteExisting": false,
        },
    });
    post(url, &body)?
        .get("result")
        .and_then(|result| result.as_str())
        .map(str::to_string)
        .context("storeMediaFile did not return a filename")
}

/// Maps source values to Anki fields.
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

/// Returns `true` when a field-map row sends the specified source.
fn field_map_routes(field_map: &[crate::config::FieldMapping], source: &str) -> bool {
    field_map.iter().any(|m| m.source == source)
}

/// Rejects a map that does not send the expression.
///
/// The add and duplicate probe both need the looked-up word in the note.
/// Without that word, the add creates an empty note and the probe cannot report a duplicate.
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

/// An image to attach to a note.
pub struct NotePicture {
    pub data_base64: String,
    pub filename: String,
    /// Anki fields that embed the image.
    pub fields: Vec<String>,
}

/// Builds an `addNote` request.
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
        "options": { "allowDuplicate": false, "duplicateScope": "deck" },
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

/// Adds a note with an optional picture.
pub fn add_note(
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

/// Adds a note or updates one verified duplicate.
pub fn write_note(
    url: &str,
    deck: &str,
    model: &str,
    fields: &HashMap<String, String>,
    field_map: &[crate::config::FieldMapping],
    picture: Option<&NotePicture>,
    overwrite_duplicates: bool,
) -> Result<WriteResult> {
    require_expression_route(field_map)?;
    if !overwrite_duplicates {
        return add_note(url, deck, model, fields, field_map, picture)
            .map(WriteResult::Added);
    }

    let expression = fields
        .get("expression")
        .filter(|value| !value.is_empty())
        .context("the expression source is empty")?;
    let model_fields = model_field_names(url, model)?;
    let first_field = model_fields.first().context("the note type has no fields")?;
    let expression_maps_first = field_map
        .iter()
        .filter(|mapping| mapping.source == "expression")
        .filter_map(|mapping| canonical_field_name(&model_fields, &mapping.anki_field))
        .any(|field| field == *first_field);
    if !expression_maps_first {
        anyhow::bail!("expression must map to the note type's first field");
    }

    let deck_id = deck_id(url, deck)?;
    let ids = note_ids(&post(
        url,
        &build_find_notes_body(deck_id, model, expression),
    )?)?;
    let matches = if ids.is_empty() {
        Vec::new()
    } else {
        exact_note_ids(
            url,
            deck,
            &post(url, &build_notes_info_body(&ids))?,
            &ids,
            model,
            first_field,
            expression,
        )?
    };
    match matches.as_slice() {
        [] => add_note(url, deck, model, fields, field_map, picture).map(WriteResult::Added),
        [note_id] => {
            let mut update_fields = canonical_mapped_fields(fields, field_map, &model_fields);
            if let Some(picture) = picture {
                let media_fields: Vec<_> = field_map
                    .iter()
                    .filter(|mapping| mapping.source == "screenshot")
                    .filter_map(|mapping| canonical_field_name(&model_fields, &mapping.anki_field))
                    .collect();
                if !media_fields.is_empty() {
                    let filename = store_media_file(url, picture)?;
                    let image = serde_json::json!(format!(
                        "<img src=\"{}\">",
                        escape_html(&filename)
                    ));
                    for media_field in media_fields {
                        update_fields.insert(media_field, image.clone());
                    }
                }
            }
            let body = build_update_note_fields_body(*note_id, update_fields);
            post(url, &body)?;
            Ok(WriteResult::Updated(*note_id))
        }
        _ => anyhow::bail!("multiple exact Anki notes match; no note was changed"),
    }
}

/// Escapes Dictionary text before it enters an Anki HTML field.
/// The text comes from the Yomitan archive's `index.json` and can contain arbitrary characters.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// This value separates Dictionary groups in the HTML that Anki stores for the
/// plain glossary.
///
/// HTML collapses newlines in the source. These newlines cannot provide the
/// visible blank line.
const PLAIN_GROUP_SEPARATOR: &str = "<br><br>\n";

const HTML_GROUP_SEPARATOR: &str = "<hr style=\"border:none;border-top:1px solid #666;margin:4px 0\">";

/// Places a Dictionary name above its plain-text definitions.
///
/// The definitions arrive numbered when the Dictionary is plural, and bare when
/// it is singular. `plain_definition_lines` decides which form applies.
///
/// Anki stores field values as HTML. Square brackets can become furigana under a
/// Japanese note template, so the name uses HTML instead of `[Dictionary]` text.
fn plain_dictionary_group(name: &str, definitions: String, include_dictionary_name: bool) -> String {
    if include_dictionary_name {
        format!("<b>{}</b><br>\n{definitions}", escape_html(name))
    } else {
        definitions
    }
}

/// Places one Dictionary's formatted definitions under its optional heading.
///
/// - A lone definition needs an explicit break after the heading.
/// - A plural group uses the block boundary of its ordered list.
fn html_dictionary_group(name: &str, values: Vec<String>, include_dictionary_name: bool) -> String {
    let heading = if include_dictionary_name {
        let separator = if values.len() == 1 { "<br>" } else { "" };
        format!("<b>{}</b>{separator}", escape_html(name))
    } else {
        String::new()
    };
    let items = html_definition_items(values);
    format!("{heading}{items}")
}

/// Wraps the HTML definition list for one Dictionary group.
///
/// - A lone definition stays bare. An `<ol>` of one item renders as "1." in Anki.
/// - The popup already leaves a lone row unnumbered (`src/ui/layout/chrome.rs`).
/// - A plural list needs `<li>` tags, because a newline has no effect in HTML.
fn html_definition_items(values: Vec<String>) -> String {
    if values.len() > 1 {
        let items =
            values.into_iter().map(|value| format!("<li>{value}</li>")).collect::<String>();
        format!("<ol style=\"margin:2px 0 2px 20px;padding:0\">{items}</ol>")
    } else {
        values.into_iter().collect()
    }
}

/// Joins the plain-text definitions for one Dictionary group.
///
/// - A lone definition carries no number. The item count decides the shape.
/// - Two or more number from one across the rows of one Dictionary, so a reader
///   can identify "sense 3 of 大辞林" rather than "sense 1 of row 2".
fn plain_definition_lines(values: Vec<String>) -> String {
    if values.len() > 1 {
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| format!("{}. {value}", index + 1))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        values.into_iter().collect()
    }
}

/// Builds the glossary fields for the supplied Dictionary groups.
fn glossary_fields<'a>(
    card: &crate::present::Card,
    groups: impl Iterator<Item = (&'a str, Vec<String>, Vec<String>)>,
    include_dictionary_name: bool,
) -> HashMap<String, String> {
    let mut plain_groups = Vec::new();
    let mut html_groups = Vec::new();
    for (dict_name, plain, html) in groups {
        if plain.is_empty() && html.is_empty() {
            continue;
        }
        if !plain.is_empty() {
            plain_groups.push(plain_dictionary_group(
                dict_name,
                plain_definition_lines(plain),
                include_dictionary_name,
            ));
        }
        if !html.is_empty() {
            html_groups.push(html_dictionary_group(
                dict_name,
                html,
                include_dictionary_name,
            ));
        }
    }
    fields_with_glossary(
        card,
        plain_groups.join(PLAIN_GROUP_SEPARATOR),
        html_groups.join(HTML_GROUP_SEPARATOR),
    )
}

/// Builds field values from a card.
pub fn fields_from_card(
    card: &crate::present::Card,
    blocks: &[crate::present::GlossBlock],
    include_dictionary_name: bool,
) -> HashMap<String, String> {
    // Build this field here because card mining occurs once. This avoids work during each hover.
    let groups = blocks.iter().map(|block| {
        let plain = block.glosses().map(str::to_string).collect();
        let html = block
            .entries
            .iter()
            .flat_map(|entry| render_html(&entry.doc, Selection::Whole, RoleFilter::CARD))
            .collect();
        (block.dict_name.as_str(), plain, html)
    });
    glossary_fields(card, groups, include_dictionary_name)
}

/// Builds field values from the selected ranges of every Entry.
///
/// The Controller calls this function only for a non-empty selection. This
/// guard keeps an empty selection on the existing whole-card path.
pub fn fields_from_selection(
    card: &crate::present::Card,
    selection: &CardSelection,
    separator: Separator,
    include_dictionary_name: bool,
) -> HashMap<String, String> {
    let mut ordinal = 0u32;
    let groups = card.blocks.iter().map(|block| {
        let mut plain = Vec::new();
        let mut html = Vec::new();
        for entry in &block.entries {
            let ranges = selection.entry_ranges(ordinal);
            ordinal += 1;
            if ranges.is_empty() {
                continue;
            }
            plain.extend(plain_selected(&entry.doc, &ranges, RoleFilter::CARD, separator));
            html.extend(render_html(
                &entry.doc,
                Selection::Ranges { ranges: &ranges, separator },
                RoleFilter::CARD,
            ));
        }
        (block.dict_name.as_str(), plain, html)
    });
    glossary_fields(card, groups, include_dictionary_name)
}

fn fields_with_glossary(
    card: &crate::present::Card,
    glossary: String,
    glossary_html: String,
) -> HashMap<String, String> {
    let expression = card
        .written
        .as_deref()
        .or(card.reading.as_deref())
        .unwrap_or("")
        .to_string();
    let reading = card.reading.as_deref().unwrap_or("").to_string();
    let mut fields = HashMap::from([
        ("expression".to_string(), expression),
        ("reading".to_string(), reading),
        ("glossary".to_string(), glossary),
        ("glossary_html".to_string(), glossary_html),
    ]);
    if let Some(freq) = card.freq {
        fields.insert("frequency".to_string(), freq.to_string());
    }
    // Put marked kana in the HTML field only. Marked kana *is* markup.
    // A plain-text field cannot preserve the notation without extra words.
    if let Some(pitch) = pitch_html(card) {
        fields.insert("pitch_html".to_string(), pitch);
    }
    fields
}

/// Builds the card's pitch accents in the marked-kana format used by Anki communities.
///
/// A `<span>` uses `border-top` for high moras and `border-right` after the mora where pitch falls.
/// Current community templates style this format, and Yomitan emits the same format.
/// A note mined here therefore renders in a Yomitan deck without a new template.
///
/// The function uses the same `marked_morae` result as the card header.
/// This keeps the field consistent with the panel that the user reads.
/// It adds one `<li>` for each accent in pitch-list order. Each item names its source Dictionaries.
/// The rows are deduplicated, not the raw claims.
///
/// It returns `None` when the card has no accent, so the note has no empty field.
fn pitch_html(card: &crate::present::Card) -> Option<String> {
    let reading = card.reading.as_deref().filter(|r| !r.is_empty())?;
    if card.pitch.is_empty() {
        return None;
    }
    let items: String = card
        .pitch
        .iter()
        .map(|row| {
            let marked: String = marked_morae(reading, &row.accent.position)
                .into_iter()
                .map(|mora| {
                    let mut edges = Vec::new();
                    if mora.high {
                        edges.push("border-top:1px solid currentColor");
                    }
                    if mora.fall {
                        edges.push("border-right:1px solid currentColor");
                    }
                    let text = escape_html(mora.mora.text);
                    if edges.is_empty() {
                        text
                    } else {
                        format!("<span style=\"{}\">{text}</span>", edges.join(";"))
                    }
                })
                .collect();
            let sources = row
                .dicts
                .iter()
                .map(|d| escape_html(d))
                .collect::<Vec<_>>()
                .join(" \u{b7} ");
            format!(
                "<li><span style=\"display:inline-block;white-space:nowrap\">{marked}</span> \
                 <span style=\"opacity:0.6\">{sources}</span></li>"
            )
        })
        .collect();
    Some(format!("<ol style=\"margin:2px 0 2px 20px;padding:0\">{items}</ol>"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::GlossBlock;
    use serde_json::json;

    /// Builds one `GlossBlock` from a glossary payload in the stored format.
    /// Tests then use the real parser and renderers, not strings that a Dictionary cannot produce.
    fn block(dict: &str, glossary: serde_json::Value) -> GlossBlock {
        GlossBlock::parse(dict, &glossary.to_string())
    }

    /// Builds the headword part of a card.
    /// The test passes blocks separately to `fields_from_card`.
    /// This keeps the field builder from reading the card's own `blocks`.
    fn card(
        written: Option<&str>,
        reading: Option<&str>,
        freq: Option<i64>,
    ) -> crate::present::Card {
        crate::present::Card {
            written: written.map(str::to_string),
            reading: reading.map(str::to_string),
            pos: vec![],
            inflections: vec![],
            freq,
            blocks: vec![],
            match_len: 1,
            pitch: Vec::new(),
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
        let f = fields_from_card(&card(Some("猫"), Some("ねこ"), Some(42)), &blocks, true);
        assert_eq!(Some(&"猫".to_string()), f.get("expression"));
        assert_eq!(Some(&"ねこ".to_string()), f.get("reading"));
        assert_eq!(
            Some(
                &"<b>大辞林</b><br>\nネコ科の哺乳類。<br><br>\n<b>Jitendex</b><br>\n1. cat\n2. feline"
                    .to_string()
            ),
            f.get("glossary"),
        );
        assert_eq!(
            Some(&"<b>大辞林</b><br>ネコ科の<b>哺乳類</b>。<hr style=\"border:none;border-top:1px solid #666;margin:4px 0\"><b>Jitendex</b><ol style=\"margin:2px 0 2px 20px;padding:0\"><li>cat</li><li><i>feline</i></li></ol>".to_string()),
            f.get("glossary_html"),
        );
        assert_eq!(Some(&"42".to_string()), f.get("frequency"));
    }

    // Tests for the duplicate probe.

    /// Reads one HTTP request body from its `Content-Length` header.
    fn read_body(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        // Read headers one byte at a time. This keeps body bytes available for the next read.
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

    struct ScriptedAnki {
        url: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    fn scripted_anki(replies: Vec<serde_json::Value>) -> ScriptedAnki {
        let count = replies.len();
        let mut replies = replies.into_iter();
        replying_anki(count, move |_| replies.next().expect("a scripted reply"))
    }

    fn replying_anki(
        count: usize,
        mut reply: impl FnMut(&serde_json::Value) -> serde_json::Value + Send + 'static,
    ) -> ScriptedAnki {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("the bound address"));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        std::thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let request = serde_json::from_str(&read_body(&mut stream))
                    .unwrap_or(serde_json::Value::Null);
                let text = reply(&request).to_string();
                recorded.lock().expect("the request log").push(request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        ScriptedAnki { url, seen }
    }

    fn write_fields() -> Vec<crate::config::FieldMapping> {
        vec![
            crate::config::FieldMapping { anki_field: "front".into(), source: "expression".into() },
            crate::config::FieldMapping { anki_field: "Back".into(), source: "reading".into() },
            crate::config::FieldMapping { anki_field: "Missing".into(), source: "glossary".into() },
        ]
    }

    fn write_input() -> HashMap<String, String> {
        HashMap::from([
            ("expression".into(), "猫".into()),
            ("reading".into(), "ねこ".into()),
        ])
    }

    fn one_note_info(model: &str, id: i64, field: &str, value: &str) -> serde_json::Value {
        serde_json::json!({
            "noteId": id,
            "modelName": model,
            "fields": { field: { "value": value } },
            "cards": [id + 1000],
        })
    }

    #[test]
    fn overwrite_on_updates_the_only_exact_matching_note_id() {
        let server = scripted_anki(vec![
            serde_json::json!({ "result": ["Front", "Back"], "error": null }),
            serde_json::json!({ "result": { "Default": 1 }, "error": null }),
            serde_json::json!({ "result": [42], "error": null }),
            serde_json::json!({ "result": [one_note_info("Basic", 42, "Front", "猫")], "error": null }),
            serde_json::json!({ "result": { "Default": [1042] }, "error": null }),
            serde_json::json!({ "result": null, "error": null }),
        ]);
        let result = write_note(
            &server.url,
            "Default",
            "Basic",
            &write_input(),
            &write_fields(),
            None,
            true,
        )
        .expect("the exact update path");
        assert_eq!(WriteResult::Updated(42), result);
        let seen = server.seen.lock().unwrap();
        let update = seen.last().unwrap();
        assert_eq!("updateNoteFields", update["action"]);
        assert_eq!(42, update["params"]["note"]["id"]);
        assert_eq!("猫", update["params"]["note"]["fields"]["Front"]);
        assert_eq!("ねこ", update["params"]["note"]["fields"]["Back"]);
        assert!(update["params"]["note"]["fields"].get("Missing").is_none());
    }

    #[test]
    fn overwrite_refuses_multiple_exact_matches_without_mutation() {
        let server = scripted_anki(vec![
            serde_json::json!({ "result": ["Front", "Back"], "error": null }),
            serde_json::json!({ "result": { "Default": 1 }, "error": null }),
            serde_json::json!({ "result": [42, 43], "error": null }),
            serde_json::json!({ "result": [
                one_note_info("Basic", 42, "Front", "猫"),
                one_note_info("Basic", 43, "Front", "猫"),
            ], "error": null }),
            serde_json::json!({ "result": { "Default": [1042, 1043] }, "error": null }),
        ]);
        let error = write_note(
            &server.url,
            "Default",
            "Basic",
            &write_input(),
            &write_fields(),
            None,
            true,
        )
        .expect_err("ambiguous matches must fail");
        assert!(format!("{error:#}").contains("multiple exact"));
        let seen = server.seen.lock().unwrap();
        let actions: Vec<_> = seen.iter()
            .map(|request| request["action"].as_str().unwrap()).collect();
        assert_eq!(vec!["modelFieldNames", "deckNamesAndIds", "findNotes", "notesInfo", "getDecks"], actions);
    }

    #[test]
    fn update_failure_never_falls_back_to_add_note() {
        let server = scripted_anki(vec![
            serde_json::json!({ "result": ["Front", "Back"], "error": null }),
            serde_json::json!({ "result": { "Default": 1 }, "error": null }),
            serde_json::json!({ "result": [42], "error": null }),
            serde_json::json!({ "result": [one_note_info("Basic", 42, "Front", "猫")], "error": null }),
            serde_json::json!({ "result": { "Default": [1042] }, "error": null }),
            serde_json::json!({ "result": null, "error": "update failed" }),
        ]);
        let error = write_note(
            &server.url,
            "Default",
            "Basic",
            &write_input(),
            &write_fields(),
            None,
            true,
        )
        .expect_err("update failure must stay a failure");
        assert!(format!("{error:#}").contains("update failed"));
        assert!(server.seen.lock().unwrap().iter().all(|request| {
            request["action"] != "addNote"
        }));
    }

    // Tests for pitch fields.

}

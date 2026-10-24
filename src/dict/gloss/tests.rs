//! Test the GlossDoc parser with one fixture for each real Dictionary shape.
//! Also test the losslessness, robustness, and size limits that the module defines.
//!
//! The fixtures come from the census of 97 archives in `docs/research/dict-shapes.md`.
//! They do not come from assumptions. A fix that passes Jitendex does not prove
//! that 大辞林 works. Use each representative node exactly when the census prints
//! it. Build each other fixture from the hooks and counts that the census names.
//! State which source each fixture uses.

use super::*;
use serde_json::{json, Value};

/// Parse a glossary the way a hover parses it.
///
/// The stored format is minified glossary JSON from the Dictionary. This helper
/// serializes a `Value` to JSON before it parses the GlossDoc. Tests that call
/// this helper cover the stored-format conversion. Tests that call `GlossDoc::parse`
/// directly cover malformed or deeply nested input instead.
pub(super) fn doc(glossary: &Value) -> GlossDoc {
    GlossDoc::parse(&glossary.to_string())
}

/// Return the first node with this tag in parse order.
fn find_tag(d: &GlossDoc, tag: Tag) -> Option<NodeId> {
    (0..d.all_nodes().len() as NodeId).find(|&i| d.node(i).tag == tag)
}

// ---------------------------------------------------------------------------
// losslessness over the census tables
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// one fixture per real dictionary shape
// ---------------------------------------------------------------------------

/// Jitendex. The fixture exercises the census hooks `content=part-of-speech-info`,
/// `content=sense`, `content=glossary`, `content=example-sentence*`, and
/// `content=attribution`.
#[test]
fn jitendex_parses_to_pills_a_sense_list_and_dropped_editorial_matter() {
    let g = json!([{"type": "structured-content", "content": [
        {"tag": "span", "data": {"class": "tag", "code": "n", "content": "part-of-speech-info"},
         "content": "noun"},
        {"tag": "span", "data": {"class": "tag", "code": "vs", "content": "part-of-speech-info"},
         "content": "suru verb"},
        {"tag": "div", "data": {"content": "sense"}, "content": {
            "tag": "ul", "data": {"content": "glossary"}, "style": {"listStyleType": "\"◍\""},
            "content": [
                {"tag": "li", "content": "chatting"},
                {"tag": "li", "content": "idle talk"}
            ]}},
        {"tag": "div", "data": {"content": "example-sentence"}, "content": [
            {"tag": "div", "data": {"content": "example-sentence-a"},
             "content": "もっと果物を食べるべきです。"},
            {"tag": "div", "data": {"content": "example-sentence-b"},
             "content": "You should eat more fruit."}
        ]},
        {"tag": "div", "data": {"content": "attribution"}, "content": [
            {"tag": "a", "href": "https://jitendex.org", "content": "Jitendex"}
        ]}
    ]}]);
    let d = doc(&g);

    assert_eq!(vec!["noun".to_string(), "suru verb".to_string()], pos_labels(&d));
    assert_eq!(vec!["chatting\nidle talk".to_string()], plain_items(&d));

    // The list marker that Jitendex declares inline stays on the node. The PopupScene
    // can draw it, and the role classifier can see the roles beside it.
    let list = find_tag(&d, Tag::Ul).unwrap();
    assert_eq!(
        Some("\"◍\""),
        d.style_of(list, StyleKey::ListStyleType).and_then(|v| d.scalar_str(v))
    );
    let attribution = d.items().next().map(|item| d.children(item).nth(4).unwrap()).unwrap();
    assert_eq!(Role::Attribution, d.role(attribution));
    // The summary drops this node, but the tree keeps it. The popup needs this role
    // for the "show attributions" control to restore it.
    assert_eq!(Tag::A, d.node(d.children(attribution).next().unwrap()).tag);
}

/// 大辞林 第四版. The fixture exercises `name=用例` and `name=慣用例`.
/// It also records its own `①②③` markers inside the tree. Therefore, no code
/// creates an outer sense number.
#[test]
fn daijirin_keeps_its_own_sense_markers_and_its_example_hooks() {
    let g = json!([{"type": "structured-content", "content": {
        "tag": "div", "content": [
            {"tag": "div", "content": [
                {"tag": "span", "data": {"name": "語義番号"}, "content": "①"},
                {"tag": "span", "content": "走る。駆ける。"},
                {"tag": "div", "data": {"name": "用例"}, "content": "馬が―"}
            ]},
            {"tag": "div", "content": [
                {"tag": "span", "data": {"name": "語義番号"}, "content": "②"},
                {"tag": "span", "content": "流れる。"},
                {"tag": "div", "data": {"name": "慣用例"}, "content": "水が―"}
            ]}
        ]}}]);
    let d = doc(&g);

    // Both example hooks classify. 大辞林 now matches Jitendex: two senses form
    // peer blocks. Each block uses its own line, and the Dictionary's marker stays.
    // No code creates a number. Both `用例` blocks leave the summary. The old drop
    // list named neither `用例` nor `慣用例`, so both blocks once rendered.
    assert_eq!(vec!["①走る。駆ける。\n②流れる。".to_string()], plain_items(&d));

    let roles: Vec<(&str, Role)> = (0..d.all_nodes().len() as NodeId)
        .filter_map(|i| d.data_of(i, "name").and_then(|v| d.scalar_str(v)).map(|n| (n, d.role(i))))
        .collect();
    assert_eq!(
        vec![
            ("語義番号", Role::Content),
            ("用例", Role::Example),
            ("語義番号", Role::Content),
            ("慣用例", Role::Example),
        ],
        roles,
        "a sense number is content; a substring needle covers both example spellings"
    );
}

// ---------------------------------------------------------------------------
// editorial role classification
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// robustness
// ---------------------------------------------------------------------------

/// Isolate malformed rows as Yomitan and Hoshi Reader do. A depth cap does not
/// cover a bad node. One unreadable row must cost only that row.
#[test]
fn a_malformed_row_degrades_alone_and_the_rest_of_the_entry_renders() {
    // The middle item lacks a comma between two members. Its brackets still balance,
    // so the splitter isolates it. `serde_json` rejects it after it reads the text.
    let stored = r#"[
        {"type": "structured-content", "content": "first"},
        {"tag": "div", "content": "second" "extra": 1},
        {"type": "structured-content", "content": "third"}
    ]"#;
    let d = GlossDoc::parse(stored);

    assert_eq!(1, d.degraded_items());
    assert_eq!(
        vec!["first".to_string(), "second".to_string(), "third".to_string()],
        plain_items(&d),
        "the broken row loses its wrapper, not its text, and its siblings are untouched"
    );
}

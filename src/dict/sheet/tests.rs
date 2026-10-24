//! Test the matcher against CSS shapes that the corpus ships.
//!
//! Use tables when each input maps to one output. A failure then identifies one
//! wrong row instead of a wrong rule. Each CSS fixture comes from a real stylesheet
//! or a real selector shape. State its source where needed.

use super::*;
use crate::dict::gloss::{plain_items, GlossDoc, NodeId, StyleKey, Tag};
use serde_json::json;

/// Parse one structured-content glossary.
fn doc(content: serde_json::Value) -> GlossDoc {
    GlossDoc::parse(&json!([{"type": "structured-content", "content": content}]).to_string())
}

/// Return the first node with this tag in parse order.
fn find(d: &GlossDoc, tag: Tag) -> NodeId {
    (0..d.all_nodes().len() as NodeId)
        .find(|id| d.node(*id).tag == tag)
        .unwrap_or_else(|| panic!("no {tag:?} node"))
}

/// Return one node's resolved record as `(property, value)` text.
fn record(d: &GlossDoc, id: NodeId) -> Vec<(StyleKey, String)> {
    d.style(id)
        .iter()
        .map(|(k, v)| (*k, d.scalar_str(*v).unwrap_or("<non-text>").to_string()))
        .collect()
}

/// Return the first value for a property in a node record.
/// Every reader of a record uses its first occurrence.
fn prop(d: &GlossDoc, id: NodeId, key: StyleKey) -> Option<String> {
    d.style_of(id, key).and_then(|v| d.scalar_str(v)).map(str::to_string)
}

/// Parse CSS, compile it, and fold it into the document.
fn styled(css: &str, content: serde_json::Value) -> GlossDoc {
    let sheet = Sheet::compile(css);
    let mut d = doc(content);
    apply(&mut d, &sheet);
    d
}

// ---- the data-key to attribute-name transform ----

// ---- the property table ----

// ---- the grammar the census scores against ----

// ---- yomitan's image chrome ----

// ---- a css-only dictionary draws its boxes ----

/// 字通 uses a `data-sc` key on a span and a full box. It has no inline `style` in
/// the entry. This test proves that the mechanism works. Before this change, the box
/// model drew nothing for 13 `css-only` Dictionaries in the census.
#[test]
fn a_css_only_dictionary_gets_a_box_with_no_inline_style_anywhere() {
    let d = styled(
        "span[data-sc音] {
             background-color: #eef;
             border-style: solid;
             border-width: 1px;
             border-radius: 0.3em;
             padding: 0.1em 0.4em;
         }",
        json!([{"tag": "span", "data": {"音": "呉"}, "content": "ゴ"}]),
    );
    let span = find(&d, Tag::Span);
    assert_eq!(
        vec![
            (StyleKey::BackgroundColor, "#eef".to_string()),
            (StyleKey::BorderStyle, "solid".to_string()),
            (StyleKey::BorderWidth, "1px".to_string()),
            (StyleKey::BorderRadius, "0.3em".to_string()),
            (StyleKey::PaddingTop, "0.1em".to_string()),
            (StyleKey::PaddingRight, "0.4em".to_string()),
            (StyleKey::PaddingBottom, "0.1em".to_string()),
            (StyleKey::PaddingLeft, "0.4em".to_string()),
        ],
        record(&d, span),
    );
    // The tree that the renderer reads stays otherwise unchanged.
    assert_eq!(vec!["ゴ".to_string()], plain_items(&d));
}

// ---- the cascade ----

/// This build accepts no id or class selector. Its specificity is `(b, c)`.
/// `b` counts attribute tests and pseudo-classes. `c` counts tag names.
/// Compare the pair as one packed number. Use source order to break ties, and let
/// the later rule win. `:where` adds nothing. `:is`, `:not`, and `:has` provide their
/// most specific argument.
#[test]
fn a_more_specific_rule_beats_a_less_specific_one() {
    let cases: [(&str, &str, &str); 5] = [
        // A (0,1) tag loses to a (1,0) attribute.
        ("span { color: red } [data-sc-content=\"a\"] { color: blue }", "blue", "attr > tag"),
        // A (1,1) beats a (1,0), regardless of source order.
        (
            "span[data-sc-content=\"a\"] { color: blue } [data-sc-content=\"a\"] { color: red }",
            "blue",
            "tag+attr > attr, against source order",
        ),
        // A (2,1) beats a (1,1).
        (
            "span[data-sc-content=\"a\"] { color: red }
             div span[data-sc-content=\"a\"] { color: red }
             span[data-sc-content=\"a\"][data-sc-class=\"b\"] { color: blue }",
            "blue",
            "two attributes beat one attribute and two tags",
        ),
        // Equal specificity: the later rule wins.
        ("span { color: red } span { color: blue }", "blue", "source order breaks a tie"),
        // `:where` adds nothing, so (0,1) loses to (1,0).
        (
            ":where([data-sc-class=\"b\"]) span { color: red }
             [data-sc-content=\"a\"] { color: blue }",
            "blue",
            ":where contributes no specificity",
        ),
    ];
    for (css, want, why) in cases {
        let d = styled(
            css,
            json!([{"tag": "div", "data": {"class": "b"}, "content": [
                {"tag": "span", "data": {"content": "a", "class": "b"}, "content": "x"}
            ]}]),
        );
        let span = find(&d, Tag::Span);
        assert_eq!(Some(want.to_string()), prop(&d, span, StyleKey::Color), "{why}");
    }
}

// ---- CSS nesting ----

// ---- shorthand expansion ----

// ---- what is dropped, and counted ----

// ---- malformed input ----

// ---- the fold itself ----

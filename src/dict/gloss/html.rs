//! The HTML renderer, for the Anki `glossary_html` field.
//!
//! The second of three renderers over [`GlossDoc`]. Before ticket 02 this was
//! an independent recursion over the raw JSON that mirrored the plain-text
//! walk's drop rules by hand - two views of one tree, which is the bug class
//! the spec set out to close. It now reads the same parsed tree, so the card
//! and the popup cannot disagree about what an entry contains.
//!
//! Structured content is untrusted input from a dictionary file rather than
//! from chibipop, so tags and attributes are allow-listed rather than passed
//! through: an unrecognised tag keeps its text and drops the wrapper, the same
//! way [`Kind::Unknown`](super::Kind) does in the tree itself.
//!
//! This runs when a card is mined, not per hover, so it builds owned strings
//! rather than threading a buffer.

use super::{GlossDoc, ItemType, Kind, NodeId, Scalar, StyleKey, Tag};

/// Tags this renderer emits. `details`, `summary` and `tfoot` are absent
/// because Anki's note templates predate them here and a wrapper this
/// renderer drops still keeps its text; `img` is absent because
/// `src/anki.rs` has no media upload path, so an image has nothing to point
/// at (ticket 04 renders its `alt` text instead).
fn html_tag(tag: Tag) -> Option<&'static str> {
    matches!(
        tag,
        Tag::A
            | Tag::B
            | Tag::Strong
            | Tag::I
            | Tag::Em
            | Tag::U
            | Tag::S
            | Tag::Small
            | Tag::Big
            | Tag::Sub
            | Tag::Sup
            | Tag::Code
            | Tag::Pre
            | Tag::Span
            | Tag::Div
            | Tag::Ruby
            | Tag::Rt
            | Tag::Rp
            | Tag::Br
            | Tag::Ul
            | Tag::Ol
            | Tag::Li
            | Tag::Table
            | Tag::Thead
            | Tag::Tbody
            | Tag::Tr
            | Tag::Th
            | Tag::Td
    )
    .then(|| tag.as_str())
}

/// Style properties this renderer maps to CSS, in emission order.
///
/// Deliberately small: the properties dictionaries reach for that an Anki
/// card can honour (superscript reference marks, small print, coloured tags),
/// not a general style-object translator. The popup renderer reads the whole
/// resolved record; a note template does not need it.
const STYLE_KEYS: [(StyleKey, &str); 6] = [
    (StyleKey::FontWeight, "font-weight"),
    (StyleKey::FontStyle, "font-style"),
    (StyleKey::FontSize, "font-size"),
    (StyleKey::VerticalAlign, "vertical-align"),
    (StyleKey::TextDecorationLine, "text-decoration"),
    (StyleKey::ListStyleType, "list-style-type"),
];

/// One HTML fragment per top-level glossary item - parallel to
/// [`plain_items`](super::plain_items).
pub fn render_html(doc: &GlossDoc) -> Vec<String> {
    let mut out = Vec::new();
    for id in doc.items() {
        let html = match doc.node(id).item_type {
            ItemType::Image => String::new(),
            ItemType::Text => escape_text(doc.text(id)),
            ItemType::StructuredContent => children_html(doc, id),
            _ if doc.is_plain_string(id) => escape_text(doc.text(id)),
            _ => node_html(doc, id),
        };
        let trimmed = html.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// One node to HTML. Mirrors the plain renderer's drop rules; only what a
/// kept node turns into differs.
fn node_html(doc: &GlossDoc, id: NodeId) -> String {
    let node = doc.node(id);
    if doc.is_dropped_subtree(id) || doc.is_part_of_speech(id) {
        return String::new();
    }
    if node.kind == Kind::Image {
        return String::new();
    }
    if node.kind == Kind::Text && node.tag == Tag::None {
        return escape_text(doc.text(id));
    }
    match html_tag(node.tag) {
        Some("br") => "<br>".to_string(),
        Some(tag) => {
            let inner = children_html(doc, id);
            if inner.trim().is_empty() {
                // Nothing left after drops (e.g. an image-only wrapper): no
                // point in an empty tag shell.
                return inner;
            }
            let attrs = html_attrs(doc, id, node.tag);
            format!("<{tag}{attrs}>{inner}</{tag}>")
        }
        // Not a tag we emit: keep the text, drop the wrapper.
        None => children_html(doc, id),
    }
}

fn children_html(doc: &GlossDoc, id: NodeId) -> String {
    doc.children(id).map(|child| node_html(doc, child)).collect()
}

/// `href` on `<a>`, `colSpan`/`rowSpan` on cells, and a small `style` subset
/// on anything - a leading-space-prefixed attribute string, or empty.
fn html_attrs(doc: &GlossDoc, id: NodeId, tag: Tag) -> String {
    let mut attrs = String::new();
    if tag == Tag::A {
        if let Some(href) = doc.attr_of(id, "href").and_then(|v| doc.scalar_str(v)) {
            attrs.push_str(&format!(" href=\"{}\"", escape_attr(href)));
        }
    }
    if tag == Tag::Td || tag == Tag::Th {
        for (key, html_attr) in [("colSpan", "colspan"), ("rowSpan", "rowspan")] {
            if let Some(n) = doc.attr_of(id, key).and_then(count) {
                attrs.push_str(&format!(" {html_attr}=\"{n}\""));
            }
        }
    }
    let decls: Vec<String> = STYLE_KEYS
        .iter()
        .filter_map(|(key, css)| {
            doc.style_of(id, *key).map(|v| format!("{css}:{}", style_value(doc, v)))
        })
        .collect();
    if !decls.is_empty() {
        attrs.push_str(&format!(" style=\"{}\"", escape_attr(&decls.join(";"))));
    }
    attrs
}

/// A span count: a non-negative whole number, or nothing.
fn count(v: Scalar) -> Option<u64> {
    match v {
        Scalar::Num(n) if n >= 0.0 && n.fract() == 0.0 => Some(n as u64),
        _ => None,
    }
}

/// A style value as CSS. Numbers are Yomitan's em-multiplier convention; a
/// list-valued property such as `textDecorationLine` was space-joined at
/// parse time, so it arrives as one string.
fn style_value(doc: &GlossDoc, v: Scalar) -> String {
    match v {
        Scalar::Num(n) => format!("{n}em"),
        Scalar::Text(s) => doc.span(s).to_string(),
        _ => String::new(),
    }
}

/// Escapes text content. Not attribute-safe on its own - see [`escape_attr`].
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// [`escape_text`] plus quotes, for use inside a `"..."` attribute value.
fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc;
    use super::*;
    use serde_json::{json, Value};

    fn html(g: &Value) -> Vec<String> {
        render_html(&doc(g))
    }

    fn pos_doc(labels: &[&str]) -> Value {
        let mut spans: Vec<Value> = labels
            .iter()
            .map(|label| {
                json!({
                    "tag": "span",
                    "data": {"class": "tag", "code": "n", "content": "part-of-speech-info"},
                    "content": label,
                })
            })
            .collect();
        spans.push(json!({
            "tag": "div",
            "data": {"content": "sense"},
            "content": {
                "tag": "ul",
                "data": {"content": "glossary"},
                "content": [
                    {"tag": "li", "content": "chatting"},
                    {"tag": "li", "content": "idle talk"},
                ],
            },
        }));
        json!([{"type": "structured-content", "content": spans}])
    }

    #[test]
    fn a_plain_string_gloss_is_escaped_but_unwrapped() {
        assert_eq!(vec!["to eat".to_string()], html(&json!(["to eat"])));
    }

    #[test]
    fn a_typed_text_node_is_escaped() {
        let g = json!([{"type": "text", "text": "5 < 10 & true"}]);
        assert_eq!(vec!["5 &lt; 10 &amp; true".to_string()], html(&g));
    }

    #[test]
    fn known_inline_tags_are_preserved() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "b", "content": "bold"},
            {"tag": "span", "content": " and "},
            {"tag": "i", "content": "italic"}
        ]}]);
        assert_eq!(vec!["<b>bold</b><span> and </span><i>italic</i>".to_string()], html(&g));
    }

    #[test]
    fn ruby_and_rt_are_kept_unlike_the_plain_text_renderer() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "content": "ねこ"}
            ]}
        ]}]);
        assert_eq!(vec!["<ruby><span>猫</span><rt>ねこ</rt></ruby>".to_string()], html(&g));
    }

    #[test]
    fn a_link_keeps_its_href() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "a", "href": "?query=見出し語", "content": "見出し語"}
        ]}]);
        assert_eq!(vec!["<a href=\"?query=見出し語\">見出し語</a>".to_string()], html(&g));
    }

    #[test]
    fn an_href_value_is_attribute_escaped() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "a", "href": "\"><b>x</b>", "content": "z"}
        ]}]);
        assert_eq!(
            vec!["<a href=\"&quot;&gt;&lt;b&gt;x&lt;/b&gt;\">z</a>".to_string()],
            html(&g)
        );
    }

    #[test]
    fn an_unrecognised_tag_keeps_its_text_and_drops_the_wrapper() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "script", "content": "alert(1)"}
        ]}]);
        assert_eq!(vec!["alert(1)".to_string()], html(&g));
    }

    #[test]
    fn img_is_dropped_in_html_mode_too() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg"},
            {"tag": "span", "content": "meaning"}
        ]}]);
        assert_eq!(vec!["<span>meaning</span>".to_string()], html(&g));
    }

    #[test]
    fn br_becomes_a_void_element() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "a"},
            {"tag": "br"},
            {"tag": "span", "content": "b"}
        ]}]);
        assert_eq!(vec!["<span>a</span><br><span>b</span>".to_string()], html(&g));
    }

    #[test]
    fn lists_render_as_real_markup_not_a_separator_hack() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "ul", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"}
            ]}}]);
        assert_eq!(vec!["<ul><li>first</li><li>second</li></ul>".to_string()], html(&g));
    }

    #[test]
    fn attribution_and_example_sentences_are_dropped_in_html_mode_too() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "to eat"},
            {"tag": "div", "data": {"content": "attribution"}, "content": [
                {"tag": "a", "content": "JMdict"}
            ]},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "content": "a sentence"}
            ]}
        ]}]);
        assert_eq!(vec!["<span>to eat</span>".to_string()], html(&g));
    }

    #[test]
    fn part_of_speech_spans_do_not_leak_into_html_glosses_either() {
        let g = pos_doc(&["noun", "suru"]);
        assert_eq!(
            vec!["<div><ul><li>chatting</li><li>idle talk</li></ul></div>".to_string()],
            html(&g)
        );
    }

    #[test]
    fn an_inline_style_object_becomes_a_css_style_attribute() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "style": {"fontWeight": "bold", "fontSize": 0.8},
             "content": "note"}
        ]}]);
        assert_eq!(
            vec!["<span style=\"font-weight:bold;font-size:0.8em\">note</span>".to_string()],
            html(&g)
        );
    }

    #[test]
    fn a_table_cell_keeps_its_span_attributes() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "table", "content": {
                "tag": "tr", "content": {
                    "tag": "td", "colSpan": 2, "content": "wide"
                }
            }
        }}]);
        assert_eq!(
            vec!["<table><tr><td colspan=\"2\">wide</td></tr></table>".to_string()],
            html(&g)
        );
    }

    #[test]
    fn an_image_only_sense_yields_nothing_in_html_mode() {
        assert!(html(&json!([{"type": "image", "path": "x.png"}])).is_empty());
    }

    #[test]
    fn an_all_whitespace_html_result_is_dropped() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": ["  ", {"tag": "img"}, "  "]}}]);
        assert!(html(&g).is_empty());
    }

    /// A list-valued style property was space-joined at parse time, so it
    /// reaches CSS as one declaration rather than as an array literal.
    #[test]
    fn a_list_valued_style_property_joins_with_a_space() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "style": {"textDecorationLine": ["underline", "overline"]},
             "content": "x"}
        ]}]);
        assert_eq!(
            vec!["<span style=\"text-decoration:underline overline\">x</span>".to_string()],
            html(&g)
        );
    }
}

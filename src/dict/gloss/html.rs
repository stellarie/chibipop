//! The HTML renderer for the Anki `glossary_html` field.
//!
//! This module is the second of three renderers over [`GlossDoc`].
//! Before the arena rewrite, it walked raw JSON on its own.
//! It copied the plain-text walk's rules for dropped nodes.
//! That duplicate tree walk caused inconsistent views of one Entry.
//! This renderer reads the parsed tree.
//! The card and the popup therefore agree about Entry content.
//!
//! The card and the popup can choose different content.
//! The popup is a hover panel.
//! The card is a permanent record.
//! This renderer therefore uses its own [`RoleFilter`].
//! The popup can hide an example on screen.
//! The mined card keeps that example.
//!
//! This renderer also accepts a [`Selection`].
//! It can render any set of subtrees, not only a whole document.
//! This is the Anki half of the sense picker.
//! It returns formatted markup for the selected senses.
//! It never returns one flattened text range.
//!
//! Structured content comes from a Dictionary file, not from chibipop.
//! It is untrusted input.
//! The renderer accepts only known tags, attributes, and link schemes.
//! It checks each value before output.
//! An unrecognized tag keeps its text and drops its wrapper.
//! [`Kind::Unknown`](super::Kind) uses the same rule in the tree.
//!
//! This code runs when a card is mined, not on every hover.
//! It therefore builds owned strings.
//! It does not pass a scratch buffer through the recursion.

use super::{GlossDoc, ItemType, Kind, NodeId, NodePath, Role, Scalar, Selection, StyleKey, Tag};

/// The editorial roles that reach the output.
///
/// The caller supplies this value.
/// This module does not choose it.
/// The card answer and the popup answer stay separate.
/// The popup has its own settings.
/// Those settings do not change this value.
///
/// This struct has three controls for six [`Role`] values.
/// Three roles cannot be dropped.
/// The parser already classifies [`Role::Reference`] and [`Role::Commentary`].
/// A later filter can add controls for them without a classifier change.
/// [`Role::Content`] is the role for a node with no recognized hook.
/// [`allows`](Self::allows) covers every role and keeps roles without controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoleFilter {
    pub examples: bool,
    pub attributions: bool,
    pub part_of_speech: bool,
}

impl RoleFilter {
    /// Content that a mined card keeps: all Entry content except part-of-speech labels.
    ///
    /// A card is a record, not a panel.
    /// Card density does not justify a drop.
    /// Part-of-speech labels are the only exception because the card has a separate field.
    /// `Card::pos` renders above the glosses.
    /// An inline label appears twice.
    pub const CARD: RoleFilter =
        RoleFilter { examples: true, attributions: true, part_of_speech: false };

    /// What a one-line summary keeps: gloss content without optional editorial roles.
    ///
    /// This is the plain-text renderer's policy.
    /// It replaces the deleted list of roles that matched names.
    /// That list checked six `data.content` values: three example roles,
    /// two attribution roles, and the part-of-speech marker.
    /// This constant gives that policy to every Dictionary.
    /// It does not depend on the spellings in one Dictionary.
    ///
    /// Both consumers are summaries by construction.
    /// The collapsed row is one truncated line.
    /// The Anki plain-text `glossary` field sits beside `glossary_html`.
    /// That field uses [`CARD`](Self::CARD) instead and keeps every example.
    /// The card keeps examples through its markup field.
    /// It does not place three sentences per sense in a numbered plain-text list.
    pub const SUMMARY: RoleFilter =
        RoleFilter { examples: false, attributions: false, part_of_speech: false };

    /// Returns true when this filter keeps a node with `role`.
    ///
    /// This method serves the HTML renderer, the plain-text renderer, and the popup filter in
    /// `ui::layout`.
    /// One predicate over one enum gives the card and the panel the same rule for examples.
    pub fn allows(self, role: Role) -> bool {
        match role {
            Role::Example => self.examples,
            Role::Attribution => self.attributions,
            Role::PartOfSpeech => self.part_of_speech,
            Role::Content | Role::Reference | Role::Commentary => true,
        }
    }
}

impl Default for RoleFilter {
    fn default() -> Self {
        RoleFilter::CARD
    }
}

/// HTML tags that this renderer emits.
///
/// `details`, `summary`, and `tfoot` are absent because Anki note templates predate them.
/// A wrapper that this renderer drops still keeps its text.
/// `img` is absent because `src/anki.rs` has no media upload path.
/// An image has no file to reference, so it emits its `alt` text instead.
/// See [`image_html`].
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

/// Returns the CSS name for a resolved style property.
///
/// This method emits the full record, not a subset.
/// A Dictionary's `styles.css` folds into this record.
/// The census has 13 CSS-only Dictionaries.
/// Their pills use only box properties.
/// A subset leaves those Dictionaries as unstyled text while the popup draws their pills.
fn css_name(key: StyleKey) -> &'static str {
    match key {
        StyleKey::FontStyle => "font-style",
        StyleKey::FontWeight => "font-weight",
        StyleKey::FontSize => "font-size",
        // Use the shorthand, not `text-decoration-line`.
        // Some Anki note templates do not support the newer longhand.
        StyleKey::TextDecorationLine => "text-decoration",
        StyleKey::TextDecorationStyle => "text-decoration-style",
        StyleKey::TextDecorationColor => "text-decoration-color",
        StyleKey::VerticalAlign => "vertical-align",
        StyleKey::TextAlign => "text-align",
        StyleKey::WhiteSpace => "white-space",
        StyleKey::WordBreak => "word-break",
        StyleKey::Cursor => "cursor",
        StyleKey::ListStyleType => "list-style-type",
        StyleKey::Color => "color",
        StyleKey::BackgroundColor => "background-color",
        StyleKey::BorderColor => "border-color",
        StyleKey::BorderStyle => "border-style",
        StyleKey::BorderRadius => "border-radius",
        StyleKey::BorderWidth => "border-width",
        StyleKey::Margin => "margin",
        StyleKey::MarginTop => "margin-top",
        StyleKey::MarginRight => "margin-right",
        StyleKey::MarginBottom => "margin-bottom",
        StyleKey::MarginLeft => "margin-left",
        StyleKey::Padding => "padding",
        StyleKey::PaddingTop => "padding-top",
        StyleKey::PaddingRight => "padding-right",
        StyleKey::PaddingBottom => "padding-bottom",
        StyleKey::PaddingLeft => "padding-left",
    }
}

/// Returns one HTML fragment for each selected node.
///
/// [`plain_items`](super::plain_items) uses a whole-document walk.
///
/// [`Selection::Whole`] gives one fragment per top-level glossary item.
/// `src/anki.rs` puts each fragment in one `<li>` element.
/// [`Selection::Nodes`] gives one fragment per selected node in document order.
/// Each fragment is byte-for-byte the markup that the whole document emits for that subtree.
pub fn render_html(doc: &GlossDoc, select: Selection<'_>, roles: RoleFilter) -> Vec<String> {
    let mut out = Vec::new();
    match select {
        Selection::Whole => {
            for id in doc.items() {
                push(item_html(doc, id, roles), &mut out);
            }
        }
        Selection::Nodes(wanted) => {
            for (i, id) in doc.items().enumerate() {
                // A path step that does not fit addresses no node.
                // No descendant can match that path.
                let Some(path) = NodePath::ROOT.child(i) else { continue };
                collect(doc, id, path, wanted, roles, &mut out);
            }
        }
    }
    out
}

/// Emits each selected node at or under `id` in document order.
///
/// A selected node stops descent.
/// If a picker selects a sense and a descendant, this method emits the sense once.
fn collect(
    doc: &GlossDoc,
    id: NodeId,
    path: NodePath,
    wanted: &[NodePath],
    roles: RoleFilter,
    out: &mut Vec<String>,
) {
    if wanted.contains(&path) {
        // A top-level item and an inner node use different rules.
        // This renderer emits a bare glossary string without a wrapper.
        // It emits the children of a structured-content item.
        // The fragment for a selected item must match the item's own fragment.
        // Otherwise, a selection for the whole Entry does not reproduce the Entry.
        let html =
            if path.len() == 1 { item_html(doc, id, roles) } else { node_html(doc, id, roles) };
        push(html, out);
        return;
    }
    for (i, child) in doc.children(id).enumerate() {
        let Some(next) = path.child(i) else { continue };
        collect(doc, child, next, wanted, roles, out);
    }
}

/// Whitespace-only output is empty.
/// An item whose filter drops every node produces no `<li>` element.
fn push(html: String, out: &mut Vec<String>) {
    let trimmed = html.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
}

/// Converts one top-level glossary item to HTML.
fn item_html(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> String {
    if !roles.allows(doc.role(id)) {
        return String::new();
    }
    match doc.node(id).item_type {
        ItemType::Text => escape_text(doc.text(id)),
        ItemType::StructuredContent => children_html(doc, id, roles),
        _ if doc.is_plain_string(id) => escape_text(doc.text(id)),
        _ => node_html(doc, id, roles),
    }
}

/// Converts one node to HTML.
fn node_html(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> String {
    let node = doc.node(id);
    if !roles.allows(doc.role(id)) {
        return String::new();
    }
    if node.kind == Kind::Image {
        return image_html(doc, id);
    }
    if node.kind == Kind::Text && node.tag == Tag::None {
        return escape_text(doc.text(id));
    }
    match html_tag(node.tag) {
        Some("br") => "<br>".to_string(),
        Some(tag) => {
            let inner = children_html(doc, id, roles);
            if inner.trim().is_empty() {
                // A filter can remove all children, such as an image-only wrapper.
                // An empty tag shell has no content, so this method drops it.
                return inner;
            }
            let attrs = html_attrs(doc, id, node.tag);
            format!("<{tag}{attrs}>{inner}</{tag}>")
        }
        // This renderer does not emit the tag. It keeps the text and drops the wrapper.
        None => children_html(doc, id, roles),
    }
}

fn children_html(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> String {
    doc.children(id).map(|child| node_html(doc, child, roles)).collect()
}

/// Returns image content as text.
///
/// The tree keeps image nodes that the old flattener dropped.
/// `src/anki.rs` has no `storeMediaFile` path.
/// An `<img>` tag names a file that Anki does not have.
/// The `alt` text identifies the character that the image represents.
/// The `title` value is the fallback label.
/// If neither value exists, this method emits nothing.
/// This matches the behavior before this renderer handled image nodes.
fn image_html(doc: &GlossDoc, id: NodeId) -> String {
    for key in ["alt", "title"] {
        match doc.attr_of(id, key).and_then(|v| doc.scalar_str(v)) {
            Some(text) if !text.trim().is_empty() => return escape_text(text),
            _ => {}
        }
    }
    String::new()
}

/// Returns an HTML attribute string for a node.
///
/// It includes `href` on `<a>`, `colSpan` and `rowSpan` on cells, and the resolved style record
/// on any tag.
/// The result starts with one space when it has an attribute.
/// It is empty when no attribute applies.
fn html_attrs(doc: &GlossDoc, id: NodeId, tag: Tag) -> String {
    let mut attrs = String::new();
    if tag == Tag::A {
        if let Some(href) = doc.attr_of(id, "href").and_then(|v| doc.scalar_str(v)) {
            if safe_href(href) {
                attrs.push_str(&format!(" href=\"{}\"", escape_attr(href)));
            }
        }
    }
    if tag == Tag::Td || tag == Tag::Th {
        for (key, html_attr) in [("colSpan", "colspan"), ("rowSpan", "rowspan")] {
            if let Some(n) = doc.attr_of(id, key).and_then(count) {
                attrs.push_str(&format!(" {html_attr}=\"{n}\""));
            }
        }
    }
    let style = style_attr(doc, id);
    if !style.is_empty() {
        attrs.push_str(&format!(" style=\"{}\"", escape_attr(&style)));
    }
    attrs
}

/// Converts the resolved style record to one CSS declaration list.
///
/// This method emits properties in [`StyleKey`] declaration order, not record order.
/// Card markup therefore does not depend on source property order in a Dictionary.
/// This order places each shorthand before its longhands.
/// CSS uses this order to preserve the intended value.
/// If `margin` follows `marginTop`, it silently erases that property.
fn style_attr(doc: &GlossDoc, id: NodeId) -> String {
    let record = doc.style(id);
    let mut decls: Vec<(u8, String)> = Vec::with_capacity(record.len());
    for (i, (key, value)) in record.iter().enumerate() {
        // Keep the first occurrence.
        // This matches the value that `GlossDoc::style_of` returns to every other reader.
        if record[..i].iter().any(|(seen, _)| seen == key) {
            continue;
        }
        if let Some(v) = style_value(doc, *value) {
            decls.push((*key as u8, format!("{}:{v}", css_name(*key))));
        }
    }
    decls.sort_by_key(|(order, _)| *order);
    decls.iter().map(|(_, decl)| decl.as_str()).collect::<Vec<_>>().join(";")
}

/// Returns a non-negative whole-number span count, or `None`.
fn count(v: Scalar) -> Option<u64> {
    match v {
        Scalar::Num(n) if n >= 0.0 && n.fract() == 0.0 => Some(n as u64),
        _ => None,
    }
}

/// Converts a style value to CSS, or returns `None` when the schema rejects it.
///
/// Yomitan uses numbers as em multipliers.
/// The parser joins list values, such as `textDecorationLine`, with spaces.
/// This method receives that result as one string.
/// A boolean, null, or blank string is not a declaration.
/// It therefore drops the value and emits no property.
fn style_value(doc: &GlossDoc, v: Scalar) -> Option<String> {
    match v {
        Scalar::Num(n) => Some(format!("{n}em")),
        Scalar::Text(s) => {
            let text = doc.span(s).trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// Returns true when a card can follow this link.
///
/// A Dictionary's cross-references have no scheme, for example `?query=見出し語`.
/// Citations use `http` or `https`.
/// Other schemes, such as `javascript:`, `data:`, or `vbscript:`, can execute script
/// in Anki's webview.
/// The value comes from a file that chibipop did not write.
/// The `<a>` tag therefore keeps its text but loses its `href`.
/// This method removes whitespace and control characters before it reads the scheme.
/// An HTML parser ignores those characters inside a URL.
/// A direct check can miss a hidden scheme.
fn safe_href(url: &str) -> bool {
    let cleaned: String =
        url.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    match cleaned.find([':', '/', '?', '#']) {
        // A scheme ends at the first `:`.
        // Only `http` and `https` pass this check.
        Some(at) if cleaned.as_bytes()[at] == b':' => {
            let scheme = &cleaned[..at];
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }
        // No scheme means a relative link.
        _ => true,
    }
}

/// Escapes text content.
/// This result is not safe inside an attribute value. See [`escape_attr`].
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

/// Escapes text content and quotes for a `"..."` attribute value.
fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::super::plain_items;
    use super::super::tests::doc;
    use super::*;
    use serde_json::{json, Value};

    fn html(g: &Value) -> Vec<String> {
        render_html(&doc(g), Selection::Whole, RoleFilter::CARD)
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

    /// A Jitendex-shaped Entry has a gloss, an example, and an attribution.
    /// Each node uses the `data.content` hook that the census records for its role.
    /// The parser classifies the nodes, so the test does not define their roles.
    fn gloss_with_editorial_matter() -> Value {
        json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "to eat"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "content": "ご飯を食べる"}
            ]},
            {"tag": "ul", "data": {"content": "attribution"}, "content": [
                {"tag": "li", "content": "JMdict"}
            ]}
        ]}])
    }

    /// Two senses are sibling blocks inside one glossary item.
    /// Sixty-four of the 72 census Dictionaries use this shape.
    /// The sense picker uses this shape.
    fn two_senses() -> Value {
        json!([{"type": "structured-content", "content": [
            {"tag": "div", "data": {"content": "sense"}, "content": [
                {"tag": "span", "content": "to run"},
                {"tag": "b", "content": " (of a person)"}
            ]},
            {"tag": "div", "data": {"content": "sense"}, "content": [
                {"tag": "span", "content": "to flow"},
                {"tag": "i", "content": " (of liquid)"}
            ]}
        ]}])
    }

    const SENSE_TWO: &str = "<div><span>to flow</span><i> (of liquid)</i></div>";

    // -- cases from before the arena rewrite --

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

    /// A card uses ruby markup.
    /// The plain-text renderer puts the pronunciation in parentheses after its base instead.
    #[test]
    fn ruby_and_rt_keep_their_own_markup() {
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
    fn an_all_whitespace_html_result_is_dropped() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": ["  ", {"tag": "img"}, "  "]}}]);
        assert!(html(&g).is_empty());
    }

    /// The parser joins a list-valued style property with spaces.
    /// The HTML renderer therefore receives one declaration value, not an array literal.
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

    // -- image output: alt text, no Anki src --

    #[test]
    fn an_image_renders_its_alt_text() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg", "alt": "𠮟"},
            {"tag": "span", "content": "meaning"}
        ]}]);
        assert_eq!(vec!["𠮟<span>meaning</span>".to_string()], html(&g));
    }

    #[test]
    fn an_image_falls_back_to_its_title() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg", "title": "a rare kanji"}
        ]}]);
        assert_eq!(vec!["a rare kanji".to_string()], html(&g));
    }

    #[test]
    fn an_image_alt_text_is_escaped_like_any_other_text() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "x.svg", "alt": "<b>&</b>"}
        ]}]);
        assert_eq!(vec!["&lt;b&gt;&amp;&lt;/b&gt;".to_string()], html(&g));
    }

    /// The card has no copy of the asset.
    /// A `src` can reference a broken image in every note.
    #[test]
    fn an_image_never_emits_a_tag_or_a_src() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg", "alt": "𠮟"},
            {"type": "image", "path": "y.png", "alt": "y"}
        ]}]);
        let out = html(&g).join("");
        assert!(!out.contains("<img"), "no image tag: {out}");
        assert!(!out.contains("src="), "no source attribute: {out}");
        assert!(!out.contains("gaiji/x.svg"), "no archive path: {out}");
    }

    #[test]
    fn a_top_level_image_item_mines_as_its_alt_text() {
        assert_eq!(
            vec!["y".to_string()],
            html(&json!([{"type": "image", "path": "y.png", "alt": "y"}]))
        );
    }

    #[test]
    fn an_image_with_no_alt_or_title_contributes_nothing() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg"},
            {"tag": "span", "content": "meaning"}
        ]}]);
        assert_eq!(vec!["<span>meaning</span>".to_string()], html(&g));
    }

    #[test]
    fn an_image_only_sense_with_no_alt_yields_nothing_in_html_mode() {
        assert!(html(&json!([{"type": "image", "path": "x.png"}])).is_empty());
    }

    // -- editorial roles: filters differ from popup settings --

    /// Popup density and card completeness are separate choices.
    /// The popup plain-text renderer drops both roles from the same tree.
    /// The card keeps both roles.
    #[test]
    fn an_example_the_popup_drops_still_reaches_the_card() {
        let d = doc(&gloss_with_editorial_matter());
        assert_eq!(vec!["to eat".to_string()], plain_items(&d));
        assert_eq!(
            vec![concat!(
                "<span>to eat</span>",
                "<div><span>ご飯を食べる</span></div>",
                "<ul><li>JMdict</li></ul>",
            )
            .to_string()],
            render_html(&d, Selection::Whole, RoleFilter::CARD),
        );
    }

    #[test]
    fn the_card_filter_drops_examples_when_asked_to() {
        let d = doc(&gloss_with_editorial_matter());
        let kept = render_html(&d, Selection::Whole, RoleFilter::CARD);
        assert!(kept[0].contains("ご飯を食べる"), "examples are on by default: {kept:?}");
        let without =
            render_html(&d, Selection::Whole, RoleFilter { examples: false, ..RoleFilter::CARD });
        assert_eq!(
            vec!["<span>to eat</span><ul><li>JMdict</li></ul>".to_string()],
            without,
            "the example goes and the attribution stays",
        );
    }

    #[test]
    fn attributions_hide_independently_of_examples() {
        let d = doc(&gloss_with_editorial_matter());
        let without = render_html(
            &d,
            Selection::Whole,
            RoleFilter { attributions: false, ..RoleFilter::CARD },
        );
        assert_eq!(
            vec!["<span>to eat</span><div><span>ご飯を食べる</span></div>".to_string()],
            without,
            "the attribution goes and the example stays",
        );
    }

    /// The test checks the role that the parser classified.
    #[test]
    fn a_part_of_speech_role_stays_out_of_the_card_body() {
        let d = doc(&json!([{"type": "structured-content", "content": [
            {"tag": "b", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "content": "a cat"}
        ]}]));
        assert_eq!(
            vec!["<span>a cat</span>".to_string()],
            render_html(&d, Selection::Whole, RoleFilter::CARD),
        );
        assert_eq!(
            vec!["<b>noun</b><span>a cat</span>".to_string()],
            render_html(
                &d,
                Selection::Whole,
                RoleFilter { part_of_speech: true, ..RoleFilter::CARD },
            ),
        );
    }

    // -- selected nodes --

    #[test]
    fn the_default_selection_is_the_whole_document() {
        let d = doc(&two_senses());
        assert_eq!(
            render_html(&d, Selection::Whole, RoleFilter::CARD),
            render_html(&d, Selection::default(), RoleFilter::default()),
        );
    }

    /// A selected sense reaches the card as markup with its style.
    /// The card does not include its neighbor.
    #[test]
    fn a_subtree_selection_renders_exactly_what_the_document_rendered_for_it() {
        let d = doc(&two_senses());
        let whole = render_html(&d, Selection::Whole, RoleFilter::CARD);
        let second = NodePath::ROOT.child(0).and_then(|p| p.child(1)).expect("the second sense");

        let picked = render_html(&d, Selection::Nodes(&[second]), RoleFilter::CARD);

        assert_eq!(vec![SENSE_TWO.to_string()], picked);
        assert!(
            whole[0].ends_with(SENSE_TWO),
            "the same bytes the whole document emitted: {whole:?}",
        );
        assert!(!picked[0].contains("to run"), "nothing from the first sense: {picked:?}");
        assert!(picked[0].contains("<i>"), "the sense keeps its formatting: {picked:?}");
    }

    #[test]
    fn selecting_a_top_level_item_reproduces_that_item_exactly() {
        let d = doc(&json!(["cat", {"type": "structured-content",
                                    "content": {"tag": "i", "content": "feline"}}]));
        let whole = render_html(&d, Selection::Whole, RoleFilter::CARD);
        let items: Vec<NodePath> =
            (0..2).map(|i| NodePath::ROOT.child(i).expect("an item path")).collect();
        assert_eq!(whole, render_html(&d, Selection::Nodes(&items), RoleFilter::CARD));
    }

    #[test]
    fn a_selection_renders_in_document_order_however_it_is_given() {
        let d = doc(&two_senses());
        let first = NodePath::ROOT.child(0).and_then(|p| p.child(0)).expect("the first sense");
        let second = NodePath::ROOT.child(0).and_then(|p| p.child(1)).expect("the second sense");
        let picked = render_html(&d, Selection::Nodes(&[second, first]), RoleFilter::CARD);
        assert_eq!(2, picked.len());
        assert!(picked[0].contains("to run"), "document order, not slice order: {picked:?}");
        assert!(picked[1].contains("to flow"));
    }

    /// A picker can select any node that the user clicks.
    /// A sense and a descendant inside it count as one sense, not two.
    #[test]
    fn a_selected_node_subsumes_a_selected_descendant() {
        let d = doc(&two_senses());
        let sense = NodePath::ROOT.child(0).and_then(|p| p.child(1)).expect("the second sense");
        let inside = sense.child(0).expect("a node inside it");
        let picked = render_html(&d, Selection::Nodes(&[sense, inside]), RoleFilter::CARD);
        assert_eq!(vec![SENSE_TWO.to_string()], picked);
    }

    #[test]
    fn a_repeated_path_renders_once() {
        let d = doc(&two_senses());
        let sense = NodePath::ROOT.child(0).and_then(|p| p.child(1)).expect("the second sense");
        assert_eq!(
            vec![SENSE_TWO.to_string()],
            render_html(&d, Selection::Nodes(&[sense, sense]), RoleFilter::CARD),
        );
    }

    #[test]
    fn an_empty_selection_renders_nothing() {
        let d = doc(&two_senses());
        assert!(render_html(&d, Selection::Nodes(&[]), RoleFilter::CARD).is_empty());
    }

    /// A path from another Entry addresses no node in this tree.
    /// It must not select the node at the same index.
    #[test]
    fn a_path_the_document_does_not_have_renders_nothing() {
        let d = doc(&json!(["cat"]));
        let deep = NodePath::ROOT
            .child(0)
            .and_then(|p| p.child(7))
            .and_then(|p| p.child(3))
            .expect("a well-formed path");
        assert!(render_html(&d, Selection::Nodes(&[deep]), RoleFilter::CARD).is_empty());
    }

    #[test]
    fn a_selected_subtree_is_role_filtered_the_same_way_the_document_is() {
        let d = doc(&gloss_with_editorial_matter());
        let item = NodePath::ROOT.child(0).expect("the item path");
        assert_eq!(
            vec!["<span>to eat</span><ul><li>JMdict</li></ul>".to_string()],
            render_html(
                &d,
                Selection::Nodes(&[item]),
                RoleFilter { examples: false, ..RoleFilter::CARD },
            ),
        );
    }

    // -- untrusted input --

    /// This case combines a script element, a script-scheme link, and a style value that closes
    /// its own attribute.
    #[test]
    fn a_hostile_entry_neither_scripts_nor_breaks_out_of_an_attribute() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "script", "content": "alert(1)"},
            {"tag": "a", "href": "javascript:alert(1)", "content": "click"},
            {"tag": "span", "style": {"color": "red\" onmouseover=\"alert(1)"},
             "content": "5 < 6 & 7 > 2"}
        ]}]);
        let out = html(&g);
        assert_eq!(
            vec![concat!(
                "alert(1)",
                "<a>click</a>",
                "<span style=\"color:red&quot; onmouseover=&quot;alert(1)\">",
                "5 &lt; 6 &amp; 7 &gt; 2</span>",
            )
            .to_string()],
            out,
        );
        let joined = out.join("");
        assert!(!joined.contains("<script"), "no script element: {joined}");
        assert!(!joined.contains("javascript:"), "no script scheme: {joined}");
        assert!(!joined.contains("onmouseover=\""), "no attribute break-out: {joined}");
    }

    #[test]
    fn only_followable_link_schemes_survive() {
        let cases = [
            ("https://example.org/x", true),
            ("http://example.org/x", true),
            ("?query=猫", true),
            ("#anchor", true),
            ("../other.html", true),
            ("javascript:alert(1)", false),
            ("JavaScript:alert(1)", false),
            ("  java\tscript:alert(1)", false),
            ("data:text/html;base64,PHNjcmlwdD4=", false),
            ("vbscript:msgbox(1)", false),
        ];
        for (href, kept) in cases {
            let g = json!([{"type": "structured-content", "content": [
                {"tag": "a", "href": href, "content": "link"}
            ]}]);
            let out = html(&g).join("");
            assert_eq!(
                kept,
                out.contains("href="),
                "{href:?} should {} an href: {out}",
                if kept { "keep" } else { "lose" },
            );
            assert!(out.contains(">link<"), "the text survives either way: {out}");
        }
    }

    // -- resolved style record --

    /// A CSS-only Dictionary draws its pill with box properties.
    /// `styles.css` folds into the same resolved record as an inline `style` attribute.
    /// This test checks that the full record reaches the card, not only the six properties that
    /// the old subset handled.
    #[test]
    fn a_pill_reaches_the_card_as_one_inline_style() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "style": {
                "fontSize": 0.8, "color": "white", "backgroundColor": "#8080ff",
                "borderColor": "#666", "borderStyle": "solid", "borderRadius": 0.25,
                "borderWidth": 0.0625, "marginRight": 0.25, "paddingLeft": 0.25,
                "paddingRight": 0.25
            }, "content": "n"}
        ]}]);
        assert_eq!(
            vec![concat!(
                "<span style=\"font-size:0.8em;color:white;background-color:#8080ff;",
                "border-color:#666;border-style:solid;border-radius:0.25em;",
                "border-width:0.0625em;margin-right:0.25em;padding-right:0.25em;",
                "padding-left:0.25em\">n</span>",
            )
            .to_string()],
            html(&g),
        );
    }

    /// Emission order is fixed, not source order.
    /// A shorthand never follows a longhand that erases its value.
    /// This test parses raw text because `json!` sorts keys and cannot express that order.
    #[test]
    fn a_shorthand_is_emitted_before_the_longhand_it_would_erase() {
        let d = GlossDoc::parse(concat!(
            r#"[{"type":"structured-content","content":"#,
            r#"{"tag":"div","style":{"marginTop":0.5,"margin":0},"content":"x"}}]"#,
        ));
        assert_eq!(
            vec!["<div style=\"margin:0em;margin-top:0.5em\">x</div>".to_string()],
            render_html(&d, Selection::Whole, RoleFilter::CARD),
        );
    }

    #[test]
    fn a_style_value_that_is_not_a_css_value_emits_no_declaration() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "style": {"color": null, "fontWeight": "bold"}, "content": "x"},
            {"tag": "b", "style": {"color": null}, "content": "y"}
        ]}]);
        assert_eq!(
            vec!["<span style=\"font-weight:bold\">x</span><b>y</b>".to_string()],
            html(&g),
        );
    }
}

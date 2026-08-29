//! The HTML renderer, for the Anki `glossary_html` field.
//!
//! The second of three renderers over [`GlossDoc`]. Before ticket 02 this was
//! an independent recursion over the raw JSON that mirrored the plain-text
//! walk's drop rules by hand - two views of one tree, which is the bug class
//! the spec set out to close. It now reads the same parsed tree, so the card
//! and the popup cannot disagree about what an entry *contains*.
//!
//! What they may disagree about, on purpose, is what to *show*. The popup is
//! a hover panel and the card is a permanent record, so this renderer takes
//! its own [`RoleFilter`] rather than the popup's: hiding examples on screen
//! must not strip them from a mined card (spec story 42).
//!
//! It also takes a [`Selection`], so it can render an arbitrary set of
//! subtrees and not only a whole document. That is the Anki half of stories
//! 45 and 46 - a sense picker built on it hands over formatted markup for the
//! senses a user chose, never a flattened text range.
//!
//! Structured content is untrusted input from a dictionary file rather than
//! from chibipop, so tags, attributes, and link schemes are allow-listed
//! rather than passed through: an unrecognised tag keeps its text and drops
//! the wrapper, the same way [`Kind::Unknown`](super::Kind) does in the tree
//! itself.
//!
//! This runs when a card is mined, not per hover, so it builds owned strings
//! rather than threading a buffer.

use super::{GlossDoc, ItemType, Kind, NodeId, NodePath, Role, Scalar, Selection, StyleKey, Tag};

/// Which editorial roles reach the card.
///
/// Deliberately a value the caller passes rather than a rule this module
/// knows, because the whole point is that the card's answer and the popup's
/// are separate: ticket 14 gives the popup its own settings and this stays
/// untouched by them.
///
/// Ticket 15 populates [`Role`]; today the parser writes
/// [`Role::Unclassified`] on every node, which every filter keeps. So what
/// this changes *today* is only that the HTML renderer stopped consulting the
/// name-matched drop list the plain-text renderer still uses - which is
/// exactly the independence story 42 asks for, one dictionary at a time as
/// ticket 15 starts classifying.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoleFilter {
    pub examples: bool,
    pub attributions: bool,
    pub part_of_speech: bool,
}

impl RoleFilter {
    /// What a mined card keeps: everything the entry says, except the
    /// part-of-speech labels.
    ///
    /// A card is a record rather than a panel, so density is not a reason to
    /// drop anything from it. The labels are the one exception because they
    /// are already the card's *own* field - `Card::pos`, rendered above the
    /// glosses - so leaving them inline would print them twice.
    pub const CARD: RoleFilter =
        RoleFilter { examples: true, attributions: true, part_of_speech: false };

    fn allows(self, role: Role) -> bool {
        match role {
            Role::Example => self.examples,
            Role::Attribution => self.attributions,
            Role::PartOfSpeech => self.part_of_speech,
            Role::Unclassified | Role::Content => true,
        }
    }
}

impl Default for RoleFilter {
    fn default() -> Self {
        RoleFilter::CARD
    }
}

/// Tags this renderer emits. `details`, `summary` and `tfoot` are absent
/// because Anki's note templates predate them here and a wrapper this
/// renderer drops still keeps its text; `img` is absent because `src/anki.rs`
/// has no media upload path, so an image has nothing to point at and renders
/// its `alt` text instead - see [`image_html`].
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

/// A resolved style property as its CSS name.
///
/// The whole record is emitted, not a chosen subset: ticket 17 folds a
/// dictionary's `styles.css` into this same record, and the 13 css-only
/// dictionaries in the census draw their pills with box properties alone. A
/// subset here would mine those dictionaries as unstyled text while the popup
/// drew their pills.
fn css_name(key: StyleKey) -> &'static str {
    match key {
        StyleKey::FontStyle => "font-style",
        StyleKey::FontWeight => "font-weight",
        StyleKey::FontSize => "font-size",
        // The shorthand, not `text-decoration-line`: the longhand is younger
        // than some of the renderers an Anki note template runs in.
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

/// One HTML fragment per selected node - parallel to
/// [`plain_items`](super::plain_items), which renders the whole document.
///
/// [`Selection::Whole`] gives one fragment per top-level glossary item, which
/// is what `src/anki.rs` turns into one `<li>` each.
/// [`Selection::Nodes`] gives one fragment per selected node, in document
/// order, and each is byte-for-byte the markup the whole document produced
/// for that subtree.
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
                // A step that does not fit addresses nothing, so nothing
                // under it can have been selected.
                let Some(path) = NodePath::ROOT.child(i) else { continue };
                collect(doc, id, path, wanted, roles, &mut out);
            }
        }
    }
    out
}

/// Emits every selected node at or under `id`, in document order.
///
/// A selected node ends the descent, so a picker that hands over both a sense
/// and something inside it renders the sense once rather than twice.
fn collect(
    doc: &GlossDoc,
    id: NodeId,
    path: NodePath,
    wanted: &[NodePath],
    roles: RoleFilter,
    out: &mut Vec<String>,
) {
    if wanted.contains(&path) {
        // A top-level item and a node inside one are rendered by different
        // rules - a bare glossary string is emitted unwrapped, a
        // structured-content item is its children - so the fragment for a
        // selected item has to be the item's, or selecting the whole entry
        // would not reproduce the whole entry.
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

/// Whitespace-only content is no content: an item whose every node was
/// dropped contributes no `<li>` rather than an empty one.
fn push(html: String, out: &mut Vec<String>) {
    let trimmed = html.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
}

/// One top-level glossary item to HTML.
fn item_html(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> String {
    if !keeps(doc, id, roles) {
        return String::new();
    }
    match doc.node(id).item_type {
        ItemType::Text => escape_text(doc.text(id)),
        ItemType::StructuredContent => children_html(doc, id, roles),
        _ if doc.is_plain_string(id) => escape_text(doc.text(id)),
        _ => node_html(doc, id, roles),
    }
}

/// One node to HTML.
fn node_html(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> String {
    let node = doc.node(id);
    if !keeps(doc, id, roles) {
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
                // Nothing left after drops (e.g. an image-only wrapper): no
                // point in an empty tag shell.
                return inner;
            }
            let attrs = html_attrs(doc, id, node.tag);
            format!("<{tag}{attrs}>{inner}</{tag}>")
        }
        // Not a tag we emit: keep the text, drop the wrapper.
        None => children_html(doc, id, roles),
    }
}

fn children_html(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> String {
    doc.children(id).map(|child| node_html(doc, child, roles)).collect()
}

/// Does this node's editorial role reach the card?
fn keeps(doc: &GlossDoc, id: NodeId, roles: RoleFilter) -> bool {
    if !roles.allows(doc.node(id).role) {
        return false;
    }
    // Ticket 15 replaces this with `Role::PartOfSpeech` on the node. Until it
    // classifies, the name-matched marker is the only thing here that can
    // tell a label from a gloss, and printing the labels inline would repeat
    // the card's own `pos` field.
    roles.part_of_speech || !doc.is_part_of_speech(id)
}

/// An image as text.
///
/// The tree carries image nodes the old flattener dropped, and `src/anki.rs`
/// has no `storeMediaFile` path, so an `<img>` here would name a file Anki
/// does not have. The `alt` text is the character the image stands for and
/// `title` is the next best label; with neither, the node contributes
/// nothing, which is what it did before this renderer could reach it.
fn image_html(doc: &GlossDoc, id: NodeId) -> String {
    for key in ["alt", "title"] {
        match doc.attr_of(id, key).and_then(|v| doc.scalar_str(v)) {
            Some(text) if !text.trim().is_empty() => return escape_text(text),
            _ => {}
        }
    }
    String::new()
}

/// `href` on `<a>`, `colSpan`/`rowSpan` on cells, and the resolved style
/// record on anything - a leading-space-prefixed attribute string, or empty.
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

/// The resolved style record as one CSS declaration list.
///
/// Emitted in [`StyleKey`] declaration order rather than in the record's own
/// order, so that a card's markup does not depend on which order a dictionary
/// happened to write its properties in. That order also puts each shorthand
/// ahead of its longhands, which is the one ordering CSS cares about here:
/// `margin` written after `marginTop` would silently erase it.
fn style_attr(doc: &GlossDoc, id: NodeId) -> String {
    let record = doc.style(id);
    let mut decls: Vec<(u8, String)> = Vec::with_capacity(record.len());
    for (i, (key, value)) in record.iter().enumerate() {
        // First occurrence wins, which is the answer `GlossDoc::style_of`
        // gives every other reader of the record.
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

/// A span count: a non-negative whole number, or nothing.
fn count(v: Scalar) -> Option<u64> {
    match v {
        Scalar::Num(n) if n >= 0.0 && n.fract() == 0.0 => Some(n as u64),
        _ => None,
    }
}

/// A style value as CSS, or nothing when the schema's value cannot be one.
///
/// Numbers are Yomitan's em-multiplier convention; a list-valued property
/// such as `textDecorationLine` was space-joined at parse time, so it arrives
/// as one string. A boolean, a null, or a blank string is not a declaration
/// and is dropped rather than emitted as a property with no value.
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

/// May a card follow this link?
///
/// A dictionary's own cross-references carry no scheme (`?query=見出し語`)
/// and its citations are `http` or `https`. Anything else - `javascript:`,
/// `data:`, `vbscript:` - is a script vector inside Anki's webview, arriving
/// from a file chibipop did not write, so the `<a>` keeps its text and loses
/// its `href`. Whitespace and control characters are stripped before the
/// scheme is read, because an HTML parser ignores them inside a URL and a
/// naive check would not.
fn safe_href(url: &str) -> bool {
    let cleaned: String =
        url.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    match cleaned.find([':', '/', '?', '#']) {
        // A scheme: it ends at the first `:`, and only two are followable.
        Some(at) if cleaned.as_bytes()[at] == b':' => {
            let scheme = &cleaned[..at];
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }
        // No scheme, so relative to the dictionary itself.
        _ => true,
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
    use super::super::plain_items;
    use super::super::tests::doc;
    use super::*;
    use serde_json::{json, Value};

    fn html(g: &Value) -> Vec<String> {
        render_html(&doc(g), Selection::Whole, RoleFilter::CARD)
    }

    /// Marks every node carrying `tag` with `role`.
    ///
    /// Ticket 15 owns the classifier; until it lands the parser writes
    /// [`Role::Unclassified`] on everything, so a role filter has nothing to
    /// bite on unless a fixture sets one. Writing the field directly is
    /// deliberate: the alternative is a public mutator on [`GlossDoc`] that
    /// only a test would ever call.
    fn classify(d: &mut GlossDoc, tag: Tag, role: Role) {
        for node in d.nodes.iter_mut().filter(|n| n.tag == tag) {
            node.role = role;
        }
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

    /// A Jitendex-shaped entry: a gloss, then an example the popup's drop
    /// list names, then an attribution it also names.
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

    /// Two senses as sibling blocks inside one glossary item - the shape 64
    /// of 72 census dictionaries use, and the one a sense picker selects in.
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

    // -- shape that existed before this ticket, unchanged --

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

    // -- images: alt text, never a src Anki cannot resolve --

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

    /// The card has no copy of the asset, so a `src` would be a broken image
    /// in every note this ever mines.
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

    // -- editorial roles, filtered independently of the popup --

    /// Story 42: popup density and card completeness are separate choices.
    /// The popup's plain-text renderer still drops both, from the same tree,
    /// in the same process - which is the strongest form of "independent"
    /// reachable before ticket 14 gives the popup a setting of its own.
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
        let mut d = doc(&gloss_with_editorial_matter());
        classify(&mut d, Tag::Div, Role::Example);
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
        let mut d = doc(&gloss_with_editorial_matter());
        classify(&mut d, Tag::Div, Role::Example);
        classify(&mut d, Tag::Ul, Role::Attribution);
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

    /// Ticket 15's acceptance, asserted through the role rather than through
    /// the marker name the current parser still matches on.
    #[test]
    fn a_part_of_speech_role_stays_out_of_the_card_body() {
        let mut d = doc(&json!([{"type": "structured-content", "content": [
            {"tag": "b", "content": "noun"},
            {"tag": "span", "content": "a cat"}
        ]}]));
        classify(&mut d, Tag::B, Role::PartOfSpeech);
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

    // -- rendering a selection --

    #[test]
    fn the_default_selection_is_the_whole_document() {
        let d = doc(&two_senses());
        assert_eq!(
            render_html(&d, Selection::Whole, RoleFilter::CARD),
            render_html(&d, Selection::default(), RoleFilter::default()),
        );
    }

    /// Stories 45 and 46 on the Anki side: a selected sense reaches the card
    /// as markup, with its formatting, and without its neighbour.
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

    /// A picker may hand over whatever the user clicked; a sense and
    /// something inside it is one sense, not two.
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

    /// A path from another entry's tree addresses nothing here rather than
    /// whatever node happens to sit at that index.
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
        let mut d = doc(&gloss_with_editorial_matter());
        classify(&mut d, Tag::Div, Role::Example);
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

    /// Every hostile shape at once: a script element, a script-scheme link,
    /// and a style value that tries to close its own attribute.
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

    // -- the resolved style record --

    /// A css-only dictionary draws its pill entirely with box properties.
    /// Ticket 17 folds `styles.css` into the same resolved record inline
    /// `style` writes into, so what this asserts is that the whole record
    /// reaches the card, not just the six properties the old subset knew.
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

    /// Emission order is fixed, not the order the dictionary wrote, so a
    /// shorthand can never land after the longhand it would erase. Parsed
    /// from raw text because `json!` sorts its keys and cannot express the
    /// hostile order.
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

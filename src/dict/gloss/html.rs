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

use super::{
    DocRange, GlossDoc, ItemType, Kind, NodeId, NodePath, Role, Scalar, Selection, StyleKey, Tag,
};
use super::select::SelectionPlan;

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
        StyleKey::Display => "display",
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
        // An image node emits its `alt` text alone, so these keys never reach
        // the markup. The fold reads these spellings as the properties.
        StyleKey::ImageWidth => "width",
        StyleKey::ImageMaxWidth | StyleKey::ImageLinkMaxWidth => "max-width",
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
///
/// A range separator applies only inside the nearest kept block tag, list item,
/// table cell, or top-level item. Container boundaries never receive a separator.
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
        Selection::Ranges { ranges, separator } => {
            render_ranges(doc, ranges, separator, roles, &mut out);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Position {
    leaf: usize,
    byte: u32,
}

#[derive(Clone)]
struct Part {
    html: String,
    start: Position,
    end: Position,
}

struct Rendered {
    html: String,
    first: Option<Position>,
    last: Option<Position>,
    parts: Vec<Part>,
    boundary: bool,
}


/// Shared inputs for one selected HTML tree walk.
///
/// This context keeps recursive calls focused on node-specific state.
#[derive(Clone, Copy)]
struct RangeWalk<'a> {
    doc: &'a GlossDoc,
    plan: &'a SelectionPlan,
    roles: RoleFilter,
    separator: super::Separator,
}
fn render_ranges(
    doc: &GlossDoc,
    ranges: &[DocRange],
    separator: super::Separator,
    roles: RoleFilter,
    out: &mut Vec<String>,
) {
    let Some(plan) = SelectionPlan::new(doc, ranges, roles) else { return };
    if plan.covers_all() {
        for id in doc.items() {
            push(item_html(doc, id, roles), out);
        }
        return;
    }
    let walk = RangeWalk { doc, plan: &plan, roles, separator };
    for (index, id) in doc.items().enumerate() {
        let Some(path) = NodePath::ROOT.child(index) else { continue };
        let rendered = render_range_item(&walk, id, path);
        push(rendered.html, out);
    }
}
fn render_range_item(walk: &RangeWalk<'_>, id: NodeId, path: NodePath) -> Rendered {
    if walk.doc.node(id).item_type == ItemType::StructuredContent {
        return render_range_children(walk, id, path, true);
    }
    render_range_node(walk, id, path, path, true)
}

fn render_range_node(
    walk: &RangeWalk<'_>,
    id: NodeId,
    path: NodePath,
    active_container: NodePath,
    top_level: bool,
) -> Rendered {
    let doc = walk.doc;
    let plan = walk.plan;
    let roles = walk.roles;
    let separator = walk.separator;
    let node = doc.node(id);
    if matches!(node.tag, Tag::Rt | Tag::Rp)
        || (node.kind != Kind::Text
            && node.tag != Tag::Ruby
            && !plan.has_selected_child(doc, id, path))
    {
        return Rendered::empty();
    }
    if node.tag == Tag::Ruby {
        if !plan.intervals.contains_key(&path) || !roles.allows(node.role) {
            return Rendered::empty();
        }
        let html = node_html(doc, id, roles);
        if html.trim().is_empty() {
            return Rendered::empty();
        }
        let index = plan.indexes[&path];
        let part = Part {
            html: html.clone(),
            start: Position { leaf: index, byte: 0 },
            end: Position { leaf: index, byte: 1 },
        };
        return Rendered {
            html,
            first: Some(part.start),
            last: Some(part.end),
            parts: vec![part],
            boundary: false,
        };
    }
    if node.kind == Kind::Text {
        let Some(covered) = plan.intervals.get(&path) else { return Rendered::empty() };
        if !roles.allows(node.role) {
            return Rendered::empty();
        }
        let text = doc.text(id);
        let index = plan.indexes[&path];
        let parts: Vec<Part> = covered
            .iter()
            .filter_map(|&(start, end)| {
                let slice = text.get(start as usize..end as usize)?;
                (!slice.is_empty()).then(|| Part {
                    html: escape_text(slice),
                    start: Position { leaf: index, byte: start },
                    end: Position { leaf: index, byte: end },
                })
            })
            .collect();
        let html = join_parts(&parts, separator, plan);
        return Rendered {
            html,
            first: parts.first().map(|part| part.start),
            last: parts.last().map(|part| part.end),
            parts,
            boundary: false,
        };
    }
    if node.kind == Kind::Image || node.tag == Tag::Br {
        return Rendered::empty();
    }
    let own_container = top_level || doc.is_block(id) || matches!(node.tag, Tag::Td | Tag::Th);
    let child_container = if own_container { path } else { active_container };
    let content = render_range_children(walk, id, child_container, own_container);
    if content.html.is_empty() {
        return Rendered::empty();
    }
    let parts = if own_container {
        vec![Part {
            html: match html_tag(node.tag) {
                Some(tag) => format!("<{tag}{}>{}</{tag}>", html_attrs(doc, id, node.tag), content.html),
                None => content.html.clone(),
            },
            start: content.first.expect("selected content has a first leaf"),
            end: content.last.expect("selected content has a last leaf"),
        }]
    } else {
        content
            .parts
            .into_iter()
            .map(|part| Part { html: wrap_part(doc, id, node.tag, &part.html), ..part })
            .collect()
    };
    if parts.is_empty() {
        return Rendered::empty();
    }
    let html = if own_container {
        parts[0].html.clone()
    } else {
        join_parts(&parts, separator, plan)
    };
    if html.trim().is_empty() {
        return Rendered::empty();
    }
    let boundary = own_container;
    Rendered {
        html,
        first: content.first,
        last: content.last,
        parts,
        boundary,
    }
}
fn wrap_part(doc: &GlossDoc, id: NodeId, tag: Tag, inner: &str) -> String {
    match html_tag(tag) {
        Some("br") => "<br>".to_string(),
        Some(tag_name) => format!("<{tag_name}{}>{inner}</{tag_name}>", html_attrs(doc, id, tag)),
        None => inner.to_string(),
    }
}


fn render_range_children(
    walk: &RangeWalk<'_>,
    id: NodeId,
    active_container: NodePath,
    container: bool,
) -> Rendered {
    let doc = walk.doc;
    let plan = walk.plan;
    let separator = walk.separator;
    let mut children = Vec::new();
    for child in doc.children(id) {
        let Some(path) = NodePath::of(doc, child) else { continue };
        let rendered = render_range_node(walk, child, path, active_container, false);
        if !rendered.html.is_empty() {
            children.push(rendered);
        }
    }
    if children.is_empty() {
        return Rendered::empty();
    }
    if container && separator == super::Separator::ListItems {
        let mut html = String::new();
        let mut pending = Vec::new();
        let mut parts = Vec::new();
        for child in &children {
            if child.boundary {
                if !pending.is_empty() {
                    html.push_str(&list_html(&pending));
                    pending.clear();
                }
                html.push_str(&child.html);
                if let (Some(start), Some(end)) = (child.first, child.last) {
                    parts.push(Part { html: child.html.clone(), start, end });
                }
            } else if child.parts.is_empty() {
                if let (Some(start), Some(end)) = (child.first, child.last) {
                    pending.push(Part { html: child.html.clone(), start, end });
                }
            } else {
                pending.extend(child.parts.iter().cloned());
            }
        }
        if !pending.is_empty() {
            html.push_str(&list_html(&pending));
            parts.extend(pending);
        }
        let first = children.iter().find_map(|child| child.first);
        let last = children.iter().rev().find_map(|child| child.last);
        return Rendered { html, first, last, parts, boundary: false };
    }
    let mut joined = String::new();
    for (index, child) in children.iter().enumerate() {
        if index != 0 {
            let previous = &children[index - 1];
            if !previous.boundary
                && !child.boundary
                && has_gap(previous.last, child.first, plan)
            {
                joined.push_str(separator_text(separator));
            }
        }
        joined.push_str(&child.html);
    }
    let first = children.first().and_then(|child| child.first);
    let last = children.last().and_then(|child| child.last);
    let mut parts = Vec::new();
    for child in &children {
        if child.parts.is_empty() {
            if let (Some(start), Some(end)) = (child.first, child.last) {
                parts.push(Part { html: child.html.clone(), start, end });
            }
        } else {
            parts.extend(child.parts.iter().cloned());
        }
    }
    Rendered { html: joined, first, last, parts, boundary: false }
}
fn list_html(parts: &[Part]) -> String {
    format!(
        "<ul>{}</ul>",
        parts.iter().map(|part| format!("<li>{}</li>", part.html)).collect::<String>()
    )
}


fn separator_text(separator: super::Separator) -> &'static str {
    match separator {
        super::Separator::Ellipsis => "…",
        super::Separator::Space => " ",
        super::Separator::LineBreak => "<br>",
        super::Separator::ListItems => "",
    }
}

fn join_parts(parts: &[Part], separator: super::Separator, plan: &SelectionPlan) -> String {
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index != 0 && has_gap(Some(parts[index - 1].end), Some(part.start), plan) {
            out.push_str(separator_text(separator));
        }
        out.push_str(&part.html);
    }
    out
}

fn has_gap(previous: Option<Position>, current: Option<Position>, plan: &SelectionPlan) -> bool {
    let (Some(previous), Some(current)) = (previous, current) else { return false };
    if current.leaf == previous.leaf {
        return previous.byte < current.byte;
    }
    if current.leaf == previous.leaf + 1 {
        return previous.byte < plan.leaves[previous.leaf].len || current.byte > 0;
    }
    true
}

impl Rendered {
    fn empty() -> Self {
        Rendered { html: String::new(), first: None, last: None, parts: Vec::new(), boundary: false }
    }
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
    
    
    use super::super::tests::doc;
    
    use super::*;
    use serde_json::{json, Value};

    fn html(g: &Value) -> Vec<String> {
        render_html(&doc(g), Selection::Whole, RoleFilter::CARD)
    }

    // -- cases from before the arena rewrite --

    // -- image output: alt text, no Anki src --

    // -- editorial roles: filters differ from popup settings --

    // -- selected nodes --

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

    // -- resolved style record --

}

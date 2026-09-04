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
    leaves, DocAddr, DocRange, GlossDoc, ItemType, Kind, Leaf, NodeId, NodePath, Role,
    Scalar, Selection, StyleKey, Tag,
};
use std::collections::HashMap;

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

struct RangePlan {
    leaves: Vec<Leaf>,
    intervals: HashMap<NodePath, Vec<(u32, u32)>>,
    indexes: HashMap<NodePath, usize>,
}

/// Shared inputs for one selected HTML tree walk.
///
/// This context keeps recursive calls focused on node-specific state.
#[derive(Clone, Copy)]
struct RangeWalk<'a> {
    doc: &'a GlossDoc,
    plan: &'a RangePlan,
    roles: RoleFilter,
    separator: super::Separator,
}


fn atomic_endpoint(doc: &GlossDoc, addr: DocAddr, end: bool) -> DocAddr {
    let mut prefix = NodePath::ROOT;
    for &step in addr.path.steps() {
        let Some(next) = prefix.child(step as usize) else { break };
        prefix = next;
        if prefix.resolve(doc).is_some_and(|id| doc.node(id).tag == Tag::Ruby) {
            return DocAddr { path: prefix, byte: end as u32 };
        }
    }
    addr
}

fn render_ranges(
    doc: &GlossDoc,
    ranges: &[DocRange],
    separator: super::Separator,
    roles: RoleFilter,
    out: &mut Vec<String>,
) {
    let mut union: Vec<DocRange> = ranges
        .iter()
        .copied()
        .map(|range| DocRange {
            start: atomic_endpoint(doc, range.start, false),
            end: atomic_endpoint(doc, range.end, true),
        })
        .collect();
    union.retain(|range| range.start < range.end);
    union.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<DocRange> = Vec::with_capacity(union.len());
    for range in union {
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                if range.end > last.end {
                    last.end = range.end;
                }
                continue;
            }
        }
        merged.push(range);
    }
    if merged.is_empty() {
        return;
    }

    let visible = leaves(doc, roles);
    if visible.is_empty() {
        return;
    }
    let mut intervals = HashMap::new();
    let mut indexes = HashMap::new();
    for (index, leaf) in visible.iter().copied().enumerate() {
        indexes.insert(leaf.path, index);
        let leaf_start = DocAddr { path: leaf.path, byte: 0 };
        let leaf_end = DocAddr { path: leaf.path, byte: leaf.len };
        let mut covered: Vec<(u32, u32)> = Vec::new();
        for range in &merged {
            if range.end <= leaf_start || range.start >= leaf_end {
                continue;
            }
            let start = if range.start.path < leaf.path {
                0
            } else {
                range.start.byte.min(leaf.len)
            };
            let end = if range.end.path > leaf.path {
                leaf.len
            } else {
                range.end.byte.min(leaf.len)
            };
            if start < end {
                if let Some(previous) = covered.last_mut() {
                    if start <= previous.1 {
                        previous.1 = previous.1.max(end);
                        continue;
                    }
                }
                covered.push((start, end));
            }
        }
        if !covered.is_empty() {
            intervals.insert(leaf.path, covered);
        }
    }
    if intervals.is_empty() {
        return;
    }
    if visible.iter().all(|leaf| {
        intervals
            .get(&leaf.path)
            .is_some_and(|covered| covered.len() == 1 && covered[0] == (0, leaf.len))
    }) {
        for id in doc.items() {
            push(item_html(doc, id, roles), out);
        }
        return;
    }
    let plan = RangePlan { leaves: visible, intervals, indexes };
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
            && !has_selected_child(doc, id, path, plan))
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

fn has_selected_child(doc: &GlossDoc, id: NodeId, path: NodePath, plan: &RangePlan) -> bool {
    if plan.intervals.contains_key(&path) {
        return true;
    }
    doc.children(id).enumerate().any(|(index, child)| {
        path.child(index)
            .is_some_and(|child_path| has_selected_child(doc, child, child_path, plan))
    })
}

fn separator_text(separator: super::Separator) -> &'static str {
    match separator {
        super::Separator::Ellipsis => "…",
        super::Separator::Space => " ",
        super::Separator::LineBreak => "<br>",
        super::Separator::ListItems => "",
    }
}

fn join_parts(parts: &[Part], separator: super::Separator, plan: &RangePlan) -> String {
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index != 0 && has_gap(Some(parts[index - 1].end), Some(part.start), plan) {
            out.push_str(separator_text(separator));
        }
        out.push_str(&part.html);
    }
    out
}

fn has_gap(previous: Option<Position>, current: Option<Position>, plan: &RangePlan) -> bool {
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
    use super::super::plain_items;
    use super::super::tests::doc;
    use super::super::{extent, Separator};
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

    fn path(steps: &[usize]) -> NodePath {
        steps
            .iter()
            .try_fold(NodePath::ROOT, |path, &step| path.child(step))
            .expect("path fits")
    }

    fn range(path: NodePath, start: u32, end: u32) -> DocRange {
        DocRange {
            start: DocAddr { path, byte: start },
            end: DocAddr { path, byte: end },
        }
    }

    #[test]
    fn a_partial_link_keeps_its_href_wrapper() {
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "a", "href": "?query=見出し語", "content": "見出し語"
        }}]));
        let selected = range(path(&[0, 0, 0]), 6, 12);
        assert_eq!(
            vec!["<a href=\"?query=見出し語\">し語</a>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &[selected], separator: Separator::Ellipsis }, RoleFilter::CARD)
        );
    }

    #[test]
    fn a_partial_text_keeps_its_bold_ancestor() {
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "b", "content": "bold text"
        }}]));
        let selected = range(path(&[0, 0, 0]), 5, 9);
        assert_eq!(
            vec!["<b>text</b>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &[selected], separator: Separator::Ellipsis }, RoleFilter::CARD)
        );
    }

    #[test]
    fn a_range_inside_a_ruby_base_keeps_the_atomic_ruby_and_reading() {
        let d = doc(&json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "content": "ねこ"}
            ]},
            {"tag": "span", "content": "tail"}
        ]}]));
        let start = DocAddr { path: path(&[0, 0, 0, 0]), byte: 1 };
        let end_path = path(&[0, 1, 0]);
        let selected = DocRange { start, end: DocAddr { path: end_path, byte: 4 } };
        assert_eq!(
            vec!["<ruby><span>猫</span><rt>ねこ</rt></ruby><span>tail</span>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &[selected], separator: Separator::Ellipsis }, RoleFilter::CARD)
        );
    }

    #[test]
    fn each_separator_applies_between_fragments_in_one_container() {
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "div", "content": "abcdef"
        }}]));
        let ranges = [range(path(&[0, 0, 0]), 1, 2), range(path(&[0, 0, 0]), 4, 5)];
        assert_eq!(
            vec!["<div>b…e</div>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &ranges, separator: Separator::Ellipsis }, RoleFilter::CARD)
        );
        assert_eq!(
            vec!["<div>b e</div>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &ranges, separator: Separator::Space }, RoleFilter::CARD)
        );
        assert_eq!(
            vec!["<div>b<br>e</div>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &ranges, separator: Separator::LineBreak }, RoleFilter::CARD)
        );
        assert_eq!(
            vec!["<div><ul><li>b</li><li>e</li></ul></div>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &ranges, separator: Separator::ListItems }, RoleFilter::CARD)
        );
    }

    #[test]
    fn separators_do_not_cross_list_item_containers() {
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "ol", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"}
            ]
        }}]));
        let ranges = [range(path(&[0, 0, 0, 0]), 0, 3), range(path(&[0, 0, 1, 0]), 0, 3)];
        assert_eq!(
            vec!["<ol><li>fir</li><li>sec</li></ol>".to_string()],
            render_html(&d, Selection::Ranges { ranges: &ranges, separator: Separator::Ellipsis }, RoleFilter::CARD)
        );
    }

    #[test]
    fn a_full_extent_range_matches_whole_for_plain_and_ol_shapes() {
        let plain = doc(&json!(["first", "second"]));
        let plain_extent = extent(&plain, RoleFilter::CARD).expect("plain extent");
        assert_eq!(
            render_html(&plain, Selection::Whole, RoleFilter::CARD),
            render_html(
                &plain,
                Selection::Ranges { ranges: &[plain_extent], separator: Separator::Ellipsis },
                RoleFilter::CARD,
            )
        );

        let ol = doc(&json!([{"type": "structured-content", "content": {
            "tag": "ol", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"}
            ]
        }}]));
        let ol_extent = extent(&ol, RoleFilter::CARD).expect("ol extent");
        assert_eq!(
            render_html(&ol, Selection::Whole, RoleFilter::CARD),
            render_html(
                &ol,
                Selection::Ranges { ranges: &[ol_extent], separator: Separator::Space },
                RoleFilter::CARD,
            )
        );
    }

    #[test]
    fn an_empty_range_union_renders_nothing() {
        let d = doc(&json!(["text"]));
        assert!(render_html(
            &d,
            Selection::Ranges { ranges: &[], separator: Separator::Ellipsis },
            RoleFilter::CARD,
        )
        .is_empty());
    }
}

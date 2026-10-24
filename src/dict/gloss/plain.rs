//! The plain-text renderer reads a [`GlossDoc`] document-tree.
//!
//! [`super::html`] renders Anki HTML, and `ui::layout` builds a `PopupScene`.
//! Before the arena rewrite, the plain-text walk and HTML walk parsed raw JSON
//! separately. Their results could differ. Both renderers now read one parsed
//! tree.
//!
//! Two consumers need a plain string. A collapsed-row summary needs one line.
//! The Anki plain-text `glossary` field needs all summary lines.
//!
//! Both outputs are summaries, so this renderer applies
//! [`RoleFilter::SUMMARY`]. All [`GlossDoc`] renderers use this role policy.
//! At Dictionary build time, [`renders_text`] decides whether a term row
//! becomes an `entry` row. A popup setting must not change the number of
//! Dictionary rows. The fixed policy is not a parameter.
//!
//! This renderer preserves the original line-break rules. A block tag emits a
//! mark. [`tidy`] changes each mark to a line break and removes empty parts.
//! Inline children cannot add a line break because the schema forbids one
//! there. The popup inline pass uses the same rule. A marked inline node beside
//! plain sentence text stays on its line. [`GlossDoc::prose`] identifies this
//! case.

use super::{
    DocRange, GlossDoc, ItemType, Kind, NodeId, NodePath, Role, RoleFilter, Separator, Tag,
};
use super::select::SelectionPlan;

/// A line-break mark that waits in the render buffer for [`tidy`].
///
/// The renderer uses a private-use control sequence instead of `\n`. This
/// separates a Dictionary's `\n` from a break that the renderer adds.
const BLOCK_MARK: &str = "\u{0}LI\u{0}";

/// Writes a ruby base and its readings to a plain-text buffer.
///
/// `<ruby>猫<rt>ねこ</rt></ruby>` becomes `猫(ねこ)`. The largest bilingual
/// Dictionary in the census already uses this form. JMdict writes readings as
/// parenthetical text and does not emit `ruby`. Both forms therefore produce
/// the same Anki card text.
///
/// The renderer adds parentheses only when the source does not supply them.
/// `<rp>` supplies parentheses for a renderer that cannot draw ruby. This
/// renderer keeps those parentheses and does not print `猫((ねこ))`. It also
/// keeps brackets that a Dictionary writes around the reading.
///
/// The renderer uses one slot for each `rt` in document order. Therefore,
/// `<ruby>漢<rt>かん</rt>字<rt>じ</rt></ruby>` becomes `漢(かん)字(じ)`.
/// It does not become `漢字(かんじ)`.
fn ruby_into(doc: &GlossDoc, id: NodeId, out: &mut String) {
    let fenced = doc.children(id).any(|c| doc.node(c).tag == Tag::Rp);
    let mut prose = doc.prose(id);
    for child in doc.children(id) {
        if doc.node(child).tag != Tag::Rt {
            node_into(doc, child, prose, out);
            prose = prose || doc.inline_prose(child);
            continue;
        }
        let mut reading = String::new();
        children_into(doc, child, false, &mut reading);
        let reading = reading.trim();
        if reading.is_empty() {
            continue;
        }
        if fenced || bracketed(reading) {
            out.push_str(reading);
            continue;
        }
        out.push('(');
        out.push_str(reading);
        out.push(')');
    }
}

/// Tests whether a reading already has brackets.
///
/// This function accepts standard and full-width pairs. A Japanese Dictionary
/// can write the full-width pair.
fn bracketed(reading: &str) -> bool {
    let open = reading.starts_with('(') || reading.starts_with('（');
    let close = reading.ends_with(')') || reading.ends_with('）');
    open && close
}

/// Renders one plain-text string for each top-level glossary item.
pub fn plain_items(doc: &GlossDoc) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for id in doc.items() {
        buf.clear();
        item_into(doc, id, &mut buf);
        let text = tidy(&buf);
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}
/// Renders selected ranges into one plain string for each top-level item.
///
/// The walk keeps the line rules of [`plain_items`]. The caller supplies the
/// role filter because an Anki card and a popup can keep different roles.
pub fn plain_selected(
    doc: &GlossDoc,
    ranges: &[DocRange],
    roles: RoleFilter,
    separator: Separator,
) -> Vec<String> {
    let Some(plan) = SelectionPlan::new(doc, ranges, roles) else { return Vec::new() };
    let mut out = Vec::new();
    for (index, id) in doc.items().enumerate() {
        let Some(path) = NodePath::ROOT.child(index) else { continue };
        let selected = selected_item(doc, id, path, &plan, roles, separator);
        let text = tidy(&selected.text);
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PlainPosition {
    leaf: usize,
    byte: u32,
}

#[derive(Clone)]
struct PlainFragment {
    text: String,
    start: PlainPosition,
    end: PlainPosition,
}

struct PlainRendered {
    text: String,
    first: Option<PlainPosition>,
    last: Option<PlainPosition>,
    fragments: Vec<PlainFragment>,
    boundary: bool,
}
impl PlainRendered {
    fn empty() -> Self {
        PlainRendered {
            text: String::new(),
            first: None,
            last: None,
            fragments: Vec::new(),
            boundary: false,
        }
    }
}



/// Shared inputs for one selected plain-text tree walk.
///
/// This context keeps recursive calls focused on node-specific state.
#[derive(Clone, Copy)]
struct PlainWalk<'a> {
    doc: &'a GlossDoc,
    plan: &'a SelectionPlan,
    roles: RoleFilter,
    separator: Separator,
}
fn selected_item(
    doc: &GlossDoc,
    id: NodeId,
    path: NodePath,
    plan: &SelectionPlan,
    roles: RoleFilter,
    separator: Separator,
) -> PlainRendered {
    if doc.node(id).item_type == ItemType::Image {
        return PlainRendered::empty();
    }
    let walk = PlainWalk { doc, plan, roles, separator };
    if doc.node(id).item_type == ItemType::StructuredContent {
        return selected_children(&walk, id, path, true, path);
    }
    selected_node(&walk, id, path, path, true, false, None)
}

fn selected_node(
    walk: &PlainWalk<'_>,
    id: NodeId,
    path: NodePath,
    item_path: NodePath,
    top_level: bool,
    prose: bool,
    number: Option<usize>,
) -> PlainRendered {
    let doc = walk.doc;
    let plan = walk.plan;
    let roles = walk.roles;
    let separator = walk.separator;
    let node = doc.node(id);
    if !roles.allows(node.role) || matches!(node.tag, Tag::Rt | Tag::Rp) {
        return PlainRendered::empty();
    }
    if node.tag == Tag::Ruby {
        let Some(index) = plan.indexes.get(&path).copied() else { return PlainRendered::empty() };
        if !plan.intervals.contains_key(&path) {
            return PlainRendered::empty();
        }
        let mut text = String::new();
        ruby_into_roles(doc, id, roles, &mut text);
        if text.trim().is_empty() {
            return PlainRendered::empty();
        }
        let fragment = PlainFragment {
            text: text.clone(),
            start: PlainPosition { leaf: index, byte: 0 },
            end: PlainPosition { leaf: index, byte: 1 },
        };
        return PlainRendered {
            text,
            first: Some(fragment.start),
            last: Some(fragment.end),
            fragments: vec![fragment],
            boundary: false,
        };
    }
    if node.kind == Kind::Text {
        let Some(covered) = plan.intervals.get(&path) else { return PlainRendered::empty() };
        let Some(index) = plan.indexes.get(&path).copied() else { return PlainRendered::empty() };
        let source = doc.text(id);
        let fragments: Vec<PlainFragment> = covered
            .iter()
            .filter_map(|&(start, end)| {
                let text = source.get(start as usize..end as usize)?;
                (!text.is_empty()).then(|| PlainFragment {
                    text: text.to_string(),
                    start: PlainPosition { leaf: index, byte: start },
                    end: PlainPosition { leaf: index, byte: end },
                })
            })
            .collect();
        let text = if top_level && separator == Separator::ListItems {
            list_text(&fragments)
        } else {
            join_plain_fragments(&fragments, separator, plan)
        };
        return PlainRendered {
            text,
            first: fragments.first().map(|fragment| fragment.start),
            last: fragments.last().map(|fragment| fragment.end),
            fragments,
            boundary: false,
        };
    }
    if node.kind == Kind::Image {
        return PlainRendered::empty();
    }
    if node.tag == Tag::Br {
        if break_between(path, item_path, plan) {
            return PlainRendered {
                text: "\n".to_string(),
                first: None,
                last: None,
                fragments: Vec::new(),
                boundary: false,
            };
        }
        return PlainRendered::empty();
    }
    if !plan.has_selected_child(doc, id, path) {
        return PlainRendered::empty();
    }
    let own_container = top_level || doc.is_block(id) || matches!(node.tag, Tag::Td | Tag::Th);
    let children = selected_children(walk, id, path, own_container, item_path);
    if children.text.is_empty() {
        return PlainRendered::empty();
    }
    let mut text = String::new();
    if doc.is_block(id) || (doc.has_marker(id) && !prose) {
        text.push_str(BLOCK_MARK);
    }
    if let Some(number) = number {
        text.push_str(&format!("{number}. "));
    }
    text.push_str(&children.text);
    let fragments = if own_container {
        vec![PlainFragment {
            text: text.clone(),
            start: children.first.expect("selected content has a first leaf"),
            end: children.last.expect("selected content has a last leaf"),
        }]
    } else {
        children.fragments
    };
    PlainRendered {
        text,
        first: children.first,
        last: children.last,
        fragments,
        boundary: own_container,
    }
}

fn selected_children(
    walk: &PlainWalk<'_>,
    id: NodeId,
    path: NodePath,
    container: bool,
    item_path: NodePath,
) -> PlainRendered {
    let doc = walk.doc;
    let plan = walk.plan;
    let separator = walk.separator;
    let mut children = Vec::new();
    let mut prose = doc.prose(id);
    for (index, child) in doc.children(id).enumerate() {
        let Some(child_path) = path.child(index) else { continue };
        let number = (doc.node(id).tag == Tag::Ol).then_some(
            children.iter().filter(|child: &&PlainRendered| child.boundary).count() + 1,
        );
        let selected = selected_node(walk, child, child_path, item_path, false, prose, number);
        prose = prose || doc.inline_prose(child);
        if !selected.text.is_empty() {
            children.push(selected);
        }
    }
    if children.is_empty() {
        return PlainRendered::empty();
    }
    if container && doc.is_string_list(id) {
        let mut text = String::new();
        for (index, child) in children.iter().enumerate() {
            if index != 0 {
                text.push_str(BLOCK_MARK);
            }
            if separator == Separator::ListItems {
                text.push_str("- ");
            }
            text.push_str(&child.text);
        }
        return PlainRendered {
            text,
            first: children.first().and_then(|child| child.first),
            last: children.last().and_then(|child| child.last),
            fragments: children
                .iter()
                .filter_map(|child| {
                    Some(PlainFragment {
                        text: child.text.clone(),
                        start: child.first?,
                        end: child.last?,
                    })
                })
                .collect(),
            boundary: false,
        };
    }
    if container && separator == Separator::ListItems {
        let mut text = String::new();
        let mut pending = Vec::new();
        let mut fragments = Vec::new();
        for child in &children {
            if child.boundary {
                if !pending.is_empty() {
                    text.push_str(&list_text(&pending));
                    fragments.append(&mut pending);
                }
                text.push_str(&child.text);
                if let (Some(start), Some(end)) = (child.first, child.last) {
                    fragments.push(PlainFragment { text: child.text.clone(), start, end });
                }
            } else if child.fragments.is_empty() {
                if let (Some(start), Some(end)) = (child.first, child.last) {
                    pending.push(PlainFragment { text: child.text.clone(), start, end });
                }
            } else {
                pending.extend(child.fragments.iter().cloned());
            }
        }
        if !pending.is_empty() {
            text.push_str(&list_text(&pending));
            fragments.append(&mut pending);
        }
        let first = fragments.first().map(|fragment| fragment.start);
        let last = fragments.last().map(|fragment| fragment.end);
        return PlainRendered { text, first, last, fragments, boundary: false };
    }
    let mut text = String::new();
    for (index, child) in children.iter().enumerate() {
        if index != 0 {
            let previous = &children[index - 1];
            if !previous.boundary
                && !child.boundary
                && has_plain_gap(previous.last, child.first, plan)
            {
                text.push_str(plain_separator(separator));
            }
        }
        text.push_str(&child.text);
    }
    let first = children.first().and_then(|child| child.first);
    let last = children.last().and_then(|child| child.last);
    let fragments = children
        .iter()
        .filter_map(|child| {
            Some(PlainFragment {
                text: child.text.clone(),
                start: child.first?,
                end: child.last?,
            })
        })
        .collect();
    PlainRendered { text, first, last, fragments, boundary: false }
}

fn list_text(fragments: &[PlainFragment]) -> String {
    let mut text = String::new();
    for (index, fragment) in fragments.iter().enumerate() {
        if index != 0 {
            text.push_str(BLOCK_MARK);
        }
        text.push_str("- ");
        text.push_str(&fragment.text);
    }
    text
}

fn join_plain_fragments(
    fragments: &[PlainFragment],
    separator: Separator,
    plan: &SelectionPlan,
) -> String {
    let mut text = String::new();
    for (index, fragment) in fragments.iter().enumerate() {
        if index != 0 && has_plain_gap(Some(fragments[index - 1].end), Some(fragment.start), plan) {
            text.push_str(plain_separator(separator));
        }
        text.push_str(&fragment.text);
    }
    text
}

fn plain_separator(separator: Separator) -> &'static str {
    match separator {
        Separator::Ellipsis => "…",
        Separator::Space => " ",
        Separator::LineBreak => "\n",
        Separator::ListItems => "",
    }
}

fn has_plain_gap(
    previous: Option<PlainPosition>,
    current: Option<PlainPosition>,
    plan: &SelectionPlan,
) -> bool {
    let (Some(previous), Some(current)) = (previous, current) else { return false };
    if current.leaf == previous.leaf {
        return previous.byte < current.byte;
    }
    if current.leaf == previous.leaf + 1 {
        return previous.byte < plan.leaves[previous.leaf].len || current.byte > 0;
    }
    true
}


fn break_between(path: NodePath, item_path: NodePath, plan: &SelectionPlan) -> bool {
    let before = plan.intervals.keys().any(|leaf| is_prefix(item_path, *leaf) && *leaf < path);
    let after = plan.intervals.keys().any(|leaf| is_prefix(item_path, *leaf) && *leaf > path);
    before && after
}

fn is_prefix(prefix: NodePath, path: NodePath) -> bool {
    prefix.len() <= path.len() && prefix.steps() == &path.steps()[..prefix.len()]
}

fn ruby_into_roles(doc: &GlossDoc, id: NodeId, roles: RoleFilter, out: &mut String) {
    let fenced = doc.children(id).any(|child| doc.node(child).tag == Tag::Rp);
    let mut prose = doc.prose(id);
    for child in doc.children(id) {
        if doc.node(child).tag != Tag::Rt {
            full_node_into(doc, child, prose, roles, out);
            prose = prose || doc.inline_prose(child);
            continue;
        }
        let mut reading = String::new();
        full_children_into(doc, child, false, roles, &mut reading);
        let reading = reading.trim();
        if reading.is_empty() {
            continue;
        }
        if fenced || bracketed(reading) {
            out.push_str(reading);
        } else {
            out.push('(');
            out.push_str(reading);
            out.push(')');
        }
    }
}

fn full_node_into(
    doc: &GlossDoc,
    id: NodeId,
    prose: bool,
    roles: RoleFilter,
    out: &mut String,
) {
    let node = doc.node(id);
    if !roles.allows(node.role) || node.kind == Kind::Image {
        return;
    }
    if node.tag == Tag::Ruby {
        ruby_into_roles(doc, id, roles, out);
        return;
    }
    if node.tag == Tag::Br {
        out.push('\n');
        return;
    }
    if node.kind == Kind::Text && node.tag == Tag::None {
        out.push_str(doc.text(id));
        return;
    }
    if doc.is_block(id) || (doc.has_marker(id) && !prose) {
        out.push_str(BLOCK_MARK);
    }
    full_children_into(doc, id, !doc.is_inline(id), roles, out);
}

fn full_children_into(
    doc: &GlossDoc,
    id: NodeId,
    block_ctx: bool,
    roles: RoleFilter,
    out: &mut String,
) {
    if block_ctx && doc.is_string_list(id) {
        for child in doc.children(id) {
            if !roles.allows(doc.role(child)) {
                continue;
            }
            out.push_str(BLOCK_MARK);
            out.push_str(doc.text(child));
        }
        return;
    }
    let mut prose = doc.prose(id);
    for child in doc.children(id) {
        full_node_into(doc, child, prose, roles, out);
        prose = prose || doc.inline_prose(child);
    }
}

/// Tests whether any glossary item produces plain-text output.
///
/// The Dictionary builder calls this function once for each term-bank row. A
/// glossary with no plain-text output is not an Entry and gets no `entry`
/// record. Examples include an image-only gaiji row and a wrapper that contains
/// only spaces.
///
/// This function uses the [`plain_items`] walk but does not allocate output
/// strings. This saves allocations across more than two million term rows.
///
/// The result equals `!plain_items(doc).is_empty()`. [`tidy`] keeps a part only
/// when that part remains after `str::trim`. This function tests the same parts.
pub fn renders_text(doc: &GlossDoc) -> bool {
    let mut buf = String::new();
    for id in doc.items() {
        buf.clear();
        item_into(doc, id, &mut buf);
        if buf.split(BLOCK_MARK).any(|part| !part.trim().is_empty()) {
            return true;
        }
    }
    false
}

/// Renders one top-level glossary item into the buffer.
fn item_into(doc: &GlossDoc, id: NodeId, buf: &mut String) {
    match doc.node(id).item_type {
        // An image-only item has no plain-text output.
        ItemType::Image => {}
        ItemType::Text => buf.push_str(doc.text(id)),
        // Yomitan places the children of a `structured-content` item directly
        // in a `.gloss-content` block. The item therefore adds no line break
        // and does not use the role rules.
        ItemType::StructuredContent => children_into(doc, id, true, buf),
        _ if doc.is_plain_string(id) => buf.push_str(doc.text(id)),
        _ => node_into(doc, id, false, buf),
    }
}

/// Returns part-of-speech labels in order without duplicates.
///
/// This separate walk prevents duplicate labels in the Anki card.
/// `Card::pos` places labels above the glosses, and [`plain_items`] omits the
/// same labels from the glosses.
pub fn pos_labels(doc: &GlossDoc) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for id in doc.items() {
        // A plain glossary string has no `data` map or label.
        if doc.is_plain_string(id) {
            continue;
        }
        collect_pos(doc, id, &mut out, &mut buf);
    }
    out
}

/// Collects part-of-speech labels from one node and its descendants.
///
/// Two roles stop this walk for different reasons. [`Role::PartOfSpeech`] is
/// the label. The walk collects that label but does not enter its descendants.
///
/// An example or attribution is Editorial content that the summary omits. A
/// label inside a quotation describes that quotation, not the Entry. It must
/// not enter the card's `pos` field. The old drop list used the same rule.
fn collect_pos(doc: &GlossDoc, id: NodeId, out: &mut Vec<String>, buf: &mut String) {
    let role = doc.role(id);
    if role == Role::PartOfSpeech {
        buf.clear();
        children_into(doc, id, !doc.node(id).tag.is_inline(), buf);
        let label = tidy(buf);
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
        }
        return;
    }
    if !RoleFilter::SUMMARY.allows(role) {
        return;
    }
    for child in doc.children(id) {
        collect_pos(doc, child, out, buf);
    }
}

/// Renders one node into the buffer.
///
/// `prose` records this node's context. It is true when visible plain-text is
/// beside the node or a prose fragment precedes it. In this context, a marked
/// inline node does not add a line break. See [`GlossDoc::prose`] and
/// [`GlossDoc::inline_prose`].
///
/// The parent tag sets the child context. [`children_into`] uses only that tag
/// to decide whether a string sequence becomes separate lines.
/// [`ruby_into`] uses the parent `ruby` node to decide whether a reading needs
/// brackets.
///
/// An image is the only tag with no plain-text output. It can represent a
/// character that this renderer cannot draw. The popup renderer can draw that
/// character. An `rt` or `rp` outside a `ruby` is malformed. This renderer
/// keeps its plain-text output, as the popup inline pass does.
fn node_into(doc: &GlossDoc, id: NodeId, prose: bool, out: &mut String) {
    let n = doc.node(id);
    if !RoleFilter::SUMMARY.allows(doc.role(id)) {
        return;
    }
    if n.kind == Kind::Image {
        return;
    }
    if n.tag == Tag::Ruby {
        ruby_into(doc, id, out);
        return;
    }
    if n.tag == Tag::Br {
        out.push('\n');
        return;
    }
    if n.kind == Kind::Text && n.tag == Tag::None {
        out.push_str(doc.text(id));
        return;
    }
    if doc.is_block(id) || (doc.has_marker(id) && !prose) {
        out.push_str(BLOCK_MARK);
    }
    children_into(doc, id, !doc.is_inline(id), out);
}

/// Renders the children of one node.
///
/// Yomitan concatenates child output with one exception.
/// [`GlossDoc::is_string_list`] identifies a glossary array of plain strings.
/// Each string in that array gets one line.
///
/// `block_ctx` is false only under an inline tag. The schema permits no line
/// break there. Therefore, Yomitan puts all children of an inline node in one
/// box.
fn children_into(doc: &GlossDoc, id: NodeId, block_ctx: bool, out: &mut String) {
    if block_ctx && doc.is_string_list(id) {
        for child in doc.children(id) {
            out.push_str(BLOCK_MARK);
            out.push_str(doc.text(child));
        }
        return;
    }
    let mut prose = doc.prose(id);
    for child in doc.children(id) {
        node_into(doc, child, prose, out);
        prose = prose || doc.inline_prose(child);
    }
}

/// Resolves one rendered string.
///
/// Each block mark becomes a line break. This function removes empty parts.
/// Nested blocks therefore produce one line for each innermost block. They do
/// not produce a sequence of blank lines.
///
/// Both platform text engines use `\n` as a hard break. A break from this
/// function therefore reaches the Panel.
fn tidy(text: &str) -> String {
    let parts: Vec<&str> =
        text.split(BLOCK_MARK).map(str::trim).filter(|p| !p.is_empty()).collect();
    let collapsed = collapse_spaces(&parts.join("\n"));
    collapsed.split('\n').map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Collapses each run of supported whitespace to one space.
///
/// Supported whitespace includes spaces, tabs, and U+3000.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\u{3000}') {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}


//! Address and range helpers for role-visible gloss text.
//!
//! A document address uses the parsed tree's preorder path and a UTF-8 byte
//! offset. This keeps selection independent of screen layout and lets every
//! renderer apply the same range boundaries.

use super::{GlossDoc, Kind, NodeId, NodePath, RoleFilter, Tag};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;

/// One position in a [`GlossDoc`]: a role-visible leaf and a byte offset in it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DocAddr {
    pub path: NodePath,
    pub byte: u32,
}

/// A half-open document range, `[start, end)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DocRange {
    pub start: DocAddr,
    pub end: DocAddr,
}

impl DocAddr {
    /// The first address in a document.
    pub const START: DocAddr = DocAddr { path: NodePath::ROOT, byte: 0 };
    /// The address after every node in a document.
    pub const END: DocAddr = DocAddr { path: NodePath::END, byte: 0 };
}

/// The text separator for fragments in one text container.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Separator {
    /// Insert an ellipsis between fragments.
    #[default]
    Ellipsis,
    /// Insert one ASCII space between fragments.
    Space,
    /// Insert an HTML line break or a plain-text line break.
    LineBreak,
    /// Wrap each fragment in one list item.
    ListItems,
}

/// One role-visible text leaf in document order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Leaf {
    pub path: NodePath,
    pub len: u32,
    pub atomic: bool,
}

/// Returns every role-visible text or ruby leaf in document order.
pub fn leaves(doc: &GlossDoc, roles: RoleFilter) -> Vec<Leaf> {
    let mut out = Vec::new();
    for (index, id) in doc.items().enumerate() {
        let Some(path) = NodePath::ROOT.child(index) else { continue };
        collect_leaves(doc, id, path, roles, true, &mut out);
    }
    out
}

fn collect_leaves(
    doc: &GlossDoc,
    id: NodeId,
    path: NodePath,
    roles: RoleFilter,
    ancestors_visible: bool,
    out: &mut Vec<Leaf>,
) {
    let node = doc.node(id);
    let visible = ancestors_visible && roles.allows(node.role);
    if !visible || matches!(node.tag, Tag::Rt | Tag::Rp) {
        return;
    }
    if node.tag == Tag::Ruby {
        out.push(Leaf { path, len: 1, atomic: true });
        return;
    }
    if node.kind == Kind::Text {
        let len = doc.text(id).len();
        if len != 0 {
            out.push(Leaf { path, len: len as u32, atomic: false });
        }
        return;
    }
    for (index, child) in doc.children(id).enumerate() {
        let Some(child_path) = path.child(index) else { continue };
        collect_leaves(doc, child, child_path, roles, visible, out);
    }
}

/// Returns the text for a text leaf, or an empty string for another path.
pub fn leaf_text(doc: &GlossDoc, path: NodePath) -> &str {
    let Some(nodes) = path_nodes(doc, path) else { return "" };
    let Some(&id) = nodes.last() else { return "" };
    let node = doc.node(id);
    if node.kind != Kind::Text
        || nodes.iter().any(|&ancestor| matches!(doc.node(ancestor).tag, Tag::Ruby | Tag::Rt | Tag::Rp))
    {
        return "";
    }
    doc.text(id)
}

/// The selected byte intervals of every role-visible leaf, from a set of ranges.
/// Both selected-range renderers (`html`, `plain`) build the same plan.
pub(super) struct SelectionPlan {
    pub leaves: Vec<Leaf>,
    pub intervals: HashMap<NodePath, Vec<(u32, u32)>>,
    pub indexes: HashMap<NodePath, usize>,
}

impl SelectionPlan {
    /// `None` when no range covers a visible leaf.
    pub(super) fn new(doc: &GlossDoc, ranges: &[DocRange], roles: RoleFilter) -> Option<Self> {
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
            return None;
        }
        let visible = leaves(doc, roles);
        if visible.is_empty() {
            return None;
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
                let start =
                    if range.start.path < leaf.path { 0 } else { range.start.byte.min(leaf.len) };
                let end =
                    if range.end.path > leaf.path { leaf.len } else { range.end.byte.min(leaf.len) };
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
        (!intervals.is_empty()).then_some(SelectionPlan { leaves: visible, intervals, indexes })
    }

    /// Every visible leaf is fully selected, so whole rendering is equivalent.
    pub(super) fn covers_all(&self) -> bool {
        self.leaves.iter().all(|leaf| {
            self.intervals
                .get(&leaf.path)
                .is_some_and(|covered| covered.len() == 1 && covered[0] == (0, leaf.len))
        })
    }

    pub(super) fn has_selected_child(&self, doc: &GlossDoc, id: NodeId, path: NodePath) -> bool {
        if self.intervals.contains_key(&path) {
            return true;
        }
        doc.children(id).enumerate().any(|(index, child)| {
            path.child(index)
                .is_some_and(|child_path| self.has_selected_child(doc, child, child_path))
        })
    }
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

/// Returns the range from the first visible leaf to the end of the last one.
pub fn extent(doc: &GlossDoc, roles: RoleFilter) -> Option<DocRange> {
    let all = leaves(doc, roles);
    let first = all.first()?;
    let last = all.last()?;
    Some(DocRange {
        start: DocAddr { path: first.path, byte: 0 },
        end: DocAddr { path: last.path, byte: last.len },
    })
}

/// Returns the visible range for the Sense that contains `addr`.
///
/// Dictionaries do not agree on how a Sense looks. Jitendex marks a Sense with
/// `data.content = "sense"`, as a list item, or as one `div`. 三省堂 and
/// 大辞林 use a `div` whose first span holds `①`. 広辞苑 nests `①` divs under
/// a `❶` div. 大辞泉 starts a Sense `div` with a bare `1`. 新和英 is one text
/// leaf whose lines start with `1 `. 国語辞典オンライン numbers a Sense with
/// an image. 明鏡 puts each Sense's examples in a `details` after the Sense
/// `div`.
///
/// The rules, innermost first:
///
/// 1. A line of a multi-line text leaf that starts with a Sense marker.
/// 2. An `li` in a `ul`. Jitendex groups separate selectable gloss lines under
///    one explicit Sense, so the inner list item wins.
/// 3. The innermost marked block ancestor, with the per-Sense blocks that
///    follow it. A marked block is an `li` in an `ol`, a block with
///    `data.content = "sense"`, or a block whose first text or image is a
///    Sense number. A nested `①` wins over its `❶` group. See
///    [`sibling_sense`].
/// 4. The innermost block or newline-delimited line. Marker-free Dictionaries
///    often put a repeated headword beside the gloss. A selection of the whole
///    `GlossDoc` would include that unrelated line.
pub fn sense_range(doc: &GlossDoc, roles: RoleFilter, addr: DocAddr) -> Option<DocRange> {
    sense(doc, roles, addr).map(|sense| sense.with_examples)
}

/// Returns the Sense that contains `addr` without its examples.
///
/// The range stops before the first example leaf inside the marked block.
/// It also excludes the per-Sense blocks that follow the marked block.
/// Examples follow the gloss in every Dictionary shape seen. The cut keeps the
/// gloss and a note that precedes an example. It drops a note that follows an
/// example because one range cannot exclude an intermediate block.
pub fn sense_core_range(doc: &GlossDoc, roles: RoleFilter, addr: DocAddr) -> Option<DocRange> {
    let own = sense(doc, roles, addr)?.own;
    let content = leaves(doc, RoleFilter { examples: false, ..roles });
    let mut end = None;
    for leaf in leaves(doc, roles) {
        let at = DocAddr { path: leaf.path, byte: 0 };
        if at < own.start {
            continue;
        }
        if at >= own.end {
            break;
        }
        if !content.iter().any(|kept| kept.path == leaf.path) {
            break;
        }
        end = Some(DocAddr { path: leaf.path, byte: leaf.len });
    }
    Some(DocRange { start: own.start, end: end.map_or(own.end, |end| end.min(own.end)) })
}

/// Returns the line that contains `addr`: a text line of a multi-line leaf,
/// or the innermost block ancestor.
///
/// This is the paragraph rule of a browser's triple-click. It ignores Sense
/// markers, so an example line in 新明解 or a note in 三省堂 selects alone.
pub fn line_range(doc: &GlossDoc, roles: RoleFilter, addr: DocAddr) -> Option<DocRange> {
    let nodes = path_nodes(doc, addr.path)?;
    let text = leaf_text(doc, addr.path);
    if text.contains('\n') {
        let line = line_at(text, addr.byte as usize);
        return Some(DocRange {
            start: DocAddr { path: addr.path, byte: line.start as u32 },
            end: DocAddr { path: addr.path, byte: line.end as u32 },
        });
    }
    let all = leaves(doc, roles);
    let depth = (0..nodes.len()).rev().find(|&depth| is_block(doc, nodes[depth])).unwrap_or(0);
    subtree_range(&all, path_prefix(addr.path, depth + 1)?)
}

/// One resolved Sense contains the marked block and its per-Sense followers.
struct Sense {
    own: DocRange,
    with_examples: DocRange,
}

fn sense(doc: &GlossDoc, roles: RoleFilter, addr: DocAddr) -> Option<Sense> {
    let nodes = path_nodes(doc, addr.path)?;
    let text = leaf_text(doc, addr.path);
    if text.contains('\n') {
        let line = line_at(text, addr.byte as usize);
        if has_sense_marker(&text[line.clone()]) {
            let range = DocRange {
                start: DocAddr { path: addr.path, byte: line.start as u32 },
                end: DocAddr { path: addr.path, byte: line.end as u32 },
            };
            return Some(Sense { own: range, with_examples: range });
        }
    }

    let all = leaves(doc, roles);
    for depth in (0..nodes.len()).rev() {
        let id = nodes[depth];
        if !is_block(doc, id) {
            continue;
        }
        let Some(&parent) = (depth > 0).then(|| &nodes[depth - 1]) else {
            if is_marked_block(doc, id, None) {
                let range = subtree_range(&all, path_prefix(addr.path, 1)?)?;
                return Some(Sense { own: range, with_examples: range });
            }
            continue;
        };
        if doc.node(id).tag == Tag::Li && doc.node(parent).tag == Tag::Ul {
            let range = subtree_range(&all, path_prefix(addr.path, depth + 1)?)?;
            return Some(Sense { own: range, with_examples: range });
        }
        let parent_path = path_prefix(addr.path, depth)?;
        let index = *addr.path.steps().get(depth)? as usize;
        if let Some(sense) = sibling_sense(doc, &all, parent, parent_path, index) {
            return Some(sense);
        }
    }
    let range = line_range(doc, roles, addr)?;
    Some(Sense { own: range, with_examples: range })
}

/// The Sense among the children of `parent` that contains child `index`.
///
/// A marked child starts a Sense. The Sense includes unmarked blocks after it
/// when the same block shape occurs after at least two marked siblings. 明鏡
/// places each Sense's examples in a `details` after its
/// `❶` div, so a click in an example selects `❶` with its examples. A note
/// that appears once after the last Sense, such as 大辞泉's `補説`, does not
/// join that Sense.
///
/// Returns `None` when child `index` is not marked and does not attach to an
/// earlier marked sibling. The caller then checks the next block.
fn sibling_sense(
    doc: &GlossDoc,
    all: &[Leaf],
    parent: NodeId,
    parent_path: NodePath,
    index: usize,
) -> Option<Sense> {
    let parent_tag = doc.node(parent).tag;
    let children: Vec<(NodeId, NodePath, bool)> = doc
        .children(parent)
        .enumerate()
        .map(|(i, child)| {
            let marked = is_block(doc, child) && is_marked_block(doc, child, Some(parent_tag));
            (child, parent_path.child(i).unwrap_or(parent_path), marked)
        })
        .collect();
    if !children.iter().any(|(_, _, marked)| *marked) {
        return None;
    }

    // A shape that follows two or more marked siblings belongs to each Sense.
    // Each tuple stores a representative, the last marked sibling it followed,
    // and the count of distinct marked siblings it followed.
    let mut followers: Vec<(NodeId, NodeId, usize)> = Vec::new();
    let mut after_marked = None;
    for &(child, _, marked) in &children {
        if marked {
            after_marked = Some(child);
            continue;
        }
        let Some(owner) = after_marked else { continue };
        if !is_block(doc, child) {
            continue;
        }
        match followers.iter_mut().find(|(other, _, _)| same_shape(doc, *other, child)) {
            Some((_, last_owner, count)) => {
                if *last_owner != owner {
                    *last_owner = owner;
                    *count += 1;
                }
            }
            None => followers.push((child, owner, 1)),
        }
    }
    let attaches = |child: NodeId| {
        is_block(doc, child)
            && followers.iter().any(|(other, _, count)| *count >= 2 && same_shape(doc, *other, child))
    };

    let mut start = index;
    while !children[start].2 {
        if start == 0 || !attaches(children[start].0) {
            return None;
        }
        start -= 1;
    }
    let mut end = start;
    while end + 1 < children.len() && !children[end + 1].2 && attaches(children[end + 1].0) {
        end += 1;
    }
    let own = subtree_range(all, children[start].1)?;
    let last = subtree_range(all, children[end].1)?;
    Some(Sense { own, with_examples: DocRange { start: own.start, end: last.end } })
}

/// Whether the block `id` is a Sense by its own shape.
fn is_marked_block(doc: &GlossDoc, id: NodeId, parent: Option<Tag>) -> bool {
    if (doc.node(id).tag == Tag::Li && parent == Some(Tag::Ol))
        || doc.marker(id) == Some("sense")
    {
        return true;
    }
    match first_item(doc, id) {
        Some(FirstItem::Text(text)) => has_sense_marker(text),
        Some(FirstItem::Image(image)) => image_is_numeral(doc, image),
        None => false,
    }
}

/// A block starts its own line. It can be a `div`, a list item, a `details`, or
/// an inline tag with a `data.content` marker.
fn is_block(doc: &GlossDoc, id: NodeId) -> bool {
    matches!(doc.node(id).tag, Tag::Div | Tag::Li | Tag::Details) || doc.has_marker(id)
}

/// The first item in a block.
enum FirstItem<'a> {
    Text(&'a str),
    Image(NodeId),
}

/// The first non-blank text or image under `id` in document order.
fn first_item(doc: &GlossDoc, id: NodeId) -> Option<FirstItem<'_>> {
    let node = doc.node(id);
    if matches!(node.tag, Tag::Rt | Tag::Rp) {
        return None;
    }
    if node.kind == Kind::Image {
        return Some(FirstItem::Image(id));
    }
    if node.kind == Kind::Text {
        let text = doc.text(id).trim_start();
        return (!text.is_empty()).then_some(FirstItem::Text(text));
    }
    doc.children(id).find_map(|child| first_item(doc, child))
}

/// Whether an image marks a Sense number.
///
/// 国語辞典オンライン marks each Sense with an `img` whose `title` is `１番`
/// and whose file is `１-fill.svg`. A `title` or `alt` that starts with a numeral
/// counts. A file stem counts only under the text rule because a hash-named file
/// such as `1a2b.png` also starts with a digit.
fn image_is_numeral(doc: &GlossDoc, id: NodeId) -> bool {
    let attr = |key: &str| doc.attr_of(id, key).and_then(|v| doc.scalar_str(v));
    if ["title", "alt"].into_iter().filter_map(attr).any(starts_with_numeral) {
        return true;
    }
    attr("path")
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .map(|file| file.split_once('.').map_or(file, |(stem, _)| stem))
        .is_some_and(has_sense_marker)
}

/// Whether `text` starts with a digit, an enclosed numeral, or a kanji numeral.
fn starts_with_numeral(text: &str) -> bool {
    let Some(first) = text.trim_start().chars().next() else { return false };
    first.is_ascii_digit()
        || ('\u{ff10}'..='\u{ff19}').contains(&first)
        || is_enclosed_numeral(first)
        || "一二三四五六七八九十".contains(first)
}

/// Whether `c` is an enclosed numeral such as `①`, `❶`, `⑴`, `Ⅰ`, `㊀`, `㋐`, or `Ⓐ`.
fn is_enclosed_numeral(c: char) -> bool {
    matches!(
        c,
        '\u{2460}'..='\u{249b}'
            | '\u{24b6}'..='\u{24f4}'
            | '\u{2776}'..='\u{2793}'
            | '\u{2160}'..='\u{216f}'
            | '\u{3220}'..='\u{3229}'
            | '\u{3251}'..='\u{325f}'
            | '\u{3280}'..='\u{3289}'
            | '\u{32b1}'..='\u{32bf}'
            | '\u{32d0}'..='\u{32fe}'
    )
}

/// The range from the first visible leaf under `root` to the end of the last
/// leaf.
fn subtree_range(all: &[Leaf], root: NodePath) -> Option<DocRange> {
    let first = all.iter().find(|leaf| is_prefix(root, leaf.path))?;
    let last = all.iter().rev().find(|leaf| is_prefix(root, leaf.path))?;
    Some(DocRange {
        start: DocAddr { path: first.path, byte: 0 },
        end: DocAddr { path: last.path, byte: last.len },
    })
}

/// Whether two nodes share a tag, a kind, and a `data` map.
fn same_shape(doc: &GlossDoc, a: NodeId, b: NodeId) -> bool {
    let (left, right) = (doc.node(a), doc.node(b));
    if left.tag != right.tag || left.kind != right.kind {
        return false;
    }
    let (mine, theirs) = (doc.data(a), doc.data(b));
    mine.len() == theirs.len()
        && mine.iter().zip(theirs).all(|((key, value), (other_key, other_value))| {
            key == other_key
                && match (doc.scalar_str(*value), doc.scalar_str(*other_value)) {
                    (Some(a), Some(b)) => a == b,
                    (None, None) => value == other_value,
                    _ => false,
                }
        })
}

/// The line of `text` that contains `byte`, without its line break.
fn line_at(text: &str, byte: usize) -> std::ops::Range<usize> {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let start = text[..byte].rfind('\n').map_or(0, |at| at + 1);
    let end = text[byte..].find('\n').map_or(text.len(), |at| byte + at);
    start..end
}

/// Whether `text` starts with a Sense number.
///
/// The set covers the enclosed numerals that Japanese Dictionaries use.
/// Examples include `①` `❶` `⓫` `⑴` `⒈` `Ⅰ` `㈠` `㊀` `㋐` and `Ⓐ`.
/// It covers one or two digits followed by a separator, such as `1 `, `2.`,
/// or `３、`. It covers bracketed numerals, such as `［一］`, `(2)`, or `〔三〕`.
/// `1980年` and `10人` are not markers because a letter follows the digits.
fn has_sense_marker(text: &str) -> bool {
    let text = text.trim_start();
    let mut chars = text.chars();
    let Some(first) = chars.next() else { return false };
    if is_enclosed_numeral(first) {
        return true;
    }
    let digit = |c: char| c.is_ascii_digit() || ('\u{ff10}'..='\u{ff19}').contains(&c);
    if digit(first) {
        let mut count = 1;
        for c in chars {
            if digit(c) {
                count += 1;
                if count > 2 {
                    return false;
                }
                continue;
            }
            return !c.is_alphanumeric();
        }
        return true;
    }
    if matches!(first, '(' | '（' | '[' | '［' | '〔' | '【') {
        let inner: Vec<char> = chars
            .take_while(|c| !matches!(c, ')' | '）' | ']' | '］' | '〕' | '】'))
            .collect();
        return !inner.is_empty()
            && inner.len() <= 3
            && inner.iter().all(|&c| digit(c) || "一二三四五六七八九十".contains(c));
    }
    false
}

/// Rounds an address down to the previous grapheme boundary.
pub fn snap_floor(doc: &GlossDoc, addr: DocAddr) -> DocAddr {
    snap(doc, addr, false)
}

/// Rounds an address up to the next grapheme boundary.
pub fn snap_ceil(doc: &GlossDoc, addr: DocAddr) -> DocAddr {
    snap(doc, addr, true)
}

fn snap(doc: &GlossDoc, addr: DocAddr, ceil: bool) -> DocAddr {
    let Some(nodes) = path_nodes(doc, addr.path) else { return addr };
    let Some(&id) = nodes.last() else { return addr };
    let node = doc.node(id);
    if node.tag == Tag::Ruby
        && !nodes[..nodes.len() - 1]
            .iter()
            .any(|&ancestor| matches!(doc.node(ancestor).tag, Tag::Ruby | Tag::Rt | Tag::Rp))
    {
        return DocAddr { path: addr.path, byte: if addr.byte == 0 { 0 } else { 1 } };
    }
    let text = leaf_text(doc, addr.path);
    if text.is_empty() {
        return addr;
    }
    let byte = addr.byte.min(text.len() as u32) as usize;
    if ceil {
        for (at, _) in UnicodeSegmentation::grapheme_indices(text, true) {
            if at >= byte {
                return DocAddr { path: addr.path, byte: at as u32 };
            }
        }
        DocAddr { path: addr.path, byte: text.len() as u32 }
    } else {
        if byte >= text.len() {
            return DocAddr { path: addr.path, byte: text.len() as u32 };
        }
        let mut floor = 0;
        for (at, _) in UnicodeSegmentation::grapheme_indices(text, true) {
            if at > byte {
                break;
            }
            floor = at;
        }
        DocAddr { path: addr.path, byte: floor as u32 }
    }
}

/// Returns the grapheme cluster that contains `addr`.
pub fn grapheme_range(doc: &GlossDoc, addr: DocAddr) -> Option<DocRange> {
    let nodes = path_nodes(doc, addr.path)?;
    let id = *nodes.last()?;
    let node = doc.node(id);
    if node.tag == Tag::Ruby
        && !nodes[..nodes.len() - 1]
            .iter()
            .any(|&ancestor| matches!(doc.node(ancestor).tag, Tag::Ruby | Tag::Rt | Tag::Rp))
    {
        return Some(DocRange {
            start: DocAddr { path: addr.path, byte: 0 },
            end: DocAddr { path: addr.path, byte: 1 },
        });
    }
    let text = leaf_text(doc, addr.path);
    if text.is_empty() || addr.byte >= text.len() as u32 {
        return None;
    }
    let byte = addr.byte as usize;
    for (start, value) in UnicodeSegmentation::grapheme_indices(text, true) {
        let end = start + value.len();
        if start <= byte && byte < end {
            return Some(DocRange {
                start: DocAddr { path: addr.path, byte: start as u32 },
                end: DocAddr { path: addr.path, byte: end as u32 },
            });
        }
    }
    None
}

fn path_nodes(doc: &GlossDoc, path: NodePath) -> Option<Vec<NodeId>> {
    let mut steps = path.steps().iter();
    let mut nodes = Vec::with_capacity(path.len());
    let id = doc.items().nth(*steps.next()? as usize)?;
    nodes.push(id);
    let mut current = id;
    for step in steps {
        current = doc.children(current).nth(*step as usize)?;
        nodes.push(current);
    }
    Some(nodes)
}

fn path_prefix(path: NodePath, len: usize) -> Option<NodePath> {
    if len > path.len() {
        return None;
    }
    let mut prefix = NodePath::ROOT;
    for &step in &path.steps()[..len] {
        prefix = prefix.child(step as usize)?;
    }
    Some(prefix)
}

fn is_prefix(prefix: NodePath, path: NodePath) -> bool {
    prefix.len() <= path.len() && prefix.steps() == &path.steps()[..prefix.len()]
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc;
    use super::*;
    use serde_json::json;

    fn path(steps: &[usize]) -> NodePath {
        steps
            .iter()
            .try_fold(NodePath::ROOT, |path, &step| path.child(step))
            .expect("path fits")
    }

    #[test]
    fn sense_range_picks_an_ol_item_and_falls_back_to_the_top_item() {
        let ol = doc(&json!([{"type": "structured-content", "content": {
            "tag": "ol", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"}
            ]
        }}]));
        let second = path(&[0, 0, 1, 0]);
        assert_eq!(
            Some(DocRange {
                start: DocAddr { path: second, byte: 0 },
                end: DocAddr { path: second, byte: 6 },
            }),
            sense_range(&ol, RoleFilter::CARD, DocAddr { path: second, byte: 2 })
        );

        let plain = doc(&json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "one"},
            {"tag": "span", "content": "two"}
        ]}]));
        let one = path(&[0, 0, 0]);
        let two = path(&[0, 1, 0]);
        assert_eq!(
            Some(DocRange {
                start: DocAddr { path: one, byte: 0 },
                end: DocAddr { path: two, byte: 3 },
            }),
            sense_range(&plain, RoleFilter::CARD, DocAddr { path: two, byte: 1 })
        );
    }

    fn at(path: NodePath, byte: u32) -> DocAddr {
        DocAddr { path, byte }
    }

    fn span(start: DocAddr, end: DocAddr) -> Option<DocRange> {
        Some(DocRange { start, end })
    }

    #[test]
    fn sense_modes_include_or_exclude_examples() {
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "div", "content": [
                {"tag": "div", "content": [
                    {"tag": "span", "content": "①"},
                    {"tag": "span", "content": "食物を口に入れる。"},
                    {"tag": "div", "data": {"content": "example-sentence"}, "content": "菓子を食べる。"}
                ]},
                {"tag": "div", "content": [
                    {"tag": "span", "content": "②"},
                    {"tag": "span", "content": "生活する。"},
                    {"tag": "div", "data": {"content": "example-sentence"}, "content": "筆一本で食べる。"}
                ]}
            ]
        }}]));
        let number = path(&[0, 0, 0, 0, 0]);
        let definition = path(&[0, 0, 0, 1, 0]);
        let example = path(&[0, 0, 0, 2, 0]);
        let core = span(at(number, 0), at(definition, "食物を口に入れる。".len() as u32));
        let full = span(at(number, 0), at(example, "菓子を食べる。".len() as u32));

        assert_eq!(core, sense_core_range(&d, RoleFilter::CARD, at(definition, 3)));
        assert_eq!(core, sense_core_range(&d, RoleFilter::CARD, at(example, 3)));
        assert_eq!(full, sense_range(&d, RoleFilter::CARD, at(definition, 3)));
        assert_eq!(full, sense_range(&d, RoleFilter::CARD, at(example, 3)));
        assert_eq!(
            span(at(example, 0), at(example, "菓子を食べる。".len() as u32)),
            line_range(&d, RoleFilter::CARD, at(example, 3))
        );
    }

}

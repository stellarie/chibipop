//! Address and range helpers for role-visible gloss text.
//!
//! A document address uses the parsed tree's preorder path and a UTF-8 byte
//! offset. This keeps selection independent of screen layout and lets every
//! renderer apply the same range boundaries.

use super::{GlossDoc, Kind, NodeId, NodePath, RoleFilter, Tag};
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
pub fn leaf_text<'a>(doc: &'a GlossDoc, path: NodePath) -> &'a str {
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

/// Returns the visible range for the sense that contains `addr`.
pub fn sense_range(doc: &GlossDoc, roles: RoleFilter, addr: DocAddr) -> Option<DocRange> {
    let nodes = path_nodes(doc, addr.path)?;
    let steps = addr.path.steps();
    let mut root = None;
    for depth in (0..nodes.len()).rev() {
        if depth > 0
            && doc.node(nodes[depth]).tag == Tag::Li
            && doc.node(nodes[depth - 1]).tag == Tag::Ol
        {
            root = path_prefix(addr.path, depth + 1);
            break;
        }
    }
    let root = root.or_else(|| NodePath::ROOT.child(*steps.first()? as usize))?;
    let all = leaves(doc, roles);
    let first = all.iter().find(|leaf| is_prefix(root, leaf.path))?;
    let last = all.iter().rev().find(|leaf| is_prefix(root, leaf.path))?;
    Some(DocRange {
        start: DocAddr { path: first.path, byte: 0 },
        end: DocAddr { path: last.path, byte: last.len },
    })
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
    let Some(&id) = nodes.last() else { return None };
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
    fn leaves_follow_document_order_and_role_visibility() {
        let d = doc(&json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "first"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": "hidden"},
            {"tag": "b", "content": "last"}
        ]}]));
        let card = leaves(&d, RoleFilter::CARD);
        assert_eq!(
            vec![
                Leaf { path: path(&[0, 0, 0]), len: 5, atomic: false },
                Leaf { path: path(&[0, 1, 0]), len: 6, atomic: false },
                Leaf { path: path(&[0, 2, 0]), len: 4, atomic: false },
            ],
            card
        );
        let summary = leaves(&d, RoleFilter::SUMMARY);
        assert_eq!(
            vec![
                Leaf { path: path(&[0, 0, 0]), len: 5, atomic: false },
                Leaf { path: path(&[0, 2, 0]), len: 4, atomic: false },
            ],
            summary
        );
    }

    #[test]
    fn ruby_is_one_atomic_leaf() {
        let d = doc(&json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "content": "ねこ"}
            ]}
        ]}]));
        let found = leaves(&d, RoleFilter::CARD);
        assert_eq!(vec![Leaf { path: path(&[0, 0]), len: 1, atomic: true }], found);
        assert_eq!("", leaf_text(&d, path(&[0, 0])));
        assert_eq!("", leaf_text(&d, path(&[0, 0, 0])));
    }

    #[test]
    fn snapping_respects_combining_graphemes_and_flag_clusters() {
        let d = doc(&json!(["a\u{301}🇯🇵x"]));
        let leaf = path(&[0]);
        assert_eq!(DocAddr { path: leaf, byte: 0 }, snap_floor(&d, DocAddr { path: leaf, byte: 2 }));
        assert_eq!(DocAddr { path: leaf, byte: 3 }, snap_ceil(&d, DocAddr { path: leaf, byte: 2 }));
        assert_eq!(DocAddr { path: leaf, byte: 3 }, snap_floor(&d, DocAddr { path: leaf, byte: 4 }));
        assert_eq!(DocAddr { path: leaf, byte: 11 }, snap_ceil(&d, DocAddr { path: leaf, byte: 4 }));
        assert_eq!(
            Some(DocRange {
                start: DocAddr { path: leaf, byte: 3 },
                end: DocAddr { path: leaf, byte: 11 },
            }),
            grapheme_range(&d, DocAddr { path: leaf, byte: 4 })
        );
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
}

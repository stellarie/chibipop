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
///
/// Dictionaries do not agree on how a sense looks. Jitendex uses `ol > li`.
/// 三省堂 and 大辞林 use a `div` whose first span holds `①`. 広辞苑 nests
/// `①` divs under a `❶` div. 大辞泉 starts a sense div with a bare `1`.
/// 新和英 is one text leaf whose lines start with `1 `. 国語辞典オンライン
/// has plain sibling divs and no numbers at all. One rule for `ol > li`
/// therefore selected a whole Entry in every other dictionary.
///
/// The rules, innermost first:
///
/// 1. A line of a multi-line text leaf that starts with a sense marker.
/// 2. The innermost block ancestor that is an `li` in an `ol`, or whose first
///    visible text starts with a sense marker. A nested `①` therefore wins
///    over its `❶` group when the address sits inside it.
/// 3. The innermost block ancestor with a sibling of the same tag and `data`.
/// 4. The top-level glossary item.
pub fn sense_range(doc: &GlossDoc, roles: RoleFilter, addr: DocAddr) -> Option<DocRange> {
    let nodes = path_nodes(doc, addr.path)?;
    let text = leaf_text(doc, addr.path);
    if text.contains('\n') {
        let line = line_at(text, addr.byte as usize);
        if has_sense_marker(&text[line.clone()]) {
            return Some(DocRange {
                start: DocAddr { path: addr.path, byte: line.start as u32 },
                end: DocAddr { path: addr.path, byte: line.end as u32 },
            });
        }
    }

    let all = leaves(doc, roles);
    let mut twin = None;
    for depth in (0..nodes.len()).rev() {
        let id = nodes[depth];
        if !is_block(doc, id) {
            continue;
        }
        let root = path_prefix(addr.path, depth + 1)?;
        let listed = doc.node(id).tag == Tag::Li
            && depth > 0
            && doc.node(nodes[depth - 1]).tag == Tag::Ol;
        if listed || first_text(doc, &all, root).is_some_and(has_sense_marker) {
            return subtree_range(&all, root);
        }
        if twin.is_none() && depth > 0 && has_twin(doc, nodes[depth - 1], id) {
            twin = Some(root);
        }
    }
    let root = twin.or_else(|| NodePath::ROOT.child(*addr.path.steps().first()? as usize))?;
    subtree_range(&all, root)
}

/// A node that starts its own line: a `div`, a list item, a `details`, or an
/// inline tag with a `data.content` marker.
fn is_block(doc: &GlossDoc, id: NodeId) -> bool {
    matches!(doc.node(id).tag, Tag::Div | Tag::Li | Tag::Details) || doc.has_marker(id)
}

/// The first non-blank leaf text under `root`, with leading whitespace removed.
fn first_text<'a>(doc: &'a GlossDoc, all: &[Leaf], root: NodePath) -> Option<&'a str> {
    all.iter()
        .filter(|leaf| is_prefix(root, leaf.path))
        .map(|leaf| leaf_text(doc, leaf.path).trim_start())
        .find(|text| !text.is_empty())
}

/// The range from the first visible leaf under `root` to the end of the last.
fn subtree_range(all: &[Leaf], root: NodePath) -> Option<DocRange> {
    let first = all.iter().find(|leaf| is_prefix(root, leaf.path))?;
    let last = all.iter().rev().find(|leaf| is_prefix(root, leaf.path))?;
    Some(DocRange {
        start: DocAddr { path: first.path, byte: 0 },
        end: DocAddr { path: last.path, byte: last.len },
    })
}

/// Whether another child of `parent` has the same tag and `data` map as `id`.
fn has_twin(doc: &GlossDoc, parent: NodeId, id: NodeId) -> bool {
    let node = doc.node(id);
    let same_data = |other: NodeId| {
        let mine = doc.data(id);
        let theirs = doc.data(other);
        mine.len() == theirs.len()
            && mine.iter().zip(theirs).all(|((key, value), (other_key, other_value))| {
                key == other_key
                    && match (doc.scalar_str(*value), doc.scalar_str(*other_value)) {
                        (Some(a), Some(b)) => a == b,
                        (None, None) => value == other_value,
                        _ => false,
                    }
            })
    };
    doc.children(parent).any(|other| {
        other != id && doc.node(other).tag == node.tag && doc.node(other).kind == node.kind && same_data(other)
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

/// Whether `text` starts with a sense number.
///
/// The set covers the enclosed numerals that Japanese dictionaries use
/// (`①` `❶` `⓫` `⑴` `⒈` `Ⅰ` `㈠` `㊀` `㋐` `Ⓐ`), one or two digits followed by
/// a separator (`1 `, `2.`, `３、`), and a bracketed numeral (`［一］`, `(2)`,
/// `〔三〕`). `1980年` and `10人` are not markers because a letter follows the
/// digits.
fn has_sense_marker(text: &str) -> bool {
    let text = text.trim_start();
    let mut chars = text.chars();
    let Some(first) = chars.next() else { return false };
    if matches!(
        first,
        '\u{2460}'..='\u{249b}'
            | '\u{24b6}'..='\u{24f4}'
            | '\u{2776}'..='\u{2793}'
            | '\u{2160}'..='\u{216f}'
            | '\u{3220}'..='\u{3229}'
            | '\u{3251}'..='\u{325f}'
            | '\u{3280}'..='\u{3289}'
            | '\u{32b1}'..='\u{32bf}'
            | '\u{32d0}'..='\u{32fe}'
    ) {
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

    fn at(path: NodePath, byte: u32) -> DocAddr {
        DocAddr { path, byte }
    }

    fn span(start: DocAddr, end: DocAddr) -> Option<DocRange> {
        Some(DocRange { start, end })
    }

    #[test]
    fn sense_markers_cover_the_shapes_that_dictionaries_use() {
        for marker in ["①", "❶", "⑴", "Ⅰ", "㊀", "㋐", "Ⓐ", "1 ", "2.", "３、", "12〔", "［一］", "(2)", "〔三〕", "1"] {
            assert!(has_sense_marker(marker), "{marker:?}");
        }
        for text in ["1980年", "10人", "〘他五〙", "「果物を", "", "(abc)", "食物", "123 "] {
            assert!(!has_sense_marker(text), "{text:?}");
        }
    }

    #[test]
    fn sense_range_picks_the_marked_div_around_a_definition_or_its_example() {
        // The 三省堂 shape: a numbered span, a definition, and an example div.
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "div", "data": {"name": "大語義"}, "content": [
                {"tag": "div", "data": {"name": "語義"}, "content": [
                    {"tag": "span", "data": {"name": "語義番号"}, "content": "①"},
                    {"tag": "span", "data": {"name": "語釈"}, "content": "食物をかんで、おなかに入れる。"},
                    {"tag": "div", "data": {"name": "用例G"}, "content": "「まんじゅうを━」"}
                ]},
                {"tag": "div", "data": {"name": "語義"}, "content": [
                    {"tag": "span", "data": {"name": "語義番号"}, "content": "②"},
                    {"tag": "span", "data": {"name": "語釈"}, "content": "生活する。"}
                ]}
            ]
        }}]));
        let first_number = path(&[0, 0, 0, 0, 0]);
        let first_example = path(&[0, 0, 0, 2, 0]);
        let expected = span(at(first_number, 0), at(first_example, "「まんじゅうを━」".len() as u32));
        assert_eq!(expected, sense_range(&d, RoleFilter::CARD, at(path(&[0, 0, 0, 1, 0]), 3)));
        assert_eq!(expected, sense_range(&d, RoleFilter::CARD, at(first_example, 3)));
        let second_number = path(&[0, 0, 1, 0, 0]);
        let second_text = path(&[0, 0, 1, 1, 0]);
        assert_eq!(
            span(at(second_number, 0), at(second_text, "生活する。".len() as u32)),
            sense_range(&d, RoleFilter::CARD, at(second_text, 0))
        );
    }

    #[test]
    fn sense_range_prefers_a_nested_number_over_its_group() {
        // The 広辞苑 shape: ❶ groups with ① members as nested divs.
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "div", "content": [
                {"tag": "div", "content": [
                    "❶手ににぎりもつ。",
                    {"tag": "div", "content": "①手にもつ。"},
                    {"tag": "div", "content": "②つかまえる。"}
                ]},
                {"tag": "div", "content": ["❷引き離す。"]}
            ]
        }}]));
        let head = path(&[0, 0, 0, 0]);
        let one = path(&[0, 0, 0, 1, 0]);
        let two = path(&[0, 0, 0, 2, 0]);
        assert_eq!(span(at(one, 0), at(one, "①手にもつ。".len() as u32)), sense_range(&d, RoleFilter::CARD, at(one, 3)));
        assert_eq!(span(at(head, 0), at(two, "②つかまえる。".len() as u32)), sense_range(&d, RoleFilter::CARD, at(head, 3)));
    }

    #[test]
    fn sense_range_accepts_a_bare_digit_leaf_as_a_marker() {
        // The 大辞泉 shape: the number is its own leaf inside the sense div.
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "div", "content": [
                {"tag": "div", "content": [{"tag": "span", "content": "1"}, {"tag": "span", "content": "食物を口に入れる。"}]},
                {"tag": "div", "content": [{"tag": "span", "content": "2"}, {"tag": "span", "content": "生計を立てる。"}]}
            ]
        }}]));
        let number = path(&[0, 0, 1, 0, 0]);
        let text = path(&[0, 0, 1, 1, 0]);
        assert_eq!(
            span(at(number, 0), at(text, "生計を立てる。".len() as u32)),
            sense_range(&d, RoleFilter::CARD, at(text, 0))
        );
    }

    #[test]
    fn sense_range_selects_a_marked_line_of_plain_text() {
        let text = "とる【取る】\n1 〔手に持つ〕 take.\n2 〔手に入れる〕 get.";
        let d = doc(&json!([text]));
        let leaf = path(&[0]);
        let second = text.find("2 〔").unwrap() as u32;
        assert_eq!(
            span(at(leaf, second), at(leaf, text.len() as u32)),
            sense_range(&d, RoleFilter::CARD, at(leaf, second + 4))
        );
        // The headword line has no marker, so the whole item remains.
        assert_eq!(span(at(leaf, 0), at(leaf, text.len() as u32)), sense_range(&d, RoleFilter::CARD, at(leaf, 1)));
    }

    #[test]
    fn sense_range_falls_back_to_a_sibling_block_without_markers() {
        // The 国語辞典オンライン shape: plain sibling divs.
        let d = doc(&json!([{"type": "structured-content", "content": {
            "tag": "div", "content": [
                {"tag": "div", "content": "離れている物を手に持つ。"},
                {"tag": "div", "content": "付いている物をはずす。"},
                {"tag": "div", "data": {"name": "参照"}, "content": "⇒"}
            ]
        }}]));
        let second = path(&[0, 0, 1, 0]);
        assert_eq!(
            span(at(second, 0), at(second, "付いている物をはずす。".len() as u32)),
            sense_range(&d, RoleFilter::CARD, at(second, 0))
        );
        // A block with no twin is not a sense. The whole item remains.
        let note = path(&[0, 0, 2, 0]);
        assert_eq!(span(at(path(&[0, 0, 0, 0]), 0), at(note, "⇒".len() as u32)), sense_range(&d, RoleFilter::CARD, at(note, 0)));
    }
}

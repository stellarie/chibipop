//! Addressing one node, or a set of them, inside a [`GlossDoc`].
//!
//! A [`NodeId`] addresses a node in *one parse of one document*: it is an
//! index into the arena, so a parser fix that inserts a wrapper renumbers
//! every node after it. That is fine for a walk and wrong for an identity.
//! A [`NodePath`] is the identity - "the third child of the first glossary
//! item" - which is what the spec means by "a selection means *sense 3 of
//! 大辞林* rather than a character range".
//!
//! Two consumers, and they set the whole design:
//!
//! - **The popup scene's block pass** puts one on every `SceneElem` while it
//!   is already descending the tree. So the type is [`Copy`] with inline
//!   storage - a `Vec` per element per frame is an allocation the scene walk
//!   cannot afford - and extending a path by one step ([`NodePath::child`])
//!   is the operation that walk performs.
//! - **The Anki HTML renderer** takes a [`Selection`] of them and emits the
//!   markup for exactly those subtrees, so a later sense picker hands Anki
//!   formatted markup rather than a flattened text range.
//!
//! The path is also the safe handle of the two: resolving one walks down from
//! the root and returns `None` at the first step that does not exist, so a
//! path paired with the wrong document yields nothing rather than the wrong
//! node.

use super::{GlossDoc, NodeId};

/// Steps a [`NodePath`] holds.
///
/// `docs/research/dict-shapes.md` measures the deepest real tree in a
/// 97-archive corpus at **15** levels, and a path spends one step per level,
/// so 16 clears the corpus with a step to spare while keeping the whole path
/// at 34 bytes - cheap enough to sit on every scene element. Going to
/// [`MAX_DEPTH`](super::MAX_DEPTH) would triple that for content no
/// dictionary emits.
const MAX_STEPS: usize = 16;

/// A route from a [`GlossDoc`]'s root to one node: the child index taken at
/// each level, top-level glossary item first.
///
/// Ordering is document (preorder) order, which the derive gets right
/// because the unused steps are zero and a prefix therefore compares equal up
/// to its length and shorter after it: an ancestor sorts before its
/// descendants, and a left sibling's subtree before a right one's.
///
/// A node deeper than [`MAX_STEPS`] levels, or past the 65 536th child of its
/// parent, has **no path**: [`child`](Self::child) returns `None` rather than
/// a path that would address some ancestor instead. Unaddressable is a
/// correct answer for a selection; silently aliased is not.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodePath {
    steps: [u16; MAX_STEPS],
    len: u8,
}

impl NodePath {
    /// Steps a path can hold. See [`MAX_STEPS`].
    pub const MAX_STEPS: usize = MAX_STEPS;

    /// The document itself. Addresses no node - a glossary is a forest of
    /// top-level items, not a tree with one root - and is where a walk
    /// starts: `NodePath::ROOT.child(i)` is the `i`th glossary item.
    pub const ROOT: NodePath = NodePath { steps: [0; MAX_STEPS], len: 0 };

    /// This path extended by one child index, or `None` when the step does
    /// not fit.
    pub fn child(self, index: usize) -> Option<NodePath> {
        let step = u16::try_from(index).ok()?;
        let at = self.len as usize;
        if at == MAX_STEPS {
            return None;
        }
        let mut next = self;
        next.steps[at] = step;
        next.len += 1;
        Some(next)
    }

    /// The child indices, outermost first.
    pub fn steps(&self) -> &[u16] {
        &self.steps[..self.len as usize]
    }

    /// How many levels down this path reaches.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Is this [`ROOT`](Self::ROOT) - the document rather than a node?
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The node this path addresses in `doc`, or `None` when `doc` has no
    /// such node - including for [`ROOT`](Self::ROOT), which is the document.
    pub fn resolve(&self, doc: &GlossDoc) -> Option<NodeId> {
        let mut steps = self.steps().iter();
        let mut id = doc.items().nth(*steps.next()? as usize)?;
        for step in steps {
            id = doc.children(id).nth(*step as usize)?;
        }
        Some(id)
    }

    /// The path to `target` in `doc`, or `None` when the node is not in the
    /// document or sits deeper than a path reaches.
    ///
    /// A preorder search, because the arena keeps first-child and
    /// next-sibling links and no parent link - walking up is not available.
    /// One search per selected node at mining time, which is once per card.
    pub fn of(doc: &GlossDoc, target: NodeId) -> Option<NodePath> {
        doc.items()
            .enumerate()
            .find_map(|(i, id)| descend(doc, id, NodePath::ROOT.child(i)?, target))
    }
}

/// `path` addresses `id`; find `target` at or under it.
fn descend(doc: &GlossDoc, id: NodeId, path: NodePath, target: NodeId) -> Option<NodePath> {
    if id == target {
        return Some(path);
    }
    doc.children(id)
        .enumerate()
        .find_map(|(i, child)| descend(doc, child, path.child(i)?, target))
}

/// The nodes one render covers.
///
/// Borrowed rather than owned so that passing a selection costs nothing: a
/// picker holds the paths, the renderer only reads them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Selection<'a> {
    /// Every top-level glossary item, in order. What mining a whole entry
    /// asks for, and the default.
    #[default]
    Whole,
    /// These nodes and no others.
    ///
    /// Rendered in document order however the slice is ordered, deduplicated,
    /// and a selected node subsumes any of its own descendants that are also
    /// listed - so a picker may hand over whatever the user clicked without
    /// normalising it first.
    Nodes(&'a [NodePath]),
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc;
    use super::*;
    use serde_json::json;

    /// Two senses under one glossary item, each with a nested child, so a
    /// path has somewhere to go in both directions.
    fn two_senses() -> GlossDoc {
        doc(&json!([{"type": "structured-content", "content": [
            {"tag": "div", "content": [{"tag": "span", "content": "to run"}]},
            {"tag": "div", "content": [{"tag": "span", "content": "to flow"}]}
        ]}]))
    }

    fn path(steps: &[usize]) -> NodePath {
        steps
            .iter()
            .try_fold(NodePath::ROOT, |p, &step| p.child(step))
            .expect("a path within capacity")
    }

    /// The property everything else rests on: a path found for a node
    /// resolves back to that node.
    #[test]
    fn every_node_round_trips_through_its_path() {
        let d = two_senses();
        for id in 0..d.all_nodes().len() as NodeId {
            let p = NodePath::of(&d, id).unwrap_or_else(|| panic!("a path for node {id}"));
            assert_eq!(Some(id), p.resolve(&d), "node {id} at {:?}", p.steps());
        }
    }

    #[test]
    fn a_path_names_the_child_indices_it_took() {
        let d = two_senses();
        let flow = path(&[0, 1, 0]);
        let id = flow.resolve(&d).expect("the second sense's span");
        assert_eq!("to flow", d.text(d.children(id).next().expect("its text node")));
        assert_eq!(&[0, 1, 0], flow.steps());
    }

    /// A glossary is a forest of items, so the document itself is not a node.
    #[test]
    fn the_root_addresses_the_document_and_no_node() {
        let d = two_senses();
        assert!(NodePath::ROOT.is_empty());
        assert_eq!(None, NodePath::ROOT.resolve(&d));
    }

    #[test]
    fn a_step_the_document_does_not_have_resolves_to_nothing() {
        let d = two_senses();
        assert_eq!(None, path(&[0, 9]).resolve(&d));
        assert_eq!(None, path(&[4]).resolve(&d));
    }

    /// The same path against a different entry's tree must miss rather than
    /// silently address whatever sits at that index - the failure mode a raw
    /// arena index would have.
    #[test]
    fn a_path_from_another_document_does_not_alias_a_node() {
        let other = doc(&json!(["cat"]));
        assert_eq!(None, path(&[0, 1, 0]).resolve(&other));
    }

    /// What a selection set needs: an ancestor before its descendants, and a
    /// left subtree before a right one.
    #[test]
    fn paths_sort_in_document_order() {
        let mut sorted = [path(&[1]), path(&[0, 1, 0]), path(&[0]), path(&[0, 0]), NodePath::ROOT];
        sorted.sort();
        assert_eq!(
            [NodePath::ROOT, path(&[0]), path(&[0, 0]), path(&[0, 1, 0]), path(&[1])],
            sorted,
        );
    }

    /// Two paths that differ only in a step past their shared length are
    /// different paths - the zero fill must not make a prefix equal its
    /// extension.
    #[test]
    fn a_prefix_is_not_equal_to_the_path_that_extends_it() {
        assert_ne!(path(&[0]), path(&[0, 0]));
        assert_ne!(path(&[0, 0]), path(&[0, 0, 0]));
    }

    /// Unaddressable, not aliased onto an ancestor: a selection that cannot
    /// name a node must say so.
    #[test]
    fn a_node_past_the_capacity_has_no_path() {
        let deep = (0..NodePath::MAX_STEPS).try_fold(NodePath::ROOT, |p, _| p.child(0));
        assert_eq!(NodePath::MAX_STEPS, deep.expect("exactly at capacity").len());
        assert_eq!(None, deep.expect("exactly at capacity").child(0));
        assert_eq!(None, NodePath::ROOT.child(u16::MAX as usize + 1));
    }

    #[test]
    fn a_node_that_is_not_in_the_document_has_no_path() {
        let d = two_senses();
        assert_eq!(None, NodePath::of(&d, d.all_nodes().len() as NodeId));
    }
}

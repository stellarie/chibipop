//! A node-path addresses one node or a set of nodes in a [`GlossDoc`].
//!
//! A [`NodeId`] addresses a node in one parsed document. It is an index into
//! the arena. A parser change that adds a wrapper can renumber later nodes.
//! Use a [`NodeId`] only in one tree walk. Do not use it as an identity.
//!
//! A [`NodePath`] identifies a node with child indexes from the document-tree
//! root. For example, it can identify the third child of the first glossary
//! item. This form lets a selection identify a Sense instead of a character
//! range.
//!
//! Two consumers depend on this design:
//!
//! - **The popup scene block pass** stores one path on each `SceneElem` in a
//!   tree walk. The path uses inline storage and implements [`Copy`]. A `Vec`
//!   for each element in each frame would allocate too much memory.
//! - **The Anki HTML renderer** takes a [`Selection`] and emits HTML for the
//!   exact subtrees. A later Sense picker can give Anki formatted HTML instead
//!   of a plain-text range.
//!
//! A [`NodePath`] is also a safe handle. Resolution starts at the root and
//! returns `None` at the first step that does not exist. A path from another
//! document cannot address an unrelated node.

use super::{DocRange, GlossDoc, NodeId, Separator};

/// The maximum number of steps that a [`NodePath`] can hold.
///
/// `docs/research/dict-shapes.md` reports a maximum real tree depth of 15 in
/// 97 archives. Sixteen steps cover that corpus and leave one spare step. This
/// capacity keeps each scene element's path at 34 bytes.
/// [`MAX_DEPTH`](super::MAX_DEPTH) would triple that size for content that no
/// measured Dictionary has.
const MAX_STEPS: usize = 16;

/// A node-path from a [`GlossDoc`] root to one node.
///
/// Each step stores a child index. The first step selects a top-level glossary
/// item.
///
/// The derived order is document preorder because unused steps contain zero.
/// An ancestor sorts before its descendants. A left sibling subtree sorts
/// before a right sibling subtree.
///
/// A node has no path when it exceeds [`MAX_STEPS`] levels. A child after the
/// 65,536th child also has no path. [`child`](Self::child) returns `None` in
/// both cases. It never returns a path for an ancestor.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodePath {
    steps: [u16; MAX_STEPS],
    len: u8,
}

impl NodePath {
    /// The maximum number of steps that a path can hold. See [`MAX_STEPS`].
    pub const MAX_STEPS: usize = MAX_STEPS;

    /// The document root, which does not address a node.
    ///
    /// A glossary is a forest of top-level items. A tree walk starts at this
    /// root. `NodePath::ROOT.child(i)` addresses glossary item `i`.
    pub const ROOT: NodePath = NodePath { steps: [0; MAX_STEPS], len: 0 };
    /// The address after every real node in document order.
    ///
    /// Every step has its maximum value. This path sorts after every path that
    /// [`child`](Self::child) can create.
    pub const END: NodePath = NodePath { steps: [u16::MAX; MAX_STEPS], len: MAX_STEPS as u8 };


    /// Appends one child index to this path.
    ///
    /// Returns `None` when the new step does not fit or when the index is the
    /// sentinel reserved by [`END`](Self::END).
    pub fn child(self, index: usize) -> Option<NodePath> {
        let step = u16::try_from(index).ok()?;
        let at = self.len as usize;
        if at == MAX_STEPS || step == u16::MAX {
            return None;
        }
        let mut next = self;
        next.steps[at] = step;
        next.len += 1;
        Some(next)
    }

    /// Returns child indexes from the outermost level.
    pub fn steps(&self) -> &[u16] {
        &self.steps[..self.len as usize]
    }

    /// Returns the number of levels in this path.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Tests if this path is [`ROOT`](Self::ROOT).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the node that this path addresses in `doc`.
    ///
    /// Returns `None` when `doc` has no such node. [`ROOT`](Self::ROOT) also
    /// returns `None` because it addresses the document root, not a node.
    pub fn resolve(&self, doc: &GlossDoc) -> Option<NodeId> {
        let mut steps = self.steps().iter();
        let mut id = doc.items().nth(*steps.next()? as usize)?;
        for step in steps {
            id = doc.children(id).nth(*step as usize)?;
        }
        Some(id)
    }

    /// Finds the path to `target` in `doc`.
    ///
    /// Returns `None` when the node is not in the document or exceeds the path
    /// capacity.
    ///
    /// The arena has `first_child` and `next_sibling` links but no parent link.
    /// This function therefore uses a preorder search. It searches once for
    /// each selected node when it makes an Anki card.
    pub fn of(doc: &GlossDoc, target: NodeId) -> Option<NodePath> {
        doc.items()
            .enumerate()
            .find_map(|(i, id)| descend(doc, id, NodePath::ROOT.child(i)?, target))
    }
}

/// Finds `target` at or below `id`, which `path` addresses.
fn descend(doc: &GlossDoc, id: NodeId, path: NodePath, target: NodeId) -> Option<NodePath> {
    if id == target {
        return Some(path);
    }
    doc.children(id)
        .enumerate()
        .find_map(|(i, child)| descend(doc, child, path.child(i)?, target))
}

/// The nodes that a render includes.
///
/// A [`Selection`] borrows paths from the picker. The renderer reads the paths
/// and does not allocate an owned path list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Selection<'a> {
    /// All top-level glossary items in document order.
    ///
    /// This is the default selection for a complete Entry.
    #[default]
    Whole,
    /// Only these nodes.
    ///
    /// The renderer sorts these nodes into document order and removes duplicate
    /// nodes. A selected node includes its selected descendants. A picker
    /// therefore does not need to sort paths or remove duplicate paths.
    Nodes(&'a [NodePath]),
    /// Only the bytes inside these ranges.
    ///
    /// The renderer computes the union of the ranges and uses `separator`
    /// between fragments inside one text container.
    Ranges { ranges: &'a [DocRange], separator: Separator },
}


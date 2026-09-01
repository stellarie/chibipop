//! `GlossDoc`: an Entry's gloss content as one flat, typed tree.
//!
//! The shape is a **flat arena**, not an enum-with-`Vec`-children box tree,
//! and the reason is not parse speed. `examples/gloss_arena_bench.rs`
//! prototyped all three candidates - `serde_json::Value`, a box tree, and
//! this arena - and asserted their walks byte-identical over 5 200 real
//! payloads. Its numbers, recorded in `docs/research/lookup-cost.md`:
//!
//! | per hover, p50 | parse µs | walk µs | allocs | retained B/node | style probe µs |
//! |---|---:|---:|---:|---:|---:|
//! | `serde_json::Value` | 94.8 | 9.8 | 3 745 | 678.2 | 11.1 |
//! | box tree | 81.5 | 1.6 | 3 085 | 510.9 | 7.8 |
//! | **flat arena** | **78.8** | **1.5** | **383** | **106.5** | **2.2** |
//!
//! The arena's parse win over a box tree is 2.7 µs - noise. What is not
//! noise is 8.1x fewer allocations, 4.6x less retained heap, 12.1x faster to
//! free, and a 3.6x-17.1x faster per-node `data-*` probe. The first three
//! matter because this tree is *cached* (`SqliteDictionary`'s parsed-tree
//! cache); the last matters because ticket 17's stylesheet matcher probes
//! every node's `data` map on every hover.
//!
//! Concretely, and this is the whole layout:
//!
//! - one `Vec<Node>` of `Copy` structs, children as a first-child /
//!   next-sibling index pair, so a subtree costs no allocation of its own;
//! - every byte of text - node text, attribute values, `data` values,
//!   interned key names - in one `String`, addressed by `(u32, u32)` spans;
//! - `data` and attribute keys interned to a `u32` id, so a probe is an
//!   integer compare rather than a hash of a string;
//! - a node's `data`, attribute and resolved-style entries flushed
//!   contiguously into three flat vectors, addressed by a span.
//!
//! Six vectors and one string per document. A clone is six `memcpy`s and a
//! drop is six frees, whatever the tree's size.
//!
//! The tree is **lossless** over the Yomitan structured-content schema. An
//! unrecognised tag becomes [`Tag::Other`] and keeps both its name (as an
//! attribute) and its children; an unrecognised `type` and every
//! non-structural field are kept as attributes. Losslessness is what makes
//! later work free: the record stores the dictionary's raw glossary JSON and
//! this parse runs per hover, so a renderer *or parser* fix ships as a patch
//! and never as a dictionary rebuild.

mod html;
mod parse;
mod path;
mod plain;

#[cfg(test)]
mod tests;

pub use html::{render_html, RoleFilter};
pub(crate) use parse::tag_for;
pub use path::{NodePath, Selection};
pub use plain::{plain_items, pos_labels, renders_text};

/// An index into [`GlossDoc`]'s node vector.
pub type NodeId = u32;

/// An interned `data`/attribute key name.
pub type KeyId = u32;

/// The absent index. `u32::MAX` rather than `Option<u32>` so a [`Node`] stays
/// 44 bytes: a niche-optimised `Option<u32>` does not exist, and a `u64`
/// would grow every node by 20 bytes for a document that cannot hold four
/// billion nodes anyway.
pub const NONE: NodeId = u32::MAX;

/// A byte range into [`GlossDoc`]'s text buffer, or an element range into one
/// of its key-value vectors. Which one depends on the field that holds it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Span {
    pub at: u32,
    pub len: u32,
}

/// A stored `data`, attribute, or style value.
///
/// Numbers and booleans keep their JSON type instead of being formatted into
/// the text buffer: the schema's numbers are em multipliers and span counts,
/// and formatting them at parse time would put an `f64` printer on the hot
/// path for no representational gain. An array of strings is joined with a
/// single space at parse time, which is what `textDecorationLine`'s list form
/// means and what the HTML renderer used to do on every walk.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scalar {
    Text(Span),
    Num(f64),
    Bool(bool),
    Null,
}

/// What a node *is*, for renderers that do not care which exact tag drew it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Text,
    Break,
    Container,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    Ruby,
    Link,
    Image,
    /// A tag this build does not know. Keeps its children, so an
    /// unrecognised wrapper loses the wrapper and not the text.
    Unknown,
}

/// The exact tag, kept alongside [`Kind`] because a `div` and a `span` are
/// both containers and a lossless tree may not forget which one it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Tag {
    /// No `tag` field: a bare glossary string, a `type: text` item, or a
    /// wrapper object carrying only `content`.
    None,
    /// Unrecognised. The name itself is kept as the node's `tag` attribute.
    Other,
    A,
    B,
    Big,
    Br,
    Code,
    Details,
    Div,
    Em,
    I,
    Img,
    Li,
    Ol,
    Pre,
    Rp,
    Rt,
    Ruby,
    S,
    Small,
    Span,
    Strong,
    Sub,
    Summary,
    Sup,
    Table,
    Tbody,
    Td,
    Tfoot,
    Th,
    Thead,
    Tr,
    U,
    Ul,
}

impl Tag {
    /// The schema's own spelling, or `""` for [`Tag::None`] and
    /// [`Tag::Other`] - neither of which has one. Use
    /// [`GlossDoc::tag_name`] when the unrecognised name matters.
    pub fn as_str(self) -> &'static str {
        match self {
            Tag::None | Tag::Other => "",
            Tag::A => "a",
            Tag::B => "b",
            Tag::Big => "big",
            Tag::Br => "br",
            Tag::Code => "code",
            Tag::Details => "details",
            Tag::Div => "div",
            Tag::Em => "em",
            Tag::I => "i",
            Tag::Img => "img",
            Tag::Li => "li",
            Tag::Ol => "ol",
            Tag::Pre => "pre",
            Tag::Rp => "rp",
            Tag::Rt => "rt",
            Tag::Ruby => "ruby",
            Tag::S => "s",
            Tag::Small => "small",
            Tag::Span => "span",
            Tag::Strong => "strong",
            Tag::Sub => "sub",
            Tag::Summary => "summary",
            Tag::Sup => "sup",
            Tag::Table => "table",
            Tag::Tbody => "tbody",
            Tag::Td => "td",
            Tag::Tfoot => "tfoot",
            Tag::Th => "th",
            Tag::Thead => "thead",
            Tag::Tr => "tr",
            Tag::U => "u",
            Tag::Ul => "ul",
        }
    }

    /// Does this tag break a line?
    ///
    /// The schema's own block/inline division, which every renderer over this
    /// tree shares: the plain-text walk turns a block into a line break and
    /// the popup's inline pass ends a paragraph at one. Structured content has
    /// no `display` property, so the mapping is fixed and needs no cascade.
    ///
    /// `td`, `th`, `thead` and `tbody` are in neither table on purpose: a cell
    /// is a grid problem rather than a line-break one, and the `tr` around it
    /// already breaks the row.
    pub fn is_block(self) -> bool {
        matches!(
            self,
            Tag::Div | Tag::Li | Tag::Ol | Tag::Ul | Tag::Table | Tag::Tr | Tag::Details
                | Tag::Summary
        )
    }

    /// Does this tag never break a line?
    ///
    /// The other half of [`is_block`](Self::is_block). Not its complement: a
    /// cell tag is in neither, and an emphasis tag such as `b` is inline by
    /// having nothing to say about lines at all. What this decides is where a
    /// run of bare strings under a node becomes lines rather than one flow.
    pub fn is_inline(self) -> bool {
        matches!(self, Tag::Span | Tag::A | Tag::Ruby | Tag::Rt | Tag::Rp | Tag::Img)
    }
}

/// A glossary item's `type` field.
///
/// Kept on every node, not just on the top-level items, because it is what
/// distinguishes a bare glossary string ([`ItemType::None`]) from a
/// `type: text` item ([`ItemType::Text`]). Both parse to one [`Kind::Text`]
/// node, and the plain-text renderer's "an array of plain strings is a list"
/// rule needs to tell them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ItemType {
    None,
    StructuredContent,
    Text,
    Image,
    /// Unrecognised. The name itself is kept as the node's `type` attribute.
    Other,
}

/// A resolved style property.
///
/// Every property `docs/research/dict-shapes.md` found in the corpus, plus
/// the schema-permitted siblings of those it found, so that a dictionary
/// outside the census loses nothing either. Resolving the key to an enum once
/// at parse time is the point: the `serde_json::Value` renderer re-resolved
/// every key on every walk, which is most of its 6.5x walk penalty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum StyleKey {
    FontStyle,
    FontWeight,
    FontSize,
    TextDecorationLine,
    TextDecorationStyle,
    TextDecorationColor,
    VerticalAlign,
    TextAlign,
    WhiteSpace,
    WordBreak,
    Cursor,
    ListStyleType,
    Color,
    BackgroundColor,
    BorderColor,
    BorderStyle,
    BorderRadius,
    BorderWidth,
    Margin,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    Padding,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
}

/// A node's classified editorial purpose - what the render settings filter
/// on.
///
/// A closed set of six, populated by [`parse`]'s classifier on every node.
/// There is no "unclassified": a node the classifier recognises nothing
/// about is [`Role::Content`], which every filter keeps. That is the failure
/// direction the evidence demands - the `data` namespace is per-dictionary
/// and unbounded, so an unrecognised node must render, never vanish.
///
/// Three of the six are droppable ([`RoleFilter`] has a knob for each) and
/// three are not. [`Role::Reference`] and [`Role::Commentary`] are
/// classified but never hidden: `docs/research/dict-shapes.md` counts 116 000
/// cross-reference and 184 000 commentary nodes across eight dictionaries
/// each, and no setting asks for them to go. Classifying them anyway makes
/// them addressable - a later ticket that adds a setting adds the knob and
/// not the classifier.
///
/// **The declaration order is the classification precedence**, lowest first,
/// and [`Ord`] is derived for exactly that: a node carrying two editorial
/// hooks takes the smaller role, so the answer does not depend on the order a
/// dictionary happened to write its `data` fields in. No node in the census
/// corpus carries two conflicting hooks, so the order only decides input the
/// corpus does not contain; it is chosen so the most destructive
/// misclassification is the one that cannot happen.
/// [`Role::PartOfSpeech`] wins because it alone *moves* content rather than
/// merely hiding it - the label is lifted into the card's own `pos` field, so
/// losing the lift loses the label everywhere, while misfiling a label as an
/// example only hides it from one panel.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[repr(u8)]
pub enum Role {
    /// A label surfaced as the card's own `pos` field rather than drawn
    /// inline. Yomitan's `data.content = "part-of-speech-info"`, 27 680
    /// census nodes, all Jitendex.
    PartOfSpeech,
    /// A source, licence, or footnote line.
    Attribution,
    /// An example sentence, its translation, its keyword, or its metadata.
    Example,
    /// A cross-reference: see-also, synonym, antonym. Not droppable.
    Reference,
    /// Editorial commentary: explanation, supplementary note, etymology.
    /// Not droppable.
    Commentary,
    /// Ordinary gloss content. The default, and the answer for every
    /// dictionary that uses none of the known conventions.
    #[default]
    Content,
}

/// One node. `Copy`, 44 bytes, no owned allocation.
///
/// The span fields are private because a span means nothing without the
/// document that owns the buffer it indexes; go through [`GlossDoc`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Node {
    pub kind: Kind,
    pub tag: Tag,
    pub item_type: ItemType,
    /// The node's editorial purpose, classified at parse time from its
    /// `data` map. Read by [`RoleFilter::allows`], which is the only gate
    /// any renderer over this tree consults.
    pub role: Role,
    /// Into the document's text buffer.
    text: Span,
    first_child: NodeId,
    next_sibling: NodeId,
    /// Into the document's `data_kv`.
    data: Span,
    /// Into the document's `attr_kv`.
    attrs: Span,
    /// Into the document's `style_kv` - the *resolved* style record.
    ///
    /// Two sources, merged by
    /// [`fold_declarations`](GlossDoc::fold_declarations): the dictionary's
    /// own `styles.css`, cascaded by `dict::sheet`, then the node's inline
    /// `style` verbatim. The two halves never name one property, because the
    /// fold drops every stylesheet declaration the inline record already
    /// sets - so inline `style` has the last word by construction, and this
    /// record reads the same whether a consumer takes the first occurrence
    /// of a property or the last. No renderer over this tree learns that CSS
    /// exists.
    style: Span,
}

/// An Entry's gloss content, flat.
#[derive(Clone, PartialEq, Debug)]
pub struct GlossDoc {
    nodes: Vec<Node>,
    /// First top-level glossary item, or [`NONE`]. Items are siblings.
    first_item: NodeId,
    text: String,
    data_kv: Vec<(KeyId, Scalar)>,
    attr_kv: Vec<(KeyId, Scalar)>,
    style_kv: Vec<(StyleKey, Scalar)>,
    /// Interned key text, by id.
    keys: Vec<Span>,
    /// Glossary items the parser could not read, each replaced by one text
    /// node holding whatever readable content the row still carried.
    degraded_items: u32,
    /// Subtrees cut off at [`parse::MAX_DEPTH`](MAX_DEPTH).
    truncated_subtrees: u32,
}

/// Node levels a parse will descend.
///
/// Counted the way `tools/dict-census` counts them - one level per object
/// whose `content` is descended, with array levels passing through, because
/// the arena flattens arrays. The census's deepest real tree is **15**
/// levels, so 48 is over three times the deepest content any of 97 archives
/// emits and exists only to bound pathological input.
///
/// It is also chosen to fire *before* `serde_json`'s own 128-level recursion
/// limit on the shape real content uses. Structured content spends two
/// `serde_json` levels per node level (an object and the array under its
/// `content`), so this cap trips at about level 97 of 128. A pathological
/// tree that nests arrays harder, or that runs hundreds of levels deep, can
/// still exhaust `serde_json` first - inside the `IgnoredAny` that consumes
/// the over-cap subtree, if nowhere else. Either way the parse terminates and
/// the levels above the cap render: the depth cap truncates, and per-item
/// isolation catches what the cap does not.
pub const MAX_DEPTH: u32 = 48;

impl Default for GlossDoc {
    fn default() -> Self {
        GlossDoc {
            nodes: Vec::new(),
            first_item: NONE,
            text: String::new(),
            data_kv: Vec::new(),
            attr_kv: Vec::new(),
            style_kv: Vec::new(),
            keys: Vec::new(),
            degraded_items: 0,
            truncated_subtrees: 0,
        }
    }
}

impl GlossDoc {
    /// No content at all.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Are there no glossary items?
    pub fn is_empty(&self) -> bool {
        self.first_item == NONE
    }

    /// Every node, in parse order. For a linear sweep - ticket 17's style
    /// probe walks this rather than the tree, which is where its 17.1x p99
    /// win comes from.
    pub fn all_nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The top-level glossary items, in order.
    pub fn items(&self) -> Siblings<'_> {
        Siblings { doc: self, next: self.first_item }
    }

    /// One node's children, in order.
    pub fn children(&self, id: NodeId) -> Siblings<'_> {
        Siblings { doc: self, next: self.nodes[id as usize].first_child }
    }

    /// Does this node have any children?
    pub fn has_children(&self, id: NodeId) -> bool {
        self.nodes[id as usize].first_child != NONE
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// A node's own text. Empty for anything but a text node.
    pub fn text(&self, id: NodeId) -> &str {
        self.span(self.nodes[id as usize].text)
    }

    /// The bytes a span addresses.
    pub fn span(&self, s: Span) -> &str {
        &self.text[s.at as usize..(s.at + s.len) as usize]
    }

    /// A node's `data` map, in source order.
    pub fn data(&self, id: NodeId) -> &[(KeyId, Scalar)] {
        Self::slice(&self.data_kv, self.nodes[id as usize].data)
    }

    /// A node's non-structural fields: `href`, `path`, `colSpan`, an image's
    /// `sizeUnits` and `appearance`, an unrecognised tag's own name, and
    /// anything else the schema grows.
    pub fn attrs(&self, id: NodeId) -> &[(KeyId, Scalar)] {
        Self::slice(&self.attr_kv, self.nodes[id as usize].attrs)
    }

    /// A node's resolved style record, in source order.
    pub fn style(&self, id: NodeId) -> &[(StyleKey, Scalar)] {
        Self::slice(&self.style_kv, self.nodes[id as usize].style)
    }

    fn slice<T>(all: &[T], s: Span) -> &[T] {
        &all[s.at as usize..(s.at + s.len) as usize]
    }

    /// An interned key's name.
    pub fn key(&self, k: KeyId) -> &str {
        self.span(self.keys[k as usize])
    }

    /// An interned key's id, or `None` when this document never saw the name.
    ///
    /// A linear scan: a document interns tens of keys, and resolving a probe
    /// set once per document then comparing `u32`s per node is the whole
    /// point of interning.
    pub fn key_id(&self, name: &str) -> Option<KeyId> {
        (0..self.keys.len() as KeyId).find(|&k| self.key(k) == name)
    }

    /// How many keys this document interned.
    ///
    /// The bound on a [`KeyId`], so a caller that resolves a whole probe set
    /// against a document - `dict::sheet` maps every interned key to the
    /// `data-*` attribute name it derives from, once - can size its table
    /// without walking the nodes.
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// One `data` entry by name.
    pub fn data_of(&self, id: NodeId, name: &str) -> Option<Scalar> {
        self.data(id).iter().find(|(k, _)| self.key(*k) == name).map(|(_, v)| *v)
    }

    /// One attribute by name.
    pub fn attr_of(&self, id: NodeId, name: &str) -> Option<Scalar> {
        self.attrs(id).iter().find(|(k, _)| self.key(*k) == name).map(|(_, v)| *v)
    }

    /// One resolved style property.
    pub fn style_of(&self, id: NodeId, key: StyleKey) -> Option<Scalar> {
        self.style(id).iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    /// A scalar's text, or `None` when it is a number, a boolean, or null.
    pub fn scalar_str(&self, v: Scalar) -> Option<&str> {
        match v {
            Scalar::Text(s) => Some(self.span(s)),
            _ => None,
        }
    }

    /// The tag's name, including an unrecognised one - which the parser kept
    /// as the node's `tag` attribute rather than throwing away.
    pub fn tag_name(&self, id: NodeId) -> &str {
        let node = &self.nodes[id as usize];
        match node.tag {
            Tag::Other => self.attr_of(id, "tag").and_then(|v| self.scalar_str(v)).unwrap_or(""),
            tag => tag.as_str(),
        }
    }

    /// Did this node come from a bare glossary string, as opposed to a
    /// `type: text` item or a tagged node?
    ///
    /// All three become one [`Kind::Text`] node so that downstream code has
    /// one shape to handle, but only the bare string counts towards the
    /// plain-text renderer's "an array of plain strings is a list" rule, and
    /// only it is emitted unwrapped by the HTML renderer.
    pub fn is_plain_string(&self, id: NodeId) -> bool {
        let n = &self.nodes[id as usize];
        n.kind == Kind::Text && n.tag == Tag::None && n.item_type == ItemType::None
    }

    /// Are this node's children more than one child, every one of them a bare
    /// glossary string?
    ///
    /// Yomitan's one exception to concatenating a node's children: such a run
    /// is a list and each string gets its own `<li>`, so both renderers over
    /// this tree that have lines - the plain-text walk and the popup's inline
    /// pass - break between them. An array mixing strings with nodes is prose
    /// broken up by its own inline markup ("see also", then a link, then more
    /// text) and is not a list.
    ///
    /// The arena flattens every array level under `content` into one child
    /// list, so the test is on the child list rather than on one JSON array.
    /// The two agree on every shape the census contains; they differ only
    /// when a bare string sits beside a nested string array under one
    /// `content`, which no corpus dictionary does.
    pub fn is_string_list(&self, id: NodeId) -> bool {
        let mut n = 0;
        for child in self.children(id) {
            if !self.is_plain_string(child) {
                return false;
            }
            n += 1;
        }
        n > 1
    }

    /// `data.content`, when it is a string. A non-string marker names no
    /// editorial rule, but it does still mark a block - see
    /// [`has_marker`](Self::has_marker).
    pub fn marker(&self, id: NodeId) -> Option<&str> {
        self.data_of(id, "content").and_then(|v| self.scalar_str(v))
    }

    /// `data.content` present and not null - which is what makes a node a
    /// block even when its tag is inline. A dictionary that tags a sense with
    /// `data: {"content": "sense"}` on a `span` still gets its own line.
    pub fn has_marker(&self, id: NodeId) -> bool {
        matches!(self.data_of(id, "content"), Some(v) if v != Scalar::Null)
    }

    /// Does this node's content read as one run of prose - is any child a
    /// bare string with visible text?
    ///
    /// Half of the exemption to the marker line break, shared by the
    /// plain-text walk and the popup's inline pass; the other half is
    /// [`inline_prose`](Self::inline_prose). A marker separates *senses*,
    /// and a sense list is marked nodes side by side; a marked inline node
    /// beside bare sentence text is markup *inside* a sentence, and
    /// breaking there cuts the sentence in two. Measured over the live
    /// database: every mid-prose marked inline node Jitendex writes is
    /// sentence markup (`example-keyword` 51 062, `lang-source-wasei`
    /// 4 114, `registered-trademark` 44), and every sense-separating one
    /// (`part-of-speech-info`, `forms-label`, `misc-info`, ...) has no
    /// bare-text sibling.
    ///
    /// Asked of the *parent*, because the arena links first-child /
    /// next-sibling and a node cannot see the text before it.
    pub fn prose(&self, id: NodeId) -> bool {
        self.children(id)
            .any(|c| self.is_plain_string(c) && !self.text(c).trim().is_empty())
    }

    /// Does this node itself read as a prose fragment - no block tag, no
    /// marker, and visible text somewhere under it?
    ///
    /// The other half of the marker exemption. [`prose`](Self::prose) sees
    /// only *bare* sentence text, and Jitendex wraps an example's
    /// translation whole (`span lang="en"`), so the footnote mark trailing
    /// it (`data.content = "attribution-footnote"`) broke onto a line of
    /// its own. A marked node *after* such a fragment trails a sentence,
    /// and both walks exempt it exactly as they exempt one amid bare text.
    ///
    /// Order is load-bearing where `prose` is symmetric: a sense head is
    /// marked pills *followed* by a prose fragment (Jitendex's
    /// forms-restriction `span`), so only prose already seen can be
    /// evidence - counting a fragment behind the marked node would re-read
    /// every such sense head as a sentence. Measured over the 97-archive
    /// corpus: the marked nodes that trail a fragment are
    /// `attribution-footnote` (9 784) and nothing else, while every sense
    /// separator leads its parent and keeps its break
    /// (`part-of-speech-info` 392 375, `forms-label` 146 307, `misc-info`
    /// 67 862, `reference-label` 56 466, `field-info` 45 143, ...).
    pub fn inline_prose(&self, id: NodeId) -> bool {
        !self.nodes[id as usize].tag.is_block()
            && !self.has_marker(id)
            && self.has_visible_text(id)
    }

    /// Any visible text under this node.
    fn has_visible_text(&self, id: NodeId) -> bool {
        if self.nodes[id as usize].kind == Kind::Text {
            return !self.text(id).trim().is_empty();
        }
        self.children(id).any(|c| self.has_visible_text(c))
    }

    /// This node's classified editorial purpose.
    ///
    /// The whole of what the old name-matched drop list used to answer, and
    /// now the answer for every dictionary rather than for the one whose
    /// six `data.content` spellings the list happened to hold.
    pub fn role(&self, id: NodeId) -> Role {
        self.nodes[id as usize].role
    }

    /// Glossary items that failed to parse and were degraded to text.
    pub fn degraded_items(&self) -> u32 {
        self.degraded_items
    }

    /// Subtrees cut off at [`MAX_DEPTH`].
    pub fn truncated_subtrees(&self) -> u32 {
        self.truncated_subtrees
    }

    /// Retained bytes, by the document's own accounting. Reported next to a
    /// counting allocator's number as a cross-check; see
    /// `examples/gloss_doc_alloc.rs`.
    pub fn footprint(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.text.capacity()
            + self.data_kv.capacity() * std::mem::size_of::<(KeyId, Scalar)>()
            + self.attr_kv.capacity() * std::mem::size_of::<(KeyId, Scalar)>()
            + self.style_kv.capacity() * std::mem::size_of::<(StyleKey, Scalar)>()
            + self.keys.capacity() * std::mem::size_of::<Span>()
    }

    /// Folds a dictionary's own `styles.css` into the resolved style
    /// records.
    ///
    /// `wins` is `(node, property, value)` in ascending node order, already
    /// cascaded by `dict::sheet`: one entry per property per node, and never
    /// a property the node's inline `style` already sets. Each node's new
    /// record is its stylesheet winners followed by its inline record
    /// verbatim, which is what makes inline `style` the last word - a record
    /// is read first-occurrence-wins, so priority runs left to right, and
    /// the two halves cannot collide anyway.
    ///
    /// One rebuild of the pool rather than a splice per node, because a
    /// record is a span into one shared vector and inserting in the middle
    /// of it would move every span after the insertion.
    pub(crate) fn fold_declarations(&mut self, wins: &[(NodeId, StyleKey, &str)]) {
        let mut kv: Vec<(StyleKey, Scalar)> =
            Vec::with_capacity(self.style_kv.len() + wins.len());
        let mut next = 0usize;
        for id in 0..self.nodes.len() {
            let at = kv.len() as u32;
            while let Some((node, key, value)) = wins.get(next) {
                if *node as usize != id {
                    break;
                }
                let span = self.push_text(value);
                kv.push((*key, Scalar::Text(span)));
                next += 1;
            }
            let inline = self.nodes[id].style;
            let from = inline.at as usize;
            kv.extend_from_slice(&self.style_kv[from..from + inline.len as usize]);
            self.nodes[id].style = Span { at, len: kv.len() as u32 - at };
        }
        self.style_kv = kv;
    }

    /// One string into the document's text buffer.
    ///
    /// No deduplication: a stylesheet value is a handful of bytes, the nodes
    /// one sheet reaches are tens rather than thousands, and a scan of the
    /// buffer would cost more than the bytes it saved.
    fn push_text(&mut self, text: &str) -> Span {
        let at = self.text.len() as u32;
        self.text.push_str(text);
        Span { at, len: text.len() as u32 }
    }
}

/// A node and its following siblings.
pub struct Siblings<'a> {
    doc: &'a GlossDoc,
    next: NodeId,
}

impl Iterator for Siblings<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.next == NONE {
            return None;
        }
        let id = self.next;
        self.next = self.doc.nodes[id as usize].next_sibling;
        Some(id)
    }
}

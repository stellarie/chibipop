//! `GlossDoc`: gloss content of an Entry as one flat, typed tree.
//!
//! The tree uses a flat arena, not a box tree of enums with `Vec` children.
//! Parse speed did not drive this choice.
//! `examples/gloss_arena_bench.rs` tested three candidates:
//! `serde_json::Value`, a box tree, and this arena.
//! It checked byte-identical walks over 5 200 real payloads.
//! `docs/research/lookup-cost.md` records these values:
//!
//! | per hover, p50 | parse µs | walk µs | allocs | retained B/node | style probe µs |
//! |---|---:|---:|---:|---:|---:|
//! | `serde_json::Value` | 94.8 | 9.8 | 3 745 | 678.2 | 11.1 |
//! | box tree | 81.5 | 1.6 | 3 085 | 510.9 | 7.8 |
//! | **flat arena** | **78.8** | **1.5** | **383** | **106.5** | **2.2** |
//!
//! The arena saves 2.7 µs over a box tree during parse.
//! That difference is noise.
//! The other four values matter.
//! The arena uses 8.1x fewer allocations and retains 4.6x less heap.
//! It frees heap 12.1x faster.
//! Its per-node `data-*` probe runs 3.6x to 17.1x faster.
//!
//! The first three values matter because `SqliteDictionary` caches this tree.
//! The last value matters because the stylesheet matcher probes each node's `data` map
//! on every hover.
//!
//! The layout has only these parts:
//!
//! - One `Vec<Node>` of `Copy` structs.
//!   Each node points to its children with a first-child index and a next-sibling index.
//!   A subtree has no separate allocation.
//! - One `String` stores all text bytes: node text, attribute values, `data`
//!   values, and interned key names. `(u32, u32)` spans address this buffer.
//! - Interned `data` and attribute keys each have a `u32` id.
//!   A probe compares two integers. It does not hash a string.
//! - Three flat vectors store each node's `data`, attribute, and resolved-style
//!   entries in contiguous runs. A span addresses each run.
//!
//! Each document has six vectors and one string.
//! A clone calls `memcpy` six times.
//! A drop calls six frees.
//! Tree size does not change these counts.
//!
//! The tree preserves the Yomitan structured-content schema.
//! An unrecognized tag becomes [`Tag::Other`].
//! The tree keeps its name as an attribute and keeps its children.
//! An unrecognized `type` and every non-structural field stay as attributes.
//! This lossless shape supports later renderer and parser changes.
//!
//! The record stores the Dictionary's raw glossary JSON, and the parser runs on every hover.
//! Selection addresses use document preorder paths and UTF-8 byte offsets.
//! `NodePath::END` is the address after every node, so a range can use one
//! sentinel instead of a document-specific final leaf.

//! A parser or renderer fix therefore ships as a patch, not a Dictionary rebuild.

mod html;
mod parse;
mod path;
mod plain;
mod select;


#[cfg(test)]
mod tests;

pub use html::{render_html, RoleFilter};
pub(crate) use parse::tag_for;
pub use path::{NodePath, Selection};
pub use plain::{plain_items, plain_selected, pos_labels, renders_text};
pub use select::{
    extent, grapheme_range, leaf_text, leaves, sense_range, snap_ceil, snap_floor, DocAddr,
    DocRange, Leaf, Separator,
};

/// An index into the node vector of [`GlossDoc`].
pub type NodeId = u32;

/// An interned key name for `data` or attributes.
pub type KeyId = u32;

/// The absent index.
///
/// This constant is `u32::MAX`, not `Option<u32>`, so a [`Node`] stays 44 bytes.
/// Rust has no niche-optimized `Option<u32>`.
/// A `u64` index adds 20 bytes to every node.
/// A document cannot hold four billion nodes.
pub const NONE: NodeId = u32::MAX;

/// A byte range in the text buffer of [`GlossDoc`] or an element range in one of its
/// key-value vectors.
///
/// The field that holds the span defines its target.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Span {
    pub at: u32,
    pub len: u32,
}

/// A stored `data`, attribute, or style value.
///
/// Numbers and booleans keep their JSON types.
/// The parser does not format them into the text buffer.
/// The schema uses numbers as em multipliers and span counts.
/// A format step during parse adds an `f64` printer to parse work without benefit.
///
/// The parser joins an array of strings with one space.
/// That join defines the list form of `textDecorationLine`.
/// The HTML renderer once repeated this join on every walk.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scalar {
    Text(Span),
    Num(f64),
    Bool(bool),
    Null,
}

/// The node class that renderers use when exact tag identity does not matter.
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
    /// A tag that this build does not know.
    ///
    /// The node keeps its children.
    /// The renderer drops the wrapper and keeps the text.
    Unknown,
}

/// The exact tag name.
///
/// The tree stores this name with [`Kind`] because `div` and `span` are both containers.
/// A lossless tree must retain the original tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Tag {
    /// This value represents a bare glossary string, a `type: text` item, or a wrapper object with
    /// only `content`.
    None,
    /// An unrecognized tag. The node stores its name in the `tag` attribute.
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
    /// Returns the schema tag name, or `""` for [`Tag::None`] and [`Tag::Other`].
    ///
    /// [`Tag::None`] and [`Tag::Other`] have no static schema name.
    /// Call [`GlossDoc::tag_name`] when you need an unrecognized name.
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

    /// Returns true when this tag breaks a line.
    ///
    /// This split defines the block and inline groups in the schema.
    /// Every renderer over this tree shares the split.
    /// The plain-text walk turns a block into a line break.
    /// The popup inline pass ends a paragraph at a block.
    /// Structured content has no `display` property, so the mapping is fixed.
    /// The mapping needs no cascade.
    ///
    /// `td`, `th`, `thead`, and `tbody` belong to neither group.
    /// A cell is a grid element, not a line-break element.
    /// The surrounding `tr` already breaks the row.
    pub fn is_block(self) -> bool {
        matches!(
            self,
            Tag::Div | Tag::Li | Tag::Ol | Tag::Ul | Tag::Table | Tag::Tr | Tag::Details
                | Tag::Summary
        )
    }

    /// Returns true when this tag never breaks a line.
    ///
    /// This method is not the complement of [`is_block`](Self::is_block).
    /// A cell tag belongs to neither group.
    /// An emphasis tag such as `b` is inline because it says nothing about lines.
    /// This method decides when bare strings under a node become separate lines.
    /// Otherwise they stay in one flow.
    pub fn is_inline(self) -> bool {
        matches!(self, Tag::Span | Tag::A | Tag::Ruby | Tag::Rt | Tag::Rp | Tag::Img)
    }
}

/// The `type` field of a glossary item.
///
/// Every node keeps this field, not only top-level items.
/// It separates a bare glossary string ([`ItemType::None`]) from a `type: text` item
/// ([`ItemType::Text`]).
/// Both forms parse to one [`Kind::Text`] node.
/// The plain-text renderer needs this distinction because an array of plain strings is a list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ItemType {
    None,
    StructuredContent,
    Text,
    Image,
    /// An unrecognized type. The node stores its name in the `type` attribute.
    Other,
}

/// A resolved style property.
///
/// This enum holds each property that `docs/research/dict-shapes.md` found in the corpus.
/// It also holds schema-permitted siblings, so a Dictionary outside the census keeps its
/// properties.
/// The parser resolves each key name to an enum variant once.
/// The `serde_json::Value` renderer resolved each key on every walk.
/// That repeated lookup caused most of its 6.5x walk penalty.
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

/// The editorial purpose that renderers use to filter a node.
///
/// The parser assigns one of six roles to every node.
/// No unclassified state exists.
/// An unknown node receives [`Role::Content`], which all filters keep.
/// The `data` namespace is dynamic and unconstrained.
/// Unknown nodes must therefore render, not disappear.
///
/// Three roles are optional. [`RoleFilter`] provides one control for each.
/// Three roles are permanent.
/// [`Role::Reference`] and [`Role::Commentary`] are classified but not hidden.
/// [`Role::Content`] is the default and always stays.
/// Census data shows many cross-reference and commentary nodes, and no filter hides them.
/// A filter can add controls for these roles without a classifier change.
///
/// Declaration order defines classification precedence from low to high.
/// [`Ord`] uses this order.
/// When a node matches two hooks, it receives the lower-value role.
/// This makes output deterministic despite source attribute order.
///
/// [`Role::PartOfSpeech`] has highest precedence.
/// It moves content into the note `pos` field.
/// If this classification fails, the whole card loses the label.
/// If the classifier marks a label as an example, the panel hides it but the card keeps it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[repr(u8)]
pub enum Role {
    /// A part-of-speech label for the note `pos` field instead of inline text.
    /// Yomitan sets `data.content = "part-of-speech-info"`.
    PartOfSpeech,
    /// A source, license, or footnote line.
    Attribution,
    /// An example sentence, translation, keyword, or metadata.
    Example,
    /// A cross-reference such as a see-also link, synonym, or antonym.
    /// Filters always keep this role.
    Reference,
    /// Editorial commentary such as an explanation, note, or etymology.
    /// Filters always keep this role.
    Commentary,
    /// Standard gloss content. This is the default role for unrecognized nodes.
    #[default]
    Content,
}

/// A single node.
///
/// The struct is `Copy`, uses 44 bytes, and owns no heap memory.
///
/// Span fields are private because a span needs the parent document text buffer.
/// Access text through [`GlossDoc`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Node {
    pub kind: Kind,
    pub tag: Tag,
    pub item_type: ItemType,
    /// The node role. The parser classifies it from the `data` map.
    /// [`RoleFilter::allows`] reads it at render time.
    pub role: Role,
    /// Byte offset and length within the document text buffer.
    text: Span,
    first_child: NodeId,
    next_sibling: NodeId,
    /// Range in the `data_kv` vector of the document.
    data: Span,
    /// Range in the `attr_kv` vector of the document.
    attrs: Span,
    /// Range in the `style_kv` vector for resolved style properties.
    ///
    /// [`fold_declarations`](GlossDoc::fold_declarations) combines two style sources:
    /// the Dictionary `styles.css` from `dict::sheet` and the node's inline `style` attribute.
    /// The merge drops stylesheet declarations when an inline style sets the same property.
    /// Inline styles take precedence.
    /// Downstream renderers do not process CSS rules directly.
    style: Span,
}

/// The gloss content of an Entry, stored as a flat tree.
#[derive(Clone, PartialEq, Debug)]
pub struct GlossDoc {
    nodes: Vec<Node>,
    /// The first top-level glossary item, or [`NONE`]. Items are sibling nodes.
    first_item: NodeId,
    text: String,
    data_kv: Vec<(KeyId, Scalar)>,
    attr_kv: Vec<(KeyId, Scalar)>,
    style_kv: Vec<(StyleKey, Scalar)>,
    /// Interned key text by identifier.
    keys: Vec<Span>,
    /// Number of invalid glossary items that became plain-text nodes.
    degraded_items: u32,
    /// Number of subtrees truncated at [`MAX_DEPTH`].
    truncated_subtrees: u32,
}

/// Maximum node depth for parser traversal.
///
/// Depth increases when the parser enters an object `content` field.
/// Array levels do not increase depth because the arena flattens them.
/// Census analysis found a maximum real depth of 15 levels.
/// The limit of 48 levels bounds unusually deep input safely.
///
/// This limit stops recursion before `serde_json` reaches its 128-level limit.
/// Real structured content uses two `serde_json` levels per node level: an object and its child
/// array.
/// The limit activates near level 97 of 128.
/// If input exceeds this depth, the parser truncates the subtree and renders the parent levels.
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
    /// Creates an empty document.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns true if the document contains no glossary items.
    pub fn is_empty(&self) -> bool {
        self.first_item == NONE
    }

    /// Returns all nodes in parse order.
    ///
    /// This slice supports a linear walk.
    /// The style probe scans it instead of the tree hierarchy for better cache performance.
    pub fn all_nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Returns an iterator over top-level glossary items.
    pub fn items(&self) -> Siblings<'_> {
        Siblings { doc: self, next: self.first_item }
    }

    /// Returns an iterator over the direct children of a node.
    pub fn children(&self, id: NodeId) -> Siblings<'_> {
        Siblings { doc: self, next: self.nodes[id as usize].first_child }
    }

    /// Returns true if the node contains children.
    pub fn has_children(&self, id: NodeId) -> bool {
        self.nodes[id as usize].first_child != NONE
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Returns the text of a node, or an empty string if the node is not text.
    pub fn text(&self, id: NodeId) -> &str {
        self.span(self.nodes[id as usize].text)
    }

    /// Returns the text addressed by a span.
    pub fn span(&self, s: Span) -> &str {
        &self.text[s.at as usize..(s.at + s.len) as usize]
    }

    /// Returns the `data` map entries of a node in source order.
    pub fn data(&self, id: NodeId) -> &[(KeyId, Scalar)] {
        Self::slice(&self.data_kv, self.nodes[id as usize].data)
    }

    /// Returns non-structural attributes of a node.
    ///
    /// These include `href`, `path`, `colSpan`, image properties, and unrecognized tag names.
    pub fn attrs(&self, id: NodeId) -> &[(KeyId, Scalar)] {
        Self::slice(&self.attr_kv, self.nodes[id as usize].attrs)
    }

    /// Returns the resolved style properties of a node in source order.
    pub fn style(&self, id: NodeId) -> &[(StyleKey, Scalar)] {
        Self::slice(&self.style_kv, self.nodes[id as usize].style)
    }

    fn slice<T>(all: &[T], s: Span) -> &[T] {
        &all[s.at as usize..(s.at + s.len) as usize]
    }

    /// Returns the name of an interned key.
    pub fn key(&self, k: KeyId) -> &str {
        self.span(self.keys[k as usize])
    }

    /// Returns the key identifier, or `None` when the document does not contain the key.
    ///
    /// This method uses a linear scan because a document interns few keys.
    /// Callers resolve keys once per document, then compare integer identifiers.
    pub fn key_id(&self, name: &str) -> Option<KeyId> {
        (0..self.keys.len() as KeyId).find(|&k| self.key(k) == name)
    }

    /// Returns the number of interned keys in this document.
    ///
    /// This number bounds [`KeyId`] values.
    /// Callers such as `dict::sheet` use it to allocate tables before they handle nodes.
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Returns a `data` entry value by key name.
    pub fn data_of(&self, id: NodeId, name: &str) -> Option<Scalar> {
        self.data(id).iter().find(|(k, _)| self.key(*k) == name).map(|(_, v)| *v)
    }

    /// Returns an attribute value by key name.
    pub fn attr_of(&self, id: NodeId, name: &str) -> Option<Scalar> {
        self.attrs(id).iter().find(|(k, _)| self.key(*k) == name).map(|(_, v)| *v)
    }

    /// Returns a resolved style property value.
    pub fn style_of(&self, id: NodeId, key: StyleKey) -> Option<Scalar> {
        self.style(id).iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    /// Returns the string value of a scalar, or `None` if the scalar is not text.
    pub fn scalar_str(&self, v: Scalar) -> Option<&str> {
        match v {
            Scalar::Text(s) => Some(self.span(s)),
            _ => None,
        }
    }

    /// Returns the tag name of a node.
    ///
    /// For an unrecognized tag, this method returns the stored `tag` attribute.
    pub fn tag_name(&self, id: NodeId) -> &str {
        let node = &self.nodes[id as usize];
        match node.tag {
            Tag::Other => self.attr_of(id, "tag").and_then(|v| self.scalar_str(v)).unwrap_or(""),
            tag => tag.as_str(),
        }
    }

    /// Returns true when a bare glossary string produced this node.
    ///
    /// Bare strings, `type: text` items, and tagged nodes all produce [`Kind::Text`] nodes.
    /// Only bare strings make the plain-text renderer format them as a list.
    /// The HTML renderer emits bare strings without wrapper tags.
    pub fn is_plain_string(&self, id: NodeId) -> bool {
        let n = &self.nodes[id as usize];
        n.kind == Kind::Text && n.tag == Tag::None && n.item_type == ItemType::None
    }

    /// Returns true when this node has multiple children that are all bare strings.
    ///
    /// Yomitan treats a sequence of bare strings as a list.
    /// The plain-text renderer and popup inline pass add line breaks between them.
    /// An array that mixes strings and nodes represents continuous text with inline markup, not a
    /// list.
    ///
    /// The arena flattens array levels under `content` into one child list.
    /// This method checks the child list, not JSON array boundaries.
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

    /// Returns the string value of `data.content`, if present.
    ///
    /// A non-string value defines no editorial role but marks a block boundary.
    /// See [`has_marker`](Self::has_marker).
    pub fn marker(&self, id: NodeId) -> Option<&str> {
        self.data_of(id, "content").and_then(|v| self.scalar_str(v))
    }

    /// Returns true when `data.content` exists and is not null.
    ///
    /// A node with this marker forms a block even when its tag is inline.
    /// For example, a `span` with `data: {"content": "sense"}` starts a new line.
    pub fn has_marker(&self, id: NodeId) -> bool {
        matches!(self.data_of(id, "content"), Some(v) if v != Scalar::Null)
    }

    /// Returns true when any child is a non-empty bare string.
    ///
    /// This condition exempts continuous prose from marker line breaks.
    /// The plain-text renderer and popup inline pass use this condition.
    /// Senses appear as adjacent marked nodes.
    /// An inline marked node next to bare sentence text is inline markup, not a sense boundary.
    /// A line break there breaks the sentence.
    pub fn prose(&self, id: NodeId) -> bool {
        self.children(id)
            .any(|c| self.is_plain_string(c) && !self.text(c).trim().is_empty())
    }

    /// Returns true when this node contains visible text without a block tag or marker.
    ///
    /// This method provides the other half of marker exemption.
    /// [`prose`](Self::prose) detects only bare text siblings.
    /// Dictionaries often wrap translations in elements such as `span lang="en"`.
    /// A marked node after such an element belongs to the sentence.
    /// Both renderers keep it on the same line.
    ///
    /// Earlier prose serves as evidence.
    /// In sense headers, marked labels precede prose fragments.
    /// The parser checks only earlier text to prevent false matches on sense headers.
    pub fn inline_prose(&self, id: NodeId) -> bool {
        !self.nodes[id as usize].tag.is_block()
            && !self.has_marker(id)
            && self.has_visible_text(id)
    }

    /// Returns true when this node or any descendant contains non-whitespace text.
    fn has_visible_text(&self, id: NodeId) -> bool {
        if self.nodes[id as usize].kind == Kind::Text {
            return !self.text(id).trim().is_empty();
        }
        self.children(id).any(|c| self.has_visible_text(c))
    }

    /// Returns the classified editorial purpose of this node.
    ///
    /// This role replaces hardcoded attribute lists.
    /// It gives every Dictionary the same classification.
    pub fn role(&self, id: NodeId) -> Role {
        self.nodes[id as usize].role
    }

    /// Returns the count of invalid glossary items converted to plain text.
    pub fn degraded_items(&self) -> u32 {
        self.degraded_items
    }

    /// Returns the count of subtrees truncated at [`MAX_DEPTH`].
    pub fn truncated_subtrees(&self) -> u32 {
        self.truncated_subtrees
    }

    /// Returns allocated memory in bytes for this document.
    ///
    /// `examples/gloss_doc_alloc.rs` uses this method to check heap use.
    pub fn footprint(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.text.capacity()
            + self.data_kv.capacity() * std::mem::size_of::<(KeyId, Scalar)>()
            + self.attr_kv.capacity() * std::mem::size_of::<(KeyId, Scalar)>()
            + self.style_kv.capacity() * std::mem::size_of::<(StyleKey, Scalar)>()
            + self.keys.capacity() * std::mem::size_of::<Span>()
    }

    /// Folds Dictionary stylesheet rules into resolved style records.
    ///
    /// The `wins` slice contains `(node, property, value)` tuples in ascending node order from
    /// `dict::sheet`.
    /// It has one winning declaration per property per node.
    /// It excludes properties that inline styles already set.
    ///
    /// The method appends stylesheet properties before inline properties.
    /// Readers use first-occurrence-wins lookup, so inline styles retain priority.
    /// This operation rebuilds the shared vector once, so slice spans never shift.
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

    /// Appends a string to the document text buffer and returns its span.
    ///
    /// This method does not deduplicate text.
    /// A buffer scan costs more than the saved memory.
    fn push_text(&mut self, text: &str) -> Span {
        let at = self.text.len() as u32;
        self.text.push_str(text);
        Span { at, len: text.len() as u32 }
    }
}

/// An iterator over a node and its next siblings.
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

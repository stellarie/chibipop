//! The parse: `serde_json`'s tokenizer straight into the arena.
//!
//! Hand-written `DeserializeSeed`/`Visitor` seeds, with **no intermediate
//! `serde_json::Value`**. The prototype in `examples/gloss_arena_bench.rs`
//! measures `Value` at 6.5x the walk cost and 6x the retained bytes of this
//! shape, because a `Value` allocates a map per object and a `String` per key
//! and then re-resolves every style key on every walk.
//!
//! Two robustness rules, and they are separate on purpose:
//!
//! - **Per-item isolation.** The glossary is split into its top-level items
//!   before parsing, and each item is parsed on its own tokenizer. One
//!   unreadable item costs that item alone; every other item still renders.
//!   Yomitan and Hoshi Reader both isolate per row for the same reason: a
//!   depth cap does not cover a bad node. An item that fails keeps every
//!   node the tokenizer did produce and loses only the wrapper it could not
//!   finish reading - the same policy [`Kind::Unknown`] applies to a tag this
//!   build does not know.
//! - **A depth cap** at [`MAX_DEPTH`], which bounds pathological *nesting*
//!   rather than pathological syntax.
//!
//! Nothing here can panic on input, and nothing here returns an error:
//! [`GlossDoc::parse`] is total. Every seed accepts every JSON type a
//! dictionary could put in a field, so a schema-malformed node - `content` as
//! a number, `text` as an object, `style` as a string - is stored or skipped
//! rather than failing. Only broken JSON *syntax* reaches the per-item
//! fallback.

use super::{
    GlossDoc, ItemType, Kind, KeyId, Node, NodeId, Role, Scalar, Span, StyleKey, Tag, MAX_DEPTH,
    NONE,
};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::HashMap;
use std::fmt;

impl GlossDoc {
    /// Parses a dictionary's raw structured-content glossary.
    ///
    /// Total by construction - see the module comment. An empty or
    /// unparseable payload yields an empty document, never an error, because
    /// the caller is a hover on the frame budget and has nothing useful to do
    /// with a `Result`.
    pub fn parse(json: &str) -> GlossDoc {
        let mut b = Builder::new();
        let mut last = NONE;
        for item in top_level_items(json) {
            b.item(item, &mut last);
        }
        b.doc
    }
}

// ---------------------------------------------------------------------------
// schema vocabulary
// ---------------------------------------------------------------------------

fn tag_for(s: &str) -> Option<Tag> {
    Some(match s {
        "a" => Tag::A,
        "b" => Tag::B,
        "big" => Tag::Big,
        "br" => Tag::Br,
        "code" => Tag::Code,
        "details" => Tag::Details,
        "div" => Tag::Div,
        "em" => Tag::Em,
        "i" => Tag::I,
        "img" => Tag::Img,
        "li" => Tag::Li,
        "ol" => Tag::Ol,
        "pre" => Tag::Pre,
        "rp" => Tag::Rp,
        "rt" => Tag::Rt,
        "ruby" => Tag::Ruby,
        "s" => Tag::S,
        "small" => Tag::Small,
        "span" => Tag::Span,
        "strong" => Tag::Strong,
        "sub" => Tag::Sub,
        "summary" => Tag::Summary,
        "sup" => Tag::Sup,
        "table" => Tag::Table,
        "tbody" => Tag::Tbody,
        "td" => Tag::Td,
        "tfoot" => Tag::Tfoot,
        "th" => Tag::Th,
        "thead" => Tag::Thead,
        "tr" => Tag::Tr,
        "u" => Tag::U,
        "ul" => Tag::Ul,
        _ => return None,
    })
}

fn item_type_for(s: &str) -> Option<ItemType> {
    Some(match s {
        "structured-content" => ItemType::StructuredContent,
        "text" => ItemType::Text,
        "image" => ItemType::Image,
        _ => return None,
    })
}

fn style_key_for(s: &str) -> Option<StyleKey> {
    Some(match s {
        "fontStyle" => StyleKey::FontStyle,
        "fontWeight" => StyleKey::FontWeight,
        "fontSize" => StyleKey::FontSize,
        "textDecorationLine" => StyleKey::TextDecorationLine,
        "textDecorationStyle" => StyleKey::TextDecorationStyle,
        "textDecorationColor" => StyleKey::TextDecorationColor,
        "verticalAlign" => StyleKey::VerticalAlign,
        "textAlign" => StyleKey::TextAlign,
        "whiteSpace" => StyleKey::WhiteSpace,
        "wordBreak" => StyleKey::WordBreak,
        "cursor" => StyleKey::Cursor,
        "listStyleType" => StyleKey::ListStyleType,
        "color" => StyleKey::Color,
        "backgroundColor" => StyleKey::BackgroundColor,
        "borderColor" => StyleKey::BorderColor,
        "borderStyle" => StyleKey::BorderStyle,
        "borderRadius" => StyleKey::BorderRadius,
        "borderWidth" => StyleKey::BorderWidth,
        "margin" => StyleKey::Margin,
        "marginTop" => StyleKey::MarginTop,
        "marginRight" => StyleKey::MarginRight,
        "marginBottom" => StyleKey::MarginBottom,
        "marginLeft" => StyleKey::MarginLeft,
        "padding" => StyleKey::Padding,
        "paddingTop" => StyleKey::PaddingTop,
        "paddingRight" => StyleKey::PaddingRight,
        "paddingBottom" => StyleKey::PaddingBottom,
        "paddingLeft" => StyleKey::PaddingLeft,
        _ => return None,
    })
}

/// The six structural fields. Everything else on a node is an attribute, and
/// is kept as one - `href`, `path`, `colSpan`, an image's `sizeUnits`, and
/// whatever the schema grows next.
enum FieldName {
    Tag,
    Type,
    Text,
    Content,
    Data,
    Style,
    Other,
}

fn field_name(s: &str) -> FieldName {
    match s {
        "tag" => FieldName::Tag,
        "type" => FieldName::Type,
        "text" => FieldName::Text,
        "content" => FieldName::Content,
        "data" => FieldName::Data,
        "style" => FieldName::Style,
        _ => FieldName::Other,
    }
}

/// A node's kind, from its tag and its `type`.
///
/// `div` and `span` are both containers, `table`/`thead`/`tbody`/`tfoot` are
/// all table scaffolding, and `ruby`/`rt`/`rp` are all ruby parts - the exact
/// tag stays on the node, so collapsing them here loses nothing.
fn kind_of(tag: Tag, ty: ItemType, has_text: bool) -> Kind {
    match tag {
        Tag::Br => Kind::Break,
        Tag::Ul | Tag::Ol => Kind::List,
        Tag::Li => Kind::ListItem,
        Tag::Table | Tag::Thead | Tag::Tbody | Tag::Tfoot => Kind::Table,
        Tag::Tr => Kind::Row,
        Tag::Td | Tag::Th => Kind::Cell,
        Tag::Ruby | Tag::Rt | Tag::Rp => Kind::Ruby,
        Tag::A => Kind::Link,
        Tag::Img => Kind::Image,
        Tag::Other => Kind::Unknown,
        Tag::None => match ty {
            ItemType::Image => Kind::Image,
            // A plain string and a `type: text` item both land here, so
            // downstream code has one shape to handle.
            ItemType::Text => Kind::Text,
            ItemType::StructuredContent => Kind::Container,
            _ if has_text => Kind::Text,
            _ => Kind::Unknown,
        },
        _ => Kind::Container,
    }
}

// ---------------------------------------------------------------------------
// splitting the glossary into items
// ---------------------------------------------------------------------------

/// The glossary's top-level items, as byte slices of the payload.
///
/// A bracket- and quote-aware scan, not a parse: it exists so that one
/// unreadable item cannot take the rest of the entry with it, which a single
/// `serde_json` tokenizer over the whole array cannot offer - a syntax error
/// leaves the tokenizer's position unrecoverable.
///
/// A payload that is not an array is returned as one item, which the item
/// parser then rejects unless it is a string or an object. That matches the
/// schema: a glossary is an array of strings and objects, and nothing else.
fn top_level_items(json: &str) -> Vec<&str> {
    let b = json.as_bytes();
    let mut i = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    if i >= b.len() || b[i] != b'[' {
        return vec![json.trim()];
    }
    i += 1;

    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth = 0u32;
    let mut in_str = false;
    let mut escaped = false;
    let mut closed = false;

    while i < b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                start = start.or(Some(i));
                in_str = true;
            }
            b'[' | b'{' => {
                start = start.or(Some(i));
                depth += 1;
            }
            b']' | b'}' if depth == 0 => {
                // The glossary array's own close. Anything after it is
                // trailing garbage and is not an item.
                push_item(&mut out, json, start, i);
                closed = true;
                break;
            }
            b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                push_item(&mut out, json, start, i);
                start = None;
            }
            _ if c.is_ascii_whitespace() => {}
            _ => start = start.or(Some(i)),
        }
        i += 1;
    }
    if !closed {
        // Unterminated array: whatever is left is one more item, and it
        // degrades on its own rather than voiding the items before it.
        push_item(&mut out, json, start, b.len());
    }
    out
}

fn push_item<'a>(out: &mut Vec<&'a str>, json: &'a str, start: Option<usize>, end: usize) {
    if let Some(s) = start {
        let item = json[s..end].trim();
        if !item.is_empty() {
            out.push(item);
        }
    }
}

// ---------------------------------------------------------------------------
// the builder
// ---------------------------------------------------------------------------

/// Parse-time state. The interner and the three staging vectors are builder
/// state, not document state: they never reach [`GlossDoc`], so a cached
/// document retains neither a `HashMap` nor a scratch vector.
struct Builder {
    doc: GlossDoc,
    intern: HashMap<Box<str>, KeyId>,
    /// A node's own key-value entries must land contiguously, but its
    /// children are parsed in between them, so entries are staged here on a
    /// stack discipline and flushed when the node's object closes.
    s_data: Vec<(KeyId, Scalar)>,
    s_attr: Vec<(KeyId, Scalar)>,
    s_style: Vec<(StyleKey, Scalar)>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            doc: GlossDoc::default(),
            intern: HashMap::new(),
            s_data: Vec::new(),
            s_attr: Vec::new(),
            s_style: Vec::new(),
        }
    }

    /// Parses one glossary item and links it, degrading that item alone if it
    /// cannot be read.
    fn item(&mut self, json: &str, last: &mut NodeId) {
        let before = self.doc.nodes.len() as NodeId;
        let mut de = serde_json::Deserializer::from_str(json);
        let parsed = ItemSeed { b: self }.deserialize(&mut de).and_then(|id| {
            de.end()?;
            Ok(id)
        });
        match parsed {
            Ok(Some(id)) => self.link(NONE, last, id),
            // Neither a string nor an object: not a glossary item.
            Ok(None) => {}
            Err(_) => {
                self.doc.degraded_items += 1;
                // Whatever the tokenizer did read is already in the arena
                // and is worth more than nothing: keep it and lose only the
                // wrapper the parse never finished. The staging vectors are
                // cleared because the aborted node never flushed them, and
                // the next item must not adopt its leftovers.
                self.s_data.clear();
                self.s_attr.clear();
                self.s_style.clear();
                if (before as usize) < self.doc.nodes.len() {
                    self.link(NONE, last, before);
                }
            }
        }
    }

    fn push_text(&mut self, s: &str) -> Span {
        let at = self.doc.text.len() as u32;
        self.doc.text.push_str(s);
        Span { at, len: s.len() as u32 }
    }

    /// Gives back bytes appended by the immediately preceding [`push_text`].
    ///
    /// A `tag` or `type` name arrives as text, is resolved to an enum, and
    /// then has no reason to be retained. Handing the bytes back keeps the
    /// text buffer to actual content: at four bytes a node over a thousand
    /// nodes per hover, this is a measurable slice of the retained heap the
    /// arena exists to shrink.
    fn unpush_text(&mut self, s: Span) {
        if (s.at + s.len) as usize == self.doc.text.len() {
            self.doc.text.truncate(s.at as usize);
        }
    }

    fn intern(&mut self, s: &str) -> KeyId {
        if let Some(&id) = self.intern.get(s) {
            return id;
        }
        let span = self.push_text(s);
        let id = self.doc.keys.len() as KeyId;
        self.doc.keys.push(span);
        self.intern.insert(s.into(), id);
        id
    }

    fn new_node(&mut self) -> NodeId {
        let idx = self.doc.nodes.len() as NodeId;
        self.doc.nodes.push(Node {
            kind: Kind::Unknown,
            tag: Tag::None,
            item_type: ItemType::None,
            role: Role::Unclassified,
            text: Span::default(),
            first_child: NONE,
            next_sibling: NONE,
            data: Span::default(),
            attrs: Span::default(),
            style: Span::default(),
        });
        idx
    }

    fn text_node(&mut self, s: &str) -> NodeId {
        let span = self.push_text(s);
        let idx = self.new_node();
        let n = &mut self.doc.nodes[idx as usize];
        n.kind = Kind::Text;
        n.text = span;
        idx
    }

    fn link(&mut self, parent: NodeId, last: &mut NodeId, child: NodeId) {
        if *last == NONE {
            if parent == NONE {
                self.doc.first_item = child;
            } else {
                self.doc.nodes[parent as usize].first_child = child;
            }
        } else {
            self.doc.nodes[*last as usize].next_sibling = child;
        }
        *last = child;
    }
}

// ---------------------------------------------------------------------------
// seeds
// ---------------------------------------------------------------------------

/// One glossary item: a string, or an object. Anything else is not an item.
struct ItemSeed<'a> {
    b: &'a mut Builder,
}

impl<'de> DeserializeSeed<'de> for ItemSeed<'_> {
    type Value = Option<NodeId>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ItemSeed<'_> {
    type Value = Option<NodeId>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a glossary item")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(Some(self.b.text_node(v)))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        object(self.b, 1, map).map(Some)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }
}

/// A `data`, attribute, or style value.
struct ScalarSeed<'a> {
    b: &'a mut Builder,
}

impl<'de> DeserializeSeed<'de> for ScalarSeed<'_> {
    type Value = Scalar;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Scalar, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ScalarSeed<'_> {
    type Value = Scalar;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a scalar")
    }

    fn visit_str<E>(self, v: &str) -> Result<Scalar, E> {
        Ok(Scalar::Text(self.b.push_text(v)))
    }

    fn visit_bool<E>(self, v: bool) -> Result<Scalar, E> {
        Ok(Scalar::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Scalar, E> {
        Ok(Scalar::Num(v as f64))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Scalar, E> {
        Ok(Scalar::Num(v as f64))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Scalar, E> {
        Ok(Scalar::Num(v))
    }

    fn visit_unit<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Null)
    }

    fn visit_none<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Null)
    }

    /// `textDecorationLine` is the schema's one list-valued property, and a
    /// space-joined string is exactly what it means. Joining here rather
    /// than per walk is why the resolved record needs no list variant.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scalar, A::Error> {
        let at = self.b.doc.text.len() as u32;
        let mut end = at;
        while let Some(v) = seq.next_element_seed(ScalarSeed { b: &mut *self.b })? {
            // Only a text element put anything in the buffer, so only a text
            // element earns a separator.
            if matches!(v, Scalar::Text(_)) {
                end = self.b.doc.text.len() as u32;
                self.b.doc.text.push(' ');
            }
        }
        Ok(Scalar::Text(Span { at, len: end - at }))
    }

    /// An object where the schema admits a scalar. Consumed and recorded as
    /// null: there is no node position here, so keeping it would need a
    /// second, nested representation for values.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scalar, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Scalar::Null)
    }
}

/// A map key, interned. `None` for a key that is not a string, which JSON
/// cannot produce but a non-JSON `Deserializer` could.
struct InternSeed<'a>(&'a mut Builder);

impl<'de> DeserializeSeed<'de> for InternSeed<'_> {
    type Value = Option<KeyId>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for InternSeed<'_> {
    type Value = Option<KeyId>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map key")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(Some(self.0.intern(v)))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
}

/// The `data` object, whose keys are per-dictionary and unbounded - which is
/// why ticket 15 classifies an editorial role out of them rather than
/// matching a fixed name list, and why ticket 17's stylesheet matcher indexes
/// on them.
struct DataSeed<'a> {
    b: &'a mut Builder,
}

impl<'de> DeserializeSeed<'de> for DataSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for DataSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a data object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key_seed(InternSeed(&mut *self.b))? {
            let v = map.next_value_seed(ScalarSeed { b: &mut *self.b })?;
            if let Some(k) = key {
                self.b.s_data.push((k, v));
            }
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
}

/// The `style` object, resolved into the node's style record. An unrecognised
/// property is dropped: the record is a closed set, and the schema's own
/// property list is closed with it.
struct StyleSeed<'a> {
    b: &'a mut Builder,
}

impl<'de> DeserializeSeed<'de> for StyleSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StyleSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a style object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key_seed(InternSeed(&mut *self.b))? {
            let resolved = key.and_then(|k| style_key_for(self.b.doc.key(k)));
            match resolved {
                Some(k) => {
                    let v = map.next_value_seed(ScalarSeed { b: &mut *self.b })?;
                    self.b.s_style.push((k, v));
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
}

/// Everything in a node position: a string, an object, or an array of either.
/// Arrays are flattened - a node's children are the concatenation of every
/// array level under its `content`, which is what Yomitan's own renderer
/// does with them.
struct Kids<'a> {
    b: &'a mut Builder,
    parent: NodeId,
    last: &'a mut NodeId,
    /// The depth the node this seed creates would sit at.
    depth: u32,
}

impl<'de> DeserializeSeed<'de> for Kids<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Kids<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("structured content")
    }

    fn visit_str<E>(self, v: &str) -> Result<(), E> {
        if self.depth > MAX_DEPTH {
            self.b.doc.truncated_subtrees += 1;
            return Ok(());
        }
        let idx = self.b.text_node(v);
        self.b.link(self.parent, self.last, idx);
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq
            .next_element_seed(Kids {
                b: &mut *self.b,
                parent: self.parent,
                last: &mut *self.last,
                depth: self.depth,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        if self.depth > MAX_DEPTH {
            // Consumed, not parsed: the tokenizer still has to walk it, but
            // nothing below the cap reaches the arena, so the outer levels
            // render and a pathological tree terminates.
            self.b.doc.truncated_subtrees += 1;
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return Ok(());
        }
        let idx = object(self.b, self.depth, map)?;
        self.b.link(self.parent, self.last, idx);
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
}

/// One structured-content object into one node.
///
/// The node is created first and its fields are assigned as they arrive
/// rather than all at the end, so an object whose JSON breaks part way
/// through still carries the tag and the children the tokenizer did reach.
fn object<'de, A: MapAccess<'de>>(
    b: &mut Builder,
    depth: u32,
    mut map: A,
) -> Result<NodeId, A::Error> {
    let idx = b.new_node();
    let d_mark = b.s_data.len();
    let a_mark = b.s_attr.len();
    let s_mark = b.s_style.len();
    let mut last = NONE;
    let mut tag = Tag::None;
    let mut ty = ItemType::None;
    let mut has_text = false;

    while let Some(field) = map.next_key_seed(InternSeed(&mut *b))? {
        // The key is interned whether or not it turns out to be structural:
        // one hash lookup, and the attribute case needs the id anyway.
        let Some(key_id) = field else { continue };
        match field_name(b.doc.key(key_id)) {
            FieldName::Tag => {
                let v = map.next_value_seed(ScalarSeed { b: &mut *b })?;
                match v {
                    Scalar::Text(span) => match tag_for(b.doc.span(span)) {
                        Some(t) => {
                            b.unpush_text(span);
                            tag = t;
                        }
                        // Unrecognised: the name is kept as an attribute, so
                        // the tree stays lossless, and the node keeps its
                        // children.
                        None => {
                            tag = Tag::Other;
                            b.s_attr.push((key_id, v));
                        }
                    },
                    // A `tag` that is not a string names nothing, so there
                    // is no tag to be unknown - but the value is still kept.
                    other => b.s_attr.push((key_id, other)),
                }
                b.doc.nodes[idx as usize].tag = tag;
            }
            FieldName::Type => {
                let v = map.next_value_seed(ScalarSeed { b: &mut *b })?;
                match v {
                    Scalar::Text(span) => match item_type_for(b.doc.span(span)) {
                        Some(t) => {
                            b.unpush_text(span);
                            ty = t;
                        }
                        None => {
                            ty = ItemType::Other;
                            b.s_attr.push((key_id, v));
                        }
                    },
                    other => b.s_attr.push((key_id, other)),
                }
                b.doc.nodes[idx as usize].item_type = ty;
            }
            FieldName::Text => {
                let v = map.next_value_seed(ScalarSeed { b: &mut *b })?;
                match v {
                    Scalar::Text(span) => {
                        b.doc.nodes[idx as usize].text = span;
                        has_text = true;
                    }
                    other => b.s_attr.push((key_id, other)),
                }
            }
            FieldName::Content => {
                map.next_value_seed(Kids {
                    b: &mut *b,
                    parent: idx,
                    last: &mut last,
                    depth: depth + 1,
                })?;
            }
            FieldName::Data => map.next_value_seed(DataSeed { b: &mut *b })?,
            FieldName::Style => map.next_value_seed(StyleSeed { b: &mut *b })?,
            FieldName::Other => {
                let v = map.next_value_seed(ScalarSeed { b: &mut *b })?;
                b.s_attr.push((key_id, v));
            }
        }
        b.doc.nodes[idx as usize].kind = kind_of(tag, ty, has_text);
    }

    // Flush this node's staged entries contiguously. Descendants already
    // flushed and drained back to their own marks, so what is above ours is
    // exactly ours.
    let data = Span { at: b.doc.data_kv.len() as u32, len: (b.s_data.len() - d_mark) as u32 };
    b.doc.data_kv.extend(b.s_data.drain(d_mark..));
    let attrs = Span { at: b.doc.attr_kv.len() as u32, len: (b.s_attr.len() - a_mark) as u32 };
    b.doc.attr_kv.extend(b.s_attr.drain(a_mark..));
    let style = Span { at: b.doc.style_kv.len() as u32, len: (b.s_style.len() - s_mark) as u32 };
    b.doc.style_kv.extend(b.s_style.drain(s_mark..));

    let n = &mut b.doc.nodes[idx as usize];
    n.kind = kind_of(tag, ty, has_text);
    n.data = data;
    n.attrs = attrs;
    n.style = style;
    Ok(idx)
}

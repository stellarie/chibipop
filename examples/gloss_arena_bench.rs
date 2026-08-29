//! How `GlossDoc` should be laid out in memory, measured before ticket 02
//! writes it.
//!
//! Three representations of the same Yomitan structured-content tree, over the
//! real glossary payloads `tools/hover-parse-bench/payload.py` extracted:
//!
//! 1. **`serde_json::Value` + a recursive walk** - what
//!    [hover-parse-cost.md](../docs/research/hover-parse-cost.md) measured, so
//!    it anchors the comparison.
//! 2. **Box tree** - an enum with `Vec<Node>` children, `String` text, and a
//!    `HashMap<String, _>` `data` map. The implementation ticket 02 produces if
//!    nobody thinks about layout.
//! 3. **Arena** - one flat `Vec<ANode>` per entry, children as a
//!    first-child/next-sibling index pair, every byte of text in one `String`
//!    addressed by `(u32, u32)` spans, `data`/attribute keys interned into a
//!    side table and stored as `(u32, u32)` ranges into flat key-value vectors.
//!    One pass, no per-node allocation, walked by index.
//!
//! Both typed parsers are hand-written `serde::de::Visitor`/`DeserializeSeed`
//! trees over the *same* `serde_json` tokenizer, so the only thing that differs
//! is what gets built - not the JSON scanner underneath.
//!
//! All three are held to the same node model and the same metrics, and the
//! bench asserts their walk results are byte-identical on every payload. A
//! faster-but-wrong variant cannot win.
//!
//! ```sh
//! cargo run -p chibipop --release --example gloss_arena_bench          # all 550 headwords
//! cargo run -p chibipop --release --example gloss_arena_bench -- 40    # first 40, for a smoke run
//! ```
//!
//! Release mode is mandatory: a debug-profile serde number is off by an order
//! of magnitude and answers the question backwards.
//!
//! A hover-equivalent is the first `MAX_RESULTS` payloads of a headword, which
//! is what `LookupEngine::run` leaves after `ranked.truncate(MAX_RESULTS)`
//! (`src/lookup/engine.rs:175-179`).
//!
//! Allocation counting is a real counting global allocator wrapping
//! `std::alloc::System`, not an estimate. It is gated on a relaxed
//! `AtomicBool` load - a plain `mov`, no locked instruction - so timing passes
//! run with the counters off and are not distorted by the instrumentation;
//! allocation passes turn them on and their timings are discarded.

use anyhow::{Context, Result};
use chibipop::lookup::engine::MAX_RESULTS;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hint::black_box;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// counting global allocator
// ---------------------------------------------------------------------------

static COUNT_ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

/// `System`, plus counters behind one relaxed load.
///
/// `realloc` delegates to `System::realloc` rather than falling back to the
/// trait's alloc-copy-dealloc default: without that, every `Vec` growth would
/// become a full copy even when the allocator could have extended in place,
/// which would penalise the arena (which grows a handful of big vectors) for no
/// reason of its own.
struct Counting;

impl Counting {
    #[inline]
    fn on_alloc(size: usize) {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(size as u64, Relaxed);
        let live = LIVE.fetch_add(size as i64, Relaxed) + size as i64;
        PEAK.fetch_max(live, Relaxed);
    }

    #[inline]
    fn on_free(size: usize) {
        FREES.fetch_add(1, Relaxed);
        FREED_BYTES.fetch_add(size as u64, Relaxed);
        LIVE.fetch_sub(size as i64, Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNT_ON.load(Relaxed) {
            Self::on_free(layout.size());
        }
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_free(layout.size());
            Self::on_alloc(new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Clone, Copy, Default)]
struct AllocStat {
    allocs: u64,
    frees: u64,
    alloc_bytes: u64,
    freed_bytes: u64,
    live: i64,
    peak: i64,
}

fn alloc_reset() {
    ALLOCS.store(0, Relaxed);
    FREES.store(0, Relaxed);
    ALLOC_BYTES.store(0, Relaxed);
    FREED_BYTES.store(0, Relaxed);
    LIVE.store(0, Relaxed);
    PEAK.store(0, Relaxed);
}

fn alloc_snapshot() -> AllocStat {
    AllocStat {
        allocs: ALLOCS.load(Relaxed),
        frees: FREES.load(Relaxed),
        alloc_bytes: ALLOC_BYTES.load(Relaxed),
        freed_bytes: FREED_BYTES.load(Relaxed),
        live: LIVE.load(Relaxed),
        peak: PEAK.load(Relaxed),
    }
}

// ---------------------------------------------------------------------------
// the node model all three representations are held to
// ---------------------------------------------------------------------------

/// The node kinds ticket 02 names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Kind {
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
    Unknown,
}

/// The exact tag, kept alongside `Kind` because a `div` and a `span` are both
/// containers and a lossless tree may not forget which one it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Tag {
    /// No `tag` field: a bare string, a `type: text` item, a wrapper.
    None,
    /// Unrecognised: the name is kept in the attribute list.
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

/// `type` on a glossary item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ItemType {
    None,
    StructuredContent,
    Text,
    Image,
    Other,
}

fn item_type_for(s: &str) -> ItemType {
    match s {
        "structured-content" => ItemType::StructuredContent,
        "text" => ItemType::Text,
        "image" => ItemType::Image,
        _ => ItemType::Other,
    }
}

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
            ItemType::Text => Kind::Text,
            ItemType::StructuredContent => Kind::Container,
            _ if has_text => Kind::Text,
            _ => Kind::Unknown,
        },
        _ => Kind::Container,
    }
}

/// The resolved style record: every `style` property the census found in the
/// corpus, plus the schema-permitted siblings of those it found. Resolution is
/// string-to-enum, done once at parse time by both typed representations - and
/// re-done on every walk by the `Value` baseline, which is one of the things
/// this bench is measuring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum StyleKey {
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

/// The editorial role field ticket 02 says to carry now and classify in
/// ticket 15. One byte, present in both typed representations, never set by
/// these prototypes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Role(u8);

impl Role {
    const UNCLASSIFIED: Self = Self(0);
}

/// The five structural fields; everything else on a node is an attribute.
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

/// Losslessness counters. Every one of these is a place a prototype would drop
/// something, so they are reported rather than assumed to be zero.
#[derive(Default)]
struct Loss {
    unknown_tags: Cell<u64>,
    unknown_types: Cell<u64>,
    unknown_style_keys: Cell<u64>,
    /// A `data`/attribute/style value that was an object: consumed, not stored.
    dropped_objects: Cell<u64>,
}

impl Loss {
    fn bump(c: &Cell<u64>) {
        c.set(c.get() + 1);
    }

    fn total(&self) -> u64 {
        self.unknown_tags.get()
            + self.unknown_types.get()
            + self.unknown_style_keys.get()
            + self.dropped_objects.get()
    }
}

/// What every walk computes, in every representation. Compared for equality
/// across all three on every payload.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Walk {
    nodes: u64,
    text_bytes: u64,
    data_nodes: u64,
    styled_nodes: u64,
    max_depth: u32,
}

impl Walk {
    fn add(&mut self, other: Walk) {
        self.nodes += other.nodes;
        self.text_bytes += other.text_bytes;
        self.data_nodes += other.data_nodes;
        self.styled_nodes += other.styled_nodes;
        self.max_depth = self.max_depth.max(other.max_depth);
    }
}

// ---------------------------------------------------------------------------
// representation 1: serde_json::Value
// ---------------------------------------------------------------------------

fn value_style_resolves(o: &serde_json::Map<String, Value>) -> bool {
    o.get("style")
        .and_then(Value::as_object)
        .is_some_and(|s| s.keys().any(|k| style_key_for(k).is_some()))
}

fn walk_value(v: &Value, depth: u32, w: &mut Walk) {
    match v {
        // A bare string in a node position is a text node.
        Value::String(s) => {
            w.nodes += 1;
            w.max_depth = w.max_depth.max(depth);
            w.text_bytes += s.len() as u64;
        }
        // Arrays are transparent: a child list, not a node.
        Value::Array(a) => {
            for x in a {
                walk_value(x, depth, w);
            }
        }
        Value::Object(o) => {
            w.nodes += 1;
            w.max_depth = w.max_depth.max(depth);
            if let Some(t) = o.get("text").and_then(Value::as_str) {
                w.text_bytes += t.len() as u64;
            }
            if o.get("data").and_then(Value::as_object).is_some_and(|d| !d.is_empty()) {
                w.data_nodes += 1;
            }
            if value_style_resolves(o) {
                w.styled_nodes += 1;
            }
            if let Some(c) = o.get("content") {
                walk_value(c, depth + 1, w);
            }
        }
        // null / bool / number in a node position: not a node.
        _ => {}
    }
}

/// Ticket 17's inner loop: does this node's `data` map carry any of the probe
/// keys? Iterating the node's own keys and binary-searching the (sorted) probe
/// list is the cheaper of the two obvious spellings, so it is the one measured.
fn probe_value(v: &Value, probe: &[String], hits: &mut u64) {
    match v {
        Value::Array(a) => {
            for x in a {
                probe_value(x, probe, hits);
            }
        }
        Value::Object(o) => {
            if let Some(d) = o.get("data").and_then(Value::as_object) {
                if d.keys().any(|k| probe.binary_search_by(|p| p.as_str().cmp(k)).is_ok()) {
                    *hits += 1;
                }
            }
            if let Some(c) = o.get("content") {
                probe_value(c, probe, hits);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// representation 2: box tree
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum BoxScalar {
    Text(String),
    Num(f64),
    Bool(bool),
    Null,
}

#[derive(Debug)]
struct BoxNode {
    kind: Kind,
    tag: Tag,
    role: Role,
    /// Text nodes only.
    text: String,
    children: Vec<BoxNode>,
    /// The naive choice, and the one ticket 17's style matching has to live
    /// with. `HashMap::new()` does not allocate until the first insert, so an
    /// unadorned node pays nothing but the 48 inline bytes.
    data: HashMap<String, BoxScalar>,
    attrs: Vec<(String, BoxScalar)>,
    style: Vec<(StyleKey, BoxScalar)>,
}

impl BoxNode {
    fn empty() -> Self {
        Self {
            kind: Kind::Unknown,
            tag: Tag::None,
            role: Role::UNCLASSIFIED,
            text: String::new(),
            children: Vec::new(),
            data: HashMap::new(),
            attrs: Vec::new(),
            style: Vec::new(),
        }
    }

    fn text_node(s: &str) -> Self {
        let mut n = Self::empty();
        n.kind = Kind::Text;
        n.text = s.to_owned();
        n
    }
}

/// A string-valued field resolved by a closure, without keeping the string
/// when the resolution succeeds.
struct StrSeed<F, R>(F, PhantomData<R>);

impl<F, R> StrSeed<F, R> {
    fn new(f: F) -> Self {
        Self(f, PhantomData)
    }
}

impl<'de, F, R> DeserializeSeed<'de> for StrSeed<F, R>
where
    F: FnOnce(Option<&str>) -> R,
{
    type Value = R;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<R, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de, F, R> Visitor<'de> for StrSeed<F, R>
where
    F: FnOnce(Option<&str>) -> R,
{
    type Value = R;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<E>(self, v: &str) -> Result<R, E> {
        Ok((self.0)(Some(v)))
    }

    fn visit_unit<E>(self) -> Result<R, E> {
        Ok((self.0)(None))
    }

    fn visit_none<E>(self) -> Result<R, E> {
        Ok((self.0)(None))
    }

    fn visit_bool<E>(self, _: bool) -> Result<R, E> {
        Ok((self.0)(None))
    }

    fn visit_i64<E>(self, _: i64) -> Result<R, E> {
        Ok((self.0)(None))
    }

    fn visit_u64<E>(self, _: u64) -> Result<R, E> {
        Ok((self.0)(None))
    }

    fn visit_f64<E>(self, _: f64) -> Result<R, E> {
        Ok((self.0)(None))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<R, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok((self.0)(None))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<R, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok((self.0)(None))
    }
}

/// A `data`/attribute/style value. Numbers and booleans keep their type rather
/// than being formatted into a string: formatting would put an f64 printer on
/// the hot path of both typed representations for no representational reason.
struct BoxScalarSeed<'a>(&'a Loss);

impl<'de> DeserializeSeed<'de> for BoxScalarSeed<'_> {
    type Value = BoxScalar;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<BoxScalar, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for BoxScalarSeed<'_> {
    type Value = BoxScalar;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a scalar")
    }

    fn visit_str<E>(self, v: &str) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Text(v.to_owned()))
    }

    fn visit_bool<E>(self, v: bool) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Num(v as f64))
    }

    fn visit_u64<E>(self, v: u64) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Num(v as f64))
    }

    fn visit_f64<E>(self, v: f64) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Num(v))
    }

    fn visit_unit<E>(self) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Null)
    }

    fn visit_none<E>(self) -> Result<BoxScalar, E> {
        Ok(BoxScalar::Null)
    }

    /// `textDecorationLine` is a list in the schema; space-join it, the way a
    /// CSS value would read. The rule is "append each text element then a
    /// space, then trim back to the last element's end", which is what the
    /// arena has to do to keep its pieces contiguous - so both spell it that
    /// way and the losslessness checksum can compare them.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<BoxScalar, A::Error> {
        let mut out = String::new();
        let mut end = 0;
        while let Some(v) = seq.next_element_seed(BoxScalarSeed(self.0))? {
            if let BoxScalar::Text(s) = v {
                out.push_str(&s);
                end = out.len();
                out.push(' ');
            }
        }
        out.truncate(end);
        Ok(BoxScalar::Text(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BoxScalar, A::Error> {
        Loss::bump(&self.0.dropped_objects);
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(BoxScalar::Null)
    }
}

/// The `data` object.
struct BoxDataSeed<'a> {
    out: &'a mut HashMap<String, BoxScalar>,
    loss: &'a Loss,
}

impl<'de> DeserializeSeed<'de> for BoxDataSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for BoxDataSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a data object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(k) = map.next_key::<String>()? {
            let v = map.next_value_seed(BoxScalarSeed(self.loss))?;
            self.out.insert(k, v);
        }
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }
}

/// The `style` object, resolved to the style record.
struct BoxStyleSeed<'a> {
    out: &'a mut Vec<(StyleKey, BoxScalar)>,
    loss: &'a Loss,
}

impl<'de> DeserializeSeed<'de> for BoxStyleSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for BoxStyleSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a style object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key_seed(StrSeed::new(|s: Option<&str>| s.and_then(style_key_for)))? {
            match key {
                Some(k) => {
                    let v = map.next_value_seed(BoxScalarSeed(self.loss))?;
                    self.out.push((k, v));
                }
                None => {
                    Loss::bump(&self.loss.unknown_style_keys);
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }
}

/// Every node in a node position, arrays flattened.
struct BoxKids<'a> {
    out: &'a mut Vec<BoxNode>,
    loss: &'a Loss,
}

impl<'de> DeserializeSeed<'de> for BoxKids<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for BoxKids<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("structured content")
    }

    fn visit_str<E>(self, v: &str) -> Result<(), E> {
        self.out.push(BoxNode::text_node(v));
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq
            .next_element_seed(BoxKids { out: &mut *self.out, loss: self.loss })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        let node = box_object(map, self.loss)?;
        self.out.push(node);
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

fn box_object<'de, A: MapAccess<'de>>(mut map: A, loss: &Loss) -> Result<BoxNode, A::Error> {
    let mut node = BoxNode::empty();
    let mut ty = ItemType::None;
    let mut has_text = false;
    while let Some(field) = map.next_key_seed(StrSeed::new(|s: Option<&str>| match s {
        Some(s) => (field_name(s), Some(s.to_owned())),
        None => (FieldName::Other, None),
    }))? {
        let (name, raw) = field;
        match name {
            FieldName::Tag => {
                let resolved = map.next_value_seed(StrSeed::new(|s: Option<&str>| match s {
                    Some(s) => tag_for(s).map_or_else(|| Err(s.to_owned()), Ok),
                    None => Err(String::new()),
                }))?;
                match resolved {
                    Ok(t) => node.tag = t,
                    Err(name) => {
                        Loss::bump(&loss.unknown_tags);
                        node.tag = Tag::Other;
                        node.attrs.push(("tag".to_owned(), BoxScalar::Text(name)));
                    }
                }
            }
            FieldName::Type => {
                ty = map.next_value_seed(StrSeed::new(|s: Option<&str>| {
                    s.map_or(ItemType::Other, item_type_for)
                }))?;
                if ty == ItemType::Other {
                    Loss::bump(&loss.unknown_types);
                }
            }
            FieldName::Text => {
                node.text = map.next_value_seed(StrSeed::new(|s: Option<&str>| {
                    s.unwrap_or_default().to_owned()
                }))?;
                has_text = true;
            }
            FieldName::Content => {
                map.next_value_seed(BoxKids { out: &mut node.children, loss })?;
            }
            FieldName::Data => {
                map.next_value_seed(BoxDataSeed { out: &mut node.data, loss })?;
            }
            FieldName::Style => {
                map.next_value_seed(BoxStyleSeed { out: &mut node.style, loss })?;
            }
            FieldName::Other => {
                let v = map.next_value_seed(BoxScalarSeed(loss))?;
                node.attrs.push((raw.unwrap_or_default(), v));
            }
        }
    }
    node.kind = kind_of(node.tag, ty, has_text);
    Ok(node)
}

fn parse_box(payload: &str, loss: &Loss) -> Vec<BoxNode> {
    let mut roots = Vec::new();
    let mut de = serde_json::Deserializer::from_str(payload);
    BoxKids { out: &mut roots, loss }
        .deserialize(&mut de)
        .expect("payload.py wrote valid json");
    de.end().expect("trailing json");
    roots
}

fn walk_box(nodes: &[BoxNode], depth: u32, w: &mut Walk) {
    for n in nodes {
        w.nodes += 1;
        w.max_depth = w.max_depth.max(depth);
        w.text_bytes += n.text.len() as u64;
        if !n.data.is_empty() {
            w.data_nodes += 1;
        }
        if !n.style.is_empty() {
            w.styled_nodes += 1;
        }
        walk_box(&n.children, depth + 1, w);
    }
}

/// Iterate the node's own keys, binary-search the sorted probe list.
fn probe_box(nodes: &[BoxNode], probe: &[String], hits: &mut u64) {
    for n in nodes {
        if !n.data.is_empty()
            && n.data.keys().any(|k| {
                probe.binary_search_by(|p| p.as_str().cmp(k.as_str())).is_ok()
            })
        {
            *hits += 1;
        }
        probe_box(&n.children, probe, hits);
    }
}

/// The other obvious spelling: ask the map for each probe key. One SipHash of
/// the key per probe per node.
fn probe_box_naive(nodes: &[BoxNode], probe: &[String], hits: &mut u64) {
    for n in nodes {
        if !n.data.is_empty() && probe.iter().any(|k| n.data.contains_key(k.as_str())) {
            *hits += 1;
        }
        probe_box_naive(&n.children, probe, hits);
    }
}

// ---------------------------------------------------------------------------
// representation 3: arena
// ---------------------------------------------------------------------------

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Span {
    at: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug)]
enum AScalar {
    Text(Span),
    Num(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Copy)]
struct ANode {
    kind: Kind,
    tag: Tag,
    role: Role,
    /// Into `Arena::text`.
    text: Span,
    first_child: u32,
    next_sibling: u32,
    /// Into `Arena::data_kv`.
    data: Span,
    /// Into `Arena::attr_kv`.
    attrs: Span,
    /// Into `Arena::style_kv`.
    style: Span,
}

/// One entry's `GlossDoc`, flat. Nothing here is allocated per node: the
/// vectors grow amortised and the only per-*unique-key* allocation is the
/// interner's lookup copy.
struct Arena {
    nodes: Vec<ANode>,
    first_root: u32,
    text: String,
    data_kv: Vec<(u32, AScalar)>,
    attr_kv: Vec<(u32, AScalar)>,
    style_kv: Vec<(StyleKey, AScalar)>,
    /// Interned key text, by id.
    keys: Vec<Span>,
    intern: HashMap<Box<str>, u32>,
    /// A node's own key-value entries have to land contiguously, but its
    /// children are parsed in between them, so entries are staged here on a
    /// stack discipline and flushed when the node's object closes.
    s_data: Vec<(u32, AScalar)>,
    s_attr: Vec<(u32, AScalar)>,
    s_style: Vec<(StyleKey, AScalar)>,
}

impl Arena {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            first_root: NONE,
            text: String::new(),
            data_kv: Vec::new(),
            attr_kv: Vec::new(),
            style_kv: Vec::new(),
            keys: Vec::new(),
            intern: HashMap::new(),
            s_data: Vec::new(),
            s_attr: Vec::new(),
            s_style: Vec::new(),
        }
    }

    fn push_text(&mut self, s: &str) -> Span {
        let at = self.text.len() as u32;
        self.text.push_str(s);
        Span { at, len: s.len() as u32 }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.intern.get(s) {
            return id;
        }
        let span = self.push_text(s);
        let id = self.keys.len() as u32;
        self.keys.push(span);
        self.intern.insert(s.into(), id);
        id
    }

    fn new_node(&mut self) -> u32 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(ANode {
            kind: Kind::Unknown,
            tag: Tag::None,
            role: Role::UNCLASSIFIED,
            text: Span::default(),
            first_child: NONE,
            next_sibling: NONE,
            data: Span::default(),
            attrs: Span::default(),
            style: Span::default(),
        });
        idx
    }

    fn link(&mut self, parent: u32, last: &mut u32, child: u32) {
        if *last == NONE {
            if parent == NONE {
                self.first_root = child;
            } else {
                self.nodes[parent as usize].first_child = child;
            }
        } else {
            self.nodes[*last as usize].next_sibling = child;
        }
        *last = child;
    }

    /// Drops the parse-time staging vectors. They are builder state, not
    /// document state, and a cache entry has no business retaining them.
    /// The content vectors are deliberately *not* shrunk: `shrink_to_fit`
    /// would trade capacity slack for a full copy of everything.
    fn finish(&mut self) {
        self.s_data = Vec::new();
        self.s_attr = Vec::new();
        self.s_style = Vec::new();
    }

    /// Retained bytes the structure holds, by its own accounting - reported
    /// next to the allocator's number as a cross-check.
    fn footprint(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<ANode>()
            + self.text.capacity()
            + self.data_kv.capacity() * std::mem::size_of::<(u32, AScalar)>()
            + self.attr_kv.capacity() * std::mem::size_of::<(u32, AScalar)>()
            + self.style_kv.capacity() * std::mem::size_of::<(StyleKey, AScalar)>()
            + self.keys.capacity() * std::mem::size_of::<Span>()
    }
}

struct AScalarSeed<'a> {
    a: &'a mut Arena,
    loss: &'a Loss,
}

impl<'de> DeserializeSeed<'de> for AScalarSeed<'_> {
    type Value = AScalar;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<AScalar, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for AScalarSeed<'_> {
    type Value = AScalar;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a scalar")
    }

    fn visit_str<E>(self, v: &str) -> Result<AScalar, E> {
        Ok(AScalar::Text(self.a.push_text(v)))
    }

    fn visit_bool<E>(self, v: bool) -> Result<AScalar, E> {
        Ok(AScalar::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<AScalar, E> {
        Ok(AScalar::Num(v as f64))
    }

    fn visit_u64<E>(self, v: u64) -> Result<AScalar, E> {
        Ok(AScalar::Num(v as f64))
    }

    fn visit_f64<E>(self, v: f64) -> Result<AScalar, E> {
        Ok(AScalar::Num(v))
    }

    fn visit_unit<E>(self) -> Result<AScalar, E> {
        Ok(AScalar::Null)
    }

    fn visit_none<E>(self) -> Result<AScalar, E> {
        Ok(AScalar::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<AScalar, A::Error> {
        let at = self.a.text.len() as u32;
        let mut end = at;
        while let Some(v) =
            seq.next_element_seed(AScalarSeed { a: &mut *self.a, loss: self.loss })?
        {
            // Only a text element put anything in the buffer, so only a text
            // element earns a separator.
            if matches!(v, AScalar::Text(_)) {
                end = self.a.text.len() as u32;
                self.a.text.push(' ');
            }
        }
        Ok(AScalar::Text(Span { at, len: end - at }))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AScalar, A::Error> {
        Loss::bump(&self.loss.dropped_objects);
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(AScalar::Null)
    }
}

/// A map key, interned. Returns the key id, or `None` for a non-string key.
struct AInternSeed<'a>(&'a mut Arena);

impl<'de> DeserializeSeed<'de> for AInternSeed<'_> {
    type Value = Option<u32>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Option<u32>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for AInternSeed<'_> {
    type Value = Option<u32>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map key")
    }

    fn visit_str<E>(self, v: &str) -> Result<Option<u32>, E> {
        Ok(Some(self.0.intern(v)))
    }

    fn visit_unit<E>(self) -> Result<Option<u32>, E> {
        Ok(None)
    }
}

struct ADataSeed<'a> {
    a: &'a mut Arena,
    loss: &'a Loss,
}

impl<'de> DeserializeSeed<'de> for ADataSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ADataSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a data object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key_seed(AInternSeed(&mut *self.a))? {
            let v = map.next_value_seed(AScalarSeed { a: &mut *self.a, loss: self.loss })?;
            if let Some(k) = key {
                self.a.s_data.push((k, v));
            }
        }
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }
}

struct AStyleSeed<'a> {
    a: &'a mut Arena,
    loss: &'a Loss,
}

impl<'de> DeserializeSeed<'de> for AStyleSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for AStyleSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a style object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) =
            map.next_key_seed(StrSeed::new(|s: Option<&str>| s.and_then(style_key_for)))?
        {
            match key {
                Some(k) => {
                    let v =
                        map.next_value_seed(AScalarSeed { a: &mut *self.a, loss: self.loss })?;
                    self.a.s_style.push((k, v));
                }
                None => {
                    Loss::bump(&self.loss.unknown_style_keys);
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }
}

struct AKids<'a> {
    a: &'a mut Arena,
    loss: &'a Loss,
    parent: u32,
    last: &'a mut u32,
}

impl<'de> DeserializeSeed<'de> for AKids<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for AKids<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("structured content")
    }

    fn visit_str<E>(self, v: &str) -> Result<(), E> {
        let span = self.a.push_text(v);
        let idx = self.a.new_node();
        let n = &mut self.a.nodes[idx as usize];
        n.kind = Kind::Text;
        n.text = span;
        self.a.link(self.parent, self.last, idx);
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq
            .next_element_seed(AKids {
                a: &mut *self.a,
                loss: self.loss,
                parent: self.parent,
                last: &mut *self.last,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        let idx = arena_object(self.a, self.loss, map)?;
        self.a.link(self.parent, self.last, idx);
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

fn arena_object<'de, A: MapAccess<'de>>(
    a: &mut Arena,
    loss: &Loss,
    mut map: A,
) -> Result<u32, A::Error> {
    let idx = a.new_node();
    let d_mark = a.s_data.len();
    let at_mark = a.s_attr.len();
    let st_mark = a.s_style.len();
    let mut last = NONE;
    let mut ty = ItemType::None;
    let mut has_text = false;
    let mut tag = Tag::None;
    while let Some(field) = map.next_key_seed(AInternSeed(&mut *a))? {
        // The key is interned whether or not it turns out to be structural:
        // one hash lookup, and the attribute case needs the id anyway.
        let Some(key_id) = field else { continue };
        let name = {
            let span = a.keys[key_id as usize];
            field_name(&a.text[span.at as usize..(span.at + span.len) as usize])
        };
        match name {
            FieldName::Tag => {
                let resolved = map.next_value_seed(StrSeed::new(|s: Option<&str>| match s {
                    Some(s) => tag_for(s).map_or_else(|| Err(s.to_owned()), Ok),
                    None => Err(String::new()),
                }))?;
                match resolved {
                    Ok(t) => tag = t,
                    Err(name) => {
                        Loss::bump(&loss.unknown_tags);
                        tag = Tag::Other;
                        let span = a.push_text(&name);
                        a.s_attr.push((key_id, AScalar::Text(span)));
                    }
                }
            }
            FieldName::Type => {
                ty = map.next_value_seed(StrSeed::new(|s: Option<&str>| {
                    s.map_or(ItemType::Other, item_type_for)
                }))?;
                if ty == ItemType::Other {
                    Loss::bump(&loss.unknown_types);
                }
            }
            FieldName::Text => {
                let span = map
                    .next_value_seed(AScalarSeed { a: &mut *a, loss })
                    .map(|v| match v {
                        AScalar::Text(s) => s,
                        _ => Span::default(),
                    })?;
                a.nodes[idx as usize].text = span;
                has_text = true;
            }
            FieldName::Content => {
                map.next_value_seed(AKids { a: &mut *a, loss, parent: idx, last: &mut last })?;
            }
            FieldName::Data => {
                map.next_value_seed(ADataSeed { a: &mut *a, loss })?;
            }
            FieldName::Style => {
                map.next_value_seed(AStyleSeed { a: &mut *a, loss })?;
            }
            FieldName::Other => {
                let v = map.next_value_seed(AScalarSeed { a: &mut *a, loss })?;
                a.s_attr.push((key_id, v));
            }
        }
    }
    // Flush this node's staged entries contiguously. Descendants already
    // flushed and truncated back to their own marks, so what is above ours is
    // exactly ours.
    let data = Span {
        at: a.data_kv.len() as u32,
        len: (a.s_data.len() - d_mark) as u32,
    };
    a.data_kv.extend(a.s_data.drain(d_mark..));
    let attrs = Span {
        at: a.attr_kv.len() as u32,
        len: (a.s_attr.len() - at_mark) as u32,
    };
    a.attr_kv.extend(a.s_attr.drain(at_mark..));
    let style = Span {
        at: a.style_kv.len() as u32,
        len: (a.s_style.len() - st_mark) as u32,
    };
    a.style_kv.extend(a.s_style.drain(st_mark..));
    let n = &mut a.nodes[idx as usize];
    n.tag = tag;
    n.kind = kind_of(tag, ty, has_text);
    n.data = data;
    n.attrs = attrs;
    n.style = style;
    Ok(idx)
}

fn parse_arena(payload: &str, loss: &Loss) -> Arena {
    let mut a = Arena::new();
    let mut last = NONE;
    let mut de = serde_json::Deserializer::from_str(payload);
    AKids { a: &mut a, loss, parent: NONE, last: &mut last }
        .deserialize(&mut de)
        .expect("payload.py wrote valid json");
    de.end().expect("trailing json");
    a.finish();
    a
}

fn walk_arena_from(a: &Arena, mut i: u32, depth: u32, w: &mut Walk) {
    while i != NONE {
        let n = &a.nodes[i as usize];
        w.nodes += 1;
        w.max_depth = w.max_depth.max(depth);
        w.text_bytes += u64::from(n.text.len);
        if n.data.len > 0 {
            w.data_nodes += 1;
        }
        if n.style.len > 0 {
            w.styled_nodes += 1;
        }
        walk_arena_from(a, n.first_child, depth + 1, w);
        i = n.next_sibling;
    }
}

fn walk_arena(a: &Arena, w: &mut Walk) {
    walk_arena_from(a, a.first_root, 1, w);
}

const PROBE_MAX: usize = 24;

/// Ticket 17's inner loop on a flat arena: resolve the probe keys to ids once
/// per document, then sweep the node vector linearly comparing `u32`s. No
/// hashing and no string comparison per node, and no tree pointer-chasing.
fn probe_arena(a: &Arena, probe: &[String]) -> u64 {
    let mut ids = [u32::MAX; PROBE_MAX];
    let mut n = 0;
    for k in probe.iter().take(PROBE_MAX) {
        if let Some(&id) = a.intern.get(k.as_str()) {
            ids[n] = id;
            n += 1;
        }
    }
    let ids = &mut ids[..n];
    ids.sort_unstable();
    let mut hits = 0;
    for node in &a.nodes {
        if node.data.len == 0 {
            continue;
        }
        let lo = node.data.at as usize;
        let hi = lo + node.data.len as usize;
        if a.data_kv[lo..hi].iter().any(|&(k, _)| ids.binary_search(&k).is_ok()) {
            hits += 1;
        }
    }
    hits
}

// ---------------------------------------------------------------------------
// losslessness checksum
// ---------------------------------------------------------------------------
//
// The walk metrics prove the three representations agree on counts. This
// proves they agree on *bytes*: every tag, kind, text run, attribute,
// `data` entry and resolved style property is folded in. It is
// order-independent within a node's key-value sets - a `HashMap`, a
// `BTreeMap` and a flat vector do not agree on iteration order - and
// order-dependent over the tree, where all three must agree.
//
// This runs in the parity pass only, never on a timed path. It also means
// every stored field is actually read, so nothing here is a field the
// optimizer may quietly decline to write.

const FNV_SEED: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A stored scalar, independent of which representation stores it.
enum ScalarRef<'a> {
    Text(&'a str),
    Num(f64),
    Bool(bool),
    Null,
}

fn scalar_sum(h: u64, v: &ScalarRef) -> u64 {
    match v {
        ScalarRef::Text(s) => fnv(h ^ 1, s.as_bytes()),
        ScalarRef::Num(n) => fnv(h ^ 2, &n.to_bits().to_le_bytes()),
        ScalarRef::Bool(b) => fnv(h ^ 3, &[u8::from(*b)]),
        ScalarRef::Null => h ^ 4,
    }
}

/// One key-value entry. Summed commutatively into a node's accumulator, so
/// iteration order cannot change the result.
fn kv_sum(ns: u64, key: &str, v: &ScalarRef) -> u64 {
    scalar_sum(fnv(FNV_SEED ^ ns, key.as_bytes()), v)
}

/// Namespaces, so a `data` entry cannot collide with an attribute of the same
/// name and value.
const NS_ATTR: u64 = 0x11;
const NS_DATA: u64 = 0x22;
const NS_STYLE: u64 = 0x33;

fn style_sum(k: StyleKey, v: &ScalarRef) -> u64 {
    scalar_sum(fnv(FNV_SEED ^ NS_STYLE, &[k as u8]), v)
}

/// Folds one node into the document accumulator, in walk order.
fn node_sum(acc: &mut u64, kind: Kind, tag: Tag, role: Role, text: &str, kv: u64) {
    let mut h = fnv(FNV_SEED, &[kind as u8, tag as u8, role.0]);
    h = fnv(h, text.as_bytes());
    h ^= kv;
    *acc = fnv(*acc, &h.to_le_bytes());
}

/// Resolves a `Value` to the scalar both parsers would store. Text lands in
/// `joined` - the array case joined by the same rule the parsers use - and the
/// return is `None` for the text case.
fn value_nontext(v: &Value, joined: &mut String) -> Option<ScalarRef<'static>> {
    match v {
        Value::String(s) => {
            joined.push_str(s);
            None
        }
        Value::Array(a) => {
            let mut end = 0;
            for x in a {
                let mut inner = String::new();
                if value_nontext(x, &mut inner).is_none() {
                    joined.push_str(&inner);
                    end = joined.len();
                    joined.push(' ');
                }
            }
            joined.truncate(end);
            None
        }
        Value::Number(n) => Some(ScalarRef::Num(n.as_f64().unwrap_or(f64::NAN))),
        Value::Bool(b) => Some(ScalarRef::Bool(*b)),
        // `null` stores Null, and an object value is dropped to Null by all
        // three representations.
        _ => Some(ScalarRef::Null),
    }
}

/// Resolves a `Value` node's scalar the way both parsers do, then sums it.
fn value_kv_sum(ns: u64, key: &str, v: &Value) -> u64 {
    let mut joined = String::new();
    match value_nontext(v, &mut joined) {
        Some(s) => kv_sum(ns, key, &s),
        None => kv_sum(ns, key, &ScalarRef::Text(&joined)),
    }
}

fn value_style_sum(k: StyleKey, v: &Value) -> u64 {
    let mut joined = String::new();
    match value_nontext(v, &mut joined) {
        Some(s) => style_sum(k, &s),
        None => style_sum(k, &ScalarRef::Text(&joined)),
    }
}

fn sum_value(v: &Value, acc: &mut u64) {
    match v {
        Value::String(s) => node_sum(acc, Kind::Text, Tag::None, Role::UNCLASSIFIED, s, 0),
        Value::Array(a) => {
            for x in a {
                sum_value(x, acc);
            }
        }
        Value::Object(o) => {
            let tag_str = o.get("tag").and_then(Value::as_str);
            let tag = match tag_str {
                Some(s) => tag_for(s).unwrap_or(Tag::Other),
                None => Tag::None,
            };
            let ty = o.get("type").and_then(Value::as_str).map_or(ItemType::None, item_type_for);
            let text = o.get("text").and_then(Value::as_str).unwrap_or("");
            let has_text = o.contains_key("text");
            let mut kv = 0u64;
            for (k, val) in o {
                if matches!(field_name(k), FieldName::Other) {
                    kv = kv.wrapping_add(value_kv_sum(NS_ATTR, k, val));
                }
            }
            // An unrecognised tag keeps its name as an attribute in both typed
            // representations, so the baseline has to count it as one too.
            if tag == Tag::Other {
                kv = kv.wrapping_add(kv_sum(
                    NS_ATTR,
                    "tag",
                    &ScalarRef::Text(tag_str.unwrap_or_default()),
                ));
            }
            if let Some(d) = o.get("data").and_then(Value::as_object) {
                for (k, val) in d {
                    kv = kv.wrapping_add(value_kv_sum(NS_DATA, k, val));
                }
            }
            if let Some(st) = o.get("style").and_then(Value::as_object) {
                for (k, val) in st {
                    if let Some(sk) = style_key_for(k) {
                        kv = kv.wrapping_add(value_style_sum(sk, val));
                    }
                }
            }
            node_sum(acc, kind_of(tag, ty, has_text), tag, Role::UNCLASSIFIED, text, kv);
            if let Some(c) = o.get("content") {
                sum_value(c, acc);
            }
        }
        _ => {}
    }
}

fn box_scalar_ref(v: &BoxScalar) -> ScalarRef<'_> {
    match v {
        BoxScalar::Text(s) => ScalarRef::Text(s),
        BoxScalar::Num(n) => ScalarRef::Num(*n),
        BoxScalar::Bool(b) => ScalarRef::Bool(*b),
        BoxScalar::Null => ScalarRef::Null,
    }
}

fn sum_box(nodes: &[BoxNode], acc: &mut u64) {
    for n in nodes {
        let mut kv = 0u64;
        for (k, v) in &n.attrs {
            kv = kv.wrapping_add(kv_sum(NS_ATTR, k, &box_scalar_ref(v)));
        }
        for (k, v) in &n.data {
            kv = kv.wrapping_add(kv_sum(NS_DATA, k, &box_scalar_ref(v)));
        }
        for (k, v) in &n.style {
            kv = kv.wrapping_add(style_sum(*k, &box_scalar_ref(v)));
        }
        node_sum(acc, n.kind, n.tag, n.role, &n.text, kv);
        sum_box(&n.children, acc);
    }
}

impl Arena {
    fn span(&self, s: Span) -> &str {
        &self.text[s.at as usize..(s.at + s.len) as usize]
    }

    fn key(&self, id: u32) -> &str {
        self.span(self.keys[id as usize])
    }

    fn scalar(&self, v: &AScalar) -> ScalarRef<'_> {
        match v {
            AScalar::Text(s) => ScalarRef::Text(self.span(*s)),
            AScalar::Num(n) => ScalarRef::Num(*n),
            AScalar::Bool(b) => ScalarRef::Bool(*b),
            AScalar::Null => ScalarRef::Null,
        }
    }
}

fn sum_arena_from(a: &Arena, mut i: u32, acc: &mut u64) {
    while i != NONE {
        let n = &a.nodes[i as usize];
        let mut kv = 0u64;
        for &(k, ref v) in &a.attr_kv[n.attrs.at as usize..(n.attrs.at + n.attrs.len) as usize] {
            kv = kv.wrapping_add(kv_sum(NS_ATTR, a.key(k), &a.scalar(v)));
        }
        for &(k, ref v) in &a.data_kv[n.data.at as usize..(n.data.at + n.data.len) as usize] {
            kv = kv.wrapping_add(kv_sum(NS_DATA, a.key(k), &a.scalar(v)));
        }
        for &(k, ref v) in &a.style_kv[n.style.at as usize..(n.style.at + n.style.len) as usize] {
            kv = kv.wrapping_add(style_sum(k, &a.scalar(v)));
        }
        node_sum(acc, n.kind, n.tag, n.role, a.span(n.text), kv);
        sum_arena_from(a, n.first_child, acc);
        i = n.next_sibling;
    }
}

fn sum_arena(a: &Arena, acc: &mut u64) {
    sum_arena_from(a, a.first_root, acc);
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Record {
    term: String,
    /// `payload.py`'s deliberately over-represented worst-case set: the 50
    /// heaviest headwords by glossary bytes unioned with the 50 with the most
    /// term-bank rows. `hover-parse-cost.md` reports it on its own line and
    /// keeps it out of the frequency sample's percentiles, so this bench can
    /// reproduce that row's population exactly.
    worst: bool,
    payloads: Vec<String>,
}

fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/hover-parse-bench/results")
}

fn load_records() -> Result<Vec<Record>> {
    let path = results_dir().join("hover-payloads.jsonl");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!("reading {} - run tools/hover-parse-bench/payload.py first", path.display())
    })?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Record>(l).context("parsing a payload record"))
        .collect()
}

/// The `data` keys ticket 17 would actually match on: the most common ones in
/// the population being probed, measured here rather than guessed.
fn census_probe_keys(slices: &[(&str, &[String])], want: usize) -> Vec<String> {
    fn count(v: &Value, out: &mut BTreeMap<String, u64>) {
        match v {
            Value::Array(a) => {
                for x in a {
                    count(x, out);
                }
            }
            Value::Object(o) => {
                if let Some(d) = o.get("data").and_then(Value::as_object) {
                    for k in d.keys() {
                        *out.entry(k.clone()).or_default() += 1;
                    }
                }
                if let Some(c) = o.get("content") {
                    count(c, out);
                }
            }
            _ => {}
        }
    }
    let mut counts = BTreeMap::new();
    for (_, payloads) in slices {
        for p in *payloads {
            let v: Value = serde_json::from_str(p).expect("payload.py wrote valid json");
            count(&v, &mut counts);
        }
    }
    let mut ranked: Vec<(String, u64)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("\nstyle-probe key set: top {want} `data` keys over the hover slices");
    for (k, c) in ranked.iter().take(want) {
        println!("  {k:<24} {c:>8} nodes");
    }
    let mut keys: Vec<String> = ranked.into_iter().take(want).map(|(k, _)| k).collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------
// statistics
// ---------------------------------------------------------------------------

/// Nearest-rank percentile, matching `tools/hover-parse-bench`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((q * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[i]
}

struct Dist {
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    total: f64,
}

fn dist(samples: &[f64]) -> Dist {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    Dist {
        p50: percentile(&s, 0.50),
        p95: percentile(&s, 0.95),
        p99: percentile(&s, 0.99),
        max: s.last().copied().unwrap_or(0.0),
        total: s.iter().sum(),
    }
}

const MIN_SAMPLE: Duration = Duration::from_millis(20);

/// Repeats until `MIN_SAMPLE` of wall time has passed, accumulating only the
/// duration the closure reports, so setup and teardown stay out of the number.
fn time_accum(mut f: impl FnMut() -> Duration) -> f64 {
    let start = Instant::now();
    let mut acc = Duration::ZERO;
    let mut iters: u32 = 0;
    loop {
        acc += f();
        iters += 1;
        if start.elapsed() >= MIN_SAMPLE {
            break;
        }
    }
    acc.as_secs_f64() * 1e6 / f64::from(iters)
}

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

const REPS: usize = 3;
const LABELS: [&str; REPS] = ["serde_json::Value", "box tree", "arena"];

#[derive(Default)]
struct Series {
    parse: Vec<f64>,
    walk: Vec<f64>,
    p3w: Vec<f64>,
    probe: Vec<f64>,
    free: Vec<f64>,
    allocs: Vec<f64>,
    alloc_bytes: Vec<f64>,
    retained: Vec<f64>,
    peak: Vec<f64>,
    /// Allocations the built structure still holds: `allocs - frees`. What an
    /// `Arc`-shared cache entry would have to keep alive, and free later.
    blocks: Vec<f64>,
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let limit: usize = argv
        .first()
        .map(|s| s.parse().context("first argument must be a record count"))
        .transpose()?
        .unwrap_or(usize::MAX);

    let records = load_records()?;
    if records.is_empty() {
        anyhow::bail!("hover-payloads.jsonl is empty");
    }
    let slices: Vec<(&str, &[String])> = records
        .iter()
        .take(limit)
        .map(|r| (r.term.as_str(), &r.payloads[..r.payloads.len().min(MAX_RESULTS)]))
        .collect();
    // `payload.py` writes library archives first, so `payloads[..10]` is the
    // library top ten for every headword matching ten or more library rows -
    // the same slice `hover_parse_bench -- parse` calls `top 10`.
    let freq_only: Vec<bool> = records.iter().take(limit).map(|r| !r.worst).collect();
    let hover_bytes: Vec<f64> = slices
        .iter()
        .map(|(_, p)| p.iter().map(String::len).sum::<usize>() as f64)
        .collect();

    println!("gloss_arena_bench: how to lay out GlossDoc, measured");
    println!(
        "{} headwords, {} payloads (MAX_RESULTS = {}), {:.1} MB of glossary json",
        slices.len(),
        slices.iter().map(|(_, p)| p.len()).sum::<usize>(),
        MAX_RESULTS,
        hover_bytes.iter().sum::<f64>() / 1e6
    );
    println!(
        "node sizes: serde_json::Value {} B, BoxNode {} B inline, ANode {} B",
        std::mem::size_of::<Value>(),
        std::mem::size_of::<BoxNode>(),
        std::mem::size_of::<ANode>()
    );

    let probe = census_probe_keys(&slices, 20);

    // ---- parity: identical walk, probe and content on every payload ----

    let loss = Loss::default();
    let mut totals = Walk::default();
    let mut probe_total = 0u64;
    let mut checksum = 0u64;
    for (term, payloads) in &slices {
        for (i, p) in payloads.iter().enumerate() {
            let v: Value = serde_json::from_str(p).expect("payload.py wrote valid json");
            let b = parse_box(p, &loss);
            let a = parse_arena(p, &loss);

            let mut wv = Walk::default();
            walk_value(&v, 1, &mut wv);
            let mut wb = Walk::default();
            walk_box(&b, 1, &mut wb);
            let mut wa = Walk::default();
            walk_arena(&a, &mut wa);
            assert_eq!(wv, wb, "walk mismatch, Value vs box tree, term {term} payload {i}");
            assert_eq!(wv, wa, "walk mismatch, Value vs arena, term {term} payload {i}");
            // Makes the arena's flat probe sweep provably equivalent to a tree
            // walk: every node in the vector is reachable, none is an orphan.
            assert_eq!(
                wa.nodes,
                a.nodes.len() as u64,
                "arena holds unreachable nodes, term {term} payload {i}"
            );

            let mut hv = 0;
            probe_value(&v, &probe, &mut hv);
            let mut hb = 0;
            probe_box(&b, &probe, &mut hb);
            let mut hbn = 0;
            probe_box_naive(&b, &probe, &mut hbn);
            let ha = probe_arena(&a, &probe);
            assert_eq!(hv, hb, "probe mismatch, Value vs box tree, term {term} payload {i}");
            assert_eq!(hv, hbn, "probe mismatch, box tree spellings, term {term} payload {i}");
            assert_eq!(hv, ha, "probe mismatch, Value vs arena, term {term} payload {i}");

            let mut cv = 0u64;
            sum_value(&v, &mut cv);
            let mut cb = 0u64;
            sum_box(&b, &mut cb);
            let mut ca = 0u64;
            sum_arena(&a, &mut ca);
            assert_eq!(
                cv, cb,
                "content checksum mismatch, Value vs box tree, term {term} payload {i}"
            );
            assert_eq!(
                cv, ca,
                "content checksum mismatch, Value vs arena, term {term} payload {i}"
            );
            checksum = fnv(checksum, &cv.to_le_bytes());

            totals.add(wv);
            probe_total += hv;
        }
    }
    println!(
        "\nparity: all three agree on every payload, counts and bytes. {} nodes, \
         {} text bytes, {} nodes with a data key, {} with a resolved style, \
         max depth {}, {} nodes carry a probe key, content checksum {:#018x}",
        totals.nodes,
        totals.text_bytes,
        totals.data_nodes,
        totals.styled_nodes,
        totals.max_depth,
        probe_total,
        checksum
    );
    println!(
        "losslessness counters (unknown tag / unknown type / unknown style key / dropped object): \
         {} / {} / {} / {}, total {} over {} parses",
        loss.unknown_tags.get(),
        loss.unknown_types.get(),
        loss.unknown_style_keys.get(),
        loss.dropped_objects.get(),
        loss.total(),
        2 * slices.iter().map(|(_, p)| p.len()).sum::<usize>()
    );

    // ---- timing: counters off ----

    COUNT_ON.store(false, Relaxed);
    let mut series: [Series; REPS] = Default::default();
    let mut probe_naive: Vec<f64> = Vec::new();
    let mut arena_footprint: Vec<f64> = Vec::new();
    let mut node_counts: Vec<f64> = Vec::new();

    for (n, (_, payloads)) in slices.iter().enumerate() {
        if n % 25 == 0 {
            eprint!("\rtiming {n}/{}", slices.len());
        }

        // 1: serde_json::Value
        let build = || -> Vec<Value> {
            payloads
                .iter()
                .map(|p| serde_json::from_str(p).expect("payload.py wrote valid json"))
                .collect()
        };
        series[0].parse.push(time_accum(|| {
            let t = Instant::now();
            let built = build();
            let e = t.elapsed();
            black_box(&built);
            e
        }));
        let built = build();
        series[0].walk.push(time_accum(|| {
            let t = Instant::now();
            let mut w = Walk::default();
            for v in &built {
                walk_value(v, 1, &mut w);
            }
            let e = t.elapsed();
            black_box(w);
            e
        }));
        series[0].probe.push(time_accum(|| {
            let t = Instant::now();
            let mut h = 0;
            for v in &built {
                probe_value(v, &probe, &mut h);
            }
            let e = t.elapsed();
            black_box(h);
            e
        }));
        drop(built);
        series[0].p3w.push(time_accum(|| {
            let t = Instant::now();
            let built = build();
            let mut w = Walk::default();
            for _ in 0..3 {
                for v in &built {
                    walk_value(v, 1, &mut w);
                }
            }
            let e = t.elapsed();
            black_box((&built, w));
            e
        }));
        series[0].free.push(time_accum(|| {
            let built = build();
            let t = Instant::now();
            drop(built);
            t.elapsed()
        }));

        // 2: box tree
        let build = || -> Vec<Vec<BoxNode>> {
            payloads.iter().map(|p| parse_box(p, &loss)).collect()
        };
        series[1].parse.push(time_accum(|| {
            let t = Instant::now();
            let built = build();
            let e = t.elapsed();
            black_box(&built);
            e
        }));
        let built = build();
        series[1].walk.push(time_accum(|| {
            let t = Instant::now();
            let mut w = Walk::default();
            for b in &built {
                walk_box(b, 1, &mut w);
            }
            let e = t.elapsed();
            black_box(w);
            e
        }));
        series[1].probe.push(time_accum(|| {
            let t = Instant::now();
            let mut h = 0;
            for b in &built {
                probe_box(b, &probe, &mut h);
            }
            let e = t.elapsed();
            black_box(h);
            e
        }));
        probe_naive.push(time_accum(|| {
            let t = Instant::now();
            let mut h = 0;
            for b in &built {
                probe_box_naive(b, &probe, &mut h);
            }
            let e = t.elapsed();
            black_box(h);
            e
        }));
        drop(built);
        series[1].p3w.push(time_accum(|| {
            let t = Instant::now();
            let built = build();
            let mut w = Walk::default();
            for _ in 0..3 {
                for b in &built {
                    walk_box(b, 1, &mut w);
                }
            }
            let e = t.elapsed();
            black_box((&built, w));
            e
        }));
        series[1].free.push(time_accum(|| {
            let built = build();
            let t = Instant::now();
            drop(built);
            t.elapsed()
        }));

        // 3: arena
        let build = || -> Vec<Arena> { payloads.iter().map(|p| parse_arena(p, &loss)).collect() };
        series[2].parse.push(time_accum(|| {
            let t = Instant::now();
            let built = build();
            let e = t.elapsed();
            black_box(&built);
            e
        }));
        let built = build();
        series[2].walk.push(time_accum(|| {
            let t = Instant::now();
            let mut w = Walk::default();
            for a in &built {
                walk_arena(a, &mut w);
            }
            let e = t.elapsed();
            black_box(w);
            e
        }));
        series[2].probe.push(time_accum(|| {
            let t = Instant::now();
            let mut h = 0;
            for a in &built {
                h += probe_arena(a, &probe);
            }
            let e = t.elapsed();
            black_box(h);
            e
        }));
        arena_footprint.push(built.iter().map(Arena::footprint).sum::<usize>() as f64);
        node_counts.push(built.iter().map(|a| a.nodes.len()).sum::<usize>() as f64);
        drop(built);
        series[2].p3w.push(time_accum(|| {
            let t = Instant::now();
            let built = build();
            let mut w = Walk::default();
            for _ in 0..3 {
                for a in &built {
                    walk_arena(a, &mut w);
                }
            }
            let e = t.elapsed();
            black_box((&built, w));
            e
        }));
        series[2].free.push(time_accum(|| {
            let built = build();
            let t = Instant::now();
            drop(built);
            t.elapsed()
        }));
    }
    eprintln!("\rtiming {}/{} done", slices.len(), slices.len());

    // ---- allocation and retained footprint: counters on ----

    let mut residual = 0i64;
    for (i, (_, payloads)) in slices.iter().enumerate() {
        for rep in 0..REPS {
            COUNT_ON.store(true, Relaxed);
            alloc_reset();
            let (built_stat, nodes) = match rep {
                0 => {
                    let built: Vec<Value> = payloads
                        .iter()
                        .map(|p| serde_json::from_str(p).expect("valid json"))
                        .collect();
                    let s = alloc_snapshot();
                    let mut w = Walk::default();
                    for v in &built {
                        walk_value(v, 1, &mut w);
                    }
                    drop(built);
                    (s, w.nodes)
                }
                1 => {
                    let built: Vec<Vec<BoxNode>> =
                        payloads.iter().map(|p| parse_box(p, &loss)).collect();
                    let s = alloc_snapshot();
                    let mut w = Walk::default();
                    for b in &built {
                        walk_box(b, 1, &mut w);
                    }
                    drop(built);
                    (s, w.nodes)
                }
                _ => {
                    let built: Vec<Arena> =
                        payloads.iter().map(|p| parse_arena(p, &loss)).collect();
                    let s = alloc_snapshot();
                    let mut w = Walk::default();
                    for a in &built {
                        walk_arena(a, &mut w);
                    }
                    drop(built);
                    (s, w.nodes)
                }
            };
            let after = alloc_snapshot();
            COUNT_ON.store(false, Relaxed);
            residual = residual.max(after.live);
            series[rep].allocs.push(built_stat.allocs as f64);
            series[rep].alloc_bytes.push(built_stat.alloc_bytes as f64);
            series[rep].retained.push(built_stat.live as f64);
            series[rep].peak.push(built_stat.peak as f64);
            series[rep]
                .blocks
                .push((built_stat.allocs - built_stat.frees) as f64);
            // Internal consistency of the allocator's own books.
            assert_eq!(
                built_stat.alloc_bytes - built_stat.freed_bytes,
                built_stat.live as u64,
                "allocator accounting disagrees with itself, rep {rep}"
            );
            // The walk that ran inside the counted window must have seen the
            // same tree the timing pass measured.
            assert_eq!(
                nodes as f64, node_counts[i],
                "node count drifted between passes, rep {rep}"
            );
        }
    }

    // ---- report ----

    let bytes_total: f64 = hover_bytes.iter().sum();
    let nodes_total: f64 = node_counts.iter().sum();
    let d: Vec<[Dist; 4]> = (0..REPS)
        .map(|r| {
            [
                dist(&series[r].parse),
                dist(&series[r].walk),
                dist(&series[r].p3w),
                dist(&series[r].probe),
            ]
        })
        .collect();

    println!("\nper hover ({} headwords, first {MAX_RESULTS} payloads each), p50 unless noted", slices.len());
    println!(
        "{:<18}  {:>9}  {:>9}  {:>11}  {:>7}  {:>12}  {:>10}  {:>10}",
        "representation", "parse us", "walk us", "p+3walks us", "MB/s", "allocs/hover", "B/node", "probe us"
    );
    println!("{}", "-".repeat(18 + 2 + 9 + 2 + 9 + 2 + 11 + 2 + 7 + 2 + 12 + 2 + 10 + 2 + 10));
    for r in 0..REPS {
        let retained: f64 = series[r].retained.iter().sum();
        println!(
            "{:<18}  {:>9.1}  {:>9.1}  {:>11.1}  {:>7.0}  {:>12.0}  {:>10.1}  {:>10.1}",
            LABELS[r],
            d[r][0].p50,
            d[r][1].p50,
            d[r][2].p50,
            bytes_total / d[r][0].total,
            dist(&series[r].allocs).p50,
            retained / nodes_total,
            d[r][3].p50,
        );
    }
    println!(
        "{:<18}  {:>9}  {:>9}  {:>11}  {:>7}  {:>12}  {:>10}  {:>10.1}",
        "box tree, naive", "-", "-", "-", "-", "-", "-",
        dist(&probe_naive).p50
    );
    println!(
        "\n\"box tree, naive\" probes with `data.contains_key(k)` for each of the {} keys; \
         the `box tree` row iterates the node's own keys and binary-searches them.",
        probe.len()
    );

    for (i, name) in ["parse", "walk", "parse + 3 walks", "style probe"].iter().enumerate() {
        println!("\n{name} per hover, us");
        println!(
            "{:<18}  {:>9}  {:>9}  {:>9}  {:>9}",
            "representation", "p50", "p95", "p99", "max"
        );
        println!("{}", "-".repeat(18 + 2 + 9 * 4 + 6));
        for r in 0..REPS {
            println!(
                "{:<18}  {:>9.1}  {:>9.1}  {:>9.1}  {:>9.1}",
                LABELS[r], d[r][i].p50, d[r][i].p95, d[r][i].p99, d[r][i].max
            );
        }
    }

    println!("\nallocation and retained heap per hover (counting global allocator)");
    println!(
        "{:<18}  {:>11}  {:>12}  {:>13}  {:>12}  {:>11}  {:>11}  {:>8}  {:>8}",
        "representation",
        "allocs p50",
        "alloc B p50",
        "retained B p50",
        "peak B p50",
        "live blocks",
        "allocs/node",
        "B/node",
        "free us"
    );
    println!("{}", "-".repeat(18 + 11 + 12 + 13 + 12 + 11 + 11 + 8 + 8 + 16));
    for r in 0..REPS {
        let allocs: f64 = series[r].allocs.iter().sum();
        let retained: f64 = series[r].retained.iter().sum();
        println!(
            "{:<18}  {:>11.0}  {:>12.0}  {:>13.0}  {:>12.0}  {:>11.0}  {:>11.2}  {:>8.1}  {:>8.1}",
            LABELS[r],
            dist(&series[r].allocs).p50,
            dist(&series[r].alloc_bytes).p50,
            dist(&series[r].retained).p50,
            dist(&series[r].peak).p50,
            dist(&series[r].blocks).p50,
            allocs / nodes_total,
            retained / nodes_total,
            dist(&series[r].free).p50,
        );
    }
    println!(
        "nodes per hover: p50 {:.0}, max {:.0}; total {:.0}. \
         Arena self-reported footprint per hover: p50 {:.0} B (allocator says {:.0} B).",
        dist(&node_counts).p50,
        dist(&node_counts).max,
        nodes_total,
        dist(&arena_footprint).p50,
        dist(&series[2].retained).p50,
    );
    println!("residual live bytes after every structure was dropped: {residual} (0 = no leak)");

    // The anchor. `hover-parse-cost.md` quotes "frequency, top 10 rendered",
    // which is this same `payloads[..10]` slice with the worst-case headwords
    // held out, timed as one parse plus one walk. Reproducing that population
    // here is what lets the arena's win be stated against a budget rather than
    // against itself.
    let keep = |v: &[f64]| -> Vec<f64> {
        v.iter().zip(&freq_only).filter(|(_, &f)| f).map(|(x, _)| *x).collect()
    };
    println!(
        "\nparse + 1 walk, frequency sample only ({} of {} headwords) - the population \
         docs/research/hover-parse-cost.md quotes as 153.8 / 785.6 / 1811.9 / 3634.8 us",
        freq_only.iter().filter(|f| **f).count(),
        freq_only.len()
    );
    println!(
        "{:<18}  {:>9}  {:>9}  {:>9}  {:>9}  {:>7}",
        "representation", "p50", "p95", "p99", "max", "MB/s"
    );
    println!("{}", "-".repeat(18 + 9 * 4 + 7 + 12));
    let kept_bytes: f64 = keep(&hover_bytes).iter().sum();
    for r in 0..REPS {
        let pw: Vec<f64> = series[r]
            .parse
            .iter()
            .zip(&series[r].walk)
            .map(|(p, w)| p + w)
            .collect();
        let k = dist(&keep(&pw));
        println!(
            "{:<18}  {:>9.1}  {:>9.1}  {:>9.1}  {:>9.1}  {:>7.0}",
            LABELS[r], k.p50, k.p95, k.p99, k.max, kept_bytes / k.total
        );
    }

    Ok(())
}

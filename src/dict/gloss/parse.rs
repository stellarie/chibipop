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

pub(crate) fn tag_for(s: &str) -> Option<Tag> {
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
// editorial role
// ---------------------------------------------------------------------------
//
// Every node's role, from its `data` map, on the per-hover parse path. What
// it costs, over the 30 150 real glossary payloads `tools/hover-parse-bench`
// extracted - 3 285 630 nodes, 671 969 distinct interned keys, 31% of nodes
// carrying a `data` map. Release profile, best of seven sweeps, each row
// against the same build with `hook_of` and `Builder::role_at` stubbed out:
//
// | shape | sweep | ns/node | vs no classifier |
// |---|---:|---:|---:|
// | no classifier | 498.4 ms | - | - |
// | fold per character, inside a per-position scan | 751.0 ms | 76.9 | +50.7% |
// | fold once, `str::contains` per needle | 615.9 ms | 35.7 | +23.6% |
// | + byte-set prune | 576.0 ms | 23.6 | +15.6% |
// | + byte-window search | **554.8 ms** | **17.2** | **+11.3%** |
//
// 17.2 ns per node, and 84 ns per *distinct key* - interning is what makes
// those different numbers, since the matching runs 0.2 times per node rather
// than once. The per-node half, `role_at`'s walk over a node's one to three
// staged entries, is below this measurement's noise floor.
//
// The first row is why the rest exist: the obvious implementation, folding a
// character at a time inside a hand-written substring scan, re-decodes every
// key once per needle per start position and costs four and a half times as
// much as the shape that shipped.

/// The role vocabulary, as needles matched against a *normalised substring*
/// of a `data` key or of one of [`VALUE_KEYS`]' values.
///
/// Every row of `docs/research/dict-shapes.md`'s bilingual table, and nothing
/// invented beside them. Two properties of the table do the work:
///
/// - **It is bilingual, and Japanese dominates.** 755 264 example nodes
///   across four dictionaries sit under Japanese keys against roughly 62 000
///   under ASCII ones, so an ASCII-only match misses twelve example nodes in
///   thirteen. The same holds for every other role: `出典`, `参照`, `解説`
///   outweigh their ASCII spellings by an order of magnitude.
/// - **Substring, not equality.** 旺文社 全訳古語辞典 alone writes eleven
///   distinct `用例`-prefixed keys - `用例訳`, `用例引用`, `用例活用`,
///   `用例メタデータ`, `用例囲みG` - and 大辞林 writes `慣用例` and `用例注釈`.
///   A substring needle covers all thirteen with one row; equality would need
///   thirteen rows and would still miss the fourteenth spelling.
///
/// So the nine Japanese example keys the table names collapse to two needles:
/// every one of `用例`, `用例G`, `用例訳`, `用例引用`, `用例活用`, `慣用例`,
/// `囲み用例` contains `用例`, and `例文`, `識別例文` contain `例文`. The eight
/// ASCII example spellings - `example-sentence*`, `example-keyword`,
/// `examples`, `example_N`, `example`, `ExampleG`, `ExampleC` - all collapse
/// to `example` once case is folded.
///
/// Deliberately **not** the bare needle `例`. It would catch four more census
/// hooks - `語例`, `熟語例G`, `俳句例`, `作品例` - worth 4 221 of 832 543
/// example occurrences, 0.5%, and would also catch `凡例` (a dictionary's
/// conventions), `例外` (an exception) and `例え` in any dictionary outside
/// the corpus. Example is a *droppable* role, so a needle that broad trades a
/// 0.5% gain for the one failure this classifier must not make: content that
/// vanishes.
const NEEDLES: [(&str, Role); 13] = [
    // Yomitan's own convention, and the only role that moves content rather
    // than hiding it.
    ("part-of-speech", Role::PartOfSpeech),
    ("attribution", Role::Attribution),
    ("出典", Role::Attribution),
    ("example", Role::Example),
    ("用例", Role::Example),
    ("例文", Role::Example),
    ("xref", Role::Reference),
    ("参照", Role::Reference),
    ("類語", Role::Reference),
    ("対義", Role::Reference),
    ("解説", Role::Commentary),
    ("補説", Role::Commentary),
    ("語源", Role::Commentary),
];

/// The three keys whose *value* carries the role rather than the key.
///
/// `content` is Yomitan's own structured-content convention
/// (`data.content = "example-sentence"`). `name` and `class` are the two HTML
/// attributes the EPUB-to-Yomitan converters map into `data`, and both carry
/// the role in the value: 三省堂国語辞典 writes `name=用例`, 旺文社 全訳古語辞典
/// writes `class=fill 用例 FM`.
///
/// Closed at three on purpose. Matching every value would read a role out of
/// an `alt` text or an `href` - the census has `alt=［例］` on 24 706 gaiji
/// images, which are *characters* a font lacks, and `href=$yourei` on 91 859
/// links whose target happens to be an example lookup. Hiding either would
/// punch a hole in a word or drop a link, so the value side stays on the
/// three keys that are conventions rather than content.
const VALUE_KEYS: [&str; 3] = ["content", "name", "class"];

/// What an interned key name tells us, resolved once per distinct key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hook {
    /// The key name itself names the role.
    Named(Role),
    /// One of [`VALUE_KEYS`]: the role, if any, is in the value.
    InValue,
    /// Neither. Most keys - `ruby`, `rt`, `img`, `div`, `xmlns` - are here.
    None,
}

/// A 256-bit set of the bytes a string holds.
///
/// Built during the fold, which already walks every byte, and tested against
/// [`NEEDLE_LEADS`] before any needle is searched for. The nine bytes a
/// needle can start with are `a`, `e`, `p`, `x` and five CJK lead bytes;
/// almost every `data` key holds none of them - `ruby`, `rt`, `img`, `div`,
/// `body`, `html`, `full`, `rb`, `td` - and is rejected on four machine words
/// instead of thirteen substring searches.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct ByteSet([u64; 4]);

impl ByteSet {
    const fn with(mut self, b: u8) -> ByteSet {
        self.0[(b >> 6) as usize] |= 1 << (b & 63);
        self
    }

    fn meets(self, other: ByteSet) -> bool {
        let ByteSet(a) = self;
        let ByteSet(b) = other;
        a[0] & b[0] | a[1] & b[1] | a[2] & b[2] | a[3] & b[3] != 0
    }

    fn holds(self, b: u8) -> bool {
        self.0[(b >> 6) as usize] & 1 << (b & 63) != 0
    }
}

/// The first byte of every needle, derived from [`NEEDLES`] rather than
/// written out beside it - a hand-kept copy would drift the moment a row is
/// added, and it would drift *silently*, in the direction of classifying
/// nothing.
const NEEDLE_LEADS: ByteSet = {
    let mut set = ByteSet([0; 4]);
    let mut i = 0;
    while i < NEEDLES.len() {
        set = set.with(NEEDLES[i].0.as_bytes()[0]);
        i += 1;
    }
    set
};

/// One character, normalised for matching.
///
/// Three folds, each answering a spelling difference the corpus actually
/// contains or that a converter plainly can produce:
///
/// - **Case**, ASCII only. 旺文社漢字典 writes `ExampleG` and `ExampleC`
///   where Jitendex writes `example-sentence`; they are one convention spelled
///   by two converters. Japanese has no case, so this costs nothing there.
/// - **Width**, full-width ASCII letters and digits to half-width. Eight to
///   eleven census dictionaries were converted from HTML or EPUB and drag
///   their source markup through `data.html`/`data.body`/`data.xmlns`; such a
///   source writes `Ａ` and `１` in a class name as readily as `A` and `1`.
///   One `char` compare either way.
/// - **The ideographic space** U+3000 to U+0020, because a Japanese class list
///   is as likely to be `用例　FM` as `用例 FM`.
///
/// Not Unicode case folding, not NFKC, not kana width. Full NFKC would fold
/// half-width katakana and CJK compatibility ideographs, which are *content*
/// in a Japanese dictionary and never appear in a role hook; it also cannot
/// be done one `char` at a time, so it would cost an allocation per key on
/// the per-hover parse path to answer a question no census key asks.
fn fold(c: char) -> char {
    match c {
        'A'..='Z' => (c as u8 + 32) as char,
        '\u{FF21}'..='\u{FF3A}' => (c as u32 - 0xFF21 + 'a' as u32) as u8 as char,
        '\u{FF41}'..='\u{FF5A}' => (c as u32 - 0xFF41 + 'a' as u32) as u8 as char,
        '\u{FF10}'..='\u{FF19}' => (c as u32 - 0xFF10 + '0' as u32) as u8 as char,
        '\u{3000}' => ' ',
        _ => c,
    }
}

/// One pass over `text`'s bytes: the set of bytes it holds, or `None` when
/// [`fold`] would rewrite one of them.
///
/// The fast path, and it is the one nearly every call takes. `fold` rewrites
/// exactly three things - ASCII uppercase, the full-width ASCII block
/// U+FF10..U+FF5A, and U+3000 - and the latter two have UTF-8 lead bytes
/// `0xEF` and `0xE3`. A string holding none of the three is *already* in
/// folded form, so a needle can be searched for in it directly and the
/// scratch buffer is never touched. `content`, `sense`, `glossary`,
/// `example-sentence`, `part-of-speech-info`, `用例`, `出典` all qualify;
/// only `ExampleG` and `fill 用例 FM` take the slow path.
fn scan(text: &str) -> Option<ByteSet> {
    let mut seen = ByteSet::default();
    for &b in text.as_bytes() {
        if b.is_ascii_uppercase() || b == 0xEF || b == 0xE3 {
            return None;
        }
        seen = seen.with(b);
    }
    Some(seen)
}

/// Folds `text` into `buf`, replacing whatever `buf` held, and reports which
/// bytes the result holds.
///
/// The slow path, taken only when [`scan`] found a character to rewrite.
/// `buf` is the builder's one reusable scratch string, so even this
/// allocates only while the buffer grows to the longest such key or value a
/// document holds - tens of bytes, once per parse.
fn fold_into(buf: &mut String, text: &str) -> ByteSet {
    buf.clear();
    buf.reserve(text.len());
    buf.extend(text.chars().map(fold));
    let mut seen = ByteSet::default();
    for &b in buf.as_bytes() {
        seen = seen.with(b);
    }
    seen
}

/// The role a folded key name or convention value names, or
/// [`Role::Content`].
///
/// Three filters before any substring search, all reading the byte set the
/// one normalising pass already built:
///
/// 1. The whole table at once. A string holding none of [`NEEDLE_LEADS`]
///    cannot hold any needle, so `ruby`, `rt`, `img`, `div`, `body`, `html`,
///    `full`, `rb` and `td` - the corpus's highest-volume keys - return on
///    four machine words.
/// 2. Per needle, its own lead byte. A needle whose first byte the string
///    does not hold cannot occur in it, so `content` searches for `example`
///    alone rather than for all thirteen: one bit test replaces twelve
///    substring searches.
/// 3. Per needle, its role. A row that cannot improve on the best found so
///    far is skipped, and the loop stops dead once the table's first role is
///    found because nothing can beat it.
///
/// The smallest matching role wins, so the answer does not depend on the
/// table's row order - see [`Role`]'s note on precedence.
fn role_of_folded(folded: &str, seen: ByteSet) -> Role {
    if !seen.meets(NEEDLE_LEADS) {
        return Role::Content;
    }
    let hay = folded.as_bytes();
    let mut best = Role::Content;
    for &(needle, role) in &NEEDLES {
        let needle = needle.as_bytes();
        if role < best && seen.holds(needle[0]) && holds(hay, needle) {
            best = role;
            if role == NEEDLES[0].1 {
                break;
            }
        }
    }
    best
}

/// Does `hay` hold `needle`, both already folded?
///
/// A byte-window compare rather than [`str::contains`], which builds a
/// two-way searcher per call: at four to fourteen bytes of needle against
/// tens of bytes of key, that setup costs more than the whole search. Both
/// sides are folded here, so bytes are as good as characters - a needle can
/// only align with a character boundary because a UTF-8 lead byte never
/// appears as a continuation byte.
fn holds(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The role `text` names, normalising it on the way.
fn role_of(buf: &mut String, text: &str) -> Role {
    match scan(text) {
        Some(seen) => role_of_folded(text, seen),
        None => {
            let seen = fold_into(buf, text);
            role_of_folded(buf, seen)
        }
    }
}

/// What a key name is worth, resolved once at intern time.
fn hook_of(buf: &mut String, name: &str) -> Hook {
    match scan(name) {
        Some(seen) => hook_of_folded(name, seen),
        None => {
            let seen = fold_into(buf, name);
            hook_of_folded(buf, seen)
        }
    }
}

/// The value-key test is *equality* on the folded name, not a substring, and
/// it is the one place in this classifier that is not a substring match.
/// Nine census keys hold `content`, `name` or `class` without being one:
/// `contents`, `Contents`, `jukugoitemcontent`, `CampaignName` and five more,
/// 86 747 nodes. Reading a role out of *their* values would let a
/// `filename=example.png` hide a node. The three conventions are exact
/// spellings, so the match is exact.
fn hook_of_folded(folded: &str, seen: ByteSet) -> Hook {
    match role_of_folded(folded, seen) {
        Role::Content if VALUE_KEYS.contains(&folded) => Hook::InValue,
        Role::Content => Hook::None,
        role => Hook::Named(role),
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
    /// One [`Hook`] per interned key, parallel to `doc.keys`.
    ///
    /// **This is the whole of the classifier's interning strategy.** The
    /// normalised substring scan over the 13-row table runs once per
    /// *distinct* key name in the document - tens of names, whatever the
    /// node count - and never once per node. Per node the classifier then
    /// walks the one to three entries the node staged and indexes this
    /// vector by [`KeyId`], which is an array load and an integer compare.
    ///
    /// Sized by `doc.keys`, so it covers attribute and structural key names
    /// too. Classifying `href` or `colSpan` costs the same one scan as
    /// classifying `用例` and yields [`Hook::None`]; keeping the table a
    /// straight parallel array indexed by `KeyId` is worth more than
    /// skipping a handful of scans.
    key_hooks: Vec<Hook>,
    /// The classifier's one scratch string, holding whatever key or value it
    /// last folded. Reused across the whole parse, so folding costs no
    /// allocation past the first key.
    s_fold: String,
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
            key_hooks: Vec::new(),
            s_fold: String::new(),
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

    /// Interns a key name and classifies it, both exactly once per distinct
    /// name.
    fn intern(&mut self, s: &str) -> KeyId {
        if let Some(&id) = self.intern.get(s) {
            return id;
        }
        let span = self.push_text(s);
        let id = self.doc.keys.len() as KeyId;
        self.doc.keys.push(span);
        self.key_hooks.push(hook_of(&mut self.s_fold, s));
        self.intern.insert(s.into(), id);
        id
    }

    /// The editorial role of the node whose `data` entries are staged from
    /// `mark`.
    ///
    /// The smallest role any of the node's own `data` entries names, and
    /// [`Role::Content`] when none of them names one. Runs once per node with
    /// a `data` map, over the one to three entries that map holds.
    ///
    /// No inheritance, on purpose: a role marks a *subtree*, and every
    /// renderer over this tree stops descending at the node the filter
    /// rejects, so the subtree follows its root without a byte on any
    /// descendant. That also leaves a descendant addressable on its own -
    /// `Selection::Nodes` renders a sense a user picked out of an example
    /// block, which is the Anki half of a sense picker.
    ///
    /// A fast exit for the overwhelming majority: a node with no `data` map
    /// stages no entries, so the loop does not run and the node is content.
    fn role_at(&mut self, mark: usize) -> Role {
        // Disjoint field borrows: the staged entries are read while the fold
        // buffer is written, which `self.` syntax cannot express.
        let Builder { doc, key_hooks, s_fold, s_data, .. } = self;
        let mut best = Role::Content;
        for &(key, value) in &s_data[mark..] {
            let role = match key_hooks[key as usize] {
                Hook::Named(role) => role,
                Hook::InValue => match value {
                    Scalar::Text(span) => role_of(s_fold, doc.span(span)),
                    // A convention key whose value is a number, a bool or
                    // null names no role - but it does still mark a block,
                    // which `GlossDoc::has_marker` answers separately.
                    _ => Role::Content,
                },
                Hook::None => Role::Content,
            };
            best = best.min(role);
        }
        best
    }

    fn new_node(&mut self) -> NodeId {
        let idx = self.doc.nodes.len() as NodeId;
        self.doc.nodes.push(Node {
            kind: Kind::Unknown,
            tag: Tag::None,
            item_type: ItemType::None,
            role: Role::Content,
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
/// why the role classifier reads an editorial role out of them rather than
/// matching a fixed name list, and why the stylesheet matcher indexes on
/// them.
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

    // Classify before the drain: `s_data[d_mark..]` is exactly this node's
    // own map, and its descendants have already drained back to their marks.
    let role = b.role_at(d_mark);

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
    n.role = role;
    n.data = data;
    n.attrs = attrs;
    n.style = style;
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two tables' literals must already be normalised, because
    /// [`role_of_folded`] folds only the haystack. An unfolded needle would
    /// silently never match, and an empty one would match everything.
    #[test]
    fn every_needle_is_written_normalised_and_non_empty() {
        for (needle, role) in NEEDLES {
            assert!(!needle.is_empty(), "an empty needle matches everything ({role:?})");
            assert!(
                needle.chars().map(fold).eq(needle.chars()),
                "{needle:?} is not written folded, so it can never match"
            );
        }
        for key in VALUE_KEYS {
            assert!(!key.is_empty());
            assert!(key.chars().map(fold).eq(key.chars()), "{key:?} is not written folded");
        }
    }

    /// The table's first row is the highest-precedence role, which is what
    /// lets [`role_of_folded`] stop scanning the moment it matches.
    #[test]
    fn the_needle_table_leads_with_its_highest_precedence_role() {
        let first = NEEDLES[0].1;
        for (needle, role) in NEEDLES {
            assert!(first <= role, "{needle:?} outranks the first row");
        }
    }

    /// [`fold`] is one `char` in, one `char` out, so a folded key is as long
    /// in characters as the key and no needle can straddle a fold.
    #[test]
    fn folding_is_one_character_in_and_one_out() {
        let mut buf = String::new();
        for c in ['A', 'z', '0', 'Ａ', 'ｚ', '１', '\u{3000}', '用', '例', '-'] {
            fold_into(&mut buf, &c.to_string());
            assert_eq!(1, buf.chars().count(), "{c:?} folds to one char");
        }
    }

    /// The buffer is reused, so a short key after a long one must not leave
    /// the long one's tail behind and match on it.
    #[test]
    fn the_fold_buffer_does_not_leak_between_keys() {
        let mut buf = String::new();
        assert_eq!(Role::Example, role_of(&mut buf, "用例メタデータ"));
        assert_eq!(Role::Content, role_of(&mut buf, "rt"));
        assert_eq!(Hook::None, hook_of(&mut buf, "rt"));
        assert_eq!(Hook::InValue, hook_of(&mut buf, "content"));
        assert_eq!(Hook::None, hook_of(&mut buf, "contents"));
    }
}

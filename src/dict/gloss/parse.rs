//! Parse a dictionary glossary with `serde_json` tokenizer output written directly to the arena.
//!
//! Hand-written `DeserializeSeed` and `Visitor` seeds avoid an intermediate
//! `serde_json::Value`. `examples/gloss_arena_bench.rs` measures `Value` at 6.5
//! times the walk cost and six times the retained bytes of this shape.
//! `Value` allocates a map for each object and a `String` for each key.
//! Each walk then resolves every style key again.
//!
//! The parser has two separate robustness rules:
//!
//! - **Per-item isolation.** The glossary splits into top-level items before
//!   the parser reads each item with its own tokenizer. One unreadable item
//!   affects only that item. Every other item still renders.
//!   Yomitan and Hoshi Reader isolate each row for the same reason. A depth cap
//!   does not replace parser recovery for a bad node. A failed item keeps each
//!   node that the tokenizer read. It loses only the wrapper that parser
//!   recovery could not finish. The same policy [`Kind::Unknown`] handles an
//!   unknown tag.
//! - **A depth cap** at [`MAX_DEPTH`] limits pathological *nest depth*, not syntax.
//!
//! This parser cannot panic on input, and [`GlossDoc::parse`] returns no error.
//! Every seed accepts every JSON type that a dictionary can place in a field.
//! The parser stores or skips a malformed field, such as numeric `content`,
//! object `text`, or string `style`. Only broken JSON syntax reaches per-item
//! fallback.

use super::{
    GlossDoc, ItemType, Kind, KeyId, Node, NodeId, Role, Scalar, Span, StyleKey, Tag, MAX_DEPTH,
    NONE,
};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::HashMap;
use std::fmt;

impl GlossDoc {
    /// Parse a dictionary's raw structured-content glossary.
    ///
    /// `GlossDoc::parse` is total. An empty or unreadable payload returns an
    /// empty document because a hover has no useful work for a `Result` on its
    /// frame budget.
    pub fn parse(json: &str) -> GlossDoc {
        let mut b = Builder::new();
        let mut last = NONE;
        for item in top_level_items(json) {
            b.item(item, &mut last);
        }
        b.doc
    }
}

// Schema vocabulary
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

/// The six structural fields. Every other node field is an attribute.
/// The parser keeps attributes such as `href`, `path`, `colSpan`, and image
/// `sizeUnits`. This preserves fields that the schema adds later.
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

/// Derive a node's kind from its tag, `type`, and text state.
///
/// `div` and `span` use the container kind. `table`, `thead`, `tbody`, and
/// `tfoot` use table structure. `ruby`, `rt`, and `rp` use ruby parts.
/// The node keeps each exact tag, so this choice loses no information.
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
            // A plain string and a `type: text` item use the same kind, so
            // later code handles both with one shape.
            ItemType::Text => Kind::Text,
            ItemType::StructuredContent => Kind::Container,
            _ if has_text => Kind::Text,
            _ => Kind::Unknown,
        },
        _ => Kind::Container,
    }
}

// Editorial role
// ---------------------------------------------------------------------------
//
// The parser derives each node's role from its `data` map on the per-hover path.
// `tools/hover-parse-bench` extracted 30 150 real glossary payloads.
// Those payloads contain 3 285 630 nodes and 671 969 distinct interned keys.
// A `data` map appears on 31% of nodes.
// The release profile used the best of seven sweeps.
// Each row used the same build with `hook_of` and `Builder::role_at` as stubs:
//
// | shape | sweep | ns/node | vs no classifier |
// |---|---:|---:|---:|
// | no classifier | 498.4 ms | - | - |
// | fold per character, inside a per-position scan | 751.0 ms | 76.9 | +50.7% |
// | fold once, `str::contains` per needle | 615.9 ms | 35.7 | +23.6% |
// | + byte-set prune | 576.0 ms | 23.6 | +15.6% |
// | + byte-window search | **554.8 ms** | **17.2** | **+11.3%** |
//
// The difference comes from interned keys. The matcher runs 0.2 times per node
// instead of once. `role_at` checks one to three staged entries per node, and
// that cost stays below this measurement's noise floor.
//
// The first row explains the other rows. A character-by-character substring
// scan decodes each key once for every needle and start position. It costs 4.5
// times more than the shipped shape.

/// Role names that the classifier matches as substrings after it folds the text.
/// It checks a `data` key or a value for one of [`VALUE_KEYS`].
///
/// The table in `docs/research/dict-shapes.md` supplies every row.
/// It adds no other role name. Two table properties guide these rows:
///
/// - **Bilingual input favors Japanese.** Four dictionaries contain 755 264
///   example nodes under Japanese keys and about 62 000 under ASCII keys.
///   An ASCII-only match misses twelve of every thirteen example nodes.
///   The same pattern applies to `出典`, `参照`, and `解説`.
/// - **The substring rule covers variants.** 旺文社 全訳古語辞典 has eleven
///   `用例`-prefixed keys, including `用例訳`, `用例引用`, `用例活用`,
///   `用例メタデータ`, and `用例囲みG`. 大辞林 has `慣用例` and `用例注釈`.
///   One substring needle covers all thirteen variants. Equality needs
///   thirteen rows and still misses one spelling.
///
/// Nine Japanese example keys reduce to two needles. `用例`, `用例G`,
/// `用例訳`, `用例引用`, `用例活用`, `慣用例`, and `囲み用例` contain
/// `用例`. `例文` and `識別例文` contain `例文`.
/// The classifier folds case, so the ASCII spellings `example-sentence*`,
/// `example-keyword`, `examples`, `example_N`, `example`, `ExampleG`, and
/// `ExampleC` all become `example`.
///
/// The classifier does not use bare `例`. That needle would catch `語例`,
/// `熟語例G`, `俳句例`, and `作品例`, which add 4 221 of 832 543
/// example occurrences, or 0.5%. It would also catch `凡例`, `例外`, and
/// `例え` outside the corpus. Example is a *droppable* role.
/// This broad needle could remove content, so the classifier rejects it.
const NEEDLES: [(&str, Role); 13] = [
    // Yomitan's own convention. This is the only role that moves content.
    // Other roles hide content.
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

/// The three keys whose values can carry a role instead of their key names.
///
/// `content` follows Yomitan's structured-content convention
/// (`data.content = "example-sentence"`). `name` and `class` are HTML
/// attributes that EPUB-to-Yomitan converters place in `data`.
/// Their values carry the role. 三省堂国語辞典 writes `name=用例`, and
/// 旺文社 全訳古語辞典 writes `class=fill 用例 FM`.
///
/// The classifier checks only these three keys. If it checked every value, an
/// `alt` text or `href` could set a role. The census has `alt=［例］` on
/// 24 706 gaiji images. These are characters that a font lacks.
/// It has `href=$yourei` on 91 859 links whose target happens to be an example
/// lookup. A role there would hide a word or remove a link.
/// These three keys are conventions, not content.
const VALUE_KEYS: [&str; 3] = ["content", "name", "class"];

/// The hook for an interned key name. The parser resolves it once per key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hook {
    /// The key name supplies the role.
    Named(Role),
    /// A key in [`VALUE_KEYS`] can supply the role in its value.
    InValue,
    /// The key supplies no role. Common examples are `ruby`, `rt`, `img`, `div`, and `xmlns`.
    None,
}

/// A 256-bit set of bytes that a string contains.
///
/// The fold pass builds this set while it reads each byte.
/// [`NEEDLE_LEADS`] uses it before the classifier searches for a needle.
/// Needles start with nine possible bytes: `a`, `e`, `p`, `x`, and five CJK
/// lead bytes. Most `data` keys contain none of them, including `ruby`, `rt`,
/// `img`, `div`, `body`, `html`, `full`, `rb`, and `td`.
/// The classifier rejects those keys with four machine words instead of
/// thirteen substring searches.
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

/// First bytes for all needles, derived from [`NEEDLES`].
/// Do not duplicate this set beside the table. A duplicate can drift silently
/// when a row changes and then classify nothing.
const NEEDLE_LEADS: ByteSet = {
    let mut set = ByteSet([0; 4]);
    let mut i = 0;
    while i < NEEDLES.len() {
        set = set.with(NEEDLES[i].0.as_bytes()[0]);
        i += 1;
    }
    set
};

/// Normalize one character for role checks.
///
/// The classifier applies three folds that the corpus contains or converters
/// can produce:
///
/// - **Case**, ASCII only. 旺文社漢字典 writes `ExampleG` and `ExampleC`,
///   while Jitendex writes `example-sentence`. Japanese has no case.
/// - **Width**, full-width ASCII letters and digits to half-width.
///   Eight to eleven census dictionaries came from HTML or EPUB.
///   Their source markup can place `Ａ` and `１` in a class name as readily as
///   `A` and `1`. One `char` compare handles either spelling.
/// - **The ideographic space**, U+3000 to U+0020. A Japanese class list can
///   use `用例　FM` as readily as `用例 FM`.
///
/// The classifier does not use Unicode case conversion, NFKC, or kana width.
/// Full NFKC would fold half-width katakana and CJK compatibility ideographs.
/// A Japanese dictionary uses those characters as content, not role hooks.
/// Full NFKC also needs more than one `char`, so it would allocate on the
/// per-hover parse path for no corpus key.
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

/// Read `text` once. Return its byte set, or `None` if [`fold`] changes a byte.
///
/// Most calls use the fast path. [`fold`] changes ASCII uppercase, full-width
/// ASCII U+FF10..U+FF5A, and U+3000. The latter two have UTF-8 lead bytes
/// `0xEF` and `0xE3`.
/// A string with none of those values is already folded. The classifier
/// searches it directly and never touches the scratch buffer.
/// `content`, `sense`, `glossary`, `example-sentence`, `part-of-speech-info`,
/// `用例`, and `出典` use this path. `ExampleG` and `fill 用例 FM` use the slow
/// path.
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

/// Fold `text` into `buf` and replace its previous content.
/// Return the byte set of the folded text.
///
/// [`scan`] selects this slow path only when it finds a character to rewrite.
/// `buf` is one reusable scratch string in `Builder`. It allocates only when
/// it grows to the longest key or value in a document.
/// That allocation holds tens of bytes and occurs once per parse.
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

/// Return the role that a folded key name or convention value names.
/// Return [`Role::Content`] when no role matches.
///
/// The classifier applies three filters before any substring search.
/// Each filter reads the byte set from the same fold pass:
///
/// 1. Check [`NEEDLE_LEADS`] once. If `seen` has none, return
///    [`Role::Content`]. Keys such as `ruby`, `rt`, `img`, `div`, `body`,
///    `html`, `full`, `rb`, and `td` pass four machine-word checks.
/// 2. Check each needle's lead byte. If `seen` lacks it, skip the substring
///    search. Thus `content` checks `example` alone. One bit test replaces
///    twelve searches.
/// 3. Compare each needle's role with the current best. Skip a row that cannot
///    improve the result. Stop when the first table role matches because no
///    role can outrank it.
///
/// The smallest role that matches wins. Therefore, table row order does not
/// change the result. See [`Role`]'s precedence note.
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

/// Return true when `hay` contains `needle`. Both inputs are folded.
///
/// A byte-window compare costs less than [`str::contains`], which builds a
/// two-way searcher for each call. Needles contain four to fourteen bytes,
/// while keys contain tens of bytes.
/// Both inputs are folded, so byte comparison equals character comparison.
/// UTF-8 never uses a lead byte as a continuation byte, so each match starts
/// at a character boundary.
fn holds(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Return the role that `text` names after folding.
fn role_of(buf: &mut String, text: &str) -> Role {
    match scan(text) {
        Some(seen) => role_of_folded(text, seen),
        None => {
            let seen = fold_into(buf, text);
            role_of_folded(buf, seen)
        }
    }
}

/// Resolve the hook for `name` once when the parser interns the key.
fn hook_of(buf: &mut String, name: &str) -> Hook {
    match scan(name) {
        Some(seen) => hook_of_folded(name, seen),
        None => {
            let seen = fold_into(buf, name);
            hook_of_folded(buf, seen)
        }
    }
}

/// Resolve a hook from a folded key name and its byte set.
///
/// The value-key check uses equality, not substring search. It is the only
/// classifier check that uses equality.
/// Nine census keys contain `content`, `name`, or `class` without equal names:
/// `contents`, `Contents`, `jukugoitemcontent`, `CampaignName`, and five more.
/// They occur on 86 747 nodes. If their values supplied roles,
/// `filename=example.png` could hide a node.
/// The three conventions use exact spellings, so this check uses exact equality.
fn hook_of_folded(folded: &str, seen: ByteSet) -> Hook {
    match role_of_folded(folded, seen) {
        Role::Content if VALUE_KEYS.contains(&folded) => Hook::InValue,
        Role::Content => Hook::None,
        role => Hook::Named(role),
    }
}

// Split glossary into items
// ---------------------------------------------------------------------------

/// Return the glossary's top-level items as byte slices of the payload.
///
/// This bracket- and quote-aware scan does not parse JSON.
/// It supports parser recovery because one unreadable item cannot consume the
/// rest of the entry. A single `serde_json` tokenizer over the full array
/// cannot recover after a syntax error because its position is no longer usable.
///
/// If the payload is not an array, return one item.
/// The item parser rejects it unless it is a string or an object.
/// This matches the schema, which allows only an array of strings and objects.
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
                // The glossary array closes here. Bytes after this close are
                // trailing input, not an item.
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
        // If the array has no close, send the remaining bytes as one item.
        // This item degrades alone, so earlier items remain available.
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

// Builder
// ---------------------------------------------------------------------------

/// Parse-time state for `GlossDoc`.
/// `intern` and the three temporary vectors serve the builder only.
/// They never reach [`GlossDoc`], so a cached document keeps no `HashMap` or
/// scratch vector.
struct Builder {
    doc: GlossDoc,
    intern: HashMap<Box<str>, KeyId>,
    /// One [`Hook`] for each interned key, parallel with `doc.keys`.
    ///
    /// This vector gives the classifier one result for each interned key.
    /// The folded substring scan over 13 rows runs once for each distinct
    /// key in a document. It runs for tens of names, not for every node.
    /// For each node, the classifier checks one to three staged entries and
    /// indexes this vector with [`KeyId`]. The operation uses an array load and
    /// an integer compare.
    ///
    /// `doc.keys` sets the vector size, so attribute and structural names also
    /// have entries. `href`, `colSpan`, and `用例` each cost one scan.
    /// The first two yield [`Hook::None`]. A parallel array indexed by
    /// [`KeyId`] costs less than special cases for a few scans.
    key_hooks: Vec<Hook>,
    /// One scratch string for the classifier.
    /// The classifier stores the last folded key or value here and reuses this
    /// string for the whole parse. After the first key, a fold needs no new
    /// allocation.
    s_fold: String,
    /// Temporary vectors keep each node's key-value entries contiguous.
    /// The parser reads child nodes between parent entries, then flushes these
    /// entries when the node object closes.
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

    /// Parse one glossary item and link it.
    /// If the parser cannot read it, degrade only that item.
    fn item(&mut self, json: &str, last: &mut NodeId) {
        let before = self.doc.nodes.len() as NodeId;
        let mut de = serde_json::Deserializer::from_str(json);
        let parsed = ItemSeed { b: self }.deserialize(&mut de).and_then(|id| {
            de.end()?;
            Ok(id)
        });
        match parsed {
            Ok(Some(id)) => self.link(NONE, last, id),
            // A value that is not a string or object is not a glossary item.
            Ok(None) => {}
            Err(_) => {
                self.doc.degraded_items += 1;
                // The tokenizer already placed each node it read in the arena.
                // Keep those nodes and drop only the wrapper that parser
                // recovery could not finish.
                // Clear the temporary vectors because the aborted node did not
                // flush them. The next item must not adopt those entries.
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

    /// Remove bytes that the immediately previous [`push_text`] call appended.
    ///
    /// `tag` and `type` names arrive as text, then become enum values.
    /// The parser does not need to retain those bytes.
    /// This preserves the text buffer for actual content.
    /// This `Span` identifies a source-range in the text buffer.
    /// A hover can parse more than 1,000 nodes. Four bytes per node then form
    /// a measurable part of the heap that the arena is meant to reduce.
    fn unpush_text(&mut self, s: Span) {
        if (s.at + s.len) as usize == self.doc.text.len() {
            self.doc.text.truncate(s.at as usize);
        }
    }

    /// Intern a key name and classify it once for each distinct name.
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

    /// Return the editorial role for the node whose `data` entries start at
    /// `mark`.
    ///
    /// Return the smallest role among this node's own `data` entries.
    /// Return [`Role::Content`] when none names a role.
    /// Run once for each node with a `data` map. Such a map has one to three
    /// entries.
    ///
    /// Do not inherit a role. A role marks a *subtree*, and each renderer stops
    /// at a rejected node. Descendants therefore follow the root without
    /// another role lookup.
    /// A descendant remains addressable by itself. `Selection::Nodes` can render
    /// a sense selected from an example block, which supports the Anki sense
    /// picker.
    ///
    /// A node without a `data` map stages no entries. The loop then has no work,
    /// and the node keeps the content role.
    fn role_at(&mut self, mark: usize) -> Role {
        // Read staged entries while the fold buffer changes. Destructure
        // `Builder` because `self.` syntax cannot express these separate borrows.
        let Builder { doc, key_hooks, s_fold, s_data, .. } = self;
        let mut best = Role::Content;
        for &(key, value) in &s_data[mark..] {
            let role = match key_hooks[key as usize] {
                Hook::Named(role) => role,
                Hook::InValue => match value {
                    Scalar::Text(span) => role_of(s_fold, doc.span(span)),
                    // A convention key with a number, bool, or null has no role.
                    // It still marks a block, which `GlossDoc::has_marker` checks
                    // separately.
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

// Seeds
// ---------------------------------------------------------------------------

/// A glossary item can be a string or object. Other JSON types are not items.
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

/// A scalar value for `data`, an attribute, or a style property.
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

    /// Read `textDecorationLine` as one space-separated string.
    /// The schema defines this property as the one list-valued property.
    /// This join runs at parse time, so the resolved record needs no list variant.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scalar, A::Error> {
        let at = self.b.doc.text.len() as u32;
        let mut end = at;
        while let Some(v) = seq.next_element_seed(ScalarSeed { b: &mut *self.b })? {
            // Add a separator only after a text element. Other elements add no text.
            if matches!(v, Scalar::Text(_)) {
                end = self.b.doc.text.len() as u32;
                self.b.doc.text.push(' ');
            }
        }
        Ok(Scalar::Text(Span { at, len: end - at }))
    }

    /// Consume an object where the schema expects a scalar and record
    /// `Scalar::Null`. This position cannot hold a node, so another nested
    /// representation would add state without useful content.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scalar, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Scalar::Null)
    }
}

/// Intern a map key. Return `None` for a non-string key.
/// JSON cannot produce a non-string key, but another `Deserializer` can.
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

/// Read the `data` object. Its keys are dictionary-specific and unbounded.
/// The role classifier therefore reads roles from key names instead of a fixed
/// list. The stylesheet matcher also indexes these keys.
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

/// Read the `style` object into the node's style record.
/// Drop an unrecognized property. The record has a closed set, as does the
/// schema property list.
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

/// Read any value in a node position: a string, object, or array of either.
/// Flatten arrays. A node's children then become the concatenation of every
/// array level under `content`, as in Yomitan's renderer.
struct Kids<'a> {
    b: &'a mut Builder,
    parent: NodeId,
    last: &'a mut NodeId,
    /// The depth of the node that this seed creates.
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
            // Consume but do not parse this map. The tokenizer must walk it, but
            // no value below the cap reaches the arena.
            // Outer levels still render, and the parse terminates.
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

/// Read one structured-content object into one node.
///
/// Create the node before its fields arrive. Assign fields as the map yields
/// them. If JSON breaks part way through, the node still keeps the tag and
/// children that the tokenizer read.
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
        // Intern every key, including a non-structural key. The parser needs one
        // hash lookup, and attributes need the key id.
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
                        // Keep an unknown name as an attribute. The tree stays
                        // lossless, and the node keeps its children.
                        None => {
                            tag = Tag::Other;
                            b.s_attr.push((key_id, v));
                        }
                    },
                    // A non-string `tag` value supplies no tag. Keep its value
                    // as an attribute.
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

    // Classify before the drain. `s_data[d_mark..]` contains this node's map,
    // and child nodes have already returned their entries to their marks.
    let role = b.role_at(d_mark);

    // Flush this node's staged entries as one contiguous range. Child nodes
    // already flushed their entries, so entries above this mark belong to this node.
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

    /// Keep both tables normalized and non-empty.
    /// [`role_of_folded`] folds only the haystack. An unfolded needle never
    /// matches, and an empty needle matches every string.
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

    /// Keep the highest-precedence role in the first table row.
    /// [`role_of_folded`] stops when that role matches.
    #[test]
    fn the_needle_table_leads_with_its_highest_precedence_role() {
        let first = NEEDLES[0].1;
        for (needle, role) in NEEDLES {
            assert!(first <= role, "{needle:?} outranks the first row");
        }
    }

    /// [`fold`] maps one `char` to one `char`.
    /// A folded key keeps its character count, so no needle can cross a fold.
    #[test]
    fn folding_is_one_character_in_and_one_out() {
        let mut buf = String::new();
        for c in ['A', 'z', '0', 'Ａ', 'ｚ', '１', '\u{3000}', '用', '例', '-'] {
            fold_into(&mut buf, &c.to_string());
            assert_eq!(1, buf.chars().count(), "{c:?} folds to one char");
        }
    }

    /// Clear the reused buffer before each key.
    /// A shorter key after a longer key must not retain the longer suffix.
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

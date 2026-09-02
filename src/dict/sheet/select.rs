//! The bounded selector grammar that this module compiles and matches.
//!
//! Corpus measurement sets the bound.
//! Structured content has no `class` attribute.
//! A dictionary author can therefore reach content only through a tag name.
//! The other path uses the `data-*` attributes that Yomitan derives from the
//! `data` map of a node.
//! 897 of the 908 box-model rules in the corpus select through exactly these
//! two surfaces (`docs/research/dict-shapes.md`).
//! This module supports only these forms:
//!
//! - a tag name resolved to the schema's [`Tag`]
//! - a `data-*` attribute tested for presence or against one `=` value
//! - compound selectors with descendant and child combinators
//! - structural pseudo-classes used by the corpus: `:first-child`,
//!   `:first-of-type`, `:last-of-type`, `:nth-child`, `:nth-of-type` and
//!   `:nth-last-of-type`. The list also has `:last-child` and
//!   `:nth-last-child`, which cost the same two matcher lines as related forms
//! - selector-list forms `:is`, `:where`, `:not` and `:has`, whose arguments
//!   are compounds. `td:has([data-sc親字])` uses content like a bare attribute
//!   selector.
//!
//! Every other form fails the compile step. Examples are a class, an id, `*`,
//! a sibling combinator, and a pseudo-element.
//! Other examples are `:hover`, `:link`, an attribute operator other than `=`,
//! and a tag absent from the schema.
//! The caller drops the whole rule and counts the drop.
//! CSS also drops a whole rule with an unreadable selector.
//! This behavior prevents partial application of a rule for an arrangement
//! that this build cannot represent.

use crate::dict::gloss::{GlossDoc, Kind, NodeId, Span, Tag};

/// The relation between this compound and the compound to its left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Combinator {
    Descendant,
    Child,
}

/// One compound selector.
#[derive(Clone, Copy)]
pub(super) struct Compound {
    /// The relation between this compound and its left neighbor.
    /// The leftmost compound has no relation, and this field stores
    /// `Descendant`.
    pub(super) combinator: Combinator,
    /// The compound's tag. [`Tag::None`] means that it names no tag.
    pub(super) tag: Tag,
    /// Points into [`Pool::tests`].
    pub(super) tests: Span,
    /// Points into [`Pool::pseudos`].
    pub(super) pseudos: Span,
}

/// One `data-*` attribute test.
pub(super) struct Test {
    /// Points into [`Pool::attrs`].
    pub(super) attr: u32,
    /// `None` tests presence alone.
    pub(super) value: Option<Box<str>>,
}
/// A compound selector with no pseudo-classes. A selector-list pseudo must use
/// this form.
///
/// Measurement sets this restriction. Every `:has`, `:is`, `:where` and
/// `:not` argument in the corpus is one compound:
/// `td[data-sc親字]`, `[data-sc外字][data-sc全丸]`,
/// `> span[data-sc-zoomkanji]`, `a` and `ruby`.
/// A pseudo inside a pseudo matches nothing. It would also remove the
/// matcher's termination guarantee without a recursion budget.
#[derive(Clone, Copy)]
pub(super) struct Simple {
    /// The relation to the anchor. Only `:has` reads this field.
    /// `:has(x)` searches descendants. `:has(> x)` searches children only.
    pub(super) rel: Combinator,
    pub(super) tag: Tag,
    pub(super) tests: Span,
}

pub(super) enum Pseudo {
    FirstChild,
    LastChild,
    FirstOfType,
    LastOfType,
    NthChild(Nth),
    NthLastChild(Nth),
    NthOfType(Nth),
    NthLastOfType(Nth),
    /// `:is(...)` and `:where(...)` let one argument match this node.
    /// The forms differ only in specificity. The compile step resolves it.
    Is(Span),
    /// For `:not(...)`, no argument can match this node.
    Not(Span),
    /// For `:has(...)`, one argument matches a descendant.
    /// The argument matches a child when an author writes it as `> x`.
    Has(Span),
}

/// An `An+B` argument.
#[derive(Clone, Copy)]
pub(super) struct Nth {
    pub(super) a: i32,
    pub(super) b: i32,
}

impl Nth {
    /// Reports whether a 1-based sibling position satisfies this argument.
    fn holds(self, pos: u16) -> bool {
        let p = i32::from(pos);
        if self.a == 0 {
            return p == self.b;
        }
        let off = p - self.b;
        off % self.a == 0 && off / self.a >= 0
    }
}

/// Every selector part in one sheet, stored in shared arenas.
///
/// This type uses flat arenas for the same reason as the gloss node arena.
/// A compiled selector is a `Span` into a shared vector.
/// Therefore each rule needs no allocation of its own, and the whole sheet
/// occupies a few contiguous blocks.
#[derive(Default)]
pub(super) struct Pool {
    pub(super) compounds: Vec<Compound>,
    /// Attribute tests that belong to a [`Compound`].
    pub(super) tests: Vec<Test>,
    pub(super) pseudos: Vec<Pseudo>,
    pub(super) simples: Vec<Simple>,
    /// Attribute tests that belong to a [`Simple`], the argument of a pseudo.
    ///
    /// These tests need a separate arena.
    /// Tests for one compound form one contiguous span.
    /// With one shared arena, the parser would place the
    /// argument test inside the `td` compound's test span.
    /// The `td` node would then need the argument attribute.
    pub(super) simple_tests: Vec<Test>,
    /// Distinct `data-*` attribute names with selector spelling.
    /// A document resolves each name to one interned `KeyId` once.
    /// Every per-node probe then compares `u32` values.
    pub(super) attrs: Vec<String>,
}

impl Pool {
    /// Returns the current end of every arena so a failed compile can rewind.
    fn mark(&self) -> Mark {
        Mark {
            compounds: self.compounds.len(),
            tests: self.tests.len(),
            pseudos: self.pseudos.len(),
            simples: self.simples.len(),
            simple_tests: self.simple_tests.len(),
            attrs: self.attrs.len(),
        }
    }

    fn rewind(&mut self, m: Mark) {
        self.compounds.truncate(m.compounds);
        self.tests.truncate(m.tests);
        self.pseudos.truncate(m.pseudos);
        self.simples.truncate(m.simples);
        self.simple_tests.truncate(m.simple_tests);
        self.attrs.truncate(m.attrs);
    }

    /// Returns the ID of one attribute name. This function interns the name.
    fn attr_id(&mut self, name: &str) -> u32 {
        match self.attrs.iter().position(|a| a == name) {
            Some(at) => at as u32,
            None => {
                self.attrs.push(name.to_string());
                (self.attrs.len() - 1) as u32
            }
        }
    }
}

struct Mark {
    compounds: usize,
    tests: usize,
    pseudos: usize,
    simples: usize,
    simple_tests: usize,
    attrs: usize,
}

/// One compiled selector.
pub(super) struct Compiled {
    /// Points into [`Pool::compounds`]. The subject compound comes last.
    pub(super) sel: Span,
    /// CSS specificity, packed so that one comparison orders two rules.
    pub(super) spec: u32,
}

/// Accumulated selector specificity.
///
/// This type has no `a` field. An id selector fails the compile step, so the
/// `a` count is always zero. A field for it could hold only one value.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Spec {
    /// Attribute tests and pseudo-classes.
    b: u32,
    /// Tag names.
    c: u32,
}

impl Spec {
    fn packed(self) -> u32 {
        (self.b.min(0xFFFF) << 16) | self.c.min(0xFFFF)
    }

    fn add(&mut self, other: Spec) {
        self.b += other.b;
        self.c += other.c;
    }
}

/// Compiles one complex selector. On failure, rewinds the pool.
pub(super) fn compile(sel: &str, pool: &mut Pool) -> Option<Compiled> {
    let mark = pool.mark();
    match parse_complex(sel, pool) {
        Some(c) => Some(c),
        None => {
            pool.rewind(mark);
            None
        }
    }
}

fn parse_complex(sel: &str, pool: &mut Pool) -> Option<Compiled> {
    let b = sel.as_bytes();
    let at = pool.compounds.len() as u32;
    let mut spec = Spec::default();
    let mut i = 0usize;
    let mut combinator = Combinator::Descendant;
    loop {
        i = skip_ws(b, i);
        let (compound, part_spec) = parse_compound(sel, &mut i, combinator, pool)?;
        spec.add(part_spec);
        pool.compounds.push(compound);
        // Read a combinator or find the end.
        let after_ws = skip_ws(b, i);
        if after_ws >= b.len() {
            break;
        }
        combinator = match b[after_ws] {
            b'>' => {
                i = after_ws + 1;
                Combinator::Child
            }
            // `+` and `~` select a sibling before this node.
            // The first-child and next-sibling links in this arena cannot
            // walk backward.
            // 26 corpus selectors use one of these combinators. The compiler
            // drops each rule.
            b'+' | b'~' => return None,
            // Whitespace is the combinator here, and it is significant.
            _ if after_ws > i => Combinator::Descendant,
            _ => return None,
        };
        i = skip_ws(b, i);
        if i >= b.len() {
            // A combinator at the end of the text selects nothing.
            return None;
        }
    }
    let len = pool.compounds.len() as u32 - at;
    (len > 0).then_some(Compiled { sel: Span { at, len }, spec: spec.packed() })
}
/// Parses one compound from `*i` to the next combinator or the input end.
fn parse_compound(
    sel: &str,
    i: &mut usize,
    combinator: Combinator,
    pool: &mut Pool,
) -> Option<(Compound, Spec)> {
    let b = sel.as_bytes();
    let mut spec = Spec::default();
    let mut tag = Tag::None;
    let tests_at = pool.tests.len() as u32;
    let pseudos_at = pool.pseudos.len() as u32;
    let mut parts = 0usize;
    loop {
        if *i >= b.len() {
            break;
        }
        match b[*i] {
            b'[' => {
                let close = end_of(b, *i, b'[', b']')?;
                let test = parse_test(&sel[*i + 1..close - 1], pool)?;
                pool.tests.push(test);
                spec.b += 1;
                *i = close;
            }
            b':' => {
                let (pseudo, part) = parse_pseudo(sel, i, pool)?;
                pool.pseudos.push(pseudo);
                spec.add(part);
            }
            // Structured content cannot expose a class, an id, a universal
            // selector, a namespace, or an unresolved nesting marker.
            // The compiler drops the rule for each form.
            b'.' | b'#' | b'*' | b'|' | b'&' => return None,
            b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'+' | b'~' | b',' => break,
            _ => {
                if tag != Tag::None || parts > 0 {
                    // This grammar rejects a second tag name and a tag after
                    // a test.
                    return None;
                }
                let end = end_of_ident(b, *i);
                if end == *i {
                    return None;
                }
                tag = tag_of(&sel[*i..end])?;
                spec.c += 1;
                *i = end;
            }
        }
        parts += 1;
    }
    if parts == 0 {
        return None;
    }
    Some((
        Compound {
            combinator,
            tag,
            tests: Span { at: tests_at, len: pool.tests.len() as u32 - tests_at },
            pseudos: Span { at: pseudos_at, len: pool.pseudos.len() as u32 - pseudos_at },
        },
        spec,
    ))
}

/// One attribute selector with its brackets removed.
fn parse_test(inner: &str, pool: &mut Pool) -> Option<Test> {
    let (name, value) = match inner.split_once('=') {
        None => (inner.trim(), None),
        Some((head, tail)) => {
            // The compiler accepts only `=`.
            // A `~=`, `^=`, `$=`, `*=`, or `|=` operator matches a family of
            // values. 15 corpus selectors also reach a non-`data-*` attribute
            // with one of these operators. The compiler drops both forms.
            if head.ends_with(['~', '^', '$', '*', '|']) {
                return None;
            }
            (head.trim(), Some(unquote(tail.trim())?))
        }
    };
    if name.is_empty() || !name.to_ascii_lowercase().starts_with("data-") {
        return None;
    }
    Some(Test { attr: pool.attr_id(&name.to_ascii_lowercase()), value })
}
/// The attribute value of a selector, with its quotes removed.
fn unquote(text: &str) -> Option<Box<str>> {
    let inner = match text.as_bytes().first()? {
        q @ (b'"' | b'\'') => text.strip_prefix(*q as char)?.strip_suffix(*q as char)?,
        _ => text,
    };
    Some(inner.into())
}

/// Parses one pseudo-class and advances past its argument list.
fn parse_pseudo(sel: &str, i: &mut usize, pool: &mut Pool) -> Option<(Pseudo, Spec)> {
    let b = sel.as_bytes();
    // A pseudo-element is not a node, so this tree cannot resolve a rule that
    // names one. The compiler drops 18 corpus selectors for this reason.
    if b.get(*i + 1) == Some(&b':') {
        return None;
    }
    let head = *i + 1;
    let end = end_of_ident(b, head);
    let name = sel[head..end].to_ascii_lowercase();
    let (arg, next) = match b.get(end) {
        Some(b'(') => {
            let close = end_of(b, end, b'(', b')')?;
            (Some(sel[end + 1..close - 1].trim()), close)
        }
        _ => (None, end),
    };
    *i = next;
    // The `SUPPORTED_PSEUDO_CLASSES` array is the only gate.
    // `tools/dict-census` reads this array to score the corpus.
    // A name outside the array must not compile, regardless of the match arms
    // below.
    //
    // The gate also handles legacy one-colon forms.
    // `:before` and its three siblings are pseudo-elements, even with one
    // colon. A pseudo-element is not a node, and the array omits these names.
    // Therefore this code rejects them here and needs no separate match arm.
    // The `:is` aliases use the same rule.
    // `:matches`, `:-moz-any`, `:-webkit-any`, and `:any` do not occur in
    // corpus shapes. `docs/research/dict-shapes.md` records none of them.
    // Yomitan uses Chromium, where `:is` needs no prefix.
    // Match arms for these aliases would be unreachable because the gate
    // rejects their names.
    if !super::SUPPORTED_PSEUDO_CLASSES.contains(&name.as_str()) {
        return None;
    }
    let structural = |p: Pseudo| Some((p, Spec { b: 1, c: 0 }));
    let nth = |p: fn(Nth) -> Pseudo| {
        let n = parse_nth(arg?)?;
        Some((p(n), Spec { b: 1, c: 0 }))
    };
    match (name.as_str(), arg) {
        ("first-child", None) => structural(Pseudo::FirstChild),
        ("last-child", None) => structural(Pseudo::LastChild),
        ("first-of-type", None) => structural(Pseudo::FirstOfType),
        ("last-of-type", None) => structural(Pseudo::LastOfType),
        ("nth-child", Some(_)) => nth(Pseudo::NthChild),
        ("nth-last-child", Some(_)) => nth(Pseudo::NthLastChild),
        ("nth-of-type", Some(_)) => nth(Pseudo::NthOfType),
        ("nth-last-of-type", Some(_)) => nth(Pseudo::NthLastOfType),
        ("is", Some(args)) => {
            let (span, spec) = parse_args(args, false, pool)?;
            Some((Pseudo::Is(span), spec))
        }
        // `:where` matches like `:is`. It adds no specificity, which is its
        // purpose.
        ("where", Some(args)) => {
            let (span, _) = parse_args(args, false, pool)?;
            Some((Pseudo::Is(span), Spec::default()))
        }
        ("not", Some(args)) => {
            let (span, spec) = parse_args(args, false, pool)?;
            Some((Pseudo::Not(span), spec))
        }
        ("has", Some(args)) => {
            let (span, spec) = parse_args(args, true, pool)?;
            Some((Pseudo::Has(span), spec))
        }
        // `:hover` and `:link` report pointer and history state. Gloss content
        // in a popup has neither. The compiler drops 20 corpus selectors.
        _ => None,
    }
}

/// Selector-list pseudo arguments.
///
/// `relative` allows each argument to start with `>`, as `:has()` requires.
/// CSS gives `:is`, `:not`, and `:has` the specificity of their most specific
/// argument. This function returns that specificity.
fn parse_args(args: &str, relative: bool, pool: &mut Pool) -> Option<(Span, Spec)> {
    let at = pool.simples.len() as u32;
    let mut best = Spec::default();
    for arg in super::scan::split_selectors(args) {
        let mut text = arg.trim();
        let mut rel = Combinator::Descendant;
        if let Some(rest) = text.strip_prefix('>') {
            if !relative {
                return None;
            }
            rel = Combinator::Child;
            text = rest.trim();
        }
        let (simple, spec) = parse_simple(text, pool)?;
        pool.simples.push(Simple { rel, ..simple });
        best = best.max(spec);
    }
    let len = pool.simples.len() as u32 - at;
    (len > 0).then_some((Span { at, len }, best))
}

/// Parses one pseudo argument as a compound with an optional tag, tests, and no other parts.
fn parse_simple(text: &str, pool: &mut Pool) -> Option<(Simple, Spec)> {
    let b = text.as_bytes();
    let mut spec = Spec::default();
    let mut tag = Tag::None;
    let tests_at = pool.simple_tests.len() as u32;
    let mut parts = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'[' => {
                let close = end_of(b, i, b'[', b']')?;
                let test = parse_test(&text[i + 1..close - 1], pool)?;
                pool.simple_tests.push(test);
                spec.b += 1;
                i = close;
            }
            _ => {
                if tag != Tag::None || parts > 0 {
                    return None;
                }
                let end = end_of_ident(b, i);
                if end == i {
                    return None;
                }
                tag = tag_of(&text[i..end])?;
                spec.c += 1;
                i = end;
            }
        }
        parts += 1;
    }
    (parts > 0).then(|| {
        (
            Simple {
                rel: Combinator::Descendant,
                tag,
                tests: Span {
                    at: tests_at,
                    len: pool.simple_tests.len() as u32 - tests_at,
                },
            },
            spec,
        )
    })
}

/// Accepts `even`, `odd`, an integer, or `An+B`.
fn parse_nth(arg: &str) -> Option<Nth> {
    let text: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = text.to_ascii_lowercase();
    match lower.as_str() {
        "even" => return Some(Nth { a: 2, b: 0 }),
        "odd" => return Some(Nth { a: 2, b: 1 }),
        _ => {}
    }
    let Some((head, tail)) = lower.split_once('n') else {
        // The corpus uses one plain 1-based index.
        return Some(Nth { a: 0, b: lower.parse().ok()? });
    };
    let a = match head {
        "" | "+" => 1,
        "-" => -1,
        _ => head.parse().ok()?,
    };
    let b = match tail {
        "" => 0,
        _ => tail.strip_prefix('+').unwrap_or(tail).parse().ok()?,
    };
    Some(Nth { a, b })
}

/// Returns a selector's tag name as the schema's enum.
///
/// A name absent from the schema cannot match a node in this tree.
/// The compiler drops its rule.
/// `body`, `h3`, `p`, and `table-container` account for 5 corpus selectors.
fn tag_of(name: &str) -> Option<Tag> {
    crate::dict::gloss::tag_for(&name.to_ascii_lowercase())
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Returns the position after the identifier at `i`, or `i` when none exists.
///
/// A non-ASCII byte counts as an identifier byte.
/// `[data-sc常用外マーク]` and `p.字音` are real corpus spellings.
/// A CJK tag name must reach [`tag_of`] so the parser rejects it, rather than
/// treating it as a combinator.
fn end_of_ident(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 => i += 1,
            _ => break,
        }
    }
    i.min(b.len())
}
/// Returns the position after the closer that matches the opener at `i`.
/// Returns `None` when the input never closes it.
fn end_of(b: &[u8], i: usize, opener: u8, closer: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = i;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                let mut j = i + 1;
                while j < b.len() && b[j] != q {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                i = (j + 1).min(b.len());
                continue;
            }
            c if c == opener => depth += 1,
            c if c == closer => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ---- matches ----

/// A node's 1-based position among its siblings, as CSS counts it.
///
/// The code creates this table once per document and only when a sheet asks
/// for it. 8 of the 14 corpus stylesheets use no structural pseudo-class.
/// For those sheets, the code allocates no position table.
#[derive(Clone, Copy, Default)]
pub(super) struct Pos {
    pub(super) child: u16,
    pub(super) children: u16,
    /// Counts siblings that share this node's [`Tag`].
    /// An unrecognized tag keeps its name as an attribute instead of an enum
    /// value. Therefore every `Tag::Other` sibling counts as one type.
    /// No corpus `of-type` selector names these nodes.
    pub(super) of_type: u16,
    pub(super) of_types: u16,
}

/// Data that a match needs beyond the tree itself.
pub(super) struct Ctx<'a> {
    pub(super) doc: &'a GlossDoc,
    /// One entry per interned `KeyId`: the ID of the `data-*` attribute that
    /// the key derives from, or [`NO_ATTR`].
    pub(super) attr_of_key: &'a [u32],
    /// Empty when the sheet uses no structural pseudo-class.
    pub(super) pos: &'a [Pos],
}

/// No selector in this sheet names the attribute a `data` key derives from.
pub(super) const NO_ATTR: u32 = u32::MAX;

/// Reports whether the selector matches the node at the end of `path`.
///
/// `path` contains the chain from a glossary item to the subject.
/// An ancestor constraint moves left through this chain.
/// The matcher moves right to left and backtracks after a failed step, as a
/// browser does.
/// The subject is known, so each step left prunes the search.
pub(super) fn matches(pool: &Pool, sel: Span, ctx: &Ctx<'_>, path: &[NodeId]) -> bool {
    let last = sel.at + sel.len - 1;
    !path.is_empty() && matches_at(pool, sel.at, last, ctx, path, path.len() - 1)
}

fn matches_at(
    pool: &Pool,
    left: u32,
    ci: u32,
    ctx: &Ctx<'_>,
    path: &[NodeId],
    pi: usize,
) -> bool {
    let compound = &pool.compounds[ci as usize];
    if !compound_matches(pool, compound, ctx, path[pi]) {
        return false;
    }
    if ci == left {
        return true;
    }
    match compound.combinator {
        Combinator::Child => pi > 0 && matches_at(pool, left, ci - 1, ctx, path, pi - 1),
        Combinator::Descendant => {
            (0..pi).rev().any(|j| matches_at(pool, left, ci - 1, ctx, path, j))
        }
    }
}

fn compound_matches(pool: &Pool, c: &Compound, ctx: &Ctx<'_>, id: NodeId) -> bool {
    if c.tag != Tag::None && ctx.doc.node(id).tag != c.tag {
        return false;
    }
    let tests = &pool.tests[c.tests.at as usize..(c.tests.at + c.tests.len) as usize];
    if !tests.iter().all(|t| test_matches(ctx, id, t)) {
        return false;
    }
    let pseudos = &pool.pseudos[c.pseudos.at as usize..(c.pseudos.at + c.pseudos.len) as usize];
    pseudos.iter().all(|p| pseudo_matches(pool, ctx, id, p))
}

fn test_matches(ctx: &Ctx<'_>, id: NodeId, test: &Test) -> bool {
    ctx.doc.data(id).iter().any(|(key, value)| {
        if ctx.attr_of_key.get(*key as usize).copied().unwrap_or(NO_ATTR) != test.attr {
            return false;
        }
        match &test.value {
            None => true,
            // The schema can accept a `data` value as a number or boolean.
            // Such a value has no string form here, so an `=` test fails.
            // No corpus selector tests a non-string value.
            Some(want) => ctx.doc.scalar_str(*value) == Some(&**want),
        }
    })
}

fn pseudo_matches(pool: &Pool, ctx: &Ctx<'_>, id: NodeId, p: &Pseudo) -> bool {
    let pos = ctx.pos.get(id as usize).copied().unwrap_or_default();
    match p {
        Pseudo::FirstChild => pos.child == 1,
        Pseudo::LastChild => pos.child == pos.children && pos.children > 0,
        Pseudo::FirstOfType => pos.of_type == 1,
        Pseudo::LastOfType => pos.of_type == pos.of_types && pos.of_types > 0,
        Pseudo::NthChild(n) => n.holds(pos.child),
        Pseudo::NthLastChild(n) => n.holds(pos.children + 1 - pos.child.min(pos.children)),
        Pseudo::NthOfType(n) => n.holds(pos.of_type),
        Pseudo::NthLastOfType(n) => {
            n.holds(pos.of_types + 1 - pos.of_type.min(pos.of_types))
        }
        Pseudo::Is(args) => simples(pool, *args).iter().any(|s| simple_matches(pool, ctx, id, s)),
        Pseudo::Not(args) => {
            !simples(pool, *args).iter().any(|s| simple_matches(pool, ctx, id, s))
        }
        Pseudo::Has(args) => simples(pool, *args).iter().any(|s| match s.rel {
            Combinator::Child => {
                ctx.doc.children(id).any(|kid| simple_matches(pool, ctx, kid, s))
            }
            Combinator::Descendant => has_descendant(pool, ctx, id, s),
        }),
    }
}

fn simples(pool: &Pool, span: Span) -> &[Simple] {
    &pool.simples[span.at as usize..(span.at + span.len) as usize]
}

fn simple_matches(pool: &Pool, ctx: &Ctx<'_>, id: NodeId, s: &Simple) -> bool {
    if s.tag != Tag::None && ctx.doc.node(id).tag != s.tag {
        return false;
    }
    let tests =
        &pool.simple_tests[s.tests.at as usize..(s.tests.at + s.tests.len) as usize];
    tests.iter().all(|t| test_matches(ctx, id, t))
}

/// Reports whether any node under `id` matches.
///
/// The code uses an explicit stack, not recursion.
/// The tree has a depth limit of
/// [`MAX_DEPTH`](crate::dict::gloss::MAX_DEPTH), but a subtree has no width limit.
/// `:has` is the only grammar form that looks downward.
fn has_descendant(pool: &Pool, ctx: &Ctx<'_>, id: NodeId, s: &Simple) -> bool {
    let mut stack: Vec<NodeId> = ctx.doc.children(id).collect();
    while let Some(node) = stack.pop() {
        if simple_matches(pool, ctx, node, s) {
            return true;
        }
        stack.extend(ctx.doc.children(node));
    }
    false
}

/// Computes sibling positions for every node in one tree sweep.
pub(super) fn positions(doc: &GlossDoc) -> Vec<Pos> {
    let mut out = vec![Pos::default(); doc.all_nodes().len()];
    // Two scratch tallies serve every sibling list in the document.
    // A tag tally has at most a few entries. One allocation per node would
    // cost more than the whole sweep.
    let mut totals: Vec<(Tag, u16)> = Vec::new();
    let mut seen: Vec<(Tag, u16)> = Vec::new();
    // Top-level glossary items are siblings, like the children of one node.
    // Therefore a `:first-child` rule on an item treats the first top-level
    // item as the first child.
    number(doc, || doc.items(), &mut out, &mut totals, &mut seen);
    for id in 0..doc.all_nodes().len() as NodeId {
        if doc.has_children(id) {
            number(doc, || doc.children(id), &mut out, &mut totals, &mut seen);
        }
    }
    out
}

/// Numbers one sibling list in two passes.
///
/// This function uses two passes, not one, and an iterator factory, not a
/// collected list.
/// `:last-of-type` needs the total count of the list. This walk gets that count
/// only after its first pass.
/// A second walk over the linked sibling chain costs less than a list that
/// stores every sibling.
fn number<I: Iterator<Item = NodeId>>(
    doc: &GlossDoc,
    sibs: impl Fn() -> I,
    out: &mut [Pos],
    totals: &mut Vec<(Tag, u16)>,
    seen: &mut Vec<(Tag, u16)>,
) {
    totals.clear();
    seen.clear();
    let mut children: u16 = 0;
    for id in sibs() {
        children = children.saturating_add(1);
        tally(doc.node(id).tag, totals);
    }
    for (ix, id) in sibs().enumerate() {
        let tag = doc.node(id).tag;
        out[id as usize] = Pos {
            child: (ix + 1).min(u16::MAX as usize) as u16,
            children,
            of_type: tally(tag, seen),
            of_types: totals.iter().find(|(t, _)| *t == tag).map_or(1, |(_, n)| *n),
        };
    }
}

/// Increases one tag count and returns the new count.
fn tally(tag: Tag, at: &mut Vec<(Tag, u16)>) -> u16 {
    match at.iter_mut().find(|(t, _)| *t == tag) {
        Some((_, n)) => {
            *n = n.saturating_add(1);
            *n
        }
        None => {
            at.push((tag, 1));
            1
        }
    }
}

/// Reports whether any kept selector in this sheet asks for a sibling position.
pub(super) fn needs_positions(pool: &Pool) -> bool {
    pool.pseudos.iter().any(|p| {
        !matches!(p, Pseudo::Is(_) | Pseudo::Not(_) | Pseudo::Has(_))
    })
}

/// Reports whether a selector can reach this node.
///
/// Text and line breaks have no tag or `data`, so this grammar cannot name them.
/// The walk skips these nodes. This keeps the probe away from the most numerous
/// tree nodes.
pub(super) fn selectable(kind: Kind) -> bool {
    !matches!(kind, Kind::Text | Kind::Break)
}

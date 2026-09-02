//! The bounded selector grammar, compiled and matched.
//!
//! Bounded by measurement, not by taste. Structured content has no `class`
//! attribute, so a dictionary author can reach content only through a tag
//! name or through the `data-*` attributes Yomitan derives from a node's
//! `data` map, and 897 of the corpus's 908 box rules select through exactly
//! those two (`docs/research/dict-shapes.md`). What is here is that surface
//! and nothing else:
//!
//! - a tag name, resolved to the schema's own [`Tag`]
//! - a `data-*` attribute, tested for presence or against one `=` value
//! - compound selectors, and the descendant and child combinators
//! - the structural pseudo-classes the corpus uses - `:first-child`,
//!   `:first-of-type`, `:last-of-type`, `:nth-child`, `:nth-of-type`,
//!   `:nth-last-of-type` - plus `:last-child` and `:nth-last-child`, which
//!   are the same two lines as the siblings beside them
//! - the selector-list forms `:is`, `:where`, `:not` and `:has`, whose
//!   arguments are compounds: `td:has([data-sc親字])` keys on content the
//!   way a bare attribute selector does
//!
//! Everything else - a class, an id, `*`, a sibling combinator, a
//! pseudo-element, `:hover`, `:link`, a non-`=` attribute operator, a tag
//! the schema has no node for - fails compilation, and the caller drops the
//! whole rule and counts it. Dropping whole is what CSS itself does with an
//! unreadable selector, and it is the only answer that never half-applies a
//! rule an author wrote for an arrangement this build does not have.

use crate::dict::gloss::{GlossDoc, Kind, NodeId, Span, Tag};

/// How a compound joins the compound to its left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Combinator {
    Descendant,
    Child,
}

/// One compound selector.
#[derive(Clone, Copy)]
pub(super) struct Compound {
    /// How it joins its left neighbour. Meaningless, and `Descendant`, on
    /// the leftmost compound of a selector.
    pub(super) combinator: Combinator,
    /// [`Tag::None`] when the compound names no tag.
    pub(super) tag: Tag,
    /// Into [`Pool::tests`].
    pub(super) tests: Span,
    /// Into [`Pool::pseudos`].
    pub(super) pseudos: Span,
}

/// One `data-*` attribute test.
pub(super) struct Test {
    /// Into [`Pool::attrs`].
    pub(super) attr: u32,
    /// `None` tests presence alone.
    pub(super) value: Option<Box<str>>,
}

/// A compound with no pseudo-classes: what a selector-list pseudo's argument
/// may be.
///
/// The restriction is measured. Every `:has`, `:is`, `:where` and `:not`
/// argument in the corpus is one compound - `td[data-sc親字]`,
/// `[data-sc外字][data-sc全丸]`, `> span[data-sc-zoomkanji]`, `a`, `ruby` -
/// so allowing a pseudo inside a pseudo would buy nothing and cost the
/// matcher its guarantee of terminating without a recursion budget.
#[derive(Clone, Copy)]
pub(super) struct Simple {
    /// How the argument relates to the anchor. Only `:has` reads it:
    /// `:has(x)` searches descendants, `:has(> x)` only children.
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
    /// `:is(...)` and `:where(...)`: some argument matches this node. They
    /// differ only in specificity, which is resolved at compile time.
    Is(Span),
    /// `:not(...)`: no argument matches this node.
    Not(Span),
    /// `:has(...)`: some argument matches a descendant, or a child when the
    /// argument is written `> x`.
    Has(Span),
}

/// An `An+B` argument.
#[derive(Clone, Copy)]
pub(super) struct Nth {
    pub(super) a: i32,
    pub(super) b: i32,
}

impl Nth {
    /// Does a 1-based sibling position satisfy this?
    fn holds(self, pos: u16) -> bool {
        let p = i32::from(pos);
        if self.a == 0 {
            return p == self.b;
        }
        let off = p - self.b;
        off % self.a == 0 && off / self.a >= 0
    }
}

/// Every selector part in one sheet, pooled.
///
/// Flat arenas for the same reason the gloss node arena is one: a compiled
/// selector is a `Span` into a shared vector, so a rule costs no allocation
/// of its own and the whole sheet is a handful of contiguous blocks.
#[derive(Default)]
pub(super) struct Pool {
    pub(super) compounds: Vec<Compound>,
    /// Attribute tests belonging to a [`Compound`].
    pub(super) tests: Vec<Test>,
    pub(super) pseudos: Vec<Pseudo>,
    pub(super) simples: Vec<Simple>,
    /// Attribute tests belonging to a [`Simple`] - a pseudo's argument.
    ///
    /// Its own arena, and it has to be: a compound's tests are a contiguous
    /// span, and parsing `td:has([data-sc親字])` would otherwise push the
    /// argument's test into the middle of the `td` compound's own run and
    /// make the cell require the attribute on itself.
    pub(super) simple_tests: Vec<Test>,
    /// Distinct `data-*` attribute names, spelled as the selectors spell
    /// them. A document resolves each to one interned `KeyId` once, and then
    /// every per-node probe compares `u32`s.
    pub(super) attrs: Vec<String>,
}

impl Pool {
    /// Where every arena currently ends, so a failed compile can rewind.
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

    /// One attribute name's id, interning it.
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
    /// Into [`Pool::compounds`], subject compound last.
    pub(super) sel: Span,
    /// CSS specificity, packed so that one comparison orders two rules.
    pub(super) spec: u32,
}

/// Specificity, accumulated.
///
/// No `a` column: an id selector fails compilation, so the count is always
/// zero and carrying it would be a field that can only ever hold one value.
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

/// Compiles one complex selector, or fails and rewinds the pool.
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
        // A combinator, or the end.
        let after_ws = skip_ws(b, i);
        if after_ws >= b.len() {
            break;
        }
        combinator = match b[after_ws] {
            b'>' => {
                i = after_ws + 1;
                Combinator::Child
            }
            // `+` and `~` reach a preceding sibling, which this arena's
            // first-child/next-sibling links cannot walk backwards. 26
            // corpus selectors use one; each drops its rule and is counted.
            b'+' | b'~' => return None,
            // Whitespace was the combinator, and it was significant.
            _ if after_ws > i => Combinator::Descendant,
            _ => return None,
        };
        i = skip_ws(b, i);
        if i >= b.len() {
            // A trailing combinator selects nothing.
            return None;
        }
    }
    let len = pool.compounds.len() as u32 - at;
    (len > 0).then_some(Compiled { sel: Span { at, len }, spec: spec.packed() })
}

/// One compound, from `*i` up to the next combinator or the end.
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
            // A class, an id, a universal, a namespace, and an unresolved
            // nesting marker: none of them can reach structured content, and
            // each drops its rule.
            b'.' | b'#' | b'*' | b'|' | b'&' => return None,
            b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'+' | b'~' | b',' => break,
            _ => {
                if tag != Tag::None || parts > 0 {
                    // A second tag name, or a tag after a test: neither is
                    // a compound this grammar reads.
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

/// One attribute selector, its brackets already stripped.
fn parse_test(inner: &str, pool: &mut Pool) -> Option<Test> {
    let (name, value) = match inner.split_once('=') {
        None => (inner.trim(), None),
        Some((head, tail)) => {
            // Only `=`. A `~=`, `^=`, `$=`, `*=` or `|=` matches a family of
            // values, and 15 corpus selectors reach a non-`data-*` attribute
            // this way; both drop their rule.
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

/// A selector's attribute value, quotes removed.
fn unquote(text: &str) -> Option<Box<str>> {
    let inner = match text.as_bytes().first()? {
        q @ (b'"' | b'\'') => text.strip_prefix(*q as char)?.strip_suffix(*q as char)?,
        _ => text,
    };
    Some(inner.into())
}

/// One pseudo-class, advancing past its argument list.
fn parse_pseudo(sel: &str, i: &mut usize, pool: &mut Pool) -> Option<(Pseudo, Spec)> {
    let b = sel.as_bytes();
    // A pseudo-element is not a node, so no rule that names one can be
    // resolved onto this tree: 18 corpus selectors, all dropped.
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
    // The declared list is the gate, not a copy of one: the array below is
    // what `tools/dict-census` reads to score the corpus, so a name that is
    // not in it must not compile however the match arms are written.
    //
    // It is also the whole of the legacy one-colon answer. `:before` and its
    // three siblings are pseudo-elements even with one colon, and a
    // pseudo-element is not a node; they are absent from the list, so they
    // are refused here and need no arm of their own below. Likewise the
    // `:is` aliases - `:matches`, `:-moz-any`, `:-webkit-any`, `:any` - are
    // not corpus shapes (`docs/research/dict-shapes.md` measures none of
    // them; Yomitan's own renderer is Chromium, where `:is` needs no
    // prefix), so naming them in the arm below would only have written a
    // branch the gate can never reach.
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
        // `:where` matches identically and contributes nothing to
        // specificity, which is its entire purpose.
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
        // `:hover` and `:link` are pointer and history state, which gloss
        // content in a popup never has: 20 corpus selectors, all dropped.
        _ => None,
    }
}

/// A selector-list pseudo's arguments.
///
/// `relative` allows each argument a leading `>`, which is what a `:has()`
/// argument may carry. The specificity returned is the most specific
/// argument's, as CSS says for `:is`, `:not` and `:has`.
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

/// One pseudo argument: a compound with a tag, tests, and nothing else.
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

/// `even`, `odd`, an integer, or `An+B`.
fn parse_nth(arg: &str) -> Option<Nth> {
    let text: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = text.to_ascii_lowercase();
    match lower.as_str() {
        "even" => return Some(Nth { a: 2, b: 0 }),
        "odd" => return Some(Nth { a: 2, b: 1 }),
        _ => {}
    }
    let Some((head, tail)) = lower.split_once('n') else {
        // The corpus's only form: a plain 1-based index.
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

/// A selector's tag name, as the schema's own enum.
///
/// A name the schema has no node for cannot match anything this tree holds,
/// so its rule is dropped: `body`, `h3`, `p` and `table-container` in the
/// corpus, 5 selectors between them.
fn tag_of(name: &str) -> Option<Tag> {
    crate::dict::gloss::tag_for(&name.to_ascii_lowercase())
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Just past the identifier at `i`. Equal to `i` when there is none.
///
/// A non-ASCII byte is an identifier byte: `[data-sc常用外マーク]` and
/// `p.字音` are both real corpus spellings, and a CJK tag name has to reach
/// [`tag_of`] to be rejected rather than mis-scanned as a combinator.
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

/// Just past the closer matching the opener at `i`, or `None` when the input
/// never closes it.
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

// ---- matching ----

/// A node's position among its siblings, 1-based as CSS counts.
///
/// Built once per document, and only for a sheet that asks: 8 of the 14
/// corpus stylesheets use no structural pseudo-class at all, and for those
/// this table is never allocated.
#[derive(Clone, Copy, Default)]
pub(super) struct Pos {
    pub(super) child: u16,
    pub(super) children: u16,
    /// Among siblings sharing this node's [`Tag`]. An unrecognised tag keeps
    /// its name as an attribute rather than in the enum, so every
    /// `Tag::Other` sibling counts as one type - which is exactly the set of
    /// nodes no corpus `of-type` selector names.
    pub(super) of_type: u16,
    pub(super) of_types: u16,
}

/// Everything a match needs beyond the tree itself.
pub(super) struct Ctx<'a> {
    pub(super) doc: &'a GlossDoc,
    /// One entry per interned `KeyId`: the id of the `data-*` attribute that
    /// key derives from, or [`NO_ATTR`].
    pub(super) attr_of_key: &'a [u32],
    /// Empty when the sheet uses no structural pseudo-class.
    pub(super) pos: &'a [Pos],
}

/// No selector in this sheet names the attribute a `data` key derives from.
pub(super) const NO_ATTR: u32 = u32::MAX;

/// Does the selector match the node at the end of `path`?
///
/// `path` is the chain from a glossary item down to the subject, so an
/// ancestor constraint is a walk left along it. Right-to-left with
/// backtracking, which is what a browser does and for the same reason: the
/// subject is known and every step left prunes.
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
            // A `data` value the schema let through as a number or a boolean
            // has no string form here, so an `=` test against it fails. No
            // corpus selector tests a non-string value.
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

/// Does any node under `id` match?
///
/// An explicit stack, not recursion: the tree is bounded at
/// [`MAX_DEPTH`](crate::dict::gloss::MAX_DEPTH) levels but a subtree is not
/// bounded in width, and `:has` is the one part of this grammar that looks
/// downward.
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

/// Sibling positions for every node, in one sweep of the tree.
pub(super) fn positions(doc: &GlossDoc) -> Vec<Pos> {
    let mut out = vec![Pos::default(); doc.all_nodes().len()];
    // Two scratch tallies, reused across every sibling list in the
    // document: a tag tally is at most a couple of entries long, and one
    // allocation per node would cost more than the whole sweep.
    let mut totals: Vec<(Tag, u16)> = Vec::new();
    let mut seen: Vec<(Tag, u16)> = Vec::new();
    // Top-level glossary items are each other's siblings exactly as the
    // children of a node are, so a `:first-child` rule on an item sees
    // itself as the first one.
    number(doc, || doc.items(), &mut out, &mut totals, &mut seen);
    for id in 0..doc.all_nodes().len() as NodeId {
        if doc.has_children(id) {
            number(doc, || doc.children(id), &mut out, &mut totals, &mut seen);
        }
    }
    out
}

/// Numbers one sibling list, in two passes over it.
///
/// Two passes rather than one, and an iterator factory rather than a
/// collected list, because `:last-of-type` needs a total this walk only has
/// once it has seen the whole list - and re-walking a linked sibling chain
/// is cheaper than materialising it.
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

/// Bumps one tag's tally and returns its new count.
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

/// Does any surviving selector in this sheet ask for a sibling position?
pub(super) fn needs_positions(pool: &Pool) -> bool {
    pool.pseudos.iter().any(|p| {
        !matches!(p, Pseudo::Is(_) | Pseudo::Not(_) | Pseudo::Has(_))
    })
}

/// Is this node one a selector can reach?
///
/// Text and line breaks carry no tag and no `data`, so no selector in this
/// grammar can name one, and skipping them keeps the probe off the most
/// numerous nodes in a tree.
pub(super) fn selectable(kind: Kind) -> bool {
    !matches!(kind, Kind::Text | Kind::Break)
}

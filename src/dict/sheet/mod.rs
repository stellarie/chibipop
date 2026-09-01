//! A dictionary's own `styles.css`, as a bounded matcher.
//!
//! Yomitan lets a dictionary ship a stylesheet and scopes it to that
//! dictionary's own entries. The census says ignoring it is not a small
//! omission: 14 of 72 dictionaries ship one, carrying **908 box-model
//! rules**, and over the 52 structured-content dictionaries box properties
//! live inline for 29 but **only in the stylesheet for 13**
//! ([dict-shapes.md](../../../docs/research/dict-shapes.md#dictionary-stylesheets-stylescss)).
//! So the box model of tickets 07 and 08, fed only by inline `style`, draws
//! nothing at all for 明鏡国語辞典, 大辞泉, 角川新字源, 字通, 旺文社漢字典
//! and eight more, and misses Jitendex's `[data-sc-class="tag"]` pill over
//! 48 776 nodes.
//!
//! This is a matcher, not a CSS engine, and the difference is measured
//! rather than asserted. Structured content has **no `class` attribute**, so
//! the only surface a dictionary author can reach is a tag name or a
//! `data-*` attribute derived from a node's `data` map, and 897 of those 908
//! box rules select through exactly those two. [`select`] holds the grammar;
//! everything outside it drops its rule whole and is counted, so the gap
//! stays a number.
//!
//! # Where this sits
//!
//! The stylesheet text is stored per dictionary at build time and compiled
//! **once, on first use** - not at build. That keeps it consistent with
//! ticket 02's per-hover tree parse: a matcher bug ships as a patch and not
//! as a rebuild. The whole corpus carries 174 KB of CSS across 14
//! dictionaries, so the compiled form is not worth caching on disk.
//!
//! [`apply`] then folds the winners into [`GlossDoc`]'s own resolved style
//! record, ahead of the inline `style` that keeps the last word. That is the
//! whole integration: the renderer reads one style record per node exactly
//! as it did before, and never learns that CSS exists. The
//! "honour dictionary styling" setting governs the merged record, so off
//! means neither inline `style` nor `styles.css` applies, with one gate
//! rather than two.

use crate::dict::gloss::{GlossDoc, KeyId, NodeId, Span, StyleKey, Tag};

mod scan;
mod select;

#[cfg(test)]
mod tests;

use select::{Ctx, Pool, NO_ATTR};

/// The selector kinds this grammar compiles, in `tools/dict-census`'s own
/// vocabulary.
///
/// Here, and not in the census, for the reason ticket 02's allow-lists are:
/// the census parses this array out of this source, so the count of rules it
/// reports as dropped re-scores itself as the grammar grows instead of
/// drifting from it. A test below is what keeps the array honest against the
/// parser beside it.
pub const SUPPORTED_SELECTOR_KINDS: [&str; 3] = ["tag", "data-attr", "pseudo-class"];

/// The pseudo-classes this grammar compiles.
///
/// The corpus's structural set, measured
/// (`docs/research/dict-shapes.md`): `:first-child` on 36 selectors,
/// `:nth-child` on 146, `:has` on 26, then `:first-of-type`,
/// `:nth-last-of-type`, `:last-of-type`, `:nth-of-type` and `:not`. Plus
/// `:last-child`, `:nth-last-child`, `:is` and `:where`, which are the same
/// lines as the siblings beside them and which the ticket names.
///
/// Not here: `:hover` and `:link`, which are pointer and history state that
/// gloss content in a popup never has - 20 corpus selectors, dropped.
pub const SUPPORTED_PSEUDO_CLASSES: [&str; 12] = [
    "first-child",
    "last-child",
    "first-of-type",
    "last-of-type",
    "nth-child",
    "nth-last-child",
    "nth-of-type",
    "nth-last-of-type",
    "is",
    "where",
    "not",
    "has",
];

/// A dictionary's compiled stylesheet.
///
/// Flat arenas throughout, for the reason ticket 02's node arena is flat: a
/// compiled rule is a pair of spans, so the largest corpus sheet is a
/// handful of contiguous blocks rather than 315 little allocations.
pub struct Sheet {
    pool: Pool,
    /// One entry per surviving *selector*, not per source rule: a selector
    /// list is expanded at compile time so each entry carries its own
    /// specificity and the cascade is one sort.
    rules: Vec<Rule>,
    /// Declaration pool. Several rules share a slice when they came from one
    /// selector list.
    decls: Vec<Decl>,
    /// Candidate rule ids, pooled; every index below is a slice of this.
    buckets: Vec<u32>,
    /// By the subject's `[attr="value"]` test, sorted for a binary search.
    keyed: Vec<Keyed>,
    /// By the subject's presence-only `[attr]` test, indexed by attribute id.
    by_attr: Vec<Span>,
    /// By the subject's tag, for a subject with no attribute test.
    by_tag: Vec<Span>,
    /// Subjects with neither - a bare `:first-child`.
    any: Span,
    /// Sorted attribute-name ids, so a document resolves its interned keys
    /// with a binary search instead of a scan of up to 322 names.
    attr_order: Vec<u32>,
    /// Does any surviving selector ask for a sibling position? 8 of the 14
    /// corpus sheets do not, and for those the position table is never
    /// built.
    needs_positions: bool,
    counts: SheetCounts,
}

/// One compiled selector and the declarations it carries.
struct Rule {
    /// Into `Pool::compounds`, subject compound last.
    sel: Span,
    /// Into [`Sheet::decls`].
    decls: Span,
    /// Packed CSS specificity.
    spec: u32,
    /// Source order, the tie-break between equal specificities.
    order: u32,
}

/// One declaration this build can act on.
struct Decl {
    key: StyleKey,
    /// The value text, verbatim. Read by the layout pass's own value
    /// parsers, which are the single authority on what a value means - this
    /// module never decides that `4px` is a length.
    value: Box<str>,
    important: bool,
}

/// A rule's candidate bucket, keyed by one exact attribute value.
///
/// Jitendex points 40 rules at `data-sc-content`, so bucketing on the
/// attribute name alone would hand every node with a `data.content` key 40
/// candidates to test. Keying on the value too takes that to one or two.
struct Keyed {
    attr: u32,
    value: Box<str>,
    rules: Span,
}

/// What compiling one dictionary's `styles.css` kept and dropped.
///
/// The dropped counts are the point: the census scores the corpus against
/// this build's grammar, and `dropped()` is the progress gauge the spec asks
/// it to report. `kept + dropped_selector + dropped_at_rule + no_decls`
/// always equals `rules`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SheetCounts {
    /// Style rules the scanner recovered, at-rule bodies included.
    pub rules: usize,
    /// Rules compiled into the table.
    pub kept: usize,
    /// Rules dropped whole because a selector left the supported grammar.
    pub dropped_selector: usize,
    /// Rules dropped whole because they sit inside an at-rule body. The
    /// corpus loses 4 box rules here, all behind `@media (max-width: 500px)`.
    pub dropped_at_rule: usize,
    /// Rules whose selector compiled but which declared nothing this build
    /// maps - a rule of `display` and `grid-column` alone.
    pub no_decls: usize,
    /// Selectors compiled, after expanding selector lists and `&` nesting.
    pub selectors: usize,
    /// Declarations compiled, after expanding `margin` and `padding`.
    ///
    /// From kept rules only, so this and `dropped_declarations` do not
    /// partition anything: a rule whose selector left the grammar loses its
    /// declarations uncounted, and one that declared nothing mappable is a
    /// `no_decls` rule whose losses land below.
    pub declarations: usize,
    /// Declarations dropped: a property this build does not map, or a
    /// `var()` value it cannot substitute.
    ///
    /// The property gap, where `dropped()` is the grammar gap.
    /// `dict::build::StyleCounts` carries both into the build report,
    /// because `tools/dict-census` counts a stylesheet's declarations too
    /// and two arithmetics nobody compares are two arithmetics that drift.
    pub dropped_declarations: usize,
    /// The scanner's first complaint, when the text was malformed.
    pub error: Option<&'static str>,
}

impl SheetCounts {
    /// Rules dropped whole, which is what the census reports.
    pub fn dropped(&self) -> usize {
        self.dropped_selector + self.dropped_at_rule
    }
}

impl Sheet {
    /// Compiles one dictionary's stylesheet text.
    ///
    /// Total. Every way a stylesheet can be malformed - an unterminated
    /// comment or string, a stray `}`, a brace inside `url(...)`, nested
    /// at-rules, an unreadable selector - drops the rule it is in and
    /// records a count. There is no error return, because a dictionary with
    /// a broken stylesheet must still render its entries.
    pub fn compile(css: &str) -> Sheet {
        let scanned = scan::scan(css);
        let mut sheet = Sheet {
            pool: Pool::default(),
            rules: Vec::new(),
            decls: Vec::new(),
            buckets: Vec::new(),
            keyed: Vec::new(),
            by_attr: Vec::new(),
            by_tag: Vec::new(),
            any: Span::default(),
            attr_order: Vec::new(),
            needs_positions: false,
            counts: SheetCounts { error: scanned.error, ..SheetCounts::default() },
        };
        for rule in &scanned.rules {
            sheet.counts.rules += 1;
            if rule.in_at_rule {
                sheet.counts.dropped_at_rule += 1;
                continue;
            }
            sheet.add(rule);
        }
        sheet.index();
        sheet
    }

    /// One scanned rule, compiled or counted.
    fn add(&mut self, rule: &scan::Rule) {
        // Selectors first: a rule with an unreadable selector is dropped
        // whole, declarations included, which is what CSS does with a
        // selector list it cannot read and the only answer that never
        // half-applies a rule written for an arrangement we do not have.
        let first = self.pool.compounds.len();
        let mut compiled = Vec::with_capacity(rule.selectors.len());
        for sel in &rule.selectors {
            match select::compile(sel, &mut self.pool) {
                Some(c) => compiled.push(c),
                None => {
                    self.pool.compounds.truncate(first);
                    self.counts.dropped_selector += 1;
                    return;
                }
            }
        }
        let at = self.decls.len() as u32;
        for decl in &rule.decls {
            self.push_decl(decl);
        }
        let decls = Span { at, len: self.decls.len() as u32 - at };
        if decls.len == 0 {
            self.pool.compounds.truncate(first);
            self.counts.no_decls += 1;
            return;
        }
        self.counts.kept += 1;
        self.counts.selectors += compiled.len();
        self.counts.declarations += decls.len as usize;
        for c in compiled {
            let order = self.rules.len() as u32;
            self.rules.push(Rule { sel: c.sel, decls, spec: c.spec, order });
        }
    }

    /// One declaration, mapped onto a [`StyleKey`] or counted as dropped.
    fn push_decl(&mut self, decl: &scan::Decl) {
        // `var()` is the one value form dropped rather than passed on: the
        // custom property it names is declared on Yomitan's popup chrome,
        // which this renderer has no equivalent of, so substituting it would
        // mean inventing a number.
        if decl.value.contains("var(") {
            self.counts.dropped_declarations += 1;
            return;
        }
        let Some(key) = css_key(&decl.prop) else {
            self.counts.dropped_declarations += 1;
            return;
        };
        // A `margin` or `padding` shorthand expands here, into the four
        // longhands it sets. Not cosmetic: it is what lets the cascade run
        // per property instead of per list position, so a shorthand in a
        // less specific rule can no longer erase a longhand from a more
        // specific one, and so "does the inline `style` already set this?"
        // is an exact question.
        match edges_of(key) {
            None => self.decls.push(Decl {
                key,
                value: decl.value.as_str().into(),
                important: decl.important,
            }),
            Some(edges) => {
                let parts: Vec<&str> = decl.value.split_whitespace().collect();
                let pick = |i: usize| match parts.len() {
                    1 => Some(parts[0]),
                    2 => Some(parts[i % 2]),
                    3 => Some(parts[[0, 1, 2, 1][i]]),
                    4 => Some(parts[i]),
                    _ => None,
                };
                for (i, key) in edges.into_iter().enumerate() {
                    match pick(i) {
                        Some(part) => self.decls.push(Decl {
                            key,
                            value: part.into(),
                            important: decl.important,
                        }),
                        None => self.counts.dropped_declarations += 1,
                    }
                }
            }
        }
    }

    /// Buckets every rule by its subject, so a node's candidate set is
    /// small.
    ///
    /// The subject is the rightmost compound - the node the rule actually
    /// styles - and it is bucketed by the most selective thing it names: an
    /// exact attribute value, then a bare attribute, then a tag. A rule
    /// lands in exactly one bucket, so a candidate list needs no
    /// deduplication.
    fn index(&mut self) {
        self.needs_positions = select::needs_positions(&self.pool);
        self.attr_order = (0..self.pool.attrs.len() as u32).collect();
        self.attr_order.sort_by(|a, b| {
            self.pool.attrs[*a as usize].cmp(&self.pool.attrs[*b as usize])
        });

        let mut keyed: Vec<(u32, Box<str>, Vec<u32>)> = Vec::new();
        let mut by_attr: Vec<Vec<u32>> = vec![Vec::new(); self.pool.attrs.len()];
        let mut by_tag: Vec<Vec<u32>> = Vec::new();
        let mut any: Vec<u32> = Vec::new();
        for (ix, rule) in self.rules.iter().enumerate() {
            let ix = ix as u32;
            let subject = &self.pool.compounds[(rule.sel.at + rule.sel.len - 1) as usize];
            let tests = self.pool.tests_of(subject.tests);
            match tests.iter().find(|t| t.value.is_some()).or_else(|| tests.first()) {
                Some(test) => match &test.value {
                    Some(value) => {
                        match keyed
                            .iter_mut()
                            .find(|(a, v, _)| *a == test.attr && *v == *value)
                        {
                            Some((_, _, rules)) => rules.push(ix),
                            None => keyed.push((test.attr, value.clone(), vec![ix])),
                        }
                    }
                    None => by_attr[test.attr as usize].push(ix),
                },
                None if subject.tag != Tag::None => {
                    let slot = subject.tag as usize;
                    if by_tag.len() <= slot {
                        by_tag.resize(slot + 1, Vec::new());
                    }
                    by_tag[slot].push(ix);
                }
                None => any.push(ix),
            }
        }
        // A binary search wants sorted keys, and the key is the pair.
        keyed.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        self.keyed = keyed
            .into_iter()
            .map(|(attr, value, rules)| Keyed { attr, value, rules: self.pour(&rules) })
            .collect();
        self.by_attr = by_attr.iter().map(|r| self.pour(r)).collect();
        self.by_tag = by_tag.iter().map(|r| self.pour(r)).collect();
        self.any = self.pour(&any);
    }

    /// One candidate list into the shared pool.
    fn pour(&mut self, rules: &[u32]) -> Span {
        let at = self.buckets.len() as u32;
        self.buckets.extend_from_slice(rules);
        Span { at, len: rules.len() as u32 }
    }

    fn bucket(&self, span: Span) -> &[u32] {
        &self.buckets[span.at as usize..(span.at + span.len) as usize]
    }

    /// Rules whose subject requires exactly this attribute value.
    fn keyed_bucket(&self, attr: u32, value: &str) -> &[u32] {
        match self.keyed.binary_search_by(|k| (k.attr, &*k.value).cmp(&(attr, value))) {
            Ok(at) => self.bucket(self.keyed[at].rules),
            Err(_) => &[],
        }
    }

    /// Are there no rules at all?
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn counts(&self) -> &SheetCounts {
        &self.counts
    }
}

impl Pool {
    fn tests_of(&self, span: Span) -> &[select::Test] {
        &self.tests[span.at as usize..(span.at + span.len) as usize]
    }
}

/// The CSS spelling of every property this build maps.
///
/// The kebab-case half of `gloss::parse::style_key_for`'s camelCase table,
/// and deliberately no wider: the ticket's rule is to keep the declarations
/// the renderer already understands and drop the rest. So the corpus's
/// `display`, `grid-column`, `line-height`, `font-family` and its 15 logical
/// properties (`margin-inline-start` and friends, 700-odd declarations) are
/// dropped and counted rather than half-mapped onto a box model that has no
/// per-edge colour and no writing mode.
fn css_key(prop: &str) -> Option<StyleKey> {
    Some(match prop {
        "font-style" => StyleKey::FontStyle,
        "font-weight" => StyleKey::FontWeight,
        "font-size" => StyleKey::FontSize,
        "text-decoration-line" => StyleKey::TextDecorationLine,
        "text-decoration-style" => StyleKey::TextDecorationStyle,
        "text-decoration-color" => StyleKey::TextDecorationColor,
        "vertical-align" => StyleKey::VerticalAlign,
        "text-align" => StyleKey::TextAlign,
        "white-space" => StyleKey::WhiteSpace,
        "word-break" => StyleKey::WordBreak,
        "cursor" => StyleKey::Cursor,
        "list-style-type" => StyleKey::ListStyleType,
        "color" => StyleKey::Color,
        "background-color" => StyleKey::BackgroundColor,
        "border-color" => StyleKey::BorderColor,
        "border-style" => StyleKey::BorderStyle,
        "border-radius" => StyleKey::BorderRadius,
        "border-width" => StyleKey::BorderWidth,
        "margin" => StyleKey::Margin,
        "margin-top" => StyleKey::MarginTop,
        "margin-right" => StyleKey::MarginRight,
        "margin-bottom" => StyleKey::MarginBottom,
        "margin-left" => StyleKey::MarginLeft,
        "padding" => StyleKey::Padding,
        "padding-top" => StyleKey::PaddingTop,
        "padding-right" => StyleKey::PaddingRight,
        "padding-bottom" => StyleKey::PaddingBottom,
        "padding-left" => StyleKey::PaddingLeft,
        _ => return None,
    })
}

/// The four longhands a shorthand sets, top-right-bottom-left.
///
/// `border-width` and `border-style` are shorthands too, but this build has
/// no per-edge key for either, so there is nothing to expand them into and
/// nothing that can conflict with them.
fn edges_of(key: StyleKey) -> Option<[StyleKey; 4]> {
    match key {
        StyleKey::Margin => Some([
            StyleKey::MarginTop,
            StyleKey::MarginRight,
            StyleKey::MarginBottom,
            StyleKey::MarginLeft,
        ]),
        StyleKey::Padding => Some([
            StyleKey::PaddingTop,
            StyleKey::PaddingRight,
            StyleKey::PaddingBottom,
            StyleKey::PaddingLeft,
        ]),
        _ => None,
    }
}

/// Does an inline `style` shorthand already set this longhand?
///
/// Inline `style` keeps its shorthands - this module does not rewrite the
/// tree parser's output - so the fold has to ask the question in both
/// directions before it lets a stylesheet declaration through.
fn shadowed_by(inline: StyleKey, css: StyleKey) -> bool {
    inline == css || edges_of(inline).is_some_and(|edges| edges.contains(&css))
}

/// The attribute Yomitan derives from a structured-content `data` key.
///
/// Every selector in a dictionary's stylesheet depends on this exact
/// transform, so it is ported from `tools/dict-census/census.py`, which
/// verified it against the corpus, rather than re-derived. Yomitan writes
/// the key into `dataset` under an `sc` prefix, and the DOM turns every
/// ASCII capital there into `-x`: `content` becomes `data-sc-content`,
/// `partOfSpeech` becomes `data-sc-part-of-speech`, `BM` becomes
/// `data-sc-b-m`, and a CJK key takes **no** separating hyphen at all -
/// `常用外マーク` becomes `data-sc常用外マーク`.
///
/// Forward, never backward. The transform is not injective - `content` and
/// `Content` both give `data-sc-content` - so a compiled selector's
/// attribute name is resolved against a document's interned keys by
/// transforming the keys, once per document, not by un-transforming the
/// name.
fn dataset_attr(key: &str, out: &mut String) {
    out.push_str("data-sc");
    let mut chars = key.chars();
    let Some(first) = chars.next() else { return };
    // The `sc` prefix makes the key's own first letter an interior one, so
    // `dataset` capitalises it; a non-ASCII-alphabetic first character is
    // left exactly as it is.
    let head = if first.is_ascii_alphabetic() { first.to_ascii_uppercase() } else { first };
    for c in std::iter::once(head).chain(chars) {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
}

/// Folds a dictionary's stylesheet into one parsed tree.
///
/// The whole of the integration. Each node's (tag, `data` map, ancestor
/// chain, sibling position) selects its winners, the winners are ordered by
/// importance then specificity then source order, and their declarations
/// land in the node's resolved style record **ahead of** the inline `style`
/// already there.
///
/// Ahead of, and it costs the inline half nothing: a stylesheet declaration
/// the inline record already sets - or that an inline shorthand covers - is
/// dropped before it is written, so the two halves never name one property
/// and inline `style` has the last word by construction. That is what makes
/// the record's own order stop mattering: a consumer reading the first
/// occurrence of a property and one reading the last get the same answer,
/// which is the invariant both `GlossDoc::style_of` and the layout pass rely
/// on.
///
/// Application order still matters *within* the stylesheet half, and it is
/// why a `margin` or `padding` shorthand is expanded into its four longhands
/// at compile time: after that, no two declarations here overlap and the
/// cascade is per property rather than per list position.
pub fn apply(doc: &mut GlossDoc, sheet: &Sheet) {
    if sheet.is_empty() || doc.is_empty() {
        return;
    }
    let wins = resolve(doc, sheet);
    if !wins.is_empty() {
        doc.fold_declarations(&wins);
    }
}

/// Every stylesheet declaration that wins on some node, in node order.
///
/// Borrowed values, so a win costs no allocation: the text lives in the
/// compiled sheet until [`GlossDoc::fold_declarations`] copies it into the
/// document's one text buffer.
fn resolve<'a>(doc: &GlossDoc, sheet: &'a Sheet) -> Vec<(NodeId, StyleKey, &'a str)> {
    let attr_of_key = resolve_keys(doc, sheet);
    // A document whose keys name none of the sheet's attributes can still be
    // matched by a tag or by a bare `:first-child` rule, so this is not an
    // early exit - but it is why the attribute buckets go quiet.
    let pos = if sheet.needs_positions { select::positions(doc) } else { Vec::new() };
    let ctx = Ctx { doc, attr_of_key: &attr_of_key, pos: &pos };
    let mut w = Walk {
        sheet,
        ctx,
        path: Vec::with_capacity(crate::dict::gloss::MAX_DEPTH as usize),
        cands: Vec::new(),
        hits: Vec::new(),
        out: Vec::new(),
    };
    for item in doc.items() {
        w.descend(item, 0);
    }
    w.out
}

/// One document's interned keys, mapped to the sheet's attribute ids.
///
/// Once per document, which is the whole point of ticket 02's interning: a
/// key's attribute name is computed here and every per-node probe after this
/// compares `u32`s. A binary search over the sheet's names rather than a
/// scan, because the largest corpus sheet names 322 of them.
fn resolve_keys(doc: &GlossDoc, sheet: &Sheet) -> Vec<u32> {
    let mut out = vec![NO_ATTR; doc.key_count()];
    let mut name = String::with_capacity(32);
    for (k, slot) in out.iter_mut().enumerate() {
        name.clear();
        dataset_attr(doc.key(k as KeyId), &mut name);
        let found = sheet
            .attr_order
            .binary_search_by(|a| sheet.pool.attrs[*a as usize].as_str().cmp(&name));
        if let Ok(at) = found {
            *slot = sheet.attr_order[at];
        }
    }
    out
}

/// The descent, and the scratch buffers it reuses.
///
/// Two lifetimes, because the walk borrows two unrelated things: `'s` is the
/// compiled sheet, which outlives the fold and lends every value string, and
/// `'d` is the document plus the two tables resolved against it, which live
/// only as long as this walk.
struct Walk<'s, 'd> {
    sheet: &'s Sheet,
    ctx: Ctx<'d>,
    /// The chain from a glossary item down to the node being probed.
    path: Vec<NodeId>,
    /// Candidate rule ids for one node.
    cands: Vec<u32>,
    /// `(importance, specificity, order, rule)` for one node's matches.
    hits: Vec<(bool, u32, u32, u32)>,
    out: Vec<(NodeId, StyleKey, &'s str)>,
}

impl<'s> Walk<'s, '_> {
    /// One node and its subtree.
    ///
    /// `depth` is bounded by the parser's own
    /// [`MAX_DEPTH`](crate::dict::gloss::MAX_DEPTH), so this recursion is as
    /// deep as the tree it walks and no deeper - and the guard is repeated
    /// here rather than assumed, because a stack overflow is not a rendering
    /// failure this process recovers from.
    fn descend(&mut self, id: NodeId, depth: u32) {
        if depth > crate::dict::gloss::MAX_DEPTH {
            return;
        }
        // The document reference is copied out of `ctx` first, so the sibling
        // iterator borrows the tree rather than `self` and the recursion
        // needs no per-node vector of children.
        let doc = self.ctx.doc;
        self.path.push(id);
        if select::selectable(doc.node(id).kind) {
            self.probe(id);
        }
        for kid in doc.children(id) {
            self.descend(kid, depth + 1);
        }
        self.path.pop();
    }

    /// One node's winners, folded into `out`.
    fn probe(&mut self, id: NodeId) {
        let sheet = self.sheet;
        let doc = self.ctx.doc;
        self.cands.clear();
        for (key, value) in doc.data(id) {
            let attr = self.ctx.attr_of_key.get(*key as usize).copied().unwrap_or(NO_ATTR);
            if attr == NO_ATTR {
                continue;
            }
            if let Some(text) = doc.scalar_str(*value) {
                self.cands.extend_from_slice(sheet.keyed_bucket(attr, text));
            }
            if let Some(span) = sheet.by_attr.get(attr as usize) {
                self.cands.extend_from_slice(sheet.bucket(*span));
            }
        }
        if let Some(span) = sheet.by_tag.get(doc.node(id).tag as usize) {
            self.cands.extend_from_slice(sheet.bucket(*span));
        }
        self.cands.extend_from_slice(sheet.bucket(sheet.any));
        if self.cands.is_empty() {
            return;
        }

        self.hits.clear();
        for ix in &self.cands {
            let rule = &sheet.rules[*ix as usize];
            if select::matches(&sheet.pool, rule.sel, &self.ctx, &self.path) {
                let important =
                    sheet.decls_of(rule.decls).iter().any(|d| d.important);
                self.hits.push((important, rule.spec, rule.order, *ix));
            }
        }
        if self.hits.is_empty() {
            return;
        }
        // Descending cascade order: importance, then specificity, then the
        // later of two equally specific rules. The record is read
        // first-occurrence-wins, so the winner has to come first.
        self.hits.sort_unstable_by(|a, b| b.cmp(a));

        let inline = doc.style(id);
        let at = self.out.len();
        // Importance is a property of a declaration and not of the rule that
        // carries it, so an `!important` length beats every normal one
        // whatever rule it came from - which takes a second pass over the
        // same ordering. Only when there is one: 6 of the 14 corpus sheets
        // carry no `!important` at all, and those pay for one pass. It never
        // beats inline `style`, which this ticket's contract gives the last
        // word unconditionally.
        let passes: &[bool] =
            if self.hits.iter().any(|h| h.0) { &[true, false] } else { &[false] };
        for &important in passes {
            for (_, _, _, ix) in &self.hits {
                let rule = &sheet.rules[*ix as usize];
                for decl in sheet.decls_of(rule.decls) {
                    if decl.important != important {
                        continue;
                    }
                    if inline.iter().any(|(k, _)| shadowed_by(*k, decl.key)) {
                        continue;
                    }
                    if self.out[at..].iter().any(|(_, k, _)| *k == decl.key) {
                        continue;
                    }
                    self.out.push((id, decl.key, &decl.value));
                }
            }
        }
    }
}

impl Sheet {
    fn decls_of(&self, span: Span) -> &[Decl] {
        &self.decls[span.at as usize..(span.at + span.len) as usize]
    }
}

//! A `styles.css` file from one Dictionary, compiled as a bounded matcher.
//!
//! Yomitan allows a Dictionary to carry a stylesheet.
//! Yomitan applies that stylesheet only to entries from the same Dictionary.
//! The census finds a stylesheet in 14 of 72 dictionaries.
//! Those stylesheets contain **908 box-model rules**.
//! The 52 structured-content dictionaries keep box properties inline in 29 dictionaries.
//! The stylesheet contains those properties only for 13 dictionaries
//! ([dict-shapes.md](../../../docs/research/dict-shapes.md#dictionary-stylesheets-stylescss)).
//! A box model that reads only the inline `style` attribute draws nothing for
//! 明鏡国語辞典, 大辞泉, 角川新字源, 字通, 旺文社漢字典 and eight more
//! dictionaries. It also misses the Jitendex `[data-sc-class="tag"]` pill on
//! 48 776 nodes.
//!
//! This module matches selectors. It is not a CSS engine.
//! It measures the difference and does not assert it.
//! Structured content has **no `class` attribute**.
//! A dictionary author can therefore name only a tag or a `data-*` attribute
//! from a node's `data` map.
//! Exactly those two surfaces select 897 of the 908 box rules.
//! The [`select`] module holds the grammar.
//! A selector outside that grammar drops its whole rule.
//! The build counts each drop. The gap remains a number.
//!
//! The build step stores stylesheet text for each dictionary.
//! This module compiles that text **once, on first use**, not at build time.
//! This choice matches the per-hover tree parse of the gloss arena.
//! A matcher bug then needs a patch, not a rebuild.
//! The whole corpus contains 174 KB of CSS across 14 dictionaries.
//! A disk cache for the compiled form uses more space than it saves.
//!
//! [`apply`] merges declarations that win into the resolved style record of [`GlossDoc`],
//! **ahead of** the inline `style` attribute, which then wins.
//! This completes the integration.
//! The renderer reads one style record for each node and does not know about CSS.
//! The `"honour dictionary styling"` setting controls the merged record.
//! When the setting is off, neither the inline `style` attribute nor `styles.css` applies.
//! One gate controls both.

use crate::dict::gloss::{GlossDoc, KeyId, NodeId, Span, StyleKey, Tag};

mod scan;
mod select;

#[cfg(test)]
mod tests;

use select::{Ctx, Pool, NO_ATTR};

/// Selector kinds that this grammar compiles, in the vocabulary of
/// `tools/dict-census`.
///
/// This array stays in this module, not in the census, like the gloss
/// allow-lists. The census parses this array from this source.
/// Therefore the census recalculates dropped-rule counts as the grammar grows.
/// A test below keeps the array aligned with the parser.
pub const SUPPORTED_SELECTOR_KINDS: [&str; 3] = ["tag", "data-attr", "pseudo-class"];

/// Pseudo-classes that this grammar compiles.
///
/// This list records the corpus structure measured in
/// (`docs/research/dict-shapes.md`). `:first-child` appears on 36 selectors.
/// `:nth-child` appears on 146 selectors, and `:has` appears on 26.
/// The corpus also uses `:first-of-type`, `:nth-last-of-type`, `:last-of-type`,
/// `:nth-of-type`, and `:not`.
/// This list also includes `:last-child`, `:nth-last-child`, `:is`, and `:where`.
/// These forms cost the same matcher lines as the related forms.
///
/// The list rejects `:hover` and `:link`.
/// These forms report pointer or history state, which gloss content in a popup
/// lacks. This rejection covers 20 corpus selectors.
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

/// The compiled stylesheet for one dictionary.
///
/// This type uses flat arenas for the same reason as the gloss node arena.
/// A compiled rule is a pair of spans. Therefore the largest corpus sheet uses
/// a few contiguous blocks instead of 315 small allocations.
pub struct Sheet {
    pool: Pool,
    /// Stores one entry for each kept *selector*, not for each source rule.
    /// The compile step expands a selector list. Each entry then carries its own
    /// specificity. The cascade needs one sort.
    rules: Vec<Rule>,
    /// This declaration pool stores slices that several rules can share when one
    /// selector list produces them.
    decls: Vec<Decl>,
    /// This pool stores candidate rule IDs. Every index below refers to a slice
    /// of this vector.
    buckets: Vec<u32>,
    /// This table uses the subject test `[attr="value"]`. Its entries are sorted
    /// for binary search.
    keyed: Vec<Keyed>,
    /// This table uses the attribute ID for the subject test `[attr]`, which
    /// checks attribute presence.
    by_attr: Vec<Span>,
    /// This table uses the subject tag when the subject has no attribute test.
    by_tag: Vec<Span>,
    /// This span holds subjects with neither a tag nor an attribute test, such as
    /// a bare `:first-child`.
    any: Span,
    /// This vector stores sorted attribute-name IDs. A document resolves its
    /// interned keys with binary search, not a scan of up to 322 names.
    attr_order: Vec<u32>,
    /// This field is true when a kept selector asks for a sibling position.
    /// 8 of the 14 corpus sheets ask for no position. This module does not build
    /// the position table for those sheets.
    needs_positions: bool,
    counts: SheetCounts,
}

/// One compiled selector and the declarations it carries.
struct Rule {
    /// Points into `Pool::compounds`. The subject compound comes last.
    sel: Span,
    /// Points into [`Sheet::decls`].
    decls: Span,
    /// Packed CSS specificity.
    spec: u32,
    /// Source order. This value breaks a tie between equal specificities.
    order: u32,
}

/// A declaration that this build maps to a [`StyleKey`] or counts as dropped.
struct Decl {
    key: StyleKey,
    /// The value text stays verbatim. The layout pass parses this text.
    /// Those parsers alone define each value's meaning. This module does not
    /// decide whether `4px` is a length.
    value: Box<str>,
    important: bool,
}

/// A candidate bucket for rules with one exact attribute value.
///
/// Jitendex points 40 rules at `data-sc-content`. A bucket keyed only by
/// attribute name would give every node with a `data.content` key 40
/// candidates to test. A key that also stores the value reduces the
/// candidate count to one or two.
struct Keyed {
    attr: u32,
    value: Box<str>,
    rules: Span,
}

/// Counts what the compile step of one `styles.css` file keeps and drops.
///
/// The census uses the dropped counts to score this grammar against the corpus.
/// The census reports `dropped()` as its progress measure.
/// `kept + dropped_selector + dropped_at_rule + no_decls` always equals `rules`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SheetCounts {
    /// The number of style rules that the scanner recovered, with at-rule bodies
    /// included.
    pub rules: usize,
    /// The number of rules compiled into the table.
    pub kept: usize,
    /// Rules that the build drops because a selector leaves the supported
    /// grammar.
    pub dropped_selector: usize,
    /// Rules inside an at-rule body. The build drops these rules.
    /// The corpus loses 4 box rules here. All 4 occur under
    /// `@media (max-width: 500px)`.
    pub dropped_at_rule: usize,
    /// Rules whose selectors compile but whose declarations contain no property
    /// that this build maps. A rule with only `display` and `grid-column` is one
    /// such rule.
    pub no_decls: usize,
    /// Compiled selectors after the compile step expands selector lists and `&`
    /// nesting.
    pub selectors: usize,
    /// Compiled declarations after the compile step expands `margin` and
    /// `padding`.
    ///
    /// Only kept rules contribute to this count. Therefore this count and
    /// `dropped_declarations` do not partition the declarations.
    /// A rule with an unsupported selector loses its declarations without a
    /// count. A rule with no mappable declarations is a `no_decls` rule. Its
    /// dropped declarations appear in `dropped_declarations`.
    pub declarations: usize,
    /// Declarations that the build drops because it has no property map or
    /// cannot substitute a `var()` value.
    ///
    /// The count shows the property gap. `dropped()` shows the grammar gap.
    /// `dict::build::StyleCounts` carries both counts into the build report
    /// because `tools/dict-census` also counts stylesheet declarations.
    /// Two sums that no code compares can drift.
    pub dropped_declarations: usize,
    /// This field stores the first scanner complaint when the text is malformed.
    pub error: Option<&'static str>,
}

impl SheetCounts {
    /// Returns the total number of rules that the build drops.
    pub fn dropped(&self) -> usize {
        self.dropped_selector + self.dropped_at_rule
    }
}

impl Sheet {
    /// Compiles the stylesheet text for one dictionary.
    ///
    /// This function accepts malformed stylesheets.
    /// Possible faults include an unterminated comment or string, a stray `}`,
    /// a brace inside `url(...)`, nested at-rules, or an unreadable selector.
    /// Each fault drops the rule that contains it and records a count.
    /// The function returns no error because a broken stylesheet must not make
    /// dictionary entries fail to render.
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

    /// Compiles one scanned rule and updates its drop counts.
    fn add(&mut self, rule: &scan::Rule) {
        // whole rule and its declarations. CSS uses this result for an
        // unreadable selector list. This result prevents partial application
        // of a rule for a tree arrangement that this renderer cannot represent.
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

    /// Maps one declaration to a [`StyleKey`] or counts it as dropped.
    fn push_decl(&mut self, decl: &scan::Decl) {
        // The custom property that `var()` names belongs to Yomitan popup
        // chrome. This renderer has no equivalent chrome. A substitution would
        // invent a number.
        if decl.value.contains("var(") {
            self.counts.dropped_declarations += 1;
            return;
        }
        let Some(key) = css_key(&decl.prop) else {
            self.counts.dropped_declarations += 1;
            return;
        };
        // A `margin` or `padding` shorthand expands here into the four
        // longhands that it sets. The expansion is not cosmetic.
        // It lets the cascade run for each property, not for each list
        // position. A shorthand in a less specific rule cannot erase a
        // longhand from a more specific rule. The question "does the inline
        // `style` attribute already set this property?" also becomes exact.
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

    /// This function groups each rule by its subject. The group keeps the
    /// candidate set for a node small.
    ///
    /// The subject is the rightmost compound. It is the node that the rule
    /// styles. The bucket uses the most selective item that the subject names:
    /// an exact attribute value first, a bare attribute second, and a tag third.
    /// Each rule enters exactly one bucket. Therefore a candidate list needs no
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
        // Binary search needs sorted keys. The key is the attribute and value
        // pair.
        keyed.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        self.keyed = keyed
            .into_iter()
            .map(|(attr, value, rules)| Keyed { attr, value, rules: self.pour(&rules) })
            .collect();
        self.by_attr = by_attr.iter().map(|r| self.pour(r)).collect();
        self.by_tag = by_tag.iter().map(|r| self.pour(r)).collect();
        self.any = self.pour(&any);
    }

    /// Copies a candidate list into the shared pool.
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

    /// Reports whether the sheet holds no rules.
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

/// The CSS spelling of each property that this build maps.
///
/// This table provides the kebab-case half of the camelCase table in
/// `gloss::parse::style_key_for`. It stays narrow by design.
/// The build keeps declarations that the renderer understands and drops the rest.
/// Therefore the build drops and counts `display`, `grid-column`, `line-height`,
/// and `font-family`. It also drops 15 logical properties, such as
/// `margin-inline-start`. That group contains more than 700 declarations.
/// The table does not map per-edge color or writing mode. The renderer drops
/// those properties instead of assigning incomplete semantics.
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

/// The four longhands that a shorthand sets, in top-right-bottom-left order.
///
/// `border-width` and `border-style` are also shorthands. This build has no
/// per-edge key for either property, so each expands to nothing.
/// No declaration can conflict with either property.
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

/// Reports whether an inline `style` shorthand already sets this longhand.
///
/// The inline `style` attribute keeps its shorthands because this module never
/// rewrites parser output. Therefore the fold checks both directions before it
/// accepts a stylesheet declaration.
fn shadowed_by(inline: StyleKey, css: StyleKey) -> bool {
    inline == css || edges_of(inline).is_some_and(|edges| edges.contains(&css))
}

/// The attribute that Yomitan derives from a structured-content `data` key.
///
/// Every selector in a dictionary stylesheet depends on this exact transform.
/// This code ports `tools/dict-census/census.py`, which checked the transform
/// against the corpus. It does not derive a new transform.
/// Yomitan writes the key into `dataset` with an `sc` prefix.
/// The DOM then converts each ASCII capital to `-x`.
/// `content` becomes `data-sc-content`.
/// `partOfSpeech` becomes `data-sc-part-of-speech`.
/// `BM` becomes `data-sc-b-m`.
/// A CJK key has **no** separating hyphen, so `常用外マーク` becomes
/// `data-sc常用外マーク`.
///
/// This transform runs forward only. It never runs in reverse.
/// It is not injective because `content` and `Content` both produce
/// `data-sc-content`.
/// Therefore the code resolves the attribute name of each compiled selector
/// against the interned keys of a document.
/// It transforms each key once per document. It never reverses the name.
fn dataset_attr(key: &str, out: &mut String) {
    out.push_str("data-sc");
    let mut chars = key.chars();
    let Some(first) = chars.next() else { return };
    // The `sc` prefix places the first key letter inside the name. Therefore
    // `dataset` capitalizes it. A first character that is not an ASCII letter
    // stays unchanged.
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

/// Applies one dictionary stylesheet to one parsed tree.
///
/// This function completes the integration.
/// A node's tag, `data` map, ancestor chain, and sibling position select the
/// rules that win.
/// The cascade orders winners by importance, specificity, and source order.
/// Their declarations enter the node's resolved style record, **ahead of**
/// inline `style` declarations that already exist.
///
/// That position leaves inline semantics unchanged.
/// Before the fold writes a declaration, it drops declarations that the inline
/// record already sets or that an inline shorthand covers.
/// Therefore the two halves never name one property.
/// The inline `style` attribute then wins.
/// The order inside the record then does not matter.
/// A reader that takes the first occurrence of a property and a reader that
/// takes the last occurrence get the same answer.
/// Both `GlossDoc::style_of` and the layout pass depend on this invariant.
///
/// Order still matters *inside* the stylesheet half.
/// The compile step expands `margin` and `padding` into four longhands.
/// After expansion, no two declarations overlap.
/// The cascade then runs for each property, not for each list position.
pub fn apply(doc: &mut GlossDoc, sheet: &Sheet) {
    if sheet.is_empty() || doc.is_empty() {
        return;
    }
    let wins = resolve(doc, sheet);
    if !wins.is_empty() {
        doc.fold_declarations(&wins);
    }
}

/// Returns each stylesheet declaration that wins on a node, in node order.
///
/// The values are borrowed, so a win allocates nothing.
/// The compiled sheet keeps the text until [`GlossDoc::fold_declarations`]
/// copies it into the document's single text buffer.
fn resolve<'a>(doc: &GlossDoc, sheet: &'a Sheet) -> Vec<(NodeId, StyleKey, &'a str)> {
    let attr_of_key = resolve_keys(doc, sheet);
    // A tag rule or a bare `:first-child` rule can match a document whose keys
    // name none of the sheet attributes. Therefore this function must continue
    // here and must not return early. This case also explains why the attribute
    // buckets produce no candidates.
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

/// The interned keys of one document mapped to the sheet attribute IDs.
///
/// This map runs once for each document because the gloss arena interns keys.
/// This function computes each key's attribute name.
/// Every per-node probe then compares `u32` values.
/// The lookup uses binary search over sheet names, not a scan.
/// The largest corpus sheet has 322 attributes.
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

/// The tree descent and its reusable scratch buffers.
///
/// This type has two lifetimes because the walk borrows two unrelated values.
/// `'s` is the compiled sheet. It outlives the fold and lends each value string.
/// `'d` is the document and the two tables resolved against it.
/// They live only during this walk.
struct Walk<'s, 'd> {
    sheet: &'s Sheet,
    ctx: Ctx<'d>,
    /// The chain from a glossary item down to the node under test.
    path: Vec<NodeId>,
    /// Candidate rule ids for one node.
    cands: Vec<u32>,
    /// Match data for one node: `(importance, specificity, order, rule)`.
    hits: Vec<(bool, u32, u32, u32)>,
    out: Vec<(NodeId, StyleKey, &'s str)>,
}

impl<'s> Walk<'s, '_> {
    /// Visits one node and its subtree.
    ///
    /// The parser limits `depth` to
    /// [`MAX_DEPTH`](crate::dict::gloss::MAX_DEPTH).
    /// Therefore this recursion reaches the same maximum depth as the tree
    /// and no deeper.
    /// This function repeats the guard because a stack overflow cannot recover.
    fn descend(&mut self, id: NodeId, depth: u32) {
        if depth > crate::dict::gloss::MAX_DEPTH {
            return;
        }
        // The code copies the document reference from `ctx` first. The sibling
        // iterator then borrows the tree, not `self`. Recursion also avoids a
        // per-node child vector.
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

    /// Finds the declarations that win for one node and adds them to `out`.
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
        // Sorts matches in descending cascade order: importance, specificity,
        // then source order. For equal specificities, the later rule wins.
        // A reader takes the first occurrence of each property in the record.
        // Therefore the winner must come first.
        self.hits.sort_unstable_by(|a, b| b.cmp(a));

        let inline = doc.style(id);
        let at = self.out.len();
        // Importance belongs to each declaration, not to its rule. Therefore
        // an `!important` length beats every normal length, regardless of its
        // rule. This result needs a second pass over the same order.
        // The code uses the second pass only when a declaration is important.
        // 6 of the 14 corpus sheets have no `!important` declaration, so those
        // sheets use one pass. An `!important` declaration never beats the
        // inline `style` attribute. This module makes the inline `style`
        // attribute win without a condition.
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

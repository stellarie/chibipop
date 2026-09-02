//! A dictionary's `styles.css`, scanned into flat style rules.
//!
//! Hand-rolled and character-level, because no regex sees block structure:
//! nested at-rules, native `&` nesting, and a brace inside a string or a
//! `url(...)` all defeat a pattern that counts braces. This is deliberately
//! the same scan `tools/dict-census/census.py` runs over the corpus, so the
//! rule count the census reports and the rule count this compiles from are
//! the same number and the census stays a usable gauge.
//!
//! Total, by the same rule the tree parser follows: malformed input records
//! its first complaint, drops the rule it was inside, and returns everything
//! already scanned. A dictionary that ships a broken stylesheet must still
//! render, and a build must never fail over one.

/// One style rule, its `&` nesting already resolved into flat selectors.
pub(super) struct Rule {
    /// The rule's selector list, one entry per top-level comma.
    pub(super) selectors: Vec<String>,
    /// `(property, value)`, property lowercased, **in source order**.
    ///
    /// Order is load-bearing and must not be sorted: within one rule a
    /// shorthand that comes after a longhand overwrites it, which is exactly
    /// what applying them in this order reproduces.
    pub(super) decls: Vec<Decl>,
    /// Did this rule sit inside an at-rule body?
    ///
    /// Carried rather than dropped here so the compiler counts at-rule
    /// casualties in the same place it counts unsupported selectors. The
    /// corpus loses 4 box rules to this (`docs/research/dict-shapes.md`).
    pub(super) in_at_rule: bool,
    /// Source order, assigned where the block *opens*.
    ///
    /// Not where it closes: a nested block closes before its parent, and CSS
    /// says a nested rule cascades after the rule it sits in. Taking the
    /// index at `{` is the only reading that puts them in that order.
    pub(super) order: u32,
}

/// One declaration.
pub(super) struct Decl {
    pub(super) prop: String,
    pub(super) value: String,
    /// Was the value marked `!important`?
    ///
    /// Ordering *within* the sheet only. 96 of these appear across 8 corpus
    /// stylesheets, so stripping the marker and forgetting it would hand a
    /// declaration the author flagged as decisive to whichever rule happened
    /// to be more specific. It never outranks inline `style`, which this
    /// module's contract gives the last word unconditionally.
    pub(super) important: bool,
}

/// What one scan recovered.
pub(super) struct Scan {
    pub(super) rules: Vec<Rule>,
    /// The first thing that did not parse, for the build's report.
    pub(super) error: Option<&'static str>,
}

/// Block levels the scanner will descend.
///
/// Native CSS nesting is a tree, and resolving `&` multiplies selector lists
/// at every level, so an adversarial file could ask for a combinatorial
/// blow-up. The corpus nests three levels deep at most (Jitendex's
/// `td { & span { ... } }` inside a `& td` inside a forms rule), so this is
/// five times the deepest real file and exists only to bound the cost.
const MAX_NESTING: usize = 16;

/// Resolved selectors one sheet may carry.
///
/// The corpus's largest sheet compiles 322 selectors, so this bounds a
/// pathological file at roughly a hundred times the real maximum while
/// costing a real one nothing.
const MAX_SELECTORS: usize = 32_768;

/// One open block.
struct Frame {
    /// Resolved selector list. Empty for an at-rule body.
    selectors: Vec<String>,
    at_rule: bool,
    decls: Vec<Decl>,
    order: u32,
    /// Was this frame, or anything above it, an at-rule body?
    in_at_rule: bool,
}

pub(super) fn scan(css: &str) -> Scan {
    let mut out = Scan { rules: Vec::new(), error: None };
    let mut stack: Vec<Frame> = Vec::new();
    let mut buf = String::new();
    let mut parens: u32 = 0;
    let mut selectors_made: usize = 0;
    let mut opened: u32 = 0;
    let b = css.as_bytes();
    // Byte indices throughout, and safely: every character the scan reacts
    // to is ASCII, and a UTF-8 continuation byte is never one of them, so a
    // slice taken at any of these positions lands on a character boundary.
    let mut run = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                buf.push_str(&css[run..i]);
                match find(b, i + 2, b"*/") {
                    Some(end) => {
                        // A comment separates tokens, so it collapses to a
                        // space rather than to nothing: `a/**/b` is two
                        // selectors' worth of text, not one identifier.
                        buf.push(' ');
                        i = end + 2;
                    }
                    None => {
                        out.error = out.error.or(Some("unterminated comment"));
                        // Everything after an unterminated comment is
                        // comment, so the scan is over.
                        break;
                    }
                }
                run = i;
            }
            q @ (b'"' | b'\'') => {
                let (end, ok) = skip_string(b, i, q);
                if !ok {
                    out.error = out.error.or(Some("unterminated string"));
                }
                // Copied verbatim, quotes included: `list-style-type: "＊"`
                // needs its quotes to survive into the value.
                buf.push_str(&css[run..end]);
                run = end;
                i = end;
            }
            b'(' => {
                parens += 1;
                i += 1;
            }
            b')' => {
                parens = parens.saturating_sub(1);
                i += 1;
            }
            // Inside parentheses a brace or a semicolon is data - a
            // `url(data:...;base64,...)` or a `calc()` - and must not close
            // a block.
            _ if parens > 0 => i += 1,
            b'{' => {
                buf.push_str(&css[run..i]);
                let prelude = buf.trim();
                let frame = open(prelude, &stack, opened, &mut selectors_made);
                opened += 1;
                if stack.len() >= MAX_NESTING || selectors_made > MAX_SELECTORS {
                    out.error = out.error.or(Some("nesting or selector count over the cap"));
                    // Push it anyway, with nothing in it: the braces still
                    // have to balance or every rule after this one is lost.
                    stack.push(Frame { selectors: Vec::new(), ..frame });
                } else {
                    stack.push(frame);
                }
                buf.clear();
                i += 1;
                run = i;
            }
            b'}' => {
                buf.push_str(&css[run..i]);
                match stack.pop() {
                    Some(mut frame) => {
                        flush(&mut buf, &mut frame.decls);
                        if !frame.at_rule && !frame.selectors.is_empty() {
                            out.rules.push(Rule {
                                selectors: frame.selectors,
                                decls: frame.decls,
                                in_at_rule: frame.in_at_rule,
                                order: frame.order,
                            });
                        }
                    }
                    None => out.error = out.error.or(Some("unbalanced '}'")),
                }
                buf.clear();
                i += 1;
                run = i;
            }
            b';' => {
                buf.push_str(&css[run..i]);
                match stack.last_mut() {
                    // An at-statement - `@import url(...)`, `@charset` - is
                    // not a declaration and carries no value to keep.
                    Some(frame) if !buf.trim_start().starts_with('@') => {
                        flush(&mut buf, &mut frame.decls);
                    }
                    _ => buf.clear(),
                }
                i += 1;
                run = i;
            }
            _ => i += 1,
        }
    }
    if !stack.is_empty() {
        // Every open frame is a rule whose body never ended, so its
        // declarations are as unknowable as its extent. Drop them, which is
        // the same answer the tree parser gives a truncated item.
        out.error = out.error.or(Some("unclosed block"));
    }
    // Emission order is closing order, and a nested rule closes first.
    // `order` holds the opening index, so this restores source order.
    out.rules.sort_by_key(|r| r.order);
    out
}

/// One block's frame, with `&` resolved against the rule it nests in.
fn open(prelude: &str, stack: &[Frame], order: u32, made: &mut usize) -> Frame {
    let in_at_rule = stack.last().is_some_and(|f| f.in_at_rule);
    if prelude.starts_with('@') {
        return Frame {
            selectors: Vec::new(),
            at_rule: true,
            decls: Vec::new(),
            order,
            in_at_rule: true,
        };
    }
    // The nearest enclosing *style* rule, skipping at-rule bodies: an
    // `@media` between a rule and its nested child does not break the `&`
    // relationship.
    let parent = stack.iter().rev().find(|f| !f.at_rule).map(|f| &f.selectors[..]);
    let selectors = resolve(prelude, parent);
    *made += selectors.len();
    Frame { selectors, at_rule: false, decls: Vec::new(), order, in_at_rule }
}

/// A nested prelude flattened against its parent's selector list.
///
/// One flat selector per (parent, nested) pair, because that is what CSS
/// nesting means. The specificity this build then computes is the flattened
/// selector's own, which differs from the standard only when a parent list
/// holds selectors of unequal specificity - `&` formally takes the whole
/// list's maximum. Every nested rule in the corpus has a parent list whose
/// members are equally specific (Jitendex's
/// `div[data-sc-content="forms"], li[data-sc-content="forms"]` is the widest
/// at two), so the difference is not reachable from real data.
fn resolve(prelude: &str, parent: Option<&[String]>) -> Vec<String> {
    let own = split_selectors(prelude);
    let Some(parent) = parent else { return own };
    let mut out = Vec::with_capacity(parent.len() * own.len());
    for p in parent {
        for s in &own {
            out.push(if s.contains('&') {
                s.replace('&', p)
            } else {
                // Bare nesting: `ul { ... }` inside a rule means
                // `& ul { ... }`.
                format!("{p} {s}")
            });
        }
    }
    out
}

/// A prelude split on its top-level commas.
///
/// Top-level: a comma inside `:has(a, b)`, inside `[data-x="a,b"]`, or
/// inside a string is part of one selector.
pub(super) fn split_selectors(prelude: &str) -> Vec<String> {
    let b = prelude.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => i = skip_string(b, i, q).0,
            b'(' => i = skip_nested(b, i, b'(', b')'),
            b'[' => i = skip_nested(b, i, b'[', b']'),
            b',' => {
                push_trimmed(&mut out, &prelude[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push_trimmed(&mut out, &prelude[start..]);
    out
}

fn push_trimmed(out: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
}

/// One `;`-terminated declaration out of the buffer, which it empties.
fn flush(buf: &mut String, out: &mut Vec<Decl>) {
    let text = buf.trim();
    if let Some((head, value)) = text.split_once(':') {
        let prop = head.trim().to_ascii_lowercase();
        let (value, important) = strip_important(value.trim());
        if is_property(&prop) && !value.is_empty() {
            out.push(Decl { prop, value: value.to_string(), important });
        }
    }
    buf.clear();
}

/// `4px !important` as `("4px", true)`.
fn strip_important(value: &str) -> (&str, bool) {
    let Some(head) = value.strip_suffix("important") else { return (value, false) };
    let head = head.trim_end();
    match head.strip_suffix('!') {
        Some(v) => (v.trim_end(), true),
        None => (value, false),
    }
}

/// Is this a CSS property name?
///
/// `-{0,2}[a-z][-a-z0-9]*`, which admits the vendor prefixes the corpus
/// carries (`-webkit-border-radius`) and rejects the halves of a malformed
/// block that a `:` happens to split.
fn is_property(name: &str) -> bool {
    let rest = name.strip_prefix("--").or_else(|| name.strip_prefix('-')).unwrap_or(name);
    let mut chars = rest.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Just past the closing quote of the string opening at `i`.
///
/// `false` when there is none: a CSS string never spans a raw newline, so an
/// unterminated one ends at the line break and the caller records it.
fn skip_string(b: &[u8], i: usize, quote: u8) -> (usize, bool) {
    let mut i = i + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == quote => return (i + 1, true),
            b'\n' => return (i, false),
            _ => i += 1,
        }
    }
    (b.len(), false)
}

/// Just past the closer matching the opener at `i`, honouring strings and
/// nesting. Runs to the end on unbalanced input.
fn skip_nested(b: &[u8], i: usize, opener: u8, closer: u8) -> usize {
    let mut depth = 0i32;
    let mut i = i;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                i = skip_string(b, i, q).0;
                continue;
            }
            c if c == opener => depth += 1,
            c if c == closer => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= b.len() {
        return None;
    }
    b[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

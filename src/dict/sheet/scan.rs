//! A dictionary `styles.css` file, scanned into flat style rules.
//!
//! This scanner uses explicit byte-level logic. A regex cannot see block
//! structure. Nested at-rules, native `&` nesting, and braces inside a string
//! or `url(...)` defeat brace counting.
//! This code uses the scan from `tools/dict-census/census.py` over the corpus.
//! The census report and this code then have the same rule count.
//! The census provides a useful measure.
//!
//! This scan is total. It follows the tree parser's rule.
//! Malformed input records its first complaint.
//! The scan drops the rule that contains the problem and returns all earlier
//! rules.
//! A dictionary with a broken stylesheet must still render.
//! A build must not fail because one stylesheet is broken.

/// One style rule with `&` nesting resolved into flat selectors.
pub(super) struct Rule {
    /// The rule's selector list has one entry per top-level comma.
    pub(super) selectors: Vec<String>,
    /// `(property, value)`, with the property lowercased and entries kept
    /// **in source order**.
    ///
    /// This order is required. The code must not sort this list.
    /// Within one rule, a shorthand that follows a longhand overwrites it.
    /// The code applies declarations in source order to reproduce this result.
    pub(super) decls: Vec<Decl>,
    /// True when this rule sat inside an at-rule body.
    ///
    /// The scanner keeps this rule for the compiler.
    /// The compiler counts at-rule drops separately.
    /// The corpus loses 4 box rules to at-rule bodies
    /// (`docs/research/dict-shapes.md`).
    pub(super) in_at_rule: bool,
    /// Source order, assigned when the block opens.
    ///
    /// The block's closing position is not the source order.
    /// A nested block closes before its parent, but CSS places the nested rule
    /// after its parent in the cascade.
    /// The index at `{` is the only value that preserves this order.
    pub(super) order: u32,
}

/// One declaration.
pub(super) struct Decl {
    pub(super) prop: String,
    pub(super) value: String,
    /// True when the value has an `!important` marker.
    ///
    /// The marker changes order only *within* the sheet. 96 occur across
    /// 8 corpus stylesheets.
    /// If code strips the marker and forgets it, specificity alone chooses the
    /// winner, even against a declaration that the author marked decisive.
    /// The marker never outranks the inline `style` attribute.
    /// This module makes the inline `style` attribute win without exception.
    pub(super) important: bool,
}

/// The rules and first error that one scan recovers.
pub(super) struct Scan {
    pub(super) rules: Vec<Rule>,
    /// The first parse error for the build report.
    pub(super) error: Option<&'static str>,
}

/// The deepest block nesting that the scanner allows.
///
/// Native CSS nesting forms a tree.
/// Each level multiplies selector lists when the code resolves `&`.
/// Therefore an adversarial file could cause a combinatorial blow-up.
/// The corpus nests three levels deep at most.
/// Jitendex's `td { & span { ... } }` is inside a `& td` block, which is inside
/// a forms rule.
/// This limit is five times the deepest real file.
/// It exists only to bound the cost.
const MAX_NESTING: usize = 16;

/// The maximum number of resolved selectors in one sheet.
///
/// The largest corpus sheet compiles 322 selectors.
/// This limit bounds a pathological file at about 100 times the real maximum.
/// This limit does not affect real files.
const MAX_SELECTORS: usize = 32_768;

/// One open block.
struct Frame {
    /// Resolved selector list. This list is empty for an at-rule body.
    selectors: Vec<String>,
    at_rule: bool,
    decls: Vec<Decl>,
    order: u32,
    /// True when this frame or any enclosing frame is an at-rule body.
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
    // The code uses byte indices throughout. Every character that the scan
    // handles is ASCII, and a UTF-8 continuation byte is never one of them.
    // Therefore every slice at these positions starts at a character boundary.
    let mut run = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                buf.push_str(&css[run..i]);
                match find(b, i + 2, b"*/") {
                    Some(end) => {
                        // A comment separates tokens, so this code replaces
                        // it with one space. For example, `a/**/b` contains
                        // two selectors' worth of text, not one identifier.
                        buf.push(' ');
                        i = end + 2;
                    }
                    None => {
                        out.error = out.error.or(Some("unterminated comment"));
                        // Everything after an unterminated comment belongs to
                        // the comment, so the scan ends.
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
                // The scan copies a string verbatim, with its quotes.
                // For example, `list-style-type: "＊"` needs its quotes in
                // the value.
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
            // Inside parentheses, a brace or a semicolon is data. The scanner
            // must not close a block on either character. Examples include a
            // `url(data:...;base64,...)` value and a `calc()` expression.
            _ if parens > 0 => i += 1,
            b'{' => {
                buf.push_str(&css[run..i]);
                let prelude = buf.trim();
                let frame = open(prelude, &stack, opened, &mut selectors_made);
                opened += 1;
                if stack.len() >= MAX_NESTING || selectors_made > MAX_SELECTORS {
                    out.error = out.error.or(Some("nesting or selector count over the cap"));
                    // The code pushes an empty frame after the cap. The
                    // braces must still balance, or every later rule is lost.
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
                    // An at-statement such as `@import url(...)` or `@charset`
                    // is not a declaration and has no value to keep.
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
        // Each open frame has a body that never ended. Its declarations have
        // unknown extent, so the scan drops them. The tree parser gives the
        // same result for a truncated item.
        out.error = out.error.or(Some("unclosed block"));
    }
    // Rules emit when blocks close, so nested rules emit before parents. The
    // `order` field stores each block's opening index. This sort restores
    // source order.
    out.rules.sort_by_key(|r| r.order);
    out
}

/// One block frame with `&` resolved against its parent rule.
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
    // This is the nearest style rule that encloses this block. The walk skips
    // at-rule bodies. An `@media` block between a rule and its nested child
    // does not break the `&` relationship.
    let parent = stack.iter().rev().find(|f| !f.at_rule).map(|f| &f.selectors[..]);
    let selectors = resolve(prelude, parent);
    *made += selectors.len();
    Frame { selectors, at_rule: false, decls: Vec::new(), order, in_at_rule }
}

/// Flattens a nested prelude against its parent's selector list.
///
/// CSS nesting creates one flat selector for each parent and nested pair.
/// The specificity code then uses each flattened selector's own specificity.
/// This differs from the standard only when a parent list has unequal
/// specificity. CSS assigns `&` the maximum specificity of the full list.
/// Every nested rule in the corpus has equally specific parent selectors.
/// Jitendex's `div[data-sc-content="forms"], li[data-sc-content="forms"]` is
/// the widest list, with two members. Therefore this difference does not occur
/// in real data.
fn resolve(prelude: &str, parent: Option<&[String]>) -> Vec<String> {
    let own = split_selectors(prelude);
    let Some(parent) = parent else { return own };
    let mut out = Vec::with_capacity(parent.len() * own.len());
    for p in parent {
        for s in &own {
            out.push(if s.contains('&') {
                s.replace('&', p)
            } else {
                // Bare nesting means `ul { ... }` inside a rule becomes
                // `& ul { ... }`.
                format!("{p} {s}")
            });
        }
    }
    out
}

/// Splits a prelude at its top-level commas.
///
/// A comma inside `:has(a, b)`, `[data-x="a,b"]`, or a string stays in one
/// selector.
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

/// Extracts one declaration that ends at `;` and clears the buffer.
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

/// Parses `4px !important` as `("4px", true)`.
fn strip_important(value: &str) -> (&str, bool) {
    let Some(head) = value.strip_suffix("important") else { return (value, false) };
    let head = head.trim_end();
    match head.strip_suffix('!') {
        Some(v) => (v.trim_end(), true),
        None => (value, false),
    }
}

/// Checks whether text is a CSS property name.
///
/// The grammar is `-{0,2}[a-z][-a-z0-9]*`.
/// It accepts vendor prefixes that the corpus contains, such as
/// `-webkit-border-radius`.
/// It also rejects the two malformed block halves that a `:` can split.
fn is_property(name: &str) -> bool {
    let rest = name.strip_prefix("--").or_else(|| name.strip_prefix('-')).unwrap_or(name);
    let mut chars = rest.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Returns the position after the closing quote for the string at `i`.
///
/// Returns `false` when no closing quote exists.
/// A CSS string cannot cross a raw newline. Therefore an unterminated string
/// ends at the line break, and the caller records this error.
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

/// Returns the position after the closer that matches the opener at `i`.
/// The function skips strings and tracks nested openers and closers.
/// It reaches the input end when the input is unbalanced.
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

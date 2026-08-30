//! The plain-text renderer.
//!
//! One of three renderers over [`GlossDoc`] - the other two are the Anki HTML
//! field ([`super::html`]) and the popup scene (`ui::layout`). Before ticket
//! 02 this walk and the HTML walk were two independent recursions over the
//! raw JSON, which is what let the card and the popup drift apart; they now
//! read one parsed tree.
//!
//! Two consumers need a plain string and will keep needing one: the
//! collapsed-row summary, which is one line by construction, and the Anki
//! plain-text `glossary` field.
//!
//! Both are summaries, so this walk applies [`RoleFilter::SUMMARY`] - one
//! named policy through the one role gate every renderer over this tree
//! shares, rather than a fourth idea of what an example is. It is a fixed
//! policy and not a parameter because [`renders_text`] is the *build*-time
//! gate on whether a term row becomes an `entry` row at all: a setting that
//! reached in here would make a dictionary's row count depend on a popup
//! preference.
//!
//! The line-breaking rules are ticket 01's and are reproduced exactly.
//! A block tag emits a mark, [`tidy`] turns every mark into a line break and
//! drops the empty parts, and an inline tag's children cannot break a line
//! because the schema admits no break there. One exemption, shared with the
//! popup's inline pass: a marked inline node beside bare sentence text is
//! markup inside a sentence, not a sense separator, and stays in its line
//! ([`GlossDoc::prose`]).

use super::{GlossDoc, ItemType, Kind, NodeId, Role, RoleFilter, Tag};

/// A pending line break, written into the render buffer and resolved by
/// [`tidy`]. A private-use control sequence rather than `\n` so that a
/// dictionary's own `\n` inside a string is distinguishable from a break this
/// renderer decided on.
const BLOCK_MARK: &str = "\u{0}LI\u{0}";

/// A reading, in parentheses after its base.
///
/// `<ruby>猫<rt>ねこ</rt></ruby>` is `猫(ねこ)` here, which is what the
/// census's highest-volume bilingual dictionary already writes by hand -
/// JMdict renders readings as parenthetical text and never emits `ruby` at
/// all - so the two shapes reach a card as one shape.
///
/// The parentheses are added only when nothing already supplies them.
/// `<rp>` exists precisely to carry them for a renderer that cannot draw
/// ruby, and this is that renderer, so a ruby that brought its own is left
/// alone rather than printed as `猫((ねこ))`; so is a reading a dictionary
/// wrote already bracketed.
///
/// One slot per `rt`, in document order, so per-character furigana -
/// `<ruby>漢<rt>かん</rt>字<rt>じ</rt></ruby>` - renders `漢(かん)字(じ)`
/// and not `漢字(かんじ)`.
fn ruby_into(doc: &GlossDoc, id: NodeId, out: &mut String) {
    let fenced = doc.children(id).any(|c| doc.node(c).tag == Tag::Rp);
    let mut prose = doc.prose(id);
    for child in doc.children(id) {
        if doc.node(child).tag != Tag::Rt {
            node_into(doc, child, prose, out);
            prose = prose || doc.inline_prose(child);
            continue;
        }
        let mut reading = String::new();
        children_into(doc, child, false, &mut reading);
        let reading = reading.trim();
        if reading.is_empty() {
            continue;
        }
        if fenced || bracketed(reading) {
            out.push_str(reading);
            continue;
        }
        out.push('(');
        out.push_str(reading);
        out.push(')');
    }
}

/// Does this reading already carry its own brackets?
///
/// Both widths, because a Japanese dictionary writing them by hand writes
/// the full-width pair.
fn bracketed(reading: &str) -> bool {
    let open = reading.starts_with('(') || reading.starts_with('（');
    let close = reading.ends_with(')') || reading.ends_with('）');
    open && close
}

/// One plain-text string per top-level glossary item.
pub fn plain_items(doc: &GlossDoc) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for id in doc.items() {
        buf.clear();
        item_into(doc, id, &mut buf);
        let text = tidy(&buf);
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}

/// Does any glossary item render any plain text at all?
///
/// The dictionary builder's question, asked once per term-bank row: a
/// glossary that renders nothing - an image-only gaiji row, a whitespace-only
/// wrapper - is not an entry and gets no `entry` record. Answered off the
/// same walk [`plain_items`] uses, but without building the strings it would
/// then throw away, which over two and a half million term rows is worth not
/// doing.
///
/// Exactly equivalent to `plain_items(doc).is_empty()`: [`tidy`] keeps a part
/// if and only if the part survives `str::trim`, so testing the parts is
/// testing the result.
pub fn renders_text(doc: &GlossDoc) -> bool {
    let mut buf = String::new();
    for id in doc.items() {
        buf.clear();
        item_into(doc, id, &mut buf);
        if buf.split(BLOCK_MARK).any(|part| !part.trim().is_empty()) {
            return true;
        }
    }
    false
}

/// One top-level glossary item into the render buffer.
fn item_into(doc: &GlossDoc, id: NodeId, buf: &mut String) {
    match doc.node(id).item_type {
        // An image-only item has no plain text at all.
        ItemType::Image => {}
        ItemType::Text => buf.push_str(doc.text(id)),
        // Yomitan drops a `structured-content` item's children straight into
        // a block `.gloss-content`, so the item itself neither breaks a line
        // nor takes part in the drop rules.
        ItemType::StructuredContent => children_into(doc, id, true, buf),
        _ if doc.is_plain_string(id) => buf.push_str(doc.text(id)),
        _ => node_into(doc, id, false, buf),
    }
}

/// The part-of-speech labels, in order and without duplicates.
///
/// A separate walk rather than a by-product of [`plain_items`], because the
/// labels are the card's own field: `Card::pos` renders them above the
/// glosses, so leaving them inline would print them twice.
pub fn pos_labels(doc: &GlossDoc) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for id in doc.items() {
        // A bare glossary string carries no `data` map and so no label.
        if doc.is_plain_string(id) {
            continue;
        }
        collect_pos(doc, id, &mut out, &mut buf);
    }
    out
}

/// One node's part-of-speech labels, and its children's.
///
/// Two roles stop this walk, and they stop it for opposite reasons.
/// [`Role::PartOfSpeech`] is the label itself: collect it and do not
/// descend, because a label is a leaf as far as this field goes. An example
/// or an attribution stops it because the subtree is editorial matter the
/// summary drops whole - a label written inside a quoted sentence describes
/// the quotation, not the entry, so hoisting it into the card's own `pos`
/// field would put a word in the dictionary's mouth. That was the deleted
/// drop list's rule too; the role is only how the rule is spelled now.
fn collect_pos(doc: &GlossDoc, id: NodeId, out: &mut Vec<String>, buf: &mut String) {
    let role = doc.role(id);
    if role == Role::PartOfSpeech {
        buf.clear();
        children_into(doc, id, !doc.node(id).tag.is_inline(), buf);
        let label = tidy(buf);
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
        }
        return;
    }
    if !RoleFilter::SUMMARY.allows(role) {
        return;
    }
    for child in doc.children(id) {
        collect_pos(doc, child, out, buf);
    }
}

/// One node into the render buffer.
///
/// A node's own rendering depends on one thing about the context it sits
/// in: `prose` says whether a bare string with visible text stands beside
/// it, or a prose fragment stands before it - either is what exempts a
/// marked inline node from the marker line break ([`GlossDoc::prose`],
/// [`GlossDoc::inline_prose`]). Everything else is the children's, and that
/// context comes from this node's own tag: whether a run of strings becomes
/// lines is decided in [`children_into`] from the parent tag alone, and
/// whether a reading is bracketed in [`ruby_into`] from the `ruby` it sits
/// under.
///
/// An image is the one tag with no plain rendering at all: it is a character
/// this renderer cannot draw, which is why the tree keeps it for the popup
/// renderer that can. An `rt` or `rp` reached outside a `ruby` is malformed
/// and renders as the plain text it holds, which is exactly what the popup's
/// inline pass does with it.
fn node_into(doc: &GlossDoc, id: NodeId, prose: bool, out: &mut String) {
    let n = doc.node(id);
    if !RoleFilter::SUMMARY.allows(doc.role(id)) {
        return;
    }
    if n.kind == Kind::Image {
        return;
    }
    if n.tag == Tag::Ruby {
        ruby_into(doc, id, out);
        return;
    }
    if n.tag == Tag::Br {
        out.push('\n');
        return;
    }
    if n.kind == Kind::Text && n.tag == Tag::None {
        out.push_str(doc.text(id));
        return;
    }
    if n.tag.is_block() || (doc.has_marker(id) && !prose) {
        out.push_str(BLOCK_MARK);
    }
    children_into(doc, id, !n.tag.is_inline(), out);
}

/// A node's children.
///
/// Yomitan concatenates them, with one exception:
/// [`GlossDoc::is_string_list`] - a glossary array of plain strings is a
/// list, one line per string.
///
/// `block_ctx` is false only under an inline tag, where the schema admits no
/// line break: Yomitan appends every child of an inline node into the one
/// box.
fn children_into(doc: &GlossDoc, id: NodeId, block_ctx: bool, out: &mut String) {
    if block_ctx && doc.is_string_list(id) {
        for child in doc.children(id) {
            out.push_str(BLOCK_MARK);
            out.push_str(doc.text(child));
        }
        return;
    }
    let mut prose = doc.prose(id);
    for child in doc.children(id) {
        node_into(doc, child, prose, out);
        prose = prose || doc.inline_prose(child);
    }
}

/// Cleans one rendered string.
///
/// Every block mark becomes a line break, and an empty part is dropped so
/// that nested blocks give one line per innermost block instead of a run of
/// blank ones. Both platform text engines break hard on `\n`, so a break made
/// here reaches the panel.
fn tidy(text: &str) -> String {
    let parts: Vec<&str> =
        text.split(BLOCK_MARK).map(str::trim).filter(|p| !p.is_empty()).collect();
    let collapsed = collapse_spaces(&parts.join("\n"));
    collapsed.split('\n').map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Collapses space, tab, U+3000.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\u{3000}') {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc;
    use super::*;
    use serde_json::{json, Value};

    /// The plain glosses of one glossary, straight from a JSON fixture -
    /// the seam the spec names: structured content in, rendered output out.
    fn plain(g: &Value) -> Vec<String> {
        plain_items(&doc(g))
    }

    fn pos(g: &Value) -> Vec<String> {
        pos_labels(&doc(g))
    }

    fn pos_doc(labels: &[&str]) -> Value {
        let mut spans: Vec<Value> = labels
            .iter()
            .map(|label| {
                json!({
                    "tag": "span",
                    "data": {"class": "tag", "code": "n", "content": "part-of-speech-info"},
                    "content": label,
                })
            })
            .collect();
        spans.push(json!({
            "tag": "div",
            "data": {"content": "sense"},
            "content": {
                "tag": "ul",
                "data": {"content": "glossary"},
                "content": [
                    {"tag": "li", "content": "chatting"},
                    {"tag": "li", "content": "idle talk"},
                ],
            },
        }));
        json!([{"type": "structured-content", "content": spans}])
    }

    #[test]
    fn adjacent_marked_blocks_are_separated_by_a_line_break() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "sense"}, "content": "one"},
            {"tag": "span", "data": {"content": "sense"}, "content": "two"}
        ]}]);
        assert_eq!(vec!["one\ntwo".to_string()], plain(&g));
    }

    #[test]
    fn a_non_string_marker_still_breaks_the_line() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": 5}, "content": "one"},
            {"tag": "span", "data": {"content": true}, "content": "two"}
        ]}]);
        assert_eq!(vec!["one\ntwo".to_string()], plain(&g));
    }

    /// Jitendex writes a loanword note as a marked span inside the
    /// parenthesis (`data.content = "lang-source-wasei"`, 4 114 nodes),
    /// and the marker break used to cut the note in two. Beside bare
    /// sentence text a marker separates nothing ([`GlossDoc::prose`]).
    #[test]
    fn a_marked_span_amid_bare_text_stays_in_its_line() {
        let g = json!([{"type": "structured-content", "content": [
            "（",
            {"tag": "span", "data": {"content": "lang-source-wasei"}, "content": "wasei"},
            "： night + er）"
        ]}]);
        assert_eq!(vec!["（wasei： night + er）".to_string()], plain(&g));
    }

    #[test]
    fn a_null_marker_does_not_separate_blocks() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": null}, "content": "one"},
            {"tag": "span", "data": {"content": null}, "content": "two"}
        ]}]);
        assert_eq!(vec!["onetwo".to_string()], plain(&g));
    }

    #[test]
    fn an_ideographic_space_collapses_but_a_line_break_survives() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "あ\u{3000}\u{3000}い"},
            {"tag": "br"},
            {"tag": "span", "content": "  う  "}
        ]}]);
        assert_eq!(vec!["あ い\nう".to_string()], plain(&g));
    }

    /// Ticket 11 took `rt` off the drop list: a reading is information a
    /// reader of a plain string wants, and dropping it silently was the bug.
    /// An image stays dropped - it is a character this renderer cannot
    /// draw.
    #[test]
    fn a_reading_renders_after_its_base_and_an_image_is_still_dropped() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "content": "ねこ"}
            ]},
            {"tag": "img", "content": "ignored"}
        ]}]);
        assert_eq!(vec!["猫(ねこ)".to_string()], plain(&g));
    }

    /// `rp` carries the parentheses HTML wrote for a renderer that cannot
    /// draw ruby, and this is that renderer, so it must not add a second
    /// pair around them.
    #[test]
    fn a_ruby_that_brought_its_own_parentheses_is_not_bracketed_twice() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                "猫",
                {"tag": "rp", "content": "(«"},
                {"tag": "rt", "content": "ねこ"},
                {"tag": "rp", "content": "»)"}
            ]}
        ]}]);
        assert_eq!(vec!["猫(«ねこ»)".to_string()], plain(&g));
    }

    /// The same rule when the dictionary bracketed the reading itself
    /// rather than delegating to `rp`, in the full width a Japanese
    /// dictionary writes.
    #[test]
    fn a_reading_written_already_bracketed_is_left_alone() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": ["猫", {"tag": "rt", "content": "（ねこ）"}]}
        ]}]);
        assert_eq!(vec!["猫（ねこ）".to_string()], plain(&g));
    }

    /// Per-character furigana is several pairings in one `ruby`, which is
    /// how a monolingual dictionary writes a two-kanji headword. Each
    /// reading follows its own base, not all of them the last one.
    #[test]
    fn per_character_furigana_brackets_each_reading_after_its_own_base() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                "漢", {"tag": "rt", "content": "かん"},
                "字", {"tag": "rt", "content": "じ"}
            ]}
        ]}]);
        assert_eq!(vec!["漢(かん)字(じ)".to_string()], plain(&g));
    }

    /// An `rt` reached with no `ruby` around it is malformed. It renders as
    /// the text it holds, unbracketed, which is exactly what the popup's
    /// inline pass does with it - the two renderers may not disagree.
    #[test]
    fn an_orphaned_rt_renders_as_plain_text() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "reading: "},
            {"tag": "rt", "content": "ねこ"}
        ]}]);
        assert_eq!(vec!["reading: ねこ".to_string()], plain(&g));
    }

    #[test]
    fn part_of_speech_is_collected_in_order_without_duplicates() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "suru"},
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "content": "chatting"}
        ]}]);
        assert_eq!(vec!["noun".to_string(), "suru".to_string()], pos(&g));
        assert_eq!(vec!["chatting".to_string()], plain(&g));
    }

    #[test]
    fn an_example_sentence_contributes_neither_text_nor_part_of_speech() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "verb"},
                {"tag": "span", "content": "a sentence"}
            ]},
            {"tag": "span", "content": "real gloss"}
        ]}]);
        assert_eq!(vec!["real gloss".to_string()], plain(&g));
        assert!(pos(&g).is_empty());
    }

    #[test]
    fn an_image_only_sense_yields_nothing() {
        assert!(plain(&json!([{"type": "image", "path": "x.png"}])).is_empty());
    }

    #[test]
    fn a_plain_string_passes_through() {
        assert_eq!(vec!["to eat".to_string()], plain(&json!(["to eat"])));
    }

    #[test]
    fn a_typed_text_node_uses_its_text_field() {
        let g = json!([{"type": "text", "text": "to eat"}]);
        assert_eq!(vec!["to eat".to_string()], plain(&g));
    }

    #[test]
    fn nested_tags_flatten_to_their_text() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "div", "content": [
                {"tag": "span", "content": "repetition mark"}
            ]}
        ]}]);
        assert_eq!(vec!["repetition mark".to_string()], plain(&g));
    }

    /// A `ruby` whose content is a single node rather than an array, which
    /// is the shape most of the census's ruby-emitting dictionaries write.
    #[test]
    fn ruby_reads_a_non_list_content_and_still_brackets_its_reading() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "ruby", "content": ["一", {"tag": "rt", "content": "いち"}]
        }}]);
        assert_eq!(vec!["一(いち)".to_string()], plain(&g));
    }

    #[test]
    fn an_image_sibling_is_dropped() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg"},
            {"tag": "span", "content": "meaning"}
        ]}]);
        assert_eq!(vec!["meaning".to_string()], plain(&g));
    }

    #[test]
    fn an_image_type_item_with_no_siblings_is_dropped() {
        let g = json!([{"type": "image", "path": "a.avif"}]);
        assert!(plain(&g).is_empty());
    }

    #[test]
    fn br_becomes_a_newline() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "a"},
            {"tag": "br"},
            {"tag": "span", "content": "b"}
        ]}]);
        assert_eq!(vec!["a\nb".to_string()], plain(&g));
    }

    #[test]
    fn a_list_of_three_items_renders_as_three_lines() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "ul", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"},
                {"tag": "li", "content": "third"}
            ]}}]);
        assert_eq!(vec!["first\nsecond\nthird".to_string()], plain(&g));
    }

    #[test]
    fn two_sibling_divs_render_on_two_lines() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "div", "content": "to run"},
            {"tag": "div", "content": "to flow"}
        ]}]);
        assert_eq!(vec!["to run\nto flow".to_string()], plain(&g));
    }

    #[test]
    fn nested_blocks_give_one_line_per_innermost_block() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": {"tag": "ul", "content": [
                {"tag": "li", "content": {"tag": "div", "content": "outer"}},
                {"tag": "li", "content": {"tag": "div", "content": [
                    {"tag": "ul", "content": {"tag": "li", "content": "inner"}}
                ]}}
            ]}}}]);
        assert_eq!(vec!["outer\ninner".to_string()], plain(&g));
    }

    #[test]
    fn a_br_between_blocks_does_not_double_the_break() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "div", "content": "a"},
            {"tag": "br"},
            {"tag": "div", "content": "b"}
        ]}]);
        assert_eq!(vec!["a\nb".to_string()], plain(&g));
    }

    #[test]
    fn a_br_inside_a_block_breaks_its_line() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": ["one", {"tag": "br"}, "two"]}}]);
        assert_eq!(vec!["one\ntwo".to_string()], plain(&g));
    }

    #[test]
    fn a_table_row_breaks_the_line() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "table", "content": [
                {"tag": "tr", "content": {"tag": "td", "content": "未然形"}},
                {"tag": "tr", "content": {"tag": "td", "content": "連用形"}}
            ]}}]);
        assert_eq!(vec!["未然形\n連用形".to_string()], plain(&g));
    }

    #[test]
    fn a_summary_does_not_run_into_its_details_body() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "details", "content": [
                {"tag": "summary", "content": "conjugation"},
                {"tag": "div", "content": "たべる"}
            ]}}]);
        assert_eq!(vec!["conjugation\nたべる".to_string()], plain(&g));
    }

    /// LF and CRLF alike: a dictionary that stores its own line breaks
    /// inside one string keeps them, and each glossary item stays its own
    /// vec entry.
    #[test]
    fn a_plain_string_keeps_its_own_line_breaks() {
        let g = json!(["one\ntwo", "three\r\nfour"]);
        let expected = vec!["one\ntwo".to_string(), "three\nfour".to_string()];
        assert_eq!(expected, plain(&g));
    }

    #[test]
    fn an_array_of_plain_strings_becomes_one_line_each() {
        let g = json!([{"type": "structured-content", "content": ["to run", "to flow"]}]);
        assert_eq!(vec!["to run\nto flow".to_string()], plain(&g));
    }

    #[test]
    fn an_inline_array_of_plain_strings_stays_on_one_line() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "span", "content": ["to ", "run"]}}]);
        assert_eq!(vec!["to run".to_string()], plain(&g));
    }

    /// Prose broken up by its own inline markup, not a list of lines.
    #[test]
    fn a_string_array_mixed_with_a_link_stays_on_one_line() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": [
                "see also ",
                {"tag": "a", "href": "?query=x", "content": "x"}
            ]}}]);
        assert_eq!(vec!["see also x".to_string()], plain(&g));
    }

    #[test]
    fn an_all_whitespace_result_is_dropped() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": ["  ", {"tag": "img"}, "  "]}}]);
        assert!(plain(&g).is_empty());
    }

    #[test]
    fn an_attribution_subtree_is_dropped() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "to eat"},
            {"tag": "div", "data": {"content": "attribution"}, "content": [
                {"tag": "a", "content": "JMdict"},
                " | Tatoeba ",
                {"tag": "a", "content": "[1]"}
            ]}
        ]}]);
        assert_eq!(vec!["to eat".to_string()], plain(&g));
    }

    #[test]
    fn a_nested_example_sentence_is_dropped() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "to eat"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "div", "data": {"content": "example-sentence-a"},
                 "content": "もっと果物を食べるべきです。"},
                {"tag": "div", "data": {"content": "example-sentence-b"}, "content": [
                    {"tag": "span", "content": "You should eat more fruit."},
                    {"tag": "span", "data": {"content": "attribution-footnote"},
                     "content": "[1]"}
                ]}
            ]}
        ]}]);
        assert_eq!(vec!["to eat".to_string()], plain(&g));
    }

    #[test]
    fn adjacent_non_pos_markers_get_a_line_break() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "xref"}, "content": "see also"},
            {"tag": "span", "data": {"content": "forms"}, "content": "alt form"}
        ]}]);
        assert_eq!(vec!["see also\nalt form".to_string()], plain(&g));
    }

    #[test]
    fn pos_labels_are_extracted_in_order() {
        let g = pos_doc(&["noun", "suru", "intransitive"]);
        let expected = vec!["noun".to_string(), "suru".to_string(), "intransitive".to_string()];
        assert_eq!(expected, pos(&g));
    }

    #[test]
    fn pos_labels_do_not_leak_into_the_glossary() {
        let g = pos_doc(&["noun", "suru", "intransitive"]);
        assert_eq!(vec!["chatting\nidle talk".to_string()], plain(&g));
    }

    #[test]
    fn repeated_pos_labels_are_deduped() {
        let g = pos_doc(&["noun", "noun", "suru"]);
        assert_eq!(vec!["noun".to_string(), "suru".to_string()], pos(&g));
    }

    #[test]
    fn no_pos_markers_yields_an_empty_list() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "span", "content": "今日の一日前の日。"}}]);
        assert!(pos(&g).is_empty());
    }

    #[test]
    fn a_plain_string_glossary_has_no_pos() {
        assert!(pos(&json!(["to eat"])).is_empty());
    }

    #[test]
    fn pos_on_null_or_empty_is_safe() {
        assert!(pos(&json!(null)).is_empty());
        assert!(pos(&json!([])).is_empty());
    }

    #[test]
    fn pos_inside_a_dropped_subtree_is_ignored() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "ghost"}
            ]}
        ]}]);
        assert_eq!(vec!["noun".to_string()], pos(&g));
    }

    /// The dictionary builder writes an `entry` row if and only if
    /// [`renders_text`] says yes, so the two must never disagree: a drift
    /// here changes every dictionary's entry count and every `entry_id`.
    #[test]
    fn renders_text_agrees_with_plain_items_on_every_shape() {
        let cases = [
            json!([]),
            json!(null),
            json!(["to eat"]),
            json!([""]),
            json!(["   "]),
            json!(["\u{3000}\u{3000}"]),
            json!(["\n\n"]),
            json!([{"type": "text", "text": "to eat"}]),
            json!([{"type": "text", "text": " "}]),
            json!([{"type": "image", "path": "x.png"}]),
            json!([{"type": "structured-content", "content": {"tag": "img", "path": "g.svg"}}]),
            json!([{"type": "structured-content", "content": {
                "tag": "div", "content": ["  ", {"tag": "img"}, "  "]}}]),
            json!([{"type": "structured-content", "content": {
                "tag": "div", "content": [{"tag": "br"}, {"tag": "br"}]}}]),
            json!([{"type": "structured-content", "content": {
                "tag": "div", "data": {"content": "attribution"}, "content": "JMdict"}}]),
            json!([{"type": "structured-content", "content": {
                "tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"}}]),
            json!([{"type": "structured-content", "content": [
                {"tag": "div", "content": "to run"}, {"tag": "div", "content": "to flow"}]}]),
            json!([{"type": "structured-content", "content": {"tag": "ruby", "content": [
                {"tag": "rt", "content": "ねこ"}]}}]),
            json!([42, true, ["nested"]]),
        ];
        for g in cases {
            let d = doc(&g);
            assert_eq!(
                !plain_items(&d).is_empty(),
                renders_text(&d),
                "renders_text disagreed with plain_items on {g}"
            );
        }
    }
}

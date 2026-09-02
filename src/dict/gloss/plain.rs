//! The plain-text renderer reads a [`GlossDoc`] document-tree.
//!
//! [`super::html`] renders Anki HTML, and `ui::layout` builds a `PopupScene`.
//! Before the arena rewrite, the plain-text walk and HTML walk parsed raw JSON
//! separately. Their results could differ. Both renderers now read one parsed
//! tree.
//!
//! Two consumers need a plain string. A collapsed-row summary needs one line.
//! The Anki plain-text `glossary` field needs all summary lines.
//!
//! Both outputs are summaries, so this renderer applies
//! [`RoleFilter::SUMMARY`]. All [`GlossDoc`] renderers use this role policy.
//! At Dictionary build time, [`renders_text`] decides whether a term row
//! becomes an `entry` row. A popup setting must not change the number of
//! Dictionary rows. The fixed policy is not a parameter.
//!
//! This renderer preserves the original line-break rules. A block tag emits a
//! mark. [`tidy`] changes each mark to a line break and removes empty parts.
//! Inline children cannot add a line break because the schema forbids one
//! there. The popup inline pass uses the same rule. A marked inline node beside
//! plain sentence text stays on its line. [`GlossDoc::prose`] identifies this
//! case.

use super::{GlossDoc, ItemType, Kind, NodeId, Role, RoleFilter, Tag};

/// A line-break mark that waits in the render buffer for [`tidy`].
///
/// The renderer uses a private-use control sequence instead of `\n`. This
/// separates a Dictionary's `\n` from a break that the renderer adds.
const BLOCK_MARK: &str = "\u{0}LI\u{0}";

/// Writes a ruby base and its readings to a plain-text buffer.
///
/// `<ruby>猫<rt>ねこ</rt></ruby>` becomes `猫(ねこ)`. The largest bilingual
/// Dictionary in the census already uses this form. JMdict writes readings as
/// parenthetical text and does not emit `ruby`. Both forms therefore produce
/// the same Anki card text.
///
/// The renderer adds parentheses only when the source does not supply them.
/// `<rp>` supplies parentheses for a renderer that cannot draw ruby. This
/// renderer keeps those parentheses and does not print `猫((ねこ))`. It also
/// keeps brackets that a Dictionary writes around the reading.
///
/// The renderer uses one slot for each `rt` in document order. Therefore,
/// `<ruby>漢<rt>かん</rt>字<rt>じ</rt></ruby>` becomes `漢(かん)字(じ)`.
/// It does not become `漢字(かんじ)`.
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

/// Tests whether a reading already has brackets.
///
/// This function accepts standard and full-width pairs. A Japanese Dictionary
/// can write the full-width pair.
fn bracketed(reading: &str) -> bool {
    let open = reading.starts_with('(') || reading.starts_with('（');
    let close = reading.ends_with(')') || reading.ends_with('）');
    open && close
}

/// Renders one plain-text string for each top-level glossary item.
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

/// Tests whether any glossary item produces plain-text output.
///
/// The Dictionary builder calls this function once for each term-bank row. A
/// glossary with no plain-text output is not an Entry and gets no `entry`
/// record. Examples include an image-only gaiji row and a wrapper that contains
/// only spaces.
///
/// This function uses the [`plain_items`] walk but does not allocate output
/// strings. This saves allocations across more than two million term rows.
///
/// The result equals `!plain_items(doc).is_empty()`. [`tidy`] keeps a part only
/// when that part remains after `str::trim`. This function tests the same parts.
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

/// Renders one top-level glossary item into the buffer.
fn item_into(doc: &GlossDoc, id: NodeId, buf: &mut String) {
    match doc.node(id).item_type {
        // An image-only item has no plain-text output.
        ItemType::Image => {}
        ItemType::Text => buf.push_str(doc.text(id)),
        // Yomitan places the children of a `structured-content` item directly
        // in a `.gloss-content` block. The item therefore adds no line break
        // and does not use the role rules.
        ItemType::StructuredContent => children_into(doc, id, true, buf),
        _ if doc.is_plain_string(id) => buf.push_str(doc.text(id)),
        _ => node_into(doc, id, false, buf),
    }
}

/// Returns part-of-speech labels in order without duplicates.
///
/// This separate walk prevents duplicate labels in the Anki card.
/// `Card::pos` places labels above the glosses, and [`plain_items`] omits the
/// same labels from the glosses.
pub fn pos_labels(doc: &GlossDoc) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for id in doc.items() {
        // A plain glossary string has no `data` map or label.
        if doc.is_plain_string(id) {
            continue;
        }
        collect_pos(doc, id, &mut out, &mut buf);
    }
    out
}

/// Collects part-of-speech labels from one node and its descendants.
///
/// Two roles stop this walk for different reasons. [`Role::PartOfSpeech`] is
/// the label. The walk collects that label but does not enter its descendants.
///
/// An example or attribution is Editorial content that the summary omits. A
/// label inside a quotation describes that quotation, not the Entry. It must
/// not enter the card's `pos` field. The old drop list used the same rule.
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

/// Renders one node into the buffer.
///
/// `prose` records this node's context. It is true when visible plain-text is
/// beside the node or a prose fragment precedes it. In this context, a marked
/// inline node does not add a line break. See [`GlossDoc::prose`] and
/// [`GlossDoc::inline_prose`].
///
/// The parent tag sets the child context. [`children_into`] uses only that tag
/// to decide whether a string sequence becomes separate lines.
/// [`ruby_into`] uses the parent `ruby` node to decide whether a reading needs
/// brackets.
///
/// An image is the only tag with no plain-text output. It can represent a
/// character that this renderer cannot draw. The popup renderer can draw that
/// character. An `rt` or `rp` outside a `ruby` is malformed. This renderer
/// keeps its plain-text output, as the popup inline pass does.
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

/// Renders the children of one node.
///
/// Yomitan concatenates child output with one exception.
/// [`GlossDoc::is_string_list`] identifies a glossary array of plain strings.
/// Each string in that array gets one line.
///
/// `block_ctx` is false only under an inline tag. The schema permits no line
/// break there. Therefore, Yomitan puts all children of an inline node in one
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

/// Resolves one rendered string.
///
/// Each block mark becomes a line break. This function removes empty parts.
/// Nested blocks therefore produce one line for each innermost block. They do
/// not produce a sequence of blank lines.
///
/// Both platform text engines use `\n` as a hard break. A break from this
/// function therefore reaches the Panel.
fn tidy(text: &str) -> String {
    let parts: Vec<&str> =
        text.split(BLOCK_MARK).map(str::trim).filter(|p| !p.is_empty()).collect();
    let collapsed = collapse_spaces(&parts.join("\n"));
    collapsed.split('\n').map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Collapses each run of supported whitespace to one space.
///
/// Supported whitespace includes spaces, tabs, and U+3000.
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

    /// Returns plain-text glosses from one JSON fixture.
    ///
    /// The fixture supplies structured content. This helper returns the
    /// rendered output that the specification defines.
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

    /// Jitendex places a marked loanword note inside the parentheses.
    ///
    /// `data.content = "lang-source-wasei"` occurs on 4,114 nodes. The old
    /// marker rule split each note. A marker beside sentence text does not
    /// separate blocks. See [`GlossDoc::prose`].
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

    /// An `rt` remains because a plain-text reader needs its reading. The old
    /// drop rule lost this text. An image remains omitted because this renderer
    /// cannot draw it.
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

    /// HTML uses `rp` to supply parentheses for a renderer that cannot draw
    /// ruby. The plain-text renderer must use these parentheses. It must not add
    /// a second pair.
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

    /// This test applies the same rule when a Dictionary supplies brackets in
    /// `rt` text instead of an `rp`. It uses Japanese full-width brackets.
    #[test]
    fn a_reading_written_already_bracketed_is_left_alone() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": ["猫", {"tag": "rt", "content": "（ねこ）"}]}
        ]}]);
        assert_eq!(vec!["猫（ねこ）".to_string()], plain(&g));
    }

    /// A monolingual Dictionary can put several base and reading pairs in one
    /// `ruby`. Each reading must follow its own base.
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

    /// An `rt` without a `ruby` is malformed. This renderer outputs its text
    /// without brackets, as the popup inline pass does. Both renderers must
    /// agree.
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

    /// Most Dictionaries in the census put one node directly in the `content`
    /// of a `ruby`. They do not use an array for that node.
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

    /// A Dictionary can store LF or CRLF line breaks in one string.
    ///
    /// The renderer keeps these breaks. It keeps each glossary item in a
    /// separate `Vec` element.
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

    /// Inline markup can divide prose without making a list of lines.
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

    /// The Dictionary builder writes an `entry` row only when [`renders_text`]
    /// returns true. This result must agree with [`plain_items`]. A difference
    /// would change each Dictionary's Entry count and each `entry_id`.
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

//! Glossary flattening.

use serde_json::Value;

const BLOCK_MARK: &str = "\u{0}LI\u{0}";
const POS_CONTENT: &str = "part-of-speech-info";
const DROP_TAGS: [&str; 3] = ["rt", "rp", "img"];
const DROP_CONTENT: [&str; 6] = [
    "attribution",
    "attribution-footnote",
    "example-keyword",
    "example-sentence",
    "example-sentence-a",
    "example-sentence-b",
];

/// Plain-text glosses.
pub fn flatten_glossary(glossary: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(items) = glossary.as_array() else { return out };
    for item in items {
        let text = match item {
            Value::String(s) => tidy(s),
            Value::Object(_) => {
                let kind = item.get("type").and_then(Value::as_str);
                match kind {
                    Some("text") => tidy(item.get("text").and_then(Value::as_str).unwrap_or("")),
                    Some("structured-content") => tidy(&content_of(item)),
                    Some("image") => String::new(),
                    _ => tidy(&render(item)),
                }
            }
            _ => String::new(),
        };
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}

/// Part-of-speech labels.
pub fn extract_pos(glossary: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(items) = glossary.as_array() {
        for item in items {
            if item.is_object() {
                collect_pos(item, &mut out);
            }
        }
    }
    out
}

/// data.content, if not null.
fn content_marker(node: &Value) -> Option<&Value> {
    node.get("data").and_then(|d| d.get("content")).filter(|v| !v.is_null())
}

/// One node to plain text.
fn render(node: &Value) -> String {
    match node {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(render).collect(),
        Value::Object(_) => {
            let present = content_marker(node);
            let marker = present.and_then(Value::as_str);
            if marker.is_some_and(|m| DROP_CONTENT.contains(&m) || m == POS_CONTENT) {
                return String::new();
            }
            let tag = node.get("tag").and_then(Value::as_str);
            if tag.is_some_and(|t| DROP_TAGS.contains(&t)) {
                return String::new();
            }
            if tag == Some("br") {
                return "\n".to_string();
            }
            if tag == Some("li") || present.is_some() {
                let mut marked = String::from(BLOCK_MARK);
                marked.push_str(&content_of(node));
                return marked;
            }
            content_of(node)
        }
        _ => String::new(),
    }
}

/// A node's content field.
fn content_of(node: &Value) -> String {
    node.get("content").map_or_else(String::new, render)
}

/// POS labels under one node.
fn collect_pos(node: &Value, out: &mut Vec<String>) {
    if let Some(items) = node.as_array() {
        for child in items {
            collect_pos(child, out);
        }
        return;
    }
    if !node.is_object() {
        return;
    }
    let marker = node.get("data").and_then(|d| d.get("content")).and_then(Value::as_str);
    if marker.is_some_and(|m| DROP_CONTENT.contains(&m)) {
        return;
    }
    if marker == Some(POS_CONTENT) {
        let label = tidy(&content_of(node));
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
        }
        return;
    }
    if let Some(content) = node.get("content") {
        collect_pos(content, out);
    }
}

/// Cleans one rendered string.
fn tidy(text: &str) -> String {
    let parts: Vec<&str> = text
        .split(BLOCK_MARK)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let collapsed = collapse_spaces(&parts.join("; "));
    collapsed
        .split('\n')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
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
    use super::*;
    use serde_json::json;

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
    fn adjacent_marked_blocks_are_separated_not_fused() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "sense"}, "content": "one"},
            {"tag": "span", "data": {"content": "sense"}, "content": "two"}
        ]}]);
        assert_eq!(vec!["one; two".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn a_non_string_marker_still_separates_blocks() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": 5}, "content": "one"},
            {"tag": "span", "data": {"content": true}, "content": "two"}
        ]}]);
        assert_eq!(vec!["one; two".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn a_null_marker_does_not_separate_blocks() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": null}, "content": "one"},
            {"tag": "span", "data": {"content": null}, "content": "two"}
        ]}]);
        assert_eq!(vec!["onetwo".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn an_ideographic_space_collapses_but_a_line_break_survives() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "あ\u{3000}\u{3000}い"},
            {"tag": "br"},
            {"tag": "span", "content": "  う  "}
        ]}]);
        assert_eq!(vec!["あ い\nう".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn furigana_and_images_are_dropped_but_ruby_base_text_is_kept() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "ruby", "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "content": "ねこ"}
            ]},
            {"tag": "img", "content": "ignored"}
        ]}]);
        assert_eq!(vec!["猫".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn part_of_speech_is_collected_in_order_without_duplicates() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "suru"},
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "span", "content": "chatting"}
        ]}]);
        assert_eq!(vec!["noun".to_string(), "suru".to_string()], extract_pos(&g));
        assert_eq!(vec!["chatting".to_string()], flatten_glossary(&g));
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
        assert_eq!(vec!["real gloss".to_string()], flatten_glossary(&g));
        assert!(extract_pos(&g).is_empty());
    }

    #[test]
    fn an_image_only_sense_yields_nothing() {
        assert!(flatten_glossary(&json!([{"type": "image", "path": "x.png"}])).is_empty());
    }

    #[test]
    fn a_plain_string_passes_through() {
        assert_eq!(vec!["to eat".to_string()], flatten_glossary(&json!(["to eat"])));
    }

    #[test]
    fn a_typed_text_node_uses_its_text_field() {
        let g = json!([{"type": "text", "text": "to eat"}]);
        assert_eq!(vec!["to eat".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn nested_tags_flatten_to_their_text() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "div", "content": [
                {"tag": "span", "content": "repetition mark"}
            ]}
        ]}]);
        assert_eq!(vec!["repetition mark".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn ruby_keeps_base_when_content_is_not_a_list() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "ruby", "content": ["一", {"tag": "rt", "content": "いち"}]
        }}]);
        assert_eq!(vec!["一".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn an_image_sibling_is_dropped() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "img", "path": "gaiji/x.svg"},
            {"tag": "span", "content": "meaning"}
        ]}]);
        assert_eq!(vec!["meaning".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn an_image_type_item_with_no_siblings_is_dropped() {
        let g = json!([{"type": "image", "path": "a.avif"}]);
        assert!(flatten_glossary(&g).is_empty());
    }

    #[test]
    fn br_becomes_a_newline() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "content": "a"},
            {"tag": "br"},
            {"tag": "span", "content": "b"}
        ]}]);
        assert_eq!(vec!["a\nb".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn list_items_get_a_separator() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "ul", "content": [
                {"tag": "li", "content": "first"},
                {"tag": "li", "content": "second"}
            ]}}]);
        assert_eq!(vec!["first; second".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn an_all_whitespace_result_is_dropped() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "div", "content": ["  ", {"tag": "img"}, "  "]}}]);
        assert!(flatten_glossary(&g).is_empty());
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
        assert_eq!(vec!["to eat".to_string()], flatten_glossary(&g));
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
        assert_eq!(vec!["to eat".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn adjacent_non_pos_markers_get_a_separator() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "xref"}, "content": "see also"},
            {"tag": "span", "data": {"content": "forms"}, "content": "alt form"}
        ]}]);
        assert_eq!(vec!["see also; alt form".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn pos_labels_are_extracted_in_order() {
        let g = pos_doc(&["noun", "suru", "intransitive"]);
        let expected = vec!["noun".to_string(), "suru".to_string(), "intransitive".to_string()];
        assert_eq!(expected, extract_pos(&g));
    }

    #[test]
    fn pos_labels_do_not_leak_into_the_glossary() {
        let g = pos_doc(&["noun", "suru", "intransitive"]);
        assert_eq!(vec!["chatting; idle talk".to_string()], flatten_glossary(&g));
    }

    #[test]
    fn repeated_pos_labels_are_deduped() {
        let g = pos_doc(&["noun", "noun", "suru"]);
        assert_eq!(vec!["noun".to_string(), "suru".to_string()], extract_pos(&g));
    }

    #[test]
    fn no_pos_markers_yields_an_empty_list() {
        let g = json!([{"type": "structured-content", "content": {
            "tag": "span", "content": "今日の一日前の日。"}}]);
        assert!(extract_pos(&g).is_empty());
    }

    #[test]
    fn a_plain_string_glossary_has_no_pos() {
        assert!(extract_pos(&json!(["to eat"])).is_empty());
    }

    #[test]
    fn extract_pos_on_null_or_empty_is_safe() {
        assert!(extract_pos(&json!(null)).is_empty());
        assert!(extract_pos(&json!([])).is_empty());
    }

    #[test]
    fn pos_inside_a_dropped_subtree_is_ignored() {
        let g = json!([{"type": "structured-content", "content": [
            {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
            {"tag": "div", "data": {"content": "example-sentence"}, "content": [
                {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "ghost"}
            ]}
        ]}]);
        assert_eq!(vec!["noun".to_string()], extract_pos(&g));
    }
}

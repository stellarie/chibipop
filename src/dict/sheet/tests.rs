//! What the matcher promises, asserted against the shapes the corpus ships.
//!
//! Table-driven wherever the answer is a mapping, because the interesting
//! failures are one row wrong rather than the whole rule wrong. Every CSS
//! fixture here is either lifted verbatim from a real stylesheet or built
//! from a real selector shape, named where it came from.

use super::*;
use crate::dict::gloss::{plain_items, GlossDoc, NodeId, StyleKey, Tag};
use serde_json::json;

/// One structured-content glossary, parsed.
fn doc(content: serde_json::Value) -> GlossDoc {
    GlossDoc::parse(&json!([{"type": "structured-content", "content": content}]).to_string())
}

/// The first node carrying this tag, in parse order.
fn find(d: &GlossDoc, tag: Tag) -> NodeId {
    (0..d.all_nodes().len() as NodeId)
        .find(|id| d.node(*id).tag == tag)
        .unwrap_or_else(|| panic!("no {tag:?} node"))
}

/// One node's resolved record, as `(property, value)` text.
fn record(d: &GlossDoc, id: NodeId) -> Vec<(StyleKey, String)> {
    d.style(id)
        .iter()
        .map(|(k, v)| (*k, d.scalar_str(*v).unwrap_or("<non-text>").to_string()))
        .collect()
}

/// One property off a node's record, by the first-occurrence rule every
/// reader of the record uses.
fn prop(d: &GlossDoc, id: NodeId, key: StyleKey) -> Option<String> {
    d.style_of(id, key).and_then(|v| d.scalar_str(v)).map(str::to_string)
}

/// Parses, compiles, folds.
fn styled(css: &str, content: serde_json::Value) -> GlossDoc {
    let sheet = Sheet::compile(css);
    let mut d = doc(content);
    apply(&mut d, &sheet);
    d
}

// ---- the data-key to attribute-name transform ----

/// Every selector in a dictionary's stylesheet depends on this exact
/// transform, and it is not obvious: the DOM's `dataset` capitalises the
/// key's own first letter because Yomitan's `sc` prefix puts it in the
/// middle, then turns every ASCII capital into `-x`, and leaves a CJK key
/// entirely alone. Ported from `tools/dict-census/census.py`, which verified
/// it against the corpus; these are the awkward cases the ticket names plus
/// the four most-selected real keys.
#[test]
fn a_data_key_becomes_the_attribute_name_yomitan_derives() {
    let cases = [
        ("content", "data-sc-content"),
        ("class", "data-sc-class"),
        ("partOfSpeech", "data-sc-part-of-speech"),
        ("BM", "data-sc-b-m"),
        ("BL2", "data-sc-b-l2"),
        ("Bc", "data-sc-bc"),
        ("常用外マーク", "data-sc常用外マーク"),
        ("親字", "data-sc親字"),
        ("使い分け", "data-sc使い分け"),
        // A digit is not a capital, so it takes no hyphen of its own.
        ("l1", "data-sc-l1"),
        ("", "data-sc"),
    ];
    let mut out = String::new();
    for (key, want) in cases {
        out.clear();
        dataset_attr(key, &mut out);
        assert_eq!(want, out, "data key {key:?}");
    }
}

// ---- the property table ----

/// The kebab-case half of the tree parser's camelCase table, and no wider.
/// The `unmapped` rows are the corpus's highest-volume unsupported
/// properties: dropping them is the ticket's rule, and a table that quietly
/// grew past the renderer would put a value in a record nothing can read.
#[test]
fn only_the_properties_the_renderer_maps_survive() {
    let mapped = [
        ("font-size", StyleKey::FontSize),
        ("font-weight", StyleKey::FontWeight),
        ("color", StyleKey::Color),
        ("background-color", StyleKey::BackgroundColor),
        ("border-radius", StyleKey::BorderRadius),
        ("border-width", StyleKey::BorderWidth),
        ("border-style", StyleKey::BorderStyle),
        ("border-color", StyleKey::BorderColor),
        ("vertical-align", StyleKey::VerticalAlign),
        ("word-break", StyleKey::WordBreak),
        ("list-style-type", StyleKey::ListStyleType),
        ("margin-right", StyleKey::MarginRight),
        ("padding-left", StyleKey::PaddingLeft),
    ];
    for (prop, want) in mapped {
        assert_eq!(Some(want), css_key(prop), "{prop}");
    }
    let unmapped = [
        "display",
        "grid-column",
        "line-height",
        "font-family",
        "margin-inline-start",
        "padding-block-end",
        "border-block-start",
        "box-shadow",
        "animation",
        "marginTop",
    ];
    for prop in unmapped {
        assert!(css_key(prop).is_none(), "{prop} should not map");
    }
}

// ---- the grammar the census scores against ----

/// The two allow-lists `tools/dict-census/census.py` reads out of
/// `select.rs` have to say what the parser beside them actually does, or the
/// census's dropped-rule count is a number about nothing. Both directions:
/// every listed pseudo-class compiles, and a pseudo-class outside the list
/// drops its rule.
#[test]
fn the_declared_pseudo_class_list_is_the_one_the_parser_accepts() {
    for name in SUPPORTED_PSEUDO_CLASSES {
        // `nth-*` needs an argument and the list forms need a selector; each
        // gets the shape it takes.
        let sel = match name {
            "is" | "where" | "not" | "has" => format!("span:{name}(div)"),
            n if n.starts_with("nth-") => format!("span:{name}(2)"),
            _ => format!("span:{name}"),
        };
        let sheet = Sheet::compile(&format!("{sel} {{ color: red }}"));
        assert_eq!(1, sheet.counts().kept, "{sel} should compile");
    }
    for name in ["hover", "link", "focus", "checked", "lang", "nth-col"] {
        let sheet = Sheet::compile(&format!("span:{name} {{ color: red }}"));
        assert_eq!(1, sheet.counts().dropped_selector, ":{name} should drop its rule");
    }
}

/// The selector-kind list, in the census's vocabulary, against one selector
/// of each kind it names and one of each kind it does not.
#[test]
fn the_declared_selector_kind_list_is_the_one_the_parser_accepts() {
    let by_kind = [
        ("tag", "span { color: red }"),
        ("data-attr", "[data-sc-content=\"a\"] { color: red }"),
        ("pseudo-class", "span:first-child { color: red }"),
    ];
    for (kind, css) in by_kind {
        assert!(
            SUPPORTED_SELECTOR_KINDS.contains(&kind),
            "{kind} is compiled but not declared",
        );
        assert_eq!(1, Sheet::compile(css).counts().kept, "{css}");
    }
    let refused = [
        ("class", ".x { color: red }"),
        ("id", "#x { color: red }"),
        ("universal", "* { color: red }"),
        ("other-attr", "[title] { color: red }"),
        ("pseudo-element", "span::before { content: \"x\" }"),
    ];
    for (kind, css) in refused {
        assert!(
            !SUPPORTED_SELECTOR_KINDS.contains(&kind),
            "{kind} is declared but refused",
        );
        assert_eq!(1, Sheet::compile(css).counts().dropped_selector, "{css}");
    }
}

// ---- a css-only dictionary draws its boxes ----

/// 字通's own shape: a `data-sc`-keyed span carrying a full box, and not one
/// inline `style` anywhere in the entry. This is the test that the mechanism
/// works at all - before this ticket the box model drew nothing for the 13
/// dictionaries in the census's `css-only` bucket.
#[test]
fn a_css_only_dictionary_gets_a_box_with_no_inline_style_anywhere() {
    let d = styled(
        "span[data-sc音] {
             background-color: #eef;
             border-style: solid;
             border-width: 1px;
             border-radius: 0.3em;
             padding: 0.1em 0.4em;
         }",
        json!([{"tag": "span", "data": {"音": "呉"}, "content": "ゴ"}]),
    );
    let span = find(&d, Tag::Span);
    assert_eq!(
        vec![
            (StyleKey::BackgroundColor, "#eef".to_string()),
            (StyleKey::BorderStyle, "solid".to_string()),
            (StyleKey::BorderWidth, "1px".to_string()),
            (StyleKey::BorderRadius, "0.3em".to_string()),
            (StyleKey::PaddingTop, "0.1em".to_string()),
            (StyleKey::PaddingRight, "0.4em".to_string()),
            (StyleKey::PaddingBottom, "0.1em".to_string()),
            (StyleKey::PaddingLeft, "0.4em".to_string()),
        ],
        record(&d, span),
    );
    // And the tree the renderer reads is otherwise untouched.
    assert_eq!(vec!["ゴ".to_string()], plain_items(&d));
}

/// 明鏡国語辞典's own selector shape, `div:has(> span[data-sc-zoomkanji])`,
/// which is the one relative `:has` in the corpus.
#[test]
fn a_has_selector_reaches_a_parent_through_its_child() {
    let css = "div:has(> span[data-sc-zoomkanji]) { border-width: 2px; }";
    let hit = styled(
        css,
        json!([{"tag": "div", "content": [
            {"tag": "span", "data": {"zoomkanji": "漢"}, "content": "漢"}
        ]}]),
    );
    assert_eq!(Some("2px".to_string()), prop(&hit, find(&hit, Tag::Div), StyleKey::BorderWidth));

    // A grandchild is not a child, and `:has(> x)` says child.
    let miss = styled(
        css,
        json!([{"tag": "div", "content": [
            {"tag": "div", "content": [
                {"tag": "span", "data": {"zoomkanji": "漢"}, "content": "漢"}
            ]}
        ]}]),
    );
    assert_eq!(None, prop(&miss, find(&miss, Tag::Div), StyleKey::BorderWidth));
}

/// 旺文社漢字典's `td:has([data-sc親字])`, the descendant form the ticket
/// names, on a real CJK `data` key.
#[test]
fn a_descendant_has_selector_reaches_a_cell_through_a_cjk_data_key() {
    let d = styled(
        "td:has([data-sc親字]) { padding-left: 0.5em; }",
        json!([{"tag": "table", "content": [{"tag": "tr", "content": [
            {"tag": "td", "content": [{"tag": "span", "data": {"親字": "亜"}, "content": "亜"}]},
            {"tag": "td", "content": "plain"}
        ]}]}]),
    );
    let cells: Vec<Option<String>> = (0..d.all_nodes().len() as NodeId)
        .filter(|id| d.node(*id).tag == Tag::Td)
        .map(|id| prop(&d, id, StyleKey::PaddingLeft))
        .collect();
    assert_eq!(vec![Some("0.5em".to_string()), None], cells);
}

// ---- the cascade ----

/// Inline `style` is the dictionary author's last word, whatever the
/// stylesheet asks for. Asserted at both extremes: against a rule of the
/// highest specificity this grammar can express, and against `!important`,
/// which outranks every other stylesheet declaration and still loses here.
#[test]
fn inline_style_beats_a_stylesheet_rule_of_any_specificity() {
    let d = styled(
        "span { font-size: 3em }
         div span[data-sc-content=\"a\"][data-sc-class=\"b\"] { font-size: 4em }
         span { font-size: 5em !important }",
        json!([{"tag": "div", "content": [{
            "tag": "span",
            "data": {"content": "a", "class": "b"},
            "style": {"fontSize": "0.8em"},
            "content": "x"
        }]}]),
    );
    let span = find(&d, Tag::Span);
    assert_eq!(Some("0.8em".to_string()), prop(&d, span, StyleKey::FontSize));
    // And the losing declarations are not merely outranked, they are absent:
    // one record entry per property is what keeps the record's own order
    // from deciding anything.
    assert_eq!(1, record(&d, span).len(), "{:?}", record(&d, span));
}

/// The specificity rule this build implements, stated: no id or class
/// selector can compile, so specificity is `(b, c)` where **b** counts
/// attribute tests and pseudo-classes and **c** counts tag names, compared
/// as one packed number, with source order breaking a tie and the later rule
/// winning. `:where` contributes nothing; `:is`, `:not` and `:has`
/// contribute their most specific argument.
#[test]
fn a_more_specific_rule_beats_a_less_specific_one() {
    let cases: [(&str, &str, &str); 5] = [
        // (0,1) tag loses to (1,0) attribute.
        ("span { color: red } [data-sc-content=\"a\"] { color: blue }", "blue", "attr > tag"),
        // (1,1) beats (1,0), whatever the source order.
        (
            "span[data-sc-content=\"a\"] { color: blue } [data-sc-content=\"a\"] { color: red }",
            "blue",
            "tag+attr > attr, against source order",
        ),
        // (2,1) beats (1,1).
        (
            "span[data-sc-content=\"a\"] { color: red }
             div span[data-sc-content=\"a\"] { color: red }
             span[data-sc-content=\"a\"][data-sc-class=\"b\"] { color: blue }",
            "blue",
            "two attributes beat one attribute and two tags",
        ),
        // Equal specificity: the later rule wins.
        ("span { color: red } span { color: blue }", "blue", "source order breaks a tie"),
        // `:where` adds nothing, so this stays (0,1) and loses to (1,0).
        (
            ":where([data-sc-class=\"b\"]) span { color: red }
             [data-sc-content=\"a\"] { color: blue }",
            "blue",
            ":where contributes no specificity",
        ),
    ];
    for (css, want, why) in cases {
        let d = styled(
            css,
            json!([{"tag": "div", "data": {"class": "b"}, "content": [
                {"tag": "span", "data": {"content": "a", "class": "b"}, "content": "x"}
            ]}]),
        );
        let span = find(&d, Tag::Span);
        assert_eq!(Some(want.to_string()), prop(&d, span, StyleKey::Color), "{why}");
    }
}

/// `!important` orders declarations *within* one stylesheet, which is what
/// its 96 corpus occurrences ask for. It is not a second origin: the test
/// above already fixes that it loses to inline `style`.
#[test]
fn an_important_declaration_beats_a_more_specific_normal_one() {
    let d = styled(
        "div span[data-sc-content=\"a\"] { color: red }
         span { color: blue !important }",
        json!([{"tag": "div", "content": [
            {"tag": "span", "data": {"content": "a"}, "content": "x"}
        ]}]),
    );
    assert_eq!(Some("blue".to_string()), prop(&d, find(&d, Tag::Span), StyleKey::Color));
}

/// A descendant selector constrains the ancestor chain, and a node whose
/// chain fails does not match - which is the whole difference between a
/// matcher and the census's key-presence count.
#[test]
fn a_descendant_selector_does_not_match_a_node_whose_ancestors_fail() {
    let css = "li[data-sc-content=\"sense\"] ul[data-sc-content=\"glossary\"] \
               { list-style-type: none }";
    let under = styled(
        css,
        json!([{"tag": "li", "data": {"content": "sense"}, "content": [
            {"tag": "ul", "data": {"content": "glossary"}, "content": [
                {"tag": "li", "content": "to eat"}
            ]}
        ]}]),
    );
    assert_eq!(
        Some("none".to_string()),
        prop(&under, find(&under, Tag::Ul), StyleKey::ListStyleType),
    );

    let outside = styled(
        css,
        json!([{"tag": "li", "data": {"content": "forms"}, "content": [
            {"tag": "ul", "data": {"content": "glossary"}, "content": [
                {"tag": "li", "content": "to eat"}
            ]}
        ]}]),
    );
    assert_eq!(None, prop(&outside, find(&outside, Tag::Ul), StyleKey::ListStyleType));
}

/// A child combinator is not a descendant combinator.
#[test]
fn a_child_combinator_refuses_a_grandchild() {
    let css = "div > span { color: red }";
    let child = styled(css, json!([{"tag": "div", "content": [{"tag": "span", "content": "x"}]}]));
    assert_eq!(Some("red".to_string()), prop(&child, find(&child, Tag::Span), StyleKey::Color));

    let grandchild = styled(
        css,
        json!([{"tag": "div", "content": [{"tag": "code", "content": [
            {"tag": "span", "content": "x"}
        ]}]}]),
    );
    assert_eq!(None, prop(&grandchild, find(&grandchild, Tag::Span), StyleKey::Color));
}

/// Jitendex's `li[data-sc-content="sense-group"]:first-child`, and the
/// sibling that is not first.
#[test]
fn first_child_and_nth_child_count_siblings_from_one() {
    let d = styled(
        "li:first-child { margin-top: 0.1em }
         li:nth-child(2) { margin-top: 0.5em }
         li:nth-child(2n+1) { color: red }",
        json!([{"tag": "ul", "content": [
            {"tag": "li", "content": "one"},
            {"tag": "li", "content": "two"},
            {"tag": "li", "content": "three"}
        ]}]),
    );
    let items: Vec<(Option<String>, Option<String>)> = (0..d.all_nodes().len() as NodeId)
        .filter(|id| d.node(*id).tag == Tag::Li)
        .map(|id| (prop(&d, id, StyleKey::MarginTop), prop(&d, id, StyleKey::Color)))
        .collect();
    assert_eq!(
        vec![
            (Some("0.1em".to_string()), Some("red".to_string())),
            (Some("0.5em".to_string()), None),
            (None, Some("red".to_string())),
        ],
        items,
    );
}

/// `nth-of-type` counts only siblings sharing the tag, which is what makes
/// it different from `nth-child` on a mixed list.
#[test]
fn of_type_pseudos_count_only_siblings_sharing_the_tag() {
    let d = styled(
        "span:first-of-type { color: red }
         span:last-of-type { color: blue }",
        json!([{"tag": "div", "content": [
            {"tag": "code", "content": "a"},
            {"tag": "span", "content": "b"},
            {"tag": "code", "content": "c"},
            {"tag": "span", "content": "d"}
        ]}]),
    );
    let spans: Vec<Option<String>> = (0..d.all_nodes().len() as NodeId)
        .filter(|id| d.node(*id).tag == Tag::Span)
        .map(|id| prop(&d, id, StyleKey::Color))
        .collect();
    assert_eq!(vec![Some("red".to_string()), Some("blue".to_string())], spans);
}

// ---- CSS nesting ----

/// Jitendex writes its marker-suppression rule with native `&` nesting, so
/// a scanner that flattens blocks without resolving `&` would lose it. The
/// text here is that rule verbatim.
#[test]
fn native_nesting_resolves_against_the_rule_it_sits_in() {
    let d = styled(
        "li[data-sc-content=\"sense\"] {
             padding-left: 0.25em;
             & ul[data-sc-content=\"glossary\"] {
                 list-style-type: none;
                 padding-left: 0.25em;
             }
         }",
        json!([{"tag": "li", "data": {"content": "sense"}, "content": [
            {"tag": "ul", "data": {"content": "glossary"}, "content": [
                {"tag": "li", "content": "to eat"}
            ]}
        ]}]),
    );
    assert_eq!(
        Some("0.25em".to_string()),
        prop(&d, find(&d, Tag::Li), StyleKey::PaddingLeft),
        "the parent rule's own declaration",
    );
    assert_eq!(
        Some("none".to_string()),
        prop(&d, find(&d, Tag::Ul), StyleKey::ListStyleType),
        "the nested rule, resolved through `&`",
    );
}

/// A nested block with no `&` takes an implicit descendant one, which is
/// what CSS nesting means and what Jitendex's forms rules are written as.
#[test]
fn a_nested_block_without_an_ampersand_is_a_descendant() {
    let d = styled(
        "li[data-sc-content=\"forms\"] { & table { margin-top: 0.2em } th { border-width: 1px } }",
        json!([{"tag": "li", "data": {"content": "forms"}, "content": [
            {"tag": "table", "content": [{"tag": "tr", "content": [
                {"tag": "th", "content": "h"}
            ]}]}
        ]}]),
    );
    assert_eq!(Some("0.2em".to_string()), prop(&d, find(&d, Tag::Table), StyleKey::MarginTop));
    assert_eq!(Some("1px".to_string()), prop(&d, find(&d, Tag::Th), StyleKey::BorderWidth));
}

// ---- shorthand expansion ----

/// A `margin`/`padding` shorthand expands into its four longhands at compile
/// time, so the cascade runs per property. Without that, the shorthand from
/// a less specific rule would be applied after - and therefore erase - the
/// longhand from a more specific one, because a record applies in order.
#[test]
fn a_shorthand_cannot_erase_a_longhand_from_a_more_specific_rule() {
    let d = styled(
        "span[data-sc-content=\"a\"] { padding-left: 9px }
         span { padding: 1px }",
        json!([{"tag": "span", "data": {"content": "a"}, "content": "x"}]),
    );
    let span = find(&d, Tag::Span);
    assert_eq!(Some("9px".to_string()), prop(&d, span, StyleKey::PaddingLeft));
    assert_eq!(Some("1px".to_string()), prop(&d, span, StyleKey::PaddingTop));
    assert_eq!(Some("1px".to_string()), prop(&d, span, StyleKey::PaddingRight));
    assert_eq!(Some("1px".to_string()), prop(&d, span, StyleKey::PaddingBottom));
}

/// The 1/2/3/4-value edge shorthand, each form.
#[test]
fn an_edge_shorthand_splits_the_way_css_splits_it() {
    let cases = [
        ("1px", ["1px", "1px", "1px", "1px"]),
        ("1px 2px", ["1px", "2px", "1px", "2px"]),
        ("1px 2px 3px", ["1px", "2px", "3px", "2px"]),
        ("1px 2px 3px 4px", ["1px", "2px", "3px", "4px"]),
    ];
    for (value, want) in cases {
        let d = styled(
            &format!("span {{ margin: {value} }}"),
            json!([{"tag": "span", "content": "x"}]),
        );
        let span = find(&d, Tag::Span);
        let got = [
            prop(&d, span, StyleKey::MarginTop),
            prop(&d, span, StyleKey::MarginRight),
            prop(&d, span, StyleKey::MarginBottom),
            prop(&d, span, StyleKey::MarginLeft),
        ];
        assert_eq!(want.map(|v| Some(v.to_string())).to_vec(), got.to_vec(), "margin: {value}");
    }
}

/// An inline shorthand covers every longhand a stylesheet could set, and an
/// inline longhand covers only itself - the other three still come from the
/// stylesheet, which is what CSS does per property.
#[test]
fn an_inline_longhand_leaves_the_other_edges_to_the_stylesheet() {
    let d = styled(
        "span { padding: 1px }",
        json!([{"tag": "span", "style": {"paddingLeft": "9px"}, "content": "x"}]),
    );
    let span = find(&d, Tag::Span);
    assert_eq!(Some("9px".to_string()), prop(&d, span, StyleKey::PaddingLeft));
    assert_eq!(Some("1px".to_string()), prop(&d, span, StyleKey::PaddingTop));

    let shorthand = styled(
        "span { padding-left: 1px }",
        json!([{"tag": "span", "style": {"padding": "9px"}, "content": "x"}]),
    );
    let span = find(&shorthand, Tag::Span);
    assert_eq!(
        vec![(StyleKey::Padding, "9px".to_string())],
        record(&shorthand, span),
        "an inline shorthand leaves the stylesheet's longhand nothing to set",
    );
}

// ---- what is dropped, and counted ----

/// The whole rule goes, declarations included, when any of its selectors
/// leaves the grammar - and the count is what `tools/dict-census` reports as
/// its progress gauge. Every selector here is a real corpus shape.
#[test]
fn an_unsupported_selector_drops_its_rule_whole_and_is_counted() {
    let cases: [(&str, &str); 10] = [
        (".gloss-image-container { border: 1px }", "a class"),
        ("#id { color: red }", "an id"),
        ("li + li { margin-top: 1px }", "an adjacent sibling combinator"),
        ("a ~ b { margin-top: 1px }", "a general sibling combinator"),
        ("span::before { content: \"x\" }", "a pseudo-element"),
        ("span:before { content: \"x\" }", "a legacy pseudo-element"),
        ("summary:hover { color: red }", "pointer state"),
        ("a:link { color: red }", "history state"),
        ("body { margin: 0 }", "a tag the schema has no node for"),
        ("span[title=\"x\"] { color: red }", "a non-data attribute"),
    ];
    for (css, why) in cases {
        let sheet = Sheet::compile(css);
        let counts = sheet.counts();
        assert!(sheet.is_empty(), "{why}: {css}");
        assert_eq!(1, counts.rules, "{why}");
        assert_eq!(1, counts.dropped_selector, "{why}");
        assert_eq!(1, counts.dropped(), "{why}");
    }
}

/// A selector list is atomic: one unreadable member drops the readable ones
/// with it, which is what CSS does with an unreadable selector list and the
/// only answer that never half-applies a rule. Jitendex loses a real
/// `margin-top` this way, and the ticket's measured cost is that count.
#[test]
fn one_bad_selector_in_a_list_drops_the_whole_rule() {
    let sheet = Sheet::compile(
        "li[data-sc-content=\"sense-group\"] + li[data-sc-content=\"sense-group\"],
         div[data-sc-content=\"forms\"] { margin-top: 0.5em }",
    );
    assert!(sheet.is_empty());
    assert_eq!(1, sheet.counts().dropped_selector);
}

/// An at-rule body is out of scope by measurement: only 4 of the corpus's
/// 908 box rules sit behind one, always `@media (max-width: 500px)`. The
/// rules inside are dropped and counted separately, so the two gaps stay
/// distinguishable.
#[test]
fn an_at_rule_body_drops_its_rules_and_counts_them_apart() {
    let sheet = Sheet::compile(
        "span { color: red }
         @media (max-width: 500px) { span { color: blue } div { margin: 0 } }",
    );
    let counts = sheet.counts();
    assert_eq!(3, counts.rules);
    assert_eq!(1, counts.kept);
    assert_eq!(2, counts.dropped_at_rule);
    assert_eq!(0, counts.dropped_selector);
    assert_eq!(2, counts.dropped());
}

/// A `var()` value drops the declaration, not the rule: Jitendex's
/// `extra-box` carries a `var()` border width beside three lengths that are
/// perfectly readable, and losing the rule would lose those too.
#[test]
fn a_var_value_drops_its_declaration_and_leaves_the_rule() {
    let d = styled(
        "div[data-sc-class=\"extra-box\"] {
             border-radius: 0.4rem;
             border-width: calc(3em / var(--font-size-no-units, 14));
             padding: 0.5rem;
         }",
        json!([{"tag": "div", "data": {"class": "extra-box"}, "content": "x"}]),
    );
    let div = find(&d, Tag::Div);
    assert_eq!(Some("0.4rem".to_string()), prop(&d, div, StyleKey::BorderRadius));
    assert_eq!(Some("0.5rem".to_string()), prop(&d, div, StyleKey::PaddingTop));
    assert_eq!(None, prop(&d, div, StyleKey::BorderWidth));
}

/// A rule whose selector compiles but whose every declaration is unmapped is
/// neither kept nor dropped-for-selector: it is its own count, because the
/// two mean different things to the census - one is a grammar gap and the
/// other a property gap.
#[test]
fn a_rule_of_unmapped_properties_only_is_counted_apart() {
    let sheet = Sheet::compile("span { display: grid; grid-column: 1; line-height: 1.4 }");
    let counts = sheet.counts();
    assert_eq!(1, counts.rules);
    assert_eq!(1, counts.no_decls);
    assert_eq!(0, counts.dropped());
    assert_eq!(3, counts.dropped_declarations);
}

/// The invariant the census's arithmetic rests on.
#[test]
fn every_scanned_rule_lands_in_exactly_one_count() {
    let sheet = Sheet::compile(
        "span { color: red }
         .chrome { border: 1px }
         div { display: grid }
         @media print { td { margin: 0 } }
         li:first-child { margin-top: 1px }",
    );
    let c = sheet.counts();
    assert_eq!(5, c.rules);
    assert_eq!(c.rules, c.kept + c.dropped_selector + c.dropped_at_rule + c.no_decls);
    assert_eq!(2, c.kept);
    assert_eq!(1, c.dropped_selector);
    assert_eq!(1, c.dropped_at_rule);
    assert_eq!(1, c.no_decls);
}

// ---- malformed input ----

/// Malformed CSS drops the rule it is in, records what went wrong, and never
/// panics. Every case here is one the ticket names.
#[test]
fn malformed_css_drops_rules_and_never_panics() {
    let cases: [(&str, &str); 8] = [
        ("span { color: red } /* unterminated", "unterminated comment"),
        ("span { content: \"unterminated\n} div { color: red }", "unterminated string"),
        ("} span { color: red }", "a stray closing brace"),
        (
            "span { background-color: #eee } div { background: url(a{b.png) }",
            "a brace inside url()",
        ),
        ("@media screen { @media print { span { color: red } } }", "nested at-rules"),
        ("span { color: red", "an unclosed block"),
        ("span color red }", "no colon and no brace"),
        ("", "nothing at all"),
    ];
    for (css, why) in cases {
        let sheet = Sheet::compile(css);
        // The point is that it returns: a stylesheet is third-party text and
        // a build must never fail over one.
        let mut d = doc(json!([{"tag": "span", "content": "x"}]));
        apply(&mut d, &sheet);
        assert_eq!(vec!["x".to_string()], plain_items(&d), "{why}");
    }
}

/// A brace inside `url(...)` must not end the block, or every rule after it
/// is lost - which is why the scanner counts parentheses rather than braces
/// alone.
#[test]
fn a_brace_inside_a_url_does_not_end_the_block() {
    let sheet = Sheet::compile(
        "div { background: url(a{b.png); background-color: #eee }
         span { color: red }",
    );
    assert_eq!(2, sheet.counts().rules);
    assert_eq!(2, sheet.counts().kept);
}

/// Nesting deeper than the scanner's cap stops compiling rules and never
/// recurses without bound. The corpus nests three levels.
#[test]
fn nesting_past_the_cap_is_refused_rather_than_followed() {
    let deep = format!("{}span {{ color: red }}{}", "div { ".repeat(64), "}".repeat(64));
    let sheet = Sheet::compile(&deep);
    assert!(sheet.counts().error.is_some());
}

// ---- the fold itself ----

/// A document under a sheet that matches nothing keeps its record byte for
/// byte, so a dictionary with a stylesheet full of chrome selectors pays
/// nothing.
#[test]
fn a_sheet_that_matches_nothing_leaves_the_record_alone() {
    let content = json!([{"tag": "span", "style": {"fontSize": "0.8em"}, "content": "x"}]);
    let plain = doc(content.clone());
    let under = styled("div[data-sc-nothing] { color: red }", content);
    assert_eq!(plain, under);
}

/// Text and line-break nodes carry no tag and no `data`, so no selector in
/// this grammar can name one - and the walk skips them rather than probing
/// the most numerous nodes in a tree.
#[test]
fn a_text_node_is_never_probed() {
    let d = styled(
        "span { color: red }",
        json!([{"tag": "span", "content": ["a", {"tag": "br"}, "b"]}]),
    );
    let styled_nodes: Vec<Tag> = (0..d.all_nodes().len() as NodeId)
        .filter(|id| !d.style(*id).is_empty())
        .map(|id| d.node(id).tag)
        .collect();
    assert_eq!(vec![Tag::Span], styled_nodes);
}

/// The probe is over the whole tree, so a rule reaches a node at any depth -
/// and the depth cap is the parser's own, which the walk repeats rather than
/// assumes.
#[test]
fn a_rule_reaches_a_deeply_nested_node() {
    let mut content = json!({"tag": "span", "data": {"content": "leaf"}, "content": "x"});
    for _ in 0..20 {
        content = json!({"tag": "div", "content": [content]});
    }
    let d = styled("[data-sc-content=\"leaf\"] { color: red }", json!([content]));
    assert_eq!(Some("red".to_string()), prop(&d, find(&d, Tag::Span), StyleKey::Color));
}

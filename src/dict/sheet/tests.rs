//! Test the matcher against CSS shapes that the corpus ships.
//!
//! Use tables when each input maps to one output. A failure then identifies one
//! wrong row instead of a wrong rule. Each CSS fixture comes from a real stylesheet
//! or a real selector shape. State its source where needed.

use super::*;
use crate::dict::gloss::{plain_items, GlossDoc, NodeId, StyleKey, Tag};
use serde_json::json;

/// Parse one structured-content glossary.
fn doc(content: serde_json::Value) -> GlossDoc {
    GlossDoc::parse(&json!([{"type": "structured-content", "content": content}]).to_string())
}

/// Return the first node with this tag in parse order.
fn find(d: &GlossDoc, tag: Tag) -> NodeId {
    (0..d.all_nodes().len() as NodeId)
        .find(|id| d.node(*id).tag == tag)
        .unwrap_or_else(|| panic!("no {tag:?} node"))
}

/// Return one node's resolved record as `(property, value)` text.
fn record(d: &GlossDoc, id: NodeId) -> Vec<(StyleKey, String)> {
    d.style(id)
        .iter()
        .map(|(k, v)| (*k, d.scalar_str(*v).unwrap_or("<non-text>").to_string()))
        .collect()
}

/// Return the first value for a property in a node record.
/// Every reader of a record uses its first occurrence.
fn prop(d: &GlossDoc, id: NodeId, key: StyleKey) -> Option<String> {
    d.style_of(id, key).and_then(|v| d.scalar_str(v)).map(str::to_string)
}

/// Parse CSS, compile it, and fold it into the document.
fn styled(css: &str, content: serde_json::Value) -> GlossDoc {
    let sheet = Sheet::compile(css);
    let mut d = doc(content);
    apply(&mut d, &sheet);
    d
}

// ---- the data-key to attribute-name transform ----

/// Use this exact transform for every data selector in a Dictionary stylesheet.
/// The transform has several steps. The DOM `dataset` capitalizes the key's first
/// letter because Yomitan's `sc` prefix places it in the middle. It then turns
/// each ASCII capital into `-x` and leaves a CJK key unchanged. The census script
/// `tools/dict-census/census.py` verified this transform against the corpus. Test
/// awkward cases and the four most-selected real keys.
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
        // A digit is not a capital, so it does not get its own hyphen.
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

/// Test the kebab-case half of the tree parser's camelCase table, and no other properties.
/// The `unmapped` rows exercise properties that `css_key` rejects.
/// Drop them. A table that grows beyond the renderer would add values that no reader
/// can use.
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

/// Keep the two allow-lists that `tools/dict-census/census.py` reads from
/// `sheet/mod.rs` aligned with parser behavior. If the lists and parser differ, the
/// dropped-rule count has no meaning. Test both directions: every listed
/// pseudo-class compiles, and an unlisted pseudo-class drops its rule.
#[test]
fn the_declared_pseudo_class_list_is_the_one_the_parser_accepts() {
    for name in SUPPORTED_PSEUDO_CLASSES {
        // `nth-*` needs an argument, and list forms need a selector. Give each
        // pseudo-class the shape that it accepts.
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

/// Test each selector kind in the census vocabulary. Compile one listed kind and
/// reject one unlisted kind.
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

/// 字通 uses a `data-sc` key on a span and a full box. It has no inline `style` in
/// the entry. This test proves that the mechanism works. Before this change, the box
/// model drew nothing for 13 `css-only` Dictionaries in the census.
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
    // The tree that the renderer reads stays otherwise unchanged.
    assert_eq!(vec!["ゴ".to_string()], plain_items(&d));
}

/// Test 明鏡国語辞典's selector shape, `div:has(> span[data-sc-zoomkanji])`.
/// The corpus has one relative `:has` selector.
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

    // A grandchild is not a child, and `:has(> x)` requires a child.
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

/// Test 旺文社漢字典's `td:has([data-sc親字])` selector. It uses the descendant
/// form with a real CJK `data` key.
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

/// Inline `style` has final priority, regardless of the stylesheet. Test it against
/// the highest specificity that this grammar can express and against `!important`.
/// `!important` outranks every other stylesheet declaration but still loses to inline
/// `style`.
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
    // The overridden declarations are absent, not merely outranked. One record entry per
    // property prevents record order from deciding the result.
    assert_eq!(1, record(&d, span).len(), "{:?}", record(&d, span));
}

/// This build accepts no id or class selector. Its specificity is `(b, c)`.
/// `b` counts attribute tests and pseudo-classes. `c` counts tag names.
/// Compare the pair as one packed number. Use source order to break ties, and let
/// the later rule win. `:where` adds nothing. `:is`, `:not`, and `:has` provide their
/// most specific argument.
#[test]
fn a_more_specific_rule_beats_a_less_specific_one() {
    let cases: [(&str, &str, &str); 5] = [
        // A (0,1) tag loses to a (1,0) attribute.
        ("span { color: red } [data-sc-content=\"a\"] { color: blue }", "blue", "attr > tag"),
        // A (1,1) beats a (1,0), regardless of source order.
        (
            "span[data-sc-content=\"a\"] { color: blue } [data-sc-content=\"a\"] { color: red }",
            "blue",
            "tag+attr > attr, against source order",
        ),
        // A (2,1) beats a (1,1).
        (
            "span[data-sc-content=\"a\"] { color: red }
             div span[data-sc-content=\"a\"] { color: red }
             span[data-sc-content=\"a\"][data-sc-class=\"b\"] { color: blue }",
            "blue",
            "two attributes beat one attribute and two tags",
        ),
        // Equal specificity: the later rule wins.
        ("span { color: red } span { color: blue }", "blue", "source order breaks a tie"),
        // `:where` adds nothing, so (0,1) loses to (1,0).
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

/// Use `!important` to order declarations inside one stylesheet. The corpus has 96
/// occurrences. `!important` is not a second origin. The previous test shows that
/// inline `style` still wins.
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

/// A descendant selector constrains the ancestor chain. A node matches only when the
/// chain satisfies the selector. This differs from a census key-presence count.
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

/// A child combinator does not match a descendant.
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

/// Test Jitendex's `li[data-sc-content="sense-group"]:first-child` selector and
/// a sibling that is not first.
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

/// `nth-of-type` counts only siblings with the same tag. This differs from
/// `nth-child` on a mixed list.
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

/// Jitendex uses native `&` nesting for marker suppression. A scanner that only
/// flattens blocks would lose this rule. This test uses the rule verbatim.
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

/// A nested block without `&` uses an implicit descendant combinator. CSS defines
/// this behavior, and Jitendex uses it for forms rules.
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

/// Expand `margin` and `padding` shorthands into four longhands at compile time.
/// The cascade then runs per property. Without expansion, a less-specific shorthand
/// applied after a more-specific longhand would erase it because record order controls
/// application.
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

/// Test edge shorthands with one, two, three, and four values.
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

/// An inline shorthand covers every longhand that a stylesheet can set. An inline
/// longhand covers only its own property. The other three still come from the
/// stylesheet. CSS applies these declarations per property.
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

/// Drop the whole rule and its declarations when any selector leaves the grammar.
/// `tools/dict-census` reports this count as progress. These cases cover unsupported
/// selector shapes. Some cases are synthetic and do not occur in the corpus.
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

/// Treat a selector list as one unit. If one member cannot be read, drop every
/// member. CSS uses this behavior for an unreadable list. It prevents a partial
/// rule. Jitendex loses a real `margin-top` this way, and the census records that
/// cost.
#[test]
fn one_bad_selector_in_a_list_drops_the_whole_rule() {
    let sheet = Sheet::compile(
        "li[data-sc-content=\"sense-group\"] + li[data-sc-content=\"sense-group\"],
         div[data-sc-content=\"forms\"] { margin-top: 0.5em }",
    );
    assert!(sheet.is_empty());
    assert_eq!(1, sheet.counts().dropped_selector);
}

/// Keep at-rule bodies outside this scope. Corpus measurements show 4 of its 908 box
/// rules behind one at-rule. Each uses `@media (max-width: 500px)`. Drop and count
/// rules inside separately. Keep selector and at-rule gaps distinct.
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

/// Drop a `var()` declaration but keep its rule. Jitendex's `extra-box` has a
/// `var()` border width beside three readable lengths. The rule must stay, or it
/// would lose those lengths.
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

/// Count a rule with a valid selector and no mapped declarations separately.
/// A grammar gap and a property gap mean different things to the census.
#[test]
fn a_rule_of_unmapped_properties_only_is_counted_apart() {
    let sheet = Sheet::compile("span { display: grid; grid-column: 1; line-height: 1.4 }");
    let counts = sheet.counts();
    assert_eq!(1, counts.rules);
    assert_eq!(1, counts.no_decls);
    assert_eq!(0, counts.dropped());
    assert_eq!(3, counts.dropped_declarations);
}

/// Check the invariant that supports the census arithmetic.
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

/// Drop a malformed CSS rule, record its error, and never panic.
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
        // These inputs return a sheet instead of an error. CSS is third-party text,
        // so a build must not fail because one stylesheet is malformed.
        let mut d = doc(json!([{"tag": "span", "content": "x"}]));
        apply(&mut d, &sheet);
        assert_eq!(vec!["x".to_string()], plain_items(&d), "{why}");
    }
}

/// A brace inside `url(...)` must not close the block. Count parentheses as well as
/// braces, or the scanner loses every later rule.
#[test]
fn a_brace_inside_a_url_does_not_end_the_block() {
    let sheet = Sheet::compile(
        "div { background: url(a{b.png); background-color: #eee }
         span { color: red }",
    );
    assert_eq!(2, sheet.counts().rules);
    assert_eq!(2, sheet.counts().kept);
}

/// Refuse rules deeper than the scanner's cap. Never recurse without a bound. The
/// corpus nests three levels.
#[test]
fn nesting_past_the_cap_is_refused_rather_than_followed() {
    let deep = format!("{}span {{ color: red }}{}", "div { ".repeat(64), "}".repeat(64));
    let sheet = Sheet::compile(&deep);
    assert!(sheet.counts().error.is_some());
}

// ---- the fold itself ----

/// Keep the document record byte-for-byte when the sheet matches no node. A Dictionary
/// with a stylesheet full of chrome selectors then pays no fold cost.
#[test]
fn a_sheet_that_matches_nothing_leaves_the_record_alone() {
    let content = json!([{"tag": "span", "style": {"fontSize": "0.8em"}, "content": "x"}]);
    let plain = doc(content.clone());
    let under = styled("div[data-sc-nothing] { color: red }", content);
    assert_eq!(plain, under);
}

/// Text and line-break nodes have no tag or `data`, so no selector can name them.
/// Skip them instead of probing the most numerous nodes in a tree.
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

/// Probe the whole tree, so a rule reaches a node at any depth. The parser defines
/// the depth cap. The walk applies the same cap instead of assuming a smaller one.
#[test]
fn a_rule_reaches_a_deeply_nested_node() {
    let mut content = json!({"tag": "span", "data": {"content": "leaf"}, "content": "x"});
    for _ in 0..20 {
        content = json!({"tag": "div", "content": [content]});
    }
    let d = styled("[data-sc-content=\"leaf\"] { color: red }", json!([content]));
    assert_eq!(Some("red".to_string()), prop(&d, find(&d, Tag::Span), StyleKey::Color));
}

//! Test the GlossDoc parser with one fixture for each real Dictionary shape.
//! Also test the losslessness, robustness, and size limits that the module defines.
//!
//! The fixtures come from the census of 97 archives in `docs/research/dict-shapes.md`.
//! They do not come from assumptions. A fix that passes Jitendex does not prove
//! that 大辞林 works. Use each representative node exactly when the census prints
//! it. Build each other fixture from the hooks and counts that the census names.
//! State which source each fixture uses.

use super::*;
use serde_json::{json, Value};

/// Parse a glossary the way a hover parses it.
///
/// The stored format is minified glossary JSON from the Dictionary. This helper
/// serializes a `Value` to JSON before it parses the GlossDoc. Tests that call
/// this helper cover the stored-format conversion. Tests that call `GlossDoc::parse`
/// directly cover malformed or deeply nested input instead.
pub(super) fn doc(glossary: &Value) -> GlossDoc {
    GlossDoc::parse(&glossary.to_string())
}

/// Return the first node with this tag in parse order.
fn find_tag(d: &GlossDoc, tag: Tag) -> Option<NodeId> {
    (0..d.all_nodes().len() as NodeId).find(|&i| d.node(i).tag == tag)
}

// ---------------------------------------------------------------------------
// losslessness over the census tables
// ---------------------------------------------------------------------------

/// List every tag that `docs/research/dict-shapes.md` found in the corpus and its
/// `Tag` variant.
/// `tbody` occurs in one Dictionary. `div`, `a`, and `span` occur in 43 to 45.
const CENSUS_TAGS: [(&str, Tag); 17] = [
    ("div", Tag::Div),
    ("a", Tag::A),
    ("span", Tag::Span),
    ("img", Tag::Img),
    ("li", Tag::Li),
    ("ul", Tag::Ul),
    ("tr", Tag::Tr),
    ("td", Tag::Td),
    ("table", Tag::Table),
    ("ruby", Tag::Ruby),
    ("rt", Tag::Rt),
    ("th", Tag::Th),
    ("br", Tag::Br),
    ("ol", Tag::Ol),
    ("details", Tag::Details),
    ("summary", Tag::Summary),
    ("tbody", Tag::Tbody),
];

/// List every style property that the census found with the value type that the
/// corpus uses. Use numbers for em multipliers. Use strings for keywords and colors.
fn census_styles() -> Vec<(&'static str, StyleKey, Scalar)> {
    vec![
        ("fontWeight", StyleKey::FontWeight, Scalar::Text(Span::default())),
        ("fontSize", StyleKey::FontSize, Scalar::Num(0.8)),
        ("verticalAlign", StyleKey::VerticalAlign, Scalar::Text(Span::default())),
        ("listStyleType", StyleKey::ListStyleType, Scalar::Text(Span::default())),
        ("marginRight", StyleKey::MarginRight, Scalar::Num(0.25)),
        ("marginBottom", StyleKey::MarginBottom, Scalar::Num(0.5)),
        ("borderStyle", StyleKey::BorderStyle, Scalar::Text(Span::default())),
        ("borderRadius", StyleKey::BorderRadius, Scalar::Num(0.3)),
        ("borderWidth", StyleKey::BorderWidth, Scalar::Num(0.0714)),
        ("padding", StyleKey::Padding, Scalar::Num(0.25)),
        ("cursor", StyleKey::Cursor, Scalar::Text(Span::default())),
        ("marginLeft", StyleKey::MarginLeft, Scalar::Num(1.4)),
        ("color", StyleKey::Color, Scalar::Text(Span::default())),
        ("textAlign", StyleKey::TextAlign, Scalar::Text(Span::default())),
        ("fontStyle", StyleKey::FontStyle, Scalar::Text(Span::default())),
        ("whiteSpace", StyleKey::WhiteSpace, Scalar::Text(Span::default())),
        ("backgroundColor", StyleKey::BackgroundColor, Scalar::Text(Span::default())),
        ("marginTop", StyleKey::MarginTop, Scalar::Num(0.2)),
        ("borderColor", StyleKey::BorderColor, Scalar::Text(Span::default())),
        ("paddingLeft", StyleKey::PaddingLeft, Scalar::Num(1.4)),
        ("textDecorationLine", StyleKey::TextDecorationLine, Scalar::Text(Span::default())),
        ("margin", StyleKey::Margin, Scalar::Num(0.1)),
    ]
}

/// Return the fixture value for one census style property.
fn style_json(want: Scalar, name: &str) -> Value {
    match want {
        Scalar::Num(n) => json!(n),
        _ => json!(format!("v-{name}")),
    }
}

#[test]
fn every_census_tag_survives_the_stored_format() {
    let content: Vec<Value> =
        CENSUS_TAGS.iter().map(|(name, _)| json!({"tag": name, "content": "x"})).collect();
    let d = doc(&json!([{"type": "structured-content", "content": content}]));

    for (name, want) in CENSUS_TAGS {
        let id = find_tag(&d, want)
            .unwrap_or_else(|| panic!("tag {name} did not parse to {want:?}"));
        assert_eq!(name, d.tag_name(id), "tag {name} lost its name");
    }
}

#[test]
fn every_census_style_property_survives_the_stored_format() {
    let wanted = census_styles();
    let style: serde_json::Map<String, Value> = wanted
        .iter()
        .map(|(name, _, want)| ((*name).to_string(), style_json(*want, name)))
        .collect();
    let d = doc(&json!([{"type": "structured-content", "content": {
        "tag": "span", "style": style, "content": "x"
    }}]));

    let id = find_tag(&d, Tag::Span).expect("the styled span parsed");
    assert_eq!(
        wanted.len(),
        d.style(id).len(),
        "the resolved style record dropped a census property"
    );
    for (name, key, want) in &wanted {
        let got = d.style_of(id, *key).unwrap_or_else(|| panic!("{name} is missing"));
        match want {
            Scalar::Num(n) => assert_eq!(Scalar::Num(*n), got, "{name} lost its number"),
            _ => assert_eq!(
                Some(format!("v-{name}").as_str()),
                d.scalar_str(got),
                "{name} lost its value"
            ),
        }
    }
}

/// Drop an unknown property on purpose because the record has a closed set.
/// Keep nearby properties.
#[test]
fn an_unknown_style_property_drops_without_disturbing_the_rest() {
    let d = doc(&json!([{"type": "structured-content", "content": {
        "tag": "span", "style": {"fontWeight": "bold", "clipPath": "circle(1px)"},
        "content": "x"
    }}]));
    let id = find_tag(&d, Tag::Span).unwrap();
    assert_eq!(1, d.style(id).len());
    assert_eq!(Some("bold"), d.style_of(id, StyleKey::FontWeight).and_then(|v| d.scalar_str(v)));
}

/// Preserve every image field that the census counted. The Media store reads these
/// fields from each node. A rebuild is the only way to restore them.
#[test]
fn an_image_node_keeps_every_field_the_census_counted() {
    // Taken verbatim from `docs/research/dict-shapes.md` under "Representative
    // nodes" for 三省堂国語辞典 第八版. It is a monochrome SVG with a declared em size.
    let d = doc(&json!([{"type": "structured-content", "content": {
        "tag": "img", "path": "sankoku8/svg-intonation/短.svg", "height": 1.0,
        "width": 1.5384615384615385, "sizeUnits": "em", "appearance": "monochrome",
        "background": false, "title": "短", "collapsed": false, "collapsible": false
    }}]));

    let id = find_tag(&d, Tag::Img).expect("the image parsed");
    assert_eq!(Kind::Image, d.node(id).kind);
    assert_eq!(
        Some("sankoku8/svg-intonation/短.svg"),
        d.attr_of(id, "path").and_then(|v| d.scalar_str(v))
    );
    assert_eq!(Some(Scalar::Num(1.0)), d.attr_of(id, "height"));
    // `serde_json` can round a 17-digit mantissa one ULP away from its literal value.
    // This parser behavior predates this test. The difference is invisible because
    // the number is an em multiplier. An exact comparison would pin an implementation
    // detail of `serde_json`.
    let width = match d.attr_of(id, "width") {
        Some(Scalar::Num(n)) => n,
        other => panic!("width did not parse as a number: {other:?}"),
    };
    assert!((width - 1.5384615384615385).abs() < 1e-12, "width came back as {width}");
    assert_eq!(Some("em"), d.attr_of(id, "sizeUnits").and_then(|v| d.scalar_str(v)));
    assert_eq!(Some("monochrome"), d.attr_of(id, "appearance").and_then(|v| d.scalar_str(v)));
    assert_eq!(Some(Scalar::Bool(false)), d.attr_of(id, "background"));
    assert_eq!(Some(Scalar::Bool(false)), d.attr_of(id, "collapsed"));
    assert_eq!(Some(Scalar::Bool(false)), d.attr_of(id, "collapsible"));
    assert_eq!(Some("短"), d.attr_of(id, "title").and_then(|v| d.scalar_str(v)));
}

// ---------------------------------------------------------------------------
// one fixture per real dictionary shape
// ---------------------------------------------------------------------------

/// Jitendex. The fixture exercises the census hooks `content=part-of-speech-info`,
/// `content=sense`, `content=glossary`, `content=example-sentence*`, and
/// `content=attribution`.
#[test]
fn jitendex_parses_to_pills_a_sense_list_and_dropped_editorial_matter() {
    let g = json!([{"type": "structured-content", "content": [
        {"tag": "span", "data": {"class": "tag", "code": "n", "content": "part-of-speech-info"},
         "content": "noun"},
        {"tag": "span", "data": {"class": "tag", "code": "vs", "content": "part-of-speech-info"},
         "content": "suru verb"},
        {"tag": "div", "data": {"content": "sense"}, "content": {
            "tag": "ul", "data": {"content": "glossary"}, "style": {"listStyleType": "\"◍\""},
            "content": [
                {"tag": "li", "content": "chatting"},
                {"tag": "li", "content": "idle talk"}
            ]}},
        {"tag": "div", "data": {"content": "example-sentence"}, "content": [
            {"tag": "div", "data": {"content": "example-sentence-a"},
             "content": "もっと果物を食べるべきです。"},
            {"tag": "div", "data": {"content": "example-sentence-b"},
             "content": "You should eat more fruit."}
        ]},
        {"tag": "div", "data": {"content": "attribution"}, "content": [
            {"tag": "a", "href": "https://jitendex.org", "content": "Jitendex"}
        ]}
    ]}]);
    let d = doc(&g);

    assert_eq!(vec!["noun".to_string(), "suru verb".to_string()], pos_labels(&d));
    assert_eq!(vec!["chatting\nidle talk".to_string()], plain_items(&d));

    // The list marker that Jitendex declares inline stays on the node. The PopupScene
    // can draw it, and the role classifier can see the roles beside it.
    let list = find_tag(&d, Tag::Ul).unwrap();
    assert_eq!(
        Some("\"◍\""),
        d.style_of(list, StyleKey::ListStyleType).and_then(|v| d.scalar_str(v))
    );
    let attribution = d.items().next().map(|item| d.children(item).nth(4).unwrap()).unwrap();
    assert_eq!(Role::Attribution, d.role(attribution));
    // The summary drops this node, but the tree keeps it. The popup needs this role
    // for the "show attributions" control to restore it.
    assert_eq!(Tag::A, d.node(d.children(attribution).next().unwrap()).tag);
}

/// 大辞林 第四版. The fixture exercises `name=用例` and `name=慣用例`.
/// It also records its own `①②③` markers inside the tree. Therefore, no code
/// creates an outer sense number.
#[test]
fn daijirin_keeps_its_own_sense_markers_and_its_example_hooks() {
    let g = json!([{"type": "structured-content", "content": {
        "tag": "div", "content": [
            {"tag": "div", "content": [
                {"tag": "span", "data": {"name": "語義番号"}, "content": "①"},
                {"tag": "span", "content": "走る。駆ける。"},
                {"tag": "div", "data": {"name": "用例"}, "content": "馬が―"}
            ]},
            {"tag": "div", "content": [
                {"tag": "span", "data": {"name": "語義番号"}, "content": "②"},
                {"tag": "span", "content": "流れる。"},
                {"tag": "div", "data": {"name": "慣用例"}, "content": "水が―"}
            ]}
        ]}}]);
    let d = doc(&g);

    // Both example hooks classify. 大辞林 now matches Jitendex: two senses form
    // peer blocks. Each block uses its own line, and the Dictionary's marker stays.
    // No code creates a number. Both `用例` blocks leave the summary. The old drop
    // list named neither `用例` nor `慣用例`, so both blocks once rendered.
    assert_eq!(vec!["①走る。駆ける。\n②流れる。".to_string()], plain_items(&d));

    let roles: Vec<(&str, Role)> = (0..d.all_nodes().len() as NodeId)
        .filter_map(|i| d.data_of(i, "name").and_then(|v| d.scalar_str(v)).map(|n| (n, d.role(i))))
        .collect();
    assert_eq!(
        vec![
            ("語義番号", Role::Content),
            ("用例", Role::Example),
            ("語義番号", Role::Content),
            ("慣用例", Role::Example),
        ],
        roles,
        "a sense number is content; a substring needle covers both example spellings"
    );
}

/// 明鏡国語辞典 第三版. Its 38 892 example sentences use an `example=` key that
/// no fixed drop list names. This case tests the main defect that the role
/// classifier fixes: Jitendex lost its examples, but this Dictionary kept all of them.
#[test]
fn meikyo_examples_classify_under_a_key_no_drop_list_ever_named() {
    let g = json!([{"type": "structured-content", "content": {
        "tag": "div", "content": [
            {"tag": "span", "content": "非常に。たいそう。"},
            {"tag": "div", "data": {"example": ""}, "content": "―うれしい"}
        ]}}]);
    let d = doc(&g);

    let example = (0..d.all_nodes().len() as NodeId)
        .find(|&i| d.data_of(i, "example").is_some())
        .expect("the example hook survived the parse");
    assert_eq!(Role::Example, d.role(example));
    assert_eq!(vec!["非常に。たいそう。".to_string()], plain_items(&d));
    assert!(
        render_html(&d, Selection::Whole, RoleFilter::CARD)[0].contains("―うれしい"),
        "and the card keeps it"
    );
}

/// 字通 has 139 138 image nodes and 278 340 gaiji markers. It averages more than
/// four image nodes per term row. The image node is verbatim from the census
/// `Representative nodes` entry.
#[test]
fn jitsuu_gaiji_images_keep_their_data_markers() {
    let g = json!([{"type": "structured-content", "content": [
        {"tag": "img", "path": "gaiji/対義語.svg", "background": false, "collapsed": false,
         "collapsible": false,
         "data": {"gaiji": "", "class": "gaiji", "alt": "［対義語］", "src": "gaiji/対義語.svg"}},
        {"tag": "span", "content": "むかう"}
    ]}]);
    let d = doc(&g);

    let img = find_tag(&d, Tag::Img).expect("the gaiji image parsed");
    assert_eq!(Kind::Image, d.node(img).kind);
    assert_eq!(Some("［対義語］"), d.data_of(img, "alt").and_then(|v| d.scalar_str(v)));
    assert!(d.data_of(img, "gaiji").is_some(), "the gaiji marker is what makes it a character");
    // The plain-text renderer cannot draw a character as an image, so it drops the
    // image. The tree keeps the image for a renderer that can draw it.
    assert_eq!(vec!["むかう".to_string()], plain_items(&d));
}

/// 三省堂国語辞典 第八版. A monochrome SVG uses a declared em size. It appears
/// inline in a definition beside its `name=用例G` hook.
#[test]
fn sanseido_inline_monochrome_svg_sits_inside_its_sense() {
    let g = json!([{"type": "structured-content", "content": {
        "tag": "div", "content": [
            {"tag": "span", "content": "みじかい"},
            {"tag": "img", "path": "sankoku8/svg-intonation/短.svg", "height": 1.0,
             "width": 1.5384615384615385, "sizeUnits": "em", "appearance": "monochrome",
             "background": false, "title": "短", "collapsed": false, "collapsible": false},
            {"tag": "div", "data": {"name": "用例G"}, "content": "―スカート"}
        ]}}]);
    let d = doc(&g);

    let img = find_tag(&d, Tag::Img).unwrap();
    assert_eq!(Some("monochrome"), d.attr_of(img, "appearance").and_then(|v| d.scalar_str(v)));
    assert_eq!(Some(Scalar::Num(1.0)), d.attr_of(img, "height"));

    // 三省堂's own `用例G` hook classifies the node. The example leaves the summary,
    // as it does for Jitendex. No fixed drop list named this key.
    let example = (0..d.all_nodes().len() as NodeId)
        .find(|&i| d.data_of(i, "name").is_some())
        .expect("the example hook survived the parse");
    assert_eq!(Role::Example, d.role(example));
    assert_eq!(vec!["みじかい".to_string()], plain_items(&d));

    // The image shares one block with the text. It must not create a line break.
    // Check the card because it keeps the example. The full line is then observable.
    assert_eq!(
        vec!["<div><span>みじかい</span>短<div>―スカート</div></div>".to_string()],
        render_html(&d, Selection::Whole, RoleFilter::CARD)
    );
}

/// A conjugation table. The corpus has 18 Dictionaries with `tr` and `td`, 11
/// with `th`, and 7 with `rowSpan`. Treat cells as a grid. Only a row creates a
/// line break.
#[test]
fn a_conjugation_table_parses_to_rows_of_cells_with_their_spans() {
    let g = json!([{"type": "structured-content", "content": {
        "tag": "table", "content": {"tag": "tbody", "content": [
            {"tag": "tr", "content": [
                {"tag": "th", "rowSpan": 2, "content": "五段"},
                {"tag": "td", "content": "未然形"},
                {"tag": "td", "content": "書か"}
            ]},
            {"tag": "tr", "content": [
                {"tag": "td", "colSpan": 2, "content": "連用形 書き"}
            ]}
        ]}}}]);
    let d = doc(&g);

    let table = find_tag(&d, Tag::Table).unwrap();
    assert_eq!(Kind::Table, d.node(table).kind);
    let body = d.children(table).next().unwrap();
    assert_eq!(Kind::Table, d.node(body).kind, "tbody is table scaffolding, not a row");
    let rows: Vec<NodeId> = d.children(body).collect();
    assert_eq!(2, rows.len());
    assert!(rows.iter().all(|&r| d.node(r).kind == Kind::Row));

    let header = d.children(rows[0]).next().unwrap();
    assert_eq!(Kind::Cell, d.node(header).kind);
    assert_eq!(Some(Scalar::Num(2.0)), d.attr_of(header, "rowSpan"));
    let wide = d.children(rows[1]).next().unwrap();
    assert_eq!(Some(Scalar::Num(2.0)), d.attr_of(wide, "colSpan"));

    // Create one line per row. Concatenate cells within each row.
    assert_eq!(vec!["五段未然形書か\n連用形 書き".to_string()], plain_items(&d));
}

/// One of 20 plain-string Dictionaries, such as 精選版 日本国語大辞典 and 漢字源.
/// These Dictionaries reach render parity through line-break support. Test that
/// they produce the same shape as other Dictionaries.
#[test]
fn a_plain_string_dictionary_gives_one_text_node_per_item() {
    let d = doc(&json!(["to eat", "to consume"]));

    let items: Vec<NodeId> = d.items().collect();
    assert_eq!(2, items.len());
    for id in &items {
        assert_eq!(Kind::Text, d.node(*id).kind);
        assert!(d.is_plain_string(*id));
        assert!(!d.has_children(*id));
    }
    assert_eq!(vec!["to eat".to_string(), "to consume".to_string()], plain_items(&d));
}

/// A `type: text` item has one node, like a bare string. This lets every renderer
/// handle one shape instead of three.
#[test]
fn a_typed_text_item_is_the_same_single_node_shape() {
    let d = doc(&json!([{"type": "text", "text": "to eat"}]));

    let id = d.items().next().unwrap();
    assert_eq!(Kind::Text, d.node(id).kind);
    assert_eq!(ItemType::Text, d.node(id).item_type);
    assert!(!d.is_plain_string(id), "a typed item is not a bare glossary string");
    assert_eq!(vec!["to eat".to_string()], plain_items(&d));
}

/// Four Dictionaries use `details` with 30 908 nodes and `summary` with 31 169.
/// These tags sit outside the HTML allow-list, so renderers drop the wrapper.
/// GlossDoc keeps both tags so PopupScene can distinguish the summary.
#[test]
fn a_details_summary_dictionary_keeps_both_tags_and_separates_them() {
    let g = json!([{"type": "structured-content", "content": {
        "tag": "details", "content": [
            {"tag": "summary", "content": "活用"},
            {"tag": "div", "content": "たべる／たべ／たべれ"}
        ]}}]);
    let d = doc(&g);

    assert!(find_tag(&d, Tag::Details).is_some());
    assert!(find_tag(&d, Tag::Summary).is_some());
    assert_eq!(vec!["活用\nたべる／たべ／たべれ".to_string()], plain_items(&d));
}

// ---------------------------------------------------------------------------
// editorial role classification
// ---------------------------------------------------------------------------

/// Build one node with one `data` entry for classifier input.
fn roled(key: &str, value: &str) -> (GlossDoc, NodeId) {
    let d = doc(&json!([{"tag": "span", "data": {key: value}, "content": "x"}]));
    let id = d.items().next().expect("the node parsed");
    (d, id)
}

fn role_of_hook(key: &str, value: &str) -> Role {
    let (d, id) = roled(key, value);
    d.role(id)
}

/// List the bilingual role forms that this table tests from
/// `docs/research/dict-shapes.md`, with each form in census spelling. The
/// `用例注釈` form is tested separately below.
///
/// `(data key, data value, role)`. An empty value marks a key-side hook. The
/// Dictionary field name supplies the role, and `用例=` records the empty marker.
/// A non-empty value marks a value-side hook on one of three convention keys.
///
/// Japanese hooks dominate the census by more than one order of magnitude. Four
/// Dictionaries have 755 242 example nodes. ASCII forms have roughly 62 000.
/// An ASCII-only table would pass but omit twelve example nodes in thirteen
/// Dictionaries.
const CENSUS_ROLES: [(&str, &str, Role); 34] = [
    // -- example, Japanese keys: 755 242 nodes, 4 dictionaries.
    ("用例", "", Role::Example),
    ("用例G", "", Role::Example),
    ("用例訳", "", Role::Example),
    ("用例引用", "", Role::Example),
    ("用例活用", "", Role::Example),
    ("慣用例", "", Role::Example),
    ("囲み用例", "", Role::Example),
    ("例文", "", Role::Example),
    ("識別例文", "", Role::Example),
    // -- example, ASCII: ~62 000 nodes, 7 dictionaries.
    ("content", "example-sentence", Role::Example),
    ("content", "example-sentence-a", Role::Example),
    ("content", "example-sentence-b", Role::Example),
    ("content", "example-keyword", Role::Example),
    ("content", "examples", Role::Example),
    ("name", "example_1", Role::Example),
    ("name", "example_8", Role::Example),
    ("name", "examples", Role::Example),
    ("example", "", Role::Example),
    ("ExampleG", "", Role::Example),
    ("ExampleC", "", Role::Example),
    // -- attribution: 72 000 nodes, 4 dictionaries.
    ("content", "attribution", Role::Attribution),
    ("content", "attribution-footnote", Role::Attribution),
    ("出典", "", Role::Attribution),
    // -- reference: 116 000 nodes, 8 dictionaries. Classified, never hidden.
    ("参照", "", Role::Reference),
    ("類語", "", Role::Reference),
    ("対義", "", Role::Reference),
    ("content", "xref", Role::Reference),
    // -- commentary: 184 000 nodes, 8 dictionaries. Classified, never hidden.
    ("解説", "", Role::Commentary),
    ("補説", "", Role::Commentary),
    ("語源", "", Role::Commentary),
    // -- part of speech: Yomitan's own convention, 27 680 Jitendex nodes.
    ("content", "part-of-speech-info", Role::PartOfSpeech),
    // -- the structural hooks that dominate the corpus and are not roles.
    ("content", "sense", Role::Content),
    ("ruby", "", Role::Content),
    ("gaiji", "", Role::Content),
];

#[test]
fn every_row_of_the_census_role_table_classifies_to_its_named_role() {
    for (key, value, want) in CENSUS_ROLES {
        assert_eq!(want, role_of_hook(key, value), "data {key}={value}");
    }
}

/// Test the substring rule with the forms that motivated it. 旺文社 全訳古語辞典
/// writes eleven distinct `用例`-prefixed keys. 大辞林 writes two more. An equality
/// table could not name all of them.
#[test]
fn a_key_that_only_contains_a_needle_still_classifies() {
    for key in ["用例メタデータ", "用例囲みG", "用例訳注ロゴテキスト", "用例注釈", "注内用例G"] {
        assert_eq!(Role::Example, role_of_hook(key, ""), "{key}");
    }
    assert_eq!(Role::Reference, role_of_hook("参照語義番号", ""));
    assert_eq!(Role::Commentary, role_of_hook("語義パネル解説G", ""));
    assert_eq!(Role::Attribution, role_of_hook("出典G", ""));
}

/// Test normalization one row per fold. Each row uses a form that a converter
/// produces, not one invented only for this test.
#[test]
fn classification_normalises_case_width_and_the_ideographic_space() {
    // Case: 旺文社漢字典 writes `ExampleG`, while Jitendex writes
    // `example-sentence`.
    assert_eq!(Role::Example, role_of_hook("EXAMPLE", ""));
    assert_eq!(Role::Example, role_of_hook("ExAmPlE", ""));
    assert_eq!(Role::PartOfSpeech, role_of_hook("content", "Part-Of-Speech-Info"));
    // Width: An HTML- or EPUB-based Dictionary uses full-width Latin.
    assert_eq!(Role::Example, role_of_hook("Ｅｘａｍｐｌｅ", ""));
    assert_eq!(Role::Example, role_of_hook("name", "ｅｘａｍｐｌｅ＿１"));
    // A class list, as 旺文社 全訳古語辞典 writes it. A Japanese source can also use
    // the ideographic space.
    assert_eq!(Role::Example, role_of_hook("class", "fill 用例 FM"));
    assert_eq!(Role::Example, role_of_hook("class", "fill　用例　FM"));
    assert_eq!(Role::Example, role_of_hook("class", "用例活用 IM"));
}

/// The value side has three exact conventions, not a form family.
///
/// Nine census keys contain `content`, `name`, or `class` that are not convention
/// keys. A substring rule could read a role from their values. Every false match
/// would hide content.
#[test]
fn a_key_that_merely_contains_a_convention_name_is_not_a_convention_key() {
    for key in ["contents", "Contents", "jukugoitemcontent", "CampaignName", "filename"] {
        assert_eq!(
            Role::Content,
            role_of_hook(key, "example-sentence"),
            "{key} is content of its own, not a Yomitan convention"
        );
    }
}

/// A Dictionary with no known convention renders all content. The census has 78
/// such archives out of 97. This is the direction that the evidence requires.
#[test]
fn a_dictionary_using_no_known_convention_classifies_entirely_as_content() {
    let d = doc(&json!([{"type": "structured-content", "content": {
        "tag": "div", "data": {"xmlns": "http://www.w3.org/1999/xhtml", "body": ""},
        "content": [
            {"tag": "span", "data": {"headword": ""}, "content": "猫"},
            {"tag": "div", "data": {"meaning": "", "class": "full"}, "content": "cat"},
            {"tag": "ruby", "data": {"ruby": ""}, "content": [
                {"tag": "span", "content": "猫"},
                {"tag": "rt", "data": {"rt": ""}, "content": "ねこ"}
            ]}
        ]}}]));

    assert!(
        d.all_nodes().iter().all(|n| n.role == Role::Content),
        "roles: {:?}",
        d.all_nodes().iter().map(|n| n.role).collect::<Vec<_>>()
    );
    assert_eq!(vec!["猫\ncat猫(ねこ)".to_string()], plain_items(&d));
}

/// Two hooks on one node use the higher-priority role in either field order. The
/// classifier must not use JSON field order as policy.
#[test]
fn two_hooks_on_one_node_resolve_by_precedence_not_by_field_order() {
    let one = doc(&json!([{"tag": "span",
        "data": {"content": "part-of-speech-info", "用例": ""}, "content": "noun"}]));
    let other = doc(&json!([{"tag": "span",
        "data": {"用例": "", "content": "part-of-speech-info"}, "content": "noun"}]));

    let role = |d: &GlossDoc| d.role(d.items().next().unwrap());
    assert_eq!(Role::PartOfSpeech, role(&one));
    assert_eq!(Role::PartOfSpeech, role(&other), "field order is not precedence");
}

/// A convention key with a non-string value names no role. It still marks a block.
/// Keep these questions separate.
#[test]
fn a_convention_key_with_a_non_string_value_is_content_and_still_a_block() {
    let d = doc(&json!([{"tag": "span", "data": {"content": 3}, "content": "x"}]));
    let id = d.items().next().unwrap();
    assert_eq!(Role::Content, d.role(id));
    assert!(d.has_marker(id), "a non-string marker still opens a block");
}

/// Match the behavior that the deleted drop list gave one Dictionary.
///
/// The list held six names. The part-of-speech marker stood beside those names.
/// Each example or attribution name now maps to its role.
/// The summary drops those roles as before. The card keeps them. Other Dictionaries
/// use the same settings.
#[test]
fn the_deleted_drop_list_is_now_the_role_the_knobs_govern() {
    const WAS_DROPPED: [&str; 6] = [
        "attribution",
        "attribution-footnote",
        "example-keyword",
        "example-sentence",
        "example-sentence-a",
        "example-sentence-b",
    ];
    for name in WAS_DROPPED {
        let (d, id) = roled("content", name);
        let role = d.role(id);
        assert!(
            matches!(role, Role::Example | Role::Attribution),
            "content={name} classified {role:?}"
        );
        assert!(!RoleFilter::SUMMARY.allows(role), "a summary drops it, as the list did");
        assert!(RoleFilter::CARD.allows(role), "and the card keeps it, as the list never did");
    }
    assert!(!RoleFilter::CARD.allows(role_of_hook("content", "part-of-speech-info")));
}

/// Test one parse at the tree seam. The example leaves the summary and stays on the
/// card.
#[test]
fn an_example_leaves_the_summary_and_stays_on_the_card() {
    let d = doc(&json!([{"type": "structured-content", "content": [
        {"tag": "span", "content": "to eat"},
        {"tag": "div", "data": {"content": "example-sentence"}, "content": "ご飯を食べる"}
    ]}]));

    assert_eq!(vec!["to eat".to_string()], plain_items(&d));
    let card = render_html(&d, Selection::Whole, RoleFilter::CARD);
    assert!(card[0].contains("ご飯を食べる"), "the card keeps it: {card:?}");
}

/// Examples and attributions use two independent settings. Test all four combinations.
#[test]
fn examples_and_attributions_filter_independently_in_all_four_combinations() {
    let d = doc(&json!([{"type": "structured-content", "content": [
        {"tag": "span", "content": "to eat"},
        {"tag": "div", "data": {"用例": ""}, "content": "ご飯を食べる"},
        {"tag": "div", "data": {"出典": ""}, "content": "JMdict"}
    ]}]));

    let html = |examples, attributions| {
        let roles = RoleFilter { examples, attributions, part_of_speech: false };
        render_html(&d, Selection::Whole, roles).join("")
    };
    let both = html(true, true);
    assert!(both.contains("ご飯を食べる") && both.contains("JMdict"), "{both}");
    let no_examples = html(false, true);
    assert!(!no_examples.contains("ご飯を食べる") && no_examples.contains("JMdict"));
    let no_sources = html(true, false);
    assert!(no_sources.contains("ご飯を食べる") && !no_sources.contains("JMdict"));
    let neither = html(false, false);
    assert!(!neither.contains("ご飯を食べる") && !neither.contains("JMdict"));
    for got in [&both, &no_examples, &no_sources, &neither] {
        assert!(got.contains("to eat"), "the gloss survives every combination: {got}");
    }
}

/// Move a part-of-speech label to the card's `pos` field. Leave it out of the gloss
/// body and Anki HTML field. This tests a moved role, not a hidden role.
#[test]
fn a_part_of_speech_label_is_lifted_and_never_left_inline() {
    let d = doc(&json!([{"type": "structured-content", "content": [
        {"tag": "span", "data": {"content": "part-of-speech-info"}, "content": "noun"},
        {"tag": "span", "content": "chatting"}
    ]}]));

    assert_eq!(vec!["noun".to_string()], pos_labels(&d));
    assert_eq!(vec!["chatting".to_string()], plain_items(&d));
    let card = render_html(&d, Selection::Whole, RoleFilter::CARD).join("");
    assert!(!card.contains("noun"), "the card's own pos field prints it: {card}");
    assert!(card.contains("chatting"));
}

// ---------------------------------------------------------------------------
// robustness
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_tag_keeps_its_children_and_its_name() {
    let d = doc(&json!([{"type": "structured-content", "content": {
        "tag": "marquee", "content": [{"tag": "span", "content": "kept"}]
    }}]));

    let id = d.items().next().map(|i| d.children(i).next().unwrap()).unwrap();
    assert_eq!(Tag::Other, d.node(id).tag);
    assert_eq!(Kind::Unknown, d.node(id).kind);
    assert_eq!("marquee", d.tag_name(id), "the name is kept, so the tree stays lossless");
    assert_eq!(vec!["kept".to_string()], plain_items(&d));
}

/// Give every field the wrong type. The JSON remains valid, but its schema is
/// invalid. The parser must not panic or return an error because a hover cannot use
/// this data.
#[test]
fn a_malformed_node_does_not_panic() {
    let cases = [
        json!([{"tag": {"nested": true}, "content": "x"}]),
        json!([{"tag": 5, "content": "x"}]),
        json!([{"type": ["structured-content"], "content": "x"}]),
        json!([{"tag": "span", "content": 42}]),
        json!([{"tag": "span", "content": true}]),
        json!([{"tag": "span", "content": null}]),
        json!([{"tag": "span", "text": {"a": 1}}]),
        json!([{"tag": "span", "data": "not an object", "content": "x"}]),
        json!([{"tag": "span", "data": [1, 2], "content": "x"}]),
        json!([{"tag": "span", "style": "not an object", "content": "x"}]),
        json!([{"tag": "span", "style": {"fontSize": {"a": 1}}, "content": "x"}]),
        json!([{"tag": "td", "colSpan": "two", "content": "x"}]),
        json!([42, true, null, ["nested"]]),
        json!(null),
        json!({}),
        json!([]),
    ];
    for case in cases {
        let d = doc(&case);
        // These inputs must not panic. Run every renderer to prove that the tree remains
        // walkable afterward.
        let _ = plain_items(&d);
        let _ = pos_labels(&d);
        let _ = render_html(&d, Selection::Whole, RoleFilter::CARD);
        assert_eq!(0, d.degraded_items(), "valid JSON never needs the fallback: {case}");
    }
}

/// Isolate malformed rows as Yomitan and Hoshi Reader do. A depth cap does not
/// cover a bad node. One unreadable row must cost only that row.
#[test]
fn a_malformed_row_degrades_alone_and_the_rest_of_the_entry_renders() {
    // The middle item lacks a comma between two members. Its brackets still balance,
    // so the splitter isolates it. `serde_json` rejects it after it reads the text.
    let stored = r#"[
        {"type": "structured-content", "content": "first"},
        {"tag": "div", "content": "second" "extra": 1},
        {"type": "structured-content", "content": "third"}
    ]"#;
    let d = GlossDoc::parse(stored);

    assert_eq!(1, d.degraded_items());
    assert_eq!(
        vec!["first".to_string(), "second".to_string(), "third".to_string()],
        plain_items(&d),
        "the broken row loses its wrapper, not its text, and its siblings are untouched"
    );
}

#[test]
fn an_unreadable_row_costs_only_itself() {
    let stored = r#"["first", nonsense, "third"]"#;
    let d = GlossDoc::parse(stored);

    assert_eq!(1, d.degraded_items());
    assert_eq!(vec!["first".to_string(), "third".to_string()], plain_items(&d));
}

/// An unterminated array is one broken row, not an empty Entry.
#[test]
fn a_truncated_payload_keeps_the_rows_it_did_contain() {
    let stored = r#"["first", "second", {"tag": "div", "content": "th"#;
    let d = GlossDoc::parse(stored);

    let plain = plain_items(&d);
    assert!(plain.starts_with(&["first".to_string(), "second".to_string()]), "{plain:?}");
}

/// Add one `x` at each level. The render then shows how far the walk got.
fn nested(levels: usize) -> Value {
    let mut node = json!({"tag": "span", "content": "innermost"});
    for _ in 0..levels {
        node = json!({"tag": "div", "content": [{"tag": "span", "content": "x"}, node]});
    }
    json!([{"type": "structured-content", "content": node}])
}

#[test]
fn a_tree_nested_past_the_cap_is_truncated_not_failed() {
    // Use a depth just past the cap. This tests the cap itself. Three times the
    // census's deepest real tree still fits inside the limit.
    let levels = (MAX_DEPTH + 8) as usize;
    let d = doc(&nested(levels));

    assert!(d.truncated_subtrees() > 0, "the cap fired");
    assert_eq!(0, d.degraded_items(), "the cap truncates, it does not fail the item");

    let plain = plain_items(&d);
    assert_eq!(1, plain.len());
    assert!(plain[0].contains('x'), "the outer levels rendered");
    assert!(!plain[0].contains("innermost"), "the subtree past the cap is not rendered");
    // Each level adds three nodes: `div`, its `span`, and that span's text. The arena
    // adds no nodes after the cap.
    assert!(
        (d.all_nodes().len() as u32) <= 3 * MAX_DEPTH,
        "the arena stopped growing at the cap: {} nodes",
        d.all_nodes().len()
    );
}

/// Build a tree hundreds of levels deep. `serde_json` can reach the over-cap
/// subtree before the cap's `IgnoredAny` reads it fully. Per-item isolation can
/// catch it instead. Both mechanisms must terminate and preserve outer levels. This
/// test checks that guarantee, not which mechanism acts.
#[test]
fn an_absurdly_nested_tree_terminates_and_still_renders_its_outer_levels() {
    let d = doc(&nested(400));

    let plain = plain_items(&d);
    assert_eq!(1, plain.len());
    assert!(plain[0].contains('x'), "the outer levels rendered");
    assert!(!plain[0].contains("innermost"));
    // A tree eight levels past the cap has the same node count. Extra input depth adds
    // no nodes.
    assert_eq!(
        doc(&nested((MAX_DEPTH + 8) as usize)).all_nodes().len(),
        d.all_nodes().len()
    );
}

/// An array-only nest adds no node level. The cap cannot see it, so the
/// `serde_json` recursion limit bounds it. It costs one glossary item.
#[test]
fn an_array_only_nest_terminates() {
    let deep = format!("[{}\"x\"{}]", "[".repeat(400), "]".repeat(400));
    let d = GlossDoc::parse(&deep);
    assert!(plain_items(&d).is_empty());
}

#[test]
fn an_empty_document_is_empty() {
    let d = GlossDoc::empty();
    assert!(d.is_empty());
    assert!(plain_items(&d).is_empty());
    assert!(pos_labels(&d).is_empty());
    assert!(render_html(&d, Selection::Whole, RoleFilter::CARD).is_empty());
    assert_eq!(0, d.footprint());
}

/// Keep `Node` at 44 bytes. A larger node directly increases retained heap in the
/// parsed tree cache. Cache size was one of two layout criteria.
#[test]
fn a_node_stays_pointer_free_and_small() {
    assert_eq!(44, std::mem::size_of::<Node>());
}

/// Intern data keys so a node style probe can compare integer IDs.
#[test]
fn a_repeated_data_key_is_interned_once() {
    let d = doc(&json!([{"type": "structured-content", "content": [
        {"tag": "span", "data": {"name": "用例"}, "content": "a"},
        {"tag": "span", "data": {"name": "用例"}, "content": "b"},
        {"tag": "span", "data": {"name": "用例"}, "content": "c"}
    ]}]));

    let key = d.key_id("name").expect("the key interned");
    let hits = (0..d.all_nodes().len() as NodeId)
        .filter(|&i| d.data(i).iter().any(|(k, _)| *k == key))
        .count();
    assert_eq!(3, hits, "three nodes, one key id");
}

#[test]
fn full_selection_extent_matches_whole_for_two_fixture_shapes() {
    let plain = doc(&json!(["first", "second"]));
    let plain_extent = extent(&plain, RoleFilter::CARD).expect("plain extent");
    assert_eq!(
        render_html(&plain, Selection::Whole, RoleFilter::CARD),
        render_html(
            &plain,
            Selection::Ranges { ranges: &[plain_extent], separator: Separator::Ellipsis },
            RoleFilter::CARD,
        )
    );

    let jitendex = doc(&json!([{"type": "structured-content", "content": {
        "tag": "ol", "content": [
            {"tag": "li", "content": "first"},
            {"tag": "li", "content": "second"}
        ]
    }}]));
    let jitendex_extent = extent(&jitendex, RoleFilter::CARD).expect("ol extent");
    assert_eq!(
        render_html(&jitendex, Selection::Whole, RoleFilter::CARD),
        render_html(
            &jitendex,
            Selection::Ranges { ranges: &[jitendex_extent], separator: Separator::Space },
            RoleFilter::CARD,
        )
    );
}

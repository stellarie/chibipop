//! Parser tests: one fixture per real dictionary shape, plus the
//! losslessness, robustness and bounding guarantees the module promises.
//!
//! The fixtures come from `docs/research/dict-shapes.md`'s census of 97
//! archives rather than from imagination, so a fix verified on Jitendex is
//! not assumed to work on 大辞林. Where the census prints a representative
//! node verbatim it is used verbatim, and where it names only the hooks and
//! counts the fixture is built from those hooks - each one says which.

use super::*;
use serde_json::{json, Value};

/// A glossary as the record stores it, parsed the way a hover parses it.
///
/// The stored format is the dictionary's raw glossary JSON, so serialising a
/// `Value` is exactly what `dict::build` writes - which makes every test
/// below a round trip through the stored format rather than a shortcut past
/// it.
pub(super) fn doc(glossary: &Value) -> GlossDoc {
    GlossDoc::parse(&glossary.to_string())
}

/// The first node carrying this tag, in parse order.
fn find_tag(d: &GlossDoc, tag: Tag) -> Option<NodeId> {
    (0..d.all_nodes().len() as NodeId).find(|&i| d.node(i).tag == tag)
}

// ---------------------------------------------------------------------------
// losslessness over the census tables
// ---------------------------------------------------------------------------

/// Every tag `docs/research/dict-shapes.md` found in the corpus, with the
/// variant it must resolve to. `tbody` is the corpus's rarest at one
/// dictionary; `div`, `a` and `span` are its most common at 43 to 45.
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

/// Every style property the census found, with a value of the type the corpus
/// uses for it: em multipliers as numbers, keywords and colours as strings.
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

/// The value a census style property is written with in the fixture.
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

/// An unrecognised property is dropped from the record on purpose - the
/// record is a closed set - but it must not take its neighbours with it.
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

/// Every image field the census counted reaches the tree, because the media
/// store reads them off the node and a rebuild is the only way to get them
/// back.
#[test]
fn an_image_node_keeps_every_field_the_census_counted() {
    // Verbatim from dict-shapes.md's "Representative nodes", 三省堂国語辞典
    // 第八版: a monochrome SVG with a declared em size.
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
    // `serde_json`'s decimal parser is not always correctly rounded on a
    // 17-digit mantissa, so this census value comes back one ULP off its
    // literal. Pre-existing and invisible - the number is an em multiplier -
    // but an exact comparison would pin a `serde_json` implementation detail.
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

/// Jitendex. Built from the hooks the census names for it -
/// `content=part-of-speech-info` pills, `content=sense`, `content=glossary`,
/// `content=example-sentence*`, `content=attribution` - which are the six
/// names the deleted drop list held plus the two it did not cover.
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

    // The list marker Jitendex declares inline is on the node, so the popup
    // scene can draw it and the role classifier can see the roles beside it.
    let list = find_tag(&d, Tag::Ul).unwrap();
    assert_eq!(
        Some("\"◍\""),
        d.style_of(list, StyleKey::ListStyleType).and_then(|v| d.scalar_str(v))
    );
    let attribution = d.items().next().map(|item| d.children(item).nth(4).unwrap()).unwrap();
    assert_eq!(Role::Attribution, d.role(attribution));
    // Dropped from the *render*, kept in the tree: the popup's "show
    // attributions" setting has nothing to switch on otherwise.
    assert_eq!(Tag::A, d.node(d.children(attribution).next().unwrap()).tag);
}

/// 大辞林 第四版. The census names its `name=用例`, `name=慣用例` and
/// `name=用例注釈` hooks, and records that it draws its own `①②③` inside the
/// tree - which is why nothing synthesises an outer number for a sense.
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

    // Both example hooks classify, so 大辞林's examples now behave the way
    // Jitendex's always did: two senses as sibling blocks, each on its own
    // line, the dictionary's own marker intact, no synthesised number, and
    // the two `用例` blocks out of the summary. Before the role classifier
    // the drop list named neither `用例` nor `慣用例` and both rendered.
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

/// 明鏡国語辞典 第三版, whose 38 892 example sentences hang off an `example=`
/// key no fixed drop list ever named - the headline defect the role
/// classifier closes, since Jitendex lost its examples while this dictionary
/// kept every one.
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

/// 字通: 139 138 image nodes and 278 340 gaiji markers, more than four image
/// nodes per term row. The image node is verbatim from the census's
/// "Representative nodes".
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
    // The plain-text renderer has no way to draw a character as an image, so
    // it drops it; the tree keeps it for the renderer that can.
    assert_eq!(vec!["むかう".to_string()], plain_items(&d));
}

/// 三省堂国語辞典 第八版: a monochrome SVG at a declared em size, inline in
/// the middle of a definition, beside its `name=用例` hook.
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

    // 三省堂's own `用例G` hook classifies, so its example leaves the summary
    // exactly as Jitendex's always did - and no fixed drop list ever named
    // this key.
    let example = (0..d.all_nodes().len() as NodeId)
        .find(|&i| d.data_of(i, "name").is_some())
        .expect("the example hook survived the parse");
    assert_eq!(Role::Example, d.role(example));
    assert_eq!(vec!["みじかい".to_string()], plain_items(&d));

    // The image is a sibling of the text inside one block, not a block of
    // its own: it must not break the line it sits in. Asserted on the card,
    // which keeps the example, so the whole line is observable.
    assert_eq!(
        vec!["<div><span>みじかい</span>短<div>―スカート</div></div>".to_string()],
        render_html(&d, Selection::Whole, RoleFilter::CARD)
    );
}

/// A conjugation table: 18 dictionaries emit `tr`/`td`, 11 emit `th`, and 7
/// emit `rowSpan`. Cells are a grid problem, so only the row breaks a line.
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

    // One line per row; the cells within a row concatenate.
    assert_eq!(vec!["五段未然形書か\n連用形 書き".to_string()], plain_items(&d));
}

/// One of the 20 plain-string dictionaries - 精選版 日本国語大辞典, 漢字源 and
/// friends. They reach parity from the line-break work alone, and the point
/// here is that they arrive as the *same* shape as everything else.
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

/// A `type: text` item is the same one-node shape as a bare string, which is
/// what lets every renderer handle one case instead of three.
#[test]
fn a_typed_text_item_is_the_same_single_node_shape() {
    let d = doc(&json!([{"type": "text", "text": "to eat"}]));

    let id = d.items().next().unwrap();
    assert_eq!(Kind::Text, d.node(id).kind);
    assert_eq!(ItemType::Text, d.node(id).item_type);
    assert!(!d.is_plain_string(id), "a typed item is not a bare glossary string");
    assert_eq!(vec!["to eat".to_string()], plain_items(&d));
}

/// The four `details`/`summary` dictionaries, 31 000 nodes. Outside the HTML
/// allow-list, so the *renderers* drop the wrapper - but the tree keeps both
/// tags, which is what the popup scene needs to distinguish the summary.
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

/// One node carrying one `data` entry, which is the shape the classifier
/// reads.
fn roled(key: &str, value: &str) -> (GlossDoc, NodeId) {
    let d = doc(&json!([{"tag": "span", "data": {key: value}, "content": "x"}]));
    let id = d.items().next().expect("the node parsed");
    (d, id)
}

fn role_of_hook(key: &str, value: &str) -> Role {
    let (d, id) = roled(key, value);
    d.role(id)
}

/// Every row of `docs/research/dict-shapes.md`'s bilingual role table, in the
/// real spelling the census recorded rather than a paraphrase of it.
///
/// `(data key, data value, role)`. A row with an empty value is a key-side
/// hook - the dictionary's own field name carries the role and the value is
/// the empty marker the census records as `用例=`. A row with a value is a
/// value-side hook on one of the three convention keys.
///
/// The Japanese half is the majority by more than an order of magnitude:
/// 755 264 example nodes across four dictionaries against roughly 62 000
/// under the ASCII spellings, so a table that held only the ASCII rows would
/// pass while missing twelve example nodes in thirteen.
const CENSUS_ROLES: [(&str, &str, Role); 34] = [
    // -- example, Japanese keys: 755 264 nodes, 4 dictionaries.
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

/// The substring rule, on the spellings that motivated it: 旺文社 全訳古語辞典
/// writes eleven distinct `用例`-prefixed keys and 大辞林 two more, and no
/// equality table would have named them all.
#[test]
fn a_key_that_only_contains_a_needle_still_classifies() {
    for key in ["用例メタデータ", "用例囲みG", "用例訳注ロゴテキスト", "用例注釈", "注内用例G"] {
        assert_eq!(Role::Example, role_of_hook(key, ""), "{key}");
    }
    assert_eq!(Role::Reference, role_of_hook("参照語義番号", ""));
    assert_eq!(Role::Commentary, role_of_hook("語義パネル解説G", ""));
    assert_eq!(Role::Attribution, role_of_hook("出典G", ""));
}

/// Normalisation, one row per fold, and each row is a spelling a converter
/// produces rather than one invented to exercise the code.
#[test]
fn classification_normalises_case_width_and_the_ideographic_space() {
    // Case: 旺文社漢字典 writes `ExampleG` where Jitendex writes
    // `example-sentence`.
    assert_eq!(Role::Example, role_of_hook("EXAMPLE", ""));
    assert_eq!(Role::Example, role_of_hook("ExAmPlE", ""));
    assert_eq!(Role::PartOfSpeech, role_of_hook("content", "Part-Of-Speech-Info"));
    // Width: an HTML- or EPUB-sourced dictionary writes full-width Latin.
    assert_eq!(Role::Example, role_of_hook("Ｅｘａｍｐｌｅ", ""));
    assert_eq!(Role::Example, role_of_hook("name", "ｅｘａｍｐｌｅ＿１"));
    // A class list, as 旺文社 全訳古語辞典 writes it - and with the
    // ideographic space a Japanese source is as likely to use.
    assert_eq!(Role::Example, role_of_hook("class", "fill 用例 FM"));
    assert_eq!(Role::Example, role_of_hook("class", "fill　用例　FM"));
    assert_eq!(Role::Example, role_of_hook("class", "用例活用 IM"));
}

/// The value side is three exact conventions, not a family of spellings.
///
/// Nine census keys hold `content`, `name` or `class` without being one. A
/// substring rule there would read a role out of their values, and every one
/// of those reads could only ever hide content.
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

/// A dictionary using none of the conventions renders in full - 78 of the
/// census's 97 archives, and the failure direction the evidence demands.
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

/// Two hooks on one node take the smaller role whichever order the
/// dictionary wrote them in, so the classifier does not inherit a JSON
/// field order as a policy.
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

/// A convention key whose value is not a string names no role, but still
/// marks a block - the two questions are separate and stay separate.
#[test]
fn a_convention_key_with_a_non_string_value_is_content_and_still_a_block() {
    let d = doc(&json!([{"tag": "span", "data": {"content": 3}, "content": "x"}]));
    let id = d.items().next().unwrap();
    assert_eq!(Role::Content, d.role(id));
    assert!(d.has_marker(id), "a non-string marker still opens a block");
}

/// The equivalence the deleted drop list used to give exactly one dictionary.
///
/// Its six names were three example spellings and two attribution ones, and
/// the part-of-speech marker sat beside it. Each one classifies to the role
/// whose knob now governs it, so a summary drops what the list dropped -
/// and every other dictionary's hooks now reach the same knobs.
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

/// At the tree's own seam: one document, one parse, the example gone from the
/// summary and present on the card.
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

/// Attributions and examples are two knobs over one document, and all four
/// combinations differ.
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

/// A part-of-speech label reaches the card's `pos` field and neither the
/// gloss body nor the Anki HTML field, which is what makes it a *moved*
/// role rather than a hidden one.
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

/// Every field at the wrong type at once. Valid JSON, invalid schema: the
/// parse must not panic and must not error, because a hover has nothing
/// useful to do with either.
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
        // The assertion is that these do not panic; running every renderer
        // over the result is what proves the tree is walkable afterwards.
        let _ = plain_items(&d);
        let _ = pos_labels(&d);
        let _ = render_html(&d, Selection::Whole, RoleFilter::CARD);
        assert_eq!(0, d.degraded_items(), "valid JSON never needs the fallback: {case}");
    }
}

/// Per-item isolation, which is what Yomitan and Hoshi Reader both do: a
/// depth cap does not cover a bad node, so one unreadable row must cost that
/// row alone.
#[test]
fn a_malformed_row_degrades_alone_and_the_rest_of_the_entry_renders() {
    // The middle item is missing a comma between two members. Its brackets
    // still balance, so the splitter isolates it, and `serde_json` rejects
    // it after it has already read the text.
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

/// An unterminated array is one broken row, not an empty entry.
#[test]
fn a_truncated_payload_keeps_the_rows_it_did_contain() {
    let stored = r#"["first", "second", {"tag": "div", "content": "th"#;
    let d = GlossDoc::parse(stored);

    let plain = plain_items(&d);
    assert!(plain.starts_with(&["first".to_string(), "second".to_string()]), "{plain:?}");
}

/// One `x` per level, so the render says exactly how far the walk got.
fn nested(levels: usize) -> Value {
    let mut node = json!({"tag": "span", "content": "innermost"});
    for _ in 0..levels {
        node = json!({"tag": "div", "content": [{"tag": "span", "content": "x"}, node]});
    }
    json!([{"type": "structured-content", "content": node}])
}

#[test]
fn a_tree_nested_past_the_cap_is_truncated_not_failed() {
    // Modestly past the cap, which is where the cap is the mechanism: three
    // times the census's deepest real tree is still well inside it.
    let levels = (MAX_DEPTH + 8) as usize;
    let d = doc(&nested(levels));

    assert!(d.truncated_subtrees() > 0, "the cap fired");
    assert_eq!(0, d.degraded_items(), "the cap truncates, it does not fail the item");

    let plain = plain_items(&d);
    assert_eq!(1, plain.len());
    assert!(plain[0].contains('x'), "the outer levels rendered");
    assert!(!plain[0].contains("innermost"), "the subtree past the cap is not rendered");
    // Three nodes per level - the `div`, its `span`, and the span's text -
    // and none past the cap: the arena stopped growing.
    assert!(
        (d.all_nodes().len() as u32) <= 3 * MAX_DEPTH,
        "the arena stopped growing at the cap: {} nodes",
        d.all_nodes().len()
    );
}

/// Hundreds of levels deep. `serde_json`'s own recursion limit may reach the
/// over-cap subtree before the cap's `IgnoredAny` finishes consuming it, in
/// which case per-item isolation catches it instead. Either mechanism must
/// terminate and leave the outer levels rendering, which is the guarantee
/// this asserts - not which one fired.
#[test]
fn an_absurdly_nested_tree_terminates_and_still_renders_its_outer_levels() {
    let d = doc(&nested(400));

    let plain = plain_items(&d);
    assert_eq!(1, plain.len());
    assert!(plain[0].contains('x'), "the outer levels rendered");
    assert!(!plain[0].contains("innermost"));
    // The same node count as a tree eight levels past the cap: input depth
    // beyond the cap buys nothing at all.
    assert_eq!(
        doc(&nested((MAX_DEPTH + 8) as usize)).all_nodes().len(),
        d.all_nodes().len()
    );
}

/// An array-only nest adds no node level, so the cap never sees it and
/// `serde_json`'s limit is what bounds it. It costs one glossary item.
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

/// A `Node` is 44 bytes and the arena's whole point is that it stays that
/// way: a wider node multiplies straight into the retained heap the parsed
/// tree cache holds, which is one of the two axes that chose this layout.
#[test]
fn a_node_stays_pointer_free_and_small() {
    assert_eq!(44, std::mem::size_of::<Node>());
}

/// Interning is what makes the per-node style probe an integer compare.
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

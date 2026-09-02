//! Capture geometry snapshots for the layout goldens.
//! See `ARCHITECTURE.md#popup-and-measurement` for the schema contract.
//!
//! A snapshot records the values that `layout_pass` computes for one
//! `Presentation`. It stores fixed-precision JSON for element rects, hit rects,
//! `HitAction`s, content height, `max_scroll`, side-panel geometry, and the
//! `measure()` triple.
//!
//! The capture only measures the scene. It uses no window, render target, or paint
//! operation. Therefore it runs on the unmodified build.
//!
//! **Schema expansion.** Each element now records styled spans, line and span
//! geometry from the measurer, its box, readings, list markers, dictionary
//! address, and media key. `Metrics { w, h, lines }` cannot show the position
//! of a small span inside a tall line. The baseline supplies that position, and
//! no other test checks it. The goldens now protect range format behavior in the
//! DirectWrite adapter and its baseline output after each bless run.
//!
//! Floats keep the schema's fixed precision, and goldens use exact equality.
//! See [`px`].
//!
//! Keep the fixtures here. The golden test and the `CHIBIPOP_BLESS=1` bless path
//! must call the same capture path, so capture and check stay consistent.

use super::*;
use crate::dict::media::{Intrinsic, MediaFormat};
use crate::dict::sheet;
use crate::geom::PhysRect;
use crate::dict::pitch::{Accent, Position};
use crate::present::{AnkiPopupState, Card, CollapsedRow, GlossBlock, GlossEntry, PitchRow};
use crate::text::layout::TextGeom;
use crate::text::TextSpan;
use crate::ui::layout::{BoxStyle, Edges, ElemBox, GlossOrigin, MarkerBox, Rgb, RubyBox};
use serde_json::{json, Value};

/// Complete input for one capture.
pub struct Spec {
    pub theme: Theme,
    pub p: Presentation,
    /// Maximum width in physical pixels. The capture uses this value as `measure`.
    pub max_w: i32,
    pub max_h: i32,
    pub show_back: bool,
    pub side_panel: bool,
    /// Scroll request in physical pixels. The code clamps it to
    /// `max_scroll` as the app does.
    pub scroll: i32,
    pub anki: AnkiPopupState,
    /// Source text geometry for the match highlight when the fixture covers it.
    pub span: Option<TextSpan>,
}

impl Spec {
    /// Build the common case with the dark theme and default popup box.
    /// This case has no chrome.
    fn plain(p: Presentation) -> Spec {
        Spec {
            theme: Theme::dark(),
            p,
            // Use the default 25% x 45% popup config for a 1920x1080 display.
            max_w: 480,
            max_h: 486,
            show_back: false,
            side_panel: false,
            scroll: 0,
            anki: AnkiPopupState::disabled(),
            span: None,
        }
    }
}

/// A named fixture with one golden file and one or more labeled captures.
pub struct Fixture {
    pub name: &'static str,
    pub variants: Vec<(&'static str, Spec)>,
}

/// The sixteen pinned fixtures.
///
/// Seven fixtures come from the original set. Six more cover fields that
/// plain-string fixtures cannot fill. An empty field does not gate the test.
/// The final three cover card-header pitch geometry, which had no earlier
/// fixture. They pin the header marks.
///
/// Per-fixture purpose lives in `ARCHITECTURE.md#verification`. Keep the text
/// consistent with this list.
pub fn fixtures() -> Vec<Fixture> {
    vec![
        wrapping_heavy(),
        side_panel_fixture(),
        scrolled(),
        match_highlight_fixture(),
        full_chrome(),
        minimal_edge(),
        kitchen_sink(),
        styled_spans(),
        bordered_pill(),
        nested_list(),
        table_spans(),
        ruby_run(),
        inline_image(),
        pitch_single(),
        pitch_multiple(),
        pitch_sources(),
    ]
}

/// Capture every variant in `f`.
///
/// The result contains the whole golden file.
pub fn snapshot(f: &Fixture) -> Result<Value> {
    let mut variants = serde_json::Map::new();
    for (label, spec) in &f.variants {
        variants.insert((*label).to_string(), capture(f.name, label, spec)?);
    }
    Ok(json!({ "fixture": f.name, "variants": Value::Object(variants) }))
}

/// Serialize a snapshot with stable text.
pub fn to_json_text(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("geometry snapshots are plain JSON trees");
    s.push('\n');
    s
}

/// Capture one scene without paint or a window.
///
/// Build the scene with the same `Text` engine that paint uses. Then project it
/// onto the golden schema. This function reads geometry and does not choose it.
fn capture(fixture: &str, variant: &str, spec: &Spec) -> Result<Value> {
    let text = Text::new()?;
    // Use no window. The capture uses 96 DPI because the runner has no HWND to query.
    let scale = 1.0f32;

    // Use the shipped render settings so the golden records a fresh install.
    // Do not exercise a render knob in a fixture. The layout tests assert those
    // settings through `layout::scene` with the layout fake. The fake needs no
    // font stack (`src/ui/layout/tests.rs`).
    let scene = scene_of(
        &text,
        &spec.p,
        &spec.theme,
        (spec.max_w as f32 / scale, spec.max_h as f32 / scale),
        spec.show_back,
        spec.side_panel,
        RenderSettings::default(),
    )?;
    let (total_w, view_h, content_h) = popup_size(&scene, scale, spec.max_w);

    let span_px = layout::max_scroll(content_h, view_h);
    let scroll_px = spec.scroll.clamp(0, span_px);
    let scroll_dip = (scroll_px as f32 / scale) as i32;

    let side_json = match &scene.side {
        Some(side) => {
            let rows: Vec<Value> = side
                .rows
                .iter()
                .map(|r| {
                    json!({
                        "idx": r.idx,
                        "text": r.text,
                        "y": px(r.y * scale),
                        "h": px(r.h * scale),
                    })
                })
                .collect();
            json!({
                "separator_x": px(side.rule_x * scale),
                "col_x": px(side.col_x * scale),
                "col_w": px(side.col_w * scale),
                "height": px(side.height * scale),
                "rows": rows,
            })
        }
        None => Value::Null,
    };

    // Read the thumb from the same scene method that
    // `paint_once` calls. The golden records the derivation
    // that the renderer uses, not a second copy.
    let thumb = scene
        .scrollbar_thumb(spec.theme.padding, view_h, scroll_dip)
        .map(|(top, h)| json!({ "top": top, "h": h }))
        .unwrap_or(Value::Null);

    let mut elements: Vec<Value> = Vec::with_capacity(scene.elems.len());
    for (i, e) in scene.elems.iter().enumerate() {
        elements.push(elem_json(i, e, &text, &spec.theme, scale)?);
    }

    // Record main-column targets only. `side_panel` covers the side-column
    // targets above.
    let hit_json: Vec<Value> = scene
        .hits
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({
                "i": i,
                "action": format!("{:?}", r.action),
                "x": r.x.map(|v| px(v * scale)).unwrap_or(Value::Null),
                "y": px(r.y * scale),
                "w": r.w.map(|v| px(v * scale)).unwrap_or(Value::Null),
                "h": px(r.h * scale),
            })
        })
        .collect();

    let highlight = spec
        .span
        .as_ref()
        .and_then(|span| crate::present::match_highlight(span, spec.p.top.as_ref()))
        .map(rect_json)
        .unwrap_or(Value::Null);

    let anki = layout::anki_button_label(&spec.p, &spec.theme, &spec.anki)
        .map(|(label, color)| json!({ "label": label, "color": [color.0, color.1, color.2] }))
        .unwrap_or(Value::Null);

    Ok(json!({
        "input": {
            "fixture": fixture,
            "variant": variant,
            "max_w": spec.max_w,
            "max_h": spec.max_h,
            "show_back": spec.show_back,
            "side_panel": spec.side_panel,
            "scroll_requested": spec.scroll,
            "dpi_scale": px(scale),
        },
        "measure": { "total_w": total_w, "view_h": view_h, "content_h": content_h },
        "scroll": { "max_scroll": span_px, "scroll_used": scroll_px, "thumb": thumb },
        "content": { "used_h": px(scene.used_h * scale), "content_w": px(scene.content_w * scale), "origin": px(scene.origin * scale) },
        "elements": elements,
        "hits": hit_json,
        "side_panel": side_json,
        "match_highlight": highlight,
        "anki_button": anki,
    }))
}

/// Serialize one complete element.
///
/// Keep `pen` with the rect. The added fields include readings, markers, and
/// the measurer's line and span boxes. These fields use *run-relative*
/// coordinates. A reviewer cannot check a coordinate without its origin.
fn elem_json(i: usize, e: &SceneElem, text: &Text, theme: &Theme, scale: f32) -> Result<Value> {
    let spans: Vec<Value> = e
        .spans
        .iter()
        .map(|s| {
            json!({
                "at": s.at,
                "len": s.len,
                "size": px(s.size * scale),
                "weight": s.weight,
                "italic": s.italic,
                "shift": px(s.shift * scale),
                "color": rgb(s.color),
            })
        })
        .collect();
    let ruby: Vec<Value> =
        e.ruby.iter().map(|r| OutOfFlow::from(r).json(scale)).collect();
    let marker: Vec<Value> =
        e.marker.iter().map(|m| OutOfFlow::from(m).json(scale)).collect();
    let inline_boxes: Vec<Value> =
        e.inline_boxes.iter().map(|b| box_json(b, scale)).collect();

    Ok(json!({
        "i": i,
        "kind": e.kind.as_str(),
        "text": e.text,
        "font_size": px(e.font_size * scale),
        "top_gap": px(e.top_gap * scale),
        "wrap_w": px(e.wrap_w * scale),
        "pen_x": px(e.pen.0 * scale),
        "pen_y": px(e.pen.1 * scale),
        "x": px(e.rect.x * scale),
        "y": px(e.rect.y * scale),
        "w": px(e.rect.w * scale),
        "h": px(e.rect.h * scale),
        "lines": e.lines,
        "advance": px(e.advance * scale),
        "spans": spans,
        "measured": measured_json(text, theme, e, scale)?,
        "ruby": ruby,
        "marker": marker,
        "block_box": e.block_box.as_ref().map(|b| box_json(b, scale)).unwrap_or(Value::Null),
        "inline_boxes": inline_boxes,
        "origin": origin_json(e.origin),
        "image": e.image.as_ref().map(image_json).unwrap_or(Value::Null),
    }))
}

/// Ask the measurer for the geometry of one element run.
///
/// The scene has no [`LineBox`] or [`SpanBox`]. It stores stacked results, not
/// line detail. Ask the adapter for a baseline and each span's share of its
/// line.
///
/// The request rebuilds the spans from [`SceneElem::styled_spans`] at the scene
/// width. `paint_once` makes the same request at paint time.
/// This function reads geometry. It does not choose it. If the adapter returned
/// another result, the panel ink would not match its hit rects.
///
/// Return `null` when the element has no spans. Rules, block boxes, table boxes,
/// and cell borders have no text to wrap.
fn measured_json(text: &Text, theme: &Theme, e: &SceneElem, scale: f32) -> Result<Value> {
    if e.spans.is_empty() {
        return Ok(Value::Null);
    }
    let spans: Vec<StyledSpan<'_>> = e.styled_spans(&theme.font_name).collect();
    let mut out = Measured::default();
    text.measurer()
        .measure(MeasureRun { spans: &spans, max_w: e.wrap_w }, &mut out)
        .map_err(anyhow::Error::new)
        .with_context(|| format!("re-measuring {} element {:?}", e.kind.as_str(), e.text))?;

    let lines: Vec<Value> = out
        .lines
        .iter()
        .map(|l| {
            json!({
                "y": px(l.y * scale),
                "w": px(l.w * scale),
                "h": px(l.h * scale),
                "baseline": px(l.baseline * scale),
            })
        })
        .collect();
    let boxes: Vec<Value> = out
        .spans
        .iter()
        .map(|b| {
            json!({
                "span": b.span,
                "line": b.line,
                "x": px(b.x * scale),
                "w": px(b.w * scale),
                "h": px(b.h * scale),
            })
        })
        .collect();
    Ok(json!({
        "w": px(out.metrics.w * scale),
        "h": px(out.metrics.h * scale),
        "lines": out.metrics.lines,
        "line_boxes": lines,
        "span_boxes": boxes,
    }))
}

/// A positioned run outside its element flow.
///
/// A reading and a list marker share the same nine fields. Neither is a span
/// that the measurer wraps. Use one shape for both to prevent drift.
struct OutOfFlow<'a> {
    text: &'a str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    size: f32,
    color: Rgb,
    weight: u16,
    italic: bool,
}

impl OutOfFlow<'_> {
    fn json(&self, scale: f32) -> Value {
        json!({
            "text": self.text,
            "x": px(self.x * scale),
            "y": px(self.y * scale),
            "w": px(self.w * scale),
            "h": px(self.h * scale),
            "size": px(self.size * scale),
            "weight": self.weight,
            "italic": self.italic,
            "color": rgb(self.color),
        })
    }
}

impl<'a> From<&'a RubyBox> for OutOfFlow<'a> {
    fn from(r: &'a RubyBox) -> OutOfFlow<'a> {
        OutOfFlow {
            text: &r.text,
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
            size: r.size,
            color: r.color,
            weight: r.weight,
            italic: r.italic,
        }
    }
}

impl<'a> From<&'a MarkerBox> for OutOfFlow<'a> {
    fn from(m: &'a MarkerBox) -> OutOfFlow<'a> {
        OutOfFlow {
            text: &m.text,
            x: m.x,
            y: m.y,
            w: m.w,
            h: m.h,
            size: m.size,
            color: m.color,
            weight: m.weight,
            italic: m.italic,
        }
    }
}

/// Serialize one box that can receive fill and stroke.
///
/// Record declared widths, not used widths. `border_used` comes from those
/// fields, and layout tests already pin that calculation. A record here
/// would duplicate one change.
fn box_json(b: &ElemBox, scale: f32) -> Value {
    json!({
        "rect": {
            "x": px(b.rect.x * scale),
            "y": px(b.rect.y * scale),
            "w": px(b.rect.w * scale),
            "h": px(b.rect.h * scale),
        },
        "style": style_json(b.style, scale),
    })
}

fn style_json(s: BoxStyle, scale: f32) -> Value {
    json!({
        "margin": lengths(s.margin, scale),
        "padding": lengths(s.padding, scale),
        "border": lengths(s.border, scale),
        "border_style": [
            s.border_style.top.as_str(),
            s.border_style.right.as_str(),
            s.border_style.bottom.as_str(),
            s.border_style.left.as_str(),
        ],
        "border_color": rgb(s.border_color),
        "radius": px(s.radius * scale),
        "background": s.background.map(rgb).unwrap_or(Value::Null),
    })
}

/// Return four edges in CSS order: top, right, bottom, left.
fn lengths(e: Edges<f32>, scale: f32) -> Value {
    json!([
        px(e.top * scale),
        px(e.right * scale),
        px(e.bottom * scale),
        px(e.left * scale),
    ])
}

fn rgb(c: Rgb) -> Value {
    json!([c.0, c.1, c.2])
}

/// Serialize the dictionary address of an element.
///
/// `path` contains child indices from the outermost node. This is the identity
/// that a sense picker selects. Return `null` when a node is too deep to address.
fn origin_json(o: Option<GlossOrigin>) -> Value {
    match o {
        None => Value::Null,
        Some(o) => json!({
            "dict_id": o.dict_id,
            "entry_id": o.entry_id,
            "path": o.path.map(|p| json!(p.steps())).unwrap_or(Value::Null),
        }),
    }
}

/// Serialize the asset named by an image element.
///
/// Record the key and format, not bytes. Core resolves each image box from its
/// declared dimensions first, then its recorded intrinsic size, and finally a
/// square with the surrounding text size. The element owns that resolved box.
fn image_json(img: &SceneImage) -> Value {
    json!({
        "key": img.key.as_ref().map(|k| json!({ "dict_id": k.dict_id, "path": k.path })),
        "format": img.format.map(MediaFormat::as_str),
        "appearance": format!("{:?}", img.appearance),
        "background": img.background,
        "collapsed": img.collapsed,
        "collapsible": img.collapsible,
    })
}

/// Format a value with two decimals as a string.
///
/// The golden compares text and never parses floats again. Convert `-0.0` to
/// `0.0` so zero has one sign.
fn px(v: f32) -> Value {
    let r = (f64::from(v) * 100.0).round() / 100.0;
    let r = if r == 0.0 { 0.0 } else { r };
    Value::String(format!("{r:.2}"))
}

fn rect_json(r: PhysRect) -> Value {
    json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })
}

// ---- fixtures ----

fn card(
    written: Option<&str>,
    reading: Option<&str>,
    pos: &[&str],
    freq: Option<i64>,
    blocks: Vec<GlossBlock>,
    match_len: usize,
) -> Card {
    Card {
        written: written.map(str::to_string),
        reading: reading.map(str::to_string),
        pos: pos.iter().map(|s| s.to_string()).collect(),
        freq,
        blocks,
        match_len,
        pitch: Vec::new(),
    }
}

/// One accent and the dictionaries that supplied it.
///
/// Use real accents from `docs/research/pitch-accent-shapes.md`'s census. A
/// changed golden then reports geometry from a reading that a real dictionary
/// publishes.
fn pitch(fall: u32, dicts: &[&str]) -> PitchRow {
    PitchRow {
        accent: Accent {
            position: Position::Downstep(fall),
            nasal: Vec::new(),
            devoice: Vec::new(),
            tags: Vec::new(),
        },
        dicts: dicts.iter().map(|d| d.to_string()).collect(),
    }
}

/// Parse one row as one dictionary block.
///
/// The layout pass renders each row's parsed tree. A fixture therefore uses the
/// tree that its strings parse to. Each item remains a bare glossary string,
/// the shape that 20 of the census's 72 dictionaries emit. The inline pass
/// joins one row's items with the same `; ` that the panel uses and coalesces
/// them into one styled span. The seam receives the same request as before the
/// pass existed.
///
/// One block represents one matched term-bank row. The goldens hold one
/// dictionary label with one gloss body. The six structured fixtures use [`tree`]
/// because this helper cannot express a second style on one line.
fn block(dict: &str, glosses: &[&str]) -> GlossBlock {
    GlossBlock::parse(dict, &serde_json::json!(glosses).to_string())
}

/// Wrap `content` as one structured-content item, as a term bank stores a row.
fn sc(content: &str) -> String {
    format!(r#"[{{"type":"structured-content","content":{content}}}]"#)
}

/// Build one dictionary contribution with its rows, `styles.css`, and recorded
/// media rows.
///
/// A real block has five facts, so this helper takes five arguments.
/// Keep the same body in one helper. Do not duplicate it across
/// overload-shaped helpers.
/// `dict::sheet` performs the stylesheet fold. The hover path calls it between
/// parse and tree-cache steps in `SqliteDictionary::entries`. This fixture calls
/// the same two functions to reproduce a real hover. Code below need not know
/// about CSS.
///
/// Use a real `dict_id`, not `NO_ROW`, when the fixture has media or checks a
/// [`GlossOrigin`]. The media key and dictionary address both use this value.
/// A zero value would hide a mix-up between them.
fn tree(
    dict: &str,
    dict_id: i64,
    rows: &[&str],
    css: &str,
    media: &[(&str, Intrinsic)],
) -> GlossBlock {
    let sheet = sheet::Sheet::compile(css);
    let entries = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut doc = crate::dict::gloss::GlossDoc::parse(row);
            sheet::apply(&mut doc, &sheet);
            let doc = std::sync::Arc::new(doc);
            GlossEntry {
                // Number rows within each dictionary like a term bank.
                // The snapshot can then name the source row, not only the dictionary.
                entry_id: dict_id * 100 + i as i64 + 1,
                glosses: crate::dict::gloss::plain_items(&doc),
                tags: Vec::new(),
                doc,
                media: media.iter().map(|(p, i)| ((*p).to_string(), *i)).collect(),
            }
        })
        .collect();
    GlossBlock { dict_name: dict.to_string(), dict_id, entries }
}

/// Record one media row as `dict::media::probe` writes it.
///
/// Construct an [`Intrinsic`] from a format and two dimensions. The caller stores
/// the path with the returned value. The size pass reads these values, not the
/// bytes. An image fixture then needs no archive, decoder, or database.
fn recorded(format: MediaFormat, w: f32, h: f32) -> Intrinsic {
    Intrinsic { format, width: w, height: h, aspect: w / h }
}

fn row(written: Option<&str>, reading: Option<&str>, summary: &str) -> CollapsedRow {
    CollapsedRow {
        written: written.map(str::to_string),
        reading: reading.map(str::to_string),
        summary: summary.to_string(),
    }
}

fn present(top: Option<Card>, collapsed: Vec<CollapsedRow>) -> Presentation {
    let all_cards = top.iter().cloned().collect();
    Presentation { top, collapsed, all_cards, sentence: None }
}

/// Create 30-pixel character boxes from x=100, like the fixtures in
/// `present.rs`.
fn span_of(text: &str, cursor_byte_offset: usize) -> TextSpan {
    let geom = (0..text.chars().count())
        .map(|i| TextGeom {
            char_count: 1,
            rect: PhysRect { x: 100 + 30 * i as i32, y: 200, w: 30, h: 40 },
        })
        .collect();
    TextSpan {
        text: text.to_string(),
        cursor_byte_offset,
        anchor: PhysRect { x: 100, y: 200, w: 30, h: 40 },
        geom,
    }
}

/// 1. The wrap loop and gap stack.
fn wrapping_heavy() -> Fixture {
    let top = card(
        Some("胡蝶の夢"),
        Some("こちょうのゆめ"),
        &["expression", "noun"],
        Some(24810),
        vec![
            block(
                "Jitendex",
                &[
                    "the butterfly dream: the illusion that it is impossible to \
                     distinguish between the dreamer and the dream, between waking \
                     life and the images it leaves behind",
                    "an experience impossible to tell apart from a vivid dream of \
                     the same experience",
                ],
            ),
            block(
                "大辞林",
                &["夢の中で胡蝶となった荘子が、夢がさめてから、自分が夢で胡蝶となったのか、\
                   胡蝶が夢で自分となっているのかと疑ったという故事。人生の儚さのたとえ。"],
            ),
        ],
        4,
    );
    let mut spec = Spec::plain(present(Some(top), vec![]));
    // Use a narrow width so every gloss wraps.
    spec.max_w = 360;
    spec.max_h = 700;
    Fixture { name: "wrapping_heavy", variants: vec![("default", spec)] }
}

/// 2. The same rows in both modes.
fn side_panel_fixture() -> Fixture {
    let build = |side: bool| {
        let top = card(
            Some("生"),
            Some("なま"),
            &["adjectival noun", "noun"],
            Some(1204),
            vec![block("Jitendex", &["raw; uncooked; fresh", "natural; unedited"])],
            1,
        );
        let collapsed = vec![
            row(Some("生"), Some("せい"), "life; living"),
            row(Some("生"), Some("き"), "pure; undiluted"),
            // Headless row: the panel path skips it,
            // but inline mode keeps it.
            row(None, None, "summary-only row with no headword"),
        ];
        let mut spec = Spec::plain(present(Some(top), collapsed));
        spec.side_panel = side;
        spec
    };
    Fixture {
        name: "side_panel",
        variants: vec![("panel", build(true)), ("inline", build(false))],
    }
}

/// 3. Overflow with `max_scroll` and the thumb.
fn scrolled() -> Fixture {
    let blocks: Vec<GlossBlock> = (1..=8)
        .map(|i| {
            block(
                match i % 3 {
                    0 => "Jitendex",
                    1 => "大辞林",
                    _ => "新明解国語辞典",
                },
                &[
                    "a long enough gloss to give every one of these eight blocks a \
                     wrapped body line under its dictionary label",
                ],
            )
        })
        .collect();
    let top = card(Some("辞書"), Some("じしょ"), &["noun"], Some(3400), blocks, 2);
    let mut spec = Spec::plain(present(Some(top), vec![]));
    spec.max_h = 240;
    spec.scroll = 120;
    Fixture { name: "scrolled", variants: vec![("default", spec)] }
}

/// 4. Compare `HIGHLIGHT_PAD` with glyph boxes.
fn match_highlight_fixture() -> Fixture {
    let top = card(
        Some("可哀想"),
        Some("かわいそう"),
        &["adjectival noun"],
        Some(5120),
        vec![block("Jitendex", &["poor; pitiable; pathetic"])],
        3,
    );
    let mut spec = Spec::plain(present(Some(top), vec![]));
    spec.span = Some(span_of("その可哀想", "その".len()));
    Fixture { name: "match_highlight", variants: vec![("default", spec)] }
}

/// 5. Rects for every `HitAction`: the back button, drill-down kanji, expanded
///    rows, corner, and Anki label.
fn full_chrome() -> Fixture {
    let top = card(
        Some("雑談"),
        Some("ざつだん"),
        &["noun", "suru verb"],
        Some(7671),
        vec![block("Jitendex", &["chatting; idle talk"])],
        2,
    );
    let collapsed = vec![
        row(Some("雑"), Some("ざつ"), "rough; crude; miscellaneous"),
        row(Some("談"), Some("だん"), "talk; conversation"),
    ];
    let mut spec = Spec::plain(present(Some(top), collapsed));
    spec.show_back = true;
    let mut anki = AnkiPopupState::disabled();
    anki.enabled = true;
    anki.connected = true;
    anki.dupes.insert("雑談".to_string());
    spec.anki = anki;
    Fixture { name: "full_chrome", variants: vec![("default", spec)] }
}

/// 6. Degenerate paths for gaps.
fn minimal_edge() -> Fixture {
    // No reading, part-of-speech label, or Frequency rank.
    // The block has no glosses, so it draws the label and skips the body.
    let sparse = Spec::plain(present(
        Some(card(Some("猫"), None, &[], None, vec![block("大辞林", &[])], 1)),
        vec![],
    ));
    // Empty presentation: the renderer sizes the popup to its padding.
    let empty = Spec::plain(present(None, vec![]));
    Fixture { name: "minimal_edge", variants: vec![("sparse", sparse), ("empty", empty)] }
}

/// 7. All features together.
fn kitchen_sink() -> Fixture {
    let top = card(
        Some("面白い"),
        Some("おもしろい"),
        &["i-adjective"],
        Some(890),
        vec![
            block(
                "Jitendex",
                &[
                    "interesting; fascinating; intriguing; enthralling",
                    "amusing; funny; comical",
                    "enjoyable; fun; entertaining; pleasant; agreeable",
                ],
            ),
            block("大辞林", &["心が引かれて、楽しい気持ちになるさま。興味をそそられるさま。"]),
            // Empty glosses: the label stands alone.
            block("新明解国語辞典", &[]),
        ],
        3,
    );
    let collapsed = vec![
        row(Some("面"), Some("めん"), "face; surface; mask"),
        row(Some("白い"), Some("しろい"), "white"),
        row(None, Some("おも"), "reading-only headword row"),
        row(Some("面白"), None, "written-only headword row"),
        row(None, None, "headless summary row"),
    ];
    let mut spec = Spec::plain(present(Some(top), collapsed));
    spec.show_back = true;
    spec.side_panel = true;
    spec.max_h = 300;
    spec.scroll = 40;
    let mut anki = AnkiPopupState::disabled();
    anki.enabled = true;
    anki.connected = true;
    anki.added.insert("面白い".to_string());
    spec.anki = anki;
    spec.span = Some(span_of("実に面白い話", "実に".len()));
    Fixture { name: "kitchen_sink", variants: vec![("default", spec)] }
}

// ---- the six added by the widening ----
//
// These six fixtures are authored, not captured. The capture property
// "capturable from the unmodified pre-refactor build" does not include styled
// content because the old build had no styled spans. Each tree comes from a
// real shape in `docs/research/dict-shapes.md`, not an invented shape. A changed
// golden therefore reports geometry from a dictionary that uses the shape.

/// 8. Mixed styles inside one wrapped paragraph.
///
/// This fixture tests the schema change. Before the change, a run boundary
/// always matched a line boundary. Bold and normal text could not share a
/// wrapped line. This paragraph makes five style changes without a line break,
/// and the narrow box forces a wrap.
///
/// Each property comes from the census, with counts for dictionaries that emit
/// it: `fontWeight` (24 dictionaries, 846 326 nodes), `fontSize`
/// (18 dictionaries, 772 971 nodes), `verticalAlign` (18 dictionaries,
/// 632 811 nodes), and `color` (8 dictionaries, 380 317 nodes). The rare
/// `fontStyle` case has 6 dictionaries and 238 nodes. `sup` and `sub` get
/// their rise and smaller size from HTML's own stylesheet. This creates a
/// non-zero `shift` and a shorter `span_box` on a taller line. The baseline
/// supplies the required arithmetic.
fn styled_spans() -> Fixture {
    let body = concat!(
        r##"{"tag":"div","content":["##,
        r##"{"tag":"span","style":{"fontWeight":"bold","color":"#c8a06e"},"##,
        r##""content":"名・自サ変"},"　","##,
        r##"{"tag":"span","style":{"fontSize":"1.2em"},"content":"胡蝶"},"##,
        r##"{"tag":"sup","content":"２"},"##,
        r##""の夢を見ること。","##,
        r##"{"tag":"span","style":{"fontStyle":"italic","fontSize":"0.85em","##,
        r##""color":"#8aa2c8"},"content":"（雅語）"},"##,
        r##"{"tag":"sub","content":"注"},"##,
        r##""　荘子の故事から出た言い方で、現実と夢との境が定かでないことのたとえ。"]}"##
    );
    let top = card(
        Some("胡蝶の夢"),
        Some("こちょうのゆめ"),
        &["noun"],
        Some(24810),
        vec![tree("明鏡国語辞典 第三版", 31, &[&sc(body)], "", &[])],
        4,
    );
    let mut spec = Spec::plain(present(Some(top), vec![]));
    // Use a narrow width so the mixed styles share wrapped lines.
    spec.max_w = 340;
    Fixture { name: "styled_spans", variants: vec![("default", spec)] }
}

/// 9. Box records: a stylesheet pill and an inline bordered block.
///
/// This fixture tests scope correction. The census shows margin, padding,
/// border style, border width, and border radius in 11 to 14 of 72 dictionaries.
/// Those dictionaries use them to draw pills.
///
/// Use two variants because the corpus has two mechanisms with little overlap.
/// Twenty-nine structured dictionaries write boxes inline. Thirteen write them
/// only in `styles.css`. All 14 stylesheet Dictionaries draw boxes in CSS.
///
/// - `css` is Jitendex's own pill, `span[data-sc-class="tag"]` with a radius,
///   padding, and margin across 48 776 sampled nodes. It is an inline box, so it
///   keeps its place on a line instead of breaking it.
/// - `inline` covers the other mechanism with three answers in one tree. Its
///   first `div` is a bordered block. Its box contains **both** paragraphs that
///   the block emits. A browser draws this box, so the layout creates a textless
///   `Block` element before the runs that it frames. The inner `span` has a
///   `data.content` marker and a box with only padding and a right margin. It
///   opens a line as the sense separator, then resolves to **no box drawn**.
///   An earlier marker node became a block box and indented its line by its
///   padding. The layout still reserves that padding and margin. The inline box
///   uses spacer spans in `ui::layout`'s `pill` pass, so it costs line advance,
///   not pixels.
///   The second `div` uses the same block with the marker span first. This shape
///   once lost its box because the block flushed an empty paragraph before it
///   could carry one. It now draws the same single box as the first. See the
///   fixture-set note in `ARCHITECTURE.md#verification`.
fn bordered_pill() -> Fixture {
    let jitendex = concat!(
        r#"{"tag":"div","data":{"content":"glossary"},"content":["#,
        r#"{"tag":"span","data":{"class":"tag"},"content":"noun"},"#,
        r#"{"tag":"span","data":{"class":"tag"},"content":"common"},"#,
        r#"{"tag":"span","content":"a butterfly's dream; an illusion no one can "#,
        r#"tell from the waking world"}]}"#
    );
    let jitendex_css = concat!(
        "span[data-sc-class=\"tag\"] {\n",
        "    font-size: 0.8em;\n",
        "    border-radius: 0.3em;\n",
        "    padding: 0.2em 0.4em;\n",
        "    margin-right: 0.25em;\n",
        "    background-color: #565656;\n",
        "    vertical-align: text-bottom;\n",
        "}\n",
        "div[data-sc-content=\"glossary\"] { margin-bottom: 0.5rem }\n"
    );
    let css = Spec::plain(present(
        Some(card(
            Some("胡蝶の夢"),
            Some("こちょうのゆめ"),
            &["expression"],
            Some(24810),
            vec![tree("Jitendex.org", 11, &[&sc(jitendex)], jitendex_css, &[])],
            4,
        )),
        vec![],
    ));

    let inline_body = concat!(
        r##"[{"tag":"div","style":{"margin":0.4,"padding":0.2,"borderWidth":0.2,"##,
        r##""borderStyle":"solid","borderColor":"#7f8c99","borderRadius":0.4,"##,
        r##""backgroundColor":"#1e3a5f"},"content":["注：","##,
        r##"{"tag":"span","data":{"content":"misc-info"},"##,
        r##""style":{"padding":0.2,"marginRight":0.3},"content":"colloquial"},"##,
        r##"" a bordered block whose box wraps both paragraphs it emits"]},"##,
        r##"{"tag":"div","style":{"padding":0.2,"borderWidth":0.2,"borderStyle":"solid","##,
        r##""borderColor":"#7f8c99","backgroundColor":"#24313d"},"content":["##,
        r##"{"tag":"span","data":{"content":"misc-info"},"style":{"padding":0.2},"##,
        r##""content":"dated"},"##,
        r##"" and a block whose first child opens a line of its own"]}]"##
    );
    let inline = Spec::plain(present(
        Some(card(
            Some("雑談"),
            Some("ざつだん"),
            &["noun"],
            Some(7671),
            vec![tree("三省堂国語辞典 第八版", 23, &[&sc(inline_body)], "", &[])],
            2,
        )),
        vec![],
    ));

    Fixture { name: "bordered_pill", variants: vec![("css", css), ("inline", inline)] }
}

/// 10. List markers with two levels.
///
/// The census has `li` in 20 of 72 dictionaries with 838 412 nodes. It has
/// `ul` in 19 and `ol` in 4. A marker uses the gutter that its list already
/// paid for. Its `x` is negative, and it is outside the element text. The
/// snapshot must hold this geometry because `rect` does not retain it.
///
/// - `jitendex` is the real tree from Jitendex's record:
///   a `ul[sense-groups]` with `li[sense-group]` children. Each child has a
///   bare `ol` with `li[sense]` children. Each sense has a `ul[glossary]`.
///   Its two list rules use CSS only, and the second uses native `&` syntax.
///   An item whose full content is a nested list gives the two levels one line
///   between them. The element then carries **two** markers in two gutters, as
///   a browser draws them. It also carries the sense's example pair and the
///   entry's attribution line. This is the *other* half of Jitendex's real
///   record and the only place a golden sees the role-filter default:
///   `RoleFilter::CARD` keeps examples and attributions. The deleted six-name
///   drop list hid 40 975 nodes, all from the six Jitendex names it dropped.
/// - `plain` uses the default ladder without a stylesheet. It has a `ul` bullet,
///   an `ol` number, and the indent that each level adds.
fn nested_list() -> Fixture {
    let senses = concat!(
        r#"{"tag":"ul","data":{"content":"sense-groups"},"content":["#,
        r#"{"tag":"li","data":{"content":"sense-group"},"content":["#,
        r#"{"tag":"ol","content":["#,
        r#"{"tag":"li","data":{"content":"sense"},"style":{"listStyleType":"\"①\""},"#,
        r#""content":[{"tag":"ul","data":{"content":"glossary"},"content":["#,
        r#"{"tag":"li","content":"the butterfly dream"},"#,
        r#"{"tag":"li","content":"an illusion no one can tell from waking life"}]},"#,
        r#"{"tag":"div","data":{"content":"example-sentence"},"content":["#,
        r#"{"tag":"div","data":{"content":"example-sentence-a"},"#,
        r#""content":"胡蝶の夢を見た。"},"#,
        r#"{"tag":"div","data":{"content":"example-sentence-b"},"#,
        r#""content":"I dreamt the butterfly dream."}]}]},"#,
        r#"{"tag":"li","data":{"content":"sense"},"style":{"listStyleType":"\"②\""},"#,
        r#""content":[{"tag":"ul","data":{"content":"glossary"},"content":["#,
        r#"{"tag":"li","content":"the transience of a human life"}]}]}]}]},"#,
        r#"{"tag":"div","data":{"content":"attribution"},"#,
        r#""content":"Jitendex.org (CC BY-SA 4.0)"}]}"#
    );
    let senses_css = concat!(
        "ul[data-sc-content=\"sense-groups\"] { list-style-type: \"＊\" }\n",
        "li[data-sc-content=\"sense\"] {\n",
        "    & ul[data-sc-content=\"glossary\"] { list-style-type: none }\n",
        "}\n"
    );
    let jitendex = Spec::plain(present(
        Some(card(
            Some("胡蝶の夢"),
            Some("こちょうのゆめ"),
            &["expression", "noun"],
            Some(24810),
            vec![tree("Jitendex.org", 11, &[&sc(senses)], senses_css, &[])],
            4,
        )),
        vec![],
    ));

    let bare = concat!(
        r#"{"tag":"ul","content":[{"tag":"li","content":"raw; uncooked"},"#,
        r#"{"tag":"li","content":["live, of a broadcast","#,
        r#"{"tag":"ol","content":[{"tag":"li","content":"unedited"},"#,
        r#"{"tag":"li","content":"not recorded beforehand"}]}]},"#,
        r#"{"tag":"li","content":"draught, of beer"}]}"#
    );
    let plain = Spec::plain(present(
        Some(card(
            Some("生"),
            Some("なま"),
            &["noun"],
            Some(1204),
            vec![tree("大辞林", 3, &[&sc(bare)], "", &[])],
            1,
        )),
        vec![],
    ));

    Fixture {
        name: "nested_list",
        variants: vec![("jitendex", jitendex), ("plain", plain)],
    }
}

/// 11. A table with both span types.
///
/// The census has `table` in 16 of 72 dictionaries, `td` in 18, `th` in 11,
/// and `tbody` in 1. It has `rowSpan` in 7 over 116 813 nodes and `colSpan` in
/// 2 over 4 597 nodes.
/// A conjugation table is the shape that the pinned fixture list names. It is
/// the only place where three `ElemKind`s meet. A `Table` precedes its `Cell`s
/// in draw order, so the grid background stays below them. Each cell's
/// paragraphs follow as addressed runs.
///
/// Place both spans in one grid. A `colSpan` header above a `rowSpan` body cell
/// exposes the placement rule: the pass skips columns that earlier `rowSpan`
/// claims still occupy and chooses the leftmost free column. A wrong choice
/// moves a cell's `x` instead of removing the feature.
fn table_spans() -> Fixture {
    let grid = concat!(
        r#"{"tag":"table","content":{"tag":"tbody","content":["#,
        r#"{"tag":"tr","content":[{"tag":"th","colSpan":2,"content":"活用形"},"#,
        r#"{"tag":"th","content":"飲む"}]},"#,
        r#"{"tag":"tr","content":[{"tag":"td","rowSpan":2,"content":"五段"},"#,
        r#"{"tag":"td","content":"未然形"},{"tag":"td","content":"飲ま・飲も"}]},"#,
        r#"{"tag":"tr","content":[{"tag":"td","content":"連用形"},"#,
        r#"{"tag":"td","content":"飲み・飲ん"}]},"#,
        r#"{"tag":"tr","content":[{"tag":"td","colSpan":3,"content":"「飲む」は五段活用。"}]}"#,
        r#"]}}"#
    );
    let top = card(
        Some("飲む"),
        Some("のむ"),
        &["godan verb", "transitive verb"],
        Some(640),
        vec![tree("旺文社漢字典 第四版", 41, &[&sc(grid)], "", &[])],
        2,
    );
    let mut spec = Spec::plain(present(Some(top), vec![]));
    spec.max_h = 700;
    Fixture { name: "table_spans", variants: vec![("default", spec)] }
}

/// 12. Readings above their bases.
///
/// The census has `ruby` in 14 of 72 dictionaries with 864 045 nodes and `rt`
/// with 864 054 nodes. chibipop once deleted these nodes. A reading uses no
/// horizontal room from its line. It is outside the element text and spans.
/// The layout places and draws it at its assigned position, so the snapshot is
/// its only record.
///
/// **Two term-bank rows under one dictionary label** form a shape that no other
/// fixture has. The census found 6 220 大辞林 headwords with more than one row,
/// with a maximum of eleven. The panel once repeated the dictionary name for
/// each row. These rows mean one label with two gloss bodies.
///
/// The second row's `斉` has a six-mora reading above a one-character base. The
/// reading extends beyond the base on both sides. This checks center placement.
/// Its `rt` also has a style because a reading takes `fontSize` and `color`.
fn ruby_run() -> Fixture {
    let first = sc(concat!(
        r#"[{"tag":"ruby","content":["胡蝶",{"tag":"rp","content":"（"},"#,
        r#"{"tag":"rt","content":"こちょう"},{"tag":"rp","content":"）"}]},"#,
        r#""の夢。","#,
        r#"{"tag":"ruby","content":["現",{"tag":"rt","content":"うつつ"}]},"#,
        r#""と夢の","#,
        r#"{"tag":"ruby","content":["境",{"tag":"rt","content":"さかい"}]},"#,
        r#""が定かでないことのたとえ。"]"#
    ));
    let second = sc(concat!(
        r#"[{"tag":"ruby","content":["荘",{"tag":"rt","content":"そう"},"#,
        r#""子",{"tag":"rt","content":"し"}]},"#,
        r#""の","#,
        r##"{"tag":"ruby","content":["斉",{"tag":"rt","style":{"fontSize":"0.6em","##,
        r##""color":"#8aa2c8"},"content":"せいぶつろん"},"物論"]},"##,
        r#""に見える説話。"]"#
    ));
    let top = card(
        Some("胡蝶の夢"),
        Some("こちょうのゆめ"),
        &["expression"],
        Some(24810),
        vec![tree("大辞林", 3, &[&first, &second], "", &[])],
        4,
    );
    let mut spec = Spec::plain(present(Some(top), vec![]));
    spec.max_w = 380;
    Fixture { name: "ruby_run", variants: vec![("default", spec)] }
}

/// 13. Inline assets with their text ladder.
///
/// `img` is the fourth most widely used tag: 30 of the 52 structured
/// dictionaries and 386 141 nodes. chibipop once dropped every one. 字通 alone
/// averages more than four per term row. These three nodes come from the
/// census representative samples and keep their source shape:
///
/// 1. A `height: 1em` GIF gaiji that uses its recorded intrinsic size.
/// 2. 三省堂's monochrome intonation SVG, which declares both axes and acts as a
///    *mask*. Do not paint it black on a dark panel.
/// 3. A gaiji SVG with no recorded row. It uses its `alt` text and puts
///    `［対義語］` in the flow.
///
/// Put all three in mid-sentence. A dropped asset removes part of a word, not
/// only an illustration. The element `rect` is the resolved image box, and
/// `image` names the asset. This fixture therefore reaches the media key in a
/// golden.
fn inline_image() -> Fixture {
    let body = concat!(
        r#"["用例：","#,
        r#"{"tag":"img","path":"24/1188db.gif","height":1.0,"sizeUnits":"em","#,
        r#""collapsible":false,"data":{"img":"","gaiji":""}},"#,
        r#""は","#,
        r#"{"tag":"img","path":"sankoku8/svg-intonation/短.svg","height":1.0,"#,
        r#""width":1.5384615384615385,"sizeUnits":"em","appearance":"monochrome","#,
        r#""background":false,"title":"短","collapsed":false,"collapsible":false},"#,
        r#""と読む。","#,
        r#"{"tag":"img","path":"gaiji/対義語.svg","background":false,"collapsed":false,"#,
        r#""collapsible":false,"data":{"gaiji":"","class":"gaiji","#,
        r#""alt":"［対義語］","src":"gaiji/対義語.svg"}},"#,
        r#""は長。"]"#
    );
    let media = [
        ("24/1188db.gif", recorded(MediaFormat::Gif, 24.0, 24.0)),
        (
            "sankoku8/svg-intonation/短.svg",
            recorded(MediaFormat::Svg, 40.0, 26.0),
        ),
    ];
    let top = card(
        Some("短"),
        Some("たん"),
        &["noun"],
        Some(1180),
        vec![tree("字通", 7, &[&sc(body)], "", &media)],
        1,
    );
    let mut spec = Spec::plain(present(Some(top), vec![]));
    spec.max_w = 380;
    Fixture { name: "inline_image", variants: vec![("default", spec)] }
}

/// 14. **One accent**: the simplest card-header pitch geometry. This fixture
///     pins the positions of an overline and a downstep tick.
///
/// `雑談 / ざつだん` uses heiban, which represents 48.0% of the census's
/// accents. The first mora is low, the other three are high, and no tick exists.
/// The golden records the overline extent without another row mark. Each mark
/// is an `inline_box` with one border edge. Fields that change supply paint for
/// marked kana.
fn pitch_single() -> Fixture {
    let mut top = card(
        Some("雑談"),
        Some("ざつだん"),
        &["noun"],
        Some(7671),
        vec![block("Jitendex", &["chatting; idle talk"])],
        2,
    );
    top.pitch = vec![pitch(0, &["NHK"])];
    Fixture {
        name: "pitch_single",
        variants: vec![("default", Spec::plain(present(Some(top), vec![])))],
    }
}

/// 15. **Several accents**: the row stack and an in-word tick.
///
/// `白目 / しろめ` has all four accents that the census combines from its five
/// pitch dictionaries. This is the corpus maximum. Only 50 of 218 783
/// expression+reading pairs reach it. Heiban, atamadaka, nakadaka, and odaka
/// appear in one capture. The golden holds one row of each shape and the gap
/// that the layout stacks between rows.
fn pitch_multiple() -> Fixture {
    let mut top = card(
        Some("白目"),
        Some("しろめ"),
        &["noun"],
        Some(31_602),
        vec![block("大辞林", &["白眼。眼球の白い部分。"])],
        2,
    );
    top.pitch = vec![
        pitch(0, &["大辞林"]),
        pitch(1, &["三省堂"]),
        pitch(2, &["新明解"]),
        pitch(3, &["NHK"]),
    ];
    Fixture {
        name: "pitch_multiple",
        variants: vec![("default", Spec::plain(present(Some(top), vec![])))],
    }
}

/// 16. **Several source dictionaries**: the panel's dedup result.
///
/// For `合縁奇縁 / あいえんきえん`, three census Dictionaries report position `5`,
/// while 大辞泉 reports heiban. The panel shows two rows: three names in one and
/// one name in the other.
/// The `agreed` variant repeats the reading with all dictionaries unanimous.
/// This covers 81.4% of readings known by at least two dictionaries and gives
/// one row with four names. This pair pins dedup because the difference is one
/// full geometry row.
///
/// The `agreed` variant uses a kana-only headword on purpose. Marked kana then
/// sits directly below the headword without a reading line. Every later element
/// gets a different `y` value.
fn pitch_sources() -> Fixture {
    let mut split = card(
        Some("合縁奇縁"),
        Some("あいえんきえん"),
        &["noun"],
        None,
        vec![block("Jitendex", &["a couple joined by a happy chance"])],
        4,
    );
    split.pitch = vec![
        pitch(5, &["三省堂", "新明解", "NHK"]),
        pitch(0, &["大辞泉"]),
    ];

    let mut agreed = card(
        None,
        Some("あいえんきえん"),
        &[],
        None,
        vec![block("Jitendex", &["a couple joined by a happy chance"])],
        7,
    );
    agreed.pitch = vec![pitch(5, &["大辞林", "大辞泉", "三省堂", "NHK"])];

    Fixture {
        name: "pitch_sources",
        variants: vec![
            ("split", Spec::plain(present(Some(split), vec![]))),
            ("agreed", Spec::plain(present(Some(agreed), vec![]))),
        ],
    }
}

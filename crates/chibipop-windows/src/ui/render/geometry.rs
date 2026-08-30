//! Geometry-snapshot capture for the layout goldens (ADR-0011, widened
//! by ADR-0013).
//!
//! A snapshot is everything `layout_pass` computes for one fixture
//! `Presentation`, as JSON with fixed-precision floats: element rects,
//! hit rects and their `HitAction`s, content height, `max_scroll`, the
//! side-panel geometry, plus the `measure()` triple the popup is sized
//! from. Capture is the measure-only walk - no window, no render
//! target, no painting - so it runs on the unmodified build.
//!
//! **ADR-0013 widened the schema.** An element now also projects its
//! styled spans, the line and span geometry the measurer reported for
//! them, its box record, its readings, its list markers, its dictionary
//! address, and its image's media key. Those fields are why the
//! widening was needed at all: `Metrics { w, h, lines }` says nothing
//! about where a smaller span sits inside a taller line, and the
//! baseline that answers it reaches no other test. After the re-bless
//! these goldens defend the DirectWrite adapter's per-range formatting
//! and its baseline reporting, neither of which anything else exercises.
//!
//! Floats stay at the same fixed precision the schema always used, and
//! the goldens keep exact equality - see [`px`].
//!
//! The fixtures live here, not in the test, so the golden test and the
//! `CHIBIPOP_BLESS=1` bless path share one code path (ADR-0011: capture
//! and verify must not drift apart).

use super::*;
use crate::dict::media::{Intrinsic, MediaFormat};
use crate::dict::sheet;
use crate::geom::PhysRect;
use crate::present::{AnkiPopupState, Card, CollapsedRow, GlossBlock, GlossEntry};
use crate::text::layout::TextGeom;
use crate::text::TextSpan;
use crate::ui::layout::{BoxStyle, Edges, ElemBox, GlossOrigin, MarkerBox, Rgb, RubyBox};
use serde_json::{json, Value};

/// One capture's full input.
pub struct Spec {
    pub theme: Theme,
    pub p: Presentation,
    /// Physical px, like `measure`.
    pub max_w: i32,
    pub max_h: i32,
    pub show_back: bool,
    pub side_panel: bool,
    /// Physical px; clamped to
    /// `max_scroll` like the app does.
    pub scroll: i32,
    pub anki: AnkiPopupState,
    /// Source-text geometry for the
    /// match highlight, if the
    /// fixture exercises it.
    pub span: Option<TextSpan>,
}

impl Spec {
    /// The common case: dark theme,
    /// default popup box, no chrome.
    fn plain(p: Presentation) -> Spec {
        Spec {
            theme: Theme::dark(),
            p,
            // 1920x1080 at the default
            // 25% x 45% popup config.
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

/// A named fixture: one golden file,
/// one or more labelled captures.
pub struct Fixture {
    pub name: &'static str,
    pub variants: Vec<(&'static str, Spec)>,
}

/// The thirteen ADR-0011 fixtures.
///
/// Seven from the original set, whose
/// intent is unchanged, then the six
/// ADR-0013 requires: the widened
/// schema has fields no plain-string
/// fixture can ever fill, and an
/// unfilled field gates nothing.
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
    ]
}

/// Captures every variant of `f`.
///
/// The golden file's whole content.
pub fn snapshot(f: &Fixture) -> Result<Value> {
    let mut variants = serde_json::Map::new();
    for (label, spec) in &f.variants {
        variants.insert((*label).to_string(), capture(f.name, label, spec)?);
    }
    Ok(json!({ "fixture": f.name, "variants": Value::Object(variants) }))
}

/// Stable golden serialization.
pub fn to_json_text(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("geometry snapshots are plain JSON trees");
    s.push('\n');
    s
}

/// One measure-only capture.
///
/// Builds the scene through the same
/// `Text` engine paint uses, then
/// projects it onto the golden's
/// schema. Nothing here decides
/// geometry: it only reads it.
fn capture(fixture: &str, variant: &str, spec: &Spec) -> Result<Value> {
    let text = Text::new()?;
    // No window: the capture is
    // defined at 96 dpi. The runner
    // has no HWND to ask anyway.
    let scale = 1.0f32;

    // The shipped render settings, so
    // a golden pins what a fresh
    // install draws. A fixture is not
    // where a knob is exercised: the
    // render settings are asserted
    // through `layout::scene` with the
    // layout fake, which needs no font
    // stack (`src/ui/layout/tests.rs`).
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

    // paint_once's thumb, off the same
    // scene method it calls, so the
    // golden records the derivation the
    // renderer performs rather than a
    // second copy of it.
    let thumb = scene
        .scrollbar_thumb(spec.theme.padding, view_h, scroll_dip)
        .map(|(top, h)| json!({ "top": top, "h": h }))
        .unwrap_or(Value::Null);

    let mut elements: Vec<Value> = Vec::with_capacity(scene.elems.len());
    for (i, e) in scene.elems.iter().enumerate() {
        elements.push(elem_json(i, e, &text, &spec.theme, scale)?);
    }

    // The main column only, as before:
    // the side column's targets are
    // covered by `side_panel` above.
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

/// One element, whole.
///
/// `pen` rides along with the rect
/// because every field ADR-0013 added
/// below it - the readings, the
/// markers, and the measurer's own
/// line and span boxes - is
/// *run-relative*, and a run-relative
/// coordinate with no origin beside it
/// is a number a reviewer cannot check.
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

/// What the measurer said about this
/// element's run, asked again.
///
/// The scene carries no [`LineBox`] and
/// no [`SpanBox`] - it keeps the
/// stacked result, not the detail - so
/// a baseline and a span's share of its
/// line reach the golden only by asking
/// the adapter. This is still reading
/// rather than deciding: the request is
/// the one [`SceneElem::styled_spans`]
/// exists to rebuild, at the width the
/// scene reports, which is exactly what
/// `paint_once` re-measures at draw
/// time. If it answered differently
/// here the panel's ink would not match
/// its own hit rects.
///
/// `null` for an element with no spans:
/// a rule, a block's own box, a
/// table's own box, and a cell's
/// border have no text to wrap.
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

/// A positioned run that is not in its
/// element's flow.
///
/// A reading and a list marker carry
/// the same nine fields and neither is
/// a span of the text the measurer
/// wrapped, so they project through one
/// shape rather than two that could
/// drift.
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

/// One box to fill and stroke.
///
/// The declared widths, not the used
/// ones: `border_used` is derived from
/// these two fields and the layout
/// tests already pin the derivation, so
/// putting it here would only make one
/// change diff twice.
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

/// Four edges in CSS's own order:
/// top, right, bottom, left.
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

/// Where in its dictionary an element
/// came from.
///
/// The path is its child indices,
/// outermost first - the identity a
/// sense picker selects on, and `null`
/// for a node too deep to address.
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

/// The asset an image element names.
///
/// The key and the format, not the
/// bytes: core resolved the rect from
/// the recorded intrinsic size, and the
/// rect is the element's own.
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

/// Two decimals, as a string: the
/// golden compares text, never
/// reparsed floats. `-0.0` becomes
/// `0.0` so the sign of nothing
/// can't diff.
fn px(v: f32) -> Value {
    let r = (f64::from(v) * 100.0).round() / 100.0;
    let r = if r == 0.0 { 0.0 } else { r };
    Value::String(format!("{r:.2}"))
}

fn rect_json(r: PhysRect) -> Value {
    json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })
}

// ---- fixtures (ADR-0011 §Fixture set) ----

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
    }
}

/// The layout pass renders each row's parsed tree, so a geometry fixture
/// carries the tree its own strings parse to - a bare glossary string per
/// item, the shape 20 of the census's 72 dictionaries emit. The blessed
/// geometry is unchanged by that: the inline pass joins one row's items
/// with the same `; ` the panel always drew and coalesces them into one
/// styled span, so the seam gets the request it got before the pass
/// existed.
///
/// One block, one matched term-bank row: the geometry the goldens hold is
/// one dictionary label with one gloss body under it. The six structured
/// fixtures below use [`tree`] instead, because none of this can express
/// a second style on a line.
fn block(dict: &str, glosses: &[&str]) -> GlossBlock {
    GlossBlock::parse(dict, &serde_json::json!(glosses).to_string())
}

/// One structured-content item wrapping `content`, as a term bank stores
/// a row.
fn sc(content: &str) -> String {
    format!(r#"[{{"type":"structured-content","content":{content}}}]"#)
}

/// One dictionary's whole contribution, structured: its own rows, its own
/// `styles.css`, and the media rows its build recorded.
///
/// Five arguments because a real block is five facts and splitting them
/// into overload-shaped helpers would put the same body in three places.
/// The stylesheet fold is `dict::sheet`'s and the hover path runs it
/// between the parse and the tree cache (`SqliteDictionary::entries`), so
/// a fixture reproduces a real hover by calling the same two functions -
/// and everything below this line has no idea CSS was involved.
///
/// `dict_id` is a real number rather than `NO_ROW` wherever the fixture
/// carries media or wants a checkable [`GlossOrigin`]: an element's media
/// key and its dictionary address are both built from it, and zero would
/// hide a mix-up between them.
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
                // Rows of one dictionary are numbered as a term bank
                // numbers them, so a snapshot names which row an element
                // came from and not merely which dictionary.
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

/// One recorded media row, as `dict::media::probe` writes it.
///
/// A path and four numbers: the sizing pass reads what the build recorded
/// rather than the bytes, so an image fixture needs no archive, no
/// decoder and no database.
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

/// 30px char boxes from x=100, like
/// present.rs's own span fixtures.
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

/// 1. The wrap loop and gap stacking.
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
    // Narrow: every gloss wraps.
    spec.max_w = 360;
    spec.max_h = 700;
    Fixture { name: "wrapping_heavy", variants: vec![("default", spec)] }
}

/// 2. The same rows, both modes.
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
            // Headless: the panel path
            // skips it, inline keeps it.
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

/// 3. Overflow: `max_scroll`, thumb.
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

/// 4. `HIGHLIGHT_PAD` vs glyph boxes.
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

/// 5. Every `HitAction`'s rect: back
///    button, drill-down kanji, expand
///    rows, plus corner and Anki label.
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

/// 6. Degenerate gap paths.
fn minimal_edge() -> Fixture {
    // No reading, no PoS, unranked,
    // and a block with no glosses:
    // label drawn, body skipped.
    let sparse = Spec::plain(present(
        Some(card(Some("猫"), None, &[], None, vec![block("大辞林", &[])], 1)),
        vec![],
    ));
    // Nothing at all: the popup the
    // renderer sizes to bare padding.
    let empty = Spec::plain(present(None, vec![]));
    Fixture { name: "minimal_edge", variants: vec![("sparse", sparse), ("empty", empty)] }
}

/// 7. Everything at once.
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
            // Empty glosses: the label
            // stands alone.
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

// ---- the six ADR-0013 added ----
//
// Authored, not captured. ADR-0011's capture property - "capturable from
// the unmodified pre-refactor build" - does not reach styled content,
// because the unmodified build had no styled spans to capture. Every
// tree below is a real corpus shape out of `docs/research/dict-shapes.md`
// rather than an invented one, so a golden that moves says something
// about a dictionary somebody owns.

/// 8. Mixed styling inside one wrapped paragraph.
///
/// The single fact ADR-0013 exists to change: before it a run boundary
/// was always a line boundary, so bold text and normal text could not
/// share a wrapped line. This paragraph makes five style changes without
/// a break, and the narrow box forces the mix to wrap.
///
/// Every property in it is a census property, ranked by dictionaries that
/// emit it: `fontWeight` (24 dictionaries, 846 326 nodes), `fontSize`
/// (18 / 772 971), `verticalAlign` (18 / 632 811) and `color` (8 /
/// 380 317), plus `fontStyle` (6 / 238) for the rare case. `sup` and
/// `sub` take their rise and their smaller size from HTML's own
/// stylesheet, which is what puts a non-zero `shift` and a shorter
/// `span_box` height on a taller line - the arithmetic that has no answer
/// without a baseline.
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
    // Narrow enough that the mixed
    // styles have to share lines.
    spec.max_w = 340;
    Fixture { name: "styled_spans", variants: vec![("default", spec)] }
}

/// 9. The box record: a pill from a stylesheet, a bordered block inline.
///
/// The census's single largest scope correction. Margin, padding, border
/// style, border width and border radius each appear in 11 to 14 of 72
/// dictionaries, and that is how those dictionaries draw their pills.
///
/// Two variants because the corpus has two mechanisms and they barely
/// overlap - 29 structured dictionaries write their boxes inline, 13
/// write them only in `styles.css`, and by the question that matters all
/// 14 stylesheet dictionaries draw their pill in CSS:
///
/// - `css` is Jitendex's own pill, `span[data-sc-class="tag"]` with a
///   radius, a padding and a margin over 48 776 sampled nodes. It is an
///   inline box: a pill keeps its place on its line rather than breaking
///   one.
/// - `inline` is the other half, three answers in one tree. Its first
///   `div` is a genuine bordered block, and its box is a container
///   around **both** paragraphs that block emits - which is what a
///   browser draws, and which is why the box arrives as a textless
///   `Block` element ahead of the runs it frames. The `span` inside it
///   carries a `data.content` marker *and* a box that only spaces
///   (padding and a right margin, no fill and no border), so it opens a
///   line - that is ticket 01's sense separator - and then resolves to
///   **no box drawn**, where before ticket 15's fix a marker-carrying
///   node became a block box and indented its own line by its padding.
///   It does still *reserve* that padding and margin, as an inline box
///   in a browser does: the room is spacer spans in the run itself
///   (`ui::layout`'s `pill` pass), so what a box with no ink costs the
///   line is advance and not pixels.
///   The second `div` is the same block with the marker span *first*:
///   the shape that used to lose its box entirely, because the
///   paragraph the block opened was flushed empty before it could carry
///   one. It now draws the same single box as the first. See the
///   fixture-set note in
///   `docs/adr/0011-layout-golden-verification.md`.
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

/// 10. Hanging list markers, two levels deep.
///
/// `li` is in 20 of the census's 72 dictionaries over 838 412 nodes and
/// `ul` in 19; `ol` in 4. A marker hangs in the gutter its list already
/// paid for, so its `x` is negative and it is not in the element's text -
/// which is precisely the geometry a snapshot has to hold, because
/// nothing about it survives into `rect`.
///
/// - `jitendex` is the real tree read out of Jitendex's own record: a
///   `ul[sense-groups]` of `li[sense-group]`, each holding a bare `ol` of
///   `li[sense]` whose inline `listStyleType` numbers the sense, each of
///   those holding a `ul[glossary]`. Its two list rules are CSS-only, and
///   the second is written with native `&` nesting. An item whose whole
///   content is a nested list gives the two levels one line between them,
///   so that element carries **two** markers hanging in two gutters -
///   what a browser draws. It also carries the sense's own example pair
///   and the entry's attribution line, which is the *other* half of
///   Jitendex's real record and the only place a golden sees ticket 15's
///   default: `RoleFilter::CARD` keeps examples and attributions, where
///   the deleted six-name drop list hid 40 975 nodes of exactly this
///   shape, all of them Jitendex's.
/// - `plain` is the default ladder with no stylesheet at all: a `ul`'s
///   bullet, an `ol`'s number, and the indent each level adds.
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

/// 11. A table carrying both spans.
///
/// `table` is in 16 of 72 dictionaries, `td` in 18, `th` in 11, `tbody`
/// in 1; `rowSpan` in 7 over 116 813 nodes and `colSpan` in 2 over 4 597.
/// A conjugation table is the shape the spec's own fixture list names,
/// and it is the one place three `ElemKind`s meet: a `Table` leading its
/// `Cell`s in draw order so the grid's background sits under them, and
/// each cell's paragraphs following it as their own addressed runs.
///
/// Both spans in one grid on purpose. A `colSpan` header over a
/// `rowSpan` body cell is what makes the placement pass's "leftmost
/// column no span from above has claimed" rule observable: get it wrong
/// and a cell lands in the wrong column, which is a moved `x` and not a
/// missing feature.
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

/// 12. Readings, positioned over their bases.
///
/// `ruby` is in 14 of 72 dictionaries over 864 045 nodes and `rt` over
/// 864 054, and chibipop used to delete every one of them. A reading
/// takes no horizontal room from the line it sits on, so it is neither in
/// the element's text nor one of its spans: it is placed once and drawn
/// where it is told, which makes the snapshot the only record of where
/// that is.
///
/// **Two term-bank rows under one dictionary label**, which is ticket
/// 16's shape and no other fixture holds it: the census found 6 220 大辞林
/// headwords with more than one row and a worst case of eleven, and the
/// panel used to repeat the dictionary's name once per row. Rows here
/// mean one label with two gloss bodies under it.
///
/// The second row's `斉` carries a six-mora reading over a one-character
/// base, so the reading overhangs its base on both sides and the
/// centring has something to get wrong; its `rt` is also styled, because
/// a reading takes `fontSize` and `color` like anything else.
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

/// 13. Assets composited inline, and the ladder under them.
///
/// `img` is the fourth most widely used tag - 30 of the 52 structured
/// dictionaries, 386 141 nodes - and chibipop dropped every one. 字通
/// alone averages more than four per term row. The three nodes here are
/// the census's own representative samples, verbatim in shape:
///
/// 1. a `height: 1em` GIF gaiji, sized from its recorded intrinsic size;
/// 2. 三省堂's monochrome intonation SVG, which declares both axes and is
///    a *mask* to be tinted with the body colour rather than painted
///    black on a dark panel;
/// 3. a gaiji SVG with no recorded row at all, which falls to its `alt`
///    text and puts `［対義語］` in the flow.
///
/// All three sit mid-sentence, which is the point: dropping them does not
/// lose an illustration, it punches holes in words. The element's own
/// `rect` is the resolved image box and its `image` names the asset, so
/// this fixture is where the media key reaches a golden.
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

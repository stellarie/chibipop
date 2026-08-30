//! Geometry-snapshot capture for the layout goldens (ADR-0011).
//!
//! A snapshot is everything `layout_pass` computes for one fixture
//! `Presentation`, as JSON with fixed-precision floats: element rects,
//! hit rects and their `HitAction`s, content height, `max_scroll`, the
//! side-panel geometry, plus the `measure()` triple the popup is sized
//! from. Capture is the measure-only walk - no window, no render
//! target, no painting - so it runs on the unmodified build.
//!
//! The fixtures live here, not in the test, so the golden test and the
//! `CHIBIPOP_BLESS=1` bless path share one code path (ADR-0011: capture
//! and verify must not drift apart).

use super::*;
use crate::geom::PhysRect;
use crate::present::{AnkiPopupState, Card, CollapsedRow, GlossBlock};
use crate::text::layout::TextGeom;
use crate::text::TextSpan;
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

/// The seven ADR-0011 fixtures.
pub fn fixtures() -> Vec<Fixture> {
    vec![
        wrapping_heavy(),
        side_panel_fixture(),
        scrolled(),
        match_highlight_fixture(),
        full_chrome(),
        minimal_edge(),
        kitchen_sink(),
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

    // paint_once's thumb inputs, at
    // this capture's window size.
    let track_h = view_h - 2 * spec.theme.padding;
    let total = scene.used_h.ceil() as i32 + 2 * spec.theme.padding;
    let thumb = layout::scrollbar_thumb(track_h, total, view_h, scroll_dip)
        .map(|(top, h)| json!({ "top": top, "h": h }))
        .unwrap_or(Value::Null);

    let elements: Vec<Value> = scene
        .elems
        .iter()
        .enumerate()
        .map(|(i, e)| {
            json!({
                "i": i,
                "kind": e.kind.as_str(),
                "text": e.text,
                "font_size": px(e.font_size * scale),
                "top_gap": px(e.top_gap * scale),
                "wrap_w": px(e.wrap_w * scale),
                "x": px(e.rect.x * scale),
                "y": px(e.rect.y * scale),
                "w": px(e.rect.w * scale),
                "h": px(e.rect.h * scale),
                "lines": e.lines,
                "advance": px(e.advance * scale),
            })
        })
        .collect();

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
/// existed. Authoring fixtures that *do* carry structure is ticket 13's,
/// alongside the re-bless.
///
/// One block, one matched term-bank row: the geometry the goldens hold is
/// one dictionary label with one gloss body under it.
fn block(dict: &str, glosses: &[&str]) -> GlossBlock {
    GlossBlock::parse(dict, &serde_json::json!(glosses).to_string())
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

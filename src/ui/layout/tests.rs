//! Layout against fixed metrics (ADR-0011, layer one).
//!
//! `FakeMeasure` wraps at a whole number of pixels per UTF-16 unit, so
//! every expectation below is arithmetic a reader can redo by hand. No
//! font, no platform: these run in both CI jobs, forever.

use super::*;
use crate::dict::gloss::{render_html, RoleFilter, Selection, Tag};
use crate::present::{Card, CollapsedRow, GlossBlock, GlossEntry};

/// Advance per UTF-16 unit, as a
/// fraction of the font size.
const ADVANCE: f32 = 0.5;
/// Line height, likewise.
const LINE_H: f32 = 2.0;

/// A text engine with no fonts.
///
/// One rectangle per UTF-16 unit,
/// wrapped greedily. Records every
/// span it was asked for, so a test
/// can assert what layout measured
/// and at what width.
#[derive(Default)]
struct FakeMeasure {
    /// Every span asked for, in order.
    asked: Vec<Asked>,
}

/// One span a measurer was handed.
///
/// A `StyledSpan` minus the font and
/// the colour, plus its run's width: a
/// test asserts what layout asked for,
/// at what width, in what weight and
/// style.
#[derive(Debug, Clone, PartialEq)]
struct Asked {
    text: String,
    size: f32,
    weight: u16,
    italic: bool,
    max_w: f32,
}

/// One span's piece of one line.
struct Frag {
    span: usize,
    line: usize,
    /// Pen at its start, in its line.
    x: f32,
    /// UTF-16 units before it, over
    /// the whole run.
    from: usize,
    /// Units in it.
    units: usize,
    /// One unit's width.
    advance: f32,
    /// The span's own line advance.
    h: f32,
}

/// Characters a real shaper gives a
/// glyph of zero advance.
///
/// Probed against cosmic-text rather
/// than assumed: a `\u{2060}` between
/// two kanji shaped to `w 0` and still
/// set its line's height from its own
/// size, which is the whole basis of
/// the ruby filler. A fake that
/// charged it a full unit would
/// mismeasure precisely the thing it
/// exists to model.
fn zero_advance(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| matches!(c, '\u{2060}' | '\u{200b}'))
}

/// The fake's greedy wrap.
///
/// Lays one rectangle per UTF-16 unit
/// end to end and breaks when the next
/// would not fit, so every expectation
/// below stays arithmetic a reader can
/// redo by hand. A one-span run
/// reduces to `floor(max_w / advance)`
/// units per line, which is what this
/// fake always did.
fn wrap(run: MeasureRun<'_>) -> (Vec<Frag>, Measured) {
    let max_w = run.max_w.max(1.0);
    let mut frags = Vec::new();
    let (mut line, mut x, mut from) = (0usize, 0.0f32, 0usize);
    for (span, s) in run.spans.iter().enumerate() {
        let advance = if zero_advance(s.text) { 0.0 } else { s.size * ADVANCE };
        let h = s.size * LINE_H;
        let mut left = s.text.encode_utf16().count();
        loop {
            // A span that advances
            // nothing always fits, and
            // never zero at the head of
            // a line: a measurer that
            // cannot wrap narrower than
            // one glyph overflows rather
            // than loops.
            let mut room = if advance <= 0.0 {
                left
            } else {
                ((max_w - x) / advance).floor().max(0.0) as usize
            };
            if room == 0 && x == 0.0 {
                room = 1;
            }
            let take = left.min(room);
            if take > 0 {
                frags.push(Frag { span, line, x, from, units: take, advance, h });
                x += take as f32 * advance;
                from += take;
                left -= take;
            }
            if left == 0 {
                break;
            }
            line += 1;
            x = 0.0;
        }
    }

    let mut out = Measured::default();
    for f in &frags {
        while out.lines.len() <= f.line {
            out.lines.push(LineBox::default());
        }
        let w = f.units as f32 * f.advance;
        let l = &mut out.lines[f.line];
        l.w = l.w.max(f.x + w);
        l.h = l.h.max(f.h);
        out.spans.push(SpanBox {
            span: f.span as u32,
            line: f.line as u32,
            x: f.x,
            w,
            h: f.h,
        });
    }
    // An empty run is one empty line,
    // not none: the walk stacks the gap
    // after it either way.
    if out.lines.is_empty() {
        let size = run.spans.first().map_or(0.0, |s| s.size);
        out.lines.push(LineBox { h: size * LINE_H, ..LineBox::default() });
    }
    let mut y = 0.0;
    for l in &mut out.lines {
        l.y = y;
        // Every span on a line shares
        // one baseline, an ascent of the
        // tallest span's own size above
        // the line's floor.
        l.baseline = l.h / LINE_H;
        y += l.h;
    }
    out.metrics = Metrics {
        w: out.lines.iter().fold(0.0f32, |a, l| a.max(l.w)),
        h: y,
        lines: out.lines.len() as u32,
    };
    (frags, out)
}

impl TextMeasure for FakeMeasure {
    fn measure(
        &mut self,
        run: MeasureRun<'_>,
        out: &mut Measured,
    ) -> Result<(), MeasureError> {
        for s in run.spans {
            self.asked.push(Asked {
                text: s.text.to_string(),
                size: s.size,
                weight: s.weight,
                italic: s.italic,
                max_w: run.max_w,
            });
        }
        *out = wrap(run).1;
        Ok(())
    }

    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError> {
        let (frags, m) = wrap(run);
        // Past the end - which core
        // never asks for, since every
        // offset it probes is the start
        // of a kanji it just walked -
        // answers the pen after the last
        // unit.
        let after = frags.last().map_or_else(GlyphBox::default, |f| GlyphBox {
            x: f.x + f.units as f32 * f.advance,
            y: m.lines[f.line].y,
            w: 0.0,
            h: f.h,
        });
        for &o in at {
            let o = o as usize;
            let found = frags.iter().find(|f| (f.from..f.from + f.units).contains(&o));
            out.push(found.map_or(after, |f| GlyphBox {
                x: f.x + (o - f.from) as f32 * f.advance,
                y: m.lines[f.line].y,
                w: f.advance,
                h: f.h,
            }));
        }
        Ok(())
    }
}

/// Refuses everything, once asked.
struct BrokenMeasure;

impl TextMeasure for BrokenMeasure {
    fn measure(&mut self, _: MeasureRun<'_>, _: &mut Measured) -> Result<(), MeasureError> {
        Err(MeasureError::new("no font"))
    }
    fn caret_boxes(
        &mut self,
        _: MeasureRun<'_>,
        _: &[u32],
        _: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError> {
        Err(MeasureError::new("no font"))
    }
}

/// One span, styled only by size.
///
/// Nothing the fake measures depends
/// on the family or the colour, so a
/// seam test names neither.
fn styled(text: &str, size: f32) -> StyledSpan<'_> {
    StyledSpan { text, font: "", size, weight: 400, italic: false, color: (0, 0, 0) }
}

/// `spans` through the seam, at `max_w`.
fn fake_measure(spans: &[StyledSpan<'_>], max_w: f32) -> Measured {
    let mut out = Measured::default();
    FakeMeasure::default()
        .measure(MeasureRun { spans, max_w }, &mut out)
        .expect("FakeMeasure never refuses a run");
    out
}

// ---- fixtures ----

/// The layout pass renders each row's parsed tree, so a fixture carries the
/// tree its strings parse to: a bare glossary string is one plain-string
/// item, which is what 20 of the census's 72 dictionaries emit and what
/// every geometry expectation below is arithmetic over. `tree` is for the
/// fixtures that need structure.
///
/// One row per block, the shape a one-hit dictionary produces; `rows` is
/// for the grouped case.
fn block(dict: &str, glosses: &[&str]) -> GlossBlock {
    rows(dict, &[glosses])
}

/// One dictionary's block, several matched term-bank rows.
fn rows(dict: &str, per_row: &[&[&str]]) -> GlossBlock {
    GlossBlock {
        dict_name: dict.to_string(),
        dict_id: crate::present::NO_ROW,
        entries: per_row.iter().map(|glosses| entry(glosses, &[])).collect(),
    }
}

/// One matched row, with its tags.
fn entry(glosses: &[&str], tags: &[&str]) -> GlossEntry {
    row_of(&serde_json::json!(glosses).to_string(), tags)
}

/// One dictionary's block, from one row's raw structured content.
fn tree(dict: &str, glossary: &str) -> GlossBlock {
    GlossBlock {
        dict_name: dict.to_string(),
        dict_id: crate::present::NO_ROW,
        entries: vec![row_of(glossary, &[])],
    }
}

/// One matched row, from the raw glossary JSON the record stores.
fn row_of(glossary: &str, tags: &[&str]) -> GlossEntry {
    let doc = std::sync::Arc::new(crate::dict::gloss::GlossDoc::parse(glossary));
    GlossEntry {
        entry_id: crate::present::NO_ROW,
        glosses: crate::dict::gloss::plain_items(&doc),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        doc,
    }
}

fn one_card(pos: &[&str], freq: Option<i64>) -> Presentation {
    let card = Card {
        written: Some("雑談".into()),
        reading: Some("ざつだん".into()),
        pos: pos.iter().map(|s| s.to_string()).collect(),
        freq,
        blocks: vec![block("Jitendex", &["chatting"])],
        match_len: 2,
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    }
}

fn with_collapsed() -> Presentation {
    let card = Card {
        written: Some("雑談".into()),
        reading: Some("ざつだん".into()),
        pos: vec![],
        freq: None,
        blocks: vec![block("Jitendex", &["chatting"])],
        match_len: 2,
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![
            CollapsedRow {
                written: Some("雑音".into()),
                reading: Some("ざつおん".into()),
                summary: "noise".into(),
            },
            CollapsedRow {
                written: Some("雑誌".into()),
                reading: Some("ざっし".into()),
                summary: "magazine".into(),
            },
        ],
        all_cards: vec![card],
        sentence: None,
    }
}

/// A scene at a known box.
fn laid_out(p: &Presentation, max_w: f32, max_h: f32, show_back: bool, side: bool) -> PopupScene {
    let theme = Theme::dark();
    let mut m = FakeMeasure::default();
    scene(
        &SceneRequest {
            presentation: p,
            theme: &theme,
            max_w,
            max_h,
            show_back,
            side_panel: side,
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run")
}

fn find(s: &PopupScene, kind: ElemKind) -> &SceneElem {
    s.elems
        .iter()
        .find(|e| e.kind == kind)
        .unwrap_or_else(|| panic!("no {} element in the scene", kind.as_str()))
}

/// Every run in the scene, in draw
/// order: what a reader sees, top to
/// bottom.
fn texts(s: &PopupScene) -> Vec<&str> {
    s.elems.iter().map(|e| e.text.as_str()).collect()
}

/// A card carrying exactly the blocks
/// the caller built, so a test can
/// state the grouped shape
/// `present::build` now produces.
fn card_with(blocks: Vec<GlossBlock>) -> Presentation {
    let card = Card {
        written: Some("雑談".into()),
        reading: None,
        pos: vec![],
        freq: None,
        blocks,
        match_len: 2,
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    }
}

/// A scene under `theme`, plus every
/// run the walk measured for it.
fn measured(theme: &Theme, p: &Presentation, side: bool) -> (PopupScene, Vec<Asked>) {
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: p,
            theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: true,
            side_panel: side,
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run");
    (s, m.asked)
}

/// A theme with no two roles alike.
///
/// Every per-role size, weight and
/// style distinct, so a run's role is
/// readable off the run itself. Only
/// `body` keeps the default 15.0, to
/// prove `reading` stopped borrowing
/// it.
fn roled_theme() -> Theme {
    Theme {
        headword_size: 21.0,
        reading_size: 17.0,
        dict_label_size: 11.0,
        collapsed_size: 12.0,
        dimmed_size: 9.0,
        frequency_size: 7.0,
        headword_weight: 700,
        reading_weight: 300,
        body_weight: 500,
        dict_label_weight: 600,
        collapsed_weight: 200,
        dimmed_weight: 100,
        frequency_weight: 800,
        reading_italic: true,
        dimmed_italic: true,
        ..Theme::dark()
    }
}

/// The run measured for `text`.
fn asked_for<'a>(runs: &'a [Asked], text: &str) -> &'a Asked {
    runs.iter()
        .find(|a| a.text == text)
        .unwrap_or_else(|| panic!("{text:?} was never measured"))
}

// ---- wrapping ----

/// The width layout offers is the
/// width the scene reports.
#[test]
fn a_run_wraps_at_the_width_layout_offered_it() {
    // padding 12 on both sides.
    let s = laid_out(&one_card(&[], None), 224.0, 4000.0, false, false);
    assert_eq!(200.0, s.content_w);
    let gloss = s.elems.iter().find(|e| e.text == "chatting").unwrap();
    assert_eq!(200.0, gloss.wrap_w);
    // 8 units at 15.0 * 0.5 = 7.5px:
    // 26 fit per line, so one line.
    assert_eq!(1, gloss.lines);
    assert_eq!(15.0 * LINE_H, gloss.rect.h);
}

#[test]
fn a_run_too_wide_for_the_column_wraps_onto_more_lines() {
    let long = "a".repeat(120);
    let p = Presentation {
        top: Some(Card {
            written: Some("猫".into()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![block("Jitendex", &[&long])],
            match_len: 1,
        }),
        collapsed: vec![],
        all_cards: vec![],
        sentence: None,
    };
    // 100px column, 7.5px per unit:
    // 13 per line, 120 units -> 10.
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = s.elems.iter().find(|e| e.text == long).unwrap();
    assert_eq!(100.0, gloss.wrap_w);
    assert_eq!(10, gloss.lines);
    assert_eq!(10.0 * 15.0 * LINE_H, gloss.rect.h);
    assert_eq!(gloss.rect.h, gloss.advance, "a text run advances by its height");
}

/// An exact fit must not spill.
#[test]
fn a_run_that_exactly_fills_the_column_stays_on_one_line() {
    let exact = "a".repeat(13);
    let p = Presentation {
        top: Some(Card {
            written: Some("猫".into()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![block("Jitendex", &[&exact])],
            match_len: 1,
        }),
        collapsed: vec![],
        all_cards: vec![],
        sentence: None,
    };
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = s.elems.iter().find(|e| e.text == exact).unwrap();
    assert_eq!(1, gloss.lines);
}

/// The corner steals width, once.
#[test]
fn the_frequency_corner_narrows_only_the_run_beside_it() {
    let s = laid_out(&one_card(&[], Some(7671)), 424.0, 4000.0, false, false);
    let corner = find(&s, ElemKind::Corner);
    assert_eq!(Align::Trailing, corner.align);
    assert_eq!(0.0, corner.advance, "the corner shares the headword's line");
    assert_eq!(
        s.origin + s.content_w - corner.rect.w,
        corner.rect.x,
        "a trailing run hugs the right edge"
    );

    let head = find(&s, ElemKind::Headword);
    assert_eq!(s.content_w - (corner.rect.w + CORNER_GAP), head.wrap_w);
    let reading = s.elems.iter().find(|e| e.text == "ざつだん").unwrap();
    assert_eq!(s.content_w, reading.wrap_w, "the reserve is spent once");
}

// ---- gap stacking ----

#[test]
fn gaps_stack_a_block_at_a_time() {
    let s = laid_out(&one_card(&["noun"], None), 424.0, 4000.0, false, false);
    let head = find(&s, ElemKind::Headword);
    assert_eq!(0.0, head.top_gap, "the first element sits on the padding");
    assert_eq!(s.origin, head.pen.1);

    let reading = s.elems.iter().find(|e| e.text == "ざつだん").unwrap();
    assert_eq!(LINE_GAP, reading.top_gap);
    assert_eq!(head.pen.1 + head.advance + LINE_GAP, reading.pen.1);

    let pos = s.elems.iter().find(|e| e.text == "noun").unwrap();
    assert_eq!(LINE_GAP, pos.top_gap);

    let label = s.elems.iter().find(|e| e.text == "Jitendex").unwrap();
    assert_eq!(SECTION_GAP, label.top_gap, "a new dictionary opens a new block");
    assert_eq!(pos.pen.1 + pos.advance + SECTION_GAP, label.pen.1);
}

#[test]
fn used_h_is_what_the_walk_stacked() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    let last = s.elems.last().unwrap();
    assert_eq!(last.pen.1 - s.origin + last.advance, s.used_h);
}

#[test]
fn content_h_is_the_body_plus_both_paddings() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    assert_eq!(s.used_h.ceil() + 2.0 * s.origin, s.content_h);
}

/// The box wins, not the content.
#[test]
fn view_h_clamps_content_to_the_box() {
    let s = laid_out(&one_card(&[], None), 424.0, 40.0, false, false);
    assert_eq!(40.0, s.view_h);
    assert!(s.content_h > s.view_h);
}

#[test]
fn a_popup_that_fits_views_its_whole_content() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    assert_eq!(s.content_h, s.view_h);
}

/// The inline separator is a rule.
#[test]
fn inline_collapsed_rows_open_with_a_separator_rule() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, false);
    let sep = find(&s, ElemKind::Separator);
    assert_eq!(SEPARATOR_MARGIN, sep.top_gap);
    assert_eq!(Theme::dark().separator_height, sep.rect.h);
    assert_eq!(s.content_w, sep.rect.w);
    assert!(sep.text.is_empty());
    assert_eq!(0, sep.lines);

    let rows: Vec<&SceneElem> =
        s.elems.iter().filter(|e| e.kind == ElemKind::Collapsed).collect();
    assert_eq!(2, rows.len());
    assert_eq!(SEPARATOR_MARGIN, rows[0].top_gap, "the rule gets air on both sides");
    assert_eq!(LINE_GAP, rows[1].top_gap);
}

// ---- scroll culling ----

#[test]
fn nothing_scrolls_when_the_content_fits() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    assert_eq!(0, max_scroll(s.content_h as i32, s.view_h as i32));
}

#[test]
fn scroll_moves_every_pen_by_the_same_amount() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    let unscrolled: Vec<f32> = s.visible(0.0, 4000.0).map(|p| p.pen.1).collect();
    let scrolled: Vec<f32> = s.visible(7.0, 4000.0).map(|p| p.pen.1).collect();
    assert_eq!(unscrolled.len(), scrolled.len());
    for (a, b) in unscrolled.iter().zip(&scrolled) {
        assert_eq!(a - 7.0, *b);
    }
}

#[test]
fn an_element_scrolled_far_above_the_panel_is_culled() {
    let s = laid_out(&one_card(&["noun"], None), 424.0, 4000.0, false, false);
    let all = s.elems.len();
    let head = find(&s, ElemKind::Headword);
    // Past its box and its em of slack.
    let past = head.pen.1 + head.rect.h + head.font_size + 1.0;
    let kept: Vec<&str> = s.visible(past, 4000.0).map(|p| p.elem.text.as_str()).collect();
    assert!(kept.len() < all, "scrolling past the headword must cull it");
    assert!(!kept.contains(&"雑談"));
}

/// Ink overhangs the measured box.
#[test]
fn an_element_at_the_top_edge_survives_on_its_slack() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    let head = find(&s, ElemKind::Headword);
    let just_off = head.pen.1 + head.rect.h;
    assert!(
        s.visible(just_off, 4000.0).any(|p| p.elem.text == "雑談"),
        "an element one pixel off the top keeps its ascender"
    );
}

#[test]
fn an_element_below_the_panel_is_culled() {
    let s = laid_out(&one_card(&["noun"], None), 424.0, 4000.0, false, false);
    let last = s.elems.last().unwrap().text.clone();
    assert!(s.visible(0.0, 4000.0).any(|p| p.elem.text == last));
    assert!(
        !s.visible(0.0, 20.0).any(|p| p.elem.text == last),
        "a 20px panel cannot show the last element"
    );
}

// ---- the side panel ----

#[test]
fn the_side_panel_takes_its_width_off_the_main_column() {
    let inline = laid_out(&with_collapsed(), 424.0, 4000.0, false, false);
    let panel = laid_out(&with_collapsed(), 424.0, 4000.0, false, true);
    let extra = SIDE_GAP + SEPARATOR_THICKNESS + SIDE_GAP + SIDE_PANEL_W;
    assert_eq!(inline.content_w - extra, panel.content_w);
    assert_eq!(Some(panel.content_w + extra + 2.0 * panel.origin), panel.panel_w);
    assert_eq!(None, inline.panel_w, "no side column: keep the width offered");
}

#[test]
fn the_side_column_sits_right_of_the_rule() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, true);
    let side = s.side.as_ref().expect("a side panel was requested");
    assert_eq!(s.origin, side.origin_y);
    assert_eq!(s.origin + s.content_w + SIDE_GAP, side.rule_x);
    assert_eq!(SEPARATOR_THICKNESS, side.rule_w);
    assert_eq!(side.rule_x + SEPARATOR_THICKNESS + SIDE_GAP, side.col_x);
    assert_eq!(SIDE_PANEL_W, side.col_w);
}

#[test]
fn the_side_column_is_a_heading_then_one_row_per_entry() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, true);
    let side = s.side.as_ref().unwrap();
    assert_eq!(3, side.rows.len());
    assert_eq!(None, side.rows[0].idx, "the heading is not clickable");
    assert_eq!("See also", side.rows[0].text);
    assert_eq!(0.0, side.rows[0].y);
    assert_eq!(Some(0), side.rows[1].idx);
    assert_eq!(Some(1), side.rows[2].idx);

    let h = side.rows[0].h;
    assert_eq!(h + LINE_GAP, side.rows[1].y);
    assert_eq!(side.rows[1].y + LINE_GAP + side.rows[1].h, side.rows[2].y);
    assert_eq!(side.rows[2].y + LINE_GAP + side.rows[2].h, side.height);
}

/// The taller column sets the height.
#[test]
fn the_body_is_as_tall_as_its_tallest_column() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, true);
    let side_h = s.side.as_ref().unwrap().height;
    assert_eq!(s.used_h.max(side_h).ceil() + 2.0 * s.origin, s.content_h);
}

#[test]
fn side_rows_scroll_and_cull_with_the_panel() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, true);
    let side = s.side.as_ref().unwrap();
    assert_eq!(3, side.visible(0.0, 4000.0).count());
    let heading_h = side.rows[0].h;
    assert_eq!(
        2,
        side.visible(side.origin_y + heading_h, 4000.0).count(),
        "the heading scrolls off with no slack"
    );
    assert_eq!(0, side.visible(0.0, 0.0).count());
}

#[test]
fn a_headless_collapsed_row_earns_no_side_entry() {
    let mut p = with_collapsed();
    p.collapsed.push(CollapsedRow { written: None, reading: None, summary: "orphan".into() });
    let s = laid_out(&p, 424.0, 4000.0, false, true);
    let side = s.side.as_ref().unwrap();
    assert_eq!(3, side.rows.len(), "heading plus the two rows with headwords");
    assert!(!side.rows.iter().any(|r| r.text.contains("orphan")));
}

// ---- hit targets ----

#[test]
fn an_inline_collapsed_row_is_clickable_across_the_panel() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, false);
    let rows: Vec<&HitTarget> = s
        .hits
        .iter()
        .filter(|h| matches!(h.action, HitAction::ExpandEntry(_)))
        .collect();
    assert_eq!(2, rows.len());
    assert_eq!(None, rows[0].x, "a row spans whatever width the panel has");
    assert_eq!(None, rows[0].w);
    assert_eq!(HitAction::ExpandEntry(0), rows[0].action);
    assert_eq!(HitAction::ExpandEntry(1), rows[1].action);

    let elem = s.elems.iter().find(|e| e.kind == ElemKind::Collapsed).unwrap();
    assert_eq!(elem.pen.1, rows[0].y, "the target covers the row it was measured from");
    assert_eq!(elem.rect.h, rows[0].h);
}

#[test]
fn the_back_button_is_clickable_and_leads_the_scene() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, true, false);
    assert_eq!(ElemKind::BackButton, s.elems[0].kind);
    let back = s.hits.iter().find(|h| h.action == HitAction::Back).unwrap();
    assert_eq!(s.elems[0].pen.1, back.y);
    assert_eq!(s.elems[0].rect.h, back.h);
    let head = find(&s, ElemKind::Headword);
    assert_eq!(LINE_GAP, head.top_gap, "the headword makes room for the button");
}

/// One box per kanji, and no more.
#[test]
fn every_kanji_in_the_headword_drills_down() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    let drills: Vec<&HitTarget> = s
        .hits
        .iter()
        .filter(|h| matches!(h.action, HitAction::DrillDown(_)))
        .collect();
    assert_eq!(
        vec![
            HitAction::DrillDown("雑".into()),
            HitAction::DrillDown("談".into()),
        ],
        drills.iter().map(|h| h.action.clone()).collect::<Vec<_>>()
    );
    let head = find(&s, ElemKind::Headword);
    let advance = head.font_size * ADVANCE;
    assert_eq!(Some(s.origin), drills[0].x);
    assert_eq!(Some(s.origin + advance), drills[1].x);
    assert_eq!(head.pen.1, drills[0].y, "caret boxes are run-relative");
    assert_eq!(Some(advance), drills[0].w);
}

#[test]
fn a_kana_only_headword_drills_nowhere() {
    let p = Presentation {
        top: Some(Card {
            written: None,
            reading: Some("ざつだん".into()),
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 2,
        }),
        collapsed: vec![],
        all_cards: vec![],
        sentence: None,
    };
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert!(!s.hits.iter().any(|h| matches!(h.action, HitAction::DrillDown(_))));
}

/// Paint order is hit order.
#[test]
fn side_rows_come_after_the_main_column_in_hit_order() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, true, true);
    let all = s.hit_targets();
    assert_eq!(s.hits.len() + 2, all.len());
    assert_eq!(HitAction::Back, all[0].action);
    let side = s.side.as_ref().unwrap();
    let tail = &all[s.hits.len()..];
    assert_eq!(HitAction::ExpandEntry(0), tail[0].action);
    assert_eq!(Some(side.col_x), tail[0].x);
    assert_eq!(Some(side.col_w), tail[0].w);
    assert_eq!(side.origin_y + side.rows[1].y, tail[0].y);
    assert_eq!(side.rows[1].h, tail[0].h);
    assert_eq!(HitAction::ExpandEntry(1), tail[1].action);
}

#[test]
fn a_scene_without_a_side_column_has_no_extra_targets() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, false);
    assert_eq!(s.hits.len(), s.hit_targets().len());
}

// ---- the Anki slot ----

#[test]
fn no_anki_state_reserves_no_slot() {
    let s = laid_out(&one_card(&[], None), 424.0, 4000.0, false, false);
    assert_eq!(None, s.anki);
}

#[test]
fn a_connected_anki_reserves_a_strip_under_the_panel() {
    let theme = Theme::dark();
    let anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
    let p = one_card(&[], None);
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: &p,
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: false,
            anki: Some(&anki),
        },
        &mut m,
    )
    .unwrap();
    let slot = s.anki.as_ref().expect("a connected Anki reserves its slot");
    assert_eq!("\u{ff0b} Add to Anki", slot.label);
    assert_eq!(theme.dict_label_text, slot.color);
    assert_eq!(0.0, slot.rect.x, "the strip is flush with the panel's edges");
    assert_eq!(424.0, slot.rect.w);
    assert_eq!(s.view_h, slot.rect.y, "it sits directly under the panel");
    assert_eq!(theme.collapsed_size * LINE_H, slot.rect.h);
}

#[test]
fn a_disabled_anki_reserves_no_slot() {
    let theme = Theme::dark();
    let p = one_card(&[], None);
    let anki = AnkiPopupState::disabled();
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: &p,
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: false,
            anki: Some(&anki),
        },
        &mut m,
    )
    .unwrap();
    assert_eq!(None, s.anki);
}

/// The slot spans the widened panel.
#[test]
fn the_anki_strip_matches_the_panel_the_side_column_widened() {
    let theme = Theme::dark();
    let anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
    let p = with_collapsed();
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: &p,
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: true,
            anki: Some(&anki),
        },
        &mut m,
    )
    .unwrap();
    assert_eq!(s.panel_w, Some(s.anki.as_ref().unwrap().rect.w));
}

// ---- the measurement seam ----

/// Measure-only: layout asks for a
/// run per element, and nothing else.
#[test]
fn layout_measures_each_run_at_the_width_it_reports() {
    let theme = Theme::dark();
    let p = one_card(&[], Some(7671));
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: &p,
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: false,
            anki: None,
        },
        &mut m,
    )
    .unwrap();
    let runs: Vec<(String, f32)> =
        m.asked.iter().map(|a| (a.text.clone(), a.max_w)).collect();
    for elem in &s.elems {
        if elem.kind == ElemKind::Separator {
            continue;
        }
        assert!(
            runs.contains(&(elem.text.clone(), elem.wrap_w)),
            "{:?} was reported at {} but never measured there",
            elem.text,
            elem.wrap_w
        );
    }
}

#[test]
fn a_refused_run_abandons_the_walk() {
    let theme = Theme::dark();
    let p = one_card(&[], None);
    let err = scene(
        &SceneRequest {
            presentation: &p,
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: false,
            anki: None,
        },
        &mut BrokenMeasure,
    )
    .expect_err("a refusing engine cannot produce a scene");
    assert_eq!("measuring text failed: no font", err.to_string());
}

/// One span in, the aggregate the
/// walk stacks out - and the detail
/// beside it saying the same thing.
#[test]
fn one_span_measures_to_one_line_box_that_fills_the_run() {
    let spans = [styled("abcd", 10.0)];
    let m = fake_measure(&spans, 100.0);

    assert_eq!(Metrics { w: 20.0, h: 20.0, lines: 1 }, m.metrics);
    assert_eq!(vec![LineBox { y: 0.0, w: 20.0, h: 20.0, baseline: 10.0 }], m.lines);
    assert_eq!(vec![SpanBox { span: 0, line: 0, x: 0.0, w: 20.0, h: 20.0 }], m.spans);
}

/// The contract ticket 07's inline
/// pass is written against: spans that
/// fit share one line, sit end to end
/// across it, and hang off one
/// baseline whatever their own heights
/// are (ADR-0013).
#[test]
fn spans_that_fit_share_one_line_and_one_baseline() {
    // 4 units at 10px plus 4 at 20px:
    // 4×5 + 4×10 = 60 wide, inside 100.
    let spans = [styled("abcd", 10.0), styled("wxyz", 20.0)];
    let m = fake_measure(&spans, 100.0);

    assert_eq!(1, m.metrics.lines, "60 units of text fit a 100px line");
    assert_eq!(2, m.spans.len(), "one box per span");
    let (small, big) = (m.spans[0], m.spans[1]);
    assert_eq!((0, 0, 0.0, 20.0), (small.span, small.line, small.x, small.w));
    assert_eq!((1, 0, 20.0, 40.0), (big.span, big.line, big.x, big.w));
    assert_eq!(m.lines[0].w, big.x + big.w, "the spans sum to the line's width");

    // Each span asks for its own
    // advance; the line takes the
    // largest, and one baseline serves
    // both.
    assert_eq!((20.0, 40.0), (small.h, big.h));
    assert_eq!(40.0, m.lines[0].h, "the taller span sets the line");
    assert_eq!(20.0, m.lines[0].baseline);
    assert_eq!(Metrics { w: 60.0, h: 40.0, lines: 1 }, m.metrics);
}

/// A span boundary is not a line
/// boundary: the second span keeps
/// filling the line the first left off
/// on, and wraps mid-span when it runs
/// out. That is the single fact
/// ADR-0013 exists to change.
#[test]
fn a_span_wraps_within_itself_rather_than_at_its_boundary() {
    // 5px units, 8 to a 40px line.
    let spans = [styled("abcde", 10.0), styled("fghijk", 10.0)];
    let m = fake_measure(&spans, 40.0);

    assert_eq!(2, m.metrics.lines);
    assert_eq!(
        vec![
            SpanBox { span: 0, line: 0, x: 0.0, w: 25.0, h: 20.0 },
            SpanBox { span: 1, line: 0, x: 25.0, w: 15.0, h: 20.0 },
            SpanBox { span: 1, line: 1, x: 0.0, w: 15.0, h: 20.0 },
        ],
        m.spans,
        "the second span finishes line one and continues on line two"
    );
    assert_eq!(40.0, m.lines[0].w);
    assert_eq!(20.0, m.lines[1].y, "line two starts under line one");
}

// ---- the inline formatting pass ----

/// A card whose one dictionary row carries `glossary` verbatim.
///
/// The headword is kana, so it earns no per-character drill target and
/// every hit in the scene is one the gloss itself produced.
fn rich(glossary: &str) -> Presentation {
    let card = Card {
        written: None,
        reading: Some("\u{3055}\u{3064}\u{3060}\u{3093}".into()),
        pos: vec![],
        freq: None,
        blocks: vec![tree("Jitendex", glossary)],
        match_len: 4,
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    }
}

/// One structured-content item wrapping `content`.
fn sc(content: &str) -> String {
    format!(r#"[{{"type":"structured-content","content":{content}}}]"#)
}

/// Every gloss-body element of `s`, in draw order.
///
/// The dictionary label is the `Text` element before them and carries the
/// label role's size, so the body is what is left at the body size.
fn bodies(s: &PopupScene) -> Vec<&SceneElem> {
    let body = Theme::dark().body_size;
    s.elems
        .iter()
        .filter(|e| e.kind == ElemKind::Text && e.font_size == body)
        .collect()
}

/// A gloss that is one plain string must still produce the element it
/// produced before an inline pass existed: one element, one span, the body
/// role, and one seam request for exactly that text.
#[test]
fn a_plain_string_gloss_is_one_element_of_one_span() {
    let theme = Theme::dark();
    let (s, asked) = measured(&theme, &one_card(&[], None), false);
    let gloss = s.elems.iter().find(|e| e.text == "chatting").expect("the gloss");

    assert_eq!(ElemKind::Text, gloss.kind);
    assert_eq!(1, gloss.lines);
    assert_eq!(
        vec![ElemSpan {
            at: 0,
            len: "chatting".len() as u32,
            color: theme.body_text,
            size: theme.body_size,
            weight: theme.body_weight,
            italic: theme.body_italic,
            shift: 0.0,
        }],
        gloss.spans
    );
    assert_eq!(1, asked.iter().filter(|a| a.text == "chatting").count());
}

/// Two top-level glossary items measure as *one* span, not three.
///
/// The separator has always been there; what must not change is the request
/// the seam gets, because that is what the geometry goldens hold. Adjacent
/// runs in one style are one run.
#[test]
fn items_in_one_style_coalesce_into_a_single_span() {
    let theme = Theme::dark();
    let p = card_with(vec![block("Jitendex", &["raw; uncooked", "natural"])]);
    let (s, asked) = measured(&theme, &p, false);

    let gloss = bodies(&s);
    assert_eq!(1, gloss.len(), "both items share one paragraph");
    assert_eq!("raw; uncooked; natural", gloss[0].text);
    assert_eq!(1, gloss[0].spans.len(), "one style, one span");
    asked_for(&asked, "raw; uncooked; natural");
}

/// The one thing ADR-0013 exists to change: a bold word and a normal word
/// adjacent in the source share a line.
#[test]
fn a_bold_word_and_a_normal_word_share_one_wrapped_line() {
    let p = rich(&sc(r#"[{"tag":"b","content":"bold"},"normal text"]"#));
    // 200px column: 26 units fit, the run is 15.
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len(), "one element, not one per style");
    assert_eq!(1, gloss[0].lines, "and one line, not one per style");
    assert_eq!("boldnormal text", gloss[0].text);
    let weights: Vec<u16> = gloss[0].spans.iter().map(|s| s.weight).collect();
    assert_eq!(vec![700, Theme::dark().body_weight], weights);
    assert_eq!(15.0 * 15.0 * ADVANCE, gloss[0].rect.w);
}

/// And the paragraph rewraps as one unit.
///
/// The break lands *inside* the second span, so line one is full: a
/// renderer that ended a line at every style change would leave line one
/// four units wide instead of thirteen.
#[test]
fn mixed_spans_wrap_as_one_paragraph_and_break_within_a_span() {
    let p = rich(&sc(r#"[{"tag":"b","content":"bold"},"normal text"]"#));
    // 100px column: 13 units per line, 15 units of text.
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len());
    assert_eq!(2, gloss[0].lines);
    assert_eq!(
        13.0 * 15.0 * ADVANCE,
        gloss[0].rect.w,
        "line one is full, so the normal span continues it"
    );
    assert_eq!(2.0 * 15.0 * LINE_H, gloss[0].rect.h);
}

/// No spaces to break at, and it still wraps.
#[test]
fn a_cjk_run_wraps_without_a_single_space_in_it() {
    let kanji = "\u{6f22}".repeat(20);
    let p = card_with(vec![block("\u{5927}\u{8f9e}\u{6797}", &[&kanji])]);
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len());
    assert_eq!(2, gloss[0].lines, "20 units at 7.5px do not fit a 100px column");
}

/// Sibling blocks are separated, and the separator is the gap.
///
/// Before the tree reached the panel these two arrived as `to runto flow`.
#[test]
fn sibling_blocks_become_two_elements_a_line_gap_apart() {
    let p = rich(&sc(
        r#"[{"tag":"div","content":"to run"},{"tag":"div","content":"to flow"}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(vec!["to run", "to flow"], gloss.iter().map(|e| e.text.as_str()).collect::<Vec<_>>());
    assert_eq!(LINE_GAP, gloss[1].top_gap, "the separator is a line gap");
    assert_eq!(gloss[0].pen.1 + gloss[0].advance + LINE_GAP, gloss[1].pen.1);
}

/// A `sup` is raised off its line's baseline and takes no height with it.
///
/// The line is as tall as the body span alone: a reference mark that grew
/// the line would push every following block down.
#[test]
fn a_superscript_is_raised_without_growing_its_line() {
    let theme = Theme::dark();
    let p = rich(&sc(r#"["note",{"tag":"sup","content":"1"}]"#));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len());
    assert_eq!(1, gloss[0].lines);
    let (body, mark) = (gloss[0].spans[0], gloss[0].spans[1]);
    assert_eq!(0.0, body.shift, "the body sits on the baseline");
    assert_eq!(theme.body_size / 3.0, mark.shift, "and the mark a third of an em above it");
    assert_eq!(theme.body_size / 1.2, mark.size, "`smaller`, as HTML draws a sup");
    assert_eq!(theme.body_size * LINE_H, gloss[0].rect.h, "the body span sets the line");
}

/// A `sub` drops instead, and `verticalAlign` says so directly.
#[test]
fn a_subscript_drops_and_an_explicit_vertical_align_agrees() {
    let theme = Theme::dark();
    let p = rich(&sc(
        r#"["x",{"tag":"sub","content":"2"},{"tag":"span","style":{"verticalAlign":"super"},"content":"n"}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let spans = &bodies(&s)[0].spans;

    assert_eq!(-theme.body_size / 5.0, spans[1].shift);
    assert_eq!(theme.body_size / 3.0, spans[2].shift, "a declared `super` raises like a sup");
    assert_eq!(theme.body_size, spans[2].size, "but changes no size of its own");
}

/// The text-relative values are answered against the line the span landed
/// on, which is the one fact only the measurer knows (ADR-0013).
#[test]
fn text_top_lifts_a_small_span_to_its_lines_own_text_top() {
    let theme = Theme::dark();
    let p = rich(&sc(
        r#"["big",{"tag":"span","style":{"fontSize":"0.5em","verticalAlign":"text-top"},"content":"small"}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);
    let small = gloss[0].spans[1];

    // The fake hangs every line off an ascent of its tallest span's own
    // size, so a half-size span's own ascent is half of one: the lift is
    // the difference.
    assert_eq!(theme.body_size / 2.0, small.size);
    assert_eq!(theme.body_size / 2.0, small.shift);
    assert_eq!(theme.body_size * LINE_H, gloss[0].rect.h, "and the line is unmoved");
}

// ---- ruby ----

/// The acceptance geometry: a reading takes a slot of its own out of the
/// line, above the base, and the line above keeps every pixel it had.
///
/// Six body units at a 30px content width is four to a line, so the base
/// lands on the second line and there is a first line for it to clear. The
/// control is the same paragraph with the `ruby` wrapper taken off, which
/// pins the two facts that matter: the reading's top edge is exactly where
/// the second line used to start, so nothing overlaps and no gap opens;
/// and its bottom edge is exactly its base's own ink top.
///
/// The slot is bought by a [`RUBY_FILLER`] span, and a line gives only its
/// ascent share of any growth to the space above its baseline - half, for
/// this fake, and about four fifths for a real CJK face. So the line grows
/// by `reading / ascent` and not by `reading`. That is the price of a slot
/// the *measurer* reserves: line boxes grown after the wrap would be
/// geometry the bins' own re-measure never reproduces.
#[test]
fn a_reading_reserves_its_own_slot_and_clears_the_line_above() {
    let theme = Theme::dark();
    let base_line = theme.body_size * LINE_H;
    let read_line = theme.body_size * RUBY_RATIO * LINE_H;
    // The fake hangs every line off an ascent of its tallest span's own
    // size, so its ascent share is `1 / LINE_H`.
    let ascent = 1.0 / LINE_H;
    let ruby_line = base_line + read_line / ascent;

    // A `span` and not a bare second string: an array of nothing but bare
    // strings is a list, one paragraph per string, and the control has to
    // be the same one paragraph the ruby fixture is.
    let plain = laid_out(
        &rich(&sc(r#"["aaaa",{"tag":"span","content":"bb"}]"#)),
        54.0,
        4000.0,
        false,
        false,
    );
    let plain = bodies(&plain)[0];
    assert_eq!(2, plain.lines, "the control wrapped to two lines");
    assert_eq!(2.0 * base_line, plain.rect.h);
    assert!(plain.ruby.is_empty(), "and carries no reading");

    let s = laid_out(
        &rich(&sc(r#"["aaaa",{"tag":"ruby","content":["bb",{"tag":"rt","content":"cc"}]}]"#)),
        54.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!(
        "aaaabb\u{2060}",
        gloss.text,
        "the reading itself is not in the flow's text - only the filler that \
         buys its slot, which advances nothing",
    );
    assert_eq!(2, gloss.lines, "and adds no line of its own");
    assert_eq!(base_line + ruby_line, gloss.rect.h, "the base's line grew, once");
    assert_eq!(gloss.rect.h, gloss.advance, "which is what the block walk stacks");

    let read = &gloss.ruby[0];
    assert_eq!(read_line, read.h);
    assert_eq!(
        base_line, read.y,
        "the reading starts exactly where line one ends: no overlap, no gap",
    );
    // The base's own ink top: its line's baseline, less its own ascent.
    let base_ink = base_line + ascent * ruby_line - ascent * base_line;
    assert_eq!(base_ink, read.y + read.h, "and ends on its base's own ink top");
}

/// A reading is centred over the horizontal extent its base measured to,
/// and a base is addressable on its own even when the text beside it is in
/// the identical style - which is why a base gets its own span.
#[test]
fn a_reading_centres_over_its_own_base_and_not_over_its_neighbours() {
    let unit = Theme::dark().body_size * ADVANCE;
    let s = laid_out(
        &rich(&sc(r#"["aa",{"tag":"ruby","content":["b",{"tag":"rt","content":"c"}]},"cc"]"#)),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!("aab\u{2060}cc", gloss.text);
    assert_eq!(4, gloss.spans.len(), "the base did not coalesce into its neighbours");

    // The base is one unit at two units in; the reading is one unit of a
    // half-size run, so it is half as wide and sits a quarter of a base in.
    let read = &gloss.ruby[0];
    assert_eq!(2.0 * unit + (unit - unit * RUBY_RATIO) / 2.0, read.x);
    assert_eq!(unit * RUBY_RATIO, read.w);
}

/// A reading wider than its base overhangs it, as a browser lets it, and
/// the element's ink box covers what it drew.
#[test]
fn a_reading_wider_than_its_base_overhangs_and_widens_the_ink_box() {
    let unit = Theme::dark().body_size * ADVANCE;
    let s = laid_out(
        &rich(&sc(r#"[{"tag":"ruby","content":["a",{"tag":"rt","content":"bbbb"}]}]"#)),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];
    let read = &gloss.ruby[0];

    // One base unit against four half-size reading units: the reading is
    // twice its base and would hang half a base off either side. The left
    // is clamped into the panel, so all of the overhang shows on the right.
    assert_eq!(4.0 * unit * RUBY_RATIO, read.w);
    assert_eq!(0.0, read.x, "clamped into the panel, not off its left edge");
    assert_eq!(read.w, gloss.rect.w, "and the ink box covers what was drawn");
}

/// Ruby is inline: a ruby run wraps with the text around it and forces no
/// break of its own. Pinned against the identical paragraph with the
/// wrapper removed - same line count, same wrap - so the only difference
/// the wrapper makes is the slot above the line its base landed on.
#[test]
fn a_ruby_run_mid_sentence_wraps_with_its_text_and_forces_no_break() {
    let base_line = Theme::dark().body_size * LINE_H;
    let read_line = Theme::dark().body_size * RUBY_RATIO * LINE_H;
    let narrow = 39.0;

    let plain = laid_out(
        &rich(&sc(r#"["aa",{"tag":"span","content":"b"},"cc"]"#)),
        narrow,
        4000.0,
        false,
        false,
    );
    let plain = bodies(&plain)[0];

    let s = laid_out(
        &rich(&sc(r#"["aa",{"tag":"ruby","content":["b",{"tag":"rt","content":"c"}]},"cc"]"#)),
        narrow,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!(3, plain.lines, "two units to a line puts the base on line two");
    assert_eq!(plain.lines, gloss.lines, "ruby broke no line the text did not");
    // A line gives only its ascent share of any growth to the space above
    // its baseline, so it grows by `reading / ascent` - see
    // `a_reading_reserves_its_own_slot_and_clears_the_line_above`.
    let ascent = 1.0 / LINE_H;
    assert_eq!(plain.rect.h + read_line / ascent, gloss.rect.h, "it only took its slot");

    let read = &gloss.ruby[0];
    assert_eq!(base_line, read.y, "and the reading followed its base to line two");
    // The base starts the line, so the half-width reading sits a quarter
    // of a base in - centred over it, not flush with the line.
    let unit = Theme::dark().body_size * ADVANCE;
    assert_eq!((unit - unit * RUBY_RATIO) / 2.0, read.x);
}

/// One slot per `rt`, so per-character furigana pairs each reading with the
/// base it was written after. Two kanji, two readings, two slots.
#[test]
fn per_character_furigana_gives_each_base_its_own_reading() {
    let unit = Theme::dark().body_size * ADVANCE;
    let s = laid_out(
        &rich(&sc(
            r#"[{"tag":"ruby","content":["漢",{"tag":"rt","content":"かん"},"字",{"tag":"rt","content":"じ"}]}]"#,
        )),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!("漢\u{2060}字\u{2060}", gloss.text, "one filler per reading");
    assert_eq!(4, gloss.spans.len(), "each base and each filler its own span");
    assert_eq!(
        vec!["かん", "じ"],
        gloss.ruby.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
    );
    // Two half-size units are exactly one base wide, so かん covers 漢
    // flush; じ is half that and sits a quarter of a base into 字.
    assert_eq!(0.0, gloss.ruby[0].x);
    assert_eq!(unit, gloss.ruby[0].w);
    assert_eq!(unit + (unit - unit * RUBY_RATIO) / 2.0, gloss.ruby[1].x);
}

/// Story 13. `rp` holds the parentheses HTML wrote for a renderer that
/// cannot draw ruby. This one can, so they are spent only when no reading
/// arrives - and then a malformed ruby degrades to readable text instead of
/// to a bare base.
#[test]
fn an_rp_fallback_renders_only_when_no_reading_arrives() {
    let with_rt = laid_out(
        &rich(&sc(
            r#"[{"tag":"ruby","content":["猫",{"tag":"rp","content":"("},{"tag":"rt","content":"ねこ"},{"tag":"rp","content":")"}]}]"#,
        )),
        424.0,
        4000.0,
        false,
        false,
    );
    let with_rt = bodies(&with_rt)[0];
    assert_eq!("猫\u{2060}", with_rt.text, "the parentheses stay unspent");
    assert_eq!(vec!["ねこ"], with_rt.ruby.iter().map(|r| r.text.as_str()).collect::<Vec<_>>());

    let without = laid_out(
        &rich(&sc(
            r#"[{"tag":"ruby","content":["猫",{"tag":"rp","content":"（"},{"tag":"rp","content":"）"}]}]"#,
        )),
        424.0,
        4000.0,
        false,
        false,
    );
    let without = bodies(&without)[0];
    assert_eq!("猫（）", without.text, "with no reading the fallback flows inline");
    assert!(without.ruby.is_empty(), "and nothing is placed above the base");
}

/// The reading's size is the base's stepped by the theme-independent ruby
/// ratio, and the `rt`'s own resolved style is kept on top of it: a
/// dictionary that colours its readings is honoured, and a `fontSize` on
/// the `rt` is relative to the reading's size rather than the base's, as
/// CSS says.
#[test]
fn a_readings_size_halves_its_base_and_its_own_style_survives() {
    let theme = Theme::dark();
    let s = laid_out(
        &rich(&sc(
            r#"[{"tag":"ruby","content":["猫",{"tag":"rt","content":"ねこ"}]},{"tag":"ruby","content":["犬",{"tag":"rt","style":{"color":"red","fontSize":"0.5em"},"content":"いぬ"}]}]"#,
        )),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!(theme.body_size * RUBY_RATIO, gloss.ruby[0].size);
    assert_eq!(theme.body_text, gloss.ruby[0].color);
    assert_eq!(theme.body_weight, gloss.ruby[0].weight);

    assert_eq!(theme.body_size * RUBY_RATIO * 0.5, gloss.ruby[1].size);
    assert_eq!((255, 0, 0), gloss.ruby[1].color);
}

/// A matched row's number is written in the body's own style, and joins the
/// span it precedes when nothing about the two differs. A ruby base
/// differs: joined, the reading would centre over `1. 猫` instead of over
/// `猫`.
#[test]
fn a_row_number_does_not_join_the_ruby_base_it_precedes() {
    let unit = Theme::dark().body_size * ADVANCE;
    let block = GlossBlock {
        dict_name: "大辞林".into(),
        dict_id: crate::present::NO_ROW,
        entries: vec![
            row_of(&sc(r#"[{"tag":"ruby","content":["猫",{"tag":"rt","content":"ねこ"}]}]"#), &[]),
            row_of(&sc(r#"["dog"]"#), &[]),
        ],
    };
    let s = laid_out(&card_with(vec![block]), 424.0, 4000.0, false, false);
    let gloss = bodies(&s)[0];

    assert_eq!("1. 猫\u{2060}", gloss.text);
    assert_eq!(3, gloss.spans.len(), "the number is its own span");
    // Three units of number, then the base: the reading is two half-size
    // units, exactly one base wide, so it sits flush over it.
    assert_eq!(3.0 * unit, gloss.ruby[0].x);
}

/// An internal cross-reference drills down, and its rect covers its own
/// spans on every line they reached.
#[test]
fn an_internal_link_drills_down_across_a_wrap_boundary() {
    // "see " then a 12-unit link: 16 units, 13 to a 100px line.
    let p = rich(&sc(
        r#"["see ",{"tag":"a","href":"?query=%E7%8C%AB&wildcards=off","content":"cat and kitten"}]"#,
    ));
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = bodies(&s);
    let unit = 15.0 * ADVANCE;

    let drills: Vec<&HitTarget> = s
        .hits
        .iter()
        .filter(|h| matches!(h.action, HitAction::DrillDown(_)))
        .collect();
    assert_eq!(2, drills.len(), "one target per line the link touched");
    for hit in &drills {
        assert_eq!(HitAction::DrillDown("\u{732B}".into()), hit.action, "percent-decoded");
    }
    // Line one: the link starts after "see " and runs to the margin.
    assert_eq!(Some(s.origin + 4.0 * unit), drills[0].x);
    assert_eq!(Some(9.0 * unit), drills[0].w);
    assert_eq!(gloss[0].pen.1, drills[0].y);
    // Line two: the rest, from the margin.
    assert_eq!(Some(s.origin), drills[1].x);
    assert_eq!(Some(5.0 * unit), drills[1].w);
    assert_eq!(gloss[0].pen.1 + 15.0 * LINE_H, drills[1].y);
    assert_eq!(15.0 * LINE_H, drills[0].h, "as tall as the line it sits on");
}

/// A citation opens in a browser instead.
#[test]
fn an_external_link_opens_in_the_browser() {
    let p = rich(&sc(
        r#"["from ",{"tag":"a","href":"https://example.org/x","content":"the source"}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert_eq!(
        vec![HitAction::OpenUrl("https://example.org/x".into())],
        s.hits.iter().map(|h| h.action.clone()).collect::<Vec<_>>()
    );
}

/// A scheme chibipop will not follow earns no target at all - the text
/// stays, the click does not.
#[test]
fn an_unfollowable_link_earns_no_hit_target() {
    for href in ["javascript:alert(1)", "data:text/html,x", "other.html"] {
        let p = rich(&sc(&format!(
            r#"[{{"tag":"a","href":"{href}","content":"click"}}]"#
        )));
        let s = laid_out(&p, 424.0, 4000.0, false, false);
        assert!(s.hits.is_empty(), "{href} must not be clickable");
        assert!(texts(&s).contains(&"click"), "{href} must keep its text");
    }
}

/// Rich content must not disturb the clicks that already worked.
#[test]
fn rich_content_leaves_the_existing_hit_targets_alone() {
    let plain = with_collapsed();
    let mut rich = plain.clone();
    let body = tree(
        "Jitendex",
        &sc(r#"[{"tag":"b","content":"chat"},{"tag":"div","content":"idle talk"}]"#),
    );
    for card in rich.all_cards.iter_mut().chain(rich.top.iter_mut()) {
        card.blocks = vec![body.clone()];
    }

    let a = laid_out(&plain, 424.0, 4000.0, true, false);
    let b = laid_out(&rich, 424.0, 4000.0, true, false);
    let kept = |s: &PopupScene| -> Vec<HitAction> {
        s.hit_targets()
            .iter()
            .filter(|h| !matches!(h.action, HitAction::OpenUrl(_)))
            .map(|h| h.action.clone())
            .collect()
    };
    assert_eq!(kept(&a), kept(&b));
    assert!(
        kept(&b).contains(&HitAction::DrillDown("\u{96d1}".into())),
        "the headword still drills through caret_boxes"
    );
    assert!(kept(&b).contains(&HitAction::ExpandEntry(0)));
    assert!(kept(&b).contains(&HitAction::Back));

    // And the Anki slot is still reserved from the same label.
    let theme = Theme::dark();
    let anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
    let slot = |p: &Presentation| -> Option<AnkiSlot> {
        scene(
            &SceneRequest {
                presentation: p,
                theme: &theme,
                max_w: 424.0,
                max_h: 4000.0,
                show_back: false,
                side_panel: false,
                anki: Some(&anki),
            },
            &mut FakeMeasure::default(),
        )
        .unwrap()
        .anki
    };
    assert_eq!(slot(&plain).map(|a| a.rect.h), slot(&rich).map(|a| a.rect.h));
}

/// Colour and weight from the dictionary's own `style`, on one line beside
/// the body's.
#[test]
fn a_styled_span_carries_its_own_colour_and_weight() {
    let p = rich(&sc(
        r##"["plain ",{"tag":"span","style":{"color":"#ff0000","fontWeight":"bold","fontStyle":"italic"},"content":"red"}]"##,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let spans = &bodies(&s)[0].spans;

    assert_eq!(2, spans.len());
    assert_eq!(Theme::dark().body_text, spans[0].color);
    assert_eq!(((255, 0, 0), 700, true), (spans[1].color, spans[1].weight, spans[1].italic));
}

/// A header cell is bold beside its row's data cells, per the spec's
/// defaults table - the shape a real conjugation table has.
#[test]
fn a_header_cell_is_bold_on_the_same_line_as_its_data_cells() {
    let p = rich(&sc(
        r#"[{"tag":"table","content":{"tag":"tr","content":[{"tag":"th","content":"past"},{"tag":"td","content":"\u98f2\u3093\u3060"}]}}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len(), "a row is one paragraph until ticket 10 grids it");
    assert_eq!(1, gloss[0].lines);
    assert_eq!(
        vec![700, Theme::dark().body_weight],
        gloss[0].spans.iter().map(|s| s.weight).collect::<Vec<_>>()
    );
}

/// A pathological tree terminates and its outer levels reach the panel.
#[test]
fn a_tree_nested_past_the_depth_cap_still_renders_its_outer_levels() {
    let depth = crate::dict::gloss::MAX_DEPTH as usize + 20;
    let mut json = String::from("\"deepest\"");
    for i in (0..depth).rev() {
        json = format!(r#"{{"tag":"div","content":["level {i}",{json}]}}"#);
    }
    let s = laid_out(&rich(&sc(&format!("[{json}]"))), 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert!(texts(&s).contains(&"level 0"), "the outermost level renders");
    assert!(texts(&s).contains(&"level 1"));
    assert!(
        gloss.len() < depth,
        "the walk stops at the cap: {} paragraphs for {depth} levels",
        gloss.len()
    );
    assert!(!texts(&s).contains(&"deepest"), "and the over-cap subtree is gone");
}

/// A row's number leads its first paragraph and no other, in the body's own
/// style - so it joins the span it precedes instead of measuring as one of
/// its own.
#[test]
fn a_numbered_row_numbers_only_its_first_paragraph() {
    let two = tree(
        "\u{5927}\u{8f9e}\u{6797}",
        &sc(r#"[{"tag":"div","content":"first"},{"tag":"div","content":"second"}]"#),
    );
    let mut blocks = two.clone();
    blocks.entries.push(two.entries[0].clone());
    let s = laid_out(&card_with(vec![blocks]), 424.0, 4000.0, false, false);

    let bodies: Vec<&str> = bodies(&s).iter().map(|e| e.text.as_str()).collect();
    assert_eq!(vec!["1. first", "second", "2. first", "second"], bodies);
    let numbered = s.elems.iter().find(|e| e.text == "1. first").unwrap();
    assert_eq!(1, numbered.spans.len(), "the number joins the text it leads");
}

// ---- the block box pass ----

/// The em a box length resolves against: a box property is a fraction of
/// its *own* element's font size, so every expectation below is
/// `Theme::body_size` times the number the fixture declares.
const BOX_EM: f32 = 15.0;

/// One line of body text, as `FakeMeasure` measures it.
const BODY_LINE: f32 = BOX_EM * LINE_H;

/// The gloss body of a scene with exactly one.
fn one_body(s: &PopupScene) -> &SceneElem {
    let found = bodies(s);
    assert_eq!(1, found.len(), "expected one gloss element, got {found:?}");
    found[0]
}

/// The block box a gloss element is, which must exist.
fn block_box(e: &SceneElem) -> &ElemBox {
    e.block_box.as_ref().expect("this element must carry a block box")
}

/// The acceptance geometry: the walk advances by the box's *outer* height,
/// margins and all, while the element's own ink box stays the height of the
/// text inside it.
#[test]
fn a_box_with_margin_and_padding_advances_the_walk_by_its_outer_height() {
    let p = rich(&sc(
        r#"{"tag":"div","style":{"margin":0.4,"padding":0.2},"content":"boxed"}"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    // margin 6, padding 3, on all four edges; one 30px line inside.
    assert_eq!(BODY_LINE, gloss.rect.h, "the ink box is the text, as it always was");
    assert_eq!(
        6.0 + 3.0 + BODY_LINE + 3.0 + 6.0,
        gloss.advance,
        "and the advance is the outer height"
    );
    // The border box is the fill and the stroke: padding in, margin out.
    assert_eq!(3.0 + BODY_LINE + 3.0, block_box(gloss).rect.h);
}

/// The chosen rule, asserted: **adjacent siblings do not collapse.** A
/// browser would draw 6px between these two blocks; the panel draws 12.
///
/// Collapsing needs the box tree CSS resolves it against - parent to first
/// child, parent to last child, adjacent siblings, and an empty block
/// through itself - and this walk is a forward accumulation with no box
/// tree to resolve against. Implementing one of those four rules and not
/// the others is the "unexpectedly" the ticket warns about, so none is
/// implemented and the divergence is bounded to the pair of dictionaries
/// that declare `marginTop` (3) and `marginBottom` (12) on facing edges.
#[test]
fn adjacent_block_siblings_do_not_collapse_their_margins() {
    let p = rich(&sc(concat!(
        r#"{"tag":"div","content":["#,
        r#"{"tag":"div","style":{"marginBottom":0.4},"content":"one"},"#,
        r#"{"tag":"div","style":{"marginTop":0.4},"content":"two"}]}"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(2, gloss.len(), "two sibling blocks, two elements");
    assert_eq!(BODY_LINE + 6.0, gloss[0].advance, "its own bottom margin");
    assert_eq!(6.0 + BODY_LINE, gloss[1].advance, "and its own top margin");
    assert_eq!(
        BODY_LINE + 6.0 + LINE_GAP + 6.0,
        gloss[1].pen.1 - gloss[0].pen.1,
        "both margins are paid; collapsing would pay the larger one once"
    );
}

/// A block's text is inset by its padding, and its wrap width shrinks by
/// it: a padded box that did not narrow its content would overflow itself.
#[test]
fn a_blocks_padding_insets_its_text_and_narrows_its_wrap() {
    let plain = rich(&sc(r#"{"tag":"div","content":"padded"}"#));
    let padded = rich(&sc(
        r#"{"tag":"div","style":{"paddingLeft":0.4,"paddingRight":0.2},"content":"padded"}"#,
    ));
    let bare = laid_out(&plain, 424.0, 4000.0, false, false);
    let s = laid_out(&padded, 424.0, 4000.0, false, false);
    let (bare, gloss) = (one_body(&bare), one_body(&s));

    assert_eq!(bare.pen.0 + 6.0, gloss.pen.0, "the pen moves in by padding-left");
    assert_eq!(bare.wrap_w - 9.0, gloss.wrap_w, "and the wrap loses both sides");
}

/// The visible goal: a pill. Border width, style, colour and radius all
/// reach the scene, and the box is drawn around the pill's own run rather
/// than around the paragraph holding it.
#[test]
fn a_bordered_pill_puts_its_border_and_radius_in_the_scene() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"##,
        r##""borderWidth":0.2,"borderStyle":"solid","borderColor":"#ff0000","##,
        r##""borderRadius":0.4,"padding":0.2,"backgroundColor":"green"},"##,
        r##""content":"noun"}," a word"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    assert_eq!(None, gloss.block_box, "an inline box is not the block's");
    assert_eq!(1, gloss.inline_boxes.len(), "one line, one box");
    let pill = gloss.inline_boxes[0];
    assert_eq!(Edges::all(3.0), pill.style.border);
    assert_eq!(Edges::all(BorderStyle::Solid), pill.style.border_style);
    assert_eq!((255, 0, 0), pill.style.border_color);
    assert_eq!(6.0, pill.style.radius);
    assert_eq!(Some((0, 128, 0)), pill.style.background);

    // "noun" is four units at 7.5, outset by 3 of padding and 3 of border
    // on every side. It hugs its own run, not the whole paragraph.
    assert_eq!(gloss.pen.0 - 6.0, pill.rect.x);
    assert_eq!(gloss.pen.1 - 6.0, pill.rect.y);
    assert_eq!(4.0 * BOX_EM * ADVANCE + 12.0, pill.rect.w);
    assert_eq!(BODY_LINE + 12.0, pill.rect.h);
    assert_eq!(
        "noun a word", gloss.text,
        "and it kept its place on the line rather than breaking one"
    );
}

/// A background pill needs no border at all: Jitendex's own
/// `span[data-sc-class="tag"]` is a background, a radius and a padding,
/// and nothing else.
#[test]
fn a_background_pill_draws_without_a_border() {
    let p = rich(&sc(concat!(
        r##"{"tag":"span","style":{"backgroundColor":"#565656","##,
        r##""borderRadius":0.3,"padding":0.2,"marginRight":0.5},"content":"noun"}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    let pill = gloss.inline_boxes[0];
    assert_eq!(Some((0x56, 0x56, 0x56)), pill.style.background);
    assert_eq!(Edges::default(), pill.style.border_used(), "no border declared");
    assert!(pill.style.paints(), "a fill is ink even with no border");
    assert_eq!(4.5, pill.style.radius);
}

/// CSS fidelity that a plausible bug would get wrong: `border-style` is
/// `none` until declared, and `none` forces the used width to zero however
/// wide the author wrote it. A width alone draws nothing in a browser and
/// must draw nothing here.
#[test]
fn a_border_width_with_no_style_draws_nothing() {
    let p = rich(&sc(
        r#"{"tag":"div","style":{"borderWidth":0.2},"content":"unbordered"}"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    assert_eq!(None, gloss.block_box, "no style, no border, no box");
    assert_eq!(BODY_LINE, gloss.advance, "and it takes no space either");
}

/// The shorthand grammars, over the two edge types that use them. Every
/// case is one a real dictionary writes: Jitendex declares `padding: 0.2em
/// 0.3em` on its pill and `border-style: none none none solid` on its
/// info box.
#[test]
fn edge_shorthands_expand_the_way_css_expands_them() {
    // (declaration, top, right, bottom, left)
    let lengths: &[(&str, f32, f32, f32, f32)] = &[
        (r#""0.2em""#, 3.0, 3.0, 3.0, 3.0),
        (r#""0.2em 0.4em""#, 3.0, 6.0, 3.0, 6.0),
        (r#""0.2em 0.4em 0.8em""#, 3.0, 6.0, 12.0, 6.0),
        (r#""0.2em 0.4em 0.8em 1em""#, 3.0, 6.0, 12.0, 15.0),
        // A fifth value is not a shorthand; CSS drops the declaration.
        (r#""1em 1em 1em 1em 1em""#, 0.0, 0.0, 0.0, 0.0),
        // Nor is a unit this build cannot read - and it must not take
        // the half it understood.
        (r#""0.2em 3vw""#, 0.0, 0.0, 0.0, 0.0),
        // A bare number is Yomitan's own em multiplier.
        ("0.2", 3.0, 3.0, 3.0, 3.0),
        // `px` is relative to Yomitan's base, so it scales with the panel.
        (r#""14px""#, 15.0, 15.0, 15.0, 15.0),
    ];
    for &(decl, top, right, bottom, left) in lengths {
        let p = rich(&sc(&format!(
            r#"{{"tag":"div","style":{{"padding":{decl}}},"content":"x"}}"#
        )));
        let s = laid_out(&p, 424.0, 4000.0, false, false);
        let got = one_body(&s).block_box.map_or(Edges::default(), |b| b.style.padding);
        assert_eq!(Edges { top, right, bottom, left }, got, "padding: {decl}");
    }

    let styles: &[(&str, Edges<BorderStyle>)] = &[
        ("solid", Edges::all(BorderStyle::Solid)),
        ("dashed", Edges::all(BorderStyle::Dashed)),
        // `groove` is drawn as one solid rule at a hairline width.
        ("groove", Edges::all(BorderStyle::Solid)),
        ("hidden", Edges::all(BorderStyle::None)),
        (
            "none none none solid",
            Edges {
                top: BorderStyle::None,
                right: BorderStyle::None,
                bottom: BorderStyle::None,
                left: BorderStyle::Solid,
            },
        ),
    ];
    for &(decl, want) in styles {
        let p = rich(&sc(&format!(
            r#"{{"tag":"div","style":{{"borderWidth":0.2,"borderStyle":"{decl}"}},"content":"x"}}"#
        )));
        let s = laid_out(&p, 424.0, 4000.0, false, false);
        let gloss = one_body(&s);
        let got = gloss.block_box.map_or(Edges::default(), |b| b.style.border_style);
        assert_eq!(want, got, "borderStyle: {decl}");
    }
}

/// One real dictionary's left rule, end to end: the used width is zero on
/// the three edges whose style is `none`, so the box takes space and draws
/// ink on one side only.
#[test]
fn a_one_sided_border_takes_space_on_that_side_alone() {
    let p = rich(&sc(concat!(
        r#"{"tag":"div","style":{"borderStyle":"none none none solid","#,
        r#""borderWidth":0.2,"borderColor":"green"},"content":"note"}"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    let used = block_box(gloss).style.border_used();
    assert_eq!(Edges { top: 0.0, right: 0.0, bottom: 0.0, left: 3.0 }, used);
    assert_eq!(BODY_LINE, gloss.advance, "a vertical border of nothing adds nothing");
    assert_eq!(3.0, gloss.pen.0 - s.origin, "and the text starts inside the rule");
}

/// `textAlign` positions a line within its block's width, and the element
/// reports the alignment so both painters can hand it to their engine.
#[test]
fn text_align_centre_and_end_position_a_line_in_its_block() {
    let p = rich(&sc(concat!(
        r#"{"tag":"div","content":["#,
        r#"{"tag":"div","style":{"textAlign":"center"},"content":"mid"},"#,
        r#"{"tag":"div","style":{"textAlign":"end"},"content":"end"},"#,
        r#"{"tag":"div","content":"start"}]}"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);
    let ink = 3.0 * BOX_EM * ADVANCE;

    assert_eq!(Align::Center, gloss[0].align);
    assert_eq!(gloss[0].pen.0 + (gloss[0].wrap_w - ink) / 2.0, gloss[0].rect.x);
    assert_eq!(Align::Trailing, gloss[1].align);
    assert_eq!(gloss[1].pen.0 + gloss[1].wrap_w - ink, gloss[1].rect.x);
    assert_eq!(Align::Leading, gloss[2].align, "and it is not inherited from a sibling");
    assert_eq!(gloss[2].pen.0, gloss[2].rect.x);
}

/// `whiteSpace: pre-line` preserves the dictionary's own newline, at the
/// paragraph's edges as well as inside it. Without it, a browser collapses
/// the edge break away and so does the panel.
#[test]
fn white_space_pre_line_preserves_a_literal_newline() {
    let cases: &[(&str, &str)] = &[
        (r#","style":{"whiteSpace":"pre-line"}"#, "\none\ntwo\n"),
        ("", "one\ntwo"),
        // Not `pre-line`: the seam has no request for turning wrapping
        // off, so the paragraph is left exactly as it was.
        (r#","style":{"whiteSpace":"nowrap"}"#, "one\ntwo"),
    ];
    for &(style, want) in cases {
        let p = rich(&sc(&format!(
            r#"{{"tag":"div"{style},"content":"\none\ntwo\n"}}"#
        )));
        let s = laid_out(&p, 424.0, 4000.0, false, false);
        assert_eq!(want, one_body(&s).text, "whiteSpace{style}");
    }
}

/// A `details`/`summary` pair is two elements, not one concatenated
/// sentence: four census dictionaries and 31k nodes used to run the summary
/// into the body it labels. Rendered expanded, with the summary carrying
/// the heading weight the spec's defaults table gives a header cell - the
/// panel has no disclosure affordance and the weight is what distinguishes
/// them.
#[test]
fn a_details_summary_pair_is_two_distinguishable_elements() {
    let theme = Theme::dark();
    let p = rich(&sc(concat!(
        r#"{"tag":"details","content":[{"tag":"summary","content":"Etymology"},"#,
        r#""from Middle Chinese"]}"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(
        vec!["Etymology", "from Middle Chinese"],
        gloss.iter().map(|e| e.text.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(BOLD_WEIGHT, gloss[0].spans[0].weight, "the summary is the heading");
    assert_eq!(theme.body_weight, gloss[1].spans[0].weight, "the body is not");
}

// ---- sense identity ----

/// Every element built from a `GlossDoc` node carries the node's path, and
/// the path resolves back to that node in that document. The panel's own
/// chrome carries none, because it was built from a `Presentation` and
/// addresses no tree.
#[test]
fn every_gloss_element_carries_a_path_that_resolves_to_its_own_node() {
    let p = rich(&sc(
        r#"[{"tag":"div","content":"to eat"},{"tag":"div","content":"to dine"}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let doc = &p.top.as_ref().unwrap().blocks[0].entries[0].doc;

    for gloss in bodies(&s) {
        let origin = gloss.origin.expect("a gloss element names its row");
        let path = origin.path.expect("and the node it renders");
        let id = path.resolve(doc).expect("which must exist in that document");
        assert_eq!(Tag::Div, doc.node(id).tag, "the block that opened the paragraph");
    }
    let head = find(&s, ElemKind::Headword);
    assert_eq!(None, head.origin, "the panel's chrome addresses no tree");
}

/// Stories 45 and 46, end to end: a hit on a sense resolves to that
/// sense's node path, and the path round trips through ticket 04's
/// renderer - it yields that sense's markup and nothing else.
///
/// This is the half of the sense picker that is expensive to retrofit. The
/// interaction is out of scope; the addressability is not.
#[test]
fn a_senses_path_round_trips_through_the_subtree_renderer() {
    let p = rich(&sc(concat!(
        r#"[{"tag":"div","content":"to eat"},"#,
        r#"{"tag":"div","content":[{"tag":"i","content":"formal"}," to dine"]}]"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let doc = &p.top.as_ref().unwrap().blocks[0].entries[0].doc;
    let gloss = bodies(&s);

    assert_eq!(2, gloss.len(), "two sibling blocks are two senses");
    let second = gloss[1].origin.and_then(|o| o.path).expect("the second sense's path");

    let picked = render_html(doc, Selection::Nodes(&[second]), RoleFilter::CARD);

    assert_eq!(vec!["<div><i>formal</i> to dine</div>".to_string()], picked);
    let whole = render_html(doc, Selection::Whole, RoleFilter::CARD);
    assert!(whole[0].contains("to eat"), "the whole entry still holds both: {whole:?}");
}

/// The other two thirds of the identity: the dictionary and the row. A
/// path alone means "the second block of some tree"; with these it means
/// "sense 2 of this 大辞林 row".
#[test]
fn a_gloss_element_names_the_dictionary_and_the_row_it_came_from() {
    let mut block = tree("\u{5927}\u{8f9e}\u{6797}", &sc(r#""to eat""#));
    block.dict_id = 7;
    block.entries[0].entry_id = 4321;
    let s = laid_out(&card_with(vec![block]), 424.0, 4000.0, false, false);

    let origin = one_body(&s).origin.expect("a gloss element names its row");
    assert_eq!(7, origin.dict_id);
    assert_eq!(4321, origin.entry_id);
}

/// A node past a `NodePath`'s reach is unaddressable rather than aliased
/// to an ancestor: `child()` refuses the seventeenth step, so the element
/// still names its row and reports no path. Aliasing would hand a sense
/// picker the wrong subtree.
#[test]
fn a_node_deeper_than_a_path_reaches_carries_no_path() {
    let deep = (0..20).fold(r#""deep""#.to_string(), |inner, _| {
        format!(r#"{{"tag":"div","content":{inner}}}"#)
    });
    let s = laid_out(&rich(&sc(&deep)), 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    let origin = gloss.origin.expect("it still names its row");
    assert_eq!(None, origin.path, "and refuses to name a node it cannot address");
    assert_eq!("deep", gloss.text, "while the text still renders");
}

/// The box model must not disturb the targets the panel already had.
///
/// One card twice, once with a plain gloss and once with every box
/// property the census ranks on it. "Unchanged" cannot mean "at the same
/// y": a box that takes space pushes what follows it down, and that is
/// the whole point of it. What must hold is that the panel's own targets
/// keep their kind, their order and their size, that the ones *above* the
/// gloss do not move at all, and that the ones below move by exactly the
/// box's own added height - once, not twice.
#[test]
fn the_box_model_leaves_the_panels_own_hit_targets_alone() {
    let boxed = concat!(
        r#"{"tag":"div","style":{"margin":0.4,"padding":0.2,"borderWidth":0.2,"#,
        r#""borderStyle":"solid","borderRadius":0.4,"backgroundColor":"green","#,
        r#""textAlign":"center"},"content":"chatting"}"#
    );
    let anki = AnkiPopupState { connected: true, ..AnkiPopupState::fresh(true) };
    let theme = Theme::dark();
    let of = |p: &Presentation| {
        scene(
            &SceneRequest {
                presentation: p,
                theme: &theme,
                max_w: 424.0,
                max_h: 4000.0,
                show_back: true,
                side_panel: false,
                anki: Some(&anki),
            },
            &mut FakeMeasure::default(),
        )
        .expect("FakeMeasure never refuses a run")
    };

    let with_gloss = |glossary: &str| {
        let mut p = with_collapsed();
        p.top.as_mut().unwrap().blocks = vec![tree("Jitendex", glossary)];
        p
    };
    let plain = of(&with_gloss(&sc(r#""chatting""#)));
    let styled = of(&with_gloss(&sc(boxed)));

    // margin 6, border 3 and padding 3, top and bottom.
    let grew = 2.0 * (6.0 + 3.0 + 3.0);
    let gloss_y = bodies(&plain)[0].pen.1;
    assert!(!plain.hits.is_empty(), "the panel does have targets to disturb");
    assert_eq!(plain.hits.len(), styled.hits.len(), "and the same ones");
    for (a, b) in plain.hits.iter().zip(&styled.hits) {
        assert_eq!(a.action, b.action, "same target, same action");
        assert_eq!((a.x, a.w, a.h), (b.x, b.w, b.h), "and the same rect");
        let moved = if a.y < gloss_y { 0.0 } else { grew };
        assert_eq!(a.y + moved, b.y, "{:?} moved by the box and no more", a.action);
    }
    assert_eq!(
        plain.anki.map(|a| (a.label, a.rect.h)),
        styled.anki.map(|a| (a.label, a.rect.h)),
        "the Anki slot keeps its label and its height"
    );
    assert_eq!(grew, styled.content_h - plain.content_h, "the box is paid once");
}

/// The one decision the two painters share, decided here so they cannot
/// decide it differently.
///
/// Neither bin has a test that can see its drawing API - Direct2D needs a
/// window - so "one stroke around the rounded box, or a fill per edge?"
/// is answered in core and asserted here.
#[test]
fn an_even_border_strokes_once_and_an_uneven_one_fills_each_edge() {
    let solid = Edges::all(BorderStyle::Solid);
    let left_only = Edges {
        top: BorderStyle::None,
        right: BorderStyle::None,
        bottom: BorderStyle::None,
        left: BorderStyle::Solid,
    };
    // (border widths, per-edge styles, one stroke of this width?)
    let cases: &[(Edges<f32>, Edges<BorderStyle>, Option<f32>)] = &[
        (Edges::all(2.0), solid, Some(2.0)),
        // No style is no border, however wide: CSS's own rule.
        (Edges::all(2.0), Edges::default(), None),
        // A width of nothing is nothing to stroke.
        (Edges::all(0.0), solid, None),
        // One real dictionary's left rule: four equal widths, but three
        // edges whose style zeroes them, so the used widths are uneven.
        (Edges::all(2.0), left_only, None),
        // Genuinely uneven widths have no single rounded path either.
        (Edges { top: 1.0, right: 2.0, bottom: 1.0, left: 2.0 }, solid, None),
    ];
    for &(border, border_style, want) in cases {
        let bx = BoxStyle { border, border_style, ..BoxStyle::default() };
        assert_eq!(want, bx.even_border(), "{border:?} / {border_style:?}");
    }
}

// ---- element construction ----

/// It must lead the list.
#[test]
fn frequency_leads_as_a_corner_so_it_shares_the_headword_line() {
    let theme = Theme::dark();
    let (elems, _) = build_elements(&one_card(&[], Some(7671)), &theme, false, false);
    match &elems[0] {
        Elem::Corner(line) => {
            assert_eq!("freq 7671", line.text);
            assert_eq!(theme.frequency_text, line.color);
        }
        _ => panic!("the frequency corner must be the first element"),
    }
}

#[test]
fn an_unranked_entry_draws_no_corner() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
    assert!(!elems.iter().any(|e| matches!(e, Elem::Corner(_))));
}

#[test]
fn part_of_speech_is_dimmed_metadata_not_body_text() {
    let theme = Theme::dark();
    let (elems, _) = build_elements(&one_card(&["noun", "suru"], Some(1)), &theme, false, false);
    let pos = elems
        .iter()
        .find_map(|e| match e {
            Elem::Text(line) if line.text.contains("noun") => Some(line),
            _ => None,
        })
        .expect("a POS line must be drawn");
    assert_eq!("noun · suru", pos.text);
    assert_eq!(theme.dimmed_text, pos.color);
    assert_ne!(theme.body_text, pos.color, "POS must not read as body text");
    assert_eq!(theme.dimmed_size, pos.size);
}

/// Every role owns its own metrics.
///
/// The sizes the roles used to borrow
/// from each other: `reading` took
/// `body_size`, POS and the dictionary
/// label both took `collapsed_size`,
/// and the frequency corner took
/// `collapsed_size` and `dimmed_text`.
#[test]
fn each_role_takes_its_own_size() {
    let theme = roled_theme();
    let (elems, _) = build_elements(&one_card(&["noun"], Some(7671)), &theme, true, false);
    let size_of = |want: &str| -> f32 {
        elems
            .iter()
            .find_map(|e| match e {
                Elem::Text(line) if line.text.contains(want) => Some(line.size),
                // The body is a gloss paragraph, not a `Line`: its style
                // rides on the spans the inline pass built.
                Elem::Gloss(flow) if flow.text.contains(want) => {
                    Some(flow.spans[0].style.size)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing holding {want:?}"))
    };
    let corner = elems
        .iter()
        .find_map(|e| match e {
            Elem::Corner(line) => Some(line),
            _ => None,
        })
        .expect("a ranked entry draws a corner");
    assert_eq!(theme.frequency_size, corner.size);
    assert_eq!(theme.reading_size, size_of("ざつだん"));
    assert_eq!(theme.dimmed_size, size_of("noun"));
    assert_eq!(theme.dict_label_size, size_of("Jitendex"));
    assert_eq!(theme.body_size, size_of("chatting"));
}

/// Weight and style travel with size.
#[test]
fn each_role_takes_its_own_weight_and_style() {
    let theme = roled_theme();
    let (_, runs) = measured(&theme, &one_card(&["noun"], Some(7671)), false);

    let freq = asked_for(&runs, "freq 7671");
    assert_eq!((theme.frequency_weight, theme.frequency_italic), (freq.weight, freq.italic));
    let head = asked_for(&runs, "雑談");
    assert_eq!((theme.headword_weight, theme.headword_italic), (head.weight, head.italic));
    let reading = asked_for(&runs, "ざつだん");
    assert_eq!((theme.reading_weight, true), (reading.weight, reading.italic));
    let pos = asked_for(&runs, "noun");
    assert_eq!((theme.dimmed_weight, true), (pos.weight, pos.italic));
    let label = asked_for(&runs, "Jitendex");
    assert_eq!((theme.dict_label_weight, theme.dict_label_italic), (label.weight, label.italic));
    let gloss = asked_for(&runs, "chatting");
    assert_eq!((theme.body_weight, theme.body_italic), (gloss.weight, gloss.italic));
    let back = asked_for(&runs, "\u{2190} Back");
    assert_eq!((theme.dict_label_weight, theme.dict_label_italic), (back.weight, back.italic));
}

/// The painter needs them too.
#[test]
fn the_scene_carries_each_run_s_weight_and_style() {
    let theme = roled_theme();
    let (s, _) = measured(&theme, &with_collapsed(), false);
    let head = find(&s, ElemKind::Headword);
    assert_eq!(theme.headword_weight, head.weight);
    assert!(!head.italic);
    let row = find(&s, ElemKind::Collapsed);
    assert_eq!(theme.collapsed_weight, row.weight);
    // A rule has no text to weight.
    let sep = find(&s, ElemKind::Separator);
    assert_eq!(REGULAR_WEIGHT, sep.weight);
    assert!(!sep.italic);
}

/// The side column is one format.
#[test]
fn the_side_column_measures_at_the_collapsed_role() {
    let theme = roled_theme();
    let (_, runs) = measured(&theme, &with_collapsed(), true);
    for text in ["See also", "雑音", "雑誌"] {
        let run = asked_for(&runs, text);
        assert_eq!(theme.collapsed_size, run.size);
        assert_eq!(theme.collapsed_weight, run.weight);
        assert_eq!(theme.collapsed_italic, run.italic);
    }
}

/// The default theme must measure
/// exactly as it did before roles
/// carried weight: regular, upright,
/// and the sizes the goldens hold.
#[test]
fn the_default_theme_measures_every_run_regular_and_upright() {
    let theme = Theme::dark();
    let (_, runs) = measured(&theme, &with_collapsed(), true);
    assert!(!runs.is_empty());
    for run in &runs {
        assert_eq!(REGULAR_WEIGHT, run.weight, "{:?} is not regular", run.text);
        assert!(!run.italic, "{:?} is not upright", run.text);
    }
}

/// And at the sizes they measured at
/// before the roles split apart.
///
/// The geometry goldens
/// (`crates/chibipop-windows/tests/goldens/geometry`) are an
/// exact-equality gate over these numbers: 13px metadata, 15px body,
/// 20px headword, a 1px rule. Both default themes hold every role at
/// the size it used to borrow, so the goldens survived the split - and
/// a default that drifts must re-bless them, which is what this test
/// says out loud.
#[test]
fn both_default_themes_keep_every_role_at_its_pre_split_size() {
    for theme in [Theme::dark(), Theme::light()] {
        assert_eq!(20.0, theme.headword_size);
        assert_eq!(theme.body_size, theme.reading_size, "reading borrowed body_size");
        assert_eq!(15.0, theme.body_size);
        for (role, size) in [
            ("dict_label", theme.dict_label_size),
            ("dimmed", theme.dimmed_size),
            ("frequency", theme.frequency_size),
        ] {
            assert_eq!(theme.collapsed_size, size, "{role} borrowed collapsed_size");
        }
        assert_eq!(13.0, theme.collapsed_size);
        assert_eq!(SEPARATOR_THICKNESS, theme.separator_height, "the rule was a 1px const");
        assert_eq!(1.0, theme.border_width);
    }
}

/// The rule the theme sets, and the
/// one it does not: `separator_height`
/// is the horizontal rule between
/// blocks, never the side column's
/// vertical one.
#[test]
fn the_theme_sets_the_separator_height_but_not_the_side_rule() {
    let theme = Theme { separator_height: 3.0, ..Theme::dark() };
    let (s, _) = measured(&theme, &with_collapsed(), false);
    let sep = find(&s, ElemKind::Separator);
    assert_eq!(3.0, sep.rect.h);
    assert_eq!(3.0, sep.advance, "the walk stacks the themed height");

    let (side, _) = measured(&theme, &with_collapsed(), true);
    assert_eq!(SEPARATOR_THICKNESS, side.side.unwrap().rule_w);
}

/// 大辞林 has no POS markup.
#[test]
fn an_entry_without_part_of_speech_draws_no_pos_line() {
    let (elems, _) = build_elements(&one_card(&[], Some(1)), &Theme::dark(), false, false);
    assert!(!elems
        .iter()
        .any(|e| matches!(e, Elem::Text(line) if line.text.contains('·'))));
}

#[test]
fn the_headword_is_a_headword_element_not_text() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
    assert!(
        elems.iter().any(|e| matches!(e, Elem::Headword { .. })),
        "expected a Headword element for the headword"
    );
}

#[test]
fn headword_prefix_u16_is_zero_without_anki_marks() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
    let hw = elems.iter().find_map(|e| match e {
        Elem::Headword { prefix_u16, .. } => Some(*prefix_u16),
        _ => None,
    });
    assert_eq!(Some(0), hw);
}

#[test]
fn show_back_adds_a_back_button_element() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), true, false);
    assert!(matches!(&elems[0], Elem::BackButton(_)));
}

#[test]
fn no_back_button_without_show_back() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
    assert!(!elems.iter().any(|e| matches!(e, Elem::BackButton(_))));
}

#[test]
fn is_kanji_covers_cjk_unified() {
    assert!(is_kanji('\u{98DF}'));
    assert!(is_kanji('\u{4E00}'));
    assert!(is_kanji('\u{9FFF}'));
    assert!(!is_kanji('\u{3042}'));
    assert!(!is_kanji('a'));
}

/// No marks on collapsed rows.
#[test]
fn collapsed_rows_carry_no_dupe_marks() {
    let (elems, _) = build_elements(&with_collapsed(), &Theme::dark(), false, false);
    for e in &elems {
        if let Elem::Collapsed(_, line) = e {
            assert!(!line.text.starts_with('\u{2713}'), "no check marks on collapsed rows");
        }
    }
}

#[test]
fn side_panel_false_keeps_collapsed_rows_inline() {
    let (elems, side) = build_elements(&with_collapsed(), &Theme::dark(), false, false);
    assert!(side.is_empty());
    assert!(elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
}

#[test]
fn side_panel_true_moves_collapsed_rows_to_side() {
    let (elems, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true);
    assert!(!elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
    assert_eq!(2, side.len());
    assert!(side[0].text.contains('\u{96D1}'));
}

#[test]
fn side_entries_carry_expand_indices() {
    let (_, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true);
    assert_eq!(0, side[0].idx);
    assert_eq!(1, side[1].idx);
}

#[test]
fn side_entries_show_headword_only() {
    let (_, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true);
    assert!(!side[0].text.contains("noise"));
    assert!(!side[1].text.contains("magazine"));
}

// ---- one label per dictionary ----

/// The defect ticket 16 fixes: a headword with eleven 大辞林 rows used to
/// draw eleven 大辞林 headings, one gloss under each.
#[test]
fn three_rows_from_one_dictionary_draw_one_label() {
    let p = card_with(vec![rows("大辞林", &[&["ある"], &["いる"], &["うる"]])]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert_eq!(
        1,
        s.elems.iter().filter(|e| e.text == "大辞林").count(),
        "one dictionary, one heading: {:?}",
        texts(&s)
    );
    assert_eq!(
        vec!["雑談", "大辞林", "1. ある", "2. いる", "3. うる"],
        texts(&s),
        "the headword, one label, and three numbered gloss entries"
    );
}

/// Yomitan's `<ol>` holds one item per matched term-bank row, and Hoshi
/// Reader emits the list at all only when a dictionary contributed more
/// than one row - so a lone row carries no number, and the glossary items
/// inside it are never numbered either.
#[test]
fn a_single_row_dictionary_is_not_numbered() {
    let p = card_with(vec![block("Jitendex", &["chatting", "idle talk"])]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert_eq!(vec!["雑談", "Jitendex", "chatting; idle talk"], texts(&s));
}

#[test]
fn two_dictionaries_draw_two_labels_in_the_cards_order() {
    let p = card_with(vec![
        rows("大辞林", &[&["ある"], &["いる"]]),
        block("Jitendex", &["chatting"]),
    ]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert_eq!(
        vec!["雑談", "大辞林", "1. ある", "2. いる", "Jitendex", "chatting"],
        texts(&s)
    );
}

/// A row's tags are dimmed metadata, like the card's own tag line. An empty
/// set draws nothing: that is how `present` says "the row above already
/// printed this one".
#[test]
fn a_rows_tags_draw_a_dimmed_line_and_an_empty_set_draws_none() {
    let theme = Theme::dark();
    let p = card_with(vec![GlossBlock {
        dict_name: "大辞林".into(),
        dict_id: crate::present::NO_ROW,
        entries: vec![entry(&["ある"], &["noun", "suru"]), entry(&["いる"], &[])],
    }]);
    let (elems, _) = build_elements(&p, &theme, false, false);
    let tag = elems
        .iter()
        .find_map(|e| match e {
            Elem::Text(line) if line.text.contains("noun") => Some(line),
            _ => None,
        })
        .expect("a tag line must be drawn");
    assert_eq!("noun · suru", tag.text);
    assert_eq!(theme.dimmed_text, tag.color);
    assert_eq!(theme.dimmed_size, tag.size);

    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert_eq!(vec!["雑談", "大辞林", "noun · suru", "1. ある", "2. いる"], texts(&s));
}

/// A dictionary that matched but rendered nothing still names itself, and
/// draws no empty body line - the `minimal_edge` golden's shape.
#[test]
fn a_dictionary_with_no_glosses_draws_only_its_label() {
    let p = card_with(vec![block("大辞林", &[])]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert_eq!(vec!["雑談", "大辞林"], texts(&s));
}

// ---- the Anki label ----

#[test]
fn anki_button_label_is_none_when_disabled() {
    let p = one_card(&[], None);
    assert!(anki_button_label(&p, &Theme::dark(), &AnkiPopupState::disabled()).is_none());
}

#[test]
fn anki_button_label_shows_add_by_default() {
    let theme = Theme::dark();
    let anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
    let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("\u{ff0b} Add to Anki", text);
    assert_eq!(theme.dict_label_text, color);
}

#[test]
fn anki_button_label_shows_adding_while_in_flight() {
    let theme = Theme::dark();
    let anki = AnkiPopupState {
        enabled: true,
        connected: true,
        adding: true,
        ..AnkiPopupState::disabled()
    };
    let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("Adding\u{2026}", text);
    assert_eq!(theme.dimmed_text, color);
}

#[test]
fn anki_button_label_flags_a_known_dupe() {
    let theme = Theme::dark();
    let mut anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
    anki.dupes.insert("\u{96D1}\u{8AC7}".to_string());
    let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("\u{ff0b} Add to Anki (duplicate)", text);
    assert_eq!(theme.dict_label_text, color);
}

#[test]
fn anki_button_label_shows_added_after_success() {
    let theme = Theme::dark();
    let mut anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
    anki.added.insert("\u{96D1}\u{8AC7}".to_string());
    let (text, _) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("\u{2713} Added", text);
}

/// Adding outranks both markers.
#[test]
fn anki_button_label_prefers_adding_over_dupe_or_added() {
    let theme = Theme::dark();
    let mut anki = AnkiPopupState {
        enabled: true,
        connected: true,
        adding: true,
        ..AnkiPopupState::disabled()
    };
    anki.dupes.insert("\u{96D1}\u{8AC7}".to_string());
    anki.added.insert("\u{96D1}\u{8AC7}".to_string());
    let (text, _) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("Adding\u{2026}", text);
}

/// Checking outranks the add label.
#[test]
fn anki_button_label_shows_checking() {
    let theme = Theme::dark();
    let anki = AnkiPopupState {
        enabled: true,
        connected: true,
        checking: true,
        ..AnkiPopupState::disabled()
    };
    let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("Checking\u{2026}", text);
    assert_eq!(theme.dimmed_text, color);
}

#[test]
fn anki_button_label_shows_failed() {
    let theme = Theme::dark();
    let anki = AnkiPopupState {
        enabled: true,
        connected: true,
        failed: true,
        ..AnkiPopupState::disabled()
    };
    let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
    assert_eq!("\u{2717} Failed to add", text);
    assert_eq!(theme.dimmed_text, color);
}

/// Disconnected hides the button.
#[test]
fn anki_button_label_is_none_when_disconnected() {
    let anki = AnkiPopupState { enabled: true, connected: false, ..AnkiPopupState::disabled() };
    assert!(anki_button_label(&one_card(&[], None), &Theme::dark(), &anki).is_none());
}

// ---- scrolling ----

#[test]
fn content_that_fits_cannot_scroll() {
    assert_eq!(0, max_scroll(200, 300));
    assert_eq!(0, max_scroll(300, 300));
}

#[test]
fn max_scroll_is_the_overflow() {
    assert_eq!(200, max_scroll(500, 300));
}

#[test]
fn content_that_fits_has_no_thumb() {
    assert_eq!(None, scrollbar_thumb(300, 200, 300, 0));
    assert_eq!(None, scrollbar_thumb(300, 300, 300, 0));
}

#[test]
fn the_thumb_is_proportional_and_starts_at_the_top() {
    let (top, h) = scrollbar_thumb(300, 600, 300, 0).unwrap();
    assert_eq!(0, top);
    assert_eq!(150, h, "half the content is visible, so half the track");
}

/// Else it looks unscrolled.
#[test]
fn the_thumb_ends_flush_with_the_track_at_max_scroll() {
    let (top, h) = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
    assert_eq!(300, top + h);
}

/// Else a 1px sliver.
#[test]
fn the_thumb_has_a_floor() {
    let (_, h) = scrollbar_thumb(300, 100_000, 300, 0).unwrap();
    assert_eq!(SCROLLBAR_MIN_THUMB, h);
}

/// The floor must not overhang.
#[test]
fn a_floored_thumb_still_ends_inside_the_track() {
    let m = max_scroll(100_000, 300);
    let (top, h) = scrollbar_thumb(300, 100_000, 300, m).unwrap();
    assert!(top + h <= 300, "thumb {top}+{h} escaped a 300px track");
    assert_eq!(300, top + h);
}

#[test]
fn a_scroll_beyond_the_end_is_treated_as_the_end() {
    let a = scrollbar_thumb(300, 600, 300, 999_999).unwrap();
    let b = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
    assert_eq!(b, a);
}

/// A short track still fits.
#[test]
fn a_track_shorter_than_the_floor_still_fits() {
    let (top, h) = scrollbar_thumb(10, 600, 300, 0).unwrap();
    assert!(h <= 10, "thumb {h} in a 10px track");
    assert!(top + h <= 10);
}

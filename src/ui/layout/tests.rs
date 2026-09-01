//! Layout against fixed metrics (ADR-0011, layer one).
//!
//! `FakeMeasure` wraps at a whole number of pixels per UTF-16 unit, so
//! every expectation below is arithmetic a reader can redo by hand. No
//! font, no platform: these run in both CI jobs, forever.
//!
//! The corpus sweep is a child of this module ([`sweep`]) because it renders
//! against the same `FakeMeasure` and reuses these fixtures' own builders;
//! it reads a corpus directory from the environment and never runs in CI.

use super::*;
// Every submodule these tests reach into. They are not a test of the
// module's face: each asserts what one pass measured against fixed
// metrics, so they name the private vocabulary the submodules share
// exactly as they did when all of it was one file.
use super::{chrome::*, flow::*, gloss::*, image::*, marker::*, pass::*, pill::*, ruby::*, style::*};
use crate::dict::gloss::{render_html, RoleFilter, Selection, Tag};
use crate::dict::media::{Intrinsic, MediaFormat, MediaKey};
use crate::present::{Card, CollapsedRow, GlossBlock, GlossEntry};

mod sweep;

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

/// Characters a real shaper refuses to
/// break beside: UAX #14's class GL,
/// of which one is written here.
///
/// U+00A0 NO-BREAK SPACE is what an
/// inline box buys its horizontal room
/// with ([`PILL_SPACER`]) and what an
/// image buys its width with
/// ([`IMAGE_SPACER`]), and *both* rest
/// on the break being forbidden: a wrap
/// that split a pill from its own
/// padding, or left a `margin-right`'s
/// gap on one line and the word it
/// separates on the next, would draw
/// the box in one place and the room in
/// another. A fake that broke there
/// would mismeasure precisely the thing
/// it exists to model, exactly as one
/// charging [`zero_advance`] a full
/// unit would.
fn glue(c: char) -> bool {
    c == '\u{a0}'
}

/// One unit of the fake's wrap: one
/// UTF-16 unit of one span.
struct Unit {
    span: usize,
    advance: f32,
    h: f32,
    /// May a line break happen
    /// immediately before it?
    breakable: bool,
}

/// The fake's greedy wrap.
///
/// Lays one rectangle per UTF-16 unit
/// end to end and breaks when the next
/// would not fit, so every expectation
/// below stays arithmetic a reader can
/// redo by hand. A one-span run of text
/// carrying no [`glue`] reduces to
/// `floor(max_w / advance)` units per
/// line, which is what this fake always
/// did.
///
/// The unit it breaks *at* is UAX #14's
/// answer and not "anywhere", because
/// two of this renderer's reservations
/// depend on the difference ([`glue`]).
/// Two of the algorithm's rules are
/// enough for that: no break after glue
/// (LB12) and none before it either
/// unless a space comes first (LB12a).
/// Everything else is still a break
/// opportunity, which is what keeps a
/// plain run wrapping per unit.
fn wrap(run: MeasureRun<'_>) -> (Vec<Frag>, Measured) {
    let max_w = run.max_w.max(1.0);
    let mut units: Vec<Unit> = Vec::new();
    let mut prev: Option<char> = None;
    for (span, s) in run.spans.iter().enumerate() {
        let advance = if zero_advance(s.text) { 0.0 } else { s.size * ADVANCE };
        let h = s.size * LINE_H;
        for c in s.text.chars() {
            // LB12 and LB12a, over the
            // run's whole text: a span
            // boundary is not a line
            // boundary (ADR-0013), so the
            // character before this one
            // may belong to the span
            // before it.
            let after_glue = prev.is_some_and(glue);
            let before_glue = glue(c) && !prev.is_some_and(|p| p == ' ' || p == '\t');
            let breakable = !after_glue && !before_glue;
            for i in 0..c.len_utf16() {
                // A surrogate pair is two
                // units of one character,
                // and no break goes
                // between them.
                units.push(Unit { span, advance, h, breakable: breakable && i == 0 });
            }
            prev = Some(c);
        }
    }

    let mut frags: Vec<Frag> = Vec::new();
    let (mut line, mut x, mut from) = (0usize, 0.0f32, 0usize);
    let mut at = 0usize;
    while at < units.len() {
        // One chunk: this break
        // opportunity to the next. Plain
        // text gives one unit per chunk,
        // so it still wraps per unit; a
        // run of glue gives one chunk
        // holding the whole reservation
        // and the units it is fused to.
        let mut end = at + 1;
        while end < units.len() && !units[end].breakable {
            end += 1;
        }
        let chunk = &units[at..end];
        let w: f32 = chunk.iter().map(|u| u.advance).sum();
        // Never at the head of a line: a
        // measurer that cannot wrap
        // narrower than one chunk
        // overflows rather than loops. A
        // chunk that advances nothing
        // always fits.
        if x > 0.0 && w > 0.0 && x + w > max_w {
            line += 1;
            x = 0.0;
        }
        for u in chunk {
            match frags.last_mut() {
                Some(f) if f.span == u.span && f.line == line => f.units += 1,
                _ => frags.push(Frag {
                    span: u.span,
                    line,
                    x,
                    from,
                    units: 1,
                    advance: u.advance,
                    h: u.h,
                }),
            }
            x += u.advance;
            from += 1;
        }
        at = end;
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
    row_media(glossary, tags, Vec::new())
}

/// One matched row whose dictionary shipped `media`.
///
/// The sizing pass reads what the build recorded rather than the bytes
/// (`present::GlossEntry::media`), so an image fixture is a path and four
/// numbers - no archive, no decoder, no database.
fn row_media(
    glossary: &str,
    tags: &[&str],
    media: Vec<(String, Intrinsic)>,
) -> GlossEntry {
    let doc = std::sync::Arc::new(crate::dict::gloss::GlossDoc::parse(glossary));
    GlossEntry {
        entry_id: crate::present::NO_ROW,
        glosses: crate::dict::gloss::plain_items(&doc),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        doc,
        media,
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
            render: RenderSettings::default(),
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
            render: RenderSettings::default(),
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
            render: RenderSettings::default(),
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
            render: RenderSettings::default(),
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
            render: RenderSettings::default(),
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
            render: RenderSettings::default(),
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
            render: RenderSettings::default(),
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
                render: RenderSettings::default(),
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

/// A header cell is bold beside its row's data cell, per the spec's
/// defaults table - the shape a real conjugation table has.
///
/// Ticket 07 wrote this when a row was one paragraph. Ticket 10 grids it,
/// so the two cells are now two paragraphs side by side on one row; the
/// weight is what the test is about, and it survived the grid.
#[test]
fn a_header_cell_is_bold_beside_its_data_cell() {
    let p = rich(&sc(
        r#"[{"tag":"table","content":{"tag":"tr","content":[{"tag":"th","content":"past"},{"tag":"td","content":"\u98f2\u3093\u3060"}]}}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(2, gloss.len(), "a row is a grid: one paragraph per cell");
    assert!(gloss.iter().all(|e| e.lines == 1));
    assert_eq!(
        vec![700, Theme::dark().body_weight],
        gloss.iter().map(|e| e.spans[0].weight).collect::<Vec<_>>()
    );
    assert_eq!(gloss[0].pen.1, gloss[1].pen.1, "both cells start at their row's top");
    assert!(gloss[0].pen.0 < gloss[1].pen.0, "and the header cell leads");
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

/// The block box an element is, which must exist.
fn block_box(e: &SceneElem) -> &ElemBox {
    e.block_box.as_ref().expect("this element must carry a block box")
}

/// Every block-box element of a scene, in draw order.
///
/// A block's box is a container around **every** paragraph the block
/// emits, so it is a textless element of its own and never a field on
/// the paragraph inside it (`ElemKind::Block`).
fn block_boxes(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Block).collect()
}

/// The one block-box element of a scene with exactly one.
fn one_block_box(s: &PopupScene) -> &SceneElem {
    let found = block_boxes(s);
    assert_eq!(1, found.len(), "expected one block box, got {found:?}");
    found[0]
}

/// The box drawn around the paragraph holding `text`.
///
/// A box leads its own body in draw order, so the box a paragraph sits
/// in is the nearest one before it - nearest rather than first, because
/// boxes nest.
fn box_around<'a>(s: &'a PopupScene, text: &str) -> &'a SceneElem {
    let at = s
        .elems
        .iter()
        .position(|e| e.kind == ElemKind::Text && e.text == text)
        .unwrap_or_else(|| panic!("no gloss element holding {text:?}"));
    s.elems[..at]
        .iter()
        .rev()
        .find(|e| e.kind == ElemKind::Block)
        .unwrap_or_else(|| panic!("nothing boxes {text:?}"))
}

/// The acceptance geometry: the walk advances by the box's *outer*
/// height, margins and all, while the paragraph inside it stays the
/// height of its own text.
///
/// The advance is the **box's**, not the paragraph's, because the box is
/// the container: it is what the panel stacks, and what it stacks after
/// it starts below the box's own margin.
#[test]
fn a_box_with_margin_and_padding_advances_the_walk_by_its_outer_height() {
    let p = rich(&sc(
        r#"{"tag":"div","style":{"margin":0.4,"padding":0.2},"content":"boxed"}"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);
    let outer = one_block_box(&s);

    // margin 6, padding 3, on all four edges; one 30px line inside.
    assert_eq!(BODY_LINE, gloss.rect.h, "the ink box is the text, as it always was");
    assert_eq!(BODY_LINE, gloss.advance, "and the paragraph advances by its own line");
    assert_eq!(
        6.0 + 3.0 + BODY_LINE + 3.0 + 6.0,
        outer.advance,
        "the box's advance is the outer height"
    );
    // The border box is the fill and the stroke: padding in, margin out.
    assert_eq!(3.0 + BODY_LINE + 3.0, block_box(outer).rect.h);
    // And the paragraph sits inside it, at the content edge.
    assert_eq!(6.0 + 3.0, gloss.pen.0 - s.origin);
    assert_eq!(3.0, gloss.pen.1 - block_box(outer).rect.y, "padding under the border box's top");
}

/// The other half of "where a block's box belongs": a block emitting
/// several paragraphs draws **one** box around all of them, as a browser
/// does. Not one per paragraph - CSS gives the block one principal box -
/// and not one around the first, which is what the walk used to do.
#[test]
fn a_block_wrapping_several_paragraphs_draws_one_box_around_all_of_them() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","style":{"padding":0.2,"borderWidth":0.2,"##,
        r##""borderStyle":"solid","backgroundColor":"#1e3a5f"},"content":["##,
        r##"{"tag":"div","content":"one"},{"tag":"div","content":"two"},"##,
        r##"{"tag":"div","content":"three"}]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let inner = bodies(&s);
    let outer = one_block_box(&s);

    assert_eq!(3, inner.len(), "three paragraphs");
    assert_eq!(1, block_boxes(&s).len(), "and one box, not three and not one of three");
    // padding 3 and a 3px rule per edge; three 30px lines with the
    // panel's own gap between them, and none above the first.
    let body_h = 3.0 * BODY_LINE + 2.0 * LINE_GAP;
    assert_eq!(3.0 + 3.0 + body_h + 3.0 + 3.0, outer.advance);
    let rect = block_box(outer).rect;
    assert_eq!(
        SceneRect {
            x: s.origin,
            // The chrome the panel drew above it decides the y; what
            // this pins is that the first line sits one border and one
            // padding inside the box's own top.
            y: inner[0].pen.1 - 6.0,
            w: s.content_w,
            h: 6.0 + body_h + 6.0,
        },
        rect,
        "one border box, around all three lines"
    );
    assert_eq!(LINE_GAP, outer.top_gap, "the panel's gap sits outside the border");
    // Every one of them is inset by the box, and every one of them is
    // narrowed by it: a box spanning paragraphs insets each.
    for para in &inner {
        assert_eq!(s.origin + 6.0, para.pen.0);
        assert_eq!(s.content_w - 12.0, para.wrap_w);
        assert_eq!(None, para.block_box, "the box belongs to the block, not to a line");
    }
    assert_eq!(BODY_LINE + LINE_GAP, inner[1].pen.1 - inner[0].pen.1);
    assert_eq!(
        rect.y + rect.h - 6.0,
        inner[2].pen.1 + BODY_LINE,
        "and the last line ends one padding and one border above the box's bottom"
    );
}

/// The defect ticket 13's author found and did not fix: **a block lost
/// its own box when its first child opened a line.** A `span` carrying
/// `data.content` beside another span opens a paragraph (ticket 01's
/// sense separator), the box used to attach to the first paragraph the
/// block emitted, and there was none - the one the block opened was
/// still empty when the `span`'s `open` flushed it, and `flush` drops an
/// empty paragraph and its box with it. So a bordered, filled `div` drew
/// nothing at all.
///
/// The sibling is a `span` rather than a bare string on purpose: beside
/// bare sentence text the marker would stay in its line
/// (`GlossDoc::prose`) and the defect's trigger - the first child
/// opening a line - would never fire.
///
/// Jitendex's `div[data-sc-class="extra-box"]` over `data.content`
/// children is exactly this shape, and ticket 17's fold gives it
/// 0.4rem/0.5rem padding - so this is the difference between ticket 08's
/// goal being met on real data and not.
#[test]
fn a_block_whose_first_child_opens_a_line_still_draws_its_box() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","style":{"padding":0.2,"borderWidth":0.2,"##,
        r##""borderStyle":"solid","borderColor":"#7f8c99","borderRadius":0.4,"##,
        r##""backgroundColor":"#1e3a5f"},"content":["##,
        r##"{"tag":"span","data":{"content":"misc-info"},"content":"dated"},"##,
        r##"{"tag":"span","content":" and the body after it"}]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let outer = one_block_box(&s);
    let style = block_box(outer).style;

    // The box the dictionary declared, drawn once.
    assert_eq!(Edges::all(3.0), style.padding);
    assert_eq!(Edges::all(3.0), style.border_used());
    assert_eq!((0x7f, 0x8c, 0x99), style.border_color);
    assert_eq!(6.0, style.radius);
    assert_eq!(Some((0x1e, 0x3a, 0x5f)), style.background);
    // The marker still opens its line, so the `span` and the text after
    // it are one paragraph and the `div`'s own first paragraph is the
    // empty one that used to swallow the box.
    let inner = bodies(&s);
    assert_eq!(1, inner.len(), "one paragraph, opened by the marker");
    assert_eq!("dated and the body after it", inner[0].text);
    assert_eq!(
        SceneRect {
            x: s.origin,
            y: inner[0].pen.1 - 6.0,
            w: s.content_w,
            h: 6.0 + BODY_LINE + 6.0,
        },
        block_box(outer).rect,
        "and the box frames that line"
    );
    assert_eq!(s.origin + 6.0, inner[0].pen.0, "inset by the border and the padding");
}

/// The first neighbouring case: **a nested block that has its own box**
/// gets its own container, inside the one around it. The outer box
/// narrows the width once and the inner one is measured against what is
/// left, which is CSS's containing block - so neither pays the other's
/// lead and neither overflows it.
#[test]
fn a_box_inside_a_box_is_inset_and_narrowed_by_the_one_around_it() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","style":{"padding":0.4,"backgroundColor":"#111111"},"content":["##,
        r##"{"tag":"div","style":{"padding":0.2,"backgroundColor":"#222222"},"##,
        r##""content":"inner"}]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let found = block_boxes(&s);
    assert_eq!(2, found.len(), "two blocks declared a box, so two boxes");
    let (outer, inner) = (found[0], found[1]);
    let gloss = one_body(&s);

    // Outer padding 6, inner padding 3, one 30px line at the middle.
    assert_eq!(BODY_LINE + 6.0, inner.advance, "the inner box, padding and all");
    assert_eq!(BODY_LINE + 6.0 + 12.0, outer.advance, "and the outer around that");
    assert_eq!(s.origin, block_box(outer).rect.x);
    assert_eq!(s.content_w, block_box(outer).rect.w);
    assert_eq!(s.origin + 6.0, block_box(inner).rect.x, "inset by the outer padding");
    assert_eq!(s.content_w - 12.0, block_box(inner).rect.w, "and narrowed by both edges");
    assert_eq!(block_box(outer).rect.y + 6.0, block_box(inner).rect.y);
    assert_eq!(s.origin + 9.0, gloss.pen.0, "the text is inside both");
    assert_eq!(s.content_w - 18.0, gloss.wrap_w);
}

/// The second: **a block containing a table.** A table is a
/// `Piece::Table` and not a `Flow`, and a box's body is a list of pieces
/// of every kind, so the grid is framed by exactly the same code that
/// frames a paragraph.
///
/// The two boxes size differently, and that is CSS: a `div` is `display:
/// block` and takes its container's width, while a table with no
/// declared width shrinks to fit its own grid.
#[test]
fn a_box_around_a_table_frames_the_grid_it_holds() {
    let s = gridded(
        &format!(
            r##"{{"tag":"div","style":{{"padding":0.5,"backgroundColor":"#111111"}},"content":[{}]}}"##,
            table(&[tr(&["a"]), tr(&["b"])])
        ),
        424.0,
    );
    let outer = block_box(one_block_box(&s));
    let g = grid(&s);
    let pad = 0.5 * GRID_EM;

    assert_eq!(pad + g.rect.h + pad, outer.rect.h, "the box wraps the whole grid");
    assert_eq!(outer.rect.y + pad, g.rect.y, "which starts one padding inside it");
    assert_eq!(s.origin + pad, g.pen.0, "and the grid is inset by the padding");
    assert_eq!(s.content_w, outer.rect.w, "the block takes the width it was offered");
    assert!(g.rect.w < outer.rect.w, "and the grid takes only what its cells need");
}

/// The third: **a block containing only an image.** An image is inline
/// content - `Tag::Img` is inline and a gaiji is a character - so it
/// takes room on a line rather than opening one, and the box frames that
/// line. The image element itself advances nothing, because the
/// paragraph it reserved its room in already stacked it.
#[test]
fn a_box_around_only_an_image_frames_the_line_it_sits_on() {
    let p = imaged(
        concat!(
            r##"{"tag":"div","style":{"padding":0.2,"backgroundColor":"#111111"},"##,
            r##""content":{"tag":"img","path":"g/x.png"}}"##
        ),
        &[("g/x.png", recorded(MediaFormat::Png, 20.0, 10.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let outer = block_box(one_block_box(&s));
    let img = one_image(&s);
    let host = image_host(&s);
    let pad = 0.2 * BOX_EM;

    assert_eq!(pad + host.rect.h + pad, outer.rect.h, "the box frames the image's line");
    assert_eq!(s.origin + pad, host.pen.0);
    assert_eq!(s.origin + pad, img.rect.x, "the asset composites inside the padding");
    assert_eq!(0.0, img.advance, "and adds nothing to the box's height");
}

/// A boxed block **closes** its line, and it is the second tag shape
/// that does; `summary` is the other. A box has to end somewhere: text
/// written after the `div` is not the `div`'s content and a browser
/// draws it outside the border.
///
/// And a box establishes a coordinate system for its **body** and for
/// nothing else. The run after it takes the enclosing block's own
/// context back - the list indent and the inherited alignment - which is
/// the leak a container introduces if it does not restore what it
/// borrowed.
#[test]
fn a_boxed_block_closes_its_line_and_gives_the_next_run_its_parents_context() {
    let p = rich(&sc(concat!(
        r##"{"tag":"ul","content":{"tag":"li","style":{"textAlign":"center"},"content":["##,
        r##"{"tag":"div","style":{"padding":0.4,"backgroundColor":"#111111"},"##,
        r##""content":"boxed"}," after"]}}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let runs = bodies(&s);

    assert_eq!(
        vec!["boxed", "after"],
        runs.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        "the box closed its line, so the run after it is its own paragraph"
    );
    // Inside the box: the list's indent plus the box's padding.
    assert_eq!(s.origin + LEVEL + 6.0, runs[0].pen.0);
    // After it: the list's indent alone, and the item's own alignment.
    assert_eq!(s.origin + LEVEL, runs[1].pen.0, "the indent came back");
    assert_eq!(s.content_w - LEVEL, runs[1].wrap_w);
    assert_eq!(Align::Center, runs[1].align, "and so did the inherited alignment");
    assert_eq!(Align::Center, runs[0].align, "which the box's body had too");
    // The bullet is spent on the box's own first line, at the list's
    // content edge, and the run after it is not marked twice.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), marker_x(runs[0]));
    assert!(runs[1].marker.is_empty(), "a marker is owed once");
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
    // Each margin is its own block's, so each is on that block's own
    // box: a margin is never a paragraph's.
    let outer = block_boxes(&s);

    assert_eq!(2, gloss.len(), "two sibling blocks, two paragraphs");
    assert_eq!(2, outer.len(), "and two boxes, one per block that declared one");
    assert_eq!(BODY_LINE + 6.0, outer[0].advance, "its own bottom margin");
    assert_eq!(6.0 + BODY_LINE, outer[1].advance, "and its own top margin");
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

    // "noun" is four units at 7.5, with 3 of border and 3 of padding a
    // side bought as advance in the run itself, so the box is the run:
    // it starts at the pen and ends where the text after it starts.
    // Ticket 08 drew the same 42 wide outset 6 to the left of the pen,
    // over a neighbour's glyphs at both ends.
    assert_eq!(gloss.pen.0, pill.rect.x);
    assert_eq!(gloss.pen.1 - 6.0, pill.rect.y, "vertically it is still an outset");
    assert_eq!(4.0 * BOX_EM * ADVANCE + 12.0, pill.rect.w);
    assert_eq!(BODY_LINE + 12.0, pill.rect.h);
    assert_eq!(
        "\u{a0}\u{a0}noun\u{a0}\u{a0} a word", gloss.text,
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

/// A defect ticket 17's author found against real Jitendex data: a node
/// carrying `data.content` opens a block however inline its tag is
/// (`GlossDoc::has_marker`), so a pill carrying one used to carry the
/// *same* resolved box twice - once as `block_box`, once in
/// `inline_boxes` - and a bin looping over `SceneElem::boxes()` painted
/// it twice. Jitendex's `span[data-sc-class="tag"]` is exactly this
/// shape: a `data.content` key and a CSS pill.
///
/// The box follows the tag, and `span` is inline by the spec's own
/// division, so the box is the pill's and never the paragraph's. And
/// beside bare sentence text the marker opens no line at all
/// (`GlossDoc::prose`): a marked pill inside a sentence is markup, not a
/// sense separator, so the whole run is one paragraph.
#[test]
fn a_pill_carrying_a_content_marker_draws_one_box_and_not_two() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":["before",{"tag":"span","##,
        r##""data":{"content":"misc-info"},"##,
        r##""style":{"backgroundColor":"#565656","borderRadius":0.3,"padding":0.2},"##,
        r##""content":"noun"}," a word"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len(), "a marked pill amid prose breaks no line");
    // The pill's 3 of padding a side is now a no-break space at each end
    // of its run, which is what makes the room it paints over room the
    // text after it cleared (`pill::PILL_SPACER`).
    assert_eq!("before\u{a0}noun\u{a0} a word", gloss[0].text);

    let pill = gloss[0];
    assert_eq!(1, pill.boxes().count(), "one pill, one box - a bin paints `boxes()`");
    assert_eq!(None, pill.block_box, "an inline tag's box is its own, marker or not");
    assert_eq!(Some((0x56, 0x56, 0x56)), pill.inline_boxes[0].style.background);
    // And it hugs its own run rather than its paragraph: "noun" is four
    // units at 7.5, plus the 3 of padding its own two spacers bought.
    assert_eq!(4.0 * BOX_EM * ADVANCE + 6.0, pill.inline_boxes[0].rect.w);
    assert_eq!(BODY_LINE + 6.0, pill.inline_boxes[0].rect.h);
}

/// The defect a reader of 雑談 saw: Jitendex writes the example keyword
/// as a marked `span` inside the sentence
/// (`data.content = "example-keyword"`, 51 062 nodes), and ticket 01's
/// marker line break cut the sentence after every word before the
/// keyword - `ぜひ`, then a fresh line for the rest. Beside bare
/// sentence text a marker separates nothing (`GlossDoc::prose`), so the
/// sentence is one paragraph and the keyword's readings still ride
/// above their bases.
#[test]
fn an_example_keyword_amid_its_sentence_breaks_no_line() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","data":{"content":"example-sentence-a"},"content":"##,
        r##"{"tag":"span","content":["ぜひ","##,
        r##"{"tag":"span","data":{"content":"example-keyword"},"content":["##,
        r##"{"tag":"ruby","content":["雑",{"tag":"rt","content":"ざつ"}]},"##,
        r##"{"tag":"ruby","content":["談",{"tag":"rt","content":"だん"}]}]},"##,
        r##""でもしにいらしてください。"]}}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len(), "the sentence is one paragraph");
    // The word joiners are the ruby glue every base wears.
    assert_eq!("ぜひ雑\u{2060}談\u{2060}でもしにいらしてください。", gloss[0].text);
    assert_eq!(2, gloss[0].ruby.len(), "and both readings survive");
}

/// The footnote half of the same rule. Jitendex ends an example's
/// translation with a footnote mark - a marked `span`
/// (`data.content = "attribution-footnote"`, 9 784 of them trail their
/// sentence) - and wraps the sentence itself whole (`span lang="en"`), so
/// no bare string stands beside the mark and `GlossDoc::prose` alone
/// exempted nothing: the marker break put `[1]` on a line of its own.
/// Prose the mark trails is prose all the same
/// (`GlossDoc::inline_prose`), and the shape here is a corpus node
/// verbatim.
#[test]
fn a_trailing_attribution_footnote_stays_on_its_sentences_line() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","data":{"content":"example-sentence-b"},"content":["##,
        r##"{"tag":"span","lang":"en","content":"He still holds the heavyweight title."},"##,
        r##"{"tag":"span","data":{"content":"attribution-footnote"},"content":"[1]"}]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len(), "the sentence and its footnote are one paragraph");
    assert_eq!("He still holds the heavyweight title.[1]", gloss[0].text);
}

/// Where each of `elem`'s spans landed, as a bin's own re-measure answers
/// it.
///
/// Both bins re-measure an element's own spans to paint it
/// (`popup::paint::run_of`, `ui::render::draw_elem`), so this - and not
/// the walk's own `Measured` - is the geometry a background can be
/// compared against. Room a core pass added *after* the wrap would be
/// absent here, which is exactly the failure the whole reservation is
/// built to avoid.
fn painted_spans(elem: &SceneElem) -> Measured {
    let spans: Vec<StyledSpan<'_>> = elem
        .spans
        .iter()
        .map(|s| StyledSpan {
            text: &elem.text[s.at as usize..(s.at + s.len) as usize],
            font: "",
            size: s.size,
            weight: s.weight,
            italic: s.italic,
            color: s.color,
        })
        .collect();
    let mut out = Measured::default();
    FakeMeasure::default()
        .measure(MeasureRun { spans: &spans, max_w: elem.wrap_w }, &mut out)
        .expect("FakeMeasure never refuses a run");
    out
}

/// One span's box on the first line it touched, by index.
fn painted(boxes: &Measured, span: u32) -> SpanBox {
    *boxes
        .spans
        .iter()
        .find(|b| b.span == span)
        .unwrap_or_else(|| panic!("span {span} landed nowhere in {boxes:?}"))
}

/// The defect ticket 08 recorded as impossible, and the numbers that close
/// it: an inline box's horizontal margin, border and padding each reserve
/// real advance in the line.
///
/// Observed on a real Wayland surface against Jitendex, whose
/// `span[data-sc-class="tag"]` declares `padding: 0.2em 0.3em` and
/// `margin-right: 0.5em`: the panel drew `go (game)〔眼 only〕` where
/// Yomitan draws `go (game) 〔眼 only〕`. The margin reserved nothing at
/// all, and the box was outset over the padding it had not reserved
/// either - so the background painted 3.6 physical pixels *under* the
/// following word.
///
/// Every number below is arithmetic over `FakeMeasure`: one no-break space
/// advances half its size, so `n` of them at size `s` reserve
/// `n * s / 2`. A left margin and a border join Jitendex's own
/// declarations so that all three properties are priced in one pass.
#[test]
fn an_inline_boxs_horizontal_edges_each_reserve_their_own_advance() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","borderRadius":0.3,"##,
        r##""padding":"0.2em 0.3em","marginLeft":"0.1em","marginRight":"0.5em","##,
        r##""borderWidth":"0.2em","borderStyle":"solid"},"##,
        r##""content":"noun"},"Chinese character"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    // 0.1em of margin, then 0.2em of border plus 0.3em of padding, then
    // the word, then the same again, then 0.5em of margin - one span per
    // edge with room to buy, in the order a line reads them.
    assert_eq!(
        "\u{a0}\u{a0}\u{a0}noun\u{a0}\u{a0}\u{a0}\u{a0}Chinese character", gloss.text,
        "the room is text, because text is what both bins re-measure"
    );
    assert_eq!(
        vec![3.0, 7.5, BOX_EM, 7.5, 7.5, BOX_EM],
        gloss.spans.iter().map(|s| s.size).collect::<Vec<_>>(),
        "each spacer solved to the size that reserves its own edge"
    );

    // And the advance those sizes actually buy, through the seam, at the
    // same width the walk measured at.
    let boxes = painted_spans(gloss);
    assert_eq!(1, boxes.metrics.lines, "one line, so one fragment per span");
    let widths: Vec<f32> = (0..6).map(|i| painted(&boxes, i).w).collect();
    assert_eq!(
        vec![1.5, 7.5, 4.0 * BOX_EM * ADVANCE, 7.5, 7.5, 17.0 * BOX_EM * ADVANCE],
        widths,
        "margin-left 1.5, border+padding 7.5, the word, 7.5, margin-right 7.5"
    );

    // The box is the border box: the margins are outside it, both
    // paddings are inside, and its own two ends are the two spacers.
    let pill = gloss.inline_boxes[0];
    assert_eq!(gloss.pen.0 + 1.5, pill.rect.x, "the left margin is outside the box");
    assert_eq!(7.5 + 4.0 * BOX_EM * ADVANCE + 7.5, pill.rect.w);

    // The whole point, in one number: the word after the pill starts a
    // full margin-right clear of the background, where before this it
    // started flush against the pill's own glyphs. 1.5 + 7.5 + 30 + 7.5
    // + 7.5, every term of it reserved.
    let word = painted(&boxes, 5);
    assert_eq!(54.0, word.x);
    assert_eq!(
        7.5,
        (gloss.pen.0 + word.x) - (pill.rect.x + pill.rect.w),
        "margin-right, and no background under the word"
    );
}

/// The invariant a bin's re-measure enforces: the rect drawn and the
/// advance reserved are one measurement, so the background cannot reach
/// past the room the text cleared for it.
///
/// Asserted as an *identity*, `rect == cover of the box's own spans`,
/// rather than as `rect == what the style declared`. The two agree on any
/// face whose no-break space is at least a quarter of an em, which is
/// every real one and this fake; on a narrower face the solve comes out
/// short ([`PILL_SPACERS_PER_EM`]) and only the identity still holds -
/// which is why [`place_pills`] reads the box back off the run instead of
/// outsetting the text by what the style declared.
#[test]
fn a_pills_background_never_reaches_past_the_room_it_bought() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","padding":"0.3em","marginRight":"0.5em"},"##,
        r##""content":"noun"},"Chinese character"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);
    let boxes = painted_spans(gloss);
    let pill = gloss.inline_boxes[0];

    // The cover of the box's own two spacers plus its word, exactly.
    let (lead, word, trail) = (painted(&boxes, 0), painted(&boxes, 1), painted(&boxes, 2));
    assert_eq!(gloss.pen.0 + lead.x, pill.rect.x, "the box starts where its padding does");
    assert_eq!(trail.x + trail.w - lead.x, pill.rect.w, "and ends where it ends");
    assert_eq!(4.5, lead.w, "0.3em of padding, reserved");
    assert_eq!(4.5, trail.w);
    assert_eq!(lead.x + lead.w, word.x, "the word starts after the padding");

    // So no glyph of the following word is under the fill.
    let after = painted(&boxes, 4);
    assert!(
        pill.rect.x + pill.rect.w <= gloss.pen.0 + after.x,
        "{pill:?} reaches past the word at {after:?}"
    );
}

/// The reservation must not become a wrap opportunity. A pill whose
/// `margin-right` ended one line while the word it separates began the
/// next would put the gap in one place and the reason for it in another.
///
/// U+00A0 is UAX #14 class GL, so a break is forbidden after it (LB12) and
/// before it too unless a space came first (LB12a) - and `FakeMeasure`
/// models exactly those two rules, because two of this renderer's
/// reservations rest on them ([`glue`]).
#[test]
fn a_pills_margin_never_breaks_away_from_the_word_it_separates() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","marginRight":"0.5em"},"##,
        r##""content":"noun"},"Chinese character"]}"##
    )));
    // Every width the run can wrap at, rather than one: the break that
    // matters is the one that lands exactly on the gap, and pinning a
    // single width would go vacuous the moment the arithmetic around it
    // moved. `wrap_w` is `max_w - 24` here, so this sweeps 16 to 175 and
    // the naive break falls on the gap at 46.
    let mut wrapped = 0;
    for step in 0..160 {
        let s = laid_out(&p, 40.0 + step as f32, 4000.0, false, false);
        let gloss = one_body(&s);
        let boxes = painted_spans(gloss);
        wrapped += usize::from(boxes.metrics.lines > 1);

        // The gap is one fragment, never split down the middle.
        let margin: Vec<SpanBox> =
            boxes.spans.iter().copied().filter(|b| b.span == 1).collect();
        assert_eq!(1, margin.len(), "the gap wrapped inside itself: {boxes:?}");
        // And it shares its line with the end of the pill before it and
        // the start of the word after it, so the gap and the reason for
        // it are never a line apart. The pill's *last* fragment, because
        // at 16 pixels of wrap the word inside it wraps too.
        let pill = boxes.spans.iter().rfind(|b| b.span == 0).expect("the pill");
        assert_eq!(
            pill.line, margin[0].line,
            "the pill left its own margin behind: {boxes:?}"
        );
        assert_eq!(
            margin[0].line, painted(&boxes, 2).line,
            "the gap and the word it separates split across lines: {boxes:?}"
        );
    }
    assert!(wrapped > 100, "only {wrapped} of 160 widths wrapped at all");
}

/// The vertical half, which CSS answers differently and this renderer
/// already agreed with: an inline box's vertical padding and border paint
/// but do not affect line height.
///
/// So the rect grows over its neighbours' lines while the paragraph stacks
/// as though the box were not there - and the horizontal spacers do not
/// grow it either, which is what capping their solved size at the box's
/// own em is for ([`measure_pills`]).
#[test]
fn an_inline_boxs_vertical_padding_paints_without_growing_its_line() {
    let bare = laid_out(&rich(&sc(r#"{"tag":"div","content":"noun"}"#)), 424.0, 4000.0, false, false);
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","padding":"0.5em"},"content":"noun"}]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let (bare, gloss) = (one_body(&bare), one_body(&s));

    assert_eq!(BODY_LINE, bare.rect.h, "one line of body text");
    assert_eq!(bare.rect.h, gloss.rect.h, "and the box adds nothing to it");
    assert_eq!(bare.advance, gloss.advance, "so the paragraph below does not move");

    // The rect, though, is outset by 0.5em on both edges and hangs over
    // whatever is stacked above and below.
    let pill = gloss.inline_boxes[0];
    assert_eq!(gloss.pen.1 - 7.5, pill.rect.y);
    assert_eq!(BODY_LINE + 15.0, pill.rect.h);
}

/// A box that paints nothing and only spaces its content out. Ticket 08
/// resolved these to nothing, because nothing could spend them; now the
/// room is bought and no box is drawn, which is what a browser does with
/// `<span style="margin-right:.5em">`.
#[test]
fn a_margin_with_nothing_to_draw_still_reserves_its_room() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"marginRight":"0.5em"},"##,
        r##""content":"a"},"b"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    assert_eq!("a\u{a0}\u{a0}b", gloss.text, "the room is there");
    assert!(gloss.inline_boxes.is_empty(), "and nothing is drawn in it");
    assert_eq!(None, gloss.block_box, "an inline margin is no block's box");

    let boxes = painted_spans(gloss);
    assert_eq!(7.5, painted(&boxes, 1).w, "half an em of margin, as advance");
    assert_eq!(
        BOX_EM * ADVANCE + 7.5,
        painted(&boxes, 2).x,
        "so the run after it starts a margin clear of the run before"
    );
}

/// A pill whose paragraph lost a span to the edge trim. `InlineBox` names
/// its run by span index and its own spacers are found from the same two
/// indices, so the trim renumbers them ([`trim`]) - a stale pair would
/// size a *word* as though it were a spacer.
#[test]
fn a_pill_keeps_its_own_run_when_the_trim_drops_a_span_ahead_of_it() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[" ",{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","padding":"0.2em"},"content":"noun"}," tail"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    assert_eq!("\u{a0}noun\u{a0} tail", gloss.text, "the leading space is gone");
    assert_eq!(
        vec![6.0, BOX_EM, 6.0, BOX_EM],
        gloss.spans.iter().map(|s| s.size).collect::<Vec<_>>(),
        "the two spacers were sized, and the two words were not"
    );
    let pill = gloss.inline_boxes[0];
    assert_eq!(gloss.pen.0, pill.rect.x, "the box starts at its own padding");
    assert_eq!(3.0 + 4.0 * BOX_EM * ADVANCE + 3.0, pill.rect.w, "and covers only its run");
}

/// An inline box whose content turns out to hold a block. The block opens
/// a paragraph, which sends the one the box was being measured against out
/// from under it, so every span index the box took names a paragraph that
/// has left.
///
/// No box, stated rather than accidental. Before the room was priced in,
/// whether one was drawn depended on how many spans the *previous*
/// paragraph happened to hold - and now a stale index would also resize
/// the replacement paragraph's own first word.
#[test]
fn a_pill_that_turns_out_to_hold_a_block_draws_no_box() {
    let p = rich(&sc(concat!(
        r##"{"tag":"span","style":{"backgroundColor":"#565656","padding":"0.2em"},"##,
        r##""content":[{"tag":"div","content":"x"}]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert!(texts(&s).contains(&"x"), "the block still renders: {:?}", texts(&s));
    assert!(
        s.elems.iter().all(|e| e.inline_boxes.is_empty() && e.block_box.is_none()),
        "no box over another paragraph's spans"
    );
    let gloss = one_body(&s);
    assert_eq!(
        vec![BOX_EM],
        gloss.spans.iter().map(|s| s.size).collect::<Vec<_>>(),
        "and the block's own word kept its size"
    );
}

/// A box over no span is no box - and the room its edges would have bought
/// goes back out of the paragraph, because a gap with nothing in it is a
/// gap a reader can see.
#[test]
fn a_pill_around_nothing_reserves_nothing() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":["before",{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","padding":"0.2em","marginRight":"0.5em"},"##,
        r##""content":""},"after"]}"##
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);

    assert_eq!("beforeafter", gloss.text, "no spacer left behind");
    assert!(gloss.inline_boxes.is_empty());
}

/// The other defect ticket 17's author found: `css_len` read `em`, `%`
/// and `px` and dropped `rem`, which Jitendex writes on its
/// `div[data-sc-class="extra-box"]` (`0.4rem`/`0.5rem`) and
/// Onomatoproject writes on 3 096 inline nodes.
///
/// `rem` is the *root* em, and this popup's root is the theme's body
/// size - what Yomitan's root font size is, since `display.js` writes
/// the reader's own font-size setting onto
/// `documentElement.style.fontSize`. So a node that shrank its own text
/// still measures a `rem` against the panel, which is the bug a
/// plausible fix would introduce by reaching for the em already in hand.
#[test]
fn a_rem_length_resolves_against_the_panel_and_not_the_nodes_own_em() {
    let p = rich(&sc(concat!(
        r#"[{"tag":"div","style":{"padding":"0.4rem","marginRight":"0.5rem"},"#,
        r#""content":"root"},"#,
        r#"{"tag":"div","style":{"fontSize":"0.5em","padding":"0.4rem"},"#,
        r#""content":"half"}]"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    let root = block_box(box_around(&s, "root")).style;
    assert_eq!(Edges::all(0.4 * BOX_EM), root.padding, "0.4 of the panel's own em");
    assert_eq!(0.5 * BOX_EM, root.margin.right);

    let half = gloss_of(&s, "half");
    assert_eq!(BOX_EM / 2.0, half.font_size, "this node halved its own text");
    assert_eq!(
        Edges::all(0.4 * BOX_EM),
        block_box(box_around(&s, "half")).style.padding,
        "and its `rem` is unmoved by that: a root em is not an em"
    );
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
        // `rem` is the panel's own body size, whatever em the node
        // declaring it resolved to.
        (r#""0.4rem""#, 6.0, 6.0, 6.0, 6.0),
    ];
    for &(decl, top, right, bottom, left) in lengths {
        let p = rich(&sc(&format!(
            r#"{{"tag":"div","style":{{"padding":{decl}}},"content":"x"}}"#
        )));
        let s = laid_out(&p, 424.0, 4000.0, false, false);
        let got = block_boxes(&s)
            .first()
            .map_or(Edges::default(), |e| block_box(e).style.padding);
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
        let got = block_boxes(&s)
            .first()
            .map_or(Edges::default(), |e| block_box(e).style.border_style);
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
    let outer = one_block_box(&s);

    let used = block_box(outer).style.border_used();
    assert_eq!(Edges { top: 0.0, right: 0.0, bottom: 0.0, left: 3.0 }, used);
    assert_eq!(
        BODY_LINE,
        outer.advance,
        "a vertical border of nothing adds nothing to the box's own height"
    );
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

// ---- lists ----

/// One list level, in the panel's own pixels: Yomitan's `1.4em` against
/// the body size every fixture here is measured at.
const LEVEL: f32 = LIST_INDENT_EM * BOX_EM;

/// `disc`, as the walk writes it beside an item.
fn bullet() -> String {
    format!("{DISC_MARKER}{MARKER_GAP}")
}

/// A marker's own width, as `FakeMeasure` measures it.
///
/// One unit per UTF-16 unit of the label, its [`MARKER_GAP`] included -
/// the gap is inside the marker box, so it is what holds the glyph off
/// the text the box hangs beside.
fn marker_w(label: &str) -> f32 {
    label.encode_utf16().count() as f32 * BOX_EM * ADVANCE
}

/// The one marker an item carries.
fn one_marker(e: &SceneElem) -> &MarkerBox {
    assert_eq!(1, e.marker.len(), "expected one marker, got {:?}", e.marker);
    &e.marker[0]
}

/// A marker's leading edge in panel space: what a bin draws it at.
fn marker_x(e: &SceneElem) -> f32 {
    e.pen.0 + one_marker(e).x
}

/// Where each line of an element's own run starts, in panel space.
///
/// Exactly the arithmetic a bin does: it re-measures the element's spans
/// at the element's wrap width and draws the whole run from one origin
/// (ADR-0013), so a line's x is that origin plus the leftmost span box
/// the seam put on it. This is the number that says whether a wrapped
/// item's second line sits under its marker or under its text.
fn line_x(e: &SceneElem) -> Vec<f32> {
    let spans: Vec<StyledSpan<'_>> = e.styled_spans("").collect();
    let measured = fake_measure(&spans, e.wrap_w);
    let mut out = vec![f32::MAX; measured.lines.len()];
    for b in &measured.spans {
        let slot = &mut out[b.line as usize];
        *slot = slot.min(e.pen.0 + b.x);
    }
    out
}

/// One list of plain-text items, as a rich card.
///
/// `style` is the list's own inline style, comma and all, so a test
/// declares `listStyleType` on exactly the node CSS declares it on.
fn list_card(tag: &str, style: &str, items: &[&str]) -> Presentation {
    let items: Vec<String> = items
        .iter()
        .map(|text| format!(r#"{{"tag":"li","content":"{text}"}}"#))
        .collect();
    rich(&sc(&format!(
        r#"{{"tag":"{tag}"{style},"content":[{}]}}"#,
        items.join(",")
    )))
}

/// The acceptance shape (story 5): one element per item, each carrying
/// its own marker at the same indent. The marker text and the offsets
/// are what is asserted, not that the words arrived.
#[test]
fn an_unordered_list_marks_every_item_and_indents_them_alike() {
    let s = laid_out(&list_card("ul", "", &["a", "b", "c"]), 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(3, items.len(), "one element per item");
    for (i, text) in ["a", "b", "c"].iter().enumerate() {
        assert_eq!(*text, items[i].text, "the marker is no part of the item's text");
        assert_eq!(bullet(), one_marker(items[i]).text, "item {i}");
        assert_eq!(s.origin + LEVEL, items[i].pen.0, "item {i} sits one level in");
        // `list-style-position: outside`: the marker box's right edge
        // sits on the item's content edge, inside the gutter the level
        // indent opened, and the gap that separates the two is the
        // marker's own trailing one.
        assert_eq!(items[i].pen.0 - marker_w(&bullet()), marker_x(items[i]));
        assert!(marker_x(items[i]) >= s.origin, "and inside the panel's column");
    }
    // The indent comes off the wrap width as well as the pen, so an
    // indented item still stops at the column's own right edge. The
    // marker takes nothing more: it is in the gutter, not on the line.
    assert_eq!(s.content_w - LEVEL, items[0].wrap_w);
    // An indent is not a box: nothing is drawn around an item that only
    // sits further in.
    assert_eq!(None, items[0].block_box);
}

/// Story 5 again, and the numbering ticket 16 does *not* do: an ordinal
/// per item of this list, which is a marker rather than a Sense number.
#[test]
fn an_ordered_list_numbers_its_items_from_one() {
    let s = laid_out(&list_card("ol", "", &["a", "b", "c"]), 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(
        vec!["a", "b", "c"],
        items.iter().map(|e| e.text.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(
        vec!["1. ", "2. ", "3. "],
        items.iter().map(|e| one_marker(e).text.as_str()).collect::<Vec<_>>()
    );
    for item in &items {
        assert_eq!(s.origin + LEVEL, item.pen.0);
        assert_eq!(item.pen.0 - marker_w("1. "), marker_x(item));
    }
}

/// Story 6: the nesting is shown by the indentation, one level per level,
/// and the marker is resolved again at the inner level.
#[test]
fn a_nested_list_indents_its_inner_items_past_its_outer_ones() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":["outer","#,
        r#"{"tag":"ol","content":{"tag":"li","content":"inner"}}]}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(
        vec!["outer", "inner"],
        items.iter().map(|e| e.text.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(bullet(), one_marker(items[0]).text);
    assert_eq!("1. ", one_marker(items[1]).text);
    assert_eq!(s.origin + LEVEL, items[0].pen.0);
    assert_eq!(s.origin + 2.0 * LEVEL, items[1].pen.0);
    assert_eq!(LEVEL, items[1].pen.0 - items[0].pen.0, "one level, reused");
    // And the inner level takes its own width off the wrap, so the
    // deeper item is the narrower one.
    assert_eq!(items[0].wrap_w - LEVEL, items[1].wrap_w);
}

/// The acceptance bullet ticket 09 first shipped `inside` for: a nested
/// list's inner marker hangs in the *inner* gutter, one level past the
/// outer one, because a marker box is placed against the content edge of
/// the list that owed it and each level opens its own.
///
/// Two unordered levels, so both markers are one bullet and both fit
/// their 21px gutter with room to spare - the geometry under test is
/// which gutter, not how a wide counter overhangs a narrow one.
#[test]
fn a_nested_lists_inner_marker_hangs_in_the_inner_gutter() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":["outer","#,
        r#"{"tag":"ul","content":{"tag":"li","content":"inner"}}]}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let items = bodies(&s);
    let (outer, inner) = (items[0], items[1]);

    assert_eq!(LEVEL, marker_x(inner) - marker_x(outer), "one level apart");
    // Each inside its own gutter: past its list's own content edge, and
    // left of the text it marks.
    for item in [outer, inner] {
        assert!(marker_x(item) >= item.pen.0 - LEVEL, "{:?}", item.text);
        assert!(marker_x(item) + one_marker(item).w <= item.pen.0, "{:?}", item.text);
    }
    // And the inner marker is past the outer item's own text, which is
    // what "not the outer gutter" means.
    assert!(marker_x(inner) >= outer.pen.0);
}

/// An item whose whole content is a nested list shares one line with its
/// inner item, so both levels' markers land on that one element - and
/// each hangs in its own gutter, which is what a browser draws.
///
/// Jitendex's real shape: `ul[sense-groups] > li > ol > li > ul > li`
/// gave `"\u{2022} \u{2460} \u{2022} to eat"` as one run before this,
/// three markers deep on one line.
#[test]
fn an_items_marker_and_its_nested_items_marker_hang_in_their_own_gutters() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":"#,
        r#"{"tag":"ul","content":{"tag":"li","content":"to eat"}}}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let item = one_body(&s);

    assert_eq!("to eat", item.text, "one line, and no marker in it");
    assert_eq!(s.origin + 2.0 * LEVEL, item.pen.0, "two levels in");
    assert_eq!(2, item.marker.len(), "outermost list first");
    let (outer, inner) = (&item.marker[0], &item.marker[1]);
    assert_eq!(bullet(), outer.text);
    assert_eq!(bullet(), inner.text);
    // The outer list's content edge is one level in, the inner list's is
    // two, and each marker's right edge sits on its own.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), item.pen.0 + outer.x);
    assert_eq!(s.origin + 2.0 * LEVEL - marker_w(&bullet()), item.pen.0 + inner.x);
    // Both on the item's own first baseline, which is the line they
    // share. `FakeMeasure` puts a body line's baseline halfway down it.
    assert_eq!(0.0, outer.y);
    assert_eq!(0.0, inner.y);
}

/// Story 7: a dictionary's own counter, rendered as written. No counter
/// algorithm runs over it and no suffix is added to it.
#[test]
fn a_literal_string_marker_renders_verbatim() {
    // Both quote characters, because CSS takes either and the census
    // holds both.
    let quoted: &[&str] = &[r#"'\u2460'"#, r#"\"\u2460\""#];
    for value in quoted {
        let style = format!(r#","style":{{"listStyleType":"{value}"}}"#);
        let s = laid_out(&list_card("ul", &style, &["a"]), 224.0, 4000.0, false, false);
        let item = one_body(&s);
        assert_eq!(format!("\u{2460}{MARKER_GAP}"), one_marker(item).text, "{value}");
        assert_eq!("a", item.text, "{value}");
    }
}

/// `list-style-type` is inherited and the marker is drawn by the item, so
/// an item's own declaration wins over its list's.
///
/// This is Jitendex's real shape, not a corner case: it declares
/// `listStyleType` on the `li` in all 38 381 entries that carry one -
/// 97 150 nodes, every one of them an `li` - and its ①②③ sense numbering
/// is nothing but that. Resolved at the list alone, the whole dictionary
/// would draw bullets.
#[test]
fn an_items_own_list_style_wins_over_its_lists() {
    // `ol > li[listStyleType]`, as Jitendex writes a sense group.
    let p = rich(&sc(concat!(
        r#"{"tag":"ol","content":[{"tag":"li","style":{"listStyleType":"\"\u2460\""},"#,
        r#""content":"first"},{"tag":"li","style":{"listStyleType":"\"\u2461\""},"#,
        r#""content":"second"},{"tag":"li","content":"third"}]}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    assert_eq!(
        vec!["\u{2460} ", "\u{2461} ", "3. "],
        bodies(&s).iter().map(|e| one_marker(e).text.as_str()).collect::<Vec<_>>(),
        "the declaring items take their own counter, the silent one inherits"
    );
    assert_eq!(
        vec!["first", "second", "third"],
        bodies(&s).iter().map(|e| e.text.as_str()).collect::<Vec<_>>()
    );

    // And the inheritance runs the other way too: a list's own
    // declaration reaches an item that declares nothing, which is where
    // ticket 17 will put a `styles.css` rule.
    let inherited = laid_out(
        &list_card("ul", r#","style":{"listStyleType":"circle"}"#, &["a", "b"]),
        224.0,
        4000.0,
        false,
        false,
    );
    for item in bodies(&inherited) {
        let mark = &one_marker(item).text;
        assert!(mark.starts_with(CIRCLE_MARKER), "{mark:?}");
    }
}

/// An unreadable `listStyleType` falls back to the initial value for the
/// list's own tag, which is a marker of the wrong shape rather than a
/// missing one - and a keyword this build *can* read wins over the tag.
#[test]
fn an_unreadable_list_style_falls_back_to_each_tags_initial_value() {
    // (tag, keyword, marker without its gap)
    let cases: &[(&str, &str, &str)] = &[
        ("ul", "", DISC_MARKER),
        ("ul", "disc", DISC_MARKER),
        ("ul", "circle", CIRCLE_MARKER),
        ("ul", "square", SQUARE_MARKER),
        ("ul", "decimal", "1."),
        ("ol", "", "1."),
        ("ol", "decimal", "1."),
        ("ol", "disc", DISC_MARKER),
        // Locale counter algorithms are out of scope by the spec, so
        // each tag's own initial value stands.
        ("ul", "katakana", DISC_MARKER),
        ("ul", "lower-roman", DISC_MARKER),
        ("ol", "cjk-ideographic", "1."),
        ("ol", "hiragana-iroha", "1."),
        // And so does an outright unreadable one.
        ("ul", "not-a-keyword", DISC_MARKER),
        ("ol", "not-a-keyword", "1."),
    ];
    for (tag, keyword, marker) in cases {
        let style = if keyword.is_empty() {
            String::new()
        } else {
            format!(r#","style":{{"listStyleType":"{keyword}"}}"#)
        };
        let s = laid_out(&list_card(tag, &style, &["a"]), 224.0, 4000.0, false, false);
        let item = one_body(&s);
        assert_eq!(
            format!("{marker}{MARKER_GAP}"),
            one_marker(item).text,
            "{tag} declaring {keyword:?}"
        );
        assert_eq!("a", item.text, "{tag} declaring {keyword:?}");
    }
}

/// The one keyword whose fallback would *add* ink: an author writing
/// `none` removed the marker, and `disc` would hand it back. An empty
/// string counter is CSS's other way of saying it.
#[test]
fn list_style_type_none_draws_no_marker_and_still_indents() {
    for value in ["none", r#"\"\""#] {
        let style = format!(r#","style":{{"listStyleType":"{value}"}}"#);
        let s = laid_out(&list_card("ul", &style, &["a"]), 224.0, 4000.0, false, false);
        let item = one_body(&s);
        assert_eq!("a", item.text, "{value} asked for no marker");
        assert!(item.marker.is_empty(), "{value} asked for no marker");
        assert_eq!(s.origin + LEVEL, item.pen.0, "and still for the indent");
    }
}

/// `::marker` inherits from the item, not from the markup inside it.
///
/// The mechanism moved: the marker used to be the item paragraph's first
/// span, and what this asserted was that it stayed its own span at both
/// ends. It is now no span at all - it is a positioned run of its own
/// beside the element ([`MarkerBox`]) - so a neighbour's style cannot
/// reach it by construction, and what is asserted is the style the run
/// itself carries.
#[test]
fn a_marker_takes_the_items_own_style_and_not_its_contents() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":"#,
        r#"{"tag":"b","content":"bold"}}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let item = one_body(&s);
    assert_eq!("bold", item.text);
    assert_eq!(1, item.spans.len(), "the marker is no span of the run");
    assert_eq!(BOLD_WEIGHT, item.spans[0].weight);

    let mark = one_marker(item);
    assert_eq!(bullet(), mark.text);
    assert_ne!(BOLD_WEIGHT, mark.weight, "the `b` is not the item");
    assert_eq!(item.font_size, mark.size, "and the item's em is the marker's");
}

/// A marker beside a ruby base must stay out of the base's slot, or the
/// reading would centre over the bullet as well as over the base.
///
/// It cannot reach the slot any more: the marker left the run. The
/// reading's x moved with it, from two units in - past the marker the
/// run used to carry - to centred over a base at the item's own left
/// edge. That move is the proof the marker hangs.
#[test]
fn a_marker_does_not_join_the_ruby_base_it_precedes() {
    let unit = BOX_EM * ADVANCE;
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":"#,
        r#"{"tag":"ruby","content":["b",{"tag":"rt","content":"c"}]}}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let item = one_body(&s);

    assert_eq!("b\u{2060}", item.text, "the base and its filler, and nothing else");
    // The base is one unit at the item's own left edge, and the reading
    // is a half-size unit centred on it.
    let read = &item.ruby[0];
    assert_eq!((unit - unit * RUBY_RATIO) / 2.0, read.x);
    assert_eq!(unit * RUBY_RATIO, read.w);
    // And the marker is left of the base rather than before it.
    assert_eq!(bullet(), one_marker(item).text);
    assert!(one_marker(item).x < 0.0);
}

/// The marker belongs to the item's first *line*, which for an item
/// whose content is a block is that block's paragraph. Pushing it where
/// it is resolved would flush an element holding nothing but a bullet.
#[test]
fn an_item_wrapping_its_content_in_a_block_still_marks_one_element() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":"#,
        r#"{"tag":"div","content":"body"}}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(1, items.len(), "no bullet-only element");
    assert_eq!("body", items[0].text);
    assert_eq!(bullet(), one_marker(items[0]).text);
    assert_eq!(s.origin + LEVEL, items[0].pen.0, "the block inherits the indent");
    // The gutter is the *list's*, so the marker hangs beside the block's
    // line at the level the list opened.
    assert_eq!(items[0].pen.0 - marker_w(&bullet()), marker_x(items[0]));
}

/// An item with nothing to mark draws no marker, and the counter still
/// counted it - which is what a browser's counter does.
#[test]
fn an_empty_item_draws_no_marker_and_still_takes_its_ordinal() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ol","content":[{"tag":"li","content":""},"#,
        r#"{"tag":"li","content":"second"}]}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(1, items.len());
    assert_eq!("second", items[0].text);
    assert_eq!("2. ", one_marker(items[0]).text);
}

/// An item whose only content is a table has no line inside the grid to
/// hang a marker beside, so the marker takes the line above - written
/// *inline*, as its own one-line paragraph. A gutter with no line beside
/// it would simply drop the bullet.
#[test]
fn an_item_whose_only_content_is_a_table_writes_its_marker_on_the_line_above() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","content":{"tag":"table","content":"#,
        r#"{"tag":"tr","content":{"tag":"td","content":"a"}}}}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(
        vec![DISC_MARKER, "a"],
        items.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        "the marker's own line - its trailing gap trimmed, nothing following it - \
         then the cell inside the grid"
    );
    assert!(items[0].marker.is_empty(), "inline: it has no line to hang beside");
    let grid = find(&s, ElemKind::Table);
    assert!(items[0].pen.1 < grid.rect.y, "and above the grid");
}

/// A list whose items are not items is laid out as any other block: a
/// marker follows `display: list-item`, which only an `li` has.
#[test]
fn a_list_without_items_is_indented_and_unmarked() {
    let p = rich(&sc(r#"{"tag":"ul","content":["one","two"]}"#));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(
        vec!["one", "two"],
        items.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        "the bare-string rule still splits them"
    );
    for item in &items {
        assert_eq!(s.origin + LEVEL, item.pen.0, "a list indents whatever it holds");
        assert!(item.marker.is_empty(), "and marks nothing");
    }
}

/// A declared `paddingLeft` on an item composes with the level indent
/// rather than replacing it, which is what a browser does: the padding
/// is the item's and the indent is its list's.
#[test]
fn an_items_own_padding_adds_to_the_level_indent() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":{"tag":"li","style":{"paddingLeft":0.4},"#,
        r#""content":"a"}}"#
    )));
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let item = one_body(&s);
    let padding = 0.4 * BOX_EM;
    assert_eq!(s.origin + LEVEL + padding, item.pen.0);
    assert_eq!(s.content_w - LEVEL - padding, item.wrap_w);
    // The indent is the list's, so it shifts the item's border box; the
    // padding is the item's, so it insets the text inside that box.
    let outer = one_block_box(&s);
    assert_eq!(s.origin + LEVEL, block_box(outer).rect.x);
    assert_eq!(s.content_w - LEVEL, block_box(outer).rect.w);
    // And the marker hangs off the *list's* content edge, so the item's
    // own padding moves its text and leaves its bullet where the list
    // drew it - a browser's `outside` marker to the pixel.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), marker_x(item));
}

/// The acceptance bullet ticket 09 shipped `inside` for, now pinned the
/// other way round.
///
/// What this used to assert was that every line of a wrapped item sat at
/// the item's own indent *beside* the marker, because the marker was the
/// paragraph's first span - CSS's `list-style-position: inside`. The
/// marker is now out of the run, so the first line starts where every
/// continuation line starts, which is what Yomitan draws: the browser
/// default, `outside`.
#[test]
fn a_wrapped_items_continuation_lines_align_to_its_text_not_its_marker() {
    let long = "a".repeat(30);
    let s = laid_out(&list_card("ul", "", &[&long]), 224.0, 4000.0, false, false);
    let item = one_body(&s);
    // 200 wide less one 21px level is 179, which takes 23 of the fake's
    // 7.5px units, so 30 units of body text is two lines.
    assert_eq!(s.content_w - LEVEL, item.wrap_w);
    assert_eq!(2, item.lines);
    // Every line of it, first and continuation alike, at the item's text.
    assert_eq!(vec![s.origin + LEVEL, s.origin + LEVEL], line_x(item));
    // And the marker left of all of them, in the gutter.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), marker_x(item));
    assert!(marker_x(item) < line_x(item)[1], "the second line is not under it");
}

/// The parameter ticket 14 wires, exercised at both its values.
///
/// Below `layout::scene`, because nothing above it can ask for the
/// compact layout until ticket 14's setting exists. What is asserted is
/// the contract between the two tickets: what a marker *says* is shared -
/// `marker_of` and `Marker::label` never see this flag - and where it
/// *goes* is the one thing the flag decides. A compact list has no
/// gutter and gives an item no line of its own, so its marker is written
/// inline as the joined paragraph's spans; a stacked list hangs it.
/// Ticket 14 owns the setting, its plumbing, and the scene-level
/// assertion the spec asks it for.
#[test]
fn stack_items_false_joins_a_list_into_one_separated_paragraph() {
    let theme = Theme::dark();
    let doc = crate::dict::gloss::GlossDoc::parse(&sc(concat!(
        r#"{"tag":"ol","content":[{"tag":"li","style":{"listStyleType":"\"\u2460\""},"#,
        r#""content":"first"},{"tag":"li","content":"second"}]}"#
    )));
    let assets = Assets { dict_id: 1, sizes: &[] };
    let flows = |stack_items| -> Vec<Flow> {
        let render = Render { roles: RoleFilter::CARD, styling: true, images: true };
        paragraphs(&doc, &theme, LINE_GAP, stack_items, assets, render)
            .into_iter()
            .filter_map(|piece| match piece {
                Piece::Flow(flow) => Some(flow),
                Piece::Table(_) | Piece::Boxed(_) => None,
            })
            .collect()
    };

    let compact = flows(false);
    assert_eq!(
        vec![format!("\u{2460} first{ITEM_SEPARATOR}2. second")],
        compact.iter().map(|f| f.text.clone()).collect::<Vec<_>>(),
        "one paragraph, the separator between the items, the same markers"
    );
    assert!(compact[0].marker.is_empty(), "and no gutter to hang either in");

    let stacked = flows(true);
    assert_eq!(
        vec!["first".to_string(), "second".to_string()],
        stacked.iter().map(|f| f.text.clone()).collect::<Vec<_>>(),
        "and stacked, one paragraph per item"
    );
    assert_eq!(
        vec!["\u{2460} ", "2. "],
        stacked.iter().map(|f| f.marker[0].text.as_str()).collect::<Vec<_>>(),
        "with the identical labels, hanging"
    );
}

// ---- tables ----

/// Yomitan's own base font size, which the grid fixtures measure at.
///
/// The spec writes a cell border `1em / 14`, so at fourteen pixels a rule
/// is exactly one pixel, the `0.25em` padding is exactly 3.5, and
/// `FakeMeasure` charges exactly 7 per UTF-16 unit over a 28-pixel line.
/// Every number below is then arithmetic a reader can redo by hand.
const GRID_EM: f32 = YOMITAN_BASE_PX;
/// One cell rule, at [`GRID_EM`].
const RULE: f32 = 1.0;
/// One cell's padding per edge, at [`GRID_EM`].
const CELL_PAD: f32 = 3.5;
/// One line of cell text, at [`GRID_EM`].
const CELL_LINE: f32 = GRID_EM * LINE_H;
/// One UTF-16 unit of cell text, at [`GRID_EM`].
const CELL_UNIT: f32 = GRID_EM * ADVANCE;

/// A scene over `p`, at [`GRID_EM`].
fn grid_scene(p: &Presentation, max_w: f32, side: bool) -> PopupScene {
    let theme = Theme { body_size: GRID_EM, ..Theme::dark() };
    let mut m = FakeMeasure::default();
    scene(
        &SceneRequest {
            presentation: p,
            theme: &theme,
            max_w,
            max_h: 4000.0,
            show_back: false,
            side_panel: side,
            render: RenderSettings::default(),
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run")
}

/// A scene over one structured-content `glossary`, at [`GRID_EM`].
fn gridded(glossary: &str, max_w: f32) -> PopupScene {
    grid_scene(&rich(&sc(glossary)), max_w, false)
}

/// One `tr` of cells, each `tag:content`.
fn mixed_row(cells: &[(&str, &str)]) -> String {
    let body: Vec<String> = cells
        .iter()
        .map(|(tag, content)| format!(r#"{{"tag":"{tag}","content":"{content}"}}"#))
        .collect();
    format!(r#"{{"tag":"tr","content":[{}]}}"#, body.join(","))
}

/// One `tr` of `td`s holding `cells`.
fn tr(cells: &[&str]) -> String {
    let body: Vec<(&str, &str)> = cells.iter().map(|c| ("td", *c)).collect();
    mixed_row(&body)
}

/// A `table` of `rows`.
fn table(rows: &[String]) -> String {
    format!(r#"{{"tag":"table","content":[{}]}}"#, rows.join(","))
}

/// Every cell box in a scene, in document order.
fn grid_cells(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Cell).collect()
}

/// Every gloss-body run of a grid scene, in draw order.
///
/// [`bodies`] filters at the default theme's body size; these fixtures
/// measure at [`GRID_EM`], which no other role in the theme shares.
fn grid_text(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems
        .iter()
        .filter(|e| e.kind == ElemKind::Text && e.font_size == GRID_EM)
        .collect()
}

/// The one table element of a scene.
fn grid(s: &PopupScene) -> &SceneElem {
    find(s, ElemKind::Table)
}

/// Every cell's border box, as `(x, y, w, h)` relative to the table's own
/// top-left - so an expectation reads as grid arithmetic rather than as an
/// offset from whatever chrome the panel drew above it.
fn boxes(s: &PopupScene) -> Vec<(f32, f32, f32, f32)> {
    let at = grid(s).rect;
    grid_cells(s)
        .iter()
        .map(|e| (e.rect.x - at.x, e.rect.y - at.y, e.rect.w, e.rect.h))
        .collect()
}

/// The acceptance geometry, in full. Nine one-character cells on a three
/// by three grid, with `border-collapse` resolved: every cell owns the
/// rule on its left and its top, and only the cells against the grid's
/// right and bottom edges own the closing ones - so two neighbours abut
/// exactly and the rule between them is drawn once.
///
/// A column is `3.5 + 7 + 3.5 = 14` wide and a row `3.5 + 28 + 3.5 = 35`
/// tall, so an interior cell's border box is `1 + 14` by `1 + 35`.
#[test]
fn a_three_by_three_table_places_nine_cells_on_a_grid() {
    let s = gridded(&table(&[tr(&["a", "b", "c"]), tr(&["d", "e", "f"]), tr(&["g", "h", "i"])]), 424.0);

    let col = CELL_PAD + CELL_UNIT + CELL_PAD;
    let row = CELL_PAD + CELL_LINE + CELL_PAD;
    let want: Vec<(f32, f32, f32, f32)> = (0..3)
        .flat_map(|r| {
            (0..3).map(move |c| {
                (
                    c as f32 * (RULE + col),
                    r as f32 * (RULE + row),
                    RULE + col + if c == 2 { RULE } else { 0.0 },
                    RULE + row + if r == 2 { RULE } else { 0.0 },
                )
            })
        })
        .collect();
    assert_eq!(want, boxes(&s));

    let g = grid(&s);
    assert_eq!(4.0 * RULE + 3.0 * col, g.rect.w, "four rules down, three columns");
    assert_eq!(4.0 * RULE + 3.0 * row, g.rect.h);
    assert_eq!(g.rect.h, g.advance, "and the walk advances by the grid");
}

/// The acceptance: a `colSpan` cell is as wide as the columns it covers
/// plus the rule between them - asserted against the cells underneath it,
/// which is the same statement without any arithmetic to get wrong - and
/// the cell after it starts where those columns end.
#[test]
fn a_col_span_cell_is_as_wide_as_the_columns_it_covers() {
    let spanned = concat!(
        r#"{"tag":"tr","content":["#,
        r#"{"tag":"td","colSpan":2,"content":"ab"},"#,
        r#"{"tag":"td","content":"c"}]}"#
    );
    let s = gridded(&table(&[spanned.to_string(), tr(&["d", "e", "f"])]), 424.0);
    let cells = boxes(&s);

    assert_eq!(5, cells.len());
    let (span, after) = (cells[0], cells[1]);
    let (under_a, under_b) = (cells[2], cells[3]);
    assert_eq!(under_a.2 + under_b.2, span.2, "the two columns it covers, rule included");
    assert_eq!(0.0, span.0, "starting where the first of them does");
    assert_eq!(span.0 + span.2, after.0, "and the next cell starts after it");
    assert_eq!(under_b.0 + under_b.2, after.0, "in the third column");
}

/// The acceptance: a `rowSpan` cell covers both its rows, and the next
/// row's cells shift past it into the column it left free.
#[test]
fn a_row_span_cell_covers_both_rows_and_the_next_row_shifts_past_it() {
    let spanned = concat!(
        r#"{"tag":"tr","content":["#,
        r#"{"tag":"td","rowSpan":2,"content":"a"},"#,
        r#"{"tag":"td","content":"b"}]}"#
    );
    let s = gridded(&table(&[spanned.to_string(), tr(&["c"])]), 424.0);
    let cells = boxes(&s);

    assert_eq!(3, cells.len());
    let (span, first, second) = (cells[0], cells[1], cells[2]);
    assert_eq!(0.0, span.1);
    assert_eq!(grid(&s).rect.h, span.3, "the spanning cell covers the whole grid");
    assert_eq!(first.0, second.0, "the displaced cell sits in the second column");
    assert_eq!(span.0 + span.2, second.0, "past the cell that claimed the first");
    assert_eq!(first.1 + first.3, second.1, "and on the row below its neighbour");
}

/// Both spans on one cell. It claims two columns and two rows, so the
/// only free slot left in the second row is the third column - and the
/// two columns it swallowed were sized by it alone, sharing its shortfall
/// evenly because no single-column cell ever asked for them.
#[test]
fn a_cell_spanning_a_row_and_a_column_at_once_lands_on_the_slot_it_claims() {
    let spanned = concat!(
        r#"{"tag":"tr","content":["#,
        r#"{"tag":"td","rowSpan":2,"colSpan":2,"content":"a"},"#,
        r#"{"tag":"td","content":"b"}]}"#
    );
    let s = gridded(&table(&[spanned.to_string(), tr(&["c"])]), 424.0);
    let cells = boxes(&s);

    assert_eq!(3, cells.len());
    let (span, first, second) = (cells[0], cells[1], cells[2]);
    // Its own ask is 14; the two columns hold 0 plus the rule between
    // them, so 6.5 goes to each.
    let col = CELL_PAD + CELL_UNIT + CELL_PAD;
    assert_eq!((0.0, 0.0, RULE + col, grid(&s).rect.h), span);
    assert_eq!(span.0 + span.2, first.0, "the third column starts after both");
    assert_eq!(first.0, second.0, "and the second row's only cell lands in it");
    assert_eq!(first.1 + first.3, second.1);
}

/// The acceptance: a cell with no declared style still reads as a grid.
/// Yomitan's own defaults - a solid `1em / 14` border in the panel's rule
/// colour and `0.25em` of padding - and the text inset by both.
#[test]
fn a_cell_draws_yomitans_border_and_insets_its_text_by_the_padding() {
    let s = gridded(&table(&[tr(&["ab"])]), 424.0);
    let cell = grid_cells(&s)[0];
    let style = cell.block_box.expect("a cell is a box").style;

    assert_eq!(Edges::all(RULE), style.border, "one rule on every edge of a lone cell");
    assert_eq!(Edges::all(BorderStyle::Solid), style.border_style);
    assert_eq!(Theme::dark().collapsed_text, style.border_color);
    assert_eq!(Some(RULE), style.even_border(), "so a painter strokes it once");
    assert_eq!(Edges::all(CELL_PAD), style.padding);

    let text = grid_text(&s)[0];
    assert_eq!(cell.rect.x + RULE + CELL_PAD, text.pen.0, "the text clears both");
    assert_eq!(cell.rect.y + RULE + CELL_PAD, text.pen.1);
    assert_eq!(cell.rect.w - 2.0 * RULE - 2.0 * CELL_PAD, text.wrap_w);
}

/// `1em / 14` is one pixel at the base font size *and scales with the
/// panel*, which is the whole reason the spec states it as a ratio: the
/// same table at twice the size draws a two-pixel rule and twice the
/// padding.
#[test]
fn a_cell_rule_scales_with_the_font_size() {
    let theme = Theme { body_size: 2.0 * GRID_EM, ..Theme::dark() };
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: &rich(&sc(&table(&[tr(&["ab"])]))),
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: false,
            render: RenderSettings::default(),
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run");
    let style = grid_cells(&s)[0].block_box.expect("a cell is a box").style;

    assert_eq!(Edges::all(2.0 * RULE), style.border);
    assert_eq!(Edges::all(2.0 * CELL_PAD), style.padding);
}

/// `border-collapse`, as a box record: an interior cell owns the rule on
/// its left and its top and nothing else, so the rule it shares with the
/// neighbour on its right is drawn once, by that neighbour.
#[test]
fn an_interior_cell_owns_only_its_left_and_top_rule() {
    let s = gridded(&table(&[tr(&["a", "b"]), tr(&["c", "d"])]), 424.0);
    let cells = grid_cells(&s);
    let edges = |e: &SceneElem| e.block_box.expect("a cell is a box").style.border;

    assert_eq!(Edges { top: RULE, right: 0.0, bottom: 0.0, left: RULE }, edges(cells[0]));
    assert_eq!(Edges { top: RULE, right: RULE, bottom: 0.0, left: RULE }, edges(cells[1]));
    assert_eq!(Edges { top: RULE, right: 0.0, bottom: RULE, left: RULE }, edges(cells[2]));
    assert_eq!(Edges::all(RULE), edges(cells[3]), "the far corner closes both");

    let boxes = boxes(&s);
    assert_eq!(boxes[0].0 + boxes[0].2, boxes[1].0, "and neighbours abut exactly");
    assert_eq!(boxes[0].1 + boxes[0].3, boxes[2].1);
}

/// The acceptance: a header cell is bold with a tinted background, as
/// Yomitan draws it. The weight comes from `tag_style`'s HTML-default
/// table, which already knew `th`; the tint is the box property this
/// ticket added.
#[test]
fn a_header_cell_is_bold_and_tinted() {
    let s = gridded(&table(&[mixed_row(&[("th", "a"), ("td", "b")])]), 424.0);
    let cells = grid_cells(&s);
    let fill = |e: &SceneElem| e.block_box.expect("a cell is a box").style.background;

    assert_eq!(Some(Theme::dark().separator), fill(cells[0]));
    assert_eq!(None, fill(cells[1]), "a data cell is not tinted");

    let text = grid_text(&s);
    assert_eq!(700, text[0].spans[0].weight);
    assert_eq!(Theme::dark().body_weight, text[1].spans[0].weight);
}

/// Yomitan writes the same rule on `thead`, so a plain `td` in a header
/// row is a header cell too - which is how a real conjugation table's
/// first row comes out bold without every cell in it being a `th`.
#[test]
fn a_thead_makes_its_data_cells_header_cells() {
    let head = format!(r#"{{"tag":"thead","content":[{}]}}"#, tr(&["a"]));
    let body = format!(r#"{{"tag":"tbody","content":[{}]}}"#, tr(&["b"]));
    let s = gridded(&table(&[head, body]), 424.0);
    let cells = grid_cells(&s);
    let fill = |e: &SceneElem| e.block_box.expect("a cell is a box").style.background;

    assert_eq!(2, cells.len(), "a row group contributes rows and nothing else");
    assert_eq!(Some(Theme::dark().separator), fill(cells[0]));
    assert_eq!(None, fill(cells[1]));

    let text = grid_text(&s);
    assert_eq!(700, text[0].spans[0].weight, "bold is inherited from the thead");
    assert_eq!(Theme::dark().body_weight, text[1].spans[0].weight);
}

/// The acceptance: a table wider than the panel is clipped to it rather
/// than allowed to widen it. The columns are scaled by the one factor
/// that makes them fit and their content rewraps inside the narrower
/// column, so the grid ends exactly on the panel's own content edge.
///
/// At 108 pixels of content the three rules leave 105 to share. Each of
/// the two cells measures 30 units to a 98-pixel block and asks for 105
/// with its padding, so the two together ask for exactly twice what there
/// is: every column is halved to 52.5.
#[test]
fn a_table_wider_than_the_panel_is_scaled_to_fit_inside_it() {
    let long = "a".repeat(30);
    let s = gridded(&table(&[tr(&[&long, &long])]), 132.0);
    let g = grid(&s);

    assert_eq!(108.0, s.content_w);
    assert_eq!(108.0, g.rect.w, "the grid ends on the panel's own content edge");
    assert_eq!(s.origin + s.content_w, g.rect.x + g.rect.w);

    let cells = grid_cells(&s);
    for cell in &cells {
        assert!(
            cell.rect.x + cell.rect.w <= g.rect.x + g.rect.w,
            "every cell is inside the grid: {:?}",
            cell.rect
        );
    }
    let text = grid_text(&s);
    assert_eq!(52.5 - 2.0 * CELL_PAD, text[0].wrap_w, "the column shrank");
    assert_eq!(5, text[0].lines, "and the content rewrapped inside it");
    assert!(text[0].rect.w <= text[0].wrap_w, "no ink escapes the column");
}

/// The same acceptance, stated about the panel itself: the width the
/// panel asks for is the width it was offered, whatever the table inside
/// it wanted. Nothing in the walk derives a panel width from an element's
/// extent, and the grid is what keeps that safe to rely on.
#[test]
fn a_table_wider_than_the_panel_never_widens_the_panel() {
    let long = "a".repeat(30);
    let wide = |glossary: String| {
        let card = Card {
            written: None,
            reading: Some("\u{3055}\u{3064}\u{3060}\u{3093}".into()),
            pos: vec![],
            freq: None,
            blocks: vec![tree("Jitendex", &sc(&glossary))],
            match_len: 4,
        };
        Presentation {
            top: Some(card.clone()),
            collapsed: vec![CollapsedRow {
                written: Some("\u{96d1}\u{97f3}".into()),
                reading: None,
                summary: "noise".into(),
            }],
            all_cards: vec![card],
            sentence: None,
        }
    };
    let plain = grid_scene(&wide(table(&[tr(&["a", "b"])])), 300.0, true);
    let huge = grid_scene(&wide(table(&[tr(&[&long, &long])])), 300.0, true);

    assert!(plain.panel_w.is_some(), "a side column makes the panel state a width");
    assert_eq!(plain.panel_w, huge.panel_w);
    assert_eq!(plain.content_w, huge.content_w);
}

/// The acceptance: a malformed table with more cells than columns does
/// not panic and renders what it can. The longer row decides the grid's
/// width and the short one simply ends early, leaving the slots it never
/// filled empty rather than inventing cells for them.
#[test]
fn a_row_with_more_cells_than_its_neighbour_widens_the_grid() {
    let s = gridded(&table(&[tr(&["a", "b"]), tr(&["c", "d", "e", "f"])]), 424.0);
    let cells = boxes(&s);

    assert_eq!(6, cells.len(), "every written cell is drawn");
    let col = CELL_PAD + CELL_UNIT + CELL_PAD;
    assert_eq!(5.0 * RULE + 4.0 * col, grid(&s).rect.w, "four columns, from the long row");
    assert_eq!(cells[0].0, cells[2].0, "the short row starts at the same column");
    assert_eq!(cells[1].0, cells[3].0);
    assert_eq!(
        cells[1].0 + cells[1].2,
        cells[4].0,
        "and ends where the third column starts, leaving two slots empty"
    );
    assert_eq!(grid(&s).rect.w, cells[5].0 + cells[5].2, "the long row closes the grid");
}

/// A blank in a paradigm is a cell: it takes its slot and draws its
/// border, because a grid missing one box reads as a broken grid rather
/// than as an empty value. It carries no text and no spans, so a painter
/// draws the box and asks the shaper for nothing.
#[test]
fn an_empty_cell_still_draws_its_border() {
    let row = r#"{"tag":"tr","content":[{"tag":"td"},{"tag":"td","content":"b"}]}"#;
    let s = gridded(&table(&[row.to_string()]), 424.0);
    let cells = grid_cells(&s);

    assert_eq!(2, cells.len());
    assert!(cells[0].spans.is_empty(), "nothing to shape");
    assert!(cells[0].text.is_empty());
    let border = cells[0].block_box.expect("still a box").style.border;
    // The last row, but not the last column: the rule on its right
    // belongs to the cell beside it.
    assert_eq!(Edges { top: RULE, right: 0.0, bottom: RULE, left: RULE }, border);
    // An empty column is its padding and no ink.
    assert_eq!(RULE + 2.0 * CELL_PAD, cells[0].rect.w);
    assert_eq!(cells[0].rect.x + cells[0].rect.w, cells[1].rect.x);
}

/// Every cell names the node it came from, so a hit inside a conjugation
/// table resolves to that cell's subtree and ticket 04's renderer
/// reproduces exactly it.
#[test]
fn every_cell_names_the_node_it_came_from() {
    let p = rich(&sc(&table(&[mixed_row(&[("th", "past"), ("td", "ate")])])));
    let s = grid_scene(&p, 424.0, false);
    let doc = &p.top.as_ref().unwrap().blocks[0].entries[0].doc;

    let tags: Vec<Tag> = grid_cells(&s)
        .iter()
        .map(|cell| {
            let path = cell.origin.expect("a cell names its row").path.expect("and its node");
            doc.node(path.resolve(doc).expect("which must exist")).tag
        })
        .collect();
    assert_eq!(vec![Tag::Th, Tag::Td], tags);

    let path = grid_cells(&s)[1].origin.unwrap().path.unwrap();
    assert_eq!(
        vec!["<td>ate</td>".to_string()],
        render_html(doc, Selection::Nodes(&[path]), RoleFilter::CARD)
    );
}

/// A row is as tall as its tallest cell, and a shorter cell beside it
/// sits at the row's top - Yomitan's own `vertical-align: top`. Nothing
/// stretches: the short cell's own line boxes are the ones the measurer
/// reported, which is what a bin's re-measure reproduces.
#[test]
fn a_row_is_as_tall_as_its_tallest_cell_and_the_short_one_stays_at_the_top() {
    let tall = concat!(
        r#"{"tag":"td","content":["#,
        r#"{"tag":"div","content":"a"},{"tag":"div","content":"b"}]}"#
    );
    let row = format!(r#"{{"tag":"tr","content":[{tall},{{"tag":"td","content":"c"}}]}}"#);
    let s = gridded(&table(&[row]), 424.0);
    let text = grid_text(&s);

    // Two lines a `LINE_GAP` apart, padded above and below.
    let want = 2.0 * CELL_PAD + 2.0 * CELL_LINE + LINE_GAP;
    assert_eq!(2.0 * RULE + want, grid(&s).rect.h);
    assert_eq!(3, text.len());
    assert_eq!(text[0].pen.1, text[2].pen.1, "both cells start at the row's top");
    assert_eq!(CELL_LINE, text[2].rect.h, "and the short one is still one line");
}

/// A cell taller than the rows it spans grows them, evenly, so that its
/// own box ends exactly where its last row does. The rows grow; the text
/// inside them does not.
#[test]
fn a_row_span_cell_taller_than_its_rows_grows_them_evenly() {
    let tall = concat!(
        r#"{"tag":"td","rowSpan":2,"content":["#,
        r#"{"tag":"div","content":"a"},{"tag":"div","content":"b"},"#,
        r#"{"tag":"div","content":"c"}]}"#
    );
    let first = format!(r#"{{"tag":"tr","content":[{tall},{{"tag":"td","content":"d"}}]}}"#);
    let s = gridded(&table(&[first, tr(&["e"])]), 424.0);
    let cells = boxes(&s);

    // Three lines is 92; two 35-tall rows and the rule between them hold
    // 71, so each row takes half the 28-pixel shortfall.
    let row = CELL_PAD + CELL_LINE + CELL_PAD + 14.0;
    assert_eq!(3.0 * RULE + 2.0 * row, grid(&s).rect.h);
    assert_eq!(grid(&s).rect.h, cells[0].3, "the spanning cell covers both rows");
    assert_eq!(RULE + row, cells[1].3, "and each row grew by half the shortfall");
    assert_eq!(RULE + row, cells[2].1, "the second row starts after the first");

    let text = grid_text(&s);
    assert!(text.iter().all(|e| e.rect.h == CELL_LINE), "no line was stretched");
}

/// A link inside a cell is clickable where the cell landed, not where the
/// cell was measured: the grid lays a cell out at the table's top and
/// drops it into its row afterwards, and the targets it earned move with
/// it.
#[test]
fn a_link_inside_a_cell_is_clickable_where_the_cell_landed() {
    let linked = concat!(
        r#"{"tag":"tr","content":[{"tag":"td","content":"#,
        r#"{"tag":"a","href":"?query=x","content":"go"}}]}"#
    );
    let s = gridded(&table(&[tr(&["a"]), linked.to_string()]), 424.0);
    let cell = grid_cells(&s)[1];
    let hit = s
        .hits
        .iter()
        .find(|h| h.action == HitAction::DrillDown("x".into()))
        .expect("the cross-reference earns a target");

    assert_eq!(cell.pen.1, hit.y, "on the second row, where the cell ended up");
    assert_eq!(Some(cell.pen.0), hit.x);
    assert!(hit.y > grid(&s).rect.y, "and not at the table's own top");
}

/// A table of nothing draws nothing, rather than a stray rule.
#[test]
fn a_table_with_no_cells_draws_nothing() {
    let s = gridded(r#"[{"tag":"table"},"after"]"#, 424.0);

    assert!(grid_cells(&s).is_empty());
    assert!(!s.elems.iter().any(|e| e.kind == ElemKind::Table));
    assert!(texts(&s).contains(&"after"), "and the text beside it still renders");
}

/// A table is a block: it closes the paragraph before it, opens none of
/// its own, and the walk stacks whatever follows under the whole grid.
#[test]
fn a_table_advances_the_walk_by_its_own_height() {
    let s = gridded(
        &format!(r#"["before",{},"after"]"#, table(&[tr(&["a"]), tr(&["b"])])),
        424.0,
    );
    let text = grid_text(&s);
    let g = grid(&s);

    assert_eq!(
        vec!["before", "a", "b", "after"],
        text.iter().map(|e| e.text.as_str()).collect::<Vec<_>>()
    );
    let after = text[3];
    assert_eq!(g.pen.1 + g.rect.h + LINE_GAP, after.pen.1, "stacked under the whole grid");
}

/// A table inside a list item is indented once, by the table, and its
/// cells do not pay the indent a second time. `Block::inherited` carries
/// the indent down to every child, which is exactly why the grid has to
/// clear it for the cells inside it.
#[test]
fn a_table_inside_a_list_item_pays_the_indent_once() {
    let item = format!(r#"{{"tag":"li","content":[{}]}}"#, table(&[tr(&["a", "b"])]));
    let s = gridded(&format!(r#"{{"tag":"ul","content":[{item}]}}"#), 424.0);
    let level = LIST_INDENT_EM * GRID_EM;
    let cells = grid_cells(&s);

    assert_eq!(s.origin + level, grid(&s).rect.x, "one level, on the table");
    assert_eq!(grid(&s).rect.x, cells[0].rect.x, "and not again on its cells");
    assert_eq!(cells[0].rect.x + cells[0].rect.w, cells[1].rect.x);
}

/// A table inside a cell is a grid of its own. A cell holds pieces, not
/// paragraphs, so the block pass lays a nested table out through the same
/// two methods the outer one used - and the inner grid's cells are placed
/// inside the outer cell's content box.
#[test]
fn a_table_inside_a_cell_is_a_grid_of_its_own() {
    let inner = table(&[tr(&["a", "b"])]);
    let outer = format!(
        r#"{{"tag":"table","content":{{"tag":"tr","content":{{"tag":"td","content":[{inner}]}}}}}}"#
    );
    let s = gridded(&outer, 424.0);
    let tables: Vec<&SceneElem> =
        s.elems.iter().filter(|e| e.kind == ElemKind::Table).collect();
    let cells = grid_cells(&s);

    assert_eq!(2, tables.len(), "two grids");
    assert_eq!(3, cells.len(), "the outer cell and the inner row's two");
    let (outer_cell, first, second) = (cells[0], cells[1], cells[2]);
    assert_eq!(outer_cell.pen, (first.rect.x, first.rect.y), "the inner grid starts at the content box");
    assert_eq!(first.rect.x + first.rect.w, second.rect.x);
    assert!(
        second.rect.x + second.rect.w <= outer_cell.rect.x + outer_cell.rect.w,
        "and stays inside it"
    );
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
                render: RenderSettings::default(),
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
    let (elems, _) = build_elements(&one_card(&[], Some(7671)), &theme, false, false, RenderSettings::default());
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
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default());
    assert!(!elems.iter().any(|e| matches!(e, Elem::Corner(_))));
}

#[test]
fn part_of_speech_is_dimmed_metadata_not_body_text() {
    let theme = Theme::dark();
    let (elems, _) = build_elements(&one_card(&["noun", "suru"], Some(1)), &theme, false, false, RenderSettings::default());
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
    let (elems, _) = build_elements(&one_card(&["noun"], Some(7671)), &theme, true, false, RenderSettings::default());
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
    let (elems, _) = build_elements(&one_card(&[], Some(1)), &Theme::dark(), false, false, RenderSettings::default());
    assert!(!elems
        .iter()
        .any(|e| matches!(e, Elem::Text(line) if line.text.contains('·'))));
}

#[test]
fn the_headword_is_a_headword_element_not_text() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default());
    assert!(
        elems.iter().any(|e| matches!(e, Elem::Headword { .. })),
        "expected a Headword element for the headword"
    );
}

#[test]
fn headword_prefix_u16_is_zero_without_anki_marks() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default());
    let hw = elems.iter().find_map(|e| match e {
        Elem::Headword { prefix_u16, .. } => Some(*prefix_u16),
        _ => None,
    });
    assert_eq!(Some(0), hw);
}

#[test]
fn show_back_adds_a_back_button_element() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), true, false, RenderSettings::default());
    assert!(matches!(&elems[0], Elem::BackButton(_)));
}

#[test]
fn no_back_button_without_show_back() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default());
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
    let (elems, _) = build_elements(&with_collapsed(), &Theme::dark(), false, false, RenderSettings::default());
    for e in &elems {
        if let Elem::Collapsed(_, line) = e {
            assert!(!line.text.starts_with('\u{2713}'), "no check marks on collapsed rows");
        }
    }
}

#[test]
fn side_panel_false_keeps_collapsed_rows_inline() {
    let (elems, side) = build_elements(&with_collapsed(), &Theme::dark(), false, false, RenderSettings::default());
    assert!(side.is_empty());
    assert!(elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
}

#[test]
fn side_panel_true_moves_collapsed_rows_to_side() {
    let (elems, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true, RenderSettings::default());
    assert!(!elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
    assert_eq!(2, side.len());
    assert!(side[0].text.contains('\u{96D1}'));
}

#[test]
fn side_entries_carry_expand_indices() {
    let (_, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true, RenderSettings::default());
    assert_eq!(0, side[0].idx);
    assert_eq!(1, side[1].idx);
}

#[test]
fn side_entries_show_headword_only() {
    let (_, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true, RenderSettings::default());
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
    let (elems, _) = build_elements(&p, &theme, false, false, RenderSettings::default());
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

// ---- inline images ----

/// `FakeMeasure`'s answers for one [`IMAGE_SPACER`], which is what the
/// image pass probes for and every expectation below is arithmetic over:
/// one no-break space advances half its size, and a line hangs its
/// baseline a whole size down from its own top.
const SPACER_ADVANCE: f32 = ADVANCE;
const SPACER_ASCENT: f32 = LINE_H / 2.0;

/// One recorded media row, as `dict::media::probe` writes it.
fn recorded(format: MediaFormat, w: f32, h: f32) -> Intrinsic {
    Intrinsic { format, width: w, height: h, aspect: w / h }
}

/// A card holding one structured-content row, from the dictionary whose
/// build recorded `media`.
///
/// `dict_id` is 7 rather than `NO_ROW`, so a test can assert the whole
/// media key an element carries and not just its path.
fn imaged(content: &str, media: &[(&str, Intrinsic)]) -> Presentation {
    card_with(vec![GlossBlock {
        dict_name: "\u{5b57}\u{901a}".to_string(),
        dict_id: 7,
        entries: vec![row_media(
            &sc(content),
            &[],
            media.iter().map(|(p, i)| ((*p).to_string(), *i)).collect(),
        )],
    }])
}

/// Every image element, in draw order.
fn images(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Image).collect()
}

/// The one image element of a scene with exactly one.
fn one_image(s: &PopupScene) -> &SceneElem {
    let found = images(s);
    assert_eq!(1, found.len(), "expected one image element, got {}", found.len());
    found[0]
}

/// The paragraph an image sits inside: the one gloss element that is not
/// the image.
fn image_host(s: &PopupScene) -> &SceneElem {
    s.elems
        .iter()
        .find(|e| e.kind == ElemKind::Text && e.text.contains(IMAGE_RISER))
        .expect("the paragraph that reserved the image's room")
}

/// Rung one of the sizing ladder, and story 16: a `height: 1em` gaiji is
/// the size of the text it sits in, and its bottom is on that line's
/// baseline - not floating above it, not hanging below it.
///
/// The declared size wins over the recorded one, which is deliberately
/// nothing like it here (20x10 against 15x15).
#[test]
fn a_declared_em_size_beats_the_recorded_one_and_sits_on_the_baseline() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png","width":1.0,"height":1.0,"sizeUnits":"em"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 20.0, 10.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);
    let host = image_host(&s);

    assert_eq!((BOX_EM, BOX_EM), (img.rect.w, img.rect.h), "one em on each axis");
    // Its bottom on the baseline: the line is one whole em tall above it
    // (`SPACER_ASCENT` of the riser's size), so a `1em` box exactly fills
    // that space and its top is the line's top.
    assert_eq!(host.pen.1, img.rect.y, "top of the first line");
    assert_eq!(host.pen.0, img.rect.x, "and at the paragraph's own pen");
    assert_eq!(0.0, img.advance, "the paragraph already stacked its line");
}

/// Rung two, and story 18: no declared size at all takes the size the
/// build recorded, so an undeclared gaiji neither collapses nor overflows.
/// 99 807 census image nodes are this shape.
#[test]
fn an_image_with_no_declared_size_takes_its_recorded_dimensions() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 20.0, 10.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);

    assert_eq!((20.0, 10.0), (img.rect.w, img.rect.h));
}

/// The middle of the ladder, and the whole reason the media row carries
/// `aspect` as its own column: `height: 1em` and no width is what 字通 and
/// 三省堂 both write, and one length plus a ratio is the other length.
#[test]
fn one_declared_length_takes_the_other_from_the_recorded_aspect() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png","height":1.0,"sizeUnits":"em"}"#,
        &[("g/x.png", recorded(MediaFormat::Svg, 40.0, 20.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);

    assert_eq!((2.0 * BOX_EM, BOX_EM), (img.rect.w, img.rect.h), "aspect 2:1");
}

/// `sizeUnits: px` is a scene pixel, where the absent field and `em` are
/// multiples of the text's own size.
#[test]
fn size_units_px_is_taken_as_scene_pixels() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png","width":12.0,"height":9.0,"sizeUnits":"px"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 20.0, 10.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);

    assert_eq!((12.0, 9.0), (img.rect.w, img.rect.h));
}

/// Rung three: neither declared nor recorded is a one-em square. Reachable
/// only when the store has no row, which means no bytes either - so what
/// this sizes is the placeholder box, and the element carries no key for a
/// bin to go looking with.
#[test]
fn an_image_with_neither_size_nor_bytes_is_a_one_em_placeholder_box() {
    let p = imaged(r#"{"tag":"img","path":"g/x.png"}"#, &[]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);

    assert_eq!((BOX_EM, BOX_EM), (img.rect.w, img.rect.h));
    let scene_image = img.image.as_ref().expect("an image element names its asset");
    assert_eq!(None, scene_image.key, "no row, so nothing to fetch");
    assert_eq!(None, scene_image.format);
    assert!(img.text.is_empty(), "and no alt to draw instead");
    assert!(img.rect.w > 0.0 && img.rect.h > 0.0, "never a gap");
}

/// Story 19: a missing asset shows what it stood for. The `alt` goes into
/// the flow as ordinary text rather than onto an image element, because
/// that is the *better* rung - real text wraps with the sentence around it
/// - and Jitendex writes its gaiji's alt in `data`, not as an attribute.
#[test]
fn missing_media_renders_its_alt_text_in_the_flow() {
    let p = imaged(
        concat!(
            r#"[{"tag":"img","path":"g/x.svg","data":{"gaiji":"","alt":"[\u5bfe]"}},"#,
            r#"{"tag":"span","content":"\u3080\u304b\u3046"}]"#
        ),
        &[],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert!(images(&s).is_empty(), "no asset, so no image element");
    let body = bodies(&s);
    assert_eq!(1, body.len(), "the alt joins the sentence, it does not break it");
    assert_eq!("[\u{5bfe}]\u{3080}\u{304b}\u{3046}", body[0].text);
}

/// A `title` is the next best label, and an attribute is read as well as a
/// `data` entry: 三省堂 writes `title` beside `sizeUnits`.
#[test]
fn a_title_stands_in_for_a_missing_alt() {
    let p = imaged(r#"{"tag":"img","path":"g/x.svg","title":"\u77ed"}"#, &[]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert!(images(&s).is_empty());
    assert_eq!("\u{77ed}", bodies(&s)[0].text);
}

/// The undecodable rung, which is the one real data takes on this machine:
/// the row exists, so the rect is right and the key is there, and the
/// element still carries its `alt` as one ordinary span - so a bin that
/// cannot rasterise the format draws that instead of nothing, with no
/// second text path.
#[test]
fn a_stored_asset_carries_both_its_key_and_its_alt_fallback() {
    let p = imaged(
        r#"{"tag":"img","path":"g/\u5bfe.svg","height":1.0,"sizeUnits":"em","data":{"alt":"[\u5bfe]"}}"#,
        &[("g/\u{5bfe}.svg", recorded(MediaFormat::Svg, 30.0, 30.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);

    let scene_image = img.image.as_ref().expect("a stored asset names itself");
    assert_eq!(Some(MediaKey::new(7, "g/\u{5bfe}.svg")), scene_image.key);
    assert_eq!(Some(MediaFormat::Svg), scene_image.format);
    assert_eq!("[\u{5bfe}]", img.text, "the fallback a painter draws");
    assert_eq!(1, img.spans.len());
    assert_eq!(BOX_EM, img.spans[0].size, "at the text size it stands in for");
}

/// The measurement rule, and the thing that would break silently: an image
/// occupies inline space because a *span* asks the measurer for it, never
/// because a pass edited the line boxes after the wrap. Both bins
/// re-measure an element's own spans to paint it, so this asserts the run
/// the walk handed the seam - the reservation is exactly the image's width,
/// and the line is exactly as tall as the image needs.
#[test]
fn an_image_buys_its_room_from_the_measurer_and_not_after_the_wrap() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png","width":1.0,"height":1.0,"sizeUnits":"em"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 15.0, 15.0))],
    );
    let (s, asked) = measured(&Theme::dark(), &p, false);
    let host = image_host(&s);

    // `ceil(4 * 15/15)` spacers, sized so their advance is the image's
    // width exactly: 4 units at half their size each.
    let spacers = 4;
    let spacer = host
        .spans
        .iter()
        .find(|sp| host.text[sp.at as usize..(sp.at + sp.len) as usize].contains(IMAGE_SPACER))
        .expect("the spacer run is a span of the paragraph");
    assert_eq!(spacers, (spacer.len as usize) / IMAGE_SPACER.len());
    assert_eq!(
        BOX_EM,
        spacers as f32 * SPACER_ADVANCE * spacer.size,
        "the reservation is the image's own width"
    );
    // And the riser, whose ascent share is the room above the baseline.
    let riser = host
        .spans
        .iter()
        .find(|sp| host.text[sp.at as usize..(sp.at + sp.len) as usize] == *IMAGE_RISER)
        .expect("the riser is a span too");
    assert_eq!(BOX_EM, SPACER_ASCENT * riser.size, "and the height, above the baseline");
    // The probe is a real request through the seam, because only a
    // measurer knows either ratio (ADR-0013).
    assert!(
        asked.iter().any(|a| a.text == IMAGE_SPACER && a.size == BOX_EM),
        "the pass probes one spacer at the image's own em"
    );
    // The paragraph's own height counts the line the riser grew, with
    // nothing after the wrap touching it.
    assert_eq!(BOX_EM * LINE_H, host.rect.h);
    assert_eq!(1, host.lines, "and it is one line");
}

/// An image mid-sentence stays mid-sentence: one paragraph, one line, the
/// image between the two words rather than on a line of its own. The
/// spacers are non-breaking glue, so no wrap can split the reservation or
/// separate it from the word beside it.
#[test]
fn an_image_mid_sentence_wraps_with_the_text_and_forces_no_break() {
    let p = imaged(
        concat!(
            r#"["ab",{"tag":"img","path":"g/x.png","height":1.0,"sizeUnits":"em"},"#,
            r#""cd"]"#
        ),
        &[("g/x.png", recorded(MediaFormat::Png, 15.0, 15.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let host = image_host(&s);
    let img = one_image(&s);

    assert_eq!(1, host.lines, "one line, not three");
    assert_eq!(1, images(&s).len());
    // `ab` is two units at the body size, so the image starts after them
    // and `cd` after the image.
    let ab = 2.0 * BOX_EM * ADVANCE;
    assert_eq!(host.pen.0 + ab, img.rect.x);
    assert_eq!(ab + BOX_EM + 2.0 * BOX_EM * ADVANCE, host.rect.w, "text, image, text");
}

/// `verticalAlign` is ticket 07's machinery, reused rather than rebuilt: a
/// raised image is raised off the same baseline a raised span is, and the
/// pass reserves the rise as well as the height so the line above is not
/// overlapped.
#[test]
fn a_raised_image_clears_the_baseline_by_its_own_vertical_align() {
    let p = imaged(
        concat!(
            r#"{"tag":"img","path":"g/x.png","height":1.0,"sizeUnits":"em","#,
            r#""style":{"verticalAlign":"super"}}"#
        ),
        &[("g/x.png", recorded(MediaFormat::Png, 15.0, 15.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);
    let host = image_host(&s);

    let rise = BOX_EM * SUPER_RISE;
    // The line grew by the rise, and the image sits that far above where a
    // baseline-aligned one would.
    assert_eq!((BOX_EM + rise) * LINE_H, host.rect.h, "the rise is reserved");
    assert_eq!(host.pen.1, img.rect.y, "so the raised box still starts at the top");
    let baseline = BOX_EM + rise;
    assert_eq!(baseline - rise - BOX_EM, img.rect.y - host.pen.1);
}

/// A wide short banner must not make its line as tall as it is wide. The
/// spacer count rides the aspect ratio for exactly this reason, and the
/// spacer's size is capped at the riser's so the image, never the
/// reservation, decides the line's height.
#[test]
fn a_wide_image_reserves_its_width_without_growing_its_line() {
    let p = imaged(
        r#"{"tag":"img","path":"g/wide.png"}"#,
        &[("g/wide.png", recorded(MediaFormat::Png, 160.0, 10.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let host = image_host(&s);
    let img = one_image(&s);

    assert_eq!((160.0, 10.0), (img.rect.w, img.rect.h));
    assert_eq!(10.0 * LINE_H, host.rect.h, "as tall as the image, not as wide");
    assert_eq!(160.0, host.rect.w, "and as wide as it");
}

/// An image is content: it earns a paragraph of its own rather than being
/// dropped as whitespace, which is what the riser is for - the spacer run
/// alone is whitespace and `flush` would have thrown the paragraph away.
#[test]
fn a_paragraph_holding_only_an_image_survives() {
    let p = imaged(
        r#"{"tag":"div","content":{"tag":"img","path":"g/x.png"}}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 12.0, 12.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert_eq!(1, images(&s).len(), "the image is still there");
    assert_eq!((12.0, 12.0), (one_image(&s).rect.w, one_image(&s).rect.h));
}

/// A `type: image` glossary item is the same replaced element as an `img`
/// tag, and the plain-text renderer's own drop of it is unchanged.
#[test]
fn an_image_item_is_an_image_too() {
    let entry = row_media(
        r#"[{"type":"image","path":"g/x.png"}]"#,
        &[],
        vec![("g/x.png".to_string(), recorded(MediaFormat::Png, 9.0, 9.0))],
    );
    let p = card_with(vec![GlossBlock {
        dict_name: "\u{5b57}\u{901a}".to_string(),
        dict_id: 7,
        entries: vec![entry],
    }]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert_eq!((9.0, 9.0), (one_image(&s).rect.w, one_image(&s).rect.h));
}

/// Story 45/46: an image is addressable as the node it is, not as the
/// paragraph around it.
#[test]
fn an_image_element_names_the_node_it_came_from() {
    let p = imaged(
        r#"["a",{"tag":"img","path":"g/x.png"}]"#,
        &[("g/x.png", recorded(MediaFormat::Png, 9.0, 9.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let origin = one_image(&s).origin.expect("an image from a tree has an address");

    assert_eq!(7, origin.dict_id);
    assert!(origin.path.is_some(), "and names its own node");
}

/// Read and carried, acted on by nothing: the spec builds no
/// hover-to-reveal affordance, and 26 dictionaries declare `collapsed`
/// over 243 264 nodes - so a later ticket must not have to re-derive them.
#[test]
fn collapsed_and_collapsible_are_carried_and_change_nothing() {
    let p = imaged(
        concat!(
            r#"{"tag":"img","path":"g/x.png","appearance":"monochrome","#,
            r#""background":false,"collapsed":true,"collapsible":true}"#
        ),
        &[("g/x.png", recorded(MediaFormat::Png, 15.0, 15.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);
    let scene_image = img.image.as_ref().unwrap();

    assert!(scene_image.collapsed && scene_image.collapsible);
    assert!(!scene_image.background, "and `background: false` paints no fill");
    assert_eq!(Appearance::Monochrome, scene_image.appearance);
    assert_eq!((15.0, 15.0), (img.rect.w, img.rect.h), "still rendered inline");
}

/// Yomitan's default is to draw the backing, so an absent field is `true`.
/// Every image node in the census's samples turns it off.
#[test]
fn an_undeclared_background_is_drawn() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 9.0, 9.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert!(one_image(&s).image.as_ref().unwrap().background);
}

/// One row of the bound's table: the asset's format and appearance, the box
/// it resolved to, the device pixel ratio, and what a painter must do.
type TintCase = (MediaFormat, Appearance, (f32, f32), f32, Tint);

/// The rasterise-and-tint bound, which is the expensive path: taken only
/// for a gaiji-sized vector, at twice the device pixel ratio, clamped on
/// the longest edge. A larger monochrome asset composites untinted, and a
/// raster mask tints without rasterising because it already has pixels.
#[test]
fn the_tint_bound_admits_a_gaiji_sized_vector_and_refuses_an_illustration() {
    let of = |format, appearance| SceneImage {
        key: Some(MediaKey::new(7, "g/x")),
        format: Some(format),
        appearance,
        background: false,
        collapsed: false,
        collapsible: false,
    };
    let box_of = |w: f32, h: f32| SceneRect { x: 0.0, y: 0.0, w, h };
    let em = 15.0;
    let cases: &[TintCase] = &[
        // Not a mask: nothing to do, whatever the format.
        (MediaFormat::Svg, Appearance::Auto, (15.0, 15.0), 1.0, Tint::None),
        (MediaFormat::Png, Appearance::Auto, (15.0, 15.0), 1.0, Tint::None),
        // A raster mask has pixels already.
        (MediaFormat::Png, Appearance::Monochrome, (15.0, 15.0), 1.0, Tint::Alpha),
        (MediaFormat::Avif, Appearance::Monochrome, (900.0, 900.0), 1.0, Tint::Alpha),
        // A gaiji-sized vector: twice the ratio, both axes.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (15.0, 15.0),
            1.0,
            Tint::Raster(30, 30),
        ),
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (15.0, 15.0),
            2.0,
            Tint::Raster(60, 60),
        ),
        // Exactly on the 4em bound, and clamped on the longest edge:
        // 60x30 at 2x on a 2.0 ratio is 240x120, under the clamp.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (4.0 * em, 2.0 * em),
            2.0,
            Tint::Raster(240, 120),
        ),
        // Past the clamp: 4em square at 3x is 360, so both axes scale to
        // fit 256.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (4.0 * em, 4.0 * em),
            3.0,
            Tint::Raster(256, 256),
        ),
        // Past the 4em bound on one axis: an illustration, not a mask.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (4.0 * em + 0.5, 15.0),
            1.0,
            Tint::None,
        ),
        // A degenerate box has nothing to rasterise into.
        (MediaFormat::Svg, Appearance::Monochrome, (0.0, 15.0), 1.0, Tint::None),
    ];
    for &(format, appearance, (w, h), dpr, want) in cases {
        assert_eq!(
            want,
            of(format, appearance).tint(box_of(w, h), em, dpr),
            "{format} {appearance:?} {w}x{h} at {dpr}x"
        );
    }
}

/// Two paragraphs, two images, and each reads its own row back: the walk's
/// image list is renumbered per paragraph exactly as its links and its
/// readings are, so neither paragraph names an index the other owns.
#[test]
fn two_paragraphs_each_place_their_own_image() {
    let p = imaged(
        concat!(
            r#"[{"tag":"div","content":["one",{"tag":"img","path":"g/a.png"}]},"#,
            r#"{"tag":"div","content":["two",{"tag":"img","path":"g/b.png"}]}]"#
        ),
        &[
            ("g/a.png", recorded(MediaFormat::Png, 10.0, 10.0)),
            ("g/b.png", recorded(MediaFormat::Png, 20.0, 20.0)),
        ],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let found = images(&s);

    assert_eq!(2, found.len());
    assert_eq!((10.0, 10.0), (found[0].rect.w, found[0].rect.h));
    assert_eq!((20.0, 20.0), (found[1].rect.w, found[1].rect.h));
    let keys: Vec<Option<&MediaKey>> =
        found.iter().map(|e| e.image.as_ref().unwrap().key.as_ref()).collect();
    assert_eq!(
        vec![Some(&MediaKey::new(7, "g/a.png")), Some(&MediaKey::new(7, "g/b.png"))],
        keys
    );
    assert!(found[1].rect.y > found[0].rect.y, "and the second is below the first");
}

/// An image inside a cross-reference is as clickable as the word beside
/// it: the spacers carry the link, so the hit target covers the asset.
#[test]
fn an_image_inside_a_link_is_part_of_its_hit_target() {
    let p = imaged(
        concat!(
            r#"{"tag":"a","href":"?query=\u732b","content":["#,
            r#"{"tag":"img","path":"g/x.png"}]}"#
        ),
        &[("g/x.png", recorded(MediaFormat::Png, 16.0, 16.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);
    // The headword drills down per character too, so name the target the
    // cross-reference itself asked for.
    let hit = s
        .hits
        .iter()
        .find(|h| h.action == HitAction::DrillDown("\u{732b}".to_string()))
        .expect("a cross-reference earns a target");

    assert_eq!(Some(img.rect.x), hit.x);
    assert_eq!(Some(img.rect.w), hit.w, "the whole asset, not a sliver");
}

/// Ticket 20: a `<ruby>` whose base is a gaiji image keeps its reading. 251
/// nodes across eight dictionaries write this shape, and in most of them the
/// mark is editorial rather than decorative - 三省堂 and 大辞林 put their
/// 表外字 mark over the gaiji this way, and 岩波 puts a real reading there.
/// A browser lays a reading over the ruby base box and an image is a legal
/// base, so what a Yomitan reader sees is the mark above the picture.
///
/// Pinned to the image's own box on both axes: the reading is centred over
/// the asset's width, and its bottom edge is the asset's top edge. A fix that
/// drew the mark anywhere else - over the spacer's own text ascent, or off
/// the top of the paragraph - fails here rather than passing quietly.
#[test]
fn a_reading_over_an_image_base_sits_on_the_assets_own_top_edge() {
    let p = imaged(
        concat!(
            r#"{"tag":"ruby","content":[{"tag":"img","path":"g/x.svg","#,
            r#""width":1.0,"height":1.0,"sizeUnits":"em"},"#,
            r#"{"tag":"rt","content":"\u00d7"}]}"#
        ),
        &[("g/x.svg", recorded(MediaFormat::Svg, 30.0, 30.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);
    let host = image_host(&s);

    assert_eq!(
        vec!["\u{d7}"],
        host.ruby.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
        "the mark the dictionary wrote over the gaiji",
    );
    let read = &host.ruby[0];
    assert_eq!((BOX_EM, BOX_EM), (img.rect.w, img.rect.h), "a one-em gaiji");
    // One half-size unit of a reading, over a one-em box.
    assert_eq!(BOX_EM * RUBY_RATIO * ADVANCE, read.w);
    assert_eq!(BOX_EM * RUBY_RATIO * LINE_H, read.h);
    assert_eq!(
        img.rect.x + (img.rect.w - read.w) / 2.0,
        host.pen.0 + read.x,
        "centred over the asset, not over the line",
    );
    assert_eq!(
        img.rect.y,
        host.pen.1 + read.y + read.h,
        "and its bottom edge is the asset's own top edge",
    );
}

/// The reading is pinned to the *asset* and not to the line, so it follows
/// the picture wherever `verticalAlign` puts it.
///
/// Not the ticket's distilled fragment verbatim: 岩波国語辞典 hangs `ｘ` over
/// 赤鱏's gaiji and declares the alignment on a `span` wrapping the `img`,
/// and `verticalAlign` is not inherited (CSS says so, and [`tag_style`]
/// agrees), so the alignment a wrapper carries is a question for ticket 07
/// and not for this one. What this pins is the alignment reaching the image
/// itself, which is the shape whose geometry the reading has to follow.
#[test]
fn a_reading_follows_a_gaiji_its_vertical_align_moved() {
    let p = imaged(
        concat!(
            r#"{"tag":"ruby","content":[{"tag":"img","path":"iwakoku8/218080.svg","#,
            r#""width":1.0,"height":1.0,"sizeUnits":"em","#,
            r#""style":{"verticalAlign":"text-bottom"}},"#,
            r#"{"tag":"rt","content":"\uff58"}]}"#
        ),
        &[("iwakoku8/218080.svg", recorded(MediaFormat::Svg, 30.0, 30.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let img = one_image(&s);
    let host = image_host(&s);
    let read = &host.ruby[0];

    assert!(
        img.rect.y > host.pen.1 + BOX_EM,
        "text-bottom dropped the gaiji below where a baseline-aligned one sits",
    );
    assert_eq!(
        img.rect.y,
        host.pen.1 + read.y + read.h,
        "and the reading went down with it",
    );
}

// ---- render settings ----

/// A scene over `p` at chosen render settings, in the box every fixture
/// above uses.
///
/// Through `layout::scene` deliberately: the settings are a decision
/// table consumed in the scene builder, so what a knob is worth is what
/// the finished scene holds, not what an inner pass was handed.
fn shown(p: &Presentation, render: RenderSettings) -> PopupScene {
    let theme = Theme::dark();
    let mut m = FakeMeasure::default();
    scene(
        &SceneRequest {
            presentation: p,
            theme: &theme,
            max_w: 424.0,
            max_h: 4000.0,
            show_back: false,
            side_panel: false,
            render,
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run")
}

/// The shipped settings with one knob flipped.
fn without(edit: fn(&mut RenderSettings)) -> RenderSettings {
    let mut render = RenderSettings::default();
    edit(&mut render);
    render
}

/// The gloss element whose glyphs are exactly `text`.
///
/// Not [`bodies`], which selects on the body font size - and a
/// `fontSize` declaration is precisely what a styling test has to be
/// able to change.
///
/// The [`PILL_SPACER`]s come out first. An inline box buys its own
/// horizontal room with runs of them *in the paragraph's text*
/// (`pill::measure_pills`), so a styled gloss and the same gloss with
/// styling off are two different strings - and which element rendered
/// which gloss is the only thing this selector is asking.
fn gloss_of<'a>(s: &'a PopupScene, text: &str) -> &'a SceneElem {
    let glyphs = |e: &SceneElem| e.text.replace(PILL_SPACER, "");
    s.elems
        .iter()
        .find(|e| e.kind == ElemKind::Text && glyphs(e) == text)
        .unwrap_or_else(|| panic!("no gloss element says {text:?} in {:?}", texts(s)))
}

/// The span of `elem` whose glyphs are exactly `text`.
///
/// A pill's own run is not one span. A no-break space at each end bought
/// the box its padding room and a third one its margin
/// (`pill::measure_pills`), and those carry the box's style at a solved
/// size - so `spans[0]` is the room and not the word.
fn span_of<'a>(elem: &'a SceneElem, text: &str) -> &'a ElemSpan {
    elem.spans
        .iter()
        .find(|s| elem.text[s.at as usize..(s.at + s.len) as usize] == *text)
        .unwrap_or_else(|| panic!("no span says {text:?} in {:?}", elem.text))
}

/// A gloss carrying two example sentences and an attribution, each under a
/// real census `data` hook so the parser classifies them.
///
/// The two examples are deliberately two *different* conventions: Jitendex's
/// ASCII `content=example-sentence` and 明鏡国語辞典's Japanese `example=`,
/// the key that used to keep 38 892 example nodes on screen while Jitendex
/// lost every one of its own. The whole point of ticket 15 is that these two
/// now behave the same, so the fixture would fail on a classifier that
/// covered only one alphabet.
const EDITORIAL: &str = concat!(
    r#"[{"tag":"span","content":"to eat"},"#,
    r#"{"tag":"div","data":{"content":"example-sentence"},"#,
    r#""content":"\u3054\u98ef\u3092\u98df\u3079\u308b"},"#,
    r#"{"tag":"div","data":{"example":""},"#,
    r#""content":"\u30d1\u30f3\u3092\u98df\u3079\u308b"},"#,
    r#"{"tag":"ul","data":{"content":"attribution"},"#,
    r#""content":[{"tag":"li","content":"JMdict"}]}]"#
);

/// [`EDITORIAL`] parsed once, behind both the panel and the card.
///
/// One `Arc`, so a test can ask the two renderers about the same document
/// rather than about two parses of one string - which is what makes story
/// 42's "hidden here, present there" an assertion about the *filters* and
/// not about the fixture.
fn editorial() -> (std::sync::Arc<crate::dict::gloss::GlossDoc>, Presentation) {
    let doc = std::sync::Arc::new(crate::dict::gloss::GlossDoc::parse(&sc(EDITORIAL)));
    let p = card_with(vec![GlossBlock {
        dict_name: "Jitendex".to_string(),
        dict_id: crate::present::NO_ROW,
        entries: vec![GlossEntry {
            entry_id: crate::present::NO_ROW,
            glosses: crate::dict::gloss::plain_items(&doc),
            tags: vec![],
            doc: std::sync::Arc::clone(&doc),
            media: Vec::new(),
        }],
    }]);
    (doc, p)
}

/// A glossary list, the shape compact mode is defined over.
const GLOSSARY_LIST: &str = concat!(
    r#"{"tag":"ul","content":[{"tag":"li","content":"chatting"},"#,
    r#"{"tag":"li","content":"a chat"},{"tag":"li","content":"idle talk"}]}"#
);

/// The layout-mode acceptance bullet, at the seam the spec names.
///
/// Compact is an inline transform and not a different tree: the same
/// three items, the same three markers, one element instead of three,
/// and [`ITEM_SEPARATOR`] between them - which is exactly how Yomitan
/// and Hoshi Reader implement it (`li { display: inline }` plus a
/// separator on every item after the first). The separator is asserted
/// as itself, because "one element" would also be true of a mode that
/// silently dropped two items.
#[test]
fn compact_joins_a_glossary_list_into_one_separated_element_and_roomy_stacks_it() {
    let p = rich(&sc(GLOSSARY_LIST));

    let stacked = shown(&p, RenderSettings::default());
    assert_eq!(
        vec!["chatting", "a chat", "idle talk"],
        bodies(&stacked).iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        "roomy is the default, and it stacks"
    );

    let joined = shown(&p, without(|r| r.stack_items = false));
    let one = one_body(&joined);
    let dot = bullet();
    assert_eq!(
        format!("{dot}chatting{ITEM_SEPARATOR}{dot}a chat{ITEM_SEPARATOR}{dot}idle talk"),
        one.text,
        "one element, the same markers, the separator between the items"
    );
    assert_eq!(
        2,
        one.text.matches(ITEM_SEPARATOR).count(),
        "one separator per pair, never a trailing one"
    );
    assert!(one.marker.is_empty(), "no gutter in a compact list, so nothing hangs");
}

/// Story 28, concretely: a user who wants the terse one-line popup
/// chibipop used to draw can have it by choosing compact.
///
/// Two dictionaries, four glosses between them, and one line each -
/// which is the whole of what "one line per dictionary" means. Roomy
/// draws the same content on four.
#[test]
fn compact_gives_the_terse_one_line_per_dictionary_popup_back() {
    let p = card_with(vec![
        GlossBlock {
            dict_name: "\u{5927}\u{8f9e}\u{6797}".to_string(),
            dict_id: crate::present::NO_ROW,
            entries: vec![row_of(
                &sc(concat!(
                    r#"{"tag":"ul","content":[{"tag":"li","content":"chatting"},"#,
                    r#"{"tag":"li","content":"a chat"}]}"#
                )),
                &[],
            )],
        },
        GlossBlock {
            dict_name: "Jitendex".to_string(),
            dict_id: crate::present::NO_ROW,
            entries: vec![row_of(
                &sc(concat!(
                    r#"{"tag":"ul","content":[{"tag":"li","content":"idle talk"},"#,
                    r#"{"tag":"li","content":"chit-chat"}]}"#
                )),
                &[],
            )],
        },
    ]);

    let terse = shown(&p, without(|r| r.stack_items = false));
    let dot = bullet();
    assert_eq!(
        vec![
            format!("{dot}chatting{ITEM_SEPARATOR}{dot}a chat"),
            format!("{dot}idle talk{ITEM_SEPARATOR}{dot}chit-chat"),
        ],
        bodies(&terse).iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
        "one body element per dictionary, each carrying that dictionary's whole gloss"
    );
    assert_eq!(4, bodies(&shown(&p, RenderSettings::default())).len(), "roomy draws four");
}

/// Examples on and off, at the element count the spec asks for - and the
/// ticket-15 acceptance that two dictionaries' examples behave the same.
///
/// The two example blocks are one ASCII hook and one Japanese one. Before
/// ticket 15 the popup drew *neither* branch of this test on real data:
/// every node parsed unclassified, the setting had nothing to bite on, and
/// Jitendex's own examples went unconditionally through a six-name drop
/// list that never named 明鏡's key at all.
#[test]
fn examples_off_drops_every_dictionarys_examples_and_leaves_the_gloss() {
    let (_, p) = editorial();

    let all = shown(&p, RenderSettings::default());
    let with = bodies(&all);
    assert_eq!(4, with.len(), "the gloss, two examples, and the attribution");
    assert_eq!(
        2,
        with.iter().filter(|e| e.text.contains('\u{98df}')).count(),
        "both dictionaries' examples draw: {:?}",
        with.iter().map(|e| e.text.clone()).collect::<Vec<_>>()
    );

    let without_examples = shown(&p, without(|r| r.roles.examples = false));
    let kept = bodies(&without_examples);
    assert_eq!(2, kept.len(), "two elements fewer");
    assert!(
        !kept.iter().any(|e| e.text.contains('\u{98df}')),
        "and it is both examples that went, ASCII hook and Japanese alike: {kept:?}"
    );
    assert!(kept.iter().any(|e| e.text == "to eat"), "the gloss stays");
    assert!(kept.iter().any(|e| e.text == "JMdict"), "and so does the attribution");
}

/// Story 27: attributions are a separate knob, so a user can keep
/// sources without keeping three sentences per sense. All four
/// combinations, over one parse of one document.
#[test]
fn attributions_are_hidden_independently_of_examples() {
    let (_, p) = editorial();
    let eaten = [
        "\u{3054}\u{98ef}\u{3092}\u{98df}\u{3079}\u{308b}",
        "\u{30d1}\u{30f3}\u{3092}\u{98df}\u{3079}\u{308b}",
    ];

    let count = |render: RenderSettings| {
        let s = shown(&p, render);
        bodies(&s).iter().map(|e| e.text.clone()).collect::<Vec<_>>()
    };
    assert_eq!(4, count(RenderSettings::default()).len(), "both knobs on draws everything");
    assert_eq!(
        vec!["to eat".to_string(), eaten[0].to_string(), eaten[1].to_string()],
        count(without(|r| r.roles.attributions = false)),
        "the sources go and both examples stay"
    );
    assert_eq!(
        vec!["to eat".to_string(), "JMdict".to_string()],
        count(without(|r| r.roles.examples = false)),
        "and the other way round"
    );
    assert_eq!(
        vec!["to eat".to_string()],
        count(without(|r| {
            r.roles.examples = false;
            r.roles.attributions = false;
        })),
        "both off leaves the gloss alone"
    );
}

/// Story 42, at the seam where it is finally reachable: one document, one
/// parse, the example gone from the panel and present on the card.
///
/// The two filters are independent by construction - the popup resolves
/// its own from config at `build_elements` and the card renderer takes
/// `RoleFilter::CARD`, which no setting reaches. Until ticket 15 every
/// node was unclassified, so neither filter could tell an example from a
/// gloss and this assertion could not be written.
#[test]
fn an_example_hidden_in_the_popup_is_still_on_the_card() {
    let (doc, p) = editorial();

    let hidden = shown(&p, without(|r| r.roles.examples = false));
    let panel = bodies(&hidden);
    assert!(
        !panel.iter().any(|e| e.text.contains('\u{98df}')),
        "the panel hides them: {panel:?}"
    );

    let card = render_html(&doc, Selection::Whole, RoleFilter::CARD).join("");
    assert!(card.contains("\u{3054}\u{98ef}\u{3092}\u{98df}\u{3079}\u{308b}"), "{card}");
    assert!(card.contains("\u{30d1}\u{30f3}\u{3092}\u{98df}\u{3079}\u{308b}"), "{card}");
}

/// Story 32.
///
/// A part-of-speech label classifies to `Role::PartOfSpeech` from
/// `data.content = "part-of-speech-info"`, which Jitendex writes over
/// 48 776 nodes - and the popup has always dropped it because the card's
/// own `pos` field prints it above the glosses. The setting is what makes
/// that a choice.
#[test]
fn part_of_speech_labels_render_only_when_the_setting_asks_for_them() {
    let p = rich(&sc(concat!(
        r#"[{"tag":"span","data":{"content":"part-of-speech-info"},"content":"noun"},"#,
        r#"{"tag":"span","content":"chatting"}]"#
    )));

    let default = shown(&p, RenderSettings::default());
    let hidden = bodies(&default);
    assert_eq!(1, hidden.len(), "off by default, as the panel has always drawn it");
    assert_eq!("chatting", hidden[0].text);

    let asked = shown(&p, without(|r| r.roles.part_of_speech = true));
    let shown_labels = bodies(&asked);
    assert_eq!(1, shown_labels.len(), "still one paragraph: a label is inline content");
    assert_eq!("nounchatting", shown_labels[0].text, "the label joins the line before it");
}

/// Images off removes the image element and leaves no rect behind, and
/// takes the `alt` text instead of a hole in the word.
#[test]
fn images_off_removes_the_image_element_and_keeps_its_alt_text() {
    let p = imaged(
        r#"[{"tag":"img","path":"g/x.png","alt":"\u5b57"},{"tag":"span","content":"tsu"}]"#,
        &[("g/x.png", recorded(MediaFormat::Png, 16.0, 16.0))],
    );

    let with = shown(&p, RenderSettings::default());
    assert_eq!(1, images(&with).len(), "on by default: one asset, one element");

    let off = shown(&p, without(|r| r.images = false));
    assert!(images(&off).is_empty(), "no image element");
    assert!(
        off.elems.iter().all(|e| e.image.is_none()),
        "and no rect left behind on any other element"
    );
    assert_eq!(
        vec!["\u{5b57}tsu"],
        bodies(&off).iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        "the `alt` text stands in, because a gaiji is a character"
    );
}

/// Story 30: styling off draws the entry in the theme's own font and
/// colours. A node declaring a colour and a box produces neither.
#[test]
fn styling_off_renders_in_the_themes_own_colours_and_draws_no_box() {
    let theme = Theme::dark();
    let p = rich(&sc(concat!(
        r##"{"tag":"span","style":{"color":"#ff0000","padding":"0.4em","##,
        r##""backgroundColor":"#003366","fontSize":"2em","fontWeight":"bold"},"##,
        r#""content":"chatting"}"#
    )));

    let styled_scene = shown(&p, RenderSettings::default());
    let styled = gloss_of(&styled_scene, "chatting");
    let ink = span_of(styled, "chatting");
    assert_eq!((255, 0, 0), ink.color, "the dictionary's colour, honoured");
    assert_eq!(2.0 * theme.body_size, ink.size, "its size too");
    assert!(!styled.inline_boxes.is_empty(), "and its pill, drawn");

    let plain_scene = shown(&p, without(|r| r.styling = false));
    let plain = gloss_of(&plain_scene, "chatting");
    assert_eq!("chatting", plain.text, "the same text");
    assert_eq!(
        vec![theme.body_text],
        plain.spans.iter().map(|s| s.color).collect::<Vec<_>>(),
        "in the theme's own colour"
    );
    assert_eq!(vec![theme.body_size], plain.spans.iter().map(|s| s.size).collect::<Vec<_>>());
    assert_eq!(
        vec![theme.body_weight],
        plain.spans.iter().map(|s| s.weight).collect::<Vec<_>>(),
    );
    assert!(plain.inline_boxes.is_empty(), "and no box at all");
    assert_eq!(None, plain.block_box, "inside or outside the line");
}

/// The same gate, on the third reader of a resolved style record.
///
/// `listStyleType` is a declaration like any other, and Jitendex's ①②③
/// sense numbering is nothing but that, so styling off has to fall back
/// to the browser's own marker - otherwise a styled dictionary would not
/// render identically to an unstyled one.
#[test]
fn styling_off_falls_back_to_the_browsers_own_list_marker() {
    let p = rich(&sc(concat!(
        r#"{"tag":"ul","content":[{"tag":"li","style":{"listStyleType":"\"\u2460\""},"#,
        r#""content":"chatting"}]}"#
    )));

    let honoured_scene = shown(&p, RenderSettings::default());
    let honoured = one_body(&honoured_scene);
    assert_eq!(format!("\u{2460}{MARKER_GAP}"), one_marker(honoured).text);

    let plain_scene = shown(&p, without(|r| r.styling = false));
    let plain = one_body(&plain_scene);
    assert_eq!(bullet(), one_marker(plain).text, "a `ul`'s own initial value, as CSS has it");
}

/// Story 35: the height cap and the scrollbar keep working when a
/// setting makes an entry taller.
///
/// Roomy over the same tree is three lines where compact is one, so the
/// two settings put different content heights against one cap. What must
/// hold either way is the panel's own rule - the view is the content or
/// the cap, whichever is smaller - and that the taller of the two is the
/// one with more to scroll. `max_scroll` and the scrollbar are computed
/// from exactly those two numbers.
#[test]
fn a_taller_entry_still_clamps_to_the_height_cap_and_scrolls() {
    let p = rich(&sc(GLOSSARY_LIST));
    let short = |render| {
        let theme = Theme::dark();
        let mut m = FakeMeasure::default();
        scene(
            &SceneRequest {
                presentation: &p,
                theme: &theme,
                max_w: 424.0,
                max_h: 120.0,
                show_back: false,
                side_panel: false,
                render,
                anki: None,
            },
            &mut m,
        )
        .expect("FakeMeasure never refuses a run")
    };

    let cap = 120.0f32;
    let scroll_of = |s: &PopupScene| max_scroll(s.content_h.ceil() as i32, s.view_h.ceil() as i32);

    let roomy = short(RenderSettings::default());
    let compact = short(without(|r| r.stack_items = false));

    assert_eq!(roomy.content_h.min(cap), roomy.view_h, "the view is the content or the cap");
    assert_eq!(compact.content_h.min(cap), compact.view_h, "and that holds at either setting");
    assert!(roomy.content_h > cap, "roomy overflows this box");
    assert!(
        compact.content_h < roomy.content_h,
        "and compact is the shorter of the two: {} against {}",
        compact.content_h,
        roomy.content_h
    );
    assert!(scroll_of(&roomy) > scroll_of(&compact), "so the taller one has more to scroll");
    assert!(scroll_of(&roomy) > 0, "and it does scroll rather than overflow");
}

// ---- a dictionary's own styles.css ----

/// One dictionary's block, from a raw glossary and that dictionary's own
/// stylesheet.
///
/// The fold is `dict::sheet`'s and the hover path runs it between the parse
/// and the tree cache (`SqliteDictionary::entries`), so a fixture reproduces
/// it by calling the same two functions. Everything below this line is the
/// renderer's own, and it has no idea that CSS was involved: a stylesheet
/// declaration reaches it as a resolved style record and nothing else.
fn css_tree(dict: &str, glossary: &str, css: &str) -> GlossBlock {
    let sheet = crate::dict::sheet::Sheet::compile(css);
    let mut doc = crate::dict::gloss::GlossDoc::parse(glossary);
    crate::dict::sheet::apply(&mut doc, &sheet);
    let doc = std::sync::Arc::new(doc);
    GlossBlock {
        dict_name: dict.to_string(),
        dict_id: crate::present::NO_ROW,
        entries: vec![GlossEntry {
            entry_id: crate::present::NO_ROW,
            glosses: crate::dict::gloss::plain_items(&doc),
            tags: Vec::new(),
            doc,
            media: Vec::new(),
        }],
    }
}

/// Every box the scene drew, block and inline, in draw order.
fn drawn_boxes(s: &PopupScene) -> Vec<(&str, BoxStyle)> {
    s.elems
        .iter()
        .flat_map(|e| {
            e.block_box
                .into_iter()
                .chain(e.inline_boxes.iter().copied())
                .map(move |b| (e.text.as_str(), b.style))
        })
        .collect()
}

/// 明鏡国語辞典 第三版, whose box properties live **only** in its stylesheet.
/// The CSS is that dictionary's own `span[data-sc-fbox]` rule verbatim, and
/// the glossary carries not one inline `style` anywhere - which is the state
/// the census puts 13 of 52 structured-content dictionaries in, and for
/// which tickets 07 and 08 drew nothing at all before this.
///
/// The arithmetic, so a reader can redo it: `body_size` is 15, the rule's
/// own `font-size: 0.8em` makes the element 12, and every box length is a
/// fraction of the element's *own* size - `padding: 0.1em` is 1.2,
/// `border-width: 0.05em` is 0.6, `border-radius: 0.2em` is 2.4.
#[test]
fn a_css_only_dictionary_draws_its_boxes_through_the_scene() {
    let p = card_with(vec![css_tree(
        "明鏡国語辞典 第三版",
        &sc(r#"[{"tag":"span","data":{"fbox":"1"},"content":"書き方"},"のこと"]"#),
        "span[data-sc-fbox] {
             margin-inline-end: 0.25em;
             padding: 0.1em;
             font-size: 0.8em;
             font-weight: normal;
             border-style: solid;
             border-width: 0.05em;
             border-color: var(--text-color);
             border-radius: 0.2em;
             word-break: keep-all;
         }",
    )]);
    let s = laid_out(&p, 400.0, 4000.0, false, false);
    let boxes = drawn_boxes(&s);
    assert_eq!(1, boxes.len(), "one box, on the fbox span: {boxes:?}");
    let (text, style) = boxes[0];
    assert!(text.contains("書き方"), "on the span's own run: {text:?}");
    assert_eq!(Edges::all(1.2), style.padding, "padding: 0.1em of a 12px element");
    assert_eq!(Edges::all(0.6), style.border, "border-width: 0.05em");
    assert_eq!(Edges::all(BorderStyle::Solid), style.border_style);
    assert_eq!(2.4, style.radius, "border-radius: 0.2em");
    // `margin-inline-end` is a logical property this build does not map, and
    // `border-color: var(--text-color)` is a custom property it cannot
    // substitute. Both are dropped and counted; the border still draws,
    // because CSS's initial `border-color` is `currentColor` and ticket 08
    // seeds it from the element's own resolved colour.
    assert_eq!(Edges::all(0.0), style.margin, "a logical margin is dropped");
    assert_eq!(Theme::dark().body_text, style.border_color, "currentColor stands");
    assert_eq!(None, style.background);
}

/// 字通, the other dictionary the ticket names, and a *descendant* selector
/// on a CJK `data` key: `[data-sc-h3] span[data-sc筆画]`, its own rule
/// verbatim. Two assertions in one, because the interesting failure is the
/// ancestor constraint rather than the box: the same span outside a
/// `data-sc-h3` must draw nothing.
#[test]
fn a_descendant_rule_draws_a_box_only_where_its_ancestor_holds() {
    let css = "[data-sc-h3] span[data-sc筆画] {
                   color: #a96e36;
                   background: #fff9f6;
                   border-radius: 4px;
                   border-style: solid;
                   border-width: 0.1em;
                   padding: 1px 2px;
                   font-size: 0.8em;
               }";
    let under = laid_out(
        &card_with(vec![css_tree(
            "字通",
            &sc(concat!(
                r#"{"tag":"div","data":{"h3":"1"},"content":["#,
                r#"{"tag":"span","data":{"筆画":"7"},"content":"七"}]}"#,
            )),
            css,
        )]),
        400.0,
        4000.0,
        false,
        false,
    );
    let boxes = drawn_boxes(&under);
    assert_eq!(1, boxes.len(), "{boxes:?}");
    let style = boxes[0].1;
    // An absolute `px` is scaled against Yomitan's own 14px base, so a
    // dictionary's pixel grows with the panel instead of shrinking on a
    // dense screen (ticket 07's [`css_len`]). The element is 12px, so
    // `4px` is 12 * 4 / 14.
    assert_eq!(12.0 * 4.0 / YOMITAN_BASE_PX, style.radius, "border-radius: 4px");
    assert_eq!(Edges::all(1.2), style.border, "border-width: 0.1em of a 12px element");
    assert_eq!(
        Edges {
            top: 12.0 / YOMITAN_BASE_PX,
            right: 24.0 / YOMITAN_BASE_PX,
            bottom: 12.0 / YOMITAN_BASE_PX,
            left: 24.0 / YOMITAN_BASE_PX,
        },
        style.padding,
        "the two-value shorthand, split top/bottom then right/left",
    );
    // `background` is the multi-property shorthand, which this build does not
    // map - only `background-color`. Dropped and counted.
    assert_eq!(None, style.background);

    let outside = laid_out(
        &card_with(vec![css_tree(
            "字通",
            &sc(r#"{"tag":"span","data":{"筆画":"7"},"content":"七"}"#),
            css,
        )]),
        400.0,
        4000.0,
        false,
        false,
    );
    assert!(
        drawn_boxes(&outside).is_empty(),
        "no `data-sc-h3` ancestor, so no box: {:?}",
        drawn_boxes(&outside),
    );
}

/// Jitendex's pill, the one the census counts over 48 776 nodes and which is
/// CSS-only: the rule is `span[data-sc-class="tag"]` verbatim, and the entry
/// declares no inline box property at all.
///
/// `misc-info` rather than `part-of-speech-info`, and not arbitrarily: a
/// part-of-speech pill is lifted out of the flow into the card's own labels
/// (`GlossDoc::is_part_of_speech`), so the pill that actually draws inline is
/// one of the other five Jitendex tags - `misc-info`, `field-info`,
/// `dialect-info`, `lang-source-wasei`, `forms-label`. Both carry the same
/// `data-sc-class="tag"`, so this is the same rule either way.
#[test]
fn the_jitendex_pill_reaches_the_scene_as_a_box() {
    let p = card_with(vec![css_tree(
        "Jitendex.org",
        &sc(concat!(
            r#"[{"tag":"span","data":{"class":"tag","content":"misc-info"},"#,
            r#""content":"abbr."},"a thing"]"#,
        )),
        "span[data-sc-class=\"tag\"] {
             border-radius: 0.3em;
             font-size: 0.8em;
             font-weight: bold;
             margin-right: 0.5em;
             padding: 0.2em 0.3em;
             vertical-align: text-bottom;
             word-break: keep-all;
         }
         span[data-sc-content=\"misc-info\"] {
             background-color: #565656;
             color: white;
         }",
    )]);
    let s = laid_out(&p, 400.0, 4000.0, false, false);
    let boxes = drawn_boxes(&s);
    // One entry, one style. This pill carries `data.content`, which opens a
    // line, and an inline tag, which is what decides its box is the run's
    // and not the paragraph's - so the resolved box reaches the scene
    // exactly once. It used to reach it twice, as a `block_box` and as an
    // `inline_box` over the same style, and a bin looping over
    // `SceneElem::boxes()` painted it twice; see
    // `a_pill_carrying_a_content_marker_draws_one_box_and_not_two`.
    assert_eq!(1, boxes.len(), "{boxes:?}");
    let (text, style) = boxes[0];
    assert!(text.contains("abbr."), "{text:?}");
    assert_eq!(12.0 * 0.3, style.radius, "border-radius: 0.3em of a 12px element");
    assert_eq!(
        Edges { top: 12.0 * 0.2, right: 12.0 * 0.3, bottom: 12.0 * 0.2, left: 12.0 * 0.3 },
        style.padding,
        "padding: 0.2em 0.3em",
    );
    assert_eq!(
        Edges { top: 0.0, right: 6.0, bottom: 0.0, left: 0.0 },
        style.margin,
        "margin-right: 0.5em, and no other edge",
    );
    assert_eq!(Some((0x56, 0x56, 0x56)), style.background);
    // And the text half of the same record reached the span, at the same
    // time and through the same fold.
    let pill = s.elems.iter().find(|e| e.text.contains("abbr.")).expect("the pill run");
    assert_eq!(12.0, pill.font_size, "font-size: 0.8em");
    assert_eq!(BOLD_WEIGHT, pill.weight, "font-weight: bold");
}

/// Jitendex's other box, and the one `rem` was found on: the rule is
/// `div[data-sc-class="extra-box"]` verbatim from the archive's own
/// `styles.css`, `rem` and unreadable `calc()` and all. 101 360 of the
/// library's 435 448 entries carry one.
///
/// One fact a reader should not have to rediscover: the `border-width`
/// is `calc(3em / var(--font-size-no-units, 14))`, which no part of this
/// build reads, so the box declares a left `solid` style over a used
/// width of zero and draws no rule - which leaves margin and padding as
/// the whole of what it does.
///
/// The second half of this test is the real shape, and it used to draw
/// **nothing**: a real extra-box holds two `div`s and nothing else, so
/// the paragraph it opened was flushed empty and its box went with it. A
/// block's box is now a container around every paragraph the block
/// emits, so the box reaches the scene once and frames both children.
#[test]
fn jitendexs_extra_box_resolves_its_rem_lengths() {
    let sheet = "div[data-sc-class=\"extra-box\"] {
             border-radius: 0.4rem;
             border-style: none none none solid;
             border-width: calc(3em / var(--font-size-no-units, 14));
             margin-bottom: 0.5rem;
             margin-top: 0.5rem;
             padding: 0.5rem;
             width: fit-content;
         }";
    let p = card_with(vec![css_tree(
        "Jitendex.org",
        &sc(r#"{"tag":"div","data":{"class":"extra-box","content":"xref"},"content":"See also"}"#),
        sheet,
    )]);
    let s = laid_out(&p, 400.0, 4000.0, false, false);
    let style = block_box(box_around(&s, "See also")).style;

    assert_eq!(0.4 * BOX_EM, style.radius, "border-radius: 0.4rem");
    assert_eq!(Edges::all(0.5 * BOX_EM), style.padding, "padding: 0.5rem");
    assert_eq!(
        Edges { top: 0.5 * BOX_EM, right: 0.0, bottom: 0.5 * BOX_EM, left: 0.0 },
        style.margin,
        "margin-top and margin-bottom: 0.5rem, and no other edge",
    );
    assert_eq!(
        Edges::default(),
        style.border_used(),
        "a `calc()` width is unreadable, so the declared left rule draws nothing",
    );
    assert!(style.spaces(), "margin and padding are the whole of this box");
    assert!(!style.paints(), "and it has no ink at all");

    // The shape the library actually ships: the box holds `div`s and no
    // text of its own.
    let real = card_with(vec![css_tree(
        "Jitendex.org",
        &sc(concat!(
            r#"{"tag":"div","data":{"class":"extra-box","content":"xref"},"content":["#,
            r#"{"tag":"div","content":"See also"},"#,
            r#"{"tag":"div","content":"猫"}]}"#
        )),
        sheet,
    )]);
    let s = laid_out(&real, 400.0, 4000.0, false, false);
    let outer = one_block_box(&s);
    let pad = 0.5 * BOX_EM;

    assert_eq!(style, block_box(outer).style, "the same resolved box, now reaching the scene");
    assert_eq!(
        pad + 2.0 * BODY_LINE + LINE_GAP + pad,
        block_box(outer).rect.h,
        "one border box around both children",
    );
    for para in bodies(&s) {
        assert_eq!(s.origin + pad, para.pen.0, "and both are inset by its padding");
    }
}

/// The setting from ticket 14 governs a stylesheet declaration exactly as it
/// governs an inline one, because after the fold there is one record and one
/// gate over it. This is the assertion that there is no second switch.
#[test]
fn styling_off_drops_a_stylesheet_box_as_well_as_an_inline_one() {
    let p = card_with(vec![css_tree(
        "明鏡国語辞典 第三版",
        &sc(concat!(
            r#"[{"tag":"span","data":{"fbox":"1"},"content":"css"},"#,
            r##"{"tag":"span","style":{"padding":"4px","backgroundColor":"#333"},"##,
            r#""content":"inline"}]"#,
        )),
        "span[data-sc-fbox] { padding: 0.1em; border-radius: 0.2em; background-color: #eee }",
    )]);
    let theme = Theme::dark();
    let laid = |styling: bool| {
        let mut m = FakeMeasure::default();
        scene(
            &SceneRequest {
                presentation: &p,
                theme: &theme,
                max_w: 400.0,
                max_h: 4000.0,
                show_back: false,
                side_panel: false,
                render: RenderSettings { styling, ..RenderSettings::default() },
                anki: None,
            },
            &mut m,
        )
        .expect("FakeMeasure never refuses a run")
    };
    assert_eq!(2, drawn_boxes(&laid(true)).len(), "one box from CSS, one from inline");
    assert!(drawn_boxes(&laid(false)).is_empty(), "off means neither applies");
}

/// Jitendex's two list rules, which are the reason its marker is not simply
/// `•`: the outer sense-group list takes `＊` and the glossary list inside a
/// sense takes `none`. Both are CSS-only, and the second is written with
/// native `&` nesting.
///
/// The tree is the real one, read out of Jitendex's own record: a
/// `ul[sense-groups]` of `li[sense-group]`, each holding a bare `ol` of
/// `li[sense]` whose inline `listStyleType` numbers the sense, each of those
/// holding a `ul[glossary]` of the gloss text.
#[test]
fn a_stylesheet_sets_and_suppresses_a_list_marker() {
    let p = card_with(vec![css_tree(
        "Jitendex.org",
        &sc(concat!(
            r#"{"tag":"ul","data":{"content":"sense-groups"},"content":["#,
            r#"{"tag":"li","data":{"content":"sense-group"},"content":["#,
            r#"{"tag":"ol","content":["#,
            r#"{"tag":"li","data":{"content":"sense"},"#,
            r#""style":{"listStyleType":"\"\u2460\""},"content":["#,
            r#"{"tag":"ul","data":{"content":"glossary"},"#,
            r#""content":[{"tag":"li","content":"to eat"}]}]}]}]}]}"#,
        )),
        "ul[data-sc-content=\"sense-groups\"] { list-style-type: \"＊\" }
         li[data-sc-content=\"sense\"] {
             & ul[data-sc-content=\"glossary\"] { list-style-type: none }
         }",
    )]);
    let s = laid_out(&p, 400.0, 4000.0, false, false);
    let markers: Vec<Vec<&str>> = s
        .elems
        .iter()
        .filter(|e| !e.marker.is_empty())
        .map(|e| e.marker.iter().map(|m| m.text.as_str()).collect())
        .collect();
    // `＊` from the stylesheet on the outer list, `①` from the item's own
    // inline `listStyleType`, and nothing at all from the glossary list the
    // stylesheet silenced - where the default would have drawn `•`.
    assert_eq!(vec![vec!["＊ ", "① "]], markers, "{:?}", texts(&s));
}

/// The gap a reader of あくどい saw between the sense number and its
/// glosses. Jitendex declares the glossary list's own indent -
/// `ul[data-sc-content="glossary"] { padding-left: 0.25em }`, the one list
/// in the 97-archive corpus that declares any - and a browser makes that
/// padding *replace* the default list gutter, exactly as it replaces the
/// UA's `padding-inline-start` and Yomitan's `--list-padding1` rule.
/// Charging [`LIST_INDENT_EM`] on top left 1.9em of blank gutter after
/// `①` where the dictionary asked for 0.5em: the two `padding-left:
/// 0.25em` declarations (the sense item's and the glossary list's) and
/// nothing else.
#[test]
fn a_lists_own_padding_replaces_the_default_gutter() {
    let p = card_with(vec![css_tree(
        "Jitendex.org",
        &sc(concat!(
            r#"{"tag":"ul","data":{"content":"sense-groups"},"content":["#,
            r#"{"tag":"li","data":{"content":"sense-group"},"content":["#,
            r#"{"tag":"ol","content":["#,
            r#"{"tag":"li","data":{"content":"sense"},"#,
            r#""style":{"listStyleType":"\"\u2460\""},"content":["#,
            r#"{"tag":"ul","data":{"content":"glossary"},"#,
            r#""content":[{"tag":"li","content":"to eat"}]}]}]}]}]}"#,
        )),
        "ul[data-sc-content=\"sense-groups\"] { list-style-type: \"＊\" }
         li[data-sc-content=\"sense-group\"] { padding-left: 0.25em }
         li[data-sc-content=\"sense\"] {
             padding-left: 0.25em;
             & ul[data-sc-content=\"glossary\"] {
                 list-style-type: none;
                 padding-left: 0.25em;
             }
         }",
    )]);
    let s = laid_out(&p, 400.0, 4000.0, false, false);
    let item = s.elems.iter().find(|e| e.text == "to eat").expect("the gloss");

    // Two levels of default gutter - the sense-groups list and the bare
    // `ol`, neither of which declares a padding - plus the three declared
    // `padding-left: 0.25em`: the sense-group item's, the sense item's,
    // and the glossary list's own, which *replaced* its level.
    assert_eq!(s.origin + 2.0 * LEVEL + 0.75 * BOX_EM, item.pen.0, "{:?}", item.marker);
    // The sense number still hangs at the `ol`'s content edge.
    assert_eq!(2, item.marker.len(), "the outer ＊ and the sense's ①");
    let sense = &item.marker[1];
    assert_eq!(
        s.origin + 2.0 * LEVEL + 0.25 * BOX_EM - marker_w("① "),
        item.pen.0 + sense.x,
    );
    // The whole defect in one number: what stands between the marker box
    // and the first glyph of the gloss is the two remaining paddings.
    assert_eq!(0.5 * BOX_EM, -sense.x - marker_w("① "));
}

/// A `<ruby>` that reaches no base at all still draws its reading.
///
/// 岩波国語辞典　第八版 writes 円周率 as `▽「π（<ruby><rt>パイ</rt></ruby>）」で表す。`:
/// the `<ruby>` has one child and it is the `<rt>`. CSS ruby gives an
/// annotation with no base an anonymous empty ruby base, so the annotation is
/// laid out over nothing and stays visible - and Yomitan declares no `ruby`
/// or `rt` rule, so a Yomitan reader gets that browser default and reads
/// `パイ` between the parentheses. Dropping it leaves the reader an empty
/// pair of parentheses where the dictionary spelled the letter out.
///
/// Pinned to *where* the reading lands and not merely to its being drawn: an
/// annotation placed over the following word would satisfy *no dropped text*
/// and still be wrong. With no base to centre on, the reading stands flush at
/// the pen its base would have started at.
#[test]
fn a_reading_with_no_base_stands_at_the_pen_its_base_would_have_taken() {
    let unit = Theme::dark().body_size * ADVANCE;
    let base_line = Theme::dark().body_size * LINE_H;
    let read_line = Theme::dark().body_size * RUBY_RATIO * LINE_H;
    // The fake hangs every line off an ascent of its tallest span's own
    // size, so its ascent share is `1 / LINE_H`.
    let ascent = 1.0 / LINE_H;

    let s = laid_out(
        &rich(&sc(concat!(
            r#"{"tag":"div","content":["\u25bd\u300c\u03c0\uff08","#,
            r#"{"tag":"ruby","content":{"tag":"rt","content":"\u30d1\u30a4"}},"#,
            r#""\uff09\u300d\u3067\u8868\u3059\u3002"]}"#,
        ))),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!(
        "\u{25bd}\u{300c}\u{3c0}\u{ff08}\u{2060}\u{ff09}\u{300d}\u{3067}\u{8868}\u{3059}\u{3002}",
        gloss.text,
        "the base-less ruby still buys its line a filler",
    );
    assert_eq!(
        vec!["\u{30d1}\u{30a4}"],
        gloss.ruby.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
        "and the reading the dictionary wrote is drawn",
    );
    // The line grew for the reading exactly as a based one grows it.
    assert_eq!(1, gloss.lines);
    assert_eq!(base_line + read_line / ascent, gloss.rect.h);

    let read = &gloss.ruby[0];
    // Four characters stand before the `<ruby>`, so the anonymous empty base
    // begins four units in. Two half-size kana are one base wide.
    assert_eq!(4.0 * unit, read.x, "flush at the pen, not centred on nothing");
    assert_eq!(2.0 * unit * RUBY_RATIO, read.w);
    assert_eq!(read_line, read.h);
    assert_eq!(0.0, read.y, "in the room its own filler bought");
}

/// The same shape written by an archive that lost its kanji: Onomatoproject
/// writes ちゃらちゃら's example as
/// `お<ruby>父<rt>とう</rt></ruby>さんは<ruby>""<rt>きら</rt></ruby>いだ！`, so 嫌 is
/// missing from the bytes themselves. A browser renders the author's broken
/// markup readably and a Yomitan reader reads `きらいだ`; rendering less than
/// a browser renders from the same bytes is the divergence, whoever wrote the
/// bytes.
///
/// The neighbouring prose is the archive's own, because *no dropped text* is
/// stated as containment: a fragment whose surrounding prose happened to hold
/// the two kana `きら` would pass for the wrong reason. Both readings are
/// pinned, so a fix that placed the base-less one over 父 fails here.
#[test]
fn an_empty_ruby_base_keeps_its_reading_beside_the_base_that_has_one() {
    let unit = Theme::dark().body_size * ADVANCE;
    let s = laid_out(
        &rich(&sc(concat!(
            r#"{"tag":"div","content":["#,
            r#"{"tag":"span","content":"\u3057\u305f\u3084\u3064\u3001\u304a"},"#,
            r#"{"tag":"ruby","content":["\u7236","#,
            r#"{"tag":"rt","content":"\u3068\u3046"}]},"#,
            r#"{"tag":"span","content":"\u3055\u3093\u306f"},"#,
            r#"{"tag":"ruby","content":["","#,
            r#"{"tag":"rt","content":"\u304d\u3089"}]},"#,
            r#"{"tag":"span","content":"\u3044\u3060\uff01"}]}"#,
        ))),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert!(
        !gloss.text.contains('\u{304d}'),
        "the prose around the ruby holds no き of its own: {:?}",
        gloss.text,
    );
    assert_eq!(
        vec!["\u{3068}\u{3046}", "\u{304d}\u{3089}"],
        gloss.ruby.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
        "both readings survive, in the order the archive wrote them",
    );
    // Six characters, then 父 - one unit wide, wearing two half-size kana,
    // which is exactly one unit, so とう covers its base flush.
    assert_eq!(6.0 * unit, gloss.ruby[0].x);
    // Then さんは, and the empty base begins ten units in.
    assert_eq!(10.0 * unit, gloss.ruby[1].x, "over its own hole, not over 父");
}

// ---- a table whose children are not rows ----

/// 旺文社漢字典 第四版's radical index: sweep row 94 (灬), its first two
/// stroke-count groups verbatim, with the one declaration of that
/// archive's 25 303-byte `styles.css` this shape resolves.
///
/// The archive writes the index as a `table` whose children are
/// `span[data-sc-IndexSubG]`, one span per stroke-count group, and no
/// `tr` and no `td` anywhere. Its stylesheet maps those spans onto the
/// table model with `display: table-row`, which CSS 2.1 section 17.2 is
/// explicitly for. chibipop resolves no `display` - the property is
/// deliberately unmapped, because `display: grid` is the corpus's
/// commonest declaration - so the groups reach the table walk as content
/// written outside any cell, and what they get there is the
/// anonymous-box repair CSS 2.1 section 17.2.1 writes for exactly this.
fn radical_index() -> Presentation {
    card_with(vec![css_tree(
        "旺文社漢字典 第四版",
        &sc(concat!(
            r#"{"content":["#,
            r#"{"content":["#,
            r#"{"content":[{"content":[{"content":[{"content":"⓪","tag":"span"}"#,
            r#"],"tag":"span","data":{"span":""}}"#,
            r#"],"tag":"span","data":{"IndexSubNum":""}}"#,
            r#"],"tag":"span","data":{"IndexSubNumC":""}},"#,
            r#"{"content":[{"content":[{"content":[{"content":[{"content":"火","tag":"span"}"#,
            r#"],"tag":"span","data":{"Red":""}}"#,
            r#"],"tag":"span","data":{"IndexChar":"","href":"04088"}}"#,
            r#"],"tag":"span","data":{"indexlist":"","部首内":"","class":"部首内"}}"#,
            r#"],"tag":"span","data":{"IndexSubC":""}}"#,
            r#"],"tag":"span","data":{"IndexSubG":""}},"#,
            r#"{"content":["#,
            r#"{"content":[{"content":[{"content":[{"content":"②","tag":"span"}"#,
            r#"],"tag":"span","data":{"span":""}}"#,
            r#"],"tag":"span","data":{"IndexSubNum":""}}"#,
            r#"],"tag":"span","data":{"IndexSubNumC":""}},"#,
            r#"{"content":[{"content":[{"content":[{"content":[{"content":"灰","tag":"span"}"#,
            r#"],"tag":"span","data":{"Red":""}}"#,
            r#"],"tag":"span","data":{"IndexChar":"","href":"04089"}}"#,
            r#"],"tag":"span","data":{"indexlist":"","部首内":"","class":"部首内"}},"#,
            r#"{"content":[{"content":[{"content":[{"content":"灯","tag":"span"}"#,
            r#"],"tag":"span","data":{"Red":""}}"#,
            r#"],"tag":"span","data":{"IndexChar":"","href":"04090"}}"#,
            r#"],"tag":"span","data":{"indexlist":"","部首内":"","class":"部首内"}}"#,
            r#"],"tag":"span","data":{"IndexSubC":""}}"#,
            r#"],"tag":"span","data":{"IndexSubG":""}}"#,
            r#"],"data":{"table":""},"tag":"table"}"#,
        )),
        "[data-sc-indexlist][data-sc部首内] { font-size: 1.4em; }",
    )])
}

/// The acceptance: a run of children that are no table children is
/// **one** anonymous cell in **one** anonymous row, so the index reads in
/// the order the dictionary wrote it. One cell per child made the two
/// groups two columns sharing one row - 19 of them in the whole index,
/// each about 6 px wide, with the reading order turned 90 degrees, the
/// right-hand strips off the edge of the panel and neighbouring strips
/// printing glyphs over each other.
///
/// The arithmetic, so a reader can redo it: the stylesheet's
/// `font-size: 1.4em` makes a kanji 21 at [`BOX_EM`], so it advances 10.5
/// and a stroke-count number advances 7.5. Two numbers and three kanji
/// are 46.5 wide, on one line as tall as the 1.4em that set it.
#[test]
fn a_table_whose_children_are_not_rows_becomes_one_cell_and_not_one_column_each() {
    let s = laid_out(&radical_index(), 424.0, 4000.0, false, false);
    assert_eq!(1, grid_cells(&s).len(), "one anonymous cell, not one per group");

    let index = s
        .elems
        .iter()
        .find(|e| e.kind == ElemKind::Text && e.text == "⓪火②灰灯")
        .unwrap_or_else(|| panic!("the index reads in document order: {:?}", texts(&s)));
    assert_eq!(2.0 * BOX_EM * ADVANCE + 3.0 * 1.4 * BOX_EM * ADVANCE, index.rect.w);
    assert_eq!(1.4 * BOX_EM * LINE_H, index.rect.h, "one line, as tall as its kanji");

    // And the grid is that one column and nothing else: a table with no
    // declared width is shrink-to-fit, so 19 groups can no longer ask the
    // panel for 19 tracks it has to scale to fit.
    let table = find(&s, ElemKind::Table);
    assert_eq!((index.rect.x, index.rect.y, index.rect.w), (table.rect.x, table.rect.y, table.rect.w));
}

/// An anonymous cell is no `td`, so it draws none of Yomitan's cell
/// defaults: those hang on `.gloss-sc-th, .gloss-sc-td` and a `span`
/// matches neither class, and CSS 2.1 section 17.2.1 gives an anonymous
/// box the initial value of every property it does not inherit.
///
/// Charging them drew 19 boxes a browser leaves undrawn and took 0.25em
/// of padding per side out of columns narrower than that, which is what
/// drove each group's wrap width onto its own 1 px floor.
#[test]
fn an_anonymous_cell_draws_no_border_and_pays_no_padding() {
    let s = laid_out(&radical_index(), 424.0, 4000.0, false, false);
    let cell = grid_cells(&s)[0];
    assert_eq!(BoxStyle::default(), block_box(cell).style);
    // Which is one number twice: the content starts on the cell's own
    // top-left corner, with no rule and no padding between.
    assert_eq!((cell.rect.x, cell.rect.y), cell.pen);
}

/// The run is *consecutive* siblings and not every stray in the row.
/// CSS 2.1 section 17.2.1 rule 2.3 wraps a non-cell child "and all
/// consecutive siblings of C that are not 'table-cell' boxes", so a
/// written cell closes the anonymous one before it and the content after
/// it opens another - three columns, in the order the row wrote them.
#[test]
fn a_written_cell_closes_the_anonymous_cell_the_content_before_it_opened() {
    let row = r#"{"tag":"tr","content":["a","b",{"tag":"td","content":"c"},"d"]}"#;
    let s = gridded(&table(&[row.to_string()]), 424.0);

    assert_eq!(3, grid_cells(&s).len());
    let cells: Vec<&str> = grid_text(&s).iter().map(|e| e.text.as_str()).collect();
    assert_eq!(vec!["ab", "c", "d"], cells, "the two strays before the `td` share one cell");
}

// ---- a picture wider than its column ----

/// 現代国語例解辞典　第五版's コラム panel: sweep row 542 (上がる), the first
/// `tr` of its table verbatim, inside the two ancestor boxes the shape
/// signature names. The archive holds no `styles.css`, so every
/// declaration here is the entry's own.
///
/// Neither `img` declares an `alt` and the fragment carries no PNG, so
/// `image_size` sizes each box from the node's own `width` and `height`:
/// `7.95em` and `12.72em` across, `8em` down, at [`BOX_EM`].
fn column_panel() -> Presentation {
    let cell = |body: &str| {
        format!(
            concat!(
                r#"{{"content":{body},"style":{{"backgroundColor":"var(--background-color)","#,
                r#""borderStyle":"solid","borderWidth":"2px","textAlign":"center","#,
                r#""verticalAlign":"middle"}},"tag":"td"}}"#
            ),
            body = body
        )
    };
    let picture = |name: &str, w: f32| {
        format!(
            concat!(
                r#"{{"content":{{"appearance":"auto","background":true,"collapsed":false,"#,
                r#""collapsible":false,"height":8.0,"path":"genkokr5/GenKokR5-res-1/{name}","#,
                r#""sizeUnits":"em","tag":"img","width":{w}}},"#,
                r#""style":{{"margin":"0.5em"}},"tag":"div"}}"#
            ),
            name = name,
            w = w
        )
    };
    let title = concat!(
        r#"{"content":{"content":"給料が上がるとテンションが上がる？","#,
        r#""style":{"fontWeight":"bold"},"tag":"span"},"#,
        r#""style":{"fontSize":"100%","fontWeight":"bold","padding":"0.5em","#,
        r#""textAlign":"left"},"tag":"div"}"#
    );
    let row = format!(
        r#"{{"content":[{},{},{}],"tag":"tr"}}"#,
        cell(title),
        cell(&picture("コラムあ_7.png", 7.95)),
        cell(&picture("コラムあ_8.png", 12.72)),
    );
    card_with(vec![tree(
        "現代国語例解辞典　第五版",
        &sc(&format!(
            concat!(
                r#"{{"content":[{{"content":{{"content":{{"content":[{row}],"tag":"table"}},"#,
                r#""style":{{"padding":"0.5em"}},"tag":"span"}},"#,
                r#""style":{{"marginRight":"2em"}},"tag":"div"}}],"tag":"div"}}"#
            ),
            row = row
        )),
    )])
}

/// Every illustration with the block box that bought it its line: the
/// nearest `Block` element before it in draw order, which is the
/// `div{margin: 0.5em}` this archive wraps every picture in. A block's
/// box leads its own content, so nearest and not first - boxes nest.
fn pictures_in_blocks(s: &PopupScene) -> Vec<(&SceneElem, &SceneElem)> {
    s.elems
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == ElemKind::Image)
        .map(|(at, picture)| {
            let block = s.elems[..at]
                .iter()
                .rev()
                .find(|e| e.kind == ElemKind::Block)
                .unwrap_or_else(|| panic!("nothing boxes the picture at {:?}", picture.rect));
            (picture, block)
        })
        .collect()
}

/// Each コラム picture's fitted box, as `(w, h)`. The entry declares
/// `7.95em` and `12.72em` across by `8em` down, which is 119.25 and 190.80
/// by 120.00 at [`BOX_EM`]; the columns leave their blocks 78.94045 and
/// 130.14372, and one factor per picture takes both axes down to these.
const FITTED: [(f32, f32); 2] = [(78.94045, 79.436935), (130.14372, 81.8514)];

/// The acceptance: a declared width is a demand, and the answer is the
/// room the picture's own block was given. `Pass::columns` narrows a
/// column and its text rewraps; a picture has nothing to rewrap, so it
/// used to be drawn at its full declared width inside a column far
/// narrower - over the cell to its right, and off the panel from the last
/// column. Yomitan asks for the same cap twice (`max-width: 100%` on
/// `.gloss-image-link` and on `.gloss-image-container`) before it clips
/// the remainder away.
///
/// The two numbers the sweep measured on this entry, both zero here: the
/// pictures overlapped by `15.67` px, and the second stood `5.26` px
/// outside the 424 px panel - `238.46 + 190.80 - 424.00`.
#[test]
fn a_picture_wider_than_its_column_is_fitted_to_it_instead_of_drawn_over_the_next_cell() {
    let s = laid_out(&column_panel(), 424.0, 4000.0, false, false);
    let pictures = pictures_in_blocks(&s);
    assert_eq!(2, pictures.len(), "one illustration per picture cell");

    // One factor scales both axes, because this build has no clip and both
    // painters stretch an asset into the rect they are given: a width taken
    // alone would squash a scanned illustration instead of cropping it.
    let boxes: Vec<(f32, f32)> = pictures.iter().map(|(p, _)| (p.rect.w, p.rect.h)).collect();
    assert_eq!(FITTED.to_vec(), boxes);
    // And the width is the block's own room, on both edges: a picture that
    // does not leave the block that bought it can reach neither the cell
    // beside it nor the edge of the panel.
    for (picture, block) in &pictures {
        assert_eq!((block.rect.x, block.rect.w), (picture.rect.x, picture.rect.w));
    }

    let (first, second) = (pictures[0].0.rect, pictures[1].0.rect);
    assert_eq!(0.0, (first.x + first.w - second.x).max(0.0), "no picture over its neighbour");
    assert_eq!(0.0, (second.x + second.w - 424.0).max(0.0), "and none outside the panel");
}

/// A picture is fitted in the reservation as well as in the paint, because
/// [`measure_images`] and [`place_images`] fit through one function. A fix
/// that narrowed only the drawn rect would leave every コラム row holding
/// the line the full-size picture asked for: `FakeMeasure` gives a line
/// twice its tallest span and an image's riser is its own height, so the
/// paragraph around an `8em` picture measured 240 px whatever the column
/// had already done to it.
#[test]
fn a_picture_its_column_shrank_reserves_the_line_it_actually_needs() {
    let s = laid_out(&column_panel(), 424.0, 4000.0, false, false);
    let pictures = pictures_in_blocks(&s);

    let lines: Vec<f32> = pictures.iter().map(|(_, block)| block.rect.h).collect();
    assert_eq!(vec![LINE_H * FITTED[0].1, LINE_H * FITTED[1].1], lines, "240.00 each before");
    for (picture, block) in &pictures {
        assert_eq!(LINE_H * picture.rect.h, block.rect.h, "the line came down with the picture");
    }
}

// ---- two readings over one base ----

/// 岩波国語辞典　第八版 writes cross-references that carry both readings of a
/// headword: `<ruby>七色<rt>なないろ</rt><rt>しちしょく</rt></ruby>`. The HTML
/// ruby model reads a sequence of `rt` after a base as one independent
/// annotation level each, and both engines Yomitan runs in *draw the text* -
/// Gecko lays the tabular form out, Blink and WebKit do not, but an `rt` is
/// rendered content in either. Drawing neither the second annotation nor its
/// text diverges from both at once: the dictionary states that the word has
/// two readings and the panel states that it has one.
///
/// The second reading is drawn as a second band, stacked over the first and
/// centred on the same base. Both rects are pinned against that one base and
/// the line's own height is pinned with them: a band the line did not grow
/// for would draw over the paragraph above it, and a bin re-measuring the
/// same spans would get the ungrown line back.
#[test]
fn a_base_with_two_readings_stacks_the_second_band_over_the_first() {
    let unit = Theme::dark().body_size * ADVANCE;
    let read_unit = unit * RUBY_RATIO;
    let base_line = Theme::dark().body_size * LINE_H;
    let read_line = Theme::dark().body_size * RUBY_RATIO * LINE_H;
    // The fake hangs every line off an ascent of its tallest span's own
    // size, so its ascent share is `1 / LINE_H`.
    let ascent = 1.0 / LINE_H;

    let s = laid_out(
        &rich(&sc(concat!(
            r#"{"tag":"div","content":["\u8679\u306e","#,
            r#"{"tag":"ruby","content":["\u4e03\u8272","#,
            r#"{"tag":"rt","content":"\u306a\u306a\u3044\u308d"},"#,
            r#"{"tag":"rt","content":"\u3057\u3061\u3057\u3087\u304f"}]},"#,
            r#""\u3002"]}"#,
        ))),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];

    assert_eq!(
        vec!["\u{306a}\u{306a}\u{3044}\u{308d}", "\u{3057}\u{3061}\u{3057}\u{3087}\u{304f}"],
        gloss.ruby.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
        "both readings the dictionary wrote reach the panel",
    );
    // One invisible character per reading, which is what [`RUBY_FILLER`]
    // already promises: two bands, two fillers, one base.
    assert_eq!("\u{8679}\u{306e}\u{4e03}\u{8272}\u{2060}\u{2060}\u{3002}", gloss.text);
    assert_eq!(1, gloss.lines, "and neither band broke a line");
    // The line bought room for both bands at once, which is what keeps a
    // bin's own re-measure of these spans agreeing with the scene.
    assert_eq!(base_line + 2.0 * read_line / ascent, gloss.rect.h);

    // 虹の, then the two-unit base 七色.
    let (base_x, base_w) = (2.0 * unit, 2.0 * unit);
    let (near, far) = (&gloss.ruby[0], &gloss.ruby[1]);

    assert_eq!(4.0 * read_unit, near.w, "four half-size kana");
    assert_eq!(5.0 * read_unit, far.w, "five");
    assert_eq!(read_line, near.h);
    assert_eq!(read_line, far.h);

    // Both centred over the one base: なないろ is exactly a base wide, so it
    // covers 七色 flush; しちしょく is half a reading unit wider on each side.
    assert_eq!(base_x + (base_w - near.w) / 2.0, near.x);
    assert_eq!(base_x + (base_w - far.w) / 2.0, far.x);

    // The near band's bottom is the base's own ink top; the far band stands
    // directly on the near band, and the pair fills the room the line grew.
    let base_ink = ascent * gloss.rect.h - ascent * base_line;
    assert_eq!(base_ink, near.y + near.h, "なないろ rests on 七色");
    assert_eq!(near.y, far.y + far.h, "しちしょく rests on なないろ");
    assert_eq!(0.0, far.y, "and the pair reaches the top of the room it bought");
}

// ---- a reading at the end of a line ----

/// 岩波国語辞典　第八版 row 31513, verbatim: `宿酔` under
/// `しゅくすい・ふつかよい`, with the filler that puts `宿` at the end of the
/// first line. Eleven kana stand over two kanji, the base splits at the break,
/// and the whole reading is then centred over the one character that stayed
/// behind - so 2.38 px of kana stood outside the panel, where one bin clips
/// them away and the other paints them off the rounded rect. A reader reads
/// them in neither case, and half a spelled-out number reads as a different
/// number.
///
/// Yomitan declares no `ruby` or `rt` rule for glossary content, so what a
/// reader sees is the browser's own default, and the browser keeps every kana
/// inside the content box. CSS Ruby Level 1 §5.2 is the rule that answers a
/// line edge: a user agent may pull an annotation at a line edge back to that
/// edge. Chromium 151 was measured doing exactly that - with the ruby
/// mid-line the `rt` box stands 4.00 px left of the `ruby` box and 4.02 px
/// right of it, and at a line end (filler 25) the `rt` runs 371.28 to 393.78
/// against a `ruby` box of 375.02 to 393.77: hung left of its base, stopped at
/// the line's own edge.
///
/// Both boxes are pinned, and against the *content column* rather than the
/// panel, because the content column is the box a browser keeps the annotation
/// in. Shrinking the element rect alone would satisfy nothing: the kana would
/// still be drawn where they were.
#[test]
fn a_reading_at_a_line_end_is_pulled_back_to_the_content_edge() {
    let unit = Theme::dark().body_size * ADVANCE;
    let read_unit = unit * RUBY_RATIO;
    let filler = "\u{3042}".repeat(52);
    let s = laid_out(
        &rich(&sc(&format!(
            concat!(
                r#"{{"tag":"div","content":["{filler}","#,
                r#"{{"content":["\u5bbf\u9154",{{"content":"\u3057\u3085\u304f\u3059\u3044"#,
                r#"\u30fb\u3075\u3064\u304b\u3088\u3044","tag":"rt"}}],"tag":"ruby"}}]}}"#,
            ),
            filler = filler,
        ))),
        424.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];
    let read = &gloss.ruby[0];
    // The column is 424 - 2 x 12 = 400, and it ends here.
    let edge = s.origin + s.content_w;
    assert_eq!(412.0, edge, "the content column's own right edge");

    // 53 characters of 7.5 px fill the line and end it at 397.5, so the 52
    // filler kana and 宿 fill it and 酔 starts the next line.
    assert_eq!(2, gloss.lines);
    assert_eq!(11.0 * read_unit, read.w, "eleven kana at half the base's size");
    assert_eq!(41.25, read.w, "which is 41.25 px");
    assert_eq!(53.0 * unit, 397.5, "and the line before it ends at 397.5");

    // Centred over the one character that stayed behind, the reading would
    // start at 385.12 and end at 426.38 - past the panel's own 424. Pulled
    // back, it ends on the content edge.
    assert_eq!(edge, gloss.pen.0 + read.x + read.w, "no kana outside the column");
    assert!(
        gloss.pen.0 + read.x >= s.origin,
        "and none off its left edge either: {}",
        gloss.pen.0 + read.x,
    );
    // The element's own ink box grew to cover the reading, so it reports the
    // same edge rather than a width its furigana exceeds.
    assert_eq!(edge, gloss.rect.x + gloss.rect.w, "the ink box ends there too");
}

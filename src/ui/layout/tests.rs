//! These tests validate the layout pass with fixed metrics in the platform-neutral layer.
//!
//! `FakeMeasure` assigns one whole-pixel advance per UTF-16 unit. Each expectation uses
//! arithmetic that a reader can check by hand. The tests need no font or platform.
//!
//! The [`sweep`] module renders corpus entries with the same `FakeMeasure` and fixture
//! builders. It reads the corpus directory from the environment. CI never runs it.

use super::*;
// Each submodule provides code that these tests call. These tests cover private
// helpers shared by those submodules, not their public interfaces. The names match
// the earlier single-file layout.
use super::{chrome::*, flow::*, gloss::*, image::*, marker::*, pass::*, pill::*, ruby::*, style::*};
use crate::dict::gloss::{render_html, RoleFilter, Selection, Tag};
use crate::dict::media::{Intrinsic, MediaFormat, MediaKey};
use crate::present::{Card, CollapsedRow, GlossBlock, GlossEntry};

mod sweep;

/// This constant sets the advance per UTF-16 unit as a fraction of the font size.
const ADVANCE: f32 = 0.5;
/// This constant sets the line height as a multiple of the font size.
const LINE_H: f32 = 2.0;
/// `FakeMeasure` models a text engine with no fonts.
///
/// The engine creates one rectangle per UTF-16 unit and wraps from left to right.
/// It records every span that layout requests. Tests can assert each request and width.
#[derive(Default)]
struct FakeMeasure {
    /// Every span in request order.
    asked: Vec<Asked>,
}

/// One span that layout gives to a measurer.
///
/// This record stores a `StyledSpan` without its font or color. It also stores the run
/// width. Tests can assert the text, width, weight, and style.
#[derive(Debug, Clone, PartialEq)]
struct Asked {
    text: String,
    size: f32,
    weight: u16,
    italic: bool,
    max_w: f32,
}

/// A `Frag` stores part of one span on one line.
struct Frag {
    span: usize,
    line: usize,
    /// Pen position at its start in its line.
    x: f32,
    /// UTF-16 units before it in the whole run.
    from: usize,
    /// Number of units in it.
    units: usize,
    /// One unit's width.
    advance: f32,
    /// The span's own line advance.
    h: f32,
}

/// Characters that a real shaper gives a glyph with zero advance.
///
/// The author checked cosmic-text. The test does not rely on an assumption. A
/// `\u{2060}` between two kanji shapes has `w 0`. The shaper still sets the line
/// height from its own size. The ruby filler depends on this behavior.
///
/// If the fake charged this character one full unit, it would model different
/// behavior from the real shaper.
fn zero_advance(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| matches!(c, '\u{2060}' | '\u{200b}'))
}

/// Characters that a real shaper does not break beside: class GL of UAX #14.
///
/// This function names one such character. An inline box reserves horizontal space
/// with U+00A0 NO-BREAK SPACE ([`PILL_SPACER`]). An image uses [`IMAGE_SPACER`].
/// Both depend on this forbidden break.
///
/// If a wrap split a pill from its padding, the box and its space would differ. If a
/// wrap left a `margin-right` gap on one line and the next word on the next, the
/// result would differ. If the fake broke here or charged [`zero_advance`] one full
/// unit, it would model different behavior.
fn glue(c: char) -> bool {
    c == '\u{a0}'
}

/// A `Unit` stores one UTF-16 unit in the fake wrap.
struct Unit {
    span: usize,
    advance: f32,
    h: f32,
    /// Can a line break occur immediately before this unit?
    breakable: bool,
}

/// The `wrap` function implements the greedy fake wrap.
///
/// The wrap places one rectangle per UTF-16 unit from left to right. It breaks when
/// the next rectangle does not fit. Each expectation uses arithmetic that a reader
/// can check by hand. A one-span run without [`glue`] gives
/// `floor(max_w / advance)` units per line. This fixed rule defines the fake.
///
/// The break position follows UAX #14, not an arbitrary position. Two parts of this
/// renderer depend on [`glue`]. Two rules suffice: no break after glue (LB12), and no
/// break before glue unless a space comes first (LB12a). Every other position allows a
/// break, so a plain run wraps per unit.
fn wrap(run: MeasureRun<'_>) -> (Vec<Frag>, Measured) {
    let max_w = run.max_w.max(1.0);
    let mut units: Vec<Unit> = Vec::new();
    let mut prev: Option<char> = None;
    for (span, s) in run.spans.iter().enumerate() {
        let advance = if zero_advance(s.text) { 0.0 } else { s.size * ADVANCE };
        let h = s.size * LINE_H;
        for c in s.text.chars() {
            // Apply LB12 and LB12a across the complete run. A span boundary does not
            // create a line boundary. The character before this one can belong to
            // the previous span.
            let after_glue = prev.is_some_and(glue);
            let before_glue = glue(c) && !prev.is_some_and(|p| p == ' ' || p == '\t');
            let breakable = !after_glue && !before_glue;
            for i in 0..c.len_utf16() {
                // A surrogate pair has two units for one character. The wrap cannot
                // break between the pair.
                units.push(Unit { span, advance, h, breakable: breakable && i == 0 });
            }
            prev = Some(c);
        }
    }

    let mut frags: Vec<Frag> = Vec::new();
    let (mut line, mut x, mut from) = (0usize, 0.0f32, 0usize);
    let mut at = 0usize;
    while at < units.len() {
        // Group each break opportunity with the next one into one chunk. Plain
        // text gives one unit per chunk, so it still wraps per unit. A glue run gives
        // one chunk that contains the full reservation and its fused units.
        let mut end = at + 1;
        while end < units.len() && !units[end].breakable {
            end += 1;
        }
        let chunk = &units[at..end];
        let w: f32 = chunk.iter().map(|u| u.advance).sum();
        // Do not break at the start of a line. If a chunk exceeds `max_w`, keep it on
        // one line without a loop. A zero-advance chunk always fits.
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
    // An empty run has one empty line. The walk adds the gap after it in either
    // case.
    if out.lines.is_empty() {
        let size = run.spans.first().map_or(0.0, |s| s.size);
        out.lines.push(LineBox { h: size * LINE_H, ..LineBox::default() });
    }
    let mut y = 0.0;
    for l in &mut out.lines {
        l.y = y;
        // Every span on a line shares one baseline. The baseline uses an ascent
        // equal to the largest span size above the line floor.
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
        // An offset past the end returns the pen position after the last unit.
        // Core never asks for this value. Each offset that core probes starts a
        // kanji that core walked.
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

    fn hit_offset(
        &mut self,
        run: MeasureRun<'_>,
        x: f32,
        y: f32,
    ) -> Result<u32, MeasureError> {
        let (frags, measured) = wrap(run);
        let total = frags.last().map_or(0, |f| f.from + f.units);
        let Some(first) = measured.lines.first() else {
            return Ok(0);
        };
        if y < first.y {
            return Ok(0);
        }
        let Some(last) = measured.lines.last() else {
            return Ok(0);
        };
        if y >= last.y + last.h {
            return Ok(total as u32);
        }
        let line = measured
            .lines
            .iter()
            .position(|line| y >= line.y && y < line.y + line.h)
            .unwrap_or_else(|| measured.lines.len().saturating_sub(1));
        let mut nearest: Option<(f32, usize)> = None;
        let mut line_end = 0usize;
        for frag in frags.iter().filter(|frag| frag.line == line) {
            line_end = line_end.max(frag.from + frag.units);
            for unit in 0..frag.units {
                let centre = frag.x + (unit as f32 + 0.5) * frag.advance;
                let distance = (x - centre).abs();
                if nearest.is_none_or(|(best, _)| distance < best) {
                    nearest = Some((distance, frag.from + unit));
                }
            }
        }
        // The line-end caret handles a point beyond the last glyph. A strict
        // comparison maps a glyph center to that glyph's offset.
        let line_width = measured.lines[line].w;
        let distance = (x - line_width).abs();
        if nearest.is_none_or(|(best, _)| distance < best) {
            nearest = Some((distance, line_end));
        }
        Ok(nearest.map_or(line_end, |(_, offset)| offset.min(total)) as u32)
    }
}

/// A measurer that refuses every run and caret probe.
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
    fn hit_offset(
        &mut self,
        _: MeasureRun<'_>,
        _: f32,
        _: f32,
    ) -> Result<u32, MeasureError> {
        Err(MeasureError::new("no font"))
    }

}

/// The `styled` helper returns one span with only its size set.
///
/// This helper sets no family or color because `FakeMeasure` ignores both.
fn styled(text: &str, size: f32) -> StyledSpan<'_> {
    StyledSpan { text, font: "", size, weight: 400, italic: false, color: (0, 0, 0) }
}

/// The `fake_measure` helper measures `spans` through the seam at `max_w`.
fn fake_measure(spans: &[StyledSpan<'_>], max_w: f32) -> Measured {
    let mut out = Measured::default();
    FakeMeasure::default()
        .measure(MeasureRun { spans, max_w }, &mut out)
        .expect("FakeMeasure never refuses a run");
    out
}

// ---- fixtures ----

/// The layout pass renders the parsed tree for each row, so each fixture carries
/// the tree that its strings parse to. A bare glossary string becomes one
/// plain-string item. Twenty of the 72 dictionaries in the census emit that item.
/// Each geometry expectation uses arithmetic over that item. Use `tree` when a
/// fixture needs structure.
///
/// This builder makes one row per block, which matches a one-hit dictionary. Use
/// `rows` for the grouped case.
fn block(dict: &str, glosses: &[&str]) -> GlossBlock {
    rows(dict, &[glosses])
}

/// Build one dictionary block with several matched term-bank rows.
fn rows(dict: &str, per_row: &[&[&str]]) -> GlossBlock {
    GlossBlock {
        dict_name: dict.to_string(),
        dict_id: crate::present::NO_ROW,
        entries: per_row.iter().map(|glosses| entry(glosses, &[])).collect(),
    }
}

/// Build one matched row with its tags.
fn entry(glosses: &[&str], tags: &[&str]) -> GlossEntry {
    row_of(&serde_json::json!(glosses).to_string(), tags)
}

/// Build one dictionary block from one row's raw structured content.
fn tree(dict: &str, glossary: &str) -> GlossBlock {
    GlossBlock {
        dict_name: dict.to_string(),
        dict_id: crate::present::NO_ROW,
        entries: vec![row_of(glossary, &[])],
    }
}

/// Build one matched row from raw glossary JSON in the record.
fn row_of(glossary: &str, tags: &[&str]) -> GlossEntry {
    row_media(glossary, tags, Vec::new())
}

/// One matched row whose dictionary supplies `media`.
///
/// The measurement pass reads the dimensions recorded by the build, not the bytes
/// (`present::GlossEntry::media`). An image fixture therefore needs a path and
/// four numbers. It needs no archive, decoder, or database.
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
        pitch: Vec::new(),
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
        pitch: Vec::new(),
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

/// Build a `PopupScene` with the supplied maximum width and height.
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
            selection: None,
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

/// Return each scene element's text in draw order. The order matches the painter's
/// top-to-bottom order.
fn texts(s: &PopupScene) -> Vec<&str> {
    s.elems.iter().map(|e| e.text.as_str()).collect()
}

/// Build a card that contains exactly the blocks that the caller supplies. Tests can
/// assert the grouped shape that `present::build` produces.
fn card_with(blocks: Vec<GlossBlock>) -> Presentation {
    let card = Card {
        written: Some("雑談".into()),
        reading: None,
        pos: vec![],
        freq: None,
        blocks,
        match_len: 2,
        pitch: Vec::new(),
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    }
}

/// Build a scene under `theme` and return every run that the layout pass measures.
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
            selection: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run");
    (s, m.asked)
}

/// `roled_theme` returns a theme with distinct metrics for every role.
///
/// Each role has a distinct size, weight, or style, so tests can identify a run from
/// its metrics. Only `body` keeps the default 15.0. This proves that `reading` does
/// not use the default.
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

/// Get the measured run for `text`.
fn asked_for<'a>(runs: &'a [Asked], text: &str) -> &'a Asked {
    runs.iter()
        .find(|a| a.text == text)
        .unwrap_or_else(|| panic!("{text:?} was never measured"))
}

// ---- line wrap ----
/// The scene reports the width that the layout pass offers.
#[test]
fn a_run_wraps_at_the_width_layout_offered_it() {
    // The panel has 12px of padding on each side. The 200px column fits 26 units
    // at 7.5px each, so the eight-unit `chatting` run stays on one line.
    let s = laid_out(&one_card(&[], None), 224.0, 4000.0, false, false);
    assert_eq!(200.0, s.content_w);
    let gloss = s.elems.iter().find(|e| e.text == "chatting").unwrap();
    // The eight-unit `chatting` run measures 8 × 15.0 × 0.5 = 60px.
    // The result fits inside the 200px column, so it stays on one line.
    assert_eq!(200.0, gloss.wrap_w);
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
            pitch: Vec::new(),
        }),
        collapsed: vec![],
        all_cards: vec![],
        sentence: None,
    };
    // The 100px column fits 13 units at 7.5px each. The 120-unit run needs 10 lines.
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = s.elems.iter().find(|e| e.text == long).unwrap();
    assert_eq!(100.0, gloss.wrap_w);
    assert_eq!(10, gloss.lines);
    assert_eq!(10.0 * 15.0 * LINE_H, gloss.rect.h);
    assert_eq!(gloss.rect.h, gloss.advance, "a text run advances by its height");
}

/// An exact fit must stay within the column.
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
            pitch: Vec::new(),
        }),
        collapsed: vec![],
        all_cards: vec![],
        sentence: None,
    };
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = s.elems.iter().find(|e| e.text == exact).unwrap();
    assert_eq!(1, gloss.lines);
}
/// The frequency corner reduces the next run's width once.
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

// ---- gap stack ----

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

/// The box model limits the content height to the supplied view height.
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

/// The inline separator has rule geometry and no text.
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
/// A related entry has a different term, and its kana also identifies that entry.
#[test]
fn an_inline_related_row_names_its_reading_beside_its_written_form() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, false);
    let rows: Vec<&str> =
        s.elems.iter().filter(|e| e.kind == ElemKind::Collapsed).map(|e| e.text.as_str()).collect();
    assert_eq!(
        vec!["雑音\u{3010}ざつおん\u{3011} \u{2014} noise", "雑誌\u{3010}ざっし\u{3011} \u{2014} magazine"],
        rows
    );
}

#[test]
fn a_kana_only_related_row_prints_its_reading_once() {
    let mut p = with_collapsed();
    p.collapsed = vec![CollapsedRow {
        written: Some("ざつおん".into()),
        reading: Some("ざつおん".into()),
        summary: "noise".into(),
    }];
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert_eq!("ざつおん \u{2014} noise", find(&s, ElemKind::Collapsed).text);
}

/// The side column has no room for the reading, so it keeps only the headword.
#[test]
fn a_side_related_row_stays_headword_only() {
    let s = laid_out(&with_collapsed(), 424.0, 4000.0, false, true);
    let side = s.side.as_ref().unwrap();
    assert_eq!("雑音", side.rows[1].text);
    assert_eq!("雑誌", side.rows[2].text);
}

// ---- scroll cull ----

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
    // Scroll past the headword's box and its em of slack.
    let past = head.pen.1 + head.rect.h + head.font_size + 1.0;
    let kept: Vec<&str> = s.visible(past, 4000.0).map(|p| p.elem.text.as_str()).collect();
    assert!(kept.len() < all, "scrolling past the headword must cull it");
    assert!(!kept.contains(&"雑談"));
}
/// Ink can extend beyond the measured box and still remain visible.
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
/// The taller column sets the body height.
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
/// The headword has one hit box for each kanji.
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
            pitch: Vec::new(),
        }),
        collapsed: vec![],
        all_cards: vec![],
        sentence: None,
    };
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    assert!(!s.hits.iter().any(|h| matches!(h.action, HitAction::DrillDown(_))));
}

/// The hit-target order follows the paint order.
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
            selection: None,
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
            selection: None,
        },
        &mut m,
    )
    .unwrap();
    assert_eq!(None, s.anki);
}

/// The slot spans the panel after the side column widens it.
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
            selection: None,
        },
        &mut m,
    )
    .unwrap();
    assert_eq!(s.panel_w, Some(s.anki.as_ref().unwrap().rect.w));
}

// ---- the measurement seam ----

/// The pass sends one run to `TextMeasure` for each element. It performs no second
/// measurement.
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
            selection: None,
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


/// The hit test uses the same fixed geometry as caret boxes.
#[test]
fn hit_offset_round_trips_fake_caret_centres_and_clamps_vertical_points() {
    let spans = [styled("ab", 10.0), styled("cd", 10.0)];
    let run = MeasureRun { spans: &spans, max_w: 10.0 };
    let mut m = FakeMeasure::default();
    let offsets: Vec<u32> = (0..=4).collect();
    let mut boxes = Vec::new();
    m.caret_boxes(run, &offsets, &mut boxes).unwrap();

    for (offset, glyph) in offsets.iter().zip(boxes) {
        let x = glyph.x + glyph.w / 2.0;
        let y = glyph.y + glyph.h / 2.0;
        assert_eq!(*offset, m.hit_offset(run, x, y).unwrap());
    }
    assert_eq!(0, m.hit_offset(run, 0.0, -1.0).unwrap());
    assert_eq!(4, m.hit_offset(run, 0.0, 100.0).unwrap());
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
            selection: None,
        },
        &mut BrokenMeasure,
    )
    .expect_err("a refusing engine cannot produce a scene");
    assert_eq!("measuring text failed: no font", err.to_string());
}

/// One span enters the pass, and the walk produces one line box. The line details
/// report the same result.
#[test]
fn one_span_measures_to_one_line_box_that_fills_the_run() {
    let spans = [styled("abcd", 10.0)];
    let m = fake_measure(&spans, 100.0);

    assert_eq!(Metrics { w: 20.0, h: 20.0, lines: 1 }, m.metrics);
    assert_eq!(vec![LineBox { y: 0.0, w: 20.0, h: 20.0, baseline: 10.0 }], m.lines);
    assert_eq!(vec![SpanBox { span: 0, line: 0, x: 0.0, w: 20.0, h: 20.0 }], m.spans);
}

/// This test defines the inline pass contract.
///
/// Spans that fit share one line and sit end to end. All spans use one baseline,
/// even when their heights differ.
#[test]
fn spans_that_fit_share_one_line_and_one_baseline() {
    // Four units at 10px plus four at 20px give 4×5 + 4×10 = 60.
    // The result fits inside 100px.
    let spans = [styled("abcd", 10.0), styled("wxyz", 20.0)];
    let m = fake_measure(&spans, 100.0);

    assert_eq!(1, m.metrics.lines, "60 units of text fit a 100px line");
    assert_eq!(2, m.spans.len(), "one box per span");
    let (small, big) = (m.spans[0], m.spans[1]);
    assert_eq!((0, 0, 0.0, 20.0), (small.span, small.line, small.x, small.w));
    assert_eq!((1, 0, 20.0, 40.0), (big.span, big.line, big.x, big.w));
    assert_eq!(m.lines[0].w, big.x + big.w, "the spans sum to the line's width");

    // Each span supplies its own advance. The line uses the largest advance and
    // one baseline for both spans.
    assert_eq!((20.0, 40.0), (small.h, big.h));
    assert_eq!(40.0, m.lines[0].h, "the taller span sets the line");
    assert_eq!(20.0, m.lines[0].baseline);
    assert_eq!(Metrics { w: 60.0, h: 40.0, lines: 1 }, m.metrics);
}

/// A span boundary is not a line boundary. The second span uses the room that remains
/// and wraps inside itself. The inline pass depends on this behavior.
#[test]
fn a_span_wraps_within_itself_rather_than_at_its_boundary() {
    // Each unit is 5px, and eight units fit in a 40px line.
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

// ---- the inline format pass ----

/// Build a card with one dictionary row that carries `glossary` unchanged.
///
/// The headword has kana only, so it gets no per-character drill target. Every
/// hit in the scene comes from the gloss itself.
fn rich(glossary: &str) -> Presentation {
    let card = Card {
        written: None,
        reading: Some("\u{3055}\u{3064}\u{3060}\u{3093}".into()),
        pos: vec![],
        freq: None,
        blocks: vec![tree("Jitendex", glossary)],
        match_len: 4,
        pitch: Vec::new(),
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    }
}

/// Wrap `content` in one structured-content item.
fn sc(content: &str) -> String {
    format!(r#"[{{"type":"structured-content","content":{content}}}]"#)
}

/// Return every gloss-body element of `s` in draw order.
///
/// The dictionary label is the `Text` element before the body. The label uses
/// its role size. The body uses the body size.
fn bodies(s: &PopupScene) -> Vec<&SceneElem> {
    let body = Theme::dark().body_size;
    s.elems
        .iter()
        .filter(|e| e.kind == ElemKind::Text && e.font_size == body)
        .collect()
}

/// A plain-string gloss produces the same element as before the inline pass.
/// The element has one span with the body role and makes one seam request for
/// that exact text.
#[test]
fn a_plain_string_gloss_is_one_element_of_one_span() {
    let theme = Theme::dark();
    let p = one_card(&[], None);
    let (s, asked) = measured(&theme, &p, false);
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

    // The item opens no block. Its path is the only scene-element path, so a
    // sense picker can address the plain string. `TextSource` maps bytes to a
    // leaf, but it does not supply `GlossOrigin::path`.
    let doc = &p.top.as_ref().unwrap().blocks[0].entries[0].doc;
    let path = gloss.origin.expect("a gloss element names its row").path;
    let id = path.expect("and the plain string it renders").resolve(doc).expect("which exists");
    assert!(doc.is_plain_string(id), "the item itself, not an ancestor");
}

/// Two top-level glossary items measure as one span, not three.
///
/// The separator remains unchanged because geometry goldens store that request.
/// Adjacent runs with one style form one run.
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

/// The inline pass keeps adjacent styled text in one paragraph. A bold word and a
/// normal word next to each other in the source therefore share a line.
#[test]
fn a_bold_word_and_a_normal_word_share_one_wrapped_line() {
    let p = rich(&sc(r#"[{"tag":"b","content":"bold"},"normal text"]"#));
    // A 200px column fits 26 units. The run has 15 units.
    let s = laid_out(&p, 224.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len(), "one element, not one per style");
    assert_eq!(1, gloss[0].lines, "and one line, not one per style");
    assert_eq!("boldnormal text", gloss[0].text);
    let weights: Vec<u16> = gloss[0].spans.iter().map(|s| s.weight).collect();
    assert_eq!(vec![700, Theme::dark().body_weight], weights);
    assert_eq!(15.0 * 15.0 * ADVANCE, gloss[0].rect.w);
}

/// The same paragraph rewraps as one unit.
///
/// The break lands inside the second span, so line one is full. A renderer that
/// ended a line at each style change would leave four units on line one, not thirteen.
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

/// A run with no spaces still wraps.
#[test]
fn a_cjk_run_wraps_without_a_single_space_in_it() {
    let kanji = "\u{6f22}".repeat(20);
    let p = card_with(vec![block("\u{5927}\u{8f9e}\u{6797}", &[&kanji])]);
    let s = laid_out(&p, 124.0, 4000.0, false, false);
    let gloss = bodies(&s);

    assert_eq!(1, gloss.len());
    assert_eq!(2, gloss[0].lines, "20 units at 7.5px do not fit a 100px column");
}

/// The pass separates sibling blocks and inserts a line gap.
///
/// Before the tree reached the panel, both strings became `to runto flow`.
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

/// A `sup` rises above the baseline without a taller line.
///
/// The body span sets line height. A reference mark that made the line taller
/// would move every later block down.
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

/// A `sub` drops below the baseline. `verticalAlign` makes the same choice.
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

/// The measurer returns text-relative values for the line that contains the span.
/// Only the measurer has that information.
#[test]
fn text_top_lifts_a_small_span_to_its_lines_own_text_top() {
    let theme = Theme::dark();
    let p = rich(&sc(
        r#"["big",{"tag":"span","style":{"fontSize":"0.5em","verticalAlign":"text-top"},"content":"small"}]"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);
    let small = gloss[0].spans[1];

    // The fake uses an ascent equal to the largest span size. A half-size span has
    // half that ascent. The lift equals the difference.
    assert_eq!(theme.body_size / 2.0, small.size);
    assert_eq!(theme.body_size / 2.0, small.shift);
    assert_eq!(theme.body_size * LINE_H, gloss[0].rect.h, "and the line is unmoved");
}

// ---- ruby ----

/// A reading reserves a slot above its base. The line above keeps its pixels.
///
/// Six body units in 30px give four units per line. The base starts on line two.
/// Line one therefore provides clear space. The control uses the same paragraph
/// without the `ruby` wrapper. It checks that the reading top equals the old
/// second-line start, with no overlap or gap. It checks that the reading bottom
/// equals the base ink top.
///
/// [`RUBY_FILLER`] reserves the slot. A line assigns only its ascent share of
/// growth above the baseline. This fake assigns half. A real CJK face assigns
/// about four fifths.
///
/// The line therefore grows by `reading / ascent`, not by `reading`. The
/// *measurer* reserves this slot. If growth occurs after the wrap, the bins
/// cannot reproduce the geometry with their own measurement.
#[test]
fn a_reading_reserves_its_own_slot_and_clears_the_line_above() {
    let theme = Theme::dark();
    let base_line = theme.body_size * LINE_H;
    let read_line = theme.body_size * RUBY_RATIO * LINE_H;
    // The fake gives each line an ascent equal to its tallest span size.
    // Its ascent share is `1 / LINE_H`.
    let ascent = 1.0 / LINE_H;
    let ruby_line = base_line + read_line / ascent;

    // Use a `span`, not a bare second string. Bare strings create one paragraph each.
    // The control must use one paragraph, like ruby.
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
    // The base ink top is its line baseline minus its own ascent.
    let base_ink = base_line + ascent * ruby_line - ascent * base_line;
    assert_eq!(base_ink, read.y + read.h, "and ends on its base's own ink top");
}

/// The pass centers a reading over its base. It gives the base its own span even
/// when adjacent text has the same style.
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

    // The base starts two units into the run. The one-unit reading has half-size
    // units, so it is half as wide and sits one quarter of a base inside.
    let read = &gloss.ruby[0];
    assert_eq!(2.0 * unit + (unit - unit * RUBY_RATIO) / 2.0, read.x);
    assert_eq!(unit * RUBY_RATIO, read.w);
}

/// A reading wider than its base extends past it, so the element's ink box
/// covers the full reading.
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

    // Four half-size reading units span twice one base unit. The pass would extend
    // half a base on each side. It clamps the left side to the panel, so all extra
    // width appears on the right.
    assert_eq!(4.0 * unit * RUBY_RATIO, read.w);
    assert_eq!(0.0, read.x, "clamped into the panel, not off its left edge");
    assert_eq!(read.w, gloss.rect.w, "and the ink box covers what was drawn");
}

/// A ruby run is inline. It wraps with nearby text and does not force a break.
/// This test compares it with the same paragraph without the wrapper. Both runs
/// have the same line count and wrap. The wrapper adds one slot above its base.
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
    // A line assigns only its ascent share of growth above the baseline. It grows
    // by `reading / ascent`. See `a_reading_reserves_its_own_slot_and_clears_the_line_above`.
    let ascent = 1.0 / LINE_H;
    assert_eq!(plain.rect.h + read_line / ascent, gloss.rect.h, "it only took its slot");

    let read = &gloss.ruby[0];
    assert_eq!(base_line, read.y, "and the reading followed its base to line two");
    // The base starts the line, so the half-width reading sits a quarter
    // of a base in. The pass centers it over the base, not flush with the line.
    let unit = Theme::dark().body_size * ADVANCE;
    assert_eq!((unit - unit * RUBY_RATIO) / 2.0, read.x);
}

/// Each `rt` gets one slot. Per-character furigana pairs each reading with the
/// base before it. Two kanji therefore get two readings and two slots.
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
    // Two half-size units equal one base width, so かん covers 漢 flush. じ has
    // half that width and sits one quarter of a base inside 字.
    assert_eq!(0.0, gloss.ruby[0].x);
    assert_eq!(unit, gloss.ruby[0].w);
    assert_eq!(unit + (unit - unit * RUBY_RATIO) / 2.0, gloss.ruby[1].x);
}

/// `rp` provides fallback parentheses for renderers without ruby support. This
/// renderer draws ruby and uses the parentheses only when no reading exists.
/// The renderer keeps malformed ruby readable. It draws the reading, not only
/// the base.
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

/// The pass sets the reading size from the base size with the theme-independent
/// ruby ratio. It keeps the resolved style of `rt`, so a dictionary color remains.
/// `fontSize` on `rt` is relative to the reading size, not the base size, as CSS
/// defines.
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

/// The pass writes a matched row number in the body style. It joins the number
/// with the next span when their styles match. A ruby base has a different style,
/// so the reading centers over `猫`, not `1. 猫`.
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
    // Three number units precede the base. Two half-size reading units equal one
    // base unit, so the reading sits flush over the base.
    assert_eq!(3.0 * unit, gloss.ruby[0].x);
}

/// An internal cross-reference gets one target for each line that it reaches.
/// Each target rect covers the link spans on that line.
#[test]
fn an_internal_link_drills_down_across_a_wrap_boundary() {
    // "see " precedes a 12-unit link: 16 units, with 13 on a 100px line.
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
    // On line one, the link starts after "see " and reaches the margin.
    assert_eq!(Some(s.origin + 4.0 * unit), drills[0].x);
    assert_eq!(Some(9.0 * unit), drills[0].w);
    assert_eq!(gloss[0].pen.1, drills[0].y);
    // On line two, the other link text starts at the margin.
    assert_eq!(Some(s.origin), drills[1].x);
    assert_eq!(Some(5.0 * unit), drills[1].w);
    assert_eq!(gloss[0].pen.1 + 15.0 * LINE_H, drills[1].y);
    assert_eq!(15.0 * LINE_H, drills[0].h, "as tall as the line it sits on");
}

/// A citation opens in a browser.
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

/// An unsupported scheme gets no target. The text remains, but a click does
/// nothing.
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

/// Rich content must not alter current targets.
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

    // The pass also reserves the Anki slot from the same label.
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
                selection: None,
            },
            &mut FakeMeasure::default(),
        )
        .unwrap()
        .anki
    };
    assert_eq!(slot(&plain).map(|a| a.rect.h), slot(&rich).map(|a| a.rect.h));
}

/// A dictionary `style` supplies color and weight beside the body style.
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

/// A header cell is bold beside its row's data cell. The specification defaults table
/// defines this style, and a conjugation table uses it.
///
/// The old layout put one row in one paragraph. The current layout uses a grid with
/// two cell paragraphs. The grid still preserves the header weight.
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

/// A deeply nested tree stops at the depth cap but keeps its outer levels.
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

/// A row number leads only its first paragraph and uses body style. It joins the
/// next span, so it needs no separate measurement.
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

/// A box length uses its element's font size. Each expectation multiplies
/// `Theme::body_size` by the fixture value.
const BOX_EM: f32 = 15.0;

/// The height of one body-text line from `FakeMeasure`.
const BODY_LINE: f32 = BOX_EM * LINE_H;

/// Get the one gloss body from a scene.
fn one_body(s: &PopupScene) -> &SceneElem {
    let found = bodies(s);
    assert_eq!(1, found.len(), "expected one gloss element, got {found:?}");
    found[0]
}

/// Get an element's block box. The element must have one.
fn block_box(e: &SceneElem) -> &ElemBox {
    e.block_box.as_ref().expect("this element must carry a block box")
}

/// Get every block-box element in draw order.
///
/// A block box contains every paragraph that its block emits. It has no text and
/// uses `ElemKind::Block`. It is not a field on an inner paragraph.
fn block_boxes(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Block).collect()
}

/// Get the only block-box element from a scene.
fn one_block_box(s: &PopupScene) -> &SceneElem {
    let found = block_boxes(s);
    assert_eq!(1, found.len(), "expected one block box, got {found:?}");
    found[0]
}

/// The box around the paragraph with `text`.
///
/// A box precedes its body in draw order. The nearest earlier block is the
/// correct box. Boxes can nest.
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

/// A block box advances the walk by its outer height. This height includes
/// margins, while the paragraph keeps its text height.
///
/// The block owns this advance. The panel places the next element below its margin.
#[test]
fn a_box_with_margin_and_padding_advances_the_walk_by_its_outer_height() {
    let p = rich(&sc(
        r#"{"tag":"div","style":{"margin":0.4,"padding":0.2},"content":"boxed"}"#,
    ));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = one_body(&s);
    let outer = one_block_box(&s);

    // Margin 6 and padding 3 surround each edge of one 30px line.
    assert_eq!(BODY_LINE, gloss.rect.h, "the ink box is the text, as it always was");
    assert_eq!(BODY_LINE, gloss.advance, "and the paragraph advances by its own line");
    assert_eq!(
        6.0 + 3.0 + BODY_LINE + 3.0 + 6.0,
        outer.advance,
        "the box's advance is the outer height"
    );
    // The border box contains the fill and stroke. Padding is inside, and margin is outside.
    assert_eq!(3.0 + BODY_LINE + 3.0, block_box(outer).rect.h);
    // The paragraph sits at the content edge inside the box.
    assert_eq!(6.0 + 3.0, gloss.pen.0 - s.origin);
    assert_eq!(3.0, gloss.pen.1 - block_box(outer).rect.y, "padding under the border box's top");
}

/// A block with several paragraphs has one box around all of them, as CSS
/// defines. The pass does not create one box per paragraph or only the first.
/// This test protects that behavior.
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
    // Padding 3 and a 3px rule surround each edge. The box contains three 30px
    // lines with one `LINE_GAP` between them and no gap above the first line.
    let body_h = 3.0 * BODY_LINE + 2.0 * LINE_GAP;
    assert_eq!(3.0 + 3.0 + body_h + 3.0 + 3.0, outer.advance);
    let rect = block_box(outer).rect;
    assert_eq!(
        SceneRect {
            x: s.origin,
            // Panel chrome above the box determines `y`. This test checks that the first
            // line sits one border and one padding inside the box top.
            y: inner[0].pen.1 - 6.0,
            w: s.content_w,
            h: 6.0 + body_h + 6.0,
        },
        rect,
        "one border box, around all three lines"
    );
    assert_eq!(LINE_GAP, outer.top_gap, "the panel's gap sits outside the border");
    // The box insets and narrows every paragraph in the loop. A box that spans
    // several paragraphs insets each paragraph separately.
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

/// The pass once lost a block box when its first child opened a line. A `span` with
/// `data.content` beside another `span` opens the sense-separator paragraph. The
/// block's first paragraph held the box, but the child's `open` call left it empty.
/// The `flush` function removed that paragraph and its box, so the bordered, filled
/// `div` drew nothing.
///
/// The sibling is a `span`, not a bare string, because the marker stays inside
/// its own line beside bare sentence text (`GlossDoc::prose`). The defect needs a
/// first child that opens a line. Bare text does not cause this.
///
/// Jitendex's `div[data-sc-class="extra-box"]` around `data.content` children
/// has this shape. The stylesheet fold gives it 0.4rem and 0.5rem padding. This
/// test separates a declared box that draws from one that draws nothing.
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

    // The dictionary declared this box, and the scene draws it once.
    assert_eq!(Edges::all(3.0), style.padding);
    assert_eq!(Edges::all(3.0), style.border_used());
    assert_eq!((0x7f, 0x8c, 0x99), style.border_color);
    assert_eq!(6.0, style.radius);
    assert_eq!(Some((0x1e, 0x3a, 0x5f)), style.background);
    // The marker opens its line, so the `span` and the text after it form one
    // paragraph. The `div`'s first paragraph is empty. That paragraph once lost
    // the box.
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

/// A nested block gets its own box inside the parent box. The outer box narrows
/// the width once, and the pass measures the inner box in the available width.
/// CSS calls the outer box the parent box. Neither box pays the other's
/// lead or extends past the other.
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

    // Outer padding is 6, inner padding is 3, and one 30px line sits between them.
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

/// A block can contain a table. A table is `Piece::Table`, not a `Flow`. A box
/// body stores pieces of every kind, so the paragraph frame code also frames a
/// grid.
///
/// The boxes have different widths because CSS sets them differently. A `div`
/// has `display: block` and fills its container. A table without a declared
/// width shrinks to its grid.
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

/// A block can contain only an image. `Tag::Img` is inline content, and a gaiji
/// is a character. The image reserves room on the line. It does not use a
/// separate line. The box frames that line. The image adds no advance because
/// the paragraph already reserves and stacks the room.
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

/// A boxed block closes its line. `summary` also closes a line, so a box needs
/// a clear end. Text after the `div` belongs outside its content, and a browser
/// draws that text outside the border.
///
/// A box establishes a coordinate system for its body only. The run after the
/// box restores the parent block's list indent and inherited alignment. If it
/// fails to restore that context, the context leaks.
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
    // Inside the box, the list indent combines with the box padding.
    assert_eq!(s.origin + LEVEL + 6.0, runs[0].pen.0);
    // After the box, the list indent and the item's alignment return.
    assert_eq!(s.origin + LEVEL, runs[1].pen.0, "the indent came back");
    assert_eq!(s.content_w - LEVEL, runs[1].wrap_w);
    assert_eq!(Align::Center, runs[1].align, "and so did the inherited alignment");
    assert_eq!(Align::Center, runs[0].align, "which the box's body had too");
    // The bullet belongs to the box's first line at the list content edge. The run
    // after the box does not receive the marker again.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), marker_x(runs[0]));
    assert!(runs[1].marker.is_empty(), "a marker is owed once");
}

/// Adjacent siblings keep both margins. A browser draws 6px between these
/// blocks, but this panel draws 12px.
///
/// Margin collapse requires a box tree that resolves parent-child, sibling, and
/// empty-block cases. This walk only accumulates values forward and has no box
/// tree. It therefore implements none of those cases. The difference from a
/// browser stays bounded to dictionaries that declare `marginTop` (3) and
/// `marginBottom` (12) on opposite edges.
#[test]
fn adjacent_block_siblings_do_not_collapse_their_margins() {
    let p = rich(&sc(concat!(
        r#"{"tag":"div","content":["#,
        r#"{"tag":"div","style":{"marginBottom":0.4},"content":"one"},"#,
        r#"{"tag":"div","style":{"marginTop":0.4},"content":"two"}]}"#
    )));
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let gloss = bodies(&s);
    // Each margin belongs to its own block. Each margin therefore stays on that
    // block's box and never becomes part of the paragraph.
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

/// A block's padding moves its text inward and reduces its wrap width. Without
/// this reduction, text could extend past the box.
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

/// A bordered pill sends its border width, style, color, and radius to the
/// scene. The pass draws the box around the pill run, not its paragraph.
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

    // "noun" has four units at 7.5px, so its text is 30px wide. The run reserves
    // 3px of border and padding on each side. The box starts at the pen and ends
    // before the next text.
    // The earlier box pass used a 42px outset, 6px left of the pen. That outset
    // overlapped glyphs in adjacent runs.
    assert_eq!(gloss.pen.0, pill.rect.x);
    assert_eq!(gloss.pen.1 - 6.0, pill.rect.y, "vertically it is still an outset");
    assert_eq!(4.0 * BOX_EM * ADVANCE + 12.0, pill.rect.w);
    assert_eq!(BODY_LINE + 12.0, pill.rect.h);
    assert_eq!(
        "\u{a0}\u{a0}noun\u{a0}\u{a0} a word", gloss.text,
        "and it kept its place on the line rather than breaking one"
    );
}

/// A background pill needs no border. Jitendex's
/// `span[data-sc-class="tag"]` has only a background, radius, and padding.
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

/// Real Jitendex data exposed a defect: a node with `data.content` opens a
/// block even when its tag is inline (`GlossDoc::has_marker`). A marked pill
/// then carried the same box as both `block_box` and `inline_boxes`. A bin that
/// iterated over `SceneElem::boxes()` painted it twice. Jitendex's
/// `span[data-sc-class="tag"]` has this `data.content` key.
///
/// The box follows the tag. CSS treats `span` as inline, so the box belongs to
/// the pill, not the paragraph. Beside bare sentence text, the marker opens no
/// line (`GlossDoc::prose`). A marked pill is markup, not a sense separator, so
/// the run stays one paragraph.
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
    // The pill has 3px of padding on each side. Each side becomes a no-break space
    // at the run edge. These spaces reserve the room that the pill paints
    // (`pill::PILL_SPACER`).
    assert_eq!("before\u{a0}noun\u{a0} a word", gloss[0].text);

    let pill = gloss[0];
    assert_eq!(1, pill.boxes().count(), "one pill, one box - a bin paints `boxes()`");
    assert_eq!(None, pill.block_box, "an inline tag's box is its own, marker or not");
    assert_eq!(Some((0x56, 0x56, 0x56)), pill.inline_boxes[0].style.background);
    // The box matches its run, not its paragraph. "noun" has four units at 7.5px,
    // and its two spacers reserve 3px on each side.
    assert_eq!(4.0 * BOX_EM * ADVANCE + 6.0, pill.inline_boxes[0].rect.w);
    assert_eq!(BODY_LINE + 6.0, pill.inline_boxes[0].rect.h);
}

/// A Jitendex example keyword appears as a marked `span` inside a sentence.
/// The node has `data.content = "example-keyword"` and occurs in 51 062 nodes.
/// The marker once broke the sentence after each word before `ぜひ`, then
/// placed the rest on a new line. Beside bare sentence text, a marker opens no
/// separator (`GlossDoc::prose`). The sentence is one paragraph, and its ruby
/// readings stay above their bases.
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
    // The word joiners are ruby glue that each base carries.
    assert_eq!("ぜひ雑\u{2060}談\u{2060}でもしにいらしてください。", gloss[0].text);
    assert_eq!(2, gloss[0].ruby.len(), "and both readings survive");
}

/// A footnote after the sentence follows the same rule. Jitendex ends an example
/// translation with a marked `span` (`data.content = "attribution-footnote"`).
/// This shape occurs in 9 784 nodes. Jitendex also wraps the sentence in
/// `span lang="en"`, so no bare string sits beside the mark. Therefore,
/// `GlossDoc::prose` does not apply. The marker once put `[1]` on its own line.
///
/// `GlossDoc::inline_prose` treats the prose before the mark as prose. This test
/// uses a real corpus node without changes.
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

/// Return where each of `elem`'s spans lands after a bin re-measures the run.
///
/// Both bins re-measure an element's spans before they paint it
/// (`popup::paint::run_of`, `ui::render::draw_elem`). This function returns the
/// geometry that a background can compare with, not the walk's `Measured` value.
/// A core pass could add room after the wrap. The bin would not include that room.
/// The full reservation prevents that mismatch.
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

/// One span's box on the first line that it touched, by index.
fn painted(boxes: &Measured, span: u32) -> SpanBox {
    *boxes
        .spans
        .iter()
        .find(|b| b.span == span)
        .unwrap_or_else(|| panic!("span {span} landed nowhere in {boxes:?}"))
}

/// This test covers a defect where an inline box's horizontal margin, border, and
/// padding failed to reserve advance in the line.
///
/// The author saw this defect on a real Wayland surface with Jitendex.
/// Jitendex's `span[data-sc-class="tag"]` declares
/// `padding: 0.2em 0.3em` and `margin-right: 0.5em`. The panel drew
/// `go (game)〔眼 only〕`, while Yomitan draws `go (game) 〔眼 only〕`. The
/// margin reserved no space, and the box extended over padding that it had not
/// reserved. The background then covered 3.6 physical pixels below the next
/// word.
///
/// Every number below uses `FakeMeasure`: one no-break space advances half its
/// size, so `n` spaces at size `s` reserve `n * s / 2`. A left margin and a
/// border join Jitendex's declarations, so this test prices all three properties
/// in one pass.
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

    // The run reserves one edge at a time: 0.1em margin, 0.2em border, and 0.3em
    // padding. It then reserves the word and the same values with a 0.5em margin.
    // One span records each reservation in line order.
    assert_eq!(
        "\u{a0}\u{a0}\u{a0}noun\u{a0}\u{a0}\u{a0}\u{a0}Chinese character", gloss.text,
        "the room is text, because text is what both bins re-measure"
    );
    assert_eq!(
        vec![3.0, 7.5, BOX_EM, 7.5, 7.5, BOX_EM],
        gloss.spans.iter().map(|s| s.size).collect::<Vec<_>>(),
        "each spacer solved to the size that reserves its own edge"
    );

    // The seam uses those sizes at the same width that the walk measured. This
    // gives the advance that the bin uses.
    let boxes = painted_spans(gloss);
    assert_eq!(1, boxes.metrics.lines, "one line, so one fragment per span");
    let widths: Vec<f32> = (0..6).map(|i| painted(&boxes, i).w).collect();
    assert_eq!(
        vec![1.5, 7.5, 4.0 * BOX_EM * ADVANCE, 7.5, 7.5, 17.0 * BOX_EM * ADVANCE],
        widths,
        "margin-left 1.5, border+padding 7.5, the word, 7.5, margin-right 7.5"
    );

    // The box is the border box. Margins stay outside, padding stays inside, and
    // its two ends are the two spacers.
    let pill = gloss.inline_boxes[0];
    assert_eq!(gloss.pen.0 + 1.5, pill.rect.x, "the left margin is outside the box");
    assert_eq!(7.5 + 4.0 * BOX_EM * ADVANCE + 7.5, pill.rect.w);

    // The next word starts one full margin-right away from the background. Before
    // this fix, it started beside the pill's glyphs. Every term in 1.5 + 7.5 + 30
    // + 7.5 + 7.5 reserves part of the gap.
    let word = painted(&boxes, 5);
    assert_eq!(54.0, word.x);
    assert_eq!(
        7.5,
        (gloss.pen.0 + word.x) - (pill.rect.x + pill.rect.w),
        "margin-right, and no background under the word"
    );
}

/// A bin re-measures the run before it draws a box. The drawn rect and the
/// reserved advance therefore come from one measurement. The background cannot
/// pass the room that the text reserves.
///
/// This test checks `rect == cover of the box's own spans`, not
/// `rect == what the style declared`. Both checks agree when a no-break space
/// is at least a quarter of an em. Every real font and this fake meet that
/// bound. On a narrower font, [`PILL_SPACERS_PER_EM`] yields less room, so only
/// the identity check remains. [`place_pills`] reads the box from the run
/// instead of the declared style values.
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

    // The box covers its two spacers and its word exactly.
    let (lead, word, trail) = (painted(&boxes, 0), painted(&boxes, 1), painted(&boxes, 2));
    assert_eq!(gloss.pen.0 + lead.x, pill.rect.x, "the box starts where its padding does");
    assert_eq!(trail.x + trail.w - lead.x, pill.rect.w, "and ends where it ends");
    assert_eq!(4.5, lead.w, "0.3em of padding, reserved");
    assert_eq!(4.5, trail.w);
    assert_eq!(lead.x + lead.w, word.x, "the word starts after the padding");

    // No glyph from the next word lies under the fill.
    let after = painted(&boxes, 4);
    assert!(
        pill.rect.x + pill.rect.w <= gloss.pen.0 + after.x,
        "{pill:?} reaches past the word at {after:?}"
    );
}

/// The reservation must not create a wrap opportunity. A pill's
/// `margin-right` and the word it separates stay on one line. Otherwise, the
/// gap and the reason for it would land on different lines.
///
/// U+00A0 has UAX #14 class GL, so LB12 forbids a break after it. LB12a also
/// forbids a break before it unless a space comes first. `FakeMeasure` models
/// these rules because two reservations in this renderer depend on [`glue`].
#[test]
fn a_pills_margin_never_breaks_away_from_the_word_it_separates() {
    let p = rich(&sc(concat!(
        r##"{"tag":"div","content":[{"tag":"span","style":{"##,
        r##""backgroundColor":"#565656","marginRight":"0.5em"},"##,
        r##""content":"noun"},"Chinese character"]}"##
    )));
    // This test checks every width that can wrap this run. It does not pin one
    // width. The important break falls on the gap. A single width would lose that
    // guarantee when nearby arithmetic changes. Here `wrap_w` is
    // `max_w - 24`, so widths 16 through 175 include the break at 46.
    let mut wrapped = 0;
    for step in 0..160 {
        let s = laid_out(&p, 40.0 + step as f32, 4000.0, false, false);
        let gloss = one_body(&s);
        let boxes = painted_spans(gloss);
        wrapped += usize::from(boxes.metrics.lines > 1);

        // The gap is one fragment. The wrap never splits it.
        let margin: Vec<SpanBox> =
            boxes.spans.iter().copied().filter(|b| b.span == 1).collect();
        assert_eq!(1, margin.len(), "the gap wrapped inside itself: {boxes:?}");
        // The gap shares a line with the pill's final fragment and the next word's
        // first fragment. This check uses the pill's last fragment because at 16px
        // wrap the word inside the pill also wraps.
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

/// The vertical case differs from the horizontal case. CSS lets an inline box's
/// vertical padding and border paint without a taller line.
///
/// The paragraph advances as if the box were absent. Horizontal spacers do not
/// change box height. [`measure_pills`] caps spacer size at the box's em.
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

    // The rect extends 0.5em above and below the line and can cover adjacent
    // content.
    let pill = gloss.inline_boxes[0];
    assert_eq!(gloss.pen.1 - 7.5, pill.rect.y);
    assert_eq!(BODY_LINE + 15.0, pill.rect.h);
}

/// An inline margin can reserve space without a box. The layout pass
/// still reserves the room, as a browser does for
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

/// A pill can lose a span when the pass trims an edge. `InlineBox` stores its
/// run by span index and finds spacers by the same indices. [`trim`] must
/// renumber both values. A stale pair would size a word as a spacer.
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

/// An inline box can contain a block. The block opens a paragraph and replaces
/// the paragraph that the box measured. The box's recorded span indices then
/// point to a paragraph that no longer exists.
///
/// The correct result is no box. This test states that result directly. Before
/// spacers reserved the room, box output depended on the number of spans in the
/// old paragraph. A stale index could also resize the first word in the new
/// paragraph.
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

/// A pill around no span is no box. Its edge room returns to the paragraph,
/// because a visible gap must contain content.
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

/// Real dictionary data exposed another defect: `css_len` read `em`, `%`, and
/// `px`, but discarded `rem`. Jitendex writes `rem` on
/// `div[data-sc-class="extra-box"]` (`0.4rem` and `0.5rem`). Onomatoproject
/// writes it on 3 096 inline nodes.
///
/// `rem` is the root em, which is the theme body size in this popup. Yomitan
/// uses the same root size because `display.js` writes the reader font size to
/// `documentElement.style.fontSize`. A node can shrink its own text, but a
/// `rem` length still uses the panel root em. If code used the node em, it
/// would create this bug.
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

/// CSS sets `border-style` to `none` until a declaration changes it. The used
/// width then becomes zero, regardless of `borderWidth`. A width alone draws
/// nothing in a browser, and it draws nothing here.
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

/// These shorthand tests cover the two edge types that use them. Real
/// dictionaries write both forms. Jitendex uses `padding: 0.2em 0.3em` on a
/// pill. Its info box uses `border-style: none none none solid`.
#[test]
fn edge_shorthands_expand_the_way_css_expands_them() {
    // The tuple stores the declaration, then top, right, bottom, and left.
    let lengths: &[(&str, f32, f32, f32, f32)] = &[
        (r#""0.2em""#, 3.0, 3.0, 3.0, 3.0),
        (r#""0.2em 0.4em""#, 3.0, 6.0, 3.0, 6.0),
        (r#""0.2em 0.4em 0.8em""#, 3.0, 6.0, 12.0, 6.0),
        (r#""0.2em 0.4em 0.8em 1em""#, 3.0, 6.0, 12.0, 15.0),
        // CSS drops a fifth value, so it is not a shorthand.
        (r#""1em 1em 1em 1em 1em""#, 0.0, 0.0, 0.0, 0.0),
        // A unit that this build cannot read makes the shorthand invalid. The pass
        // must reject the valid part too.
        (r#""0.2em 3vw""#, 0.0, 0.0, 0.0, 0.0),
        // A bare number is Yomitan's em multiplier.
        ("0.2", 3.0, 3.0, 3.0, 3.0),
        // `px` uses Yomitan's base and scales with the panel.
        (r#""14px""#, 15.0, 15.0, 15.0, 15.0),
        // `rem` uses the panel body size, regardless of the node em.
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
        // `groove` becomes one solid hairline rule.
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

/// A real dictionary has one left rule. Styles `none` make the other three
/// used widths zero, so the box reserves space and draws one edge.
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

/// `textAlign` positions a line within the block width. The element reports the
/// alignment so both painters can pass it to their engines.
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

/// `whiteSpace: pre-line` preserves dictionary newlines at paragraph edges and
/// inside paragraphs. Without it, the browser and panel collapse edge breaks.
#[test]
fn white_space_pre_line_preserves_a_literal_newline() {
    let cases: &[(&str, &str)] = &[
        (r#","style":{"whiteSpace":"pre-line"}"#, "\none\ntwo\n"),
        ("", "one\ntwo"),
        // This is not `pre-line`: the seam does not disable line wrap, so the paragraph
        // keeps its original form.
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

/// A `details` and `summary` pair produces two elements, not one sentence.
/// Four census dictionaries with 31k nodes once merged the summary into its
/// body. This test uses the expanded pair. The header weight comes
/// from the specification defaults table. The panel has no disclosure control,
/// so weight distinguishes the summary from the body.
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

/// One list level in panel pixels. Yomitan sets it to `1.4em` of the body size.
const LEVEL: f32 = LIST_INDENT_EM * BOX_EM;

/// A `disc` marker beside an item.
fn bullet() -> String {
    format!("{DISC_MARKER}{MARKER_GAP}")
}

/// Return a marker's width as `FakeMeasure` measures it.
///
/// Each UTF-16 unit contributes one unit. The width includes [`MARKER_GAP`].
/// The gap stays inside the marker box and keeps the glyph away from marked text.
fn marker_w(label: &str) -> f32 {
    label.encode_utf16().count() as f32 * BOX_EM * ADVANCE
}

/// Get the marker on an item.
fn one_marker(e: &SceneElem) -> &MarkerBox {
    assert_eq!(1, e.marker.len(), "expected one marker, got {:?}", e.marker);
    &e.marker[0]
}

/// Return a marker's left edge in panel pixels.
fn marker_x(e: &SceneElem) -> f32 {
    e.pen.0 + one_marker(e).x
}

/// Return the start of each line in an element's run, in panel pixels.
///
/// A bin re-measures the element's spans at its wrap width and draws the run from
/// one origin. A line starts at that origin plus its leftmost span box. This value
/// shows whether a wrapped item aligns under its marker or under its text.
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

/// Build a card with one list of plain-text items.
///
/// The string in `style` is the list's inline style. It can include commas.
/// The test places `listStyleType` on the node that CSS uses.
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

/// Each item must produce one element with a marker at the same indent.
/// This test checks marker text and offsets, not item text.
#[test]
fn an_unordered_list_marks_every_item_and_indents_them_alike() {
    let s = laid_out(&list_card("ul", "", &["a", "b", "c"]), 224.0, 4000.0, false, false);
    let items = bodies(&s);
    assert_eq!(3, items.len(), "one element per item");
    for (i, text) in ["a", "b", "c"].iter().enumerate() {
        assert_eq!(*text, items[i].text, "the marker is no part of the item's text");
        assert_eq!(bullet(), one_marker(items[i]).text, "item {i}");
        assert_eq!(s.origin + LEVEL, items[i].pen.0, "item {i} sits one level in");
        // `list-style-position: outside` puts the marker box's right edge at the
        // item's content edge inside the gutter. The end gap separates
        // the marker from the text.
        assert_eq!(items[i].pen.0 - marker_w(&bullet()), marker_x(items[i]));
        assert!(marker_x(items[i]) >= s.origin, "and inside the panel's column");
    }
    // The indent reduces both wrap width and pen position. The marker stays in the
    // gutter and takes no line width.
    assert_eq!(s.content_w - LEVEL, items[0].wrap_w);
    // An indent is not a box. The pass draws no box around the item because it
    // only moves the item inward.
    assert_eq!(None, items[0].block_box);
}

/// This test checks an ordinal for each list item. It does not check Sense
/// labels. The ordinal is a marker, not a Sense number.
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

/// Each nested level adds one indent. The pass resolves the marker again at the
/// inner level.
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
    // The inner level also reduces wrap width, so the deeper item is narrower.
    assert_eq!(items[0].wrap_w - LEVEL, items[1].wrap_w);
}

/// A nested list marker hangs in the inner gutter, one level past the outer
/// gutter. A marker box meets the content edge of its list, and each level
/// opens its own gutter.
///
/// Two unordered levels use the same bullet. Both fit the 21px gutter. This
/// test checks the gutter, not how a wide counter extends past a narrow one.
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
    // Each marker stays in its own gutter. It lies past its list content edge and
    // before the marked text.
    for item in [outer, inner] {
        assert!(marker_x(item) >= item.pen.0 - LEVEL, "{:?}", item.text);
        assert!(marker_x(item) + one_marker(item).w <= item.pen.0, "{:?}", item.text);
    }
    // The inner marker lies past the outer text. It does not use the outer gutter.
    assert!(marker_x(inner) >= outer.pen.0);
}

/// An item whose content is a nested list shares one line with its inner item.
/// Both markers therefore land on one element, each in its own gutter.
///
/// Jitendex writes this shape as `ul[sense-groups] > li > ol > li > ul > li`.
/// It once produced `"\u{2022} \u{2460} \u{2022} to eat"` as one run with three
/// markers on one line.
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
    // The outer content edge is one level in, and the inner edge is two levels in.
    // Each marker's right edge meets its list content edge.
    // Both markers use the item's first baseline. They share one line, and
    // `FakeMeasure` places a body baseline halfway down that line.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), item.pen.0 + outer.x);
    assert_eq!(s.origin + 2.0 * LEVEL - marker_w(&bullet()), item.pen.0 + inner.x);
    assert_eq!(0.0, outer.y);
    assert_eq!(0.0, inner.y);
}

/// A dictionary counter renders as written. The pass does not run a counter
/// algorithm over it or add a suffix.
#[test]
fn a_literal_string_marker_renders_verbatim() {
    // CSS accepts both quote characters. The census includes both forms.
    let quoted: &[&str] = &[r#"'\u2460'"#, r#"\"\u2460\""#];
    for value in quoted {
        let style = format!(r#","style":{{"listStyleType":"{value}"}}"#);
        let s = laid_out(&list_card("ul", &style, &["a"]), 224.0, 4000.0, false, false);
        let item = one_body(&s);
        assert_eq!(format!("\u{2460}{MARKER_GAP}"), one_marker(item).text, "{value}");
        assert_eq!("a", item.text, "{value}");
    }
}

/// `list-style-type` inherits, but an item's declaration takes precedence over
/// its list's declaration. The item draws the marker.
///
/// Jitendex uses this form in 38 381 entries with 97 150 nodes. Every
/// declaration is on an `li`. Its ①②③ sense numbers use this property. If the
/// pass resolved the style only on the list, it would draw bullets everywhere.
#[test]
fn an_items_own_list_style_wins_over_its_lists() {
    // This is `ol > li[listStyleType]`, the shape that Jitendex writes for a sense group.
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

    // The list's declaration reaches an item with no declaration. A `styles.css`
    // rule can therefore set the marker on the list.
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

/// An unreadable `listStyleType` uses the initial marker for the list tag. The
/// fallback has the tag's marker shape, not no marker. A readable keyword wins.
#[test]
fn an_unreadable_list_style_falls_back_to_each_tags_initial_value() {
    // Each tuple stores a tag, a keyword, and a marker without its gap.
    let cases: &[(&str, &str, &str)] = &[
        ("ul", "", DISC_MARKER),
        ("ul", "disc", DISC_MARKER),
        ("ul", "circle", CIRCLE_MARKER),
        ("ul", "square", SQUARE_MARKER),
        ("ul", "decimal", "1."),
        ("ol", "", "1."),
        ("ol", "decimal", "1."),
        ("ol", "disc", DISC_MARKER),
        // Locale counter algorithms are out of scope in the specification. Each
        // tag keeps its initial value.
        ("ul", "katakana", DISC_MARKER),
        ("ul", "lower-roman", DISC_MARKER),
        ("ol", "cjk-ideographic", "1."),
        ("ol", "hiragana-iroha", "1."),
        // An unreadable keyword also uses the tag's initial value.
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

/// The `none` keyword removes a marker instead of the default `disc`.
/// An empty string counter has the same CSS sense.
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

/// `::marker` inherits the item's style, not the style of its content.
///
/// The marker was once the item's first paragraph span. It is now a positioned
/// run beside the element ([`MarkerBox`]). A nearby style cannot reach it.
/// This test checks the style that the marker run carries.
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

/// A marker beside a ruby base must stay outside the base slot. Otherwise, the
/// reading would center over the marker and the base.
///
/// The marker now sits outside the run, so it cannot reach the slot. The reading
/// centers over the base at the item's left edge. This proves that the marker
/// hangs outside the run.
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
    // The base starts at the item's left edge. A half-size reading unit centers on it.
    let read = &item.ruby[0];
    assert_eq!((unit - unit * RUBY_RATIO) / 2.0, read.x);
    assert_eq!(unit * RUBY_RATIO, read.w);
    // The marker stays left of the base and stays outside the run.
    assert_eq!(bullet(), one_marker(item).text);
    assert!(one_marker(item).x < 0.0);
}

/// The marker belongs to the item's first line. If the item content is a block,
/// that line belongs to the block's paragraph. If the marker attached to the block,
/// it would create an element that contains only a bullet.
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
    // The list owns the gutter. The marker hangs beside the block line at the list level.
    assert_eq!(items[0].pen.0 - marker_w(&bullet()), marker_x(items[0]));
}

/// An empty item gets no marker, but the counter still counts it.
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

/// A table-only item has no line inside its grid for a marker. The marker uses
/// an inline line above the grid. Without that line, the gutter would lose the bullet.
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

/// A list without `li` items gets no list-item behavior. The pass treats it as
/// another block. Only `li` has `display: list-item`.
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

/// An item's `paddingLeft` adds to the list level indent. It does not replace
/// that indent. The browser assigns padding to the item and indent to the list.
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
    // The list indent shifts the item's border box. The item padding insets text
    // inside that box.
    let outer = one_block_box(&s);
    assert_eq!(s.origin + LEVEL, block_box(outer).rect.x);
    assert_eq!(s.content_w - LEVEL, block_box(outer).rect.w);
    // The marker hangs at the list content edge. Item padding moves only the text,
    // so the bullet stays at the list edge. This matches the browser's `outside` marker.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), marker_x(item));
}

/// The `inside` value once placed the marker in the paragraph span. The
/// continuation lines then used the item indent. The marker now stays outside
/// the run, so every line starts at the text indent. This matches Yomitan's
/// browser default, `outside`.
#[test]
fn a_wrapped_items_continuation_lines_align_to_its_text_not_its_marker() {
    let long = "a".repeat(30);
    let s = laid_out(&list_card("ul", "", &[&long]), 224.0, 4000.0, false, false);
    let item = one_body(&s);
    // The 200px column loses one 21px level, so 179px stays. That width fits 23
    // units at 7.5px, so 30 units use two lines.
    assert_eq!(s.content_w - LEVEL, item.wrap_w);
    assert_eq!(2, item.lines);
    assert_eq!(vec![s.origin + LEVEL, s.origin + LEVEL], line_x(item));
    // The first and continuation lines start at the item's text edge. The marker
    // stays left of both lines in the gutter.
    assert_eq!(s.origin + LEVEL - marker_w(&bullet()), marker_x(item));
    assert!(marker_x(item) < line_x(item)[1], "the second line is not under it");
}

/// The `stack_items` parameter selects compact or stacked list layout.
///
/// This test calls `layout::scene` because only the scene can request compact
/// layout. It checks the contract between the scene and the pass. `marker_of`
/// and `Marker::label` produce the same marker text for either value.
/// `stack_items` changes only marker placement.
///
/// A compact list has no gutter, so the list joins markers into one paragraph.
/// A stacked list places one marker beside each item. Settings and scene-level
/// checks belong elsewhere.
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

/// Yomitan's base font size for these grid fixtures.
///
/// The specification writes a cell border as `1em / 14`. At fourteen pixels,
/// the rule is one pixel and `0.25em` padding is 3.5. `FakeMeasure` assigns 7
/// per UTF-16 unit on a 28-pixel line. Each expectation uses arithmetic that a
/// reader can check by hand.
const GRID_EM: f32 = YOMITAN_BASE_PX;
/// The width of one cell rule at [`GRID_EM`].
const RULE: f32 = 1.0;
/// The padding on each cell edge at [`GRID_EM`].
const CELL_PAD: f32 = 3.5;
/// The height of one cell-text line at [`GRID_EM`].
const CELL_LINE: f32 = GRID_EM * LINE_H;
/// The width of one UTF-16 cell-text unit at [`GRID_EM`].
const CELL_UNIT: f32 = GRID_EM * ADVANCE;

/// Build a scene for `p` at [`GRID_EM`].
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
            selection: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run")
}

/// Build a scene for one structured-content `glossary` at [`GRID_EM`].
fn gridded(glossary: &str, max_w: f32) -> PopupScene {
    grid_scene(&rich(&sc(glossary)), max_w, false)
}

/// Build one `tr` with cells that contain `tag:content`.
fn mixed_row(cells: &[(&str, &str)]) -> String {
    let body: Vec<String> = cells
        .iter()
        .map(|(tag, content)| format!(r#"{{"tag":"{tag}","content":"{content}"}}"#))
        .collect();
    format!(r#"{{"tag":"tr","content":[{}]}}"#, body.join(","))
}

/// Build one `tr` with `td` cells.
fn tr(cells: &[&str]) -> String {
    let body: Vec<(&str, &str)> = cells.iter().map(|c| ("td", *c)).collect();
    mixed_row(&body)
}

/// Build a `table` that contains `rows`.
fn table(rows: &[String]) -> String {
    format!(r#"{{"tag":"table","content":[{}]}}"#, rows.join(","))
}

/// Return every cell box in document order.
fn grid_cells(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Cell).collect()
}

/// Return every gloss-body run in a grid scene, in draw order.
///
/// [`bodies`] selects the default body size. These fixtures use [`GRID_EM`], and
/// no other theme role uses that size.
fn grid_text(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems
        .iter()
        .filter(|e| e.kind == ElemKind::Text && e.font_size == GRID_EM)
        .collect()
}

/// Get the only table element in a scene.
fn grid(s: &PopupScene) -> &SceneElem {
    find(s, ElemKind::Table)
}

/// Return every cell border box as `(x, y, w, h)` relative to the table's top-left.
///
/// Expectations use grid arithmetic instead of an offset from panel chrome.
fn boxes(s: &PopupScene) -> Vec<(f32, f32, f32, f32)> {
    let at = grid(s).rect;
    grid_cells(s)
        .iter()
        .map(|e| (e.rect.x - at.x, e.rect.y - at.y, e.rect.w, e.rect.h))
        .collect()
}

/// Full acceptance geometry: nine one-character cells form a three-by-three
/// grid with `border-collapse`. Each cell owns its left and top rule. Cells at
/// the right and bottom edges own the end rules. Adjacent cells therefore meet
/// exactly, and the pass draws each shared rule once.
///
/// A column is `3.5 + 7 + 3.5 = 14` wide. A row is `3.5 + 28 + 3.5 = 35`
/// tall. An interior cell's border box is `1 + 14` by `1 + 35`.
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

/// A `colSpan` cell covers its columns and the rule between them. The test
/// compares its width with the cells below it. The next cell starts where the
/// spanned columns end.
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

/// A `rowSpan` cell covers both rows. Cells in the next row move into the free
/// column.
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

/// A cell with both spans occupies two columns and two rows. The only free slot
/// in row two is column three. The spanned cell alone sets both column widths,
/// so it shares its shortfall equally.
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
    // The span requests 14px. The two columns contain only the rule between them,
    // so each receives 6.5px.
    let col = CELL_PAD + CELL_UNIT + CELL_PAD;
    assert_eq!((0.0, 0.0, RULE + col, grid(&s).rect.h), span);
    assert_eq!(span.0 + span.2, first.0, "the third column starts after both");
    assert_eq!(first.0, second.0, "and the second row's only cell lands in it");
    assert_eq!(first.1 + first.3, second.1);
}

/// A cell without declared style still uses the grid defaults. Yomitan sets a
/// solid `1em / 14` border and `0.25em` padding in the panel rule color. Text
/// sits inside both.
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

/// `1em / 14` gives one pixel at the base font size and scales with the panel.
/// At twice the size, the rule is two pixels and padding also doubles. This
/// ratio is why the specification uses `1em / 14`.
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
            selection: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run");
    let style = grid_cells(&s)[0].block_box.expect("a cell is a box").style;

    assert_eq!(Edges::all(2.0 * RULE), style.border);
    assert_eq!(Edges::all(2.0 * CELL_PAD), style.padding);
}

/// With `border-collapse`, an interior cell owns only its left and top rule.
/// The neighbor owns the shared rule on its right, so the pass draws it once.
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

/// A header cell is bold and has a tinted background, as Yomitan draws it.
/// `tag_style`'s HTML-default table defines `th`. The box property supplies
/// the tint.
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

/// Yomitan applies the same header rule to `thead`. A plain `td` in a header
/// row therefore becomes a header cell. This makes the first row of a real
/// conjugation table bold even when its cells are not all `th`.
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

/// The layout pass clips a table wider than the panel to the panel width. It
/// scales columns by one factor and rewraps their content in the narrower
/// columns. The grid ends at the panel content edge.
///
/// At 108 pixels of content, three rules leave 105 pixels. Each cell measures
/// 30 units and requests 105 pixels with padding. The two cells request twice
/// the available width, so each column becomes 52.5 pixels.
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

/// The panel width comes from its offered width, not from an element extent. A
/// wide table therefore cannot widen the panel. The grid keeps this rule intact.
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
            pitch: Vec::new(),
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

/// A malformed table with extra cells does not panic. The longest row sets grid
/// width. A shorter row ends early, so its empty slots remain empty. The grid
/// does not invent cells.
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

/// A blank cell still has a slot and border. Without a box, the grid would
/// break. The cell has no text or spans, so a painter draws only its box and
/// sends no text to the shaper.
#[test]
fn an_empty_cell_still_draws_its_border() {
    let row = r#"{"tag":"tr","content":[{"tag":"td"},{"tag":"td","content":"b"}]}"#;
    let s = gridded(&table(&[row.to_string()]), 424.0);
    let cells = grid_cells(&s);

    assert_eq!(2, cells.len());
    assert!(cells[0].spans.is_empty(), "nothing to shape");
    assert!(cells[0].text.is_empty());
    let border = cells[0].block_box.expect("still a box").style.border;
    // The cell is in the last row, not the last column. The right rule belongs
    // to the adjacent cell.
    assert_eq!(Edges { top: RULE, right: 0.0, bottom: RULE, left: RULE }, border);
    // An empty column has only padding and no ink.
    assert_eq!(RULE + 2.0 * CELL_PAD, cells[0].rect.w);
    assert_eq!(cells[0].rect.x + cells[0].rect.w, cells[1].rect.x);
}

/// Each cell stores its source node. A hit in a conjugation table therefore
/// resolves to that cell's subtree. The subtree renderer reproduces that subtree.
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

/// A row uses the tallest cell height. A shorter cell stays at the row top,
/// which matches Yomitan's `vertical-align: top`. Cells do not stretch. A bin
/// re-measures the short cell and gets the same line boxes.
#[test]
fn a_row_is_as_tall_as_its_tallest_cell_and_the_short_one_stays_at_the_top() {
    let tall = concat!(
        r#"{"tag":"td","content":["#,
        r#"{"tag":"div","content":"a"},{"tag":"div","content":"b"}]}"#
    );
    let row = format!(r#"{{"tag":"tr","content":[{tall},{{"tag":"td","content":"c"}}]}}"#);
    let s = gridded(&table(&[row]), 424.0);
    let text = grid_text(&s);

    // Two lines are `LINE_GAP` apart, with padding above and below.
    let want = 2.0 * CELL_PAD + 2.0 * CELL_LINE + LINE_GAP;
    assert_eq!(2.0 * RULE + want, grid(&s).rect.h);
    assert_eq!(3, text.len());
    assert_eq!(text[0].pen.1, text[2].pen.1, "both cells start at the row's top");
    assert_eq!(CELL_LINE, text[2].rect.h, "and the short one is still one line");
}

/// A cell taller than its spanned rows grows those rows evenly. Its box ends at
/// the end of the last row. The text inside the rows keeps its own height.
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

    // Three lines take 92px. Two 35px rows plus their rule take 71px, so each
    // row receives half of the 28px shortfall.
    let row = CELL_PAD + CELL_LINE + CELL_PAD + 14.0;
    assert_eq!(3.0 * RULE + 2.0 * row, grid(&s).rect.h);
    assert_eq!(grid(&s).rect.h, cells[0].3, "the spanning cell covers both rows");
    assert_eq!(RULE + row, cells[1].3, "and each row grew by half the shortfall");
    assert_eq!(RULE + row, cells[2].1, "the second row starts after the first");

    let text = grid_text(&s);
    assert!(text.iter().all(|e| e.rect.h == CELL_LINE), "no line was stretched");
}

/// A link inside a cell is clickable at the cell's final position, not its
/// measurement position. The grid first measures cells at the table top, then
/// moves each cell into its row. Hit targets move with the cell.
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

/// An empty table draws no rule.
#[test]
fn a_table_with_no_cells_draws_nothing() {
    let s = gridded(r#"[{"tag":"table"},"after"]"#, 424.0);

    assert!(grid_cells(&s).is_empty());
    assert!(!s.elems.iter().any(|e| e.kind == ElemKind::Table));
    assert!(texts(&s).contains(&"after"), "and the text beside it still renders");
}

/// A table is a block. It closes the current paragraph, opens no paragraph, and
/// places later content below the complete grid.
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

/// A table inside a list item pays the indent once. `Block::inherited` passes
/// the indent to each child, so the grid must clear it for its cells.
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

/// A table inside a cell is its own grid. A cell stores pieces, not paragraphs,
/// so the block pass uses the same two methods for the inner table. The inner
/// cells sit inside the outer cell's content box.
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

/// Each element from a `GlossDoc` node carries that node's path. The path resolves
/// to the node in that document. Panel chrome has no path because `Presentation`
/// builds it without a tree.
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

/// A sense hit resolves to its node path, and the subtree renderer returns only
/// that sense's markup.
///
/// This test covers addressability, not interaction. The interaction is outside
/// this module.
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

/// A path alone identifies a block position in any tree. The dictionary and row
/// also identify the second block as sense 2 of 大辞林.
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

/// A node beyond `NodePath`'s reach has no path instead of an ancestor's path.
/// `child()` refuses the seventeenth step, so the element still names its row.
/// An alias would give a sense picker the wrong subtree.
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

/// The box model must not change the panel's current hit targets.
///
/// This test compares a plain gloss with a boxed gloss. The box moves later
/// targets down when it adds height. Targets above stay fixed. Each target keeps
/// its kind, order, size, and x coordinate. Targets below move by the added
/// height once.
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
                selection: None,
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

    // The box adds 6px of margin, 3px of border, and 3px of padding above and below.
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

/// Both painters must make the same border decision.
///
/// Neither bin exposes a paint API because Direct2D needs a window. Core therefore
/// chooses one rounded-box stroke or one fill per edge. This test asserts that choice.
#[test]
fn an_even_border_strokes_once_and_an_uneven_one_fills_each_edge() {
    let solid = Edges::all(BorderStyle::Solid);
    let left_only = Edges {
        top: BorderStyle::None,
        right: BorderStyle::None,
        bottom: BorderStyle::None,
        left: BorderStyle::Solid,
    };
    // Each tuple stores border widths, edge styles, and the expected single stroke width.
    let cases: &[(Edges<f32>, Edges<BorderStyle>, Option<f32>)] = &[
        (Edges::all(2.0), solid, Some(2.0)),
        // No style means no border, regardless of width.
        (Edges::all(2.0), Edges::default(), None),
        // Zero width gives nothing to stroke.
        (Edges::all(0.0), solid, None),
        // One dictionary has a left rule only. Three edge styles set used widths to
        // zero, so the widths are uneven.
        (Edges::all(2.0), left_only, None),
        // Uneven widths cannot use one rounded path.
        (Edges { top: 1.0, right: 2.0, bottom: 1.0, left: 2.0 }, solid, None),
    ];
    for &(border, border_style, want) in cases {
        let bx = BoxStyle { border, border_style, ..BoxStyle::default() };
        assert_eq!(want, bx.even_border(), "{border:?} / {border_style:?}");
    }
}

// ---- element construction ----

/// The frequency corner is the first element in the list.
#[test]
fn frequency_leads_as_a_corner_so_it_shares_the_headword_line() {
    let theme = Theme::dark();
    let (elems, _) = build_elements(&one_card(&[], Some(7671)), &theme, false, false, RenderSettings::default(), None);
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
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default(), None);
    assert!(!elems.iter().any(|e| matches!(e, Elem::Corner(_))));
}

#[test]
fn part_of_speech_is_dimmed_metadata_not_body_text() {
    let theme = Theme::dark();
    let (elems, _) = build_elements(&one_card(&["noun", "suru"], Some(1)), &theme, false, false, RenderSettings::default(), None);
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

/// Every role owns its metrics.
///
/// Before this split, `reading` used `body_size`. POS and the dictionary label used
/// `collapsed_size`. The frequency corner used `collapsed_size` and `dimmed_text`.
#[test]
fn each_role_takes_its_own_size() {
    let theme = roled_theme();
    let (elems, _) = build_elements(&one_card(&["noun"], Some(7671)), &theme, true, false, RenderSettings::default(), None);
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

/// Each role also owns its weight and style.
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

/// The scene carries each run's weight and style.
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

/// The side column measures every run with the collapsed role.
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

/// The default theme must keep its old measurements: regular, upright, and the
/// sizes in the geometry goldens.
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

/// Both default themes keep role sizes from before the role split.
///
/// The geometry goldens (`crates/chibipop-windows/tests/goldens/geometry`) use exact
/// equality. They define 13px metadata, 15px body, 20px headword, and a 1px rule.
/// Both themes keep each role at its old size. A size change therefore needs new
/// goldens, and this test exposes that change.
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

/// `separator_height` sets the horizontal rule between blocks. It does not set
/// the side column's vertical rule.
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
    let (elems, _) = build_elements(&one_card(&[], Some(1)), &Theme::dark(), false, false, RenderSettings::default(), None);
    assert!(!elems
        .iter()
        .any(|e| matches!(e, Elem::Text(line) if line.text.contains('·'))));
}

#[test]
fn the_headword_is_a_headword_element_not_text() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default(), None);
    assert!(
        elems.iter().any(|e| matches!(e, Elem::Headword { .. })),
        "expected a Headword element for the headword"
    );
}

#[test]
fn headword_prefix_u16_is_zero_without_anki_marks() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default(), None);
    let hw = elems.iter().find_map(|e| match e {
        Elem::Headword { prefix_u16, .. } => Some(*prefix_u16),
        _ => None,
    });
    assert_eq!(Some(0), hw);
}

#[test]
fn show_back_adds_a_back_button_element() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), true, false, RenderSettings::default(), None);
    assert!(matches!(&elems[0], Elem::BackButton(_)));
}

#[test]
fn no_back_button_without_show_back() {
    let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false, RenderSettings::default(), None);
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

/// Collapsed rows have no duplicate marks.
#[test]
fn collapsed_rows_carry_no_dupe_marks() {
    let (elems, _) = build_elements(&with_collapsed(), &Theme::dark(), false, false, RenderSettings::default(), None);
    for e in &elems {
        if let Elem::Collapsed(_, line) = e {
            assert!(!line.text.starts_with('\u{2713}'), "no check marks on collapsed rows");
        }
    }
}

#[test]
fn side_panel_false_keeps_collapsed_rows_inline() {
    let (elems, side) = build_elements(&with_collapsed(), &Theme::dark(), false, false, RenderSettings::default(), None);
    assert!(side.is_empty());
    assert!(elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
}

#[test]
fn side_panel_true_moves_collapsed_rows_to_side() {
    let (elems, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true, RenderSettings::default(), None);
    assert!(!elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
    assert_eq!(2, side.len());
    assert!(side[0].text.contains('\u{96D1}'));
}

#[test]
fn side_entries_carry_expand_indices() {
    let (_, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true, RenderSettings::default(), None);
    assert_eq!(0, side[0].idx);
    assert_eq!(1, side[1].idx);
}

#[test]
fn side_entries_show_headword_only() {
    let (_, side) = build_elements(&with_collapsed(), &Theme::dark(), false, true, RenderSettings::default(), None);
    assert!(!side[0].text.contains("noise"));
    assert!(!side[1].text.contains("magazine"));
}

// ---- one label per dictionary ----

/// One headword with three 大辞林 rows must draw one 大辞林 label, not three
/// labels with one gloss each.
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

/// Yomitan's `<ol>` has one item per matched term-bank row. Hoshi Reader emits
/// this list only when a dictionary contributes multiple rows. A lone row has no
/// number, and its glossary items have no numbers.
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

/// Row tags use dimmed metadata, like the card tag line. An empty tag set draws
/// nothing because `present` already printed the line.
#[test]
fn a_rows_tags_draw_a_dimmed_line_and_an_empty_set_draws_none() {
    let theme = Theme::dark();
    let p = card_with(vec![GlossBlock {
        dict_name: "大辞林".into(),
        dict_id: crate::present::NO_ROW,
        entries: vec![entry(&["ある"], &["noun", "suru"]), entry(&["いる"], &[])],
    }]);
    let (elems, _) = build_elements(&p, &theme, false, false, RenderSettings::default(), None);
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

/// A dictionary that renders no gloss still shows its label. It draws no empty
/// body line, as the `minimal_edge` golden requires.
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

/// The `adding` state takes precedence over both markers.
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

/// The `checking` state takes precedence over the add label.
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

/// A disconnected Anki hides the button.
#[test]
fn anki_button_label_is_none_when_disconnected() {
    let anki = AnkiPopupState { enabled: true, connected: false, ..AnkiPopupState::disabled() };
    assert!(anki_button_label(&one_card(&[], None), &Theme::dark(), &anki).is_none());
}

// ---- scroll ----

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

/// At maximum scroll, the thumb ends at the track end.
#[test]
fn the_thumb_ends_flush_with_the_track_at_max_scroll() {
    let (top, h) = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
    assert_eq!(300, top + h);
}

/// The thumb has a 1px minimum height.
#[test]
fn the_thumb_has_a_floor() {
    let (_, h) = scrollbar_thumb(300, 100_000, 300, 0).unwrap();
    assert_eq!(SCROLLBAR_MIN_THUMB, h);
}

/// The minimum height must stay within the track.
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

/// A track shorter than the minimum still fits the thumb.
#[test]
fn a_track_shorter_than_the_floor_still_fits() {
    let (top, h) = scrollbar_thumb(10, 600, 300, 0).unwrap();
    assert!(h <= 10, "thumb {h} in a 10px track");
    assert!(top + h <= 10);
}

// ---- inline images ----

/// `FakeMeasure` answers for one [`IMAGE_SPACER`].
///
/// The image pass probes this value. Each expectation uses one no-break space at
/// half its size and a line baseline one size below its top.
const SPACER_ADVANCE: f32 = ADVANCE;
const SPACER_ASCENT: f32 = LINE_H / 2.0;

/// Match one recorded media row that `dict::media::probe` writes.
fn recorded(format: MediaFormat, w: f32, h: f32) -> Intrinsic {
    Intrinsic { format, width: w, height: h, aspect: w / h }
}

/// Build a card with one structured-content row from the dictionary that recorded
/// `media`.
///
/// `dict_id` is 7, not `NO_ROW`, so tests can check the complete media key and
/// its path.
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

/// Return every image element in draw order.
fn images(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Image).collect()
}

/// Get the only image element in a scene with one image.
fn one_image(s: &PopupScene) -> &SceneElem {
    let found = images(s);
    assert_eq!(1, found.len(), "expected one image element, got {}", found.len());
    found[0]
}

/// Get the paragraph that contains an image. It is the gloss element that is not
/// the image.
fn image_host(s: &PopupScene) -> &SceneElem {
    s.elems
        .iter()
        .find(|e| e.kind == ElemKind::Text && e.text.contains(IMAGE_RISER))
        .expect("the paragraph that reserved the image's room")
}

/// First size step: a `height: 1em` gaiji matches its text size and ends at
/// the line baseline.
///
/// The declared size wins over the recorded size. This test records 20x10, but
/// the declared size uses 15x15.
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
    // Its bottom sits on the baseline. The line is one whole em tall above it
    // (`SPACER_ASCENT` of the riser's size). A `1em` box exactly fills that space,
    // so its top is the line's top.
    assert_eq!(host.pen.1, img.rect.y, "top of the first line");
    assert_eq!(host.pen.0, img.rect.x, "and at the paragraph's own pen");
    assert_eq!(0.0, img.advance, "the paragraph already stacked its line");
}

/// Second size step: an image without a declared size uses its recorded size.
/// This avoids collapse and overflow. The census has 99 807 nodes with this shape.
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

/// One declared length uses the recorded `aspect` to find the other length.
/// 字通 and 三省堂 use `height: 1em` without a width.
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

/// `sizeUnits: px` gives scene pixels. An absent unit and `em` use the text size.
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

/// Third size step: no declared size and no stored asset uses a one-em
/// placeholder. No media row means no bytes and no key for a bin.
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

/// An absent asset uses its `alt` as ordinary flow text. The text wraps with
/// the sentence because the image pass cannot draw the asset. Jitendex stores
/// gaiji `alt` in `data`, not an attribute.
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

/// A `title` can label an absent `alt`. The image pass reads an attribute and a
/// `data` entry. 三省堂 writes `title` beside `sizeUnits`.
#[test]
fn a_title_stands_in_for_a_missing_alt() {
    let p = imaged(r#"{"tag":"img","path":"g/x.svg","title":"\u77ed"}"#, &[]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert!(images(&s).is_empty());
    assert_eq!("\u{77ed}", bodies(&s)[0].text);
}

/// An undecodable asset keeps its recorded box and `MediaKey`. It also keeps
/// `alt` as one ordinary span. A bin that cannot rasterize the format draws the
/// fallback text instead of a second text path.
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
    // Both bins remeasure a non-empty span at `wrap_w` before paint.
    // The fallback wraps inside the image box, never at zero width.
    assert!(img.rect.w > 0.0);
    assert_eq!(img.rect.w, img.wrap_w, "the fallback wraps at the image width");
}

/// An image gets inline space because a `span` asks the measurer for it. The
/// pass must not edit line boxes after the wrap. Both bins re-measure the
/// element's spans before paint. This test checks that the reservation equals
/// the image width and that the line reaches image height.
#[test]
fn an_image_buys_its_room_from_the_measurer_and_not_after_the_wrap() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png","width":1.0,"height":1.0,"sizeUnits":"em"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 15.0, 15.0))],
    );
    let (s, asked) = measured(&Theme::dark(), &p, false);
    let host = image_host(&s);

    // `ceil(4 * 15/15)` creates spacers whose total advance equals the image width:
    // four units at half their size.
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
    // The riser gives the line its ascent above the baseline.
    let riser = host
        .spans
        .iter()
        .find(|sp| host.text[sp.at as usize..(sp.at + sp.len) as usize] == *IMAGE_RISER)
        .expect("the riser is a span too");
    assert_eq!(BOX_EM, SPACER_ASCENT * riser.size, "and the height, above the baseline");
    // The seam receives a real probe because only a measurer knows either ratio.
    assert!(
        asked.iter().any(|a| a.text == IMAGE_SPACER && a.size == BOX_EM),
        "the pass probes one spacer at the image's own em"
    );
    // The paragraph height includes the line that the riser grew. Nothing after
    // the wrap changes it.
    assert_eq!(BOX_EM * LINE_H, host.rect.h);
    assert_eq!(1, host.lines, "and it is one line");
}

/// An image in a sentence stays in that paragraph and line. Spacers use
/// non-breaking glue, so the wrap cannot split the reservation or separate it
/// from adjacent text.
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

/// `verticalAlign` reuses the inline pass. A raised image uses the same baseline
/// shift as a raised span. The pass reserves the shift and image height, so the
/// line above does not overlap it.
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
    // The line grows by the rise. The image sits that far above a baseline-aligned image.
    assert_eq!((BOX_EM + rise) * LINE_H, host.rect.h, "the rise is reserved");
    assert_eq!(host.pen.1, img.rect.y, "so the raised box still starts at the top");
    let baseline = BOX_EM + rise;
    assert_eq!(baseline - rise - BOX_EM, img.rect.y - host.pen.1);
}

/// A wide, short image reserves its width without a taller line.
/// The spacer count uses the aspect ratio. The pass caps the spacer size at the
/// riser size, so the image sets line height.
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

/// An image is content, so a paragraph that contains only an image remains.
/// The riser creates the content. The spacer run alone is whitespace, and
/// `flush` would remove that paragraph.
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

/// A `type: image` glossary item is the same replaced element as an `img` tag.
/// The plain-text renderer still drops it.
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

/// An image element stores the node that produced it, not its paragraph.
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

/// The parser reads `collapsed` and `collapsible`, but no code acts on them.
/// Twenty-six dictionaries declare `collapsed` across 243 264 nodes. Later code
/// can use these fields without reading them again.
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

/// Yomitan draws the background when the field is absent, so the default is `true`.
/// Census samples set this field to `false`.
#[test]
fn an_undeclared_background_is_drawn() {
    let p = imaged(
        r#"{"tag":"img","path":"g/x.png"}"#,
        &[("g/x.png", recorded(MediaFormat::Png, 9.0, 9.0))],
    );
    let s = laid_out(&p, 424.0, 4000.0, false, false);

    assert!(one_image(&s).image.as_ref().unwrap().background);
}

/// One row of the tint bound: format, appearance, box size, device pixel ratio,
/// and painter action.
type TintCase = (MediaFormat, Appearance, (f32, f32), f32, Tint);

/// The tint bound rasterizes only a gaiji-sized vector at twice the device pixel
/// ratio, with a longest edge clamp. A larger monochrome asset composites
/// without tint. A raster mask already has pixels, so it avoids rasterization.
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
        // A non-mask needs no tint, whatever its format.
        (MediaFormat::Svg, Appearance::Auto, (15.0, 15.0), 1.0, Tint::None),
        (MediaFormat::Png, Appearance::Auto, (15.0, 15.0), 1.0, Tint::None),
        // A raster mask already has pixels.
        (MediaFormat::Png, Appearance::Monochrome, (15.0, 15.0), 1.0, Tint::Alpha),
        (MediaFormat::Avif, Appearance::Monochrome, (900.0, 900.0), 1.0, Tint::Alpha),
        // A gaiji-sized vector uses twice the ratio on both axes.
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
        // This is exactly the 4em bound. At 2x, 60x30 becomes 240x120, below the clamp.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (4.0 * em, 2.0 * em),
            2.0,
            Tint::Raster(240, 120),
        ),
        // At 3x, a 4em square becomes 360px. The clamp scales both axes to 256px.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (4.0 * em, 4.0 * em),
            3.0,
            Tint::Raster(256, 256),
        ),
        // One axis exceeds 4em, so this is an illustration, not a mask.
        (
            MediaFormat::Svg,
            Appearance::Monochrome,
            (4.0 * em + 0.5, 15.0),
            1.0,
            Tint::None,
        ),
        // A zero-size box has no area for rasterization.
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

/// Two paragraphs each place their own image. Image indices restart per
/// paragraph, like link and reading indices. No paragraph can use an index from
/// another paragraph.
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

/// An image inside a cross-reference shares its hit target with adjacent text.
/// Spacers carry the link, so the target covers the asset.
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
    // The headword also has per-character drill targets. This lookup names the
    // cross-reference target.
    let hit = s
        .hits
        .iter()
        .find(|h| h.action == HitAction::DrillDown("\u{732b}".to_string()))
        .expect("a cross-reference earns a target");

    assert_eq!(Some(img.rect.x), hit.x);
    assert_eq!(Some(img.rect.w), hit.w, "the whole asset, not a sliver");
}

/// A `<ruby>` can use a gaiji image as its base and keep its reading. 251 nodes
/// across eight dictionaries use this shape. In most nodes, the mark is
/// editorial, not decorative. 三省堂 and 大辞林 place their 表外字 mark over the
/// gaiji, and 岩波 places a real reading there. A browser places a reading over
/// a ruby base box, and an image can be that base.
///
/// The reading centers over the asset and ends at the asset's top. A different
/// position, such as the spacer ascent or the paragraph top, fails this test.
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

/// The reading follows the asset, not the line, so `verticalAlign` moves both.
///
/// This fixture matches 岩波国語辞典. It places `ｘ` over 赤鱏's gaiji.
/// It wraps the `img` in a `span` and sets alignment there. CSS and
/// [`tag_style`] define `verticalAlign` as non-inherited. The test checks
/// alignment on the image, which controls the reading geometry.
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

/// A scene for `p` with chosen render settings.
///
/// `layout::scene` consumes these settings. This helper checks the finished
/// scene, not the values passed to an inner pass.
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
            selection: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run")
}

/// Return the default settings after one value changes.
fn without(edit: fn(&mut RenderSettings)) -> RenderSettings {
    let mut render = RenderSettings::default();
    edit(&mut render);
    render
}

/// Get the gloss element whose glyphs equal `text`.
///
/// [`bodies`] selects the default body size, but style tests can change
/// `fontSize`.
///
/// A pill places [`PILL_SPACER`]s first. `pill::measure_pills` reserves
/// horizontal room in the paragraph text. The styled and unstyled cases
/// therefore produce different strings. This selector checks only which
/// element rendered the gloss.
fn gloss_of<'a>(s: &'a PopupScene, text: &str) -> &'a SceneElem {
    let glyphs = |e: &SceneElem| e.text.replace(PILL_SPACER, "");
    s.elems
        .iter()
        .find(|e| e.kind == ElemKind::Text && glyphs(e) == text)
        .unwrap_or_else(|| panic!("no gloss element says {text:?} in {:?}", texts(s)))
}

/// Get the span of `elem` whose glyphs equal `text`.
///
/// A pill uses more than one span. Its edge spacers reserve padding, and a third
/// spacer reserves margin (`pill::measure_pills`). These spans carry the box
/// style at a solved size, so `spans[0]` is room, not the word.
fn span_of<'a>(elem: &'a SceneElem, text: &str) -> &'a ElemSpan {
    elem.spans
        .iter()
        .find(|s| elem.text[s.at as usize..(s.at + s.len) as usize] == *text)
        .unwrap_or_else(|| panic!("no span says {text:?} in {:?}", elem.text))
}

/// Build a gloss with two example sentences and an attribution. Each uses a census
/// `data` hook so the parser classifies it.
///
/// The examples use two conventions: Jitendex's ASCII
/// `content=example-sentence` and 明鏡国語辞典's Japanese `example=`. The latter
/// kept 38 892 example nodes on screen while Jitendex lost its examples. Both
/// now behave the same, so this fixture rejects a classifier that supports only
/// one alphabet.
const EDITORIAL: &str = concat!(
    r#"[{"tag":"span","content":"to eat"},"#,
    r#"{"tag":"div","data":{"content":"example-sentence"},"#,
    r#""content":"\u3054\u98ef\u3092\u98df\u3079\u308b"},"#,
    r#"{"tag":"div","data":{"example":""},"#,
    r#""content":"\u30d1\u30f3\u3092\u98df\u3079\u308b"},"#,
    r#"{"tag":"ul","data":{"content":"attribution"},"#,
    r#""content":[{"tag":"li","content":"JMdict"}]}]"#
);

/// Both the panel and card parse [`EDITORIAL`] once.
///
/// One `Arc` lets both renderers inspect the same document. Both hidden and
/// present cases test filters, not two parses of one string.
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

/// A glossary list that compact mode transforms.
const GLOSSARY_LIST: &str = concat!(
    r#"{"tag":"ul","content":[{"tag":"li","content":"chatting"},"#,
    r#"{"tag":"li","content":"a chat"},{"tag":"li","content":"idle talk"}]}"#
);

/// The compact-mode acceptance case at the seam named by the specification.
///
/// Compact changes display, not the tree. The same three items and markers
/// appear in one element, with [`ITEM_SEPARATOR`] between items. Yomitan and
/// Hoshi Reader implement this with `li { display: inline }` and a separator
/// after each item except the first. The test checks the separator itself, so a
/// mode that drops items fails.
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

/// Compact mode restores the terse one-line popup that chibipop used.
///
/// Two dictionaries have four glosses. Compact uses one line per dictionary.
/// Roomy uses one line per gloss.
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

/// This test checks example visibility and element count for two dictionaries.
///
/// One example block uses an ASCII hook and the other uses a Japanese hook.
/// Before the fix, the popup drew neither branch on real data because all nodes
/// lacked classification. The option had no effect. Jitendex examples also used
/// a six-name drop list that omitted 明鏡's key.
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

/// Attribution visibility is independent of example visibility. This test checks
/// all four combinations with one parsed document.
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

/// This seam uses one document and one parse. The panel hides the example, and the
/// card shows it.
///
/// The popup gets its filter from config in `build_elements`. The card renderer
/// uses `RoleFilter::CARD`, which no option changes. Unclassified nodes gave
/// neither filter enough information to separate examples from glosses.
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

/// `data.content = "part-of-speech-info"` maps to `Role::PartOfSpeech`. Jitendex
/// writes it on 48 776 nodes. The popup hides it because the card `pos` field
/// already prints the label. The option controls this choice.
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

/// When images are off, the scene removes the image and keeps its `alt` text.
/// The text fills the image's place.
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

/// When style output is off, the entry uses the theme font and colors. A node's
/// color and box declarations have no effect.
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
    assert_eq!(theme.headword_size, ink.size, "its size too, capped at the headword");
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

/// This test applies the style gate to a third resolved style record.
///
/// `listStyleType` is a declaration. Jitendex uses it for ①②③ sense numbers.
/// With style output off, the browser's marker replaces the styled marker. A
/// styled dictionary then matches an unstyled dictionary.
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

/// The height cap and scrollbar still work when an option increases entry height.
///
/// Compact uses one line for this tree, while roomy uses three. The two heights
/// meet one cap. The view uses the smaller of content height and cap. The taller
/// entry has more scroll. `max_scroll` and the scrollbar use these values.
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
                selection: None,
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

/// A dictionary block from raw glossary data and its stylesheet.
///
/// `dict::sheet` compiles the sheet and applies it between parse and the tree
/// cache (`SqliteDictionary::entries`). This helper calls both functions. The
/// renderer then receives resolved style records and does not know about CSS.
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

/// Return every box that the scene draws, with block and inline boxes, in draw
/// order.
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

/// 明鏡国語辞典 第三版 stores its box properties only in its stylesheet. This
/// test uses its `span[data-sc-fbox]` rule and no inline `style`.
/// Thirteen of 52 structured-content dictionaries have this shape.
///
/// `body_size` is 15. The rule's `font-size: 0.8em` makes the element 12.
/// Each box length uses that element size. `padding: 0.1em` is 1.2,
/// `border-width: 0.05em` is 0.6, and `border-radius: 0.2em` is 2.4.
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
    // `margin-inline-end` is a logical property that this build does not map.
    // `border-color: var(--text-color)` is a custom property that it cannot
    // substitute. The parser drops and counts both values. The border still draws
    // because the initial CSS `border-color` is `currentColor`, and the box pass
    // seeds it from the resolved color.
    assert_eq!(Edges::all(0.0), style.margin, "a logical margin is dropped");
    assert_eq!(Theme::dark().body_text, style.border_color, "currentColor stands");
    assert_eq!(None, style.background);
}

/// 字通 uses a descendant selector on a CJK `data` key:
/// `[data-sc-h3] span[data-sc筆画]`. This test uses its rule verbatim.
///
/// The same span outside `data-sc-h3` must draw no box. The test therefore
/// checks the ancestor condition and the box.
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
    // An absolute `px` scales from Yomitan's 14px base, so it scales with the
    // panel. [`css_len`] applies this rule. The element is 12px, so `4px` is
    // 12 * 4 / 14.
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
    // `background` is a shorthand that this build does not map. It maps only
    // `background-color`, so the parser drops and counts the value.
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

/// 字通 wraps every entry in `span` nodes and makes its section a block only in its
/// stylesheet: `[data-sc-section] { display: block }`. This test uses the 相与
/// entry verbatim, minus its link markup.
///
/// In a browser, the block section starts under the inline heading. Before this
/// test, the walk read only the schema tag, so the section's first text joined the
/// heading's line: `【相与】そうよ 親しみあう。`.
#[test]
fn a_span_that_a_stylesheet_displays_as_block_opens_its_own_line() {
    let s = laid_out(
        &card_with(vec![css_tree(
            "字通",
            &sc(concat!(
                r#"[{"tag":"span","content":[{"tag":"span","content":[{"tag":"span","#,
                r#""content":"【相与】そうよ","data":{"main_title":""}}],"data":{"h3":""}},"#,
                r#"{"tag":"span","content":[{"tag":"span","content":"親しみあう。"},"#,
                r#"{"tag":"div","content":"「相」の項目を見る。","data":{"p":""}}],"#,
                r#""data":{"section":"","description":""}}]}]"#,
            )),
            "[data-sc-section] { display: block; margin: 0em 1.5em }",
        )]),
        400.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s);
    let text: Vec<&str> = gloss.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(
        vec!["【相与】そうよ", "親しみあう。", "「相」の項目を見る。"],
        text,
        "the block section starts its own paragraph under the heading"
    );
    let indent = 1.5 * Theme::dark().body_size;
    assert_eq!(s.origin, gloss[0].pen.0, "the inline heading keeps the margin");
    assert_eq!(s.origin + indent, gloss[1].pen.0, "the block pays its margin-left");
    assert_eq!(s.origin + indent, gloss[2].pen.0, "and so does the paragraph inside it");
}

/// A stylesheet can turn a schema block back into an inline: `div { display: inline }`.
/// The text of that `div` then joins the line around it, as it does in a browser.
#[test]
fn a_div_that_a_stylesheet_displays_as_inline_joins_its_line() {
    let s = laid_out(
        &card_with(vec![css_tree(
            "x",
            &sc(r#"["see ",{"tag":"div","data":{"ref":""},"content":"also"}," here"]"#),
            "div[data-sc-ref] { display: inline }",
        )]),
        400.0,
        4000.0,
        false,
        false,
    );
    let text: Vec<&str> = bodies(&s).iter().map(|e| e.text.as_str()).collect();
    assert_eq!(vec!["see also here"], text);
}

/// A cross-reference must look like one. HTML colors an `a` before any dictionary
/// rule, so the walk seeds a followable link with the theme accent. A dictionary
/// rule on a descendant still wins, as it does in a browser.
///
/// 字通 removes the underline with `a { text-decoration: none }`. The color is the
/// only cue that survives, so the test uses that rule.
#[test]
fn a_followable_link_takes_the_accent_color() {
    let theme = Theme::dark();
    let s = laid_out(
        &card_with(vec![css_tree(
            "字通",
            &sc(concat!(
                r#"["「",{"tag":"a","href":"?query=相&wildcards=off","content":"相"},"#,
                r#""」の",{"tag":"a","href":"?query=相&wildcards=off","content":"#,
                r##"[{"tag":"span","style":{"color":"#a75a23"},"content":"項目"},"を見る"]},"##,
                r#""。",{"tag":"a","href":"javascript:x","content":"dead"}]"#,
            )),
            "a { text-decoration: none }",
        )]),
        400.0,
        4000.0,
        false,
        false,
    );
    let gloss = bodies(&s)[0];
    let colors: Vec<(&str, Rgb)> = gloss
        .spans
        .iter()
        .map(|sp| (&gloss.text[sp.at as usize..(sp.at + sp.len) as usize], sp.color))
        .collect();
    assert_eq!(
        vec![
            ("「", theme.body_text),
            ("相", theme.accent),
            ("」の", theme.body_text),
            ("項目", (0xa7, 0x5a, 0x23)),
            ("を見る", theme.accent),
            ("。dead", theme.body_text),
        ],
        colors,
        "a link is accent, a styled descendant keeps its color, a dead link is body text"
    );
}

/// A dictionary can ask for gloss text larger than the panel headword. 字通 sets
/// its own heading to `1.5em` bold, so at a 15px body it outgrew the 20px
/// headword above it and read as a second, larger headword.
///
/// The panel caps a resolved gloss size at the headword size. A size under the
/// cap keeps its value. A length inside a capped node resolves against the capped
/// size, because that is the node's own em.
#[test]
fn a_gloss_font_size_never_exceeds_the_headword_size() {
    let theme = Theme::dark();
    let s = laid_out(
        &card_with(vec![css_tree(
            "字通",
            &sc(concat!(
                r#"[{"tag":"span","data":{"main_title":""},"content":["#,
                r#""【相与】",{"tag":"span","style":{"fontSize":"0.5em"},"content":"そうよ"}]},"#,
                r#"{"tag":"span","style":{"fontSize":"1.2em"},"content":"親"}]"#,
            )),
            "span[data-sc-main_title] { font-size: 1.5em }",
        )]),
        400.0,
        4000.0,
        false,
        false,
    );
    let gloss = s
        .elems
        .iter()
        .find(|e| e.text.starts_with("【相与】"))
        .unwrap_or_else(|| panic!("the heading run: {:?}", texts(&s)));
    let sizes: Vec<(&str, f32)> = gloss
        .spans
        .iter()
        .map(|sp| (&gloss.text[sp.at as usize..(sp.at + sp.len) as usize], sp.size))
        .collect();
    assert_eq!(
        vec![
            ("【相与】", theme.headword_size),
            ("そうよ", theme.headword_size / 2.0),
            ("親", 1.2 * theme.body_size),
        ],
        sizes,
        "1.5em caps at the headword, 0.5em of the capped em, 1.2em untouched"
    );
}

/// Jitendex uses a CSS-only pill on 48 776 nodes. The rule is
/// `span[data-sc-class="tag"]`, and the entry has no inline box property.
///
/// The test uses `misc-info`, not `part-of-speech-info`. A part-of-speech pill
/// moves to the card labels (`GlossDoc::is_part_of_speech`). Five other tags
/// remain inline: `misc-info`, `field-info`, `dialect-info`,
/// `lang-source-wasei`, and `forms-label`. All use the same
/// `data-sc-class="tag"` rule.
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
    // One entry has one style. The pill has `data.content`, but its inline tag
    // makes the resolved box belong to the run, not the paragraph. It therefore
    // reaches the scene once. Before this fix, it reached the scene as both a
    // `block_box` and an `inline_box`. A bin that called `SceneElem::boxes()` painted
    // it twice. See `a_pill_carrying_a_content_marker_draws_one_box_and_not_two`.
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
    // The text part of the same record reaches the span through the same fold.
    let pill = s.elems.iter().find(|e| e.text.contains("abbr.")).expect("the pill run");
    assert_eq!(12.0, pill.font_size, "font-size: 0.8em");
    assert_eq!(BOLD_WEIGHT, pill.weight, "font-weight: bold");
}

/// Jitendex uses `div[data-sc-class="extra-box"]` for its other box. This rule
/// comes from the archive `styles.css` and contains `rem` and unreadable
/// `calc()`. It appears in 101 360 of 435 448 entries.
///
/// `border-width` uses `calc(3em / var(--font-size-no-units, 14))`, which this
/// build cannot read. The left `solid` style therefore has zero used width.
/// Margin and padding supply the box's space.
///
/// A real extra-box has two `div` children and no text. Earlier code flushed the
/// empty paragraph and removed its box. The box now contains every paragraph
/// that its block emits, so it appears once around both children.
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

    // The real shape has `div` children and no text of its own.
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

/// The style option controls stylesheet and inline declarations through one
/// resolved style record. This test rejects a second switch.
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
                selection: None,
            },
            &mut m,
        )
        .expect("FakeMeasure never refuses a run")
    };
    assert_eq!(2, drawn_boxes(&laid(true)).len(), "one box from CSS, one from inline");
    assert!(drawn_boxes(&laid(false)).is_empty(), "off means neither applies");
}

/// Jitendex has two list rules. The outer sense-group list uses `＊`, and its
/// glossary list uses `none`. Both rules are CSS-only. The second uses native
/// `&` child-rule syntax.
///
/// The fixture follows Jitendex's tree: `ul[sense-groups]` contains
/// `li[sense-group]`, which contains an `ol` of `li[sense]`. Each sense contains
/// `ul[glossary]` with gloss text.
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
    // The stylesheet gives `＊` to the outer list, and the item gives `①`. The
    // stylesheet suppresses the glossary marker, which would otherwise be `•`.
    assert_eq!(vec![vec!["＊ ", "① "]], markers, "{:?}", texts(&s));
}

/// A reader of あくどい saw the gap between a sense number and its gloss. Jitendex
/// sets `padding-left: 0.25em` on the glossary list. It is the only list in the
/// 97-archive corpus with padding. A browser uses that padding instead of the
/// default gutter.
///
/// The sense item and glossary list each set `padding-left: 0.25em`. The
/// glossary value replaces its default level. If code also adds
/// [`LIST_INDENT_EM`], it would leave 1.9em instead of the requested 0.5em.
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

    // Two default gutter levels come from the sense-groups list and bare `ol`.
    // Neither list declares padding.
    // Three rules declare `padding-left: 0.25em`: one for each sense-group item,
    // sense item, and glossary list. The glossary list *replaced* its own level.
    assert_eq!(s.origin + 2.0 * LEVEL + 0.75 * BOX_EM, item.pen.0, "{:?}", item.marker);
    // The sense number still hangs at the `ol`'s content edge.
    assert_eq!(2, item.marker.len(), "the outer ＊ and the sense's ①");
    let sense = &item.marker[1];
    assert_eq!(
        s.origin + 2.0 * LEVEL + 0.25 * BOX_EM - marker_w("① "),
        item.pen.0 + sense.x,
    );
    // This value shows the defect: two other paddings separate the marker box
    // from the first gloss glyph.
    assert_eq!(0.5 * BOX_EM, -sense.x - marker_w("① "));
}

/// Ruby can have a reading without a base. 岩波国語辞典　第八版 writes
/// 円周率 as `▽「π（<ruby><rt>パイ</rt></ruby>）」で表す。`. The `<ruby>` has
/// one `<rt>` child. CSS Ruby gives it an anonymous empty base, so the reading
/// remains visible. Yomitan has no `ruby` or `rt` rule, so the browser default
/// applies. If the reading were absent, the browser would leave empty parentheses.
///
/// This test checks the reading position. It must sit at the pen where its base
/// would start, not above the next word.
#[test]
fn a_reading_with_no_base_stands_at_the_pen_its_base_would_have_taken() {
    let unit = Theme::dark().body_size * ADVANCE;
    let base_line = Theme::dark().body_size * LINE_H;
    let read_line = Theme::dark().body_size * RUBY_RATIO * LINE_H;
    // The fake sets each line's ascent from the tallest span size. Its ascent share is
    // `1 / LINE_H`.
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
    // Four characters precede `<ruby>`, so the anonymous base starts four units in.
    // Two half-size kana span one base width.
    assert_eq!(4.0 * unit, read.x, "flush at the pen, not centred on nothing");
    assert_eq!(2.0 * unit * RUBY_RATIO, read.w);
    assert_eq!(read_line, read.h);
    assert_eq!(0.0, read.y, "in the room its own filler bought");
}

/// Onomatoproject uses the same shape when the source lacks a kanji.
/// Its example for ちゃらちゃら is
/// `お<ruby>父<rt>とう</rt></ruby>さんは<ruby>""<rt>きら</rt></ruby>いだ！`.
/// The bytes omit 嫌, but a browser still reads `きらいだ`. The panel must not
/// render less text than a browser for the same bytes.
///
/// The archive supplies adjacent prose because *no dropped text* is a
/// containment check. A fragment with only `きら` would pass by accident. Both
/// readings have fixed positions, so a reading over 父 fails.
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
    // Six characters precede 父. It is one unit wide, and two half-size kana span
    // one unit, so とう covers its base.
    // さんは follows. The empty base starts at ten units, not over 父.
    assert_eq!(6.0 * unit, gloss.ruby[0].x);
    assert_eq!(10.0 * unit, gloss.ruby[1].x, "over its own hole, not over 父");
}


// ---- a right-aligned line's own ink ----

/// 小学館例解学習国語 第十二版 writes this poet line in its 百人一首 appendix.
/// The line uses `div[data-sc-読み人]` with `text-align: right` and five
/// per-character `<ruby>` elements. The card and appendix wrappers provide the
/// selector context.
///
/// The corpus marks this shape as *no horizontal overflow*. A 380.41px element
/// starts at 362.50px in a 424px panel, so 318.91px extends past the right edge.
/// The line's ink is five kanji, 41.25px. The other 339px is alignment slack.
/// [`place_ruby`] added that slack to [`RubyBox::x`] and then to `rect.x`, which
/// also counted it as width.
///
/// `body_size` is 15. The appendix `font-size: 1.1em` makes the card size 16.5.
/// Each kanji advances 8.25, so five measure 41.25. Panel padding is 12, and
/// card `padding: 0.5em` adds 8.25. The pen is 20.25 and the wrap is
/// `400 - 16.5 = 383.50`. Right alignment starts at
/// `20.25 + 383.50 - 41.25 = 362.50` and ends at 403.75, inside panel edge 412.
///
/// This test checks two numbers. [`RubyBox::x`] is run-relative, so the last
/// reading starts at `pen.0 + r.x`.
///
/// [`place_ruby`]: super::ruby::place_ruby
#[test]
fn a_right_aligned_lines_ink_box_counts_its_alignment_slack_once() {
    let p = card_with(vec![css_tree(
        "小学館例解学習国語 第十二版",
        &sc(concat!(
            r#"{"tag":"span","data":{"付録":""},"content":["#,
            r#"{"tag":"span","data":{"body":""},"content":["#,
            r#"{"tag":"div","data":{"class":"恋","句":"","恋":""},"content":["#,
            r#"{"tag":"div","data":{"読み人":""},"content":["#,
            r#"{"tag":"ruby","data":{"ruby":""},"content":[{"tag":"span","data":{"rb":""},"#,
            r#""content":[{"tag":"span","content":"柿"}]},{"tag":"rt","data":{"rt":""},"#,
            r#""content":[{"tag":"span","content":"かきの"}]}]},"#,
            r#"{"tag":"ruby","data":{"ruby":""},"content":[{"tag":"span","data":{"rb":""},"#,
            r#""content":[{"tag":"span","content":"本"}]},{"tag":"rt","data":{"rt":""},"#,
            r#""content":[{"tag":"span","content":"もとの"}]}]},"#,
            r#"{"tag":"ruby","data":{"ruby":""},"content":[{"tag":"span","data":{"rb":""},"#,
            r#""content":[{"tag":"span","content":"人"}]},{"tag":"rt","data":{"rt":""},"#,
            r#""content":[{"tag":"span","content":"ひと"}]}]},"#,
            r#"{"tag":"ruby","data":{"ruby":""},"content":[{"tag":"span","data":{"rb":""},"#,
            r#""content":[{"tag":"span","content":"麻"}]},{"tag":"rt","data":{"rt":""},"#,
            r#""content":[{"tag":"span","content":"ま"}]}]},"#,
            r#"{"tag":"ruby","data":{"ruby":""},"content":[{"tag":"span","data":{"rb":""},"#,
            r#""content":[{"tag":"span","content":"呂"}]},{"tag":"rt","data":{"rt":""},"#,
            r#""content":[{"tag":"span","content":"ろ"}]}]}"#,
            r#"]}]}]}]}"#,
        )),
        "rt {
             font-size: 0.5em;
             font-weight: normal;
         }
         [data-sc付録] [data-sc-body] {
             font-size: 1.1em;
         }
         [data-sc付録] [data-sc句] {
             display: block;
             padding: 0.5em;
         }
         [data-sc付録] [data-sc読み人] {
             display: block;
             margin-inline-start: 1.5em;
             margin-inline-end: 3em;
             text-align: right;
         }",
    )]);
    let s = laid_out(&p, 424.0, 4000.0, false, false);
    let name = s.elems.iter().find(|e| e.text.starts_with('柿')).expect("the poet's line");

    // The card em and the advance of one kanji.
    let em = BOX_EM * 1.1;
    let kanji = em * ADVANCE;
    assert_eq!(16.5, em);
    assert_eq!(Align::Trailing, name.align, "text-align: right");
    assert_eq!(5, name.ruby.len(), "one reading per character");
    assert_eq!(s.origin + em / 2.0, name.pen.0, "the panel's padding plus the card's");
    assert_eq!(400.0 - em, name.wrap_w, "the content column less the card's two paddings");

    // The line ends at the card content edge. Its box covers only five kanji because
    // every reading is narrower than its base.
    assert_eq!(362.5, name.rect.x, "the aligned line's own leading edge");
    assert_eq!(5.0 * kanji, name.rect.w, "five kanji of ink, not five of slack");
    assert_eq!(41.25, name.rect.w);
    assert_eq!(403.75, name.rect.x + name.rect.w, "flush with the card's content edge");
    assert!(
        name.rect.x + name.rect.w <= 424.0 - s.origin,
        "and inside the panel: {} against {}",
        name.rect.x + name.rect.w,
        424.0 - s.origin,
    );

    // A bin draws the last reading from the element pen plus its run-relative offset.
    let last = name.ruby.last().expect("the reading over 呂");
    assert_eq!("ろ", last.text);
    assert_eq!(kanji * RUBY_RATIO / 2.0, last.w, "half a base, halved again by the rt rule");
    assert_eq!(398.59375, name.pen.0 + last.x, "the pen plus the run-relative offset");
    assert!(
        name.pen.0 + last.x + last.w <= name.rect.x + name.rect.w,
        "so the furigana is inside the box that sized it: {} against {}",
        name.pen.0 + last.x + last.w,
        name.rect.x + name.rect.w,
    );
}

// ---- a table whose children are not rows ----

/// 旺文社漢字典 第四版 stores its radical index in a table. This fixture uses
/// row 94, `灬`, and its first two stroke-count groups. It also uses the archive's
/// only declaration in its 25 303-byte `styles.css`.
///
/// The table children are `span[data-sc-IndexSubG]`, one per stroke-count group.
/// No `tr` or `td` exists. The stylesheet maps these spans to table rows with
/// `display: table-row`. CSS 2.1 section 17.2 defines this map.
/// chibipop reads `display` only as a line-break decision
/// (`GlossDoc::is_block`), and a table-internal type keeps the tag rule. The
/// table walk therefore receives content outside cells and applies the
/// anonymous-box rule from CSS 2.1 section 17.2.1.
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

/// A run of non-cell children becomes **one** anonymous cell in **one** anonymous row.
/// The index keeps dictionary order. One cell per child would create two columns
/// for these groups. The full index has 19 such columns, each about 6px wide.
/// Their reading order would turn 90 degrees, and right strips would leave the
/// panel. Adjacent strips would also overlap.
///
/// The stylesheet `font-size: 1.4em` asks for 21px at [`BOX_EM`]. The panel caps
/// gloss text at the 20px headword size, so a kanji advances 10px, and a
/// stroke-count number advances 7.5px. Two numbers and three kanji measure 45px
/// on one line with the capped height.
#[test]
fn a_table_whose_children_are_not_rows_becomes_one_cell_and_not_one_column_each() {
    let s = laid_out(&radical_index(), 424.0, 4000.0, false, false);
    assert_eq!(1, grid_cells(&s).len(), "one anonymous cell, not one per group");

    let index = s
        .elems
        .iter()
        .find(|e| e.kind == ElemKind::Text && e.text == "⓪火②灰灯")
        .unwrap_or_else(|| panic!("the index reads in document order: {:?}", texts(&s)));
    assert_eq!(2.0 * BOX_EM * ADVANCE + 3.0 * 20.0 * ADVANCE, index.rect.w);
    assert_eq!(20.0 * LINE_H, index.rect.h, "one line, as tall as its capped kanji");

    // The grid has one column and no other. A table without declared width shrinks
    // to fit. Nineteen groups therefore no longer request nineteen tracks that need
    // size changes.
    let table = find(&s, ElemKind::Table);
    assert_eq!((index.rect.x, index.rect.y, index.rect.w), (table.rect.x, table.rect.y, table.rect.w));
}

/// An anonymous cell is not `td`, so it has none of Yomitan's cell defaults.
/// Those defaults use `.gloss-sc-th, .gloss-sc-td`, which no span matches. CSS
/// 2.1 section 17.2.1 gives anonymous boxes initial values for properties they do
/// not inherit.
///
/// If code applied cell defaults, it would draw 19 boxes that a browser leaves
/// absent. It would also add 0.25em padding per side and reduce each group to a
/// 1px width.
#[test]
fn an_anonymous_cell_draws_no_border_and_pays_no_padding() {
    let s = laid_out(&radical_index(), 424.0, 4000.0, false, false);
    let cell = grid_cells(&s)[0];
    assert_eq!(BoxStyle::default(), block_box(cell).style);
    // Content starts at the cell top-left because the anonymous cell has no rule or padding.
    assert_eq!((cell.rect.x, cell.rect.y), cell.pen);
}

/// The run is a sequence of consecutive siblings, not every stray child in a
/// row. CSS 2.1 section 17.2.1 rule 2.3 uses this phrase.
/// "and all consecutive siblings of C that are not 'table-cell' boxes".
/// A written cell closes that anonymous group. Content after it opens another, so
/// the row has three columns in order.
#[test]
fn a_written_cell_closes_the_anonymous_cell_the_content_before_it_opened() {
    let row = r#"{"tag":"tr","content":["a","b",{"tag":"td","content":"c"},"d"]}"#;
    let s = gridded(&table(&[row.to_string()]), 424.0);

    assert_eq!(3, grid_cells(&s).len());
    let cells: Vec<&str> = grid_text(&s).iter().map(|e| e.text.as_str()).collect();
    assert_eq!(vec!["ab", "c", "d"], cells, "the two strays before the `td` share one cell");
}

// ---- a picture wider than its column ----

/// 現代国語例解辞典　第五版 has a コラム panel. This fixture uses row 542,
/// `上がる`, and its first `tr` inside the two ancestor boxes named by the
/// archive. The archive has no `styles.css`, so every declaration belongs to the
/// entry.
///
/// Each `img` has no `alt`, and this fixture has no PNG. `image_size` therefore
/// uses each node's `width` and `height`: `7.95em` and `12.72em` wide, and `8em`
/// tall, at [`BOX_EM`].
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

/// Pair each image with the nearest earlier `Block` element. The archive wraps
/// each picture in `div{margin: 0.5em}`. A block box precedes its content, so the
/// nearest block gives the correct parent structure.
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

/// Each コラム image has a fitted box as `(w, h)`. The entry declares
/// `7.95em` and `12.72em` width and `8em` height. At [`BOX_EM`], these are
/// 119.25 and 190.80. The columns leave 78.94045 and 130.14372, and one factor
/// per image reduces both axes to those values.
const FITTED: [(f32, f32); 2] = [(78.94045, 79.436935), (130.14372, 81.8514)];

/// A declared width is a demand. The image uses the room that its block gets.
/// `Pass::columns` narrows a column and rewraps text. An image has no text to
/// rewrap, so earlier code drew its full width over the next cell and outside
/// the panel. Yomitan applies `max-width: 100%` to both
/// `.gloss-image-link` and `.gloss-image-container` before it clips the rest.
///
/// The sweep found no overlap or outside pixels after the pass fit each image.
/// Before that, images overlapped by `15.67`px, and the second reached `5.26`px
/// outside the panel: `238.46 + 190.80 - 424.00`.
#[test]
fn a_picture_wider_than_its_column_is_fitted_to_it_instead_of_drawn_over_the_next_cell() {
    let s = laid_out(&column_panel(), 424.0, 4000.0, false, false);
    let pictures = pictures_in_blocks(&s);
    assert_eq!(2, pictures.len(), "one illustration per picture cell");

    // One factor scales both axes. This build has no clip, and both painters stretch
    // an asset into its rect. A width-only change would squash a scanned image.
    let boxes: Vec<(f32, f32)> = pictures.iter().map(|(p, _)| (p.rect.w, p.rect.h)).collect();
    assert_eq!(FITTED.to_vec(), boxes);
    // The image width equals its block room at both edges. It cannot reach the next
    // cell or the panel edge.
    for (picture, block) in &pictures {
        assert_eq!((block.rect.x, block.rect.w), (picture.rect.x, picture.rect.w));
    }

    let (first, second) = (pictures[0].0.rect, pictures[1].0.rect);
    assert_eq!(0.0, (first.x + first.w - second.x).max(0.0), "no picture over its neighbour");
    assert_eq!(0.0, (second.x + second.w - 424.0).max(0.0), "and none outside the panel");
}

/// The pass fits both reservation and paint. [`measure_images`] and
/// [`place_images`] call one function. A smaller drawn rect alone would leave
/// each コラム row with the line that a full-size image requested. `FakeMeasure`
/// makes a line twice its tallest span, and an image riser uses its own height.
/// An `8em` image would therefore make a 240px paragraph before the column fit.
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

/// 岩波国語辞典　第八版 uses a cross-reference with two readings:
/// `<ruby>七色<rt>なないろ</rt><rt>しちしょく</rt></ruby>`. HTML Ruby treats
/// each `rt` after a base as a separate annotation level. Gecko uses a tabular
/// layout, but Blink and WebKit do not. All three engines render each `rt`.
///
/// The second reading uses a second band above the first. Both boxes use one
/// base, and the line reserves both bands. This prevents overlap above and lets
/// each platform bin get the same line after it re-measures the spans.
#[test]
fn a_base_with_two_readings_stacks_the_second_band_over_the_first() {
    let unit = Theme::dark().body_size * ADVANCE;
    let read_unit = unit * RUBY_RATIO;
    let base_line = Theme::dark().body_size * LINE_H;
    let read_line = Theme::dark().body_size * RUBY_RATIO * LINE_H;
    // `FakeMeasure` sets line ascent to the tallest span size, so the ascent fraction
    // is `1 / LINE_H`.
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
    // Each reading adds one invisible character, as [`RUBY_FILLER`] requires: two
    // bands, two fillers, and one base.
    assert_eq!("\u{8679}\u{306e}\u{4e03}\u{8272}\u{2060}\u{2060}\u{3002}", gloss.text);
    assert_eq!(1, gloss.lines, "and neither band broke a line");
    // The line reserves both bands. Each platform bin therefore gets the same scene
    // after it re-measures the spans.
    assert_eq!(base_line + 2.0 * read_line / ascent, gloss.rect.h);

    // 虹の comes before the two-unit base 七色.
    let (base_x, base_w) = (2.0 * unit, 2.0 * unit);
    let (near, far) = (&gloss.ruby[0], &gloss.ruby[1]);

    assert_eq!(4.0 * read_unit, near.w, "four half-size kana");
    assert_eq!(5.0 * read_unit, far.w, "five");
    assert_eq!(read_line, near.h);
    assert_eq!(read_line, far.h);

    // Both bands center over one base. なないろ matches the base width, while
    // しちしょく extends half a reading unit on each side.
    assert_eq!(base_x + (base_w - near.w) / 2.0, near.x);
    assert_eq!(base_x + (base_w - far.w) / 2.0, far.x);

    // The lower band ends at the base ink top. The upper band ends at the lower
    // band. Together they fill the reserved line space.
    let base_ink = ascent * gloss.rect.h - ascent * base_line;
    assert_eq!(base_ink, near.y + near.h, "なないろ rests on 七色");
    assert_eq!(near.y, far.y + far.h, "しちしょく rests on なないろ");
    assert_eq!(0.0, far.y, "and the pair reaches the top of the room it bought");
}

// ---- a reading at the end of a line ----

/// 岩波国語辞典　第八版 row 31513 places `宿酔` below
/// `しゅくすい・ふつかよい`. The filler puts `宿` at the end of line one.
/// Eleven kana annotate two kanji, but the base splits at the break. If the
/// reading used the character left, it would put 2.38px outside the panel.
///
/// One bin clips the kana. Another paints them outside the rounded rect. Both
/// hide part of a number and can change its sense.
///
/// Yomitan has no `ruby` or `rt` rule for glossary content, so browser defaults
/// control the result. The browser keeps each kana inside the content box. CSS
/// Ruby Level 1 §5.2 lets a user agent move a line-edge annotation to that edge.
///
/// Chromium 151 did this in a measurement. At mid-line, the `rt` box extended
/// 4.00px left and 4.02px right of the `ruby` box. At line end with filler 25,
/// the `rt` box covered 371.28 to 393.78px. The `ruby` box covered 375.02 to
/// 393.77px. The annotation therefore moved left of its base and stopped at the
/// line edge.
///
/// The test fixes both boxes to the content column, not the panel. Browser
/// behavior keeps the annotation in that column. A change to only the element
/// box cannot fix the error. The kana would keep their old positions.
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
    // The column is `424 - 2 x 12 = 400`. Its right edge is here.
    let edge = s.origin + s.content_w;
    assert_eq!(412.0, edge, "the content column's own right edge");

    // Fifty-three 7.5px characters fill 397.5px. The 52 filler kana and 宿 fill
    // line one, and 酔 starts line two.
    assert_eq!(2, gloss.lines);
    assert_eq!(11.0 * read_unit, read.w, "eleven kana at half the base's size");
    assert_eq!(41.25, read.w, "which is 41.25 px");
    assert_eq!(53.0 * unit, 397.5, "and the line before it ends at 397.5");

    // If centered over the character left, the reading would cover 385.12 to
    // 426.38px. The correction moves its end to the content edge.
    assert_eq!(edge, gloss.pen.0 + read.x + read.w, "no kana outside the column");
    assert!(
        gloss.pen.0 + read.x >= s.origin,
        "and none off its left edge either: {}",
        gloss.pen.0 + read.x,
    );
    // The ink box expands to cover the reading. It therefore ends at the same edge.
    assert_eq!(edge, gloss.rect.x + gloss.rect.w, "the ink box ends there too");
}

// ---- pitch ----

/// This accent has no markers. It represents 96% of the corpus.
fn accent(fall: u32) -> crate::dict::pitch::Accent {
    crate::dict::pitch::Accent {
        position: crate::dict::pitch::Position::Downstep(fall),
        nasal: Vec::new(),
        devoice: Vec::new(),
        tags: Vec::new(),
    }
}

/// One pitch row with an accent and its source dictionaries.
fn pitch_row(fall: u32, dicts: &[&str]) -> crate::present::PitchRow {
    crate::present::PitchRow {
        accent: accent(fall),
        dicts: dicts.iter().map(|d| d.to_string()).collect(),
    }
}

/// Build a card with the specified pitch rows.
fn card_with_pitch(reading: &str, pitch: Vec<crate::present::PitchRow>) -> Presentation {
    let card = Card {
        written: Some("雑談".into()),
        reading: Some(reading.into()),
        pos: vec![],
        freq: None,
        blocks: vec![block("Jitendex", &["chatting"])],
        match_len: 2,
        pitch,
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    }
}

/// Return every pitch element in scene order.
fn pitch_elems(s: &PopupScene) -> Vec<&SceneElem> {
    s.elems.iter().filter(|e| e.kind == ElemKind::Pitch).collect()
}

/// Return the advance that `FakeMeasure` gives one reading kana.
const READING_UNIT: f32 = 15.0 * ADVANCE;

/// Heiban covers 48.0% of corpus accents. The first mora is low, and later moras
/// are high. No tick appears because the rise continues into the next particle.
#[test]
fn a_heiban_accent_overlines_every_mora_but_the_first_and_draws_no_tick() {
    let p = card_with_pitch("ざつだん", vec![pitch_row(0, &["Jitendex"])]);
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let rows = pitch_elems(&s);
    assert_eq!(1, rows.len(), "one accent, one row: {:?}", texts(&s));
    let row = rows[0];
    assert_eq!("ざつだん\u{2003}Jitendex", row.text);

    // Two headword kana at 20px use a 40px line. The pitch row starts after a 4px
    // gap and replaces the plain reading line. `origin` is the 12px padding.
    assert_eq!((12.0, 12.0 + 40.0 + 4.0), row.pen);
    assert_eq!(LINE_GAP, row.top_gap);

    // Three adjacent boxes form one overline. Each covers one high mora. Heiban has
    // no downstep, so no box has a tick.
    assert_eq!(3, row.inline_boxes.len(), "{:?}", row.inline_boxes);
    for (i, drawn) in row.inline_boxes.iter().enumerate() {
        let mora = i as f32 + 1.0;
        assert_eq!(12.0 + mora * READING_UNIT, drawn.rect.x, "mora {mora}");
        assert_eq!(READING_UNIT, drawn.rect.w, "mora {mora}");
        assert_eq!(row.pen.1, drawn.rect.y, "on the row's own line");
        assert_eq!(BorderStyle::Solid, drawn.style.border_style.top);
        assert_eq!(
            BorderStyle::None,
            drawn.style.border_style.right,
            "heiban draws no downstep tick"
        );
    }
}

/// Atamadaka marks only the first mora as high. The tick is at its right edge.
#[test]
fn an_atamadaka_accent_overlines_the_first_mora_and_ticks_after_it() {
    let p = card_with_pitch("ざつだん", vec![pitch_row(1, &["Jitendex"])]);
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let row = pitch_elems(&s)[0];
    assert_eq!(1, row.inline_boxes.len());
    let drawn = &row.inline_boxes[0];
    assert_eq!(12.0, drawn.rect.x, "the first mora starts at the content edge");
    assert_eq!(READING_UNIT, drawn.rect.w);
    assert_eq!(BorderStyle::Solid, drawn.style.border_style.top);
    assert_eq!(BorderStyle::Solid, drawn.style.border_style.right, "the tick");
    assert_eq!(PITCH_MARK, drawn.style.border.right);
}

/// Nakadaka marks the moras after the first and before the downstep as high. The tick
/// follows the last high mora, not the word end.
#[test]
fn a_nakadaka_accent_ticks_inside_the_word() {
    let p = card_with_pitch("ざつだん", vec![pitch_row(3, &["Jitendex"])]);
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let boxes = &pitch_elems(&s)[0].inline_boxes;
    assert_eq!(2, boxes.len(), "moras two and three: {boxes:?}");
    assert_eq!(12.0 + READING_UNIT, boxes[0].rect.x);
    assert_eq!(BorderStyle::None, boxes[0].style.border_style.right);
    assert_eq!(12.0 + 2.0 * READING_UNIT, boxes[1].rect.x);
    assert_eq!(BorderStyle::Solid, boxes[1].style.border_style.right);
}

/// A mora has one or two characters. Its mark must cover both. A probe of only
/// the first character would leave half of `きょ` unmarked.
#[test]
fn a_two_character_mora_is_marked_by_one_box_spanning_both_of_them() {
    let p = card_with_pitch("きょうと", vec![pitch_row(0, &["Jitendex"])]);
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let boxes = &pitch_elems(&s)[0].inline_boxes;
    // Heiban marks the last two of the three moras きょ, う, and と.
    assert_eq!(2, boxes.len(), "{boxes:?}");
    assert_eq!(12.0 + 2.0 * READING_UNIT, boxes[0].rect.x, "う, after a two-unit mora");
    assert_eq!(READING_UNIT, boxes[0].rect.w);

    // Atamadaka marks `きょ` itself.
    let p = card_with_pitch("きょうと", vec![pitch_row(1, &["Jitendex"])]);
    let s = laid_out(&p, 480.0, 800.0, false, false);
    let boxes = &pitch_elems(&s)[0].inline_boxes;
    assert_eq!(1, boxes.len());
    assert_eq!(12.0, boxes[0].rect.x);
    assert_eq!(2.0 * READING_UNIT, boxes[0].rect.w, "both characters of きょ");
}

/// Two dictionaries with different accents produce two rows. `present::build`
/// orders them from the pitch list.
#[test]
fn two_accents_draw_two_pitch_rows_stacked_under_the_reading() {
    let p = card_with_pitch(
        "ざつだん",
        vec![pitch_row(0, &["Jitendex"]), pitch_row(3, &["NHK"])],
    );
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let rows = pitch_elems(&s);
    assert_eq!(2, rows.len());
    assert_eq!("ざつだん\u{2003}Jitendex", rows[0].text);
    assert_eq!("ざつだん\u{2003}NHK", rows[1].text);
    // Each row uses a 30 px line and a 4 px gap.
    assert_eq!(rows[0].pen.1 + 30.0 + LINE_GAP, rows[1].pen.1);
}

/// One merged pitch row names both source dictionaries.
#[test]
fn one_row_naming_two_dictionaries_prints_both_against_that_row() {
    let p = card_with_pitch("ざつだん", vec![pitch_row(0, &["\u{5927}\u{8f9e}\u{6797}", "NHK"])]);
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let row = pitch_elems(&s)[0];
    assert_eq!("ざつだん\u{2003}\u{5927}\u{8f9e}\u{6797} \u{b7} NHK", row.text);
    // The reading uses card reading style. Sources use the dimmed style. Each other
    // chrome element has one span.
    assert_eq!(2, row.spans.len());
    assert_eq!(15.0, row.spans[0].size);
    assert_eq!(13.0, row.spans[1].size);
    assert_eq!("ざつだん".len() as u32, row.spans[0].len);
}

/// If no enabled pitch dictionary has the reading, the scene has no pitch row.
/// An empty row or placeholder would report false data.
#[test]
fn a_card_with_no_accent_draws_no_pitch_element() {
    let s = laid_out(&one_card(&[], None), 480.0, 800.0, false, false);

    assert!(pitch_elems(&s).is_empty(), "{:?}", texts(&s));
}

/// The pitch row sits below the headword and above part of speech. It replaces
/// the plain reading line because marked kana provide that reading.
#[test]
fn the_pitch_row_replaces_the_reading_line_and_sits_above_the_part_of_speech() {
    let card = Card {
        written: Some("雑談".into()),
        reading: Some("ざつだん".into()),
        pos: vec!["noun".into()],
        freq: None,
        blocks: vec![block("Jitendex", &["chatting"])],
        match_len: 2,
        pitch: vec![pitch_row(0, &["Jitendex"])],
    };
    let p = Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    };
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let order: Vec<&str> = s.elems.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        vec!["Headword", "Pitch", "Text", "Text", "Text"],
        order,
        "{:?}",
        texts(&s)
    );
    assert!(
        !texts(&s).contains(&"ざつだん"),
        "the bare reading is never drawn beside its own marked kana: {:?}",
        texts(&s)
    );
    assert_eq!("noun", s.elems[2].text, "the part of speech, after the accent");
}

/// A kana-only headword has no separate reading line. Marked kana sit directly
/// below the headword.
#[test]
fn a_kana_only_headword_draws_its_accent_under_the_headword() {
    let card = Card {
        written: None,
        reading: Some("ざつだん".into()),
        pos: vec![],
        freq: None,
        blocks: vec![],
        match_len: 4,
        pitch: vec![pitch_row(1, &["Jitendex"])],
    };
    let p = Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    };
    let s = laid_out(&p, 480.0, 800.0, false, false);

    let order: Vec<&str> = s.elems.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(vec!["Headword", "Pitch"], order);
    // The headword is the reading. Four kana at 20px use a 40px line.
    assert_eq!(12.0 + 40.0 + LINE_GAP, s.elems[1].pen.1);
}

/// A pitch mark decorates a row without extra space. The row has the same
/// measurements as the same text without an accent. Extra space would move
/// later elements.
#[test]
fn the_marks_take_no_room_from_the_line_they_sit_on() {
    let plain = laid_out(
        &card_with_pitch("ざつだん", vec![pitch_row(0, &["Jitendex"])]),
        480.0,
        800.0,
        false,
        false,
    );
    let ticked = laid_out(
        &card_with_pitch("ざつだん", vec![pitch_row(1, &["Jitendex"])]),
        480.0,
        800.0,
        false,
        false,
    );

    let row = |s: &PopupScene| {
        let e = pitch_elems(s)[0];
        (e.rect, e.advance, e.pen)
    };
    assert_eq!(row(&plain), row(&ticked));
    assert_eq!(plain.content_h, ticked.content_h);
}

/// The frequency corner narrows only the next element. Here that element is the
/// accent because the card has no reading line or part-of-speech line. This path
/// uses the same width reservation.
#[test]
fn a_pitch_row_after_the_frequency_corner_takes_the_narrowed_width() {
    let card = Card {
        written: None,
        reading: Some("ざつだん".into()),
        pos: vec![],
        freq: Some(42),
        blocks: vec![],
        match_len: 4,
        pitch: vec![pitch_row(0, &["Jitendex"])],
    };
    let p = Presentation {
        top: Some(card.clone()),
        collapsed: vec![],
        all_cards: vec![card],
        sentence: None,
    };
    let s = laid_out(&p, 480.0, 800.0, false, false);

    // The corner narrows the headword, but the accent below uses the full column.
    let head = find(&s, ElemKind::Headword);
    let row = pitch_elems(&s)[0];
    assert!(head.wrap_w < s.content_w, "the corner narrowed the headword");
    assert_eq!(s.content_w, row.wrap_w, "and only the one after it");
}

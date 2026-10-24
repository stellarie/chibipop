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
use super::{chrome::*, gloss::*, marker::*, ruby::*, style::*};
use crate::dict::gloss::{RoleFilter, Tag};
use crate::dict::media::{Intrinsic, MediaFormat, MediaKey};
use crate::present::{Card, CollapsedRow, GlossBlock, GlossEntry};

mod sweep;

#[test]
fn hover_queries_use_painted_glyphs_and_respect_scroll_and_padding() {
    let p = card_with(vec![block("Test", &["食べる。猫"]) ]);
    let scene = laid_out(&p, 424.0, 4000.0, false, false);
    let elem = scene.elems.iter().find(|e| e.text == "食べる。猫").unwrap();
    let advance = elem.font_size * ADVANCE;
    let point = (elem.pen.0 + advance * 0.25, elem.pen.1 + 1.0);
    let query = |point, scroll| scene.hover_query(point, scroll, "Fake", &mut FakeMeasure::default()).unwrap();
    assert_eq!(query(point, 0.0), Some("食べる".into()));
    assert_eq!(query((point.0, point.1 - 10.0), 10.0), Some("食べる".into()));
    assert_eq!(query((elem.pen.0 - 1.0, point.1), 0.0), None);
    assert_eq!(query((elem.pen.0 + advance * 3.25, point.1), 0.0), None);
    assert_eq!(query((elem.pen.0 + advance * 7.0, point.1), 0.0), None);
    assert_eq!(query((point.0, scene.view_h), 0.0), None);
}

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
    caret_offsets: usize,
    hit_queries: usize,
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
        self.caret_offsets += at.len();
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
        self.hit_queries += 1;
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
        inflections: vec![],
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
        surface: None,
    }
}

fn with_collapsed() -> Presentation {
    let card = Card {
        written: Some("雑談".into()),
        reading: Some("ざつだん".into()),
        pos: vec![],
        inflections: vec![],
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
        surface: None,
    }
}

/// Build a `PopupScene` with the supplied maximum width and height.
fn laid_out(p: &Presentation, max_w: f32, max_h: f32, show_back: bool, side: bool) -> PopupScene {
    let theme = Theme::dark();
    laid_out_with_theme(p, max_w, max_h, show_back, side, &theme)
}

fn laid_out_with_theme(
    p: &Presentation,
    max_w: f32,
    max_h: f32,
    show_back: bool,
    side: bool,
    theme: &Theme,
) -> PopupScene {
    let mut m = FakeMeasure::default();
    scene(
        &SceneRequest {
            presentation: p,
            theme,
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
        inflections: vec![],
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
        surface: None,
    }
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

// ---- gap stack ----

// ---- scroll cull ----

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

// ---- hit targets ----

// ---- the Anki slot ----

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

// ---- the measurement seam ----


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
        inflections: vec![],
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
        surface: None,
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

// ---- the block box pass ----

/// A box length uses its element's font size. Each expectation multiplies
/// `Theme::body_size` by the fixture value.
const BOX_EM: f32 = 15.0;

/// Get the one gloss body from a scene.
fn one_body(s: &PopupScene) -> &SceneElem {
    let found = bodies(s);
    assert_eq!(1, found.len(), "expected one gloss element, got {found:?}");
    found[0]
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

// ---- sense identity ----

// ---- element construction ----

// ---- one label per dictionary ----

// ---- the Anki label ----

// ---- scroll ----

// ---- inline images ----

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

// ---- a dictionary's styles.css sizes an image ----

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


// ---- a right-aligned line's own ink ----

// ---- a table whose children are not rows ----

// ---- a picture wider than its column ----

// ---- two readings over one base ----

// ---- a reading at the end of a line ----

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
        inflections: vec![],
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
        surface: None,
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

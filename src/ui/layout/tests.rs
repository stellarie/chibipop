//! Layout against fixed metrics (ADR-0011, layer one).
//!
//! `FakeMeasure` wraps at a whole number of pixels per UTF-16 unit, so
//! every expectation below is arithmetic a reader can redo by hand. No
//! font, no platform: these run in both CI jobs, forever.

use super::*;
use crate::present::{Card, CollapsedRow, GlossBlock};

/// Advance per UTF-16 unit, as a
/// fraction of the font size.
const ADVANCE: f32 = 0.5;
/// Line height, likewise.
const LINE_H: f32 = 2.0;

/// A text engine with no fonts.
///
/// One rectangle per UTF-16 unit,
/// wrapped greedily. Records every
/// run it was asked for, so a test
/// can assert what layout measured
/// and at what width.
#[derive(Default)]
struct FakeMeasure {
    /// `(text, size, max_w)`, in order.
    asked: Vec<(String, f32, f32)>,
}

/// `(advance, units per line, units)`.
fn wrap(run: MeasureRun<'_>) -> (f32, usize, usize) {
    let advance = run.size * ADVANCE;
    let per_line = (run.max_w.max(1.0) / advance).floor().max(1.0) as usize;
    (advance, per_line, run.text.encode_utf16().count())
}

impl TextMeasure for FakeMeasure {
    fn measure(&mut self, run: MeasureRun<'_>) -> Result<Metrics, MeasureError> {
        self.asked.push((run.text.to_string(), run.size, run.max_w));
        let (advance, per_line, units) = wrap(run);
        let lines = units.div_ceil(per_line).max(1);
        Ok(Metrics {
            w: units.min(per_line) as f32 * advance,
            h: lines as f32 * run.size * LINE_H,
            lines: lines as u32,
        })
    }

    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError> {
        let (advance, per_line, units) = wrap(run);
        for &o in at {
            let o = (o as usize).min(units);
            out.push(GlyphBox {
                x: (o % per_line) as f32 * advance,
                y: (o / per_line) as f32 * run.size * LINE_H,
                w: advance,
                h: run.size * LINE_H,
            });
        }
        Ok(())
    }
}

/// Refuses everything, once asked.
struct BrokenMeasure;

impl TextMeasure for BrokenMeasure {
    fn measure(&mut self, _: MeasureRun<'_>) -> Result<Metrics, MeasureError> {
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

// ---- fixtures ----

fn block(dict: &str, glosses: &[&str]) -> GlossBlock {
    GlossBlock {
        dict_name: dict.to_string(),
        glosses: glosses.iter().map(|s| s.to_string()).collect(),
        glosses_html: vec![],
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
    Presentation { top: Some(card.clone()), collapsed: vec![], all_cards: vec![card] }
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
    assert_eq!(SEPARATOR_THICKNESS, sep.rect.h);
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
        m.asked.iter().map(|(t, _, w)| (t.clone(), *w)).collect();
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

// ---- element construction ----

/// It must lead the list.
#[test]
fn frequency_leads_as_a_corner_so_it_shares_the_headword_line() {
    let theme = Theme::dark();
    let (elems, _) = build_elements(&one_card(&[], Some(7671)), &theme, false, false);
    match &elems[0] {
        Elem::Corner(line) => {
            assert_eq!("freq 7671", line.text);
            assert_eq!(theme.dimmed_text, line.color);
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
    assert_eq!(theme.collapsed_size, pos.size);
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

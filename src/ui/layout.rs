//! The popup's measured scene (ADR-0004).
//!
//! Layout lives here, not in the bins: element construction, wrapping,
//! line stacking, the side panel, scrollbar geometry and hit rects.
//! Bins measure text through `TextMeasure` and paint the runs the scene
//! hands them; nothing else about the panel is theirs.
//!
//! One pixel space, and no scale factor: core never sees one. Windows
//! hands in DIPs because its Direct2D target carries the DPI, Linux
//! hands in device pixels; either way the conversion is the bin's
//! (ADR-0004 - physical pixels stay authoritative, logical geometry is
//! derived).

use crate::controller::HitAction;
use crate::present::{AnkiPopupState, Presentation};
use crate::ui::theme::{Theme, SCROLLBAR_MIN_THUMB};
use std::fmt;

/// Gap within a block.
const LINE_GAP: f32 = 4.0;
/// Gap before a new block.
const SECTION_GAP: f32 = 10.0;
/// Gap beside the corner elem.
const CORNER_GAP: f32 = 8.0;
/// Gap around the rule.
const SEPARATOR_MARGIN: f32 = 10.0;
/// The side column's vertical rule.
///
/// Not `Theme::separator_height`: that
/// themes the horizontal rule between
/// blocks, and a height cannot set the
/// width of a vertical one.
const SEPARATOR_THICKNESS: f32 = 1.0;
/// Fixed "See also" column width.
const SIDE_PANEL_W: f32 = 110.0;
/// Gap before the side panel.
const SIDE_GAP: f32 = 12.0;
/// DirectWrite's regular weight.
///
/// What a rule carries: it has no text
/// to weight, and zero is no weight.
const REGULAR_WEIGHT: u16 = 400;

/// A colour, as `Theme` carries it.
pub type Rgb = (u8, u8, u8);

// ---- the measurement seam ----

/// One run to measure.
#[derive(Debug, Clone, Copy)]
pub struct MeasureRun<'a> {
    pub text: &'a str,
    /// Family name, from the theme.
    pub font: &'a str,
    pub size: f32,
    /// DirectWrite weight, 100-900.
    pub weight: u16,
    pub italic: bool,
    /// Wrap width. A measurer that
    /// cannot wrap at zero clamps it
    /// itself; the scene reports the
    /// width it asked for.
    pub max_w: f32,
}

/// What one wrapped run measures.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    /// Widest line's width.
    pub w: f32,
    /// All lines' height.
    pub h: f32,
    /// Wrapped line count.
    pub lines: u32,
}

/// One caret's box inside a run.
///
/// Run-relative, like the layout box
/// the measurer wrapped.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GlyphBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The text engine refused a run.
///
/// Opaque: layout cannot interpret a
/// platform failure, only abandon the
/// walk. The bin re-attaches its own
/// error on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureError {
    /// What the engine reported.
    pub what: String,
}

impl MeasureError {
    pub fn new(what: impl Into<String>) -> MeasureError {
        MeasureError { what: what.into() }
    }
}

impl fmt::Display for MeasureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "measuring text failed: {}", self.what)
    }
}

impl std::error::Error for MeasureError {}

/// Text measurement, and nothing else.
///
/// Measure-only by construction: wrap
/// a string at a width, report line
/// metrics. It never paints, and the
/// scene it feeds carries positioned
/// runs as plain data, so layout is
/// testable against fixed metrics
/// (ADR-0004).
pub trait TextMeasure {
    /// Wrap `run` and measure it.
    fn measure(&mut self, run: MeasureRun<'_>) -> Result<Metrics, MeasureError>;

    /// Caret boxes inside a run.
    ///
    /// `at` are UTF-16 offsets into
    /// `run.text`; one box per offset
    /// is pushed to `out`, in order.
    /// Per-character hit targets need
    /// shaped geometry, which only the
    /// measurer has.
    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError>;
}

// ---- the scene ----

/// A box in panel space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SceneRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Where a run sits in its wrap box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Leading,
    /// The frequency corner only.
    Trailing,
}

/// What an element came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemKind {
    /// A rule, not a run.
    Separator,
    /// Right-aligned, advances no y.
    Corner,
    Text,
    /// A clickable collapsed row.
    Collapsed,
    /// Per-character click targets.
    Headword,
    BackButton,
}

impl ElemKind {
    /// Stable name for snapshots.
    pub fn as_str(self) -> &'static str {
        match self {
            ElemKind::Separator => "Separator",
            ElemKind::Corner => "Corner",
            ElemKind::Text => "Text",
            ElemKind::Collapsed => "Collapsed",
            ElemKind::Headword => "Headword",
            ElemKind::BackButton => "BackButton",
        }
    }
}

/// One measured, positioned element.
///
/// Plain data: everything a bin needs
/// to paint it, and everything a
/// snapshot needs to compare it.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneElem {
    pub kind: ElemKind,
    /// Empty for `Separator`.
    pub text: String,
    pub color: Rgb,
    /// Zero for `Separator`.
    pub font_size: f32,
    /// DirectWrite weight, 100-900.
    ///
    /// `REGULAR_WEIGHT` for a rule,
    /// which has no text to weight.
    pub weight: u16,
    pub italic: bool,
    /// Gap added above this element.
    pub top_gap: f32,
    /// Wrap width the text was given.
    pub wrap_w: f32,
    pub align: Align,
    /// Where the wrap box starts: what
    /// a bin hands its text engine.
    pub pen: (f32, f32),
    /// The measured ink box.
    pub rect: SceneRect,
    /// Wrapped line count.
    pub lines: u32,
    /// What the walk's y advanced by.
    pub advance: f32,
}

/// A clickable region in the panel.
///
/// `None` in x or w spans the panel:
/// a row is clickable across its full
/// width, however wide the panel is.
#[derive(Debug, Clone, PartialEq)]
pub struct HitTarget {
    pub x: Option<f32>,
    pub y: f32,
    pub w: Option<f32>,
    pub h: f32,
    pub action: HitAction,
}

/// One "See also" row.
#[derive(Debug, Clone, PartialEq)]
pub struct SideRow {
    /// `None` for the heading.
    pub idx: Option<usize>,
    pub text: String,
    pub color: Rgb,
    /// Column-local, like the column
    /// itself: add `SidePanel::origin_y`
    /// for panel space.
    pub y: f32,
    pub h: f32,
}

/// The "See also" column.
#[derive(Debug, Clone, PartialEq)]
pub struct SidePanel {
    /// Top of the column and its rule.
    pub origin_y: f32,
    /// The vertical rule's left edge.
    pub rule_x: f32,
    pub rule_w: f32,
    pub col_x: f32,
    pub col_w: f32,
    /// The whole column's height.
    pub height: f32,
    pub rows: Vec<SideRow>,
}

impl SidePanel {
    /// Rows to paint, scroll applied.
    pub fn visible(&self, scroll: f32, view_h: f32) -> impl Iterator<Item = PaintedRow<'_>> {
        let origin_y = self.origin_y;
        self.rows.iter().filter_map(move |row| {
            let y = origin_y + row.y - scroll;
            on_panel(y, row.h, 0.0, view_h).then_some(PaintedRow { row, y })
        })
    }
}

/// One row, ready to paint.
#[derive(Debug, Clone, Copy)]
pub struct PaintedRow<'a> {
    pub row: &'a SideRow,
    /// Scroll-applied top.
    pub y: f32,
}

/// The Anki affordance's slot.
///
/// Core reserves the strip under the
/// panel and names the label;
/// realisation is per-bin (ADR-0004).
/// Windows gives the affordance its
/// own window, sized by that window's
/// own font, and takes only the strip;
/// a bin painting in the panel uses
/// `rect` whole.
#[derive(Debug, Clone, PartialEq)]
pub struct AnkiSlot {
    pub label: String,
    pub color: Rgb,
    pub rect: SceneRect,
}

/// One popup, laid out.
///
/// Heights and widths are the layout's
/// own pixels; a bin scales them on
/// the way out (`panel_w`, `view_h`,
/// `content_h`).
#[derive(Debug, Clone, PartialEq)]
pub struct PopupScene {
    /// Inner padding: both origins.
    pub origin: f32,
    /// The main column's width.
    pub content_w: f32,
    /// In draw order.
    pub elems: Vec<SceneElem>,
    /// The main column's targets, in
    /// draw order. The side column's
    /// are in `side`; `hit_targets`
    /// chains both, as painted.
    pub hits: Vec<HitTarget>,
    pub side: Option<SidePanel>,
    pub anki: Option<AnkiSlot>,
    /// The main column's height, before
    /// padding: what the walk stacked.
    pub used_h: f32,
    /// Body plus padding, unclamped.
    pub content_h: f32,
    /// `content_h`, clamped to the box.
    pub view_h: f32,
    /// The width the panel wants, or
    /// `None` to keep the width it was
    /// offered (no side column: the
    /// main column already fills it).
    pub panel_w: Option<f32>,
}

/// One element, ready to paint.
#[derive(Debug, Clone, Copy)]
pub struct Painted<'a> {
    pub elem: &'a SceneElem,
    /// Scroll-applied pen origin.
    pub pen: (f32, f32),
}

impl PopupScene {
    /// Elements to paint, in order.
    ///
    /// Scroll applied and off-panel
    /// elements dropped here, not in
    /// the bins: a bin that clips (D2D)
    /// and one that does not (tiny-skia)
    /// must paint the same panel.
    pub fn visible(&self, scroll: f32, view_h: f32) -> impl Iterator<Item = Painted<'_>> {
        self.elems.iter().filter_map(move |elem| {
            let pen = (elem.pen.0, elem.pen.1 - scroll);
            // Ink may overhang the
            // measured box; one em of
            // slack keeps a boundary
            // element's ascender.
            on_panel(pen.1, elem.rect.h, elem.font_size, view_h).then_some(Painted { elem, pen })
        })
    }

    /// Every target, as painted.
    ///
    /// The main column first, then the
    /// side column: hit-testing takes
    /// the first match, so the order is
    /// the paint order.
    pub fn hit_targets(&self) -> Vec<HitTarget> {
        let side_rows = self.side.as_ref().map_or(0, |s| s.rows.len());
        let mut out = Vec::with_capacity(self.hits.len() + side_rows);
        out.extend(self.hits.iter().cloned());
        if let Some(side) = &self.side {
            for row in &side.rows {
                let Some(idx) = row.idx else { continue };
                out.push(HitTarget {
                    x: Some(side.col_x),
                    y: side.origin_y + row.y,
                    w: Some(side.col_w),
                    h: row.h,
                    action: HitAction::ExpandEntry(idx),
                });
            }
        }
        out
    }
}

/// Is a box worth painting?
fn on_panel(y: f32, h: f32, slack: f32, view_h: f32) -> bool {
    y + h + slack > 0.0 && y - slack < view_h
}

// ---- the walk ----

/// One popup's whole layout input.
pub struct SceneRequest<'a> {
    pub presentation: &'a Presentation,
    pub theme: &'a Theme,
    /// The width to fill.
    pub max_w: f32,
    /// The tallest the panel may get.
    pub max_h: f32,
    pub show_back: bool,
    pub side_panel: bool,
    /// `Some` reserves the Anki slot.
    pub anki: Option<&'a AnkiPopupState>,
}

/// Lays out one popup.
pub fn scene(req: &SceneRequest<'_>, m: &mut dyn TextMeasure) -> Result<PopupScene, MeasureError> {
    let theme = req.theme;
    let font = theme.font_name.as_str();
    let pad = theme.padding as f32;
    let origin = pad;

    let (elems, entries) = build_elements(req.presentation, theme, req.show_back, req.side_panel);
    let has_side = !entries.is_empty();
    let side_extra = if has_side {
        SIDE_GAP + SEPARATOR_THICKNESS + SIDE_GAP + SIDE_PANEL_W
    } else {
        0.0
    };
    let content_w = (req.max_w - 2.0 * pad - side_extra).max(0.0);

    let mut y = 0.0f32;
    let mut reserved_w = 0.0f32;
    let mut out = Vec::with_capacity(elems.len());
    let mut hits = Vec::new();
    let mut probes = Vec::new();

    for elem in &elems {
        let advance = match elem {
            Elem::Separator { top_gap } => {
                let h = theme.separator_height;
                y += top_gap;
                out.push(SceneElem {
                    kind: ElemKind::Separator,
                    text: String::new(),
                    color: theme.separator,
                    font_size: 0.0,
                    weight: REGULAR_WEIGHT,
                    italic: false,
                    top_gap: *top_gap,
                    wrap_w: content_w,
                    align: Align::Leading,
                    pen: (origin, origin + y),
                    rect: SceneRect {
                        x: origin,
                        y: origin + y,
                        w: content_w,
                        h,
                    },
                    lines: 0,
                    advance: h,
                });
                h
            }
            Elem::Corner(line) => {
                // Trailing-aligned: the box
                // hugs the right edge, and
                // the run is measured
                // pre-alignment, so x comes
                // from the width.
                let met = m.measure(run(font, line, content_w))?;
                out.push(SceneElem {
                    kind: ElemKind::Corner,
                    text: line.text.clone(),
                    color: line.color,
                    font_size: line.size,
                    weight: line.weight,
                    italic: line.italic,
                    top_gap: line.top_gap,
                    wrap_w: content_w,
                    align: Align::Trailing,
                    pen: (origin, origin + y),
                    rect: SceneRect {
                        x: origin + content_w - met.w,
                        y: origin + y,
                        w: met.w,
                        h: met.h,
                    },
                    lines: met.lines,
                    advance: 0.0,
                });
                reserved_w = met.w + CORNER_GAP;
                0.0
            }
            Elem::Text(line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = m.measure(run(font, line, avail_w))?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(ElemKind::Text, line, &met, origin, y, avail_w));
                h
            }
            Elem::Collapsed(idx, line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = m.measure(run(font, line, avail_w))?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(
                    ElemKind::Collapsed,
                    line,
                    &met,
                    origin,
                    y,
                    avail_w,
                ));
                hits.push(HitTarget {
                    x: None,
                    y: origin + y,
                    w: None,
                    h,
                    action: HitAction::ExpandEntry(*idx),
                });
                h
            }
            Elem::Headword {
                headword,
                prefix_u16,
                line,
            } => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = m.measure(run(font, line, avail_w))?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(
                    ElemKind::Headword,
                    line,
                    &met,
                    origin,
                    y,
                    avail_w,
                ));

                let mut at = Vec::new();
                let mut chars = Vec::new();
                let mut u16_pos = *prefix_u16 as u32;
                for ch in headword.chars() {
                    if is_kanji(ch) {
                        at.push(u16_pos);
                        chars.push(ch);
                    }
                    u16_pos += ch.len_utf16() as u32;
                }
                if !at.is_empty() {
                    probes.clear();
                    m.caret_boxes(run(font, line, avail_w), &at, &mut probes)?;
                    for (ch, b) in chars.iter().zip(probes.iter()) {
                        hits.push(HitTarget {
                            x: Some(origin + b.x),
                            y: origin + y + b.y,
                            w: Some(b.w),
                            h: b.h,
                            action: HitAction::DrillDown(ch.to_string()),
                        });
                    }
                }
                h
            }
            Elem::BackButton(line) => {
                let met = m.measure(run(font, line, content_w))?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(
                    ElemKind::BackButton,
                    line,
                    &met,
                    origin,
                    y,
                    content_w,
                ));
                hits.push(HitTarget {
                    x: None,
                    y: origin + y,
                    w: None,
                    h,
                    action: HitAction::Back,
                });
                h
            }
        };
        y += advance;
    }

    let used_h = y;

    let side = if has_side {
        Some(side_panel(&entries, theme, origin, content_w, m)?)
    } else {
        None
    };

    let side_h = side.as_ref().map_or(0.0, |s| s.height);
    let body_h = used_h.max(side_h);
    let content_h = body_h.ceil() + 2.0 * pad;
    let view_h = content_h.min(req.max_h);
    let panel_w = if has_side {
        Some(content_w + side_extra + 2.0 * pad)
    } else {
        None
    };

    let anki = match req
        .anki
        .and_then(|a| anki_button_label(req.presentation, theme, a))
    {
        Some((label, color)) => {
            let w = panel_w.unwrap_or(req.max_w);
            let met = m.measure(MeasureRun {
                text: &label,
                font,
                size: theme.collapsed_size,
                weight: theme.collapsed_weight,
                italic: theme.collapsed_italic,
                max_w: w,
            })?;
            Some(AnkiSlot {
                label,
                color,
                rect: SceneRect {
                    x: 0.0,
                    y: view_h,
                    w,
                    h: met.h,
                },
            })
        }
        None => None,
    };

    Ok(PopupScene {
        origin,
        content_w,
        elems: out,
        hits,
        side,
        anki,
        used_h,
        content_h,
        view_h,
        panel_w,
    })
}

/// The shared text-element shape.
fn text_elem(
    kind: ElemKind,
    line: &Line,
    met: &Metrics,
    origin: f32,
    y: f32,
    wrap_w: f32,
) -> SceneElem {
    SceneElem {
        kind,
        text: line.text.clone(),
        color: line.color,
        font_size: line.size,
        weight: line.weight,
        italic: line.italic,
        top_gap: line.top_gap,
        wrap_w,
        align: Align::Leading,
        pen: (origin, origin + y),
        rect: SceneRect {
            x: origin,
            y: origin + y,
            w: met.w,
            h: met.h,
        },
        lines: met.lines,
        advance: met.h,
    }
}

fn run<'a>(font: &'a str, line: &'a Line, max_w: f32) -> MeasureRun<'a> {
    MeasureRun {
        text: &line.text,
        font,
        size: line.size,
        weight: line.weight,
        italic: line.italic,
        max_w,
    }
}

/// The "See also" column's geometry.
fn side_panel(
    entries: &[SideEntry],
    theme: &Theme,
    origin: f32,
    content_w: f32,
    m: &mut dyn TextMeasure,
) -> Result<SidePanel, MeasureError> {
    let font = theme.font_name.as_str();
    let size = theme.collapsed_size;
    let weight = theme.collapsed_weight;
    let italic = theme.collapsed_italic;
    let heading = MeasureRun {
        text: SIDE_HEADING,
        font,
        size,
        weight,
        italic,
        max_w: SIDE_PANEL_W,
    };
    let head = m.measure(heading)?;

    let mut rows = Vec::with_capacity(entries.len() + 1);
    rows.push(SideRow {
        idx: None,
        text: SIDE_HEADING.to_string(),
        color: theme.dimmed_text,
        y: 0.0,
        h: head.h,
    });
    let mut y = head.h + LINE_GAP;

    for entry in entries {
        let met = m.measure(MeasureRun {
            text: &entry.text,
            font,
            size,
            weight,
            italic,
            max_w: SIDE_PANEL_W,
        })?;
        rows.push(SideRow {
            idx: Some(entry.idx),
            text: entry.text.clone(),
            color: entry.color,
            y,
            h: met.h,
        });
        y += LINE_GAP + met.h;
    }

    let rule_x = origin + content_w + SIDE_GAP;
    Ok(SidePanel {
        origin_y: origin,
        rule_x,
        rule_w: SEPARATOR_THICKNESS,
        col_x: rule_x + SEPARATOR_THICKNESS + SIDE_GAP,
        col_w: SIDE_PANEL_W,
        height: y,
        rows,
    })
}

/// The side column's heading.
const SIDE_HEADING: &str = "See also";

// ---- scrolling ----

/// Overflow past the view, or 0.
pub fn max_scroll(content_h: i32, view_h: i32) -> i32 {
    (content_h - view_h).max(0)
}

/// The thumb as `(top, height)`.
///
/// Floored, and kept in track.
pub fn scrollbar_thumb(
    track_h: i32,
    content_h: i32,
    view_h: i32,
    scroll: i32,
) -> Option<(i32, i32)> {
    let span = max_scroll(content_h, view_h);
    if span == 0 || track_h <= 0 || content_h <= 0 {
        return None;
    }
    let ideal = (i64::from(track_h) * i64::from(view_h) / i64::from(content_h)) as i32;
    let thumb_h = ideal.clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
    let travel = track_h - thumb_h;
    let at = scroll.clamp(0, span);
    let top = (i64::from(travel) * i64::from(at) / i64::from(span)) as i32;
    Some((top, thumb_h))
}

// ---- elements ----

/// CJK ideograph check.
fn is_kanji(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
    )
}

/// One line to lay out or draw.
///
/// Rebuilt, never cached.
struct Line {
    text: String,
    color: Rgb,
    size: f32,
    /// Extra space above this line.
    top_gap: f32,
    /// DirectWrite weight, 100-900.
    weight: u16,
    italic: bool,
}

enum Elem {
    Text(Line),
    /// Right-aligned, advances no y.
    ///
    /// Steals width from the next.
    Corner(Line),
    Separator {
        top_gap: f32,
    },
    /// A clickable collapsed row.
    Collapsed(usize, Line),
    /// Per-char click targets.
    Headword {
        headword: String,
        prefix_u16: usize,
        line: Line,
    },
    /// Navigate back in history.
    BackButton(Line),
}

/// One "See also" headword.
struct SideEntry {
    idx: usize,
    text: String,
    color: Rgb,
}

/// `p`'s content, in draw order.
fn build_elements(
    p: &Presentation,
    theme: &Theme,
    show_back: bool,
    side_panel: bool,
) -> (Vec<Elem>, Vec<SideEntry>) {
    let mut out = Vec::new();

    if show_back {
        out.push(Elem::BackButton(Line {
            text: "\u{2190} Back".to_string(),
            color: theme.dict_label_text,
            size: theme.collapsed_size,
            top_gap: 0.0,
            weight: theme.dict_label_weight,
            italic: theme.dict_label_italic,
        }));
    }

    if let Some(card) = &p.top {
        // Before the headword: same y.
        if let Some(freq) = card.freq {
            out.push(Elem::Corner(Line {
                text: format!("freq {freq}"),
                color: theme.frequency_text,
                size: theme.frequency_size,
                top_gap: 0.0,
                weight: theme.frequency_weight,
                italic: theme.frequency_italic,
            }));
        }

        let headword = card
            .written
            .clone()
            .or_else(|| card.reading.clone())
            .unwrap_or_default();
        if !headword.is_empty() {
            out.push(Elem::Headword {
                headword: headword.clone(),
                prefix_u16: 0,
                line: Line {
                    text: headword.clone(),
                    color: theme.headword_text,
                    size: theme.headword_size,
                    top_gap: if show_back { LINE_GAP } else { 0.0 },
                    weight: theme.headword_weight,
                    italic: theme.headword_italic,
                },
            });
        }

        // Only if the headword differs.
        if card.written.is_some() {
            if let Some(reading) = card.reading.as_deref().filter(|r| !r.is_empty()) {
                out.push(Elem::Text(Line {
                    text: reading.to_string(),
                    color: theme.reading_text,
                    size: theme.reading_size,
                    top_gap: LINE_GAP,
                    weight: theme.reading_weight,
                    italic: theme.reading_italic,
                }));
            }
        }

        if !card.pos.is_empty() {
            out.push(Elem::Text(Line {
                text: card.pos.join(" · "),
                color: theme.dimmed_text,
                size: theme.dimmed_size,
                top_gap: LINE_GAP,
                weight: theme.dimmed_weight,
                italic: theme.dimmed_italic,
            }));
        }

        for block in &card.blocks {
            out.push(Elem::Text(Line {
                text: block.dict_name.clone(),
                color: theme.dict_label_text,
                size: theme.dict_label_size,
                top_gap: SECTION_GAP,
                weight: theme.dict_label_weight,
                italic: theme.dict_label_italic,
            }));
            for gloss in block.glosses.iter().filter(|g| !g.is_empty()) {
                out.push(Elem::Text(Line {
                    text: gloss.clone(),
                    color: theme.body_text,
                    size: theme.body_size,
                    top_gap: LINE_GAP,
                    weight: theme.body_weight,
                    italic: theme.body_italic,
                }));
            }
        }
    }

    let mut side = Vec::new();

    if !p.collapsed.is_empty() {
        if side_panel {
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = row
                    .written
                    .clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                if head.is_empty() {
                    continue;
                }
                side.push(SideEntry {
                    idx: i,
                    text: head,
                    color: theme.collapsed_text,
                });
            }
        } else {
            out.push(Elem::Separator {
                top_gap: SEPARATOR_MARGIN,
            });
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = related_inline_head(row.written.as_deref(), row.reading.as_deref());
                let text = if head.is_empty() {
                    row.summary.clone()
                } else {
                    format!("{head} \u{2014} {}", row.summary)
                };
                out.push(Elem::Collapsed(
                    i,
                    Line {
                        text,
                        color: theme.collapsed_text,
                        size: theme.collapsed_size,
                        top_gap: if i == 0 { SEPARATOR_MARGIN } else { LINE_GAP },
                        weight: theme.collapsed_weight,
                        italic: theme.collapsed_italic,
                    },
                ));
            }
        }
    }

    (out, side)
}

fn related_inline_head(written: Option<&str>, reading: Option<&str>) -> String {
    match (
        written.filter(|s| !s.is_empty()),
        reading.filter(|s| !s.is_empty()),
    ) {
        (Some(w), Some(r)) if w != r => format!("{w}\u{3010}{r}\u{3011}"),
        (Some(w), _) => w.to_string(),
        (None, Some(r)) => r.to_string(),
        (None, None) => String::new(),
    }
}

/// The Anki button's label.
///
/// `None` means: show no button.
pub fn anki_button_label(
    p: &Presentation,
    theme: &Theme,
    anki: &AnkiPopupState,
) -> Option<(String, Rgb)> {
    if !anki.enabled {
        return None;
    }
    if !anki.connected {
        return None;
    }
    let expr = p
        .top
        .as_ref()
        .and_then(|c| c.written.as_deref().or(c.reading.as_deref()))
        .unwrap_or("");
    let (text, color) = if anki.checking {
        ("Checking\u{2026}", theme.dimmed_text)
    } else if anki.adding {
        ("Adding\u{2026}", theme.dimmed_text)
    } else if anki.failed {
        ("\u{2717} Failed to add", theme.dimmed_text)
    } else if anki.added.contains(expr) {
        ("\u{2713} Added", theme.dimmed_text)
    } else if anki.dupes.contains(expr) {
        ("\u{ff0b} Add to Anki (duplicate)", theme.dict_label_text)
    } else {
        ("\u{ff0b} Add to Anki", theme.dict_label_text)
    };
    Some((text.to_string(), color))
}

#[cfg(test)]
mod issue_tests {
    use super::*;
    use crate::present::{Card, CollapsedRow, GlossBlock};

    fn card(glosses: &[&str], collapsed: Vec<CollapsedRow>) -> Presentation {
        let top = Card {
            written: Some("猫".into()),
            reading: Some("ねこ".into()),
            pos: vec![],
            freq: None,
            blocks: vec![GlossBlock {
                dict_name: "Dict".to_string(),
                glosses: glosses.iter().map(|s| s.to_string()).collect(),
                glosses_html: vec![],
            }],
            match_len: 1,
        };
        Presentation {
            top: Some(top.clone()),
            collapsed,
            all_cards: vec![top],
            sentence: None,
        }
    }

    #[test]
    fn glossary_senses_become_separate_text_elements() {
        let p = card(&["first sense", "second sense"], vec![]);
        let (elems, _) = build_elements(&p, &Theme::dark(), false, false);
        let glosses: Vec<&str> = elems
            .iter()
            .filter_map(|elem| match elem {
                Elem::Text(line) if line.text == "first sense" || line.text == "second sense" => {
                    Some(line.text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(vec!["first sense", "second sense"], glosses);
    }

    #[test]
    fn inline_related_rows_show_written_and_reading() {
        let p = card(
            &["body"],
            vec![CollapsedRow {
                written: Some("猫".into()),
                reading: Some("ねこ".into()),
                summary: "cat".into(),
            }],
        );
        let (elems, _) = build_elements(&p, &Theme::dark(), false, false);
        let row = elems.iter().find_map(|elem| match elem {
            Elem::Collapsed(_, line) => Some(line.text.as_str()),
            _ => None,
        });
        assert_eq!(Some("猫\u{3010}ねこ\u{3011} \u{2014} cat"), row);
    }

    #[test]
    fn inline_related_rows_omit_duplicate_reading() {
        assert_eq!("ねこ", related_inline_head(Some("ねこ"), Some("ねこ")));
    }

    #[test]
    fn side_related_rows_remain_headword_only() {
        let p = card(
            &["body"],
            vec![CollapsedRow {
                written: Some("猫".into()),
                reading: Some("ねこ".into()),
                summary: "cat".into(),
            }],
        );
        let (_, side) = build_elements(&p, &Theme::dark(), false, true);
        assert_eq!("猫", side[0].text);
    }
}

#[cfg(test)]
mod tests;

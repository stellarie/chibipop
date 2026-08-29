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
use crate::dict::gloss::{GlossDoc, ItemType, Kind, NodeId, Scalar, StyleKey, Tag};
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

/// One run of text with one style.
///
/// The finest unit the seam addresses
/// (ADR-0013). Colour rides along for
/// the bin's paint walk, which shapes
/// the same spans; no measurer reads
/// it and no geometry depends on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledSpan<'a> {
    pub text: &'a str,
    /// Family name, from the theme.
    pub font: &'a str,
    pub size: f32,
    /// DirectWrite weight, 100-900.
    pub weight: u16,
    pub italic: bool,
    pub color: Rgb,
}

/// One run to measure.
///
/// Its spans wrap as one paragraph, so
/// a span boundary is not a line
/// boundary. That is the whole of what
/// ADR-0013 widened: before it, bold
/// text and normal text could not
/// share a wrapped line.
#[derive(Debug, Clone, Copy)]
pub struct MeasureRun<'a> {
    /// In reading order.
    pub spans: &'a [StyledSpan<'a>],
    /// Wrap width. A measurer that
    /// cannot wrap at zero clamps it
    /// itself; the scene reports the
    /// width it asked for.
    pub max_w: f32,
}

/// What one wrapped run measures.
///
/// The engine's own aggregate for the
/// whole run, which is what the block
/// walk stacks and what the geometry
/// goldens pin. [`Measured`] carries
/// the detail beside it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    /// Widest line's width.
    pub w: f32,
    /// All lines' height.
    pub h: f32,
    /// Wrapped line count.
    pub lines: u32,
}

/// One wrapped line's geometry.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LineBox {
    /// Top edge, run-relative.
    pub y: f32,
    /// Inked width.
    pub w: f32,
    /// Top edge to the next line's.
    ///
    /// As tall as the tallest span on
    /// it, which is why mixed styling
    /// ended the old `lines × size ×
    /// LINE_HEIGHT` arithmetic.
    pub h: f32,
    /// Baseline, down from `y`.
    ///
    /// The one thing `{ w, h, lines }`
    /// could never say. A superscript,
    /// a subscript and a gaiji image at
    /// text size are all positions
    /// relative to this, so without it
    /// there is no arithmetic to place
    /// them, only a guess (ADR-0013).
    pub baseline: f32,
}

/// One span's piece of one line.
///
/// A span that wraps gets one of these
/// per line it touches, in line order
/// then span order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpanBox {
    /// Index into the run's spans.
    pub span: u32,
    /// Index into `Measured::lines`.
    pub line: u32,
    /// Leading edge, run-relative,
    /// like the line it sits on.
    pub x: f32,
    /// Advance across the line.
    pub w: f32,
    /// The line advance this span asks
    /// for on its own.
    ///
    /// Its line's `h` is at least this
    /// much: a line is as tall as its
    /// tallest span, so a half-size
    /// superscript never shrinks the
    /// line it rides on, and this is
    /// what says how much shorter than
    /// the line the span itself is.
    pub h: f32,
}

/// What one measured run is.
///
/// Handed in and refilled rather than
/// returned: the walk measures every
/// element in a panel and the inline
/// pass measures a paragraph per
/// block, so one buffer serves them
/// all instead of two allocations per
/// element per frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measured {
    /// The whole run's box.
    pub metrics: Metrics,
    /// Each wrapped line, top down.
    pub lines: Vec<LineBox>,
    /// Each span's piece of each line.
    pub spans: Vec<SpanBox>,
}

impl Measured {
    /// Empties it for the next run.
    ///
    /// Keeps the capacity: that is the
    /// point of handing it in.
    pub fn clear(&mut self) {
        self.metrics = Metrics::default();
        self.lines.clear();
        self.spans.clear();
    }
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
/// styled spans at a width, report
/// line and span geometry. It never
/// paints, and the scene it feeds
/// carries positioned runs as plain
/// data, so layout is testable against
/// fixed metrics (ADR-0004, amended by
/// ADR-0013).
pub trait TextMeasure {
    /// Wrap `run` and measure it.
    ///
    /// `out` is emptied first, so one
    /// buffer measures a whole panel.
    fn measure(
        &mut self,
        run: MeasureRun<'_>,
        out: &mut Measured,
    ) -> Result<(), MeasureError>;

    /// Caret boxes inside a run.
    ///
    /// `at` are UTF-16 offsets into the
    /// run's spans end to end; one box
    /// per offset is pushed to `out`,
    /// in order. Per-character hit
    /// targets need shaped geometry,
    /// which only the measurer has.
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

/// One styled piece of an element.
///
/// A byte range into
/// [`SceneElem::text`] plus the style
/// it draws in, so an element holding
/// mixed styling costs one string and
/// a flat vector of `Copy` records -
/// and a bin rebuilds the exact
/// [`MeasureRun`] the scene was
/// measured from by walking it.
///
/// No family: no structured-content
/// property can change one, so every
/// span in a panel draws in the
/// theme's own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElemSpan {
    /// Byte offset into the text.
    pub at: u32,
    pub len: u32,
    pub color: Rgb,
    pub size: f32,
    /// DirectWrite weight, 100-900.
    pub weight: u16,
    pub italic: bool,
    /// Baseline shift, up positive.
    ///
    /// `verticalAlign`, already
    /// resolved against the line the
    /// span landed on: the seam
    /// reports the baseline and this
    /// is the arithmetic ADR-0013
    /// exists to make possible. Zero
    /// for every span that sits on
    /// its line's own baseline, which
    /// is nearly all of them.
    pub shift: f32,
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
    /// This element's styled pieces,
    /// in reading order.
    ///
    /// One span for every element the
    /// panel's own chrome builds, and
    /// one per style change for a
    /// gloss. Empty for `Separator`,
    /// which has no text.
    pub spans: Vec<ElemSpan>,
}

impl SceneElem {
    /// The run a bin re-measures and
    /// paints this element from.
    ///
    /// The same spans, in the same
    /// order, at the same width the
    /// scene reports - so the ink
    /// lands where the hit rects say
    /// it does. `font` is the bin's,
    /// because the family that is
    /// actually installed is the
    /// bin's own question.
    pub fn styled_spans<'a>(
        &'a self,
        font: &'a str,
    ) -> impl Iterator<Item = StyledSpan<'a>> {
        self.spans.iter().map(move |s| StyledSpan {
            text: &self.text[s.at as usize..(s.at + s.len) as usize],
            font,
            size: s.size,
            weight: s.weight,
            italic: s.italic,
            color: s.color,
        })
    }
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
            on_panel(pen.1, elem.rect.h, elem.font_size, view_h)
                .then_some(Painted { elem, pen })
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
pub fn scene(
    req: &SceneRequest<'_>,
    m: &mut dyn TextMeasure,
) -> Result<PopupScene, MeasureError> {
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
    let mut measured = Measured::default();
    // The gloss walk's two scratch
    // buffers: one paragraph's spans
    // as the seam takes them, and the
    // per-line boxes one link covers.
    // Both are refilled per element,
    // so a rich entry costs no
    // allocation per element per
    // frame.
    let mut run: Vec<StyledSpan<'_>> = Vec::new();
    let mut cover: Vec<(u32, f32, f32)> = Vec::new();

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
                    rect: SceneRect { x: origin, y: origin + y, w: content_w, h },
                    lines: 0,
                    advance: h,
                    spans: Vec::new(),
                });
                h
            }
            Elem::Corner(line) => {
                // Trailing-aligned: the box
                // hugs the right edge, and
                // the run is measured
                // pre-alignment, so x comes
                // from the width.
                let met = measure_line(m, font, line, content_w, &mut measured)?;
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
                    spans: one_span(line),
                });
                reserved_w = met.w + CORNER_GAP;
                0.0
            }
            Elem::Text(line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = measure_line(m, font, line, avail_w, &mut measured)?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(ElemKind::Text, line, &met, origin, y, avail_w));
                h
            }
            Elem::Collapsed(idx, line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = measure_line(m, font, line, avail_w, &mut measured)?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(ElemKind::Collapsed, line, &met, origin, y, avail_w));
                hits.push(HitTarget {
                    x: None,
                    y: origin + y,
                    w: None,
                    h,
                    action: HitAction::ExpandEntry(*idx),
                });
                h
            }
            Elem::Headword { headword, prefix_u16, line } => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = measure_line(m, font, line, avail_w, &mut measured)?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(ElemKind::Headword, line, &met, origin, y, avail_w));

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
                    let spans = [span(font, line)];
                    m.caret_boxes(
                        MeasureRun { spans: &spans, max_w: avail_w },
                        &at,
                        &mut probes,
                    )?;
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
            Elem::Gloss(flow) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                // The whole paragraph in one
                // request: its spans wrap
                // together, so a bold word and
                // a normal one beside it in the
                // source share a line and the
                // paragraph rewraps as one unit
                // (ADR-0013).
                run.clear();
                run.extend(flow.styled_spans(font));
                m.measure(MeasureRun { spans: &run, max_w: avail_w }, &mut measured)?;
                let met = measured.metrics;
                let h = met.h;
                y += flow.top_gap;

                let spans = flow
                    .spans
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        // A span that wrapped is
                        // placed against the
                        // first line it touches;
                        // one that measured to
                        // nothing keeps the
                        // shift its em alone
                        // decided.
                        let (line, span_h) = first_box(&measured, i as u32);
                        ElemSpan {
                            at: s.at,
                            len: s.len,
                            color: s.style.color,
                            size: s.style.size,
                            weight: s.style.weight,
                            italic: s.style.italic,
                            shift: shift_on(s.style, line, span_h),
                        }
                    })
                    .collect();

                // One target per line a link
                // touches, so a cross-reference
                // that wrapped is clickable on
                // both halves of itself.
                for (i, action) in flow.links.iter().enumerate() {
                    cover.clear();
                    for b in &measured.spans {
                        let inside = flow
                            .spans
                            .get(b.span as usize)
                            .is_some_and(|s| s.link == i as u32);
                        if !inside {
                            continue;
                        }
                        match cover.iter_mut().find(|(line, _, _)| *line == b.line) {
                            Some(seen) => {
                                seen.1 = seen.1.min(b.x);
                                seen.2 = seen.2.max(b.x + b.w);
                            }
                            None => cover.push((b.line, b.x, b.x + b.w)),
                        }
                    }
                    for &(line, left, right) in &cover {
                        let line = measured.lines[line as usize];
                        hits.push(HitTarget {
                            x: Some(origin + left),
                            y: origin + y + line.y,
                            w: Some(right - left),
                            h: line.h,
                            action: action.clone(),
                        });
                    }
                }

                let base = flow.base(theme);
                out.push(SceneElem {
                    kind: ElemKind::Text,
                    text: flow.text.clone(),
                    color: base.color,
                    font_size: base.size,
                    weight: base.weight,
                    italic: base.italic,
                    top_gap: flow.top_gap,
                    wrap_w: avail_w,
                    align: Align::Leading,
                    pen: (origin, origin + y),
                    rect: SceneRect { x: origin, y: origin + y, w: met.w, h: met.h },
                    lines: met.lines,
                    advance: met.h,
                    spans,
                });
                h
            }
            Elem::BackButton(line) => {
                let met = measure_line(m, font, line, content_w, &mut measured)?;
                let h = met.h;
                y += line.top_gap;
                out.push(text_elem(ElemKind::BackButton, line, &met, origin, y, content_w));
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
        Some(side_panel(&entries, theme, origin, content_w, m, &mut measured)?)
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
            let met = measure_text(
                m,
                StyledSpan {
                    text: &label,
                    font,
                    size: theme.collapsed_size,
                    weight: theme.collapsed_weight,
                    italic: theme.collapsed_italic,
                    color,
                },
                w,
                &mut measured,
            )?;
            Some(AnkiSlot {
                label,
                color,
                rect: SceneRect { x: 0.0, y: view_h, w, h: met.h },
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
        rect: SceneRect { x: origin, y: origin + y, w: met.w, h: met.h },
        lines: met.lines,
        advance: met.h,
        spans: one_span(line),
    }
}

/// The one span a `Line` is, as the
/// scene carries it.
///
/// Every element the panel's own
/// chrome builds has one style, so it
/// has one span - and the seam gets
/// exactly the request it got before
/// the inline pass existed.
fn one_span(line: &Line) -> Vec<ElemSpan> {
    vec![ElemSpan {
        at: 0,
        len: line.text.len() as u32,
        color: line.color,
        size: line.size,
        weight: line.weight,
        italic: line.italic,
        shift: 0.0,
    }]
}

/// The line a span first landed on,
/// and the advance it asked that line
/// for.
///
/// A degenerate `(0, 0)` for a span
/// the measurer reported no box for -
/// an empty run, or one whose glyphs
/// all fell outside it - which
/// [`shift_on`] reads as "no line to
/// align against".
fn first_box(measured: &Measured, span: u32) -> (LineBox, f32) {
    match measured.spans.iter().find(|b| b.span == span) {
        Some(b) => (
            measured.lines.get(b.line as usize).copied().unwrap_or_default(),
            b.h,
        ),
        None => (LineBox::default(), 0.0),
    }
}

/// The one span a `Line` is, as the
/// seam takes it.
///
/// Every element the panel's own
/// chrome builds carries one style; a
/// gloss paragraph is what hands the
/// seam more than one, through
/// [`Flow::styled_spans`].
fn span<'a>(font: &'a str, line: &'a Line) -> StyledSpan<'a> {
    StyledSpan {
        text: &line.text,
        font,
        size: line.size,
        weight: line.weight,
        italic: line.italic,
        color: line.color,
    }
}

/// Measures one styled line's box.
fn measure_line(
    m: &mut dyn TextMeasure,
    font: &str,
    line: &Line,
    max_w: f32,
    scratch: &mut Measured,
) -> Result<Metrics, MeasureError> {
    measure_text(m, span(font, line), max_w, scratch)
}

/// Measures one styled span's box.
///
/// The block walk stacks whole
/// elements and never looks inside a
/// line, so it keeps the aggregate and
/// drops the per-line and per-span
/// detail. `scratch` is what keeps
/// dropping it from costing an
/// allocation per element per frame.
fn measure_text(
    m: &mut dyn TextMeasure,
    span: StyledSpan<'_>,
    max_w: f32,
    scratch: &mut Measured,
) -> Result<Metrics, MeasureError> {
    let spans = [span];
    m.measure(MeasureRun { spans: &spans, max_w }, scratch)?;
    Ok(scratch.metrics)
}

/// One "See also" row's span.
///
/// One role for the whole column, the
/// collapsed one, so its rows differ
/// only in text and colour - and no
/// geometry rides on colour.
fn side_span<'a>(theme: &'a Theme, text: &'a str, color: Rgb) -> StyledSpan<'a> {
    StyledSpan {
        text,
        font: theme.font_name.as_str(),
        size: theme.collapsed_size,
        weight: theme.collapsed_weight,
        italic: theme.collapsed_italic,
        color,
    }
}

/// The "See also" column's geometry.
fn side_panel(
    entries: &[SideEntry],
    theme: &Theme,
    origin: f32,
    content_w: f32,
    m: &mut dyn TextMeasure,
    scratch: &mut Measured,
) -> Result<SidePanel, MeasureError> {
    let heading = side_span(theme, SIDE_HEADING, theme.dimmed_text);
    let head = measure_text(m, heading, SIDE_PANEL_W, scratch)?;

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
        let met =
            measure_text(m, side_span(theme, &entry.text, entry.color), SIDE_PANEL_W, scratch)?;
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
    Separator { top_gap: f32 },
    /// A clickable collapsed row.
    Collapsed(usize, Line),
    /// Per-char click targets.
    Headword {
        headword: String,
        prefix_u16: usize,
        line: Line,
    },
    /// One paragraph of a gloss tree.
    ///
    /// The only element the panel
    /// builds that can hold more than
    /// one style, and the only one
    /// that can earn a hit target
    /// inside its own text.
    Gloss(Flow),
    /// Navigate back in history.
    BackButton(Line),
}

// ---- the inline formatting pass ----

/// Yomitan's own base font size, in
/// CSS pixels.
///
/// Not a size the popup draws at -
/// the theme owns that - but the
/// divisor Yomitan's stylesheet
/// writes its lengths against: the
/// spec's defaults table states a
/// cell border as `1em / 14`, "one
/// pixel at base size, so it scales
/// with the panel". So a dictionary
/// asking for `12px` is asking for
/// twelve fourteenths of the em it
/// sits in, and its absolute pixel
/// scales with the panel instead of
/// shrinking on a dense screen.
const YOMITAN_BASE_PX: f32 = 14.0;

/// The ratio CSS's absolute-size
/// keywords step by, which is also
/// what `smaller` and `larger` step
/// by. HTML's own stylesheet gives
/// `small`, `sub` and `sup` a
/// `smaller` size and `big` a
/// `larger` one.
///
/// One constant, divided and
/// multiplied rather than two
/// reciprocals, so that stepping a
/// whole-pixel size down and back up
/// returns to it.
const FONT_STEP: f32 = 1.2;

/// `font-weight: bold`, on
/// DirectWrite's scale. HTML's
/// default for `b` and `strong`, and
/// the spec's default for a table
/// header cell.
const BOLD_WEIGHT: u16 = 700;

/// One step of CSS's relative-weight
/// table, which over the 400-to-900
/// range dictionaries use is exactly
/// what `bolder` and `lighter` mean.
const WEIGHT_STEP: u16 = 300;

/// `vertical-align: super`, as a
/// fraction of the em it is raised
/// inside.
///
/// CSS defines it as "the appropriate
/// superscript position", which is a
/// face metric, and the seam reports
/// line and span geometry rather than
/// a face's tables (ADR-0013). A
/// third of an em up and a fifth down
/// are the fallbacks a text engine
/// uses for a face that declares
/// neither.
const SUPER_RISE: f32 = 1.0 / 3.0;
/// `vertical-align: sub`, likewise.
const SUB_DROP: f32 = 1.0 / 5.0;

/// What separates two top-level
/// glossary items.
///
/// A dictionary row's `glossary`
/// array holds exactly one item for
/// 64 of the census's 72
/// dictionaries, so this is a no-op
/// on nearly every entry - and on the
/// ones that hold more it is what the
/// panel has always drawn. Ticket 14
/// is what lets a reader stack them
/// instead.
const ITEM_SEPARATOR: &str = "; ";

/// A link's index in [`Flow::links`],
/// or no link at all.
const NO_LINK: u32 = u32::MAX;

/// What a span's `verticalAlign`
/// still needs from its line.
///
/// `baseline`, `sub` and `super` are
/// answered from the em alone, so the
/// walk resolves them into
/// [`Inline::shift`] as it descends.
/// The rest are defined against the
/// line's own extent, which only the
/// measurer knows, so they ride along
/// and are answered afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VAlign {
    /// Already in [`Inline::shift`].
    Fixed,
    /// Its top meets the line's.
    TextTop,
    /// Its bottom meets the line's.
    TextBottom,
    /// Its middle meets half an
    /// x-height above the baseline.
    Middle,
}

/// One span's resolved inline style,
/// while a paragraph is being built.
///
/// Everything a [`StyledSpan`]
/// carries bar the family, which no
/// structured-content property can
/// change, plus what decides where
/// the span sits on its line. Two
/// adjacent runs of text with equal
/// styles are one span, which is what
/// `PartialEq` is here for.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Inline {
    size: f32,
    weight: u16,
    italic: bool,
    color: Rgb,
    /// Baseline shift, up positive.
    shift: f32,
    align: VAlign,
}

impl Inline {
    /// The body role: what a gloss
    /// inherits before any node of it
    /// has spoken.
    fn body(theme: &Theme) -> Inline {
        Inline {
            size: theme.body_size,
            weight: theme.body_weight,
            italic: theme.body_italic,
            color: theme.body_text,
            shift: 0.0,
            align: VAlign::Fixed,
        }
    }
}

/// One span of a [`Flow`].
struct FlowSpan {
    at: u32,
    len: u32,
    style: Inline,
    /// The `<a>` it sits inside.
    link: u32,
}

/// One paragraph of inline content.
///
/// The unit this pass measures: one
/// [`MeasureRun`] in, one
/// [`SceneElem`] out. The text
/// accumulates into one string so the
/// seam gets the paragraph whole -
/// a span boundary is not a line
/// boundary (ADR-0013) - and each
/// span is a byte range into it.
#[derive(Default)]
struct Flow {
    /// Gap owed above it.
    top_gap: f32,
    text: String,
    spans: Vec<FlowSpan>,
    /// One per `<a>` that earned a hit
    /// target, indexed by
    /// [`FlowSpan::link`].
    links: Vec<HitAction>,
}

impl Flow {
    /// Its spans, as the seam takes
    /// them.
    fn styled_spans<'a>(&'a self, font: &'a str) -> impl Iterator<Item = StyledSpan<'a>> {
        self.spans.iter().map(move |s| StyledSpan {
            text: &self.text[s.at as usize..(s.at + s.len) as usize],
            font,
            size: s.style.size,
            weight: s.style.weight,
            italic: s.style.italic,
            color: s.style.color,
        })
    }

    /// The element's own style: its
    /// first span's, which for a gloss
    /// that is one plain string is the
    /// body role and nothing else.
    fn base(&self, theme: &Theme) -> Inline {
        self.spans.first().map_or_else(|| Inline::body(theme), |s| s.style)
    }

    /// Prefixes this paragraph with a
    /// matched row's number.
    ///
    /// One number per matched
    /// term-bank row, as Yomitan
    /// numbers them, so it belongs to
    /// the row's first paragraph and
    /// not to every sibling block
    /// inside it. Written in the
    /// body's own style, so it joins
    /// the span it precedes rather
    /// than becoming one of its own.
    fn number(&mut self, n: usize, style: Inline) {
        let label = format!("{n}. ");
        let shift = label.len() as u32;
        self.text.insert_str(0, &label);
        for span in &mut self.spans {
            span.at += shift;
        }
        match self.spans.first_mut() {
            Some(first) if first.style == style && first.link == NO_LINK => {
                first.at = 0;
                first.len += shift;
            }
            _ => self.spans.insert(
                0,
                FlowSpan { at: 0, len: shift, style, link: NO_LINK },
            ),
        }
    }
}

/// Turns one term-bank row's parsed
/// tree into paragraphs.
///
/// The block half of the pass, and
/// deliberately the same rules the
/// plain-text renderer uses
/// (`dict::gloss::plain`): a block tag
/// or a `data` marker opens a
/// paragraph, an inline tag adds spans
/// to the one already open, and a
/// paragraph with no text is dropped.
/// Two renderers over one tree must
/// not disagree about where the lines
/// are - that disagreement is the bug
/// class this spec exists to close.
struct Paragraphs<'a> {
    doc: &'a GlossDoc,
    /// Finished, in order.
    out: Vec<Flow>,
    /// The one still filling.
    cur: Flow,
    /// Every link seen so far;
    /// [`Flow`] gets the ones it
    /// reached, renumbered.
    links: Vec<HitAction>,
    /// A separator owed to the next
    /// text this paragraph takes.
    pending_sep: bool,
}

/// One row's gloss tree, laid out as
/// paragraphs at `top_gap` apart.
fn paragraphs(doc: &GlossDoc, base: Inline, top_gap: f32) -> Vec<Flow> {
    let mut p = Paragraphs {
        doc,
        out: Vec::new(),
        cur: Flow::default(),
        links: Vec::new(),
        pending_sep: false,
    };
    for id in doc.items() {
        p.item(id, base);
        p.pending_sep = true;
    }
    p.flush();
    for flow in &mut p.out {
        flow.top_gap = top_gap;
    }
    p.out
}

impl Paragraphs<'_> {
    /// One top-level glossary item.
    fn item(&mut self, id: NodeId, base: Inline) {
        let doc = self.doc;
        match doc.node(id).item_type {
            // Ticket 12 owns image
            // items; today they draw
            // nothing, which is what
            // the plain-text walk also
            // does with them.
            ItemType::Image => {}
            ItemType::Text => self.text(doc.text(id), base, NO_LINK),
            // Yomitan drops a
            // `structured-content`
            // item's children straight
            // into a block container,
            // so the item itself
            // neither opens a paragraph
            // nor takes part in the
            // drop rules.
            ItemType::StructuredContent => self.children(id, base, true, NO_LINK),
            _ if doc.is_plain_string(id) => self.text(doc.text(id), base, NO_LINK),
            _ => self.node(id, base, NO_LINK),
        }
    }

    /// One node.
    fn node(&mut self, id: NodeId, parent: Inline, link: u32) {
        let doc = self.doc;
        let node = *doc.node(id);
        if doc.is_dropped_subtree(id) || doc.is_part_of_speech(id) {
            return;
        }
        match node.tag {
            // Ticket 11 owns ruby. Until
            // it lands a base renders and
            // its reading does not: `rt`
            // dropped into the flow would
            // read as 漢かん字じ, and `rp`
            // is the fallback for a
            // renderer that cannot draw
            // ruby - which this one is
            // about to stop being. `ruby`
            // itself is a transparent
            // inline wrapper, so ticket 11
            // plugs in at this arm and
            // nowhere else.
            Tag::Rt | Tag::Rp => return,
            // Both engines break hard on
            // a newline, so a dictionary's
            // own break reaches the panel
            // inside the paragraph rather
            // than splitting it.
            Tag::Br => return self.text("\n", parent, link),
            _ => {}
        }
        // Ticket 12 owns images.
        if node.kind == Kind::Image {
            return;
        }
        if node.kind == Kind::Text && node.tag == Tag::None {
            return self.text(doc.text(id), parent, link);
        }
        // A block *opens* a line and
        // never closes one, exactly as
        // the plain-text walk's mark
        // does: text after a block joins
        // the block's own paragraph.
        if node.tag.is_block() || doc.has_marker(id) {
            self.flush();
        }
        let style = self.styled(id, parent);
        let link = self.link_of(id, link);
        self.children(id, style, !node.tag.is_inline(), link);
    }

    /// A node's children.
    ///
    /// A glossary array of bare strings
    /// is a list, one paragraph per
    /// string - Yomitan gives each its
    /// own `<li>`. An array mixing
    /// strings with nodes is prose
    /// broken up by its own markup, so
    /// only a run that is *entirely*
    /// bare strings, and holds more
    /// than one, becomes paragraphs.
    fn children(&mut self, id: NodeId, style: Inline, block_ctx: bool, link: u32) {
        let doc = self.doc;
        if block_ctx && doc.is_string_list(id) {
            for child in doc.children(id) {
                self.flush();
                self.text(doc.text(child), style, link);
            }
            return;
        }
        for child in doc.children(id) {
            self.node(child, style, link);
        }
    }

    /// Appends text, paying any owed
    /// item separator first.
    fn text(&mut self, text: &str, style: Inline, link: u32) {
        if text.is_empty() {
            return;
        }
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, style, link);
        }
        self.push(text, style, link);
    }

    /// Appends one run, joining it to
    /// the span before it when nothing
    /// about it differs.
    ///
    /// Coalescing is not only economy.
    /// It is what keeps a gloss that is
    /// one plain string measuring as
    /// exactly one span - byte for byte
    /// the request the panel made
    /// before this pass existed, which
    /// is why the geometry goldens do
    /// not move.
    fn push(&mut self, text: &str, style: Inline, link: u32) {
        let at = self.cur.text.len() as u32;
        let len = text.len() as u32;
        self.cur.text.push_str(text);
        match self.cur.spans.last_mut() {
            Some(last)
                if last.style == style && last.link == link && last.at + last.len == at =>
            {
                last.len += len;
            }
            _ => self.cur.spans.push(FlowSpan { at, len, style, link }),
        }
    }

    /// Ends the open paragraph.
    ///
    /// A paragraph with no text is
    /// dropped rather than drawn, so
    /// nested blocks give one paragraph
    /// per innermost block instead of a
    /// run of blank ones. What survives
    /// is trimmed at both ends, because
    /// Yomitan draws this in a browser
    /// and a browser does not indent a
    /// paragraph by the space a
    /// dictionary happened to leave
    /// between two nodes.
    fn flush(&mut self) {
        if self.cur.text.trim().is_empty() {
            self.cur.text.clear();
            self.cur.spans.clear();
            return;
        }
        let mut flow = std::mem::take(&mut self.cur);
        trim(&mut flow);
        // Renumber onto this paragraph's
        // own list: an `<a>` holding a
        // block spans two paragraphs,
        // and neither may name an index
        // the other owns.
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for span in &mut flow.spans {
            if span.link == NO_LINK {
                continue;
            }
            span.link = match seen.iter().find(|(old, _)| *old == span.link) {
                Some((_, new)) => *new,
                None => {
                    let new = flow.links.len() as u32;
                    flow.links.push(self.links[span.link as usize].clone());
                    seen.push((span.link, new));
                    new
                }
            };
        }
        self.out.push(flow);
    }

    /// The link a node's content sits
    /// inside, its own or its
    /// parent's.
    fn link_of(&mut self, id: NodeId, inherited: u32) -> u32 {
        let doc = self.doc;
        if doc.node(id).kind != Kind::Link {
            return inherited;
        }
        let Some(href) = doc.attr_of(id, "href").and_then(|v| doc.scalar_str(v)) else {
            return inherited;
        };
        match link_action(href) {
            Some(action) => {
                self.links.push(action);
                self.links.len() as u32 - 1
            }
            None => inherited,
        }
    }

    /// One node's resolved inline
    /// style.
    ///
    /// HTML's own stylesheet first - a
    /// `b` is bold and a `sup` is a
    /// raised `smaller` - then the
    /// node's resolved style record,
    /// which is the dictionary
    /// author's last word and which
    /// ticket 17 will also feed from
    /// the dictionary's own
    /// `styles.css`.
    fn styled(&self, id: NodeId, parent: Inline) -> Inline {
        let doc = self.doc;
        let mut style = tag_style(doc.node(id).tag, parent);
        let record = doc.style(id);
        for (i, (key, value)) in record.iter().enumerate() {
            // First occurrence wins,
            // which is the answer
            // `GlossDoc::style_of`
            // gives every other reader
            // of the record.
            if record[..i].iter().any(|(seen, _)| seen == key) {
                continue;
            }
            apply_style(doc, *key, *value, parent.size, &mut style);
        }
        style
    }
}

/// Drops a paragraph's edge
/// whitespace, spans and all.
///
/// The text is rebuilt rather than
/// sliced in place, since a span is a
/// byte range into it and every one
/// after the cut moves. A span left
/// with nothing goes, which is what
/// keeps a separator node between two
/// blocks from measuring as a span of
/// one space.
fn trim(flow: &mut Flow) {
    let start = flow.text.len() - flow.text.trim_start().len();
    let end = flow.text.trim_end().len();
    if start == 0 && end == flow.text.len() {
        return;
    }
    let (start, end) = (start as u32, end as u32);
    flow.text = flow.text[start as usize..end as usize].to_string();
    flow.spans.retain_mut(|span| {
        let at = span.at.max(start);
        let to = (span.at + span.len).min(end);
        span.at = at.saturating_sub(start);
        span.len = to.saturating_sub(at);
        span.len > 0
    });
}

/// HTML's own stylesheet, for the tags
/// structured content admits.
///
/// `verticalAlign` is not inherited -
/// CSS says so - so every node starts
/// back on its line's baseline and a
/// `sup` inside a `sup` is raised
/// once.
fn tag_style(tag: Tag, parent: Inline) -> Inline {
    let mut style = Inline { shift: 0.0, align: VAlign::Fixed, ..parent };
    match tag {
        // A header cell is bold by the
        // spec's own defaults table; its
        // tinted background is a box
        // property and ticket 08's.
        Tag::B | Tag::Strong | Tag::Th => style.weight = BOLD_WEIGHT,
        Tag::I | Tag::Em => style.italic = true,
        Tag::Sup => {
            style.size = parent.size / FONT_STEP;
            style.shift = parent.size * SUPER_RISE;
        }
        Tag::Sub => {
            style.size = parent.size / FONT_STEP;
            style.shift = -parent.size * SUB_DROP;
        }
        Tag::Small => style.size = parent.size / FONT_STEP,
        Tag::Big => style.size = parent.size * FONT_STEP,
        _ => {}
    }
    style
}

/// Folds one resolved style property
/// into a span's style.
///
/// The properties a styled span can
/// carry, and no others: the box
/// properties are ticket 08's, the
/// list and table ones are 09's and
/// 10's, and a value this build
/// cannot read leaves the inherited
/// one standing rather than guessing.
fn apply_style(doc: &GlossDoc, key: StyleKey, value: Scalar, em: f32, out: &mut Inline) {
    match key {
        StyleKey::FontSize => {
            if let Some(size) = length_px(doc, value, em) {
                out.size = size;
            }
        }
        StyleKey::FontWeight => {
            if let Some(weight) = weight_of(doc, value, out.weight) {
                out.weight = weight;
            }
        }
        StyleKey::FontStyle => {
            if let Some(italic) = italic_of(doc, value) {
                out.italic = italic;
            }
        }
        StyleKey::Color => {
            if let Some(color) = color_of(doc, value) {
                out.color = color;
            }
        }
        StyleKey::VerticalAlign => {
            if let Some((shift, align)) = align_of(doc, value, em) {
                out.shift = shift;
                out.align = align;
            }
        }
        _ => {}
    }
}

/// A CSS length, in the panel's own
/// pixels.
///
/// A number is Yomitan's em-multiplier
/// convention, the same one the HTML
/// renderer prints with an `em`
/// suffix. A string carries its own
/// unit: `em` and `%` are relative to
/// the em it sits in, and `px` is
/// relative to Yomitan's own base (see
/// [`YOMITAN_BASE_PX`]).
fn length_px(doc: &GlossDoc, value: Scalar, em: f32) -> Option<f32> {
    let px = match value {
        Scalar::Num(n) => em * n as f32,
        Scalar::Text(span) => {
            let text = doc.span(span).trim();
            let at = text
                .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+'))
                .unwrap_or(text.len());
            let n: f32 = text[..at].parse().ok()?;
            match text[at..].trim() {
                "em" | "" => em * n,
                "%" => em * n / 100.0,
                "px" => em * n / YOMITAN_BASE_PX,
                _ => return None,
            }
        }
        Scalar::Bool(_) | Scalar::Null => return None,
    };
    (px.is_finite() && px > 0.0).then_some(px)
}

/// `fontWeight` on DirectWrite's
/// scale, which is also CSS's and
/// fontdb's.
fn weight_of(doc: &GlossDoc, value: Scalar, inherited: u16) -> Option<u16> {
    let number = |n: f64| (n.is_finite() && n >= 1.0).then(|| (n as u16).clamp(100, 900));
    match value {
        Scalar::Num(n) => number(n),
        Scalar::Text(span) => match doc.span(span).trim() {
            "normal" => Some(REGULAR_WEIGHT),
            "bold" => Some(BOLD_WEIGHT),
            "bolder" => Some(inherited.saturating_add(WEIGHT_STEP).min(900)),
            "lighter" => Some(inherited.saturating_sub(WEIGHT_STEP).max(100)),
            text => text.parse::<f64>().ok().and_then(number),
        },
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// `fontStyle`. Oblique is italic:
/// neither engine synthesizes a slant
/// and a family that has one face for
/// both is the normal case.
fn italic_of(doc: &GlossDoc, value: Scalar) -> Option<bool> {
    match doc.scalar_str(value)?.trim() {
        "italic" | "oblique" => Some(true),
        "normal" => Some(false),
        _ => None,
    }
}

/// `verticalAlign`, against the em it
/// sits in.
///
/// A line box here holds one line of
/// text, so its own edges and its text
/// edges are the same two edges:
/// `top` cannot differ from
/// `text-top`, nor `bottom` from
/// `text-bottom`.
fn align_of(doc: &GlossDoc, value: Scalar, em: f32) -> Option<(f32, VAlign)> {
    Some(match doc.scalar_str(value)?.trim() {
        "baseline" => (0.0, VAlign::Fixed),
        "super" => (em * SUPER_RISE, VAlign::Fixed),
        "sub" => (-em * SUB_DROP, VAlign::Fixed),
        "text-top" | "top" => (0.0, VAlign::TextTop),
        "text-bottom" | "bottom" => (0.0, VAlign::TextBottom),
        "middle" => (0.0, VAlign::Middle),
        _ => return None,
    })
}

/// One span's baseline shift on the
/// line it landed on.
///
/// [`VAlign::Fixed`] needs no line.
/// The rest are CSS's line-relative
/// values, and the seam reports
/// exactly two facts about a line -
/// how tall it is and how far down it
/// the baseline sits - so a span's own
/// ascent is its own advance in the
/// same proportion (ADR-0013).
fn shift_on(style: Inline, line: LineBox, span_h: f32) -> f32 {
    if style.align == VAlign::Fixed || line.h <= 0.0 {
        return style.shift;
    }
    let ascent = line.baseline;
    let descent = line.h - line.baseline;
    let own_ascent = span_h * ascent / line.h;
    let own_descent = span_h - own_ascent;
    match style.align {
        VAlign::TextTop => ascent - own_ascent,
        VAlign::TextBottom => own_descent - descent,
        // CSS puts the box's middle half
        // an x-height above the
        // baseline; with no x-height in
        // the seam, half the ascent is
        // the usual stand-in for one.
        VAlign::Middle => ascent / 4.0 - (own_ascent - own_descent) / 2.0,
        VAlign::Fixed => style.shift,
    }
}

/// What following a glossary link
/// does.
///
/// A dictionary's own cross-references
/// carry no scheme and name their
/// target in a `query` parameter
/// (`?query=見出し語&wildcards=off`),
/// so they drill down in the panel
/// exactly as a headword's kanji does.
/// Its citations are `http` or
/// `https` and belong in a browser.
/// Anything else - `javascript:`,
/// `data:` - arrives from a file
/// chibipop did not write and earns no
/// target at all, which is the same
/// allow-list the Anki HTML renderer
/// applies. Whitespace and control
/// characters go first, because a URL
/// parser ignores them inside a URL
/// and a naive scheme check would not.
fn link_action(href: &str) -> Option<HitAction> {
    let clean: String =
        href.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    if let Some(query) = query_param(&clean) {
        return (!query.is_empty()).then_some(HitAction::DrillDown(query));
    }
    let followable = match clean.find([':', '/', '?', '#']) {
        Some(at) if clean.as_bytes()[at] == b':' => {
            let scheme = &clean[..at];
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }
        // No scheme, so relative to a
        // dictionary archive nothing
        // here can serve.
        _ => false,
    };
    followable.then_some(HitAction::OpenUrl(clean))
}

/// A cross-reference's `query`
/// parameter, percent-decoded.
fn query_param(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let raw = query
        .split(['&', '#'])
        .find_map(|pair| pair.strip_prefix("query="))?;
    Some(percent_decode(raw))
}

/// `%XX` back to bytes, leaving
/// anything malformed as written.
///
/// Yomitan writes these with
/// `encodeURIComponent`, which spells
/// a space `%20` and never `+`, so `+`
/// is left alone: in a headword it is
/// a character rather than a space.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex_pair(bytes, i + 1)) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Two hex digits at `at`, as a byte.
fn hex_pair(bytes: &[u8], at: usize) -> Option<u8> {
    let hi = hex_digit(*bytes.get(at)?)?;
    let lo = hex_digit(*bytes.get(at + 1)?)?;
    Some(hi << 4 | lo)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// A CSS colour, as the scene carries
/// them.
///
/// Hex in all three lengths,
/// `rgb()`/`rgba()`, and the sixteen
/// names CSS has had since level 1
/// plus `orange` - the whole surface a
/// dictionary's `color` values use.
/// Alpha parses and is dropped: `Rgb`
/// is the scene's colour and the panel
/// composites its own opacity once, at
/// the end. Anything else keeps the
/// colour it inherited.
fn color_of(doc: &GlossDoc, value: Scalar) -> Option<Rgb> {
    let text = doc.scalar_str(value)?.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return hex_color(hex.as_bytes());
    }
    if let Some(args) = text
        .strip_prefix("rgb(")
        .or_else(|| text.strip_prefix("rgba("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let mut channel = args.split([',', '/', ' ']).filter(|p| !p.trim().is_empty());
        let mut next = || -> Option<u8> {
            let raw = channel.next()?.trim();
            let n: f32 = match raw.strip_suffix('%') {
                Some(pct) => pct.trim().parse::<f32>().ok()? * 255.0 / 100.0,
                None => raw.parse().ok()?,
            };
            Some(n.round().clamp(0.0, 255.0) as u8)
        };
        return Some((next()?, next()?, next()?));
    }
    named_color(text)
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`.
fn hex_color(hex: &[u8]) -> Option<Rgb> {
    let digit = |i: usize| hex_digit(*hex.get(i)?);
    match hex.len() {
        3 | 4 => {
            let (r, g, b) = (digit(0)?, digit(1)?, digit(2)?);
            Some((r * 17, g * 17, b * 17))
        }
        6 | 8 => Some((
            hex_pair(hex, 0)?,
            hex_pair(hex, 2)?,
            hex_pair(hex, 4)?,
        )),
        _ => None,
    }
}

/// CSS level 1's sixteen, their two
/// British spellings, and `orange`.
fn named_color(name: &str) -> Option<Rgb> {
    let mut lower = name.to_ascii_lowercase();
    lower.retain(|c| !c.is_whitespace());
    Some(match lower.as_str() {
        "black" => (0, 0, 0),
        "silver" => (192, 192, 192),
        "gray" | "grey" => (128, 128, 128),
        "white" => (255, 255, 255),
        "maroon" => (128, 0, 0),
        "red" => (255, 0, 0),
        "purple" => (128, 0, 128),
        "fuchsia" | "magenta" => (255, 0, 255),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "olive" => (128, 128, 0),
        "yellow" => (255, 255, 0),
        "navy" => (0, 0, 128),
        "blue" => (0, 0, 255),
        "teal" => (0, 128, 128),
        "aqua" | "cyan" => (0, 255, 255),
        "orange" => (255, 165, 0),
        _ => return None,
    })
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

        let headword = card.written.clone()
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
            // Yomitan's `<ol>` holds one item per matched term-bank row, and
            // Hoshi Reader emits the list at all only when a dictionary
            // contributed more than one row - so one row is unnumbered. Never
            // the Senses inside a row: 大辞林 draws its own ①②③ in the tree,
            // and an outer number would double-number it.
            let numbered = block.entries.len() > 1;
            for (i, entry) in block.entries.iter().enumerate() {
                // Empty means "same set as the row above" (see `GlossEntry`).
                if !entry.tags.is_empty() {
                    out.push(Elem::Text(Line {
                        text: entry.tags.join(" · "),
                        color: theme.dimmed_text,
                        size: theme.dimmed_size,
                        top_gap: LINE_GAP,
                        weight: theme.dimmed_weight,
                        italic: theme.dimmed_italic,
                    }));
                }
                // The panel renders the parsed tree, not the plain-text
                // render of it: `GlossEntry::glosses` is what the Anki
                // plain-text field and the collapsed summary still need,
                // and a third view of one tree is the bug class this spec
                // set out to close.
                let mut flows = paragraphs(&entry.doc, Inline::body(theme), LINE_GAP);
                let Some(first) = flows.first_mut() else { continue };
                if numbered {
                    first.number(i + 1, Inline::body(theme));
                }
                out.extend(flows.into_iter().map(Elem::Gloss));
            }
        }
    }

    let mut side = Vec::new();

    if !p.collapsed.is_empty() {
        if side_panel {
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = row.written.clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                if head.is_empty() { continue; }
                side.push(SideEntry {
                    idx: i,
                    text: head,
                    color: theme.collapsed_text,
                });
            }
        } else {
            out.push(Elem::Separator { top_gap: SEPARATOR_MARGIN });
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = row.written.clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                let text = if head.is_empty() {
                    row.summary.clone()
                } else {
                    format!("{head} \u{2014} {}", row.summary)
                };
                out.push(Elem::Collapsed(i, Line {
                    text,
                    color: theme.collapsed_text,
                    size: theme.collapsed_size,
                    top_gap: if i == 0 { SEPARATOR_MARGIN } else { LINE_GAP },
                    weight: theme.collapsed_weight,
                    italic: theme.collapsed_italic,
                }));
            }
        }
    }

    (out, side)
}

/// The Anki button's label.
///
/// `None` means: show no button.
pub fn anki_button_label(
    p: &Presentation,
    theme: &Theme,
    anki: &AnkiPopupState,
) -> Option<(String, Rgb)> {
    if !anki.enabled { return None; }
    if !anki.connected { return None; }
    let expr = p.top.as_ref()
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
mod tests;

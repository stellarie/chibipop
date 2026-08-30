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
use crate::dict::gloss::{GlossDoc, ItemType, Kind, NodeId, NodePath, Scalar, StyleKey, Tag};
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
///
/// The panel's own chrome builds
/// `Leading` for everything but the
/// frequency corner; a gloss earns
/// the other two from `textAlign`,
/// which 7 of the census's 72
/// dictionaries declare over 249 242
/// nodes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Leading,
    Center,
    /// The frequency corner, and
    /// `textAlign: right`/`end`.
    Trailing,
}

impl Align {
    /// The fraction of a line's slack
    /// that sits before it.
    ///
    /// One number, so a line's own
    /// offset and the whole ink box's
    /// are the same arithmetic over
    /// two different widths.
    pub fn slack_before(self) -> f32 {
        match self {
            Align::Leading => 0.0,
            Align::Center => 0.5,
            Align::Trailing => 1.0,
        }
    }
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

/// One reading, over its base.
///
/// Out of the element's flow and out
/// of its text: a reading takes no
/// horizontal room from the line it
/// sits on, so it is not a span of the
/// run a bin re-measures. It is
/// placed once, here, and drawn where
/// it is told.
///
/// `x` and `y` are run-relative, like
/// the [`LineBox`] the reading was
/// placed against, so a bin adds
/// [`SceneElem::pen`] and draws. No
/// bidi and no complex-script
/// positioning: a reading sits
/// centred over the horizontal extent
/// its base measured to, and nothing
/// else.
#[derive(Debug, Clone, PartialEq)]
pub struct RubyRun {
    /// The reading itself.
    pub text: String,
    /// Leading edge, run-relative.
    pub x: f32,
    /// Top edge, likewise.
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub size: f32,
    pub color: Rgb,
    /// DirectWrite weight, 100-900.
    pub weight: u16,
    pub italic: bool,
}

impl RubyRun {
    /// The run a bin draws it from.
    ///
    /// One span and one line, at
    /// [`SceneElem::wrap_w`] - the
    /// width layout measured it at -
    /// so a reading too long for the
    /// panel wraps the same way in
    /// both places and its slot is as
    /// tall as what it wrapped to.
    pub fn styled_span<'a>(&'a self, font: &'a str) -> StyledSpan<'a> {
        StyledSpan {
            text: &self.text,
            font,
            size: self.size,
            weight: self.weight,
            italic: self.italic,
            color: self.color,
        }
    }
}

/// One value per edge of a box, in
/// CSS's own order.
///
/// Generic because the box model
/// needs two of them - lengths for
/// margin, padding and border width,
/// and a [`BorderStyle`] per edge -
/// and because one shorthand parser
/// then serves both: `padding:
/// 0.2em 0.3em` and `border-style:
/// none none none solid` are the same
/// one-to-four-value grammar, and
/// both are written by real
/// dictionaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Edges<T> {
    /// All four edges alike, which is
    /// what a one-value shorthand
    /// means.
    pub fn all(v: T) -> Edges<T> {
        Edges { top: v, right: v, bottom: v, left: v }
    }
}

impl Edges<f32> {
    /// Top plus bottom: what a box
    /// adds to its content's height.
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }

    /// Left plus right: what a box
    /// takes from its content's width.
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// Negative edges dropped, which
    /// is what CSS does to a padding
    /// or a border width and what it
    /// deliberately does not do to a
    /// margin.
    pub fn non_negative(self) -> Edges<f32> {
        Edges {
            top: self.top.max(0.0),
            right: self.right.max(0.0),
            bottom: self.bottom.max(0.0),
            left: self.left.max(0.0),
        }
    }
}

/// How one border edge draws.
///
/// CSS's `none` is the initial value
/// and forces that edge's used width
/// to zero however wide it was
/// declared, which is why it is the
/// default here too: a dictionary
/// asking for a width and no style
/// gets what a browser gives it,
/// nothing.
///
/// `groove`, `ridge`, `inset` and
/// `outset` resolve to [`Solid`], as
/// a browser draws them at the
/// hairline widths dictionaries use;
/// `hidden` resolves to [`None`].
///
/// [`Solid`]: BorderStyle::Solid
/// [`None`]: BorderStyle::None
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderStyle {
    #[default]
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
}

impl BorderStyle {
    /// Stable name for snapshots.
    pub fn as_str(self) -> &'static str {
        match self {
            BorderStyle::None => "none",
            BorderStyle::Solid => "solid",
            BorderStyle::Dashed => "dashed",
            BorderStyle::Dotted => "dotted",
            BorderStyle::Double => "double",
        }
    }
}

/// One box's margin, padding, border
/// and fill - the box record the
/// scene had no concept of.
///
/// The census's unsupported block is
/// dominated by these
/// (`docs/research/dict-shapes.md`):
/// margin, padding, border style,
/// border width and border radius,
/// each in 11 to 14 of 72
/// dictionaries, and that is how
/// **those** dictionaries draw
/// part-of-speech and label pills.
///
/// Not inherited, and deliberately:
/// CSS inherits none of these, and a
/// nested block that inherited its
/// parent's margin would pay it
/// twice.
///
/// Fed today by a node's inline
/// `style` only. Ticket 17 folds a
/// dictionary's own `styles.css` in
/// underneath that, in `GlossDoc`'s
/// resolved style record, so nothing
/// here learns that CSS exists.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BoxStyle {
    pub margin: Edges<f32>,
    pub padding: Edges<f32>,
    /// Declared border width, per
    /// edge. See
    /// [`border_used`](Self::border_used)
    /// for the width that draws.
    pub border: Edges<f32>,
    pub border_style: Edges<BorderStyle>,
    /// One colour for all four edges.
    ///
    /// CSS's initial value is
    /// `currentColor`, so this is
    /// seeded from the text colour the
    /// node resolved to and a border
    /// declaring only a width and a
    /// style still draws.
    pub border_color: Rgb,
    /// One radius for all four
    /// corners.
    pub radius: f32,
    /// `None` is transparent, which is
    /// CSS's initial value and not the
    /// same as black.
    pub background: Option<Rgb>,
}

impl BoxStyle {
    /// The border widths that actually
    /// draw.
    ///
    /// `border-style: none` forces an
    /// edge's used width to zero
    /// however wide it was declared,
    /// which is what makes
    /// `border-style: none none none
    /// solid` a left rule rather than
    /// a full frame.
    pub fn border_used(self) -> Edges<f32> {
        let used = |style: BorderStyle, w: f32| match style {
            BorderStyle::None => 0.0,
            _ => w.max(0.0),
        };
        Edges {
            top: used(self.border_style.top, self.border.top),
            right: used(self.border_style.right, self.border.right),
            bottom: used(self.border_style.bottom, self.border.bottom),
            left: used(self.border_style.left, self.border.left),
        }
    }

    /// Is there any ink in this box?
    ///
    /// What a painter asks before
    /// building a path: a box that only
    /// spaces its content out is
    /// geometry, not paint.
    pub fn paints(self) -> bool {
        self.background.is_some() || self.border_used() != Edges::default()
    }

    /// Does this box occupy space its
    /// content does not?
    ///
    /// True exactly when the walk's
    /// advance, the pen or the wrap
    /// width differ from what the same
    /// content would produce with no
    /// box at all - which is why a
    /// gloss carrying no box style
    /// still measures and stacks
    /// byte-for-byte as it did before
    /// this pass existed.
    pub fn spaces(self) -> bool {
        let b = self.border_used();
        self.margin != Edges::default()
            || self.padding != Edges::default()
            || b != Edges::default()
    }

    /// Is this box anything at all?
    ///
    /// Every gloss paragraph resolves
    /// one, because a border colour
    /// defaults to the text colour and
    /// a radius alone paints nothing;
    /// only a box that takes space or
    /// carries ink reaches the scene.
    pub fn exists(self) -> bool {
        self.spaces() || self.paints()
    }

    /// One stroke around the whole
    /// box, or a fill per edge?
    ///
    /// `Some(width)` when all four
    /// used widths are equal and
    /// non-zero, which is the only
    /// case a single rounded path can
    /// draw: a corner between two
    /// different widths has no one
    /// path, and an edge whose style
    /// is `none` has no width at all.
    /// `None` means "fill each edge as
    /// a plain rect", which is exactly
    /// right for the one asymmetric
    /// border the census corpus draws
    /// - a one-sided rule.
    ///
    /// Asked here rather than in each
    /// painter so that two bins with
    /// two different drawing APIs
    /// cannot answer it two different
    /// ways. Neither bin has a test
    /// that can see Direct2D; this
    /// does.
    pub fn even_border(self) -> Option<f32> {
        let e = self.border_used();
        let even = e.top == e.right && e.right == e.bottom && e.bottom == e.left;
        (even && e.top > 0.0).then_some(e.top)
    }
}

/// One box to fill and stroke.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElemBox {
    /// The border box, in panel space:
    /// what a fill covers, and what a
    /// stroke of the used width runs
    /// down the inside of.
    pub rect: SceneRect,
    pub style: BoxStyle,
}

/// Where in its dictionary a scene
/// element came from.
///
/// The stable identity stories 45 and
/// 46 ask for: a hit on a sense means
/// "sense 3 of 大辞林" and not a
/// character range. `dict_id` and
/// `entry_id` name the term-bank row,
/// and `path` names the subtree
/// inside that row's parsed tree -
/// which is exactly what
/// [`Selection::Nodes`] consumes, so
/// a picker built on this hands Anki
/// formatted markup for the senses
/// chosen rather than flattened text.
///
/// Cheap because the block pass is
/// already descending the tree: the
/// path is extended one step per
/// level on the way down
/// ([`NodePath::child`]), never
/// searched for afterwards.
///
/// [`Selection::Nodes`]: crate::dict::gloss::Selection::Nodes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlossOrigin {
    pub dict_id: i64,
    pub entry_id: i64,
    /// The node this element renders,
    /// or `None` when it has no
    /// address.
    ///
    /// A [`NodePath`] reaches 16
    /// levels and 65 536 siblings, and
    /// past either it returns nothing
    /// rather than a path that would
    /// address an ancestor instead.
    /// Unaddressable is a correct
    /// answer for a selection;
    /// silently aliased is not.
    pub path: Option<NodePath>,
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
    /// This element's readings, each
    /// already positioned over its
    /// base.
    ///
    /// Empty for all but a gloss
    /// paragraph holding `ruby`, which
    /// is 14 of the census's 72
    /// dictionaries. A reading is not
    /// in `text` and not in `spans`,
    /// because it does not flow - see
    /// [`RubyRun`].
    pub ruby: Vec<RubyRun>,
    /// The box this element *is*, or
    /// `None` when it draws none.
    ///
    /// Its rect is the border box, and
    /// the element's own `pen`,
    /// `wrap_w` and `advance` already
    /// account for the margin, border
    /// and padding around it: a bin
    /// paints this and nothing else
    /// changes.
    pub block_box: Option<ElemBox>,
    /// Boxes drawn around runs *inside*
    /// this element - one per line each
    /// run touches.
    ///
    /// A pill is an inline box, because
    /// that is what a pill is: it keeps
    /// its place on its line instead of
    /// breaking one. Pure decoration -
    /// no geometry the walk stacked
    /// depends on these.
    pub inline_boxes: Vec<ElemBox>,
    /// Where in its dictionary this
    /// came from, or `None` for the
    /// panel's own chrome.
    pub origin: Option<GlossOrigin>,
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

    /// Every box to paint, outermost
    /// first.
    ///
    /// The element's own box before the
    /// runs inside it, so a pill draws
    /// over the block that holds it and
    /// both draw under the text. One
    /// iterator, so a bin has one loop
    /// and cannot paint them in two
    /// different orders.
    pub fn boxes(&self) -> impl Iterator<Item = &ElemBox> {
        self.block_box.iter().chain(self.inline_boxes.iter())
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
    // per-line boxes one run of spans
    // covers - a link's, for its hit
    // targets, and a pill's, for its
    // box. Both are refilled per
    // element, so a rich entry costs
    // no allocation per element per
    // frame.
    let mut run: Vec<StyledSpan<'_>> = Vec::new();
    let mut cover: Vec<Cover> = Vec::new();

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
                    ruby: Vec::new(),
                    block_box: None,
                    inline_boxes: Vec::new(),
                    origin: None,
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
                    ruby: Vec::new(),
                    block_box: None,
                    inline_boxes: Vec::new(),
                    origin: None,
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
                // The box first: its margin,
                // border and padding decide
                // the width the paragraph is
                // measured at, so they cannot
                // be applied afterwards. A
                // paragraph carrying no box
                // gets `avail_w` and the pen
                // it always got, which is why
                // no geometry golden moves.
                let bx = flow.block.style;
                let (margin, border, padding) =
                    (bx.margin, bx.border_used(), bx.padding);
                let lead = Edges {
                    top: margin.top + border.top + padding.top,
                    right: margin.right + border.right + padding.right,
                    bottom: margin.bottom + border.bottom + padding.bottom,
                    left: margin.left + border.left + padding.left,
                };
                let wrap_w = (avail_w - lead.horizontal()).max(1.0);

                // The whole paragraph in one
                // request: its spans wrap
                // together, so a bold word and
                // a normal one beside it in the
                // source share a line and the
                // paragraph rewraps as one unit
                // (ADR-0013).
                run.clear();
                run.extend(flow.styled_spans(font));
                // The readings first, because
                // what they measure to is what
                // sizes the filler span that
                // buys each one its slot - so
                // the paragraph below is
                // measured with the taller
                // lines already asked for, and
                // `met.h` counts the readings
                // without anything after the
                // fact touching it. A bin
                // re-measuring these same
                // spans gets the same lines.
                let read = measure_readings(m, font, flow, wrap_w, &mut run)?;
                m.measure(MeasureRun { spans: &run, max_w: wrap_w }, &mut measured)?;
                let met = measured.metrics;
                y += flow.top_gap;
                // Margins are in the advance,
                // and are *not* collapsed
                // against a sibling's: see
                // [`Flow::block`].
                let h = lead.top + met.h + padding.bottom + border.bottom + margin.bottom;
                let pen = (origin + lead.left, origin + y + lead.top);
                // One offset per line, so a
                // centred paragraph that
                // wrapped centres each of its
                // lines and not just its ink
                // box.
                let slack = flow.block.align.slack_before();
                let line_at = |line: LineBox| pen.0 + (wrap_w - line.w).max(0.0) * slack;
                // Each reading over the base
                // it belongs to, in the slot
                // the lines already carry.
                let ruby = place_ruby(flow, &read, &measured, wrap_w, slack);

                let spans = flow
                    .spans
                    .iter()
                    .zip(run.iter())
                    .enumerate()
                    .map(|(i, (s, asked))| {
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
                            // The size the seam was
                            // handed, which for a
                            // ruby filler is not
                            // the size the walk
                            // resolved.
                            size: asked.size,
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
                    span_cover(&measured, &mut cover, |b| {
                        flow.spans.get(b as usize).is_some_and(|s| s.link == i as u32)
                    });
                    for &(line, left, right, _) in &cover {
                        let line = measured.lines[line as usize];
                        hits.push(HitTarget {
                            x: Some(line_at(line) + left),
                            y: pen.1 + line.y,
                            w: Some(right - left),
                            h: line.h,
                            action: action.clone(),
                        });
                    }
                }

                // A pill per line its run
                // touches, drawn around the
                // text rather than pushing it
                // aside: the measurement seam
                // takes styled spans and no
                // boxes (ADR-0013), so an
                // inline box's padding cannot
                // reserve room on its line.
                // Every census pill is a short
                // label its dictionary already
                // spaces with `marginRight`, so
                // what this costs is the pill's
                // border overlapping the gap
                // beside it, never a glyph.
                let mut inline_boxes = Vec::new();
                for run in &flow.inline {
                    span_cover(&measured, &mut cover, |b| {
                        b >= run.from && b < run.to
                    });
                    let (p, b) = (run.style.padding, run.style.border_used());
                    for &(line, left, right, span_h) in &cover {
                        let line = measured.lines[line as usize];
                        let shift = flow
                            .spans
                            .get(run.from as usize)
                            .map_or(0.0, |s| shift_on(s.style, line, span_h));
                        // The run's own text box
                        // on this line, from the
                        // two facts the seam
                        // reports about a line:
                        // how tall it is and how
                        // far down it the
                        // baseline sits.
                        let ascent = if line.h > 0.0 {
                            span_h * line.baseline / line.h
                        } else {
                            span_h
                        };
                        let top = line.y + line.baseline - shift - ascent;
                        inline_boxes.push(ElemBox {
                            rect: SceneRect {
                                x: line_at(line) + left - p.left - b.left,
                                y: pen.1 + top - p.top - b.top,
                                w: (right - left) + p.horizontal() + b.horizontal(),
                                h: span_h + p.vertical() + b.vertical(),
                            },
                            style: run.style,
                        });
                    }
                }

                let base = flow.base(theme);
                // A reading wider than its
                // base overhangs it, and the
                // ink box is the ink: without
                // this an element would report
                // a width its own furigana
                // exceeds. It can only ever
                // overhang to the right -
                // [`place_ruby`] clamps the
                // left edge into the box.
                let ink_w = ruby.iter().fold(met.w, |a, r| a.max(r.x + r.w));
                out.push(SceneElem {
                    kind: ElemKind::Text,
                    text: flow.text.clone(),
                    color: base.color,
                    font_size: base.size,
                    weight: base.weight,
                    italic: base.italic,
                    top_gap: flow.top_gap,
                    wrap_w,
                    align: flow.block.align,
                    pen,
                    rect: SceneRect {
                        x: pen.0 + (wrap_w - met.w).max(0.0) * slack,
                        y: pen.1,
                        w: ink_w,
                        h: met.h,
                    },
                    lines: met.lines,
                    advance: h,
                    spans,
                    ruby,
                    block_box: bx.exists().then(|| ElemBox {
                        rect: SceneRect {
                            x: origin + margin.left,
                            y: origin + y + margin.top,
                            w: (avail_w - margin.horizontal()).max(0.0),
                            h: border.vertical() + padding.vertical() + met.h,
                        },
                        style: bx,
                    }),
                    inline_boxes,
                    origin: Some(GlossOrigin {
                        dict_id: flow.dict_id,
                        entry_id: flow.entry_id,
                        path: flow.path,
                    }),
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
        ruby: Vec::new(),
        block_box: None,
        inline_boxes: Vec::new(),
        // The panel's own chrome: a
        // headword, a reading, a
        // dictionary label and a
        // collapsed row are built from
        // a `Presentation` and not from
        // a parsed tree, so none of
        // them addresses a node.
        origin: None,
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

/// One run of spans, on one line.
///
/// `(line, left, right, height)`, all
/// run-relative: the line index, the
/// run's horizontal extent on it, and
/// the tallest span box in it - which
/// is what a hit target's rect and an
/// inline box's rect are each built
/// from.
type Cover = (u32, f32, f32, f32);

/// Where a run of spans landed, one
/// entry per line it touches.
///
/// A span that wrapped has one box
/// per line, and a run of them has
/// several per line, so this unions
/// them: a cross-reference broken
/// across two lines is clickable on
/// both halves and a pill that
/// wrapped is drawn on both. `out` is
/// refilled rather than returned,
/// because the walk does this twice
/// per paragraph.
fn span_cover(measured: &Measured, out: &mut Vec<Cover>, wanted: impl Fn(u32) -> bool) {
    out.clear();
    for b in &measured.spans {
        if !wanted(b.span) {
            continue;
        }
        match out.iter_mut().find(|(line, ..)| *line == b.line) {
            Some(seen) => {
                seen.1 = seen.1.min(b.x);
                seen.2 = seen.2.max(b.x + b.w);
                seen.3 = seen.3.max(b.h);
            }
            None => out.push((b.line, b.x, b.x + b.w, b.h)),
        }
    }
}

/// One reading, measured.
///
/// What [`measure_readings`] learns
/// and [`place_ruby`] spends.
#[derive(Clone, Copy, Default)]
struct ReadBox {
    w: f32,
    h: f32,
    /// The fraction of a line that
    /// sits above its baseline.
    ///
    /// A face metric, and the seam
    /// reports it rather than the
    /// face's tables: `baseline / h`
    /// on any line of any run in this
    /// font. Noto Sans CJK answers
    /// 0.81; the layout tests' fake
    /// answers 0.5. Nothing here may
    /// assume either.
    ascent: f32,
}

/// Every reading of a paragraph,
/// measured, and its filler span
/// sized.
///
/// Called *before* the paragraph is
/// measured, and it is what buys the
/// reading its slot. A reading is not
/// in the paragraph's text, so the
/// measurer would give its line the
/// height the base alone needs - and
/// growing the line boxes afterwards
/// would fool nobody, because a bin
/// re-measures the same spans to
/// paint them and would get the
/// ungrown lines back.
///
/// So the run itself asks for the
/// taller line, through the one rule
/// both engines already answer to and
/// ADR-0013 already documents: a line
/// is as tall as its tallest span. A
/// [`RUBY_FILLER`] beside each base
/// carries no ink and no advance and
/// exists only to be that span. Both
/// the scene and the bin's re-measure
/// therefore see one set of line
/// boxes, and `metrics.h` counts the
/// readings without anything after
/// the fact touching it.
///
/// Its size is decided here rather
/// than while the paragraph is built,
/// because the growth a line hands to
/// the space *above* its baseline is
/// the face's ascent share
/// ([`ReadBox::ascent`]) and only a
/// measurer knows it.
fn measure_readings(
    m: &mut dyn TextMeasure,
    font: &str,
    flow: &Flow,
    max_w: f32,
    run: &mut [StyledSpan<'_>],
) -> Result<Vec<ReadBox>, MeasureError> {
    if flow.ruby.is_empty() {
        return Ok(Vec::new());
    }
    // Only a paragraph that holds ruby
    // pays for this buffer, which is
    // why it is not one of the walk's.
    let mut scratch = Measured::default();
    let mut out = vec![ReadBox::default(); flow.ruby.len()];
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        if ruby.text.is_empty() {
            continue;
        }
        m.measure(
            MeasureRun { spans: &[ruby_span(font, ruby)], max_w },
            &mut scratch,
        )?;
        let first = scratch.lines.first().copied().unwrap_or_default();
        out[slot] = ReadBox {
            w: scratch.metrics.w,
            h: scratch.metrics.h,
            ascent: if first.h > 0.0 { first.baseline / first.h } else { 0.0 },
        };
    }
    // A line of height `h` gives
    // `ascent * h` to the space above
    // its baseline, and a base of its
    // own size already claims its own
    // ascent of that. So a line has to
    // grow by `reading / ascent` for
    // the reading to fit above the
    // base - and a line's height is
    // proportional to its tallest
    // span's size in both engines,
    // which turns the growth into a
    // size.
    for (span, asked) in flow.spans.iter().zip(run.iter_mut()) {
        let Some(read) = flow.ruby.get(span.ruby as usize).filter(|_| span.filler) else {
            continue;
        };
        let box_ = out[span.ruby as usize];
        let grown = if box_.ascent > 0.0 {
            read.style.size / box_.ascent
        } else {
            read.style.size
        };
        asked.size = span.style.size + grown;
    }
    Ok(out)
}

/// The paragraph's readings, placed.
///
/// Pure: the lines already carry the
/// slot [`measure_readings`] bought,
/// so this only decides where in it
/// each reading sits.
fn place_ruby(
    flow: &Flow,
    read: &[ReadBox],
    measured: &Measured,
    wrap_w: f32,
    slack: f32,
) -> Vec<RubyRun> {
    let mut out = Vec::new();
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        let box_ = read[slot];
        if ruby.text.is_empty() || box_.h <= 0.0 {
            continue;
        }
        let base = |b: &&SpanBox| {
            flow.spans
                .get(b.span as usize)
                .is_some_and(|s| s.ruby == slot as u32 && !s.filler)
        };
        // The first line the base
        // landed on, and its extent
        // there: a base that wrapped
        // keeps its reading over its
        // head rather than over its
        // tail. A base is a character
        // or two, so it practically
        // never wraps.
        let Some(line) = measured.spans.iter().filter(base).map(|b| b.line).min() else {
            continue;
        };
        let Some(geom) = measured.lines.get(line as usize) else {
            continue;
        };
        let (left, right, tallest) = measured
            .spans
            .iter()
            .filter(base)
            .filter(|b| b.line == line)
            .fold((f32::MAX, 0.0f32, 0.0f32), |(l, r, h), b| {
                (l.min(b.x), r.max(b.x + b.w), h.max(b.h))
            });
        // Its bottom against the base's
        // own ink top, which is the
        // base's ascent up from the
        // line's baseline. Clamped into
        // the line, so a measurer that
        // gave the line no extra room
        // draws the reading small and
        // high rather than off the
        // paragraph.
        let floor = geom.baseline - box_.ascent * tallest;
        let y = geom.y + (floor - box_.h).max(0.0);
        // Centred over the base, and
        // never off the panel's left
        // edge: a reading wider than its
        // base overhangs it, which is
        // what a browser does too. The
        // line's own alignment slack
        // comes first, because the base
        // it sits over moved by it.
        let indent = (wrap_w - geom.w).max(0.0) * slack;
        let x = (indent + left + (right - left - box_.w) / 2.0).max(0.0);
        out.push(RubyRun {
            text: ruby.text.clone(),
            x,
            y,
            w: box_.w,
            h: box_.h,
            size: ruby.style.size,
            color: ruby.style.color,
            weight: ruby.style.weight,
            italic: ruby.style.italic,
        });
    }
    out
}

/// One reading, as the seam takes it.
fn ruby_span<'a>(font: &'a str, ruby: &'a FlowRuby) -> StyledSpan<'a> {
    StyledSpan {
        text: &ruby.text,
        font,
        size: ruby.style.size,
        weight: ruby.style.weight,
        italic: ruby.style.italic,
        color: ruby.style.color,
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

/// A reading's size, as a fraction of
/// the base it sits over.
///
/// Yomitan's own
/// `ext/css/structured-content.css` -
/// the spec's stated source of
/// defaults - declares no `ruby`,
/// `rt` or `rp` rule at all, so what
/// a reader sees in Yomitan is the
/// browser's own default, and both
/// engines Yomitan runs in give `rt`
/// `font-size: 50%`. This is
/// therefore Yomitan's drawn default
/// by exactly the route
/// [`tag_style`] already takes for
/// `b` and `sup`: HTML's own
/// stylesheet, and not a number
/// invented here.
///
/// Not [`FONT_STEP`]: `smaller` is a
/// 1.2 step and a reading is a half,
/// which is the point - furigana is
/// meant to read as an annotation
/// rather than as small text.
const RUBY_RATIO: f32 = 0.5;

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

/// A base's index in [`Flow::ruby`],
/// or no reading at all.
const NO_RUBY: u32 = u32::MAX;

/// What a ruby base's slot is bought
/// with: U+2060 WORD JOINER.
///
/// A reading has to have room above
/// its base that the line above must
/// not overlap, and the only thing
/// that grows a line is a taller span
/// on it (ADR-0013). This character
/// is that span. Two properties earn
/// it the job, and both were probed
/// against the real shaper rather
/// than assumed: it shapes to a glyph
/// of zero advance, so it costs the
/// line no width, and it is a *word
/// joiner*, so no wrap can separate
/// it from the base whose line it
/// grows.
///
/// Not U+200B ZERO WIDTH SPACE, which
/// measures the same and is a break
/// *opportunity*: it would let a line
/// break between a base and its own
/// filler.
///
/// It does mean an element's `text`
/// holds one invisible character per
/// reading. That is the honest
/// record: the text is what the bin
/// re-measures and shapes, and the
/// alternative - line boxes grown
/// after the wrap - is geometry the
/// paint would not reproduce.
const RUBY_FILLER: &str = "\u{2060}";

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

/// One node's resolved block-level
/// style, while a paragraph is being
/// built.
///
/// What [`Inline`] is to a span. Its
/// two halves behave differently
/// because CSS says so: `style` is
/// not inherited, so every node
/// starts with an empty box and a
/// nested block never pays its
/// parent's margin twice, while
/// `align` and `pre_line` are, so a
/// centred block centres the blocks
/// inside it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Block {
    /// This node's own box.
    style: BoxStyle,
    align: Align,
    /// `whiteSpace: pre-line`.
    pre_line: bool,
}

impl Block {
    /// What a child inherits: the
    /// inherited half, and an empty
    /// box.
    fn inherited(self) -> Block {
        Block { style: BoxStyle::default(), ..self }
    }
}

/// What a node inherits from the one
/// above it.
///
/// Bundled because the descent
/// carries four of them and a
/// five-argument recursion is where
/// an argument gets passed in the
/// wrong slot. `Copy`, so a child's
/// context is the parent's with one
/// field replaced.
#[derive(Debug, Clone, Copy)]
struct Ctx {
    inline: Inline,
    block: Block,
    /// The `<a>` it sits inside, or
    /// [`NO_LINK`].
    link: u32,
    /// This node's own address, or
    /// `None` once a step no longer
    /// fits - past 16 levels or the
    /// 65 536th sibling, where a
    /// [`NodePath`] addresses nothing
    /// rather than aliasing an
    /// ancestor.
    path: Option<NodePath>,
}

impl Ctx {
    /// This context at the `i`th child
    /// of the node it addresses.
    ///
    /// The path is extended one step
    /// per level on the way down, which
    /// is why every scene element can
    /// carry one for the price of a
    /// `u16` write.
    fn at(self, i: usize) -> Ctx {
        Ctx { path: self.path.and_then(|p| p.child(i)), ..self }
    }
}

/// One span of a [`Flow`].
struct FlowSpan {
    at: u32,
    len: u32,
    style: Inline,
    /// The `<a>` it sits inside.
    link: u32,
    /// The reading it wears, if it is
    /// a ruby base.
    ///
    /// Also what keeps a base from
    /// coalescing into the text beside
    /// it: the `漢` of `常用漢字` must
    /// stay addressable on its own for
    /// its reading to centre over it,
    /// and [`Paragraphs::push`] joins
    /// two runs only when everything
    /// about them matches.
    ruby: u32,
    /// Is this the slot's line-height
    /// filler rather than its base?
    ///
    /// A [`RUBY_FILLER`] span, whose
    /// only job is to be the tallest
    /// span on its line so that the
    /// measurer reserves the reading's
    /// slot - see
    /// [`measure_readings`]. It draws
    /// nothing, advances nothing, and
    /// is never a base.
    filler: bool,
}

/// One reading, before it is placed.
///
/// Held beside the paragraph rather
/// than in it: a reading takes no
/// horizontal room from the line it
/// sits on, so putting it in the run
/// would advance the pen past it and
/// let the wrap break it away from
/// the base it belongs to.
#[derive(Clone)]
struct FlowRuby {
    text: String,
    style: Inline,
}

/// One inline box's run of spans.
///
/// A pill: a `span` carrying padding,
/// a background or a border, which
/// keeps its place on its line rather
/// than breaking one - Jitendex draws
/// its part-of-speech labels this
/// way, and 11 to 14 census
/// dictionaries draw theirs.
///
/// A span range rather than a rect,
/// because where the run lands is the
/// measurer's answer: the seam reports
/// a box per span per line, so one of
/// these becomes one [`ElemBox`] per
/// line the run touches.
struct InlineRun {
    /// First span it covers.
    from: u32,
    /// One past the last.
    to: u32,
    style: BoxStyle,
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
    /// One per ruby base, indexed by
    /// [`FlowSpan::ruby`].
    ruby: Vec<FlowRuby>,
    /// The block this paragraph is:
    /// its box, its alignment, and
    /// whether it preserves newlines.
    block: Block,
    /// The node the paragraph renders,
    /// or `None` when it has no
    /// address.
    ///
    /// The block that opened it, so
    /// that a hit resolves to a
    /// subtree and
    /// `render_html(Selection::Nodes)`
    /// reproduces exactly that sense.
    path: Option<NodePath>,
    /// Boxes to draw around runs
    /// inside it.
    inline: Vec<InlineRun>,
    /// The term-bank row this
    /// paragraph renders.
    ///
    /// Stamped by [`build_elements`],
    /// which is where the ids are in
    /// hand: the tree itself does not
    /// know which row stored it.
    /// Together with `path` this is the
    /// whole of [`GlossOrigin`].
    dict_id: i64,
    entry_id: i64,
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
            Some(first)
                if first.style == style
                    && first.link == NO_LINK
                    && first.ruby == NO_RUBY =>
            {
                first.at = 0;
                first.len += shift;
            }
            _ => self.spans.insert(
                0,
                FlowSpan {
                    at: 0,
                    len: shift,
                    style,
                    link: NO_LINK,
                    ruby: NO_RUBY,
                    filler: false,
                },
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
    /// Every reading seen so far;
    /// [`Flow`] gets the ones its own
    /// bases wear, renumbered.
    rubies: Vec<FlowRuby>,
    /// The slot the text being read
    /// right now belongs to, or
    /// [`NO_RUBY`] outside a `ruby`.
    open_ruby: u32,
    /// A span break owed before the
    /// next run.
    ///
    /// What keeps an inline box's spans
    /// its own: its first run must not
    /// coalesce into the text before it
    /// and the run after it must not
    /// coalesce into its last, or the
    /// box would be drawn around its
    /// neighbours too.
    barrier: bool,
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
        rubies: Vec::new(),
        open_ruby: NO_RUBY,
        barrier: false,
    };
    let root = Ctx {
        inline: base,
        block: Block::default(),
        link: NO_LINK,
        path: Some(NodePath::ROOT),
    };
    for (i, id) in doc.items().enumerate() {
        p.item(id, root.at(i));
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
    fn item(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        // An item opens no block of its
        // own, so the paragraph it
        // lands in is attributed to the
        // item - which for the 20
        // census dictionaries that emit
        // one plain string per item is
        // the only address a scene
        // element can have. A block
        // inside it takes the address
        // back.
        if self.cur.text.is_empty() {
            self.cur.path = ctx.path;
        }
        match doc.node(id).item_type {
            // Ticket 12 owns image
            // items; today they draw
            // nothing, which is what
            // the plain-text walk also
            // does with them.
            ItemType::Image => {}
            ItemType::Text => self.text(doc.text(id), ctx.inline, ctx.link),
            // Yomitan drops a
            // `structured-content`
            // item's children straight
            // into a block container,
            // so the item itself
            // neither opens a paragraph
            // nor takes part in the
            // drop rules.
            ItemType::StructuredContent => self.children(id, ctx),
            _ if doc.is_plain_string(id) => self.text(doc.text(id), ctx.inline, ctx.link),
            _ => self.node(id, ctx),
        }
    }

    /// One node.
    fn node(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let node = *doc.node(id);
        if doc.is_dropped_subtree(id) || doc.is_part_of_speech(id) {
            return;
        }
        match node.tag {
            // A reading is drawn above
            // its base rather than in
            // the line beside it, so
            // the whole subtree is
            // handled at once: dropped
            // into the flow it would
            // read as 漢かん字じ.
            Tag::Ruby => return self.ruby(id, ctx),
            // Both engines break hard on
            // a newline, so a dictionary's
            // own break reaches the panel
            // inside the paragraph rather
            // than splitting it.
            Tag::Br => return self.text("\n", ctx.inline, ctx.link),
            _ => {}
        }
        // Ticket 12 owns images.
        if node.kind == Kind::Image {
            return;
        }
        if node.kind == Kind::Text && node.tag == Tag::None {
            return self.text(doc.text(id), ctx.inline, ctx.link);
        }
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        // A block *opens* a line and
        // never closes one, exactly as
        // the plain-text walk's mark
        // does: text after a block joins
        // the block's own paragraph -
        // and so does the block's own
        // box, which is why one
        // paragraph carries one box.
        if node.tag.is_block() || doc.has_marker(id) {
            self.open(ctx.path, block);
        }
        let next = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        // An inline node carrying ink is
        // a pill, and a pill is drawn as
        // an inline box: it keeps its
        // place on its line instead of
        // breaking one.
        if !node.tag.is_block() && block.style.paints() {
            return self.pill(id, next, block.style);
        }
        self.children(id, next);
    }

    /// One inline box's own run, kept
    /// out of its neighbours' spans.
    ///
    /// The box is drawn around exactly
    /// the spans this node produced, so
    /// a barrier goes in at each end:
    /// coalescing is what would
    /// otherwise hand the box a
    /// neighbour's text as well.
    fn pill(&mut self, id: NodeId, ctx: Ctx, style: BoxStyle) {
        self.barrier = true;
        let from = self.cur.spans.len() as u32;
        self.children(id, ctx);
        let to = self.cur.spans.len() as u32;
        self.barrier = true;
        // `to < from` when the node held
        // a block after all and flushed
        // the paragraph out from under
        // itself; a box over no span is
        // no box either way.
        if to > from {
            self.cur.inline.push(InlineRun { from, to, style });
        }
    }

    /// Ends the paragraph being built
    /// and starts one that belongs to
    /// `path`, carrying `block`'s box.
    fn open(&mut self, path: Option<NodePath>, block: Block) {
        self.flush();
        self.cur.path = path;
        self.cur.block = block;
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
    ///
    /// `ctx` is what the children
    /// inherit, so its box is already
    /// the empty one
    /// [`Block::inherited`] hands down:
    /// a bare string declares no box
    /// and each of these paragraphs
    /// carries none.
    fn children(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        if !doc.node(id).tag.is_inline() && doc.is_string_list(id) {
            for (i, child) in doc.children(id).enumerate() {
                self.open(ctx.at(i).path, ctx.block);
                self.text(doc.text(child), ctx.inline, ctx.link);
            }
            return;
        }
        for (i, child) in doc.children(id).enumerate() {
            self.node(child, ctx.at(i));
            // A `summary` closes its line
            // as well as opening one, and
            // it is the only tag that
            // does. What a block
            // otherwise does is *open* a
            // line - the rule the
            // plain-text walk shares - so
            // text after one joins it.
            // For a `details` that text
            // is the disclosure's body,
            // which a browser draws under
            // the summary and not as one
            // sentence with it. The
            // paragraph reopened here
            // belongs to the `details`,
            // whose path is this walk's
            // own.
            if doc.node(child).tag == Tag::Summary {
                self.open(ctx.path, ctx.block);
            }
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
        let ruby = self.open_ruby;
        let joins = !std::mem::take(&mut self.barrier);
        self.cur.text.push_str(text);
        match self.cur.spans.last_mut() {
            Some(last)
                if joins
                    && !last.filler
                    && last.style == style
                    && last.link == link
                    && last.ruby == ruby
                    && last.at + last.len == at =>
            {
                last.len += len;
            }
            _ => self.cur.spans.push(FlowSpan {
                at,
                len,
                style,
                link,
                ruby,
                filler: false,
            }),
        }
    }

    /// Buys the open slot's reading its
    /// room, if the slot has a base to
    /// hang it over.
    ///
    /// A [`RUBY_FILLER`] span of its
    /// own, appended straight after the
    /// base: the character is a word
    /// joiner, so no wrap can put it on
    /// a different line from the base
    /// whose line it is there to grow.
    /// Its size is left as the base's
    /// and raised by
    /// [`measure_readings`], which is
    /// the first point at which the
    /// reading's height is known.
    ///
    /// Nothing is appended for a slot
    /// no base text reached: an empty
    /// ruby would otherwise buy a
    /// taller line for a reading with
    /// nothing to read.
    fn push_filler(&mut self, style: Inline) {
        let slot = self.open_ruby;
        if !self.cur.spans.iter().any(|s| s.ruby == slot && !s.filler) {
            return;
        }
        let at = self.cur.text.len() as u32;
        self.cur.text.push_str(RUBY_FILLER);
        self.cur.spans.push(FlowSpan {
            at,
            len: RUBY_FILLER.len() as u32,
            style,
            link: NO_LINK,
            ruby: slot,
            filler: true,
        });
        // The run after it is its own
        // span: joining it would give a
        // whole word the filler's size.
        self.barrier = true;
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
            self.cur.inline.clear();
            // The dropped paragraph's
            // block goes with it: a
            // `div` holding only
            // whitespace pays no margin,
            // and its address belongs to
            // no element.
            self.cur.block = Block::default();
            self.cur.path = None;
            return;
        }
        let mut flow = std::mem::take(&mut self.cur);
        trim(&mut flow);
        // Renumber onto this paragraph's
        // own lists: an `<a>` holding a
        // block spans two paragraphs,
        // and neither may name an index
        // the other owns. A reading is
        // renumbered the same way, for
        // the same reason.
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
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for span in &mut flow.spans {
            if span.ruby == NO_RUBY {
                continue;
            }
            span.ruby = match seen.iter().find(|(old, _)| *old == span.ruby) {
                Some((_, new)) => *new,
                None => {
                    let new = flow.ruby.len() as u32;
                    flow.ruby.push(self.rubies[span.ruby as usize].clone());
                    seen.push((span.ruby, new));
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

    /// One `ruby` node: bases in the
    /// flow, readings above them.
    ///
    /// One slot per `rt` and not per
    /// `ruby`, because
    /// `<ruby>漢<rt>かん</rt>字<rt>じ
    /// </rt></ruby>` is two pairings in
    /// one node - which is how a
    /// dictionary writes per-character
    /// furigana. Each slot is opened
    /// before the base text that will
    /// wear it and closed by the `rt`
    /// that follows, so a base is
    /// stamped with its own reading and
    /// not with its neighbour's.
    fn ruby(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let style = self.styled(id, ctx.inline);
        let link = self.link_of(id, ctx.link);
        let slot = self.open_slot(style);
        let outer = std::mem::replace(&mut self.open_ruby, slot);
        // `rp` holds the parentheses
        // HTML wrote for a renderer
        // that cannot draw ruby. This
        // one can, so they are held
        // back and spent only if no
        // reading ever arrives, which
        // is the whole of the fallback
        // this tag exists for.
        let mut fallback: Vec<(String, Inline)> = Vec::new();
        let mut read = false;
        let inner = Ctx { inline: style, link, ..ctx };
        for (i, child) in doc.children(id).enumerate() {
            match doc.node(child).tag {
                Tag::Rt => {
                    if self.reading(child) {
                        read = true;
                        self.push_filler(style);
                    }
                    self.open_ruby = self.open_slot(style);
                }
                Tag::Rp => fallback.push((text_of(doc, child), self.styled(child, style))),
                _ => self.node(child, inner.at(i)),
            }
        }
        self.open_ruby = outer;
        if !read {
            for (text, style) in &fallback {
                self.text(text, *style, link);
            }
        }
    }

    /// A fresh slot, for the base text
    /// that follows it.
    ///
    /// Carries the style the reading
    /// will inherit, already stepped
    /// down to [`RUBY_RATIO`], so that
    /// a `fontSize` on the `rt` itself
    /// is relative to the reading's
    /// size as CSS says and not to the
    /// base's.
    fn open_slot(&mut self, base: Inline) -> u32 {
        self.rubies.push(FlowRuby {
            text: String::new(),
            style: Inline { size: base.size * RUBY_RATIO, ..base },
        });
        self.rubies.len() as u32 - 1
    }

    /// One `rt`, into the slot its base
    /// was stamped with.
    ///
    /// One run, however many wrappers a
    /// dictionary put inside the `rt`:
    /// a reading is one to four kana
    /// over a single base, so a style
    /// change part-way through it has
    /// nowhere to go. The `rt`'s own
    /// resolved style is kept, so a
    /// dictionary that colours its
    /// readings is honoured.
    ///
    /// `false` for an `rt` that renders
    /// nothing, which leaves the `rp`
    /// fallback still owed.
    fn reading(&mut self, id: NodeId) -> bool {
        let text = text_of(self.doc, id);
        if text.trim().is_empty() {
            return false;
        }
        let slot = self.open_ruby as usize;
        let style = self.styled(id, self.rubies[slot].style);
        self.rubies[slot] = FlowRuby { text, style };
        true
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

    /// One node's resolved
    /// block-level style.
    ///
    /// The inherited half comes from
    /// the node above; the box starts
    /// empty, because CSS inherits none
    /// of it. `inline` is this node's
    /// own resolved text style, and it
    /// is load-bearing twice: it seeds
    /// `border-color`, whose CSS
    /// initial value is
    /// `currentColor`, and it is the em
    /// a box length resolves against -
    /// `padding: 0.25em` is a quarter
    /// of the element's *own* font
    /// size, where `fontSize: 0.8em`
    /// is eight tenths of its parent's.
    ///
    /// Ticket 17 feeds this from a
    /// dictionary's own `styles.css`
    /// by widening the same resolved
    /// record, so nothing here learns
    /// that CSS exists.
    fn boxed(&self, id: NodeId, parent: Block, inline: Inline) -> Block {
        let doc = self.doc;
        let mut block = Block {
            style: BoxStyle { border_color: inline.color, ..BoxStyle::default() },
            ..parent
        };
        let record = doc.style(id);
        for (i, (key, value)) in record.iter().enumerate() {
            // First occurrence wins, as
            // it does for a span.
            if record[..i].iter().any(|(seen, _)| seen == key) {
                continue;
            }
            apply_box(doc, *key, *value, inline.size, &mut block);
        }
        block
    }
}

/// Every text descendant of a node,
/// concatenated.
///
/// What a reading and an `rp` fallback
/// are: one run of text, however many
/// wrappers a dictionary put around
/// it. The same "a text node is
/// `Kind::Text` with no tag" rule
/// [`Paragraphs::node`] uses, so the
/// two walks cannot disagree about
/// what text a subtree holds.
fn text_of(doc: &GlossDoc, id: NodeId) -> String {
    let mut out = String::new();
    push_text_of(doc, id, &mut out);
    out
}

fn push_text_of(doc: &GlossDoc, id: NodeId, out: &mut String) {
    let node = doc.node(id);
    if node.kind == Kind::Text && node.tag == Tag::None {
        out.push_str(doc.text(id));
    }
    for child in doc.children(id) {
        push_text_of(doc, child, out);
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
    // `white-space: pre-line`
    // preserves a segment break and
    // collapses everything else, so a
    // paragraph that declares it keeps
    // the newline a dictionary put at
    // its edge and loses only the
    // spaces around it.
    let edge: &[char] =
        if flow.block.pre_line { &[' ', '\t'] } else { &[' ', '\t', '\n', '\r'] };
    let start = flow.text.len() - flow.text.trim_start_matches(edge).len();
    let end = flow.text.trim_end_matches(edge).len();
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
        // property, resolved as one.
        //
        // A `summary` is its `details`'
        // heading and takes the same
        // weight from the same table.
        // The panel renders the pair
        // expanded with no disclosure
        // affordance (see the spec's Out
        // of Scope), so the weight is
        // what tells a reader which of
        // the two blocks is the label.
        Tag::B | Tag::Strong | Tag::Th | Tag::Summary => style.weight = BOLD_WEIGHT,
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

/// Folds one resolved style property
/// into a block's box.
///
/// The census's high-usage,
/// previously unsupported set and no
/// others
/// (`docs/research/dict-shapes.md`):
/// `color` is a span's and
/// [`apply_style`] already resolved
/// it, the list and table properties
/// are 09's and 10's, and a value
/// this build cannot read leaves the
/// box as it was rather than taking
/// half a declaration.
///
/// CSS forbids a negative padding,
/// border width and radius and allows
/// a negative margin, so each arm
/// clamps exactly what CSS clamps.
fn apply_box(doc: &GlossDoc, key: StyleKey, value: Scalar, em: f32, out: &mut Block) {
    let bx = &mut out.style;
    match key {
        StyleKey::Margin => {
            if let Some(e) = box_edges(doc, value, em) {
                bx.margin = e;
            }
        }
        StyleKey::MarginTop => {
            if let Some(v) = box_len(doc, value, em) {
                bx.margin.top = v;
            }
        }
        StyleKey::MarginRight => {
            if let Some(v) = box_len(doc, value, em) {
                bx.margin.right = v;
            }
        }
        StyleKey::MarginBottom => {
            if let Some(v) = box_len(doc, value, em) {
                bx.margin.bottom = v;
            }
        }
        StyleKey::MarginLeft => {
            if let Some(v) = box_len(doc, value, em) {
                bx.margin.left = v;
            }
        }
        StyleKey::Padding => {
            if let Some(e) = box_edges(doc, value, em) {
                bx.padding = e.non_negative();
            }
        }
        StyleKey::PaddingTop => {
            if let Some(v) = box_len(doc, value, em) {
                bx.padding.top = v.max(0.0);
            }
        }
        StyleKey::PaddingRight => {
            if let Some(v) = box_len(doc, value, em) {
                bx.padding.right = v.max(0.0);
            }
        }
        StyleKey::PaddingBottom => {
            if let Some(v) = box_len(doc, value, em) {
                bx.padding.bottom = v.max(0.0);
            }
        }
        StyleKey::PaddingLeft => {
            if let Some(v) = box_len(doc, value, em) {
                bx.padding.left = v.max(0.0);
            }
        }
        StyleKey::BorderWidth => {
            if let Some(e) = box_edges(doc, value, em) {
                bx.border = e.non_negative();
            }
        }
        StyleKey::BorderStyle => {
            if let Some(e) = border_styles(doc, value) {
                bx.border_style = e;
            }
        }
        StyleKey::BorderColor => {
            if let Some(c) = color_of(doc, value) {
                bx.border_color = c;
            }
        }
        StyleKey::BorderRadius => {
            if let Some(v) = box_len(doc, value, em) {
                bx.radius = v.max(0.0);
            }
        }
        StyleKey::BackgroundColor => {
            if let Some(c) = color_of(doc, value) {
                bx.background = Some(c);
            }
        }
        StyleKey::TextAlign => {
            if let Some(a) = text_align_of(doc, value) {
                out.align = a;
            }
        }
        StyleKey::WhiteSpace => {
            if let Some(pre) = pre_line_of(doc, value) {
                out.pre_line = pre;
            }
        }
        _ => {}
    }
}

/// A box length, in the panel's own
/// pixels.
///
/// Unlike [`length_px`] a box length
/// may be zero and may be negative: a
/// `marginRight: 0` overriding a
/// wider rule is real - Jitendex
/// writes exactly that - and a
/// negative margin is legal CSS.
/// Filtering either out here would
/// silently leave the wider rule
/// standing.
fn box_len(doc: &GlossDoc, value: Scalar, em: f32) -> Option<f32> {
    match value {
        Scalar::Num(n) => finite(em * n as f32),
        Scalar::Text(span) => css_len(doc.span(span), em),
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// One to four box lengths, as
/// [`edges_of`] splits them.
fn box_edges(doc: &GlossDoc, value: Scalar, em: f32) -> Option<Edges<f32>> {
    match value {
        Scalar::Num(n) => Some(Edges::all(finite(em * n as f32)?)),
        Scalar::Text(span) => edges_of(doc.span(span), |s| css_len(s, em)),
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// One border style per edge.
///
/// The same shorthand grammar:
/// `border-style: none none none
/// solid` is how one real dictionary
/// draws a left rule and nothing else.
fn border_styles(doc: &GlossDoc, value: Scalar) -> Option<Edges<BorderStyle>> {
    edges_of(doc.scalar_str(value)?, border_style_of)
}

/// CSS's one-to-four-value edge
/// grammar, in CSS's own order.
///
/// One value is all four edges, two
/// are vertical then horizontal,
/// three name the fourth from its
/// opposite, and four are literal. A
/// fifth value makes the declaration
/// invalid, and so does any single
/// value this build cannot read: the
/// box keeps what it had rather than
/// taking half a shorthand.
fn edges_of<T: Copy>(text: &str, one: impl Fn(&str) -> Option<T>) -> Option<Edges<T>> {
    let mut parts = text.split_whitespace();
    let top = one(parts.next()?)?;
    let Some(right) = parts.next() else { return Some(Edges::all(top)) };
    let right = one(right)?;
    let Some(bottom) = parts.next() else {
        return Some(Edges { top, right, bottom: top, left: right });
    };
    let bottom = one(bottom)?;
    let Some(left) = parts.next() else {
        return Some(Edges { top, right, bottom, left: right });
    };
    let left = one(left)?;
    parts.next().is_none().then_some(Edges { top, right, bottom, left })
}

/// One `border-style` keyword.
///
/// `groove`, `ridge`, `inset` and
/// `outset` resolve to `solid`, which
/// is what a browser draws them as at
/// the hairline widths dictionaries
/// use - a border the author asked
/// for, drawn plainly, is closer than
/// none. `hidden` is `none` plus a
/// table border-conflict rule this
/// renderer has no cascade for.
fn border_style_of(word: &str) -> Option<BorderStyle> {
    Some(match word {
        "none" | "hidden" => BorderStyle::None,
        "solid" | "groove" | "ridge" | "inset" | "outset" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => return None,
    })
}

/// `textAlign`, as the scene carries
/// alignment.
///
/// `start` and `left` are one value
/// here and `end` and `right` another:
/// a Japanese-first product has no
/// bidi, by the spec's own Out of
/// Scope. `justify` keeps whatever
/// alignment it inherited rather than
/// quietly reading as `start`, because
/// there is no justification pass
/// behind it.
fn text_align_of(doc: &GlossDoc, value: Scalar) -> Option<Align> {
    Some(match doc.scalar_str(value)?.trim() {
        "start" | "left" => Align::Leading,
        "center" => Align::Center,
        "end" | "right" => Align::Trailing,
        _ => return None,
    })
}

/// `whiteSpace`, of which this build
/// reads one value.
///
/// `pre-line` preserves the
/// dictionary's own newlines and
/// still wraps, which is exactly what
/// both text engines do with a string
/// holding one - so honouring it
/// costs the paragraph's edge trim
/// and nothing else. `pre`,
/// `pre-wrap` and `nowrap` all turn
/// wrapping off in ways the
/// measurement seam has no request
/// for, so they leave the paragraph as
/// it was.
fn pre_line_of(doc: &GlossDoc, value: Scalar) -> Option<bool> {
    match doc.scalar_str(value)?.trim() {
        "pre-line" => Some(true),
        "normal" => Some(false),
        _ => None,
    }
}

/// A CSS length, in the panel's own
/// pixels.
///
/// A number is Yomitan's em-multiplier
/// convention, the same one the HTML
/// renderer prints with an `em`
/// suffix. A string carries its own
/// unit, which [`css_len`] reads.
/// Zero and below are no font size, so
/// a `fontSize: 0` leaves the
/// inherited one standing.
fn length_px(doc: &GlossDoc, value: Scalar, em: f32) -> Option<f32> {
    let px = match value {
        Scalar::Num(n) => em * n as f32,
        Scalar::Text(span) => css_len(doc.span(span), em)?,
        Scalar::Bool(_) | Scalar::Null => return None,
    };
    (px.is_finite() && px > 0.0).then_some(px)
}

/// One CSS length with its own unit,
/// sign and zero intact.
///
/// `em` and `%` are relative to the em
/// it sits in, and `px` is relative to
/// Yomitan's own base (see
/// [`YOMITAN_BASE_PX`]) so that an
/// absolute pixel scales with the
/// panel instead of shrinking on a
/// dense screen. A bare number is an
/// em multiplier, as the schema's own
/// numeric values are.
fn css_len(text: &str, em: f32) -> Option<f32> {
    let text = text.trim();
    let at = text
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+'))
        .unwrap_or(text.len());
    let n: f32 = text[..at].parse().ok()?;
    finite(match text[at..].trim() {
        "em" | "" => em * n,
        "%" => em * n / 100.0,
        "px" => em * n / YOMITAN_BASE_PX,
        _ => return None,
    })
}

/// A length a box can use.
fn finite(px: f32) -> Option<f32> {
    px.is_finite().then_some(px)
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
                // The row behind every
                // paragraph of it, which the
                // tree itself cannot know.
                // With the node path each
                // paragraph already carries,
                // this is what turns "select
                // the text" into "select sense
                // 3 of 大辞林" (stories 45
                // and 46).
                for flow in &mut flows {
                    flow.dict_id = block.dict_id;
                    flow.entry_id = entry.entry_id;
                }
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

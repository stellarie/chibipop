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
use crate::dict::gloss::{
    GlossDoc, ItemType, Kind, NodeId, NodePath, RoleFilter, Scalar, StyleKey, Tag,
};
use crate::dict::media::{Intrinsic, MediaFormat, MediaKey};
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
    /// One asset, composited inline.
    ///
    /// Its `rect` is the resolved
    /// image box and its `image` names
    /// the asset (see [`SceneImage`]);
    /// its `text` and `spans` are the
    /// `alt` fallback a bin draws when
    /// the asset will not decode. It
    /// advances no y, because the
    /// paragraph it sits inside
    /// already reserved its line.
    Image,
    /// A table's block container.
    ///
    /// Its `rect` is the whole grid,
    /// its `block_box` the `table`'s
    /// own declared box, and it has no
    /// text: every word in a table
    /// belongs to a cell. It leads the
    /// cells in draw order, so a
    /// table's own background sits
    /// under them.
    Table,
    /// One cell's box.
    ///
    /// Its `rect` and its `block_box`
    /// are the cell's border box, with
    /// the collapse already resolved
    /// into the box's edge widths
    /// ([`collapsed`]). Also textless:
    /// the cell's own paragraphs are
    /// the elements that follow it,
    /// each with its own address, so a
    /// cell holding two blocks is two
    /// runs inside one border.
    Cell,
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
            ElemKind::Image => "Image",
            ElemKind::Table => "Table",
            ElemKind::Cell => "Cell",
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

/// One list marker, hanging in its
/// list's gutter.
///
/// Out of the element's flow and out
/// of its text, for the same reason a
/// reading is, and it is the whole of
/// what CSS calls
/// `list-style-position: outside`: the
/// marker box sits *beside* the item's
/// principal box rather than inside
/// it, so it takes no horizontal room
/// from any line and every line of the
/// item - the first and each
/// continuation - starts at the item's
/// own indent. A marker kept in the
/// run would put the second line of a
/// wrapped item under the bullet,
/// because a [`SceneElem`] is one run
/// at one wrap width painted from one
/// origin (ADR-0013) and every line of
/// it therefore starts at the same x.
///
/// `x` and `y` are run-relative, like
/// the [`LineBox`] the marker was
/// placed against, so a bin adds
/// [`SceneElem::pen`] and draws. `x`
/// is normally *negative*: the gutter
/// is left of the content edge the pen
/// sits at, and the room for it is the
/// list's own [`LIST_INDENT_EM`],
/// which the item already paid for.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerRun {
    /// The marker, its own trailing
    /// gap included.
    pub text: String,
    /// Leading edge, run-relative and
    /// usually negative.
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

impl MarkerRun {
    /// The run a bin draws it from.
    ///
    /// One span and one line, at
    /// [`SceneElem::wrap_w`] - the
    /// width layout measured it at -
    /// so a marker too long for the
    /// panel breaks the same way in
    /// both places.
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

/// How an asset is meant to be
/// painted.
///
/// The schema's own two values. 15 of
/// the census's 72 dictionaries
/// declare `monochrome` over 117 687
/// nodes, and it means the asset is a
/// *mask*: black ink on transparency,
/// authored for a light page. Painted
/// as-is it is invisible on a dark
/// panel, which is why it is a
/// property the scene carries rather
/// than a detail the store keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Appearance {
    /// Composite the asset's own
    /// pixels. The schema's `auto`,
    /// and the absence of the field.
    #[default]
    Auto,
    /// A mask: tint it with the text
    /// colour it stands in for.
    Monochrome,
}

/// What a painter does with a
/// monochrome asset's pixels.
///
/// Decided here rather than in each
/// bin, so the two cannot disagree
/// about which assets take the
/// expensive path - and so the bound
/// is one table-driven test instead of
/// two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    /// Composite them unchanged.
    None,
    /// Multiply their alpha by the
    /// text colour, in place.
    Alpha,
    /// Rasterise the vector at this
    /// pixel size first, then multiply.
    Raster(u32, u32),
}

/// One asset, as a scene element names
/// it.
///
/// No pixels and no bytes: core has
/// the rect from the media row's
/// recorded intrinsic size and the key
/// to fetch bytes with, which is the
/// whole reason ticket 03 records
/// sizes at extraction time. Decoding
/// is the bin's, behind its own
/// surface cache.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneImage {
    /// The asset to composite, or
    /// `None` for the placeholder rung.
    ///
    /// `None` means the store had no
    /// row - no bytes, or no readable
    /// intrinsic size - *and* the node
    /// carried no `alt` text to put in
    /// the flow instead. The element is
    /// then a box a bin outlines, which
    /// is the last rung of the ladder
    /// and never nothing: nothing is a
    /// hole in a word.
    pub key: Option<MediaKey>,
    /// The format the media row
    /// recorded, which is what decides
    /// whether tinting has to rasterise
    /// a vector first.
    pub format: Option<MediaFormat>,
    pub appearance: Appearance,
    /// The schema's `background`.
    /// `false` paints no backing fill,
    /// which is what every image node
    /// in the census's samples asks
    /// for.
    pub background: bool,
    /// Read and carried, acted on by
    /// nothing: the spec builds no
    /// hover-to-reveal affordance, and
    /// dropping the fields would mean
    /// re-deriving them when it does.
    pub collapsed: bool,
    pub collapsible: bool,
}

impl SceneImage {
    /// What to do with this asset's
    /// pixels, given the box it
    /// resolved to and the em it sits
    /// in.
    ///
    /// Hoshi Reader's bound, and it
    /// exists because rasterise-then-
    /// tint is the expensive path: a
    /// vector has no pixels until
    /// something picks a size, and
    /// picking the panel's own would
    /// re-rasterise a 300x150 default-
    /// object SVG on every theme
    /// change. So the path is taken
    /// only for a gaiji-sized box -
    /// [`IMAGE_TINT_EM`] on both axes -
    /// at twice the device pixel ratio
    /// for legibility, clamped to
    /// [`IMAGE_TINT_CLAMP`] on the
    /// longest edge. A larger
    /// monochrome asset composites
    /// untinted, which is Hoshi
    /// Reader's answer too: an
    /// illustration is not a mask.
    pub fn tint(&self, rect: SceneRect, em: f32, dpr: f32) -> Tint {
        if self.appearance != Appearance::Monochrome {
            return Tint::None;
        }
        if self.format != Some(MediaFormat::Svg) {
            // A raster mask already has
            // pixels, so tinting it is
            // one pass over the surface
            // the cache already holds.
            return Tint::Alpha;
        }
        let bound = IMAGE_TINT_EM * em;
        if !(rect.w > 0.0 && rect.h > 0.0 && rect.w <= bound && rect.h <= bound) {
            return Tint::None;
        }
        let scale = IMAGE_TINT_SCALE * dpr.max(1.0);
        let (w, h) = (rect.w * scale, rect.h * scale);
        let longest = w.max(h);
        let fit = if longest > IMAGE_TINT_CLAMP { IMAGE_TINT_CLAMP / longest } else { 1.0 };
        Tint::Raster(
            (w * fit).round().max(1.0) as u32,
            (h * fit).round().max(1.0) as u32,
        )
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
    /// This element's list markers,
    /// each already positioned in the
    /// gutter of the list that owed
    /// it.
    ///
    /// Empty for everything but the
    /// first paragraph of a list item.
    /// More than one only when an
    /// item's whole content is a
    /// nested list, in which case the
    /// two levels' markers share the
    /// one line they have between them
    /// and hang in two different
    /// gutters - which is what a
    /// browser draws, and Jitendex's
    /// real shape. A marker is not in
    /// `text` and not in `spans`,
    /// because it does not flow - see
    /// [`MarkerRun`].
    pub marker: Vec<MarkerRun>,
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
    /// The asset an
    /// [`ElemKind::Image`] element
    /// composites, and `None` on every
    /// other kind.
    ///
    /// Separate from the kind because
    /// [`ElemKind`] is `Copy` and a
    /// [`MediaKey`] owns its path: the
    /// path is a dictionary-authored
    /// string that has to round-trip
    /// verbatim, so it is stored once
    /// and not copied per element per
    /// frame.
    pub image: Option<SceneImage>,
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

/// What the popup shows of an entry,
/// as the scene builder spends it.
///
/// The spec's "render settings are a
/// decision table, not a style
/// engine": Yomitan's own
/// configurability is a small fixed
/// set of root attributes driving
/// fixed CSS rules, and this is
/// chibipop's mirror of it -
/// `popup`'s six knobs resolved to
/// the four facts the gloss walk
/// actually spends. Resolved by
/// `PopupConfig::render_settings`, so
/// the enum-to-flag mapping lives
/// once and the walk never sees a
/// config type.
///
/// Read in exactly one place
/// ([`build_elements`]), which is
/// what keeps this a table rather
/// than a cascade: nothing below that
/// point consults a setting again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderSettings {
    /// Does a glossary list stack its
    /// items?
    ///
    /// `true` gives each item its own
    /// line, marked and indented;
    /// `false` joins them into one
    /// paragraph with
    /// [`ITEM_SEPARATOR`] between
    /// them, which is the terse popup
    /// chibipop drew before it could
    /// do better. The same parameter
    /// the list pass already had
    /// ([`Paragraphs::stack_items`]) -
    /// compact mode flips it and
    /// invents nothing.
    pub stack_items: bool,
    /// Do a dictionary's own
    /// declarations apply?
    ///
    /// Off draws every entry in the
    /// theme's own font and colours.
    /// One flag for both sources by
    /// construction: inline `style`
    /// and a dictionary's own
    /// `styles.css` land in the same
    /// resolved record, and
    /// [`Paragraphs::declarations`] is
    /// the single gate over it.
    pub styling: bool,
    /// Do image assets reach the
    /// panel?
    ///
    /// Off leaves the `alt` text: an
    /// image node is a *character*
    /// far more often than an
    /// illustration, so a node cut out
    /// whole would leave a hole in a
    /// word.
    pub images: bool,
    /// Which editorial roles reach the
    /// panel.
    ///
    /// The card renderer's own
    /// vocabulary, and a separate
    /// *value* of it: the popup's
    /// answer and the card's are two
    /// filters over one enum, which is
    /// what lets a user hide examples
    /// on screen and keep them on a
    /// mined card. The card's is
    /// [`RoleFilter::CARD`] and no
    /// setting reaches it.
    pub roles: RoleFilter,
}

impl Default for RenderSettings {
    /// What a fresh install draws: a
    /// stacked list, the dictionary's
    /// own styling, its images, and
    /// its editorial matter minus the
    /// part-of-speech labels the card
    /// already prints above the
    /// glosses.
    ///
    /// The same values `PopupConfig`'s
    /// own defaults resolve to - a
    /// test pins the two together -
    /// so a geometry fixture and a
    /// fresh install draw the same
    /// panel.
    fn default() -> RenderSettings {
        RenderSettings {
            stack_items: true,
            styling: true,
            images: true,
            roles: RoleFilter::CARD,
        }
    }
}

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
    /// How much of an entry to show.
    pub render: RenderSettings,
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

    let (elems, entries) =
        build_elements(req.presentation, theme, req.show_back, req.side_panel, req.render);
    let has_side = !entries.is_empty();
    let side_extra = if has_side {
        SIDE_GAP + SEPARATOR_THICKNESS + SIDE_GAP + SIDE_PANEL_W
    } else {
        0.0
    };
    let content_w = (req.max_w - 2.0 * pad - side_extra).max(0.0);

    let mut y = 0.0f32;
    let mut reserved_w = 0.0f32;
    let mut probes = Vec::new();
    // Every buffer the gloss walk
    // reuses, and the scene it is
    // filling. Bundled because a
    // table lays its cells out
    // through the same paragraph
    // pass the panel's own stream
    // uses, so the pass has to be
    // callable from two places
    // without threading eight
    // arguments through both.
    let mut pass = Pass {
        font,
        theme,
        measured: Measured::default(),
        run: Vec::new(),
        cover: Vec::new(),
        out: Vec::with_capacity(elems.len()),
        hits: Vec::new(),
    };

    for elem in &elems {
        let advance = match elem {
            Elem::Separator { top_gap } => {
                let h = theme.separator_height;
                y += top_gap;
                pass.out.push(SceneElem {
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
                    marker: Vec::new(),
                    block_box: None,
                    inline_boxes: Vec::new(),
                    origin: None,
                    image: None,
                });
                h
            }
            Elem::Corner(line) => {
                // Trailing-aligned: the box
                // hugs the right edge, and
                // the run is measured
                // pre-alignment, so x comes
                // from the width.
                let met = measure_line(m, font, line, content_w, &mut pass.measured)?;
                pass.out.push(SceneElem {
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
                    marker: Vec::new(),
                    block_box: None,
                    inline_boxes: Vec::new(),
                    origin: None,
                    image: None,
                });
                reserved_w = met.w + CORNER_GAP;
                0.0
            }
            Elem::Text(line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = measure_line(m, font, line, avail_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::Text, line, &met, origin, y, avail_w));
                h
            }
            Elem::Collapsed(idx, line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let met = measure_line(m, font, line, avail_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::Collapsed, line, &met, origin, y, avail_w));
                pass.hits.push(HitTarget {
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
                let met = measure_line(m, font, line, avail_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::Headword, line, &met, origin, y, avail_w));

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
                        pass.hits.push(HitTarget {
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
                pass.gloss(m, flow, (origin, origin + y), avail_w)?.1
            }
            Elem::Table(grid) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                pass.table(m, grid, (origin, origin + y), avail_w)?.1
            }
            Elem::BackButton(line) => {
                let met = measure_line(m, font, line, content_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::BackButton, line, &met, origin, y, content_w));
                pass.hits.push(HitTarget {
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
        Some(side_panel(&entries, theme, origin, content_w, m, &mut pass.measured)?)
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
                &mut pass.measured,
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
        elems: pass.out,
        hits: pass.hits,
        side,
        anki,
        used_h,
        content_h,
        view_h,
        panel_w,
    })
}

/// One popup's layout, in progress.
///
/// Two things at once, and
/// deliberately: the scene being
/// filled, and every buffer filling
/// it reuses. A gloss paragraph is
/// laid out from the panel's own
/// element stream *and* from inside a
/// table cell, so the code that does
/// it has to be callable twice - and
/// a free function would have carried
/// eight arguments, six of which
/// never change over a whole panel.
///
/// `'a` is the element stream's: the
/// spans handed to the seam borrow a
/// paragraph's own text, so the
/// scratch run cannot outlive the
/// paragraphs it was filled from.
struct Pass<'a> {
    font: &'a str,
    theme: &'a Theme,
    /// One paragraph's measured lines
    /// and span boxes.
    measured: Measured,
    /// One paragraph's spans, as the
    /// seam takes them.
    run: Vec<StyledSpan<'a>>,
    /// The per-line boxes one run of
    /// spans covers - a link's, for
    /// its hit targets, and a pill's,
    /// for its box.
    cover: Vec<Cover>,
    /// The scene so far, in draw
    /// order.
    out: Vec<SceneElem>,
    /// The targets it has earned.
    hits: Vec<HitTarget>,
}

impl<'a> Pass<'a> {
    /// One gloss paragraph, measured,
    /// placed and pushed.
    ///
    /// `at` is the top-left corner of
    /// the paragraph's margin box,
    /// before its own `top_gap`, and
    /// `avail_w` is what the container
    /// around it offers. Two answers
    /// come back: the width the
    /// paragraph would like - its ink
    /// plus everything its box puts
    /// beside it, which is what a
    /// table column's demand is built
    /// from - and the y the walk
    /// advances by, its gap included.
    fn gloss(
        &mut self,
        m: &mut dyn TextMeasure,
        flow: &'a Flow,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let (font, theme) = (self.font, self.theme);
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
        let (margin, border, padding) = (bx.margin, bx.border_used(), bx.padding);
        // Every list level above it,
        // added up and *outside* the
        // box: the indent is the
        // list's own padding
        // (`--list-padding1`), so it
        // shifts an item's border box
        // where the item's own padding
        // insets the text inside that
        // box. [`Block::indent`] says
        // why it cannot ride in the
        // box itself.
        let indent = flow.block.indent;
        let lead = Edges {
            top: margin.top + border.top + padding.top,
            right: margin.right + border.right + padding.right,
            bottom: margin.bottom + border.bottom + padding.bottom,
            left: indent + margin.left + border.left + padding.left,
        };
        let wrap_w = (avail_w - lead.horizontal()).max(1.0);

        // The whole paragraph in one
        // request: its spans wrap
        // together, so a bold word and
        // a normal one beside it in the
        // source share a line and the
        // paragraph rewraps as one unit
        // (ADR-0013).
        self.run.clear();
        self.run.extend(flow.styled_spans(font));
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
        let read = measure_readings(m, font, flow, wrap_w, &mut self.run)?;
        // The images next, for the
        // same reason and by the same
        // rule: an image occupies
        // inline space and grows the
        // line it sits on, and the
        // only thing that can do
        // either is a span the
        // measurer is charged for.
        measure_images(m, font, flow, wrap_w, &mut self.run)?;
        m.measure(MeasureRun { spans: &self.run, max_w: wrap_w }, &mut self.measured)?;
        // The markers last, and after
        // the paragraph rather than
        // before it, which is the
        // difference between a marker
        // and a reading: a reading
        // grows the line it sits over
        // and so has to be priced into
        // the run, while a marker hangs
        // in a gutter the list's indent
        // already reserved and changes
        // nothing about the wrap. Its
        // own run, its own width.
        let marks = measure_markers(m, font, flow, wrap_w)?;
        let met = self.measured.metrics;
        let top = at.1 + flow.top_gap;
        // Margins are in the advance,
        // and are *not* collapsed
        // against a sibling's: see
        // [`Flow::block`].
        let h = lead.top + met.h + padding.bottom + border.bottom + margin.bottom;
        let pen = (at.0 + lead.left, top + lead.top);
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
        let ruby = place_ruby(flow, &read, &self.measured, wrap_w, slack);
        // Each marker in the gutter of
        // the list that owed it, beside
        // the item's first line.
        let marker = place_markers(flow, &marks, &self.measured, lead.left, pen.0);
        // Each image over the spacer
        // run that bought its room.
        let images = place_images(flow, &self.measured, pen, line_at);

        let spans = flow
            .spans
            .iter()
            .zip(self.run.iter())
            .enumerate()
            .map(|(i, (s, asked))| {
                // A span that wrapped is
                // placed against the
                // first line it touches;
                // one that measured to
                // nothing keeps the
                // shift its em alone
                // decided.
                let (line, span_h) = first_box(&self.measured, i as u32);
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
            span_cover(&self.measured, &mut self.cover, |b| {
                flow.spans.get(b as usize).is_some_and(|s| s.link == i as u32)
            });
            for &(line, left, right, _) in &self.cover {
                let line = self.measured.lines[line as usize];
                self.hits.push(HitTarget {
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
            span_cover(&self.measured, &mut self.cover, |b| b >= run.from && b < run.to);
            let (p, b) = (run.style.padding, run.style.border_used());
            for &(line, left, right, span_h) in &self.cover {
                let line = self.measured.lines[line as usize];
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
        // A marker adds nothing to it,
        // although it hangs left of
        // `pen.0`: the room it sits in
        // is the list's own
        // `--list-padding1`, already
        // counted in `lead.left` and so
        // already in the width demand
        // this returns. Charging for it
        // again would make a table
        // column ask for two gutters.
        self.out.push(SceneElem {
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
            marker,
            block_box: bx.exists().then(|| ElemBox {
                rect: SceneRect {
                    x: at.0 + indent + margin.left,
                    y: top + margin.top,
                    w: (avail_w - indent - margin.horizontal()).max(0.0),
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
            image: None,
        });
        // After the paragraph, so an
        // asset composites over the
        // spacer run it stands on
        // rather than under it. Each
        // advances nothing: the line
        // the riser grew is already
        // in `h`.
        self.out.extend(images);
        Ok((lead.horizontal() + ink_w, flow.top_gap + h))
    }

    /// A run of pieces, stacked.
    ///
    /// What a table cell is: a block
    /// container holding paragraphs
    /// and, where a dictionary nested
    /// one, another table. Reports the
    /// widest piece and the total
    /// advance.
    fn block(
        &mut self,
        m: &mut dyn TextMeasure,
        pieces: &'a [Piece],
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let (mut want, mut y) = (0.0f32, 0.0f32);
        for piece in pieces {
            let (w, advance) = match piece {
                Piece::Flow(flow) => self.gloss(m, flow, (at.0, at.1 + y), avail_w)?,
                Piece::Table(grid) => self.table(m, grid, (at.0, at.1 + y), avail_w)?,
            };
            want = want.max(w);
            y += advance;
        }
        Ok((want, y))
    }

    /// How wide `pieces` would like to
    /// be, without keeping the layout
    /// that found out.
    ///
    /// A column's width comes from its
    /// cells' content and a cell's
    /// content cannot be measured
    /// without a width, so the grid
    /// lays each cell out once at the
    /// width every column shares,
    /// reads the extent, and rolls the
    /// elements and targets back off
    /// the scene. Truncating two
    /// vectors is the whole of the
    /// rollback, and it hands their
    /// capacity straight to the real
    /// pass.
    fn trial(
        &mut self,
        m: &mut dyn TextMeasure,
        pieces: &'a [Piece],
        avail_w: f32,
    ) -> Result<f32, MeasureError> {
        let (elems, hits) = (self.out.len(), self.hits.len());
        let want = self.block(m, pieces, (0.0, 0.0), avail_w)?.0;
        self.out.truncate(elems);
        self.hits.truncate(hits);
        Ok(want)
    }

    /// One table, as a grid.
    ///
    /// Four steps, in order, and none
    /// of them iterates: one rule
    /// width for the whole grid, the
    /// column widths from the cells'
    /// content ([`Pass::columns`]),
    /// every cell laid out at its
    /// column with its y deferred, and
    /// then the row heights, which is
    /// the first moment a cell's own
    /// top is known.
    ///
    /// The table element itself leads
    /// the cells in draw order, so a
    /// declared background sits under
    /// them; its geometry is filled in
    /// last, because a shrink-to-fit
    /// table is exactly as wide as the
    /// grid inside it.
    fn table(
        &mut self,
        m: &mut dyn TextMeasure,
        grid: &'a Grid,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let bx = grid.block.style;
        let (margin, border, padding) = (bx.margin, bx.border_used(), bx.padding);
        let indent = grid.block.indent;
        let lead = Edges {
            top: margin.top + border.top + padding.top,
            right: margin.right + border.right + padding.right,
            bottom: margin.bottom + border.bottom + padding.bottom,
            left: indent + margin.left + border.left + padding.left,
        };
        let avail = (avail_w - lead.horizontal()).max(1.0);
        let top = at.1 + grid.top_gap;
        let (x0, y0) = (at.0 + lead.left, top + lead.top);
        let table_at = self.out.len();
        self.out.push(box_elem(ElemKind::Table, grid.base, grid.block.align, grid.origin()));

        // One rule width for the whole
        // grid, the widest any cell
        // asked for. That *is*
        // `border-collapse`'s own
        // conflict rule - the wider
        // border wins the shared edge -
        // and one width per grid is
        // what makes the column
        // arithmetic a sum instead of a
        // negotiation.
        let rule = grid.cells.iter().fold(0.0f32, |w, cell| {
            let e = cell.style.border_used();
            w.max(e.top).max(e.right).max(e.bottom).max(e.left)
        });
        // A grid of `cols` columns has
        // `cols + 1` rules down it.
        let rules = rule * (grid.cols as f32 + 1.0);
        // Everything the columns have
        // between them. Never negative:
        // a table with more rules than
        // room collapses its columns
        // rather than reserving width
        // it does not have.
        let share = (avail - rules).max(0.0);
        let inner = self.columns(m, grid, rule, share)?;

        // Column edges: the *outside*
        // of the rule before each
        // column, and one past the
        // last. Accumulated once and
        // read twice, so two
        // neighbours share one number
        // and abut exactly - an edge
        // differenced back out of a
        // running sum does not, and
        // `(x + rule) - rule` is not
        // `x`.
        let mut ex = Vec::with_capacity(grid.cols + 1);
        let mut x = x0;
        for w in &inner {
            ex.push(x);
            x += rule + w;
        }
        ex.push(x);

        // Every cell at its column,
        // with its y deferred to zero:
        // a row is as tall as its
        // tallest cell, so no cell's
        // own top is known until every
        // cell in that row has been
        // measured.
        let mut tall = Vec::with_capacity(grid.cells.len());
        let mut placed = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let (c, across) = (cell.at.1, cell.span.1);
            let pad = cell.style.padding;
            // A cell spanning columns
            // gets them and the rules it
            // swallowed, less its own
            // padding. Summed from the
            // widths rather than taken
            // off `ex`, because a wrap
            // width one ulp short of its
            // own text breaks a line
            // that fits (see [`extent`]).
            let w = (extent(&inner, c, across, rule) - pad.horizontal()).max(1.0);
            let (elems, hits) = (self.out.len(), self.hits.len());
            self.out.push(box_elem(ElemKind::Cell, cell.base, grid.block.align, cell.origin(grid)));
            let advance = self.block(m, &cell.body, (ex[c] + rule + pad.left, 0.0), w)?.1;
            placed.push((elems, self.out.len(), hits, self.hits.len()));
            tall.push(pad.vertical() + advance);
        }

        let mut rows = vec![0.0f32; grid.rows];
        distribute(
            &mut rows,
            grid.cells.iter().zip(&tall).map(|(c, &h)| (c.at.0, c.span.0, h)),
            rule,
        );
        let mut ey = Vec::with_capacity(grid.rows + 1);
        let mut y = y0;
        for h in &rows {
            ey.push(y);
            y += rule + h;
        }
        ey.push(y);

        for (i, cell) in grid.cells.iter().enumerate() {
            let (r, c) = cell.at;
            let (down, across) = cell.span;
            let pad = cell.style.padding;
            let (from, to, hit_from, hit_to) = placed[i];
            // The content, dropped into
            // the row the grid grew for
            // it. Nothing stretches: a
            // cell shorter than its row
            // sits at the row's top,
            // which is Yomitan's own
            // `vertical-align: top`, and
            // every line box is still
            // the one the measurer
            // reported.
            shift(
                &mut self.out[from + 1..to],
                &mut self.hits[hit_from..hit_to],
                ey[r] + rule + pad.top,
            );
            let style =
                collapsed(cell.style, rule, c + across == grid.cols, r + down == grid.rows);
            let used = style.border_used();
            // A cell that draws a rule
            // starts on its outside and
            // one that does not starts
            // on its inside, so the box
            // holds exactly the rules
            // this cell owns and its
            // neighbour's box begins
            // where this one ends.
            let edge = |at: f32, own: f32| if own > 0.0 { at } else { at + rule };
            let (left, top) = (edge(ex[c], used.left), edge(ey[r], used.top));
            let rect = SceneRect {
                x: left,
                y: top,
                w: ex[c + across] + used.right - left,
                h: ey[r + down] + used.bottom - top,
            };
            let elem = &mut self.out[from];
            elem.wrap_w = (extent(&inner, c, across, rule) - pad.horizontal()).max(1.0);
            elem.pen = (ex[c] + rule + pad.left, ey[r] + rule + pad.top);
            elem.rect = rect;
            elem.block_box = Some(ElemBox { rect, style });
        }

        let grid_w = ex[grid.cols] + rule - x0;
        let grid_h = ey[grid.rows] + rule - y0;
        // Ticket 08's own arithmetic,
        // with the grid's height where
        // a paragraph's would be.
        let h = lead.top + grid_h + padding.bottom + border.bottom + margin.bottom;
        let elem = &mut self.out[table_at];
        elem.top_gap = grid.top_gap;
        elem.wrap_w = avail;
        elem.pen = (x0, y0);
        elem.rect = SceneRect { x: x0, y: y0, w: grid_w, h: grid_h };
        elem.advance = h;
        // Shrink-to-fit, as a table
        // with no declared width is in
        // a browser: the box is the
        // grid's own width and not the
        // container's.
        elem.block_box = bx.exists().then(|| ElemBox {
            rect: SceneRect {
                x: at.0 + indent + margin.left,
                y: top + margin.top,
                w: border.horizontal() + padding.horizontal() + grid_w,
                h: border.vertical() + padding.vertical() + grid_h,
            },
            style: bx,
        });
        Ok((lead.horizontal() + grid_w, grid.top_gap + h))
    }

    /// Every column's width, from the
    /// content of the cells in it.
    ///
    /// Fixed sizing in one pass, and
    /// deliberately not CSS
    /// auto-layout:
    ///
    /// 1. Each cell is laid out once
    ///    at `share` - the whole width
    ///    the columns have between
    ///    them - and what it measures
    ///    to plus its own horizontal
    ///    padding is what it asks for.
    /// 2. A column takes the widest
    ///    ask among the single-column
    ///    cells in it, and a cell
    ///    spanning several adds only
    ///    its shortfall, spread evenly
    ///    ([`distribute`]).
    /// 3. If the columns together
    ///    still want more than
    ///    `share`, every one is
    ///    multiplied by the single
    ///    factor that makes them fit
    ///    exactly.
    ///
    /// Step 3 is what keeps a table
    /// inside the panel: the columns
    /// shrink and their content
    /// rewraps, so a wide table can
    /// never ask the panel to widen
    /// for it. There is no second
    /// round and no negotiation - the
    /// only thing measured again after
    /// this is each cell at its final
    /// width, which is the height
    /// pass.
    fn columns(
        &mut self,
        m: &mut dyn TextMeasure,
        grid: &'a Grid,
        rule: f32,
        share: f32,
    ) -> Result<Vec<f32>, MeasureError> {
        let mut want = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let pad = cell.style.padding.horizontal();
            // An empty cell asks for
            // nothing and is not worth a
            // measurement - which is most
            // of a Jitendex forms table.
            let ink = if cell.body.is_empty() {
                0.0
            } else {
                self.trial(m, &cell.body, (share - pad).max(1.0))?
            };
            want.push(pad + ink);
        }
        let mut inner = vec![0.0f32; grid.cols];
        distribute(
            &mut inner,
            grid.cells.iter().zip(&want).map(|(c, &w)| (c.at.1, c.span.1, w)),
            rule,
        );
        let total: f32 = inner.iter().sum();
        if total > share {
            let scale = share / total;
            for col in &mut inner {
                *col *= scale;
            }
        }
        Ok(inner)
    }
}

// ---- tables ----

/// Yomitan's own table cell border,
/// as the spec's defaults table
/// states it: `1em / 14`, one pixel
/// at the base font size, so a cell
/// rule scales with the panel instead
/// of vanishing on a dense screen
/// (`.gloss-sc-th, .gloss-sc-td {
/// border-width: calc(1em /
/// var(--font-size-no-units)) }`).
///
/// A division rather than a
/// reciprocal constant, so that a
/// panel drawn at Yomitan's own base
/// size gets exactly the one pixel
/// the rule promises
/// ([`YOMITAN_BASE_PX`]).
fn cell_border(em: f32) -> f32 {
    em / YOMITAN_BASE_PX
}

/// Yomitan's own table cell padding,
/// as a fraction of the em it sits
/// in: `padding: 0.25em`, from the
/// same rule.
const CELL_PADDING: f32 = 0.25;

/// HTML's own cap on a cell's span.
///
/// The schema puts no bound on
/// `colSpan` and a grid costs memory
/// per column, so an absurd one is a
/// bill rather than a wide cell.
/// 1000 is the number HTML itself
/// clamps `colspan` to.
const MAX_SPAN: usize = 1000;

/// One piece of a row's gloss.
///
/// A table is not a paragraph and
/// cannot be flattened into one - its
/// cells are laid out on a grid, and
/// the grid has to measure each one
/// separately - so the block half of
/// the inline pass emits a sum type
/// rather than a list of paragraphs.
enum Piece {
    Flow(Flow),
    Table(Grid),
}

impl Piece {
    /// Sets the gap owed above it.
    fn top_gap(&mut self, gap: f32) {
        match self {
            Piece::Flow(flow) => flow.top_gap = gap,
            Piece::Table(grid) => grid.top_gap = gap,
        }
    }

    /// Stamps the term-bank row behind
    /// every paragraph of it, the ones
    /// inside cells included.
    ///
    /// The tree itself does not know
    /// which row stored it, so this is
    /// [`build_elements`]' job - and a
    /// cell's paragraph needs the same
    /// address as any other, or a hit
    /// inside a conjugation table
    /// resolves to no sense at all.
    fn stamp(&mut self, dict_id: i64, entry_id: i64) {
        match self {
            Piece::Flow(flow) => {
                flow.dict_id = dict_id;
                flow.entry_id = entry_id;
            }
            Piece::Table(grid) => {
                grid.dict_id = dict_id;
                grid.entry_id = entry_id;
                for cell in &mut grid.cells {
                    for piece in &mut cell.body {
                        piece.stamp(dict_id, entry_id);
                    }
                }
            }
        }
    }
}

/// One table cell, before the grid
/// measures it.
struct Cell {
    /// Its content. A cell is a block
    /// container, so it holds
    /// paragraphs - and, where a
    /// dictionary nested one, a table.
    body: Vec<Piece>,
    /// Its box: Yomitan's own grid
    /// defaults with the dictionary's
    /// declarations over them. The
    /// grid resolves the collapse into
    /// it once the rule width is known
    /// ([`collapsed`]).
    style: BoxStyle,
    /// The style its own element
    /// draws in. It carries no text,
    /// so only the cull slack rides on
    /// this.
    base: Inline,
    /// `(row, column)`, filled in by
    /// [`place`].
    at: (usize, usize),
    /// `(rowSpan, colSpan)`, at least
    /// one each and already clipped to
    /// the grid by [`place`].
    span: (usize, usize),
    path: Option<NodePath>,
}

impl Cell {
    /// Where in its dictionary it came
    /// from.
    fn origin(&self, grid: &Grid) -> GlossOrigin {
        GlossOrigin { dict_id: grid.dict_id, entry_id: grid.entry_id, path: self.path }
    }
}

/// One table, before it is measured.
///
/// Structure only: which cell sits in
/// which slot needs no measurement,
/// so it is decided while the tree is
/// walked. What the grid pass adds is
/// the column widths, the row
/// heights, and where the whole thing
/// lands.
struct Grid {
    /// Gap owed above it.
    top_gap: f32,
    /// Every cell, in document order,
    /// each carrying the slot it was
    /// placed in.
    cells: Vec<Cell>,
    rows: usize,
    cols: usize,
    /// The `table`'s own box, indent
    /// and alignment.
    block: Block,
    /// The style its own element
    /// draws in.
    base: Inline,
    path: Option<NodePath>,
    dict_id: i64,
    entry_id: i64,
}

impl Grid {
    /// Where in its dictionary it came
    /// from.
    fn origin(&self) -> GlossOrigin {
        GlossOrigin { dict_id: self.dict_id, entry_id: self.entry_id, path: self.path }
    }
}

/// Puts every cell in a slot.
///
/// HTML's own placement, minus the
/// parts a dictionary never writes: a
/// row's cells take the leftmost
/// column no `rowSpan` from above has
/// already claimed, and each then
/// claims `colSpan` columns across
/// and `rowSpan` rows down. A span
/// reaching past the last row ends at
/// it, so every cell's slot is inside
/// the grid it will be measured
/// against.
///
/// The grid is as wide as the
/// furthest column any row reached,
/// so a row holding more cells than
/// its neighbours widens the table
/// and the short rows simply end
/// early. That is the "renders what
/// it can" a malformed table is owed:
/// no cell is dropped and no index
/// can run off the end.
fn place(rows: Vec<Vec<Cell>>) -> (Vec<Cell>, usize, usize) {
    let count = rows.len();
    // How far down each column is
    // already claimed, as the row
    // index one past the last claimed.
    let mut claimed: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    for (r, row) in rows.into_iter().enumerate() {
        let mut c = 0usize;
        for mut cell in row {
            while claimed.get(c).is_some_and(|&to| to > r) {
                c += 1;
            }
            let down = cell.span.0.min(count - r);
            let across = cell.span.1;
            cell.at = (r, c);
            cell.span = (down, across);
            if claimed.len() < c + across {
                claimed.resize(c + across, 0);
            }
            for to in &mut claimed[c..c + across] {
                *to = r + down;
            }
            c += across;
            out.push(cell);
        }
    }
    let cols = claimed.len();
    (out, count, cols)
}

/// A cell's `rowSpan` or `colSpan`.
///
/// The schema writes these as
/// numbers, which is also the only
/// form the Anki HTML renderer reads
/// off them. Anything else, and any
/// value below one, is one cell:
/// HTML's own parser treats a missing
/// or zero span that way, and a cell
/// occupying no slot has nowhere to
/// draw.
fn span_of(doc: &GlossDoc, id: NodeId, key: &str) -> usize {
    match doc.attr_of(id, key) {
        Some(Scalar::Num(n)) if n >= 1.0 => (n as usize).min(MAX_SPAN),
        _ => 1,
    }
}

/// The `len` tracks from `at`, plus
/// the rules between them.
///
/// What a cell spanning several
/// columns is offered, and what a cell
/// spanning several rows fills. Summed
/// from the track sizes rather than
/// differenced from two accumulated
/// edges, and that is load-bearing:
/// `(x0 + w + rule) - x0 - rule` is a
/// float ulp short of `w`, and a cell
/// one ulp narrower than its own text
/// wraps a line it should not.
fn extent(tracks: &[f32], at: usize, len: usize, rule: f32) -> f32 {
    tracks[at..at + len].iter().sum::<f32>() + (len as f32 - 1.0) * rule
}

/// Grows `tracks` until every span
/// fits.
///
/// The one rule the columns and the
/// rows share, so it is written once:
/// a track takes the widest ask among
/// the cells that live only in it,
/// and a cell across several tracks
/// then adds only its *shortfall* -
/// what it needs beyond the tracks it
/// already covers, the `rule` between
/// them included - spread evenly over
/// them.
///
/// Spanning cells are visited once,
/// in document order, against the
/// tracks as the cells before them
/// left them. There is no fixpoint
/// and no second round: two passes
/// over the cells, and the answer.
fn distribute(
    tracks: &mut [f32],
    spans: impl Iterator<Item = (usize, usize, f32)> + Clone,
    rule: f32,
) {
    for (at, len, want) in spans.clone() {
        if len == 1 {
            tracks[at] = tracks[at].max(want);
        }
    }
    for (at, len, want) in spans {
        if len < 2 {
            continue;
        }
        let n = len as f32;
        let have = extent(tracks, at, len, rule);
        if want > have {
            let add = (want - have) / n;
            for track in &mut tracks[at..at + len] {
                *track += add;
            }
        }
    }
}

/// One cell's box, with the shared
/// edges resolved.
///
/// `border-collapse: collapse` draws
/// the rule between two cells once,
/// so each cell owns the rules on its
/// left and its top and only the
/// cells against the grid's right and
/// bottom edges owe the closing ones.
/// Two neighbours therefore abut
/// exactly, with one `rule` between
/// them rather than two beside each
/// other - and a spanning cell simply
/// does not draw the rules it
/// swallowed.
///
/// An edge whose own `border-style`
/// is `none` still draws nothing: the
/// grid keeps the slot it reserved
/// and leaves it empty, exactly as a
/// browser does.
fn collapsed(mut style: BoxStyle, rule: f32, last_col: bool, last_row: bool) -> BoxStyle {
    let own = style.border_used();
    let edge = |own: f32, draws: bool| if own > 0.0 && draws { rule } else { 0.0 };
    style.border = Edges {
        top: edge(own.top, true),
        right: edge(own.right, last_col),
        bottom: edge(own.bottom, last_row),
        left: edge(own.left, true),
    };
    style
}

/// Moves a laid-out run of elements,
/// and the targets they earned, down
/// by `dy`.
///
/// A cell is laid out before its
/// row's height is known, because the
/// row is as tall as its tallest
/// cell, so the grid places every
/// cell at the table's own top and
/// drops it into its row afterwards.
///
/// Only origins move. A line box is
/// the measurer's answer and a
/// reading's position is
/// run-relative, so nothing a bin
/// re-measures changes - which is the
/// rule [`measure_readings`] sets and
/// the reason a row's extra height
/// never reaches the text inside it.
fn shift(elems: &mut [SceneElem], hits: &mut [HitTarget], dy: f32) {
    for elem in elems {
        elem.pen.1 += dy;
        elem.rect.y += dy;
        if let Some(b) = &mut elem.block_box {
            b.rect.y += dy;
        }
        for b in &mut elem.inline_boxes {
            b.rect.y += dy;
        }
    }
    for hit in hits {
        hit.y += dy;
    }
}

/// A textless element, before the
/// grid knows where it goes.
///
/// A table and a cell are boxes
/// rather than runs: they carry no
/// text and no spans, and their
/// geometry is the grid's answer
/// rather than the measurer's. Each
/// is pushed as a placeholder because
/// a container has to lead its own
/// contents in draw order while its
/// extent is only known once they
/// have all been laid out.
fn box_elem(kind: ElemKind, base: Inline, align: Align, origin: GlossOrigin) -> SceneElem {
    SceneElem {
        kind,
        text: String::new(),
        color: base.color,
        font_size: base.size,
        weight: base.weight,
        italic: base.italic,
        top_gap: 0.0,
        wrap_w: 0.0,
        align,
        pen: (0.0, 0.0),
        rect: SceneRect::default(),
        lines: 0,
        advance: 0.0,
        spans: Vec::new(),
        ruby: Vec::new(),
        marker: Vec::new(),
        block_box: None,
        inline_boxes: Vec::new(),
        origin: Some(origin),
        image: None,
    }
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
        marker: Vec::new(),
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
        // Chrome composites no asset.
        image: None,
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

/// One list marker, measured.
///
/// What [`measure_markers`] learns and
/// [`place_markers`] spends. The
/// baseline is absolute rather than a
/// share of the height, because that
/// is what a marker needs: it is set
/// on the item's own first baseline,
/// so the two baselines are subtracted
/// from each other directly.
#[derive(Clone, Copy, Default)]
struct MarkBox {
    w: f32,
    h: f32,
    /// How far below the run's top edge
    /// the marker's baseline sits.
    baseline: f32,
}

/// Every marker of a paragraph,
/// measured.
///
/// One run each, and nothing here
/// touches the paragraph's own run.
/// That is the difference between this
/// and [`measure_readings`], which has
/// to run first because a reading
/// grows the line it sits over: a
/// marker occupies the gutter the
/// list's [`LIST_INDENT_EM`] already
/// reserved, takes no horizontal room
/// from any line, and grows nothing.
/// So it needs no filler span, no
/// re-measure, and no edit to a line
/// box after the wrap - which is the
/// rule a bin's own re-measure
/// enforces (ADR-0013).
///
/// Measured at the paragraph's wrap
/// width and not at the gutter's: a
/// marker is one counter or one
/// authored string and is free to
/// overhang the gutter, exactly as a
/// browser lets `10.` overhang a
/// narrow one.
fn measure_markers(
    m: &mut dyn TextMeasure,
    font: &str,
    flow: &Flow,
    max_w: f32,
) -> Result<Vec<MarkBox>, MeasureError> {
    if flow.marker.is_empty() {
        return Ok(Vec::new());
    }
    // Only a paragraph that carries a
    // marker pays for this buffer,
    // which is why it is not one of
    // the walk's.
    let mut scratch = Measured::default();
    let mut out = vec![MarkBox::default(); flow.marker.len()];
    for (slot, mark) in flow.marker.iter().enumerate() {
        m.measure(
            MeasureRun { spans: &[marker_span(font, mark)], max_w },
            &mut scratch,
        )?;
        let first = scratch.lines.first().copied().unwrap_or_default();
        out[slot] =
            MarkBox { w: scratch.metrics.w, h: scratch.metrics.h, baseline: first.baseline };
    }
    Ok(out)
}

/// The paragraph's markers, placed.
///
/// Pure, and it reads exactly two
/// things the measurer already
/// reported: the first line's box, for
/// the baseline the marker shares with
/// the item's first line, and each
/// marker's own measured width.
///
/// `lead_left` is the paragraph's own
/// left lead - every list level above
/// it plus its own margin, border and
/// padding - and `pen_x` is where that
/// lead put the pen. A marker's box is
/// right-aligned against the content
/// edge of the *list* that owed it,
/// which is [`FlowMarker::indent`]:
/// that is where a browser puts an
/// `outside` marker, and it is why an
/// item's own `paddingLeft` moves the
/// item's text and leaves its bullet
/// where the list drew it. The
/// marker's trailing [`MARKER_GAP`] is
/// inside the box, so what separates
/// the two is the gap it always was -
/// now spent in the gutter instead of
/// on the line.
///
/// No alignment slack: `textAlign`
/// moves line boxes, and a marker box
/// is not one.
fn place_markers(
    flow: &Flow,
    boxes: &[MarkBox],
    measured: &Measured,
    lead_left: f32,
    pen_x: f32,
) -> Vec<MarkerRun> {
    let Some(line) = measured.lines.first().copied() else {
        return Vec::new();
    };
    flow.marker
        .iter()
        .zip(boxes)
        .filter(|(_, box_)| box_.h > 0.0)
        .map(|(mark, box_)| MarkerRun {
            text: mark.text.clone(),
            // Never off the panel's own
            // left edge: a marker wider
            // than every gutter above it
            // is drawn against that edge
            // rather than outside the
            // surface.
            x: (mark.indent - lead_left - box_.w).max(-pen_x),
            y: line.y + line.baseline - box_.baseline,
            w: box_.w,
            h: box_.h,
            size: mark.style.size,
            color: mark.style.color,
            weight: mark.style.weight,
            italic: mark.style.italic,
        })
        .collect()
}

/// One marker, as the seam takes it.
fn marker_span<'a>(font: &'a str, mark: &'a FlowMarker) -> StyledSpan<'a> {
    StyledSpan {
        text: &mark.text,
        font,
        size: mark.style.size,
        weight: mark.style.weight,
        italic: mark.style.italic,
        color: mark.style.color,
    }
}

/// Every image of a paragraph, given
/// the room it needs.
///
/// Called *before* the paragraph is
/// measured, and it is the whole of
/// how an inline image occupies a
/// line. The measurement seam takes
/// styled spans and no boxes
/// (ADR-0013), so a replaced element
/// can only take room by *being* a
/// span the measurer charges for - and
/// editing the line boxes afterwards
/// would fool nobody, because both
/// bins re-measure an element's own
/// spans to paint it and would get the
/// ungrown lines back. Ticket 11's
/// ruby filler is the same trick for
/// the same reason.
///
/// So the run asks, and this decides
/// what it asks for. Two ratios are
/// needed and only a measurer knows
/// either: what one [`IMAGE_SPACER`]
/// advances per unit of size, and how
/// far down a line its baseline sits.
/// One probe answers both.
///
/// Then the arithmetic. `n` spacers at
/// size `s` advance `n * u * s`, so
/// the width the ladder resolved fixes
/// `s` exactly. A span of size `r`
/// gives `asc * r` to the space above
/// its line's baseline, so the height
/// fixes the riser. The spacer is
/// capped at the riser, which is what
/// keeps a wide short banner from
/// making its line as tall as it is
/// wide: past that cap the reservation
/// comes out a few percent narrow
/// instead ([`IMAGE_SPACERS_PER_ASPECT`]).
fn measure_images(
    m: &mut dyn TextMeasure,
    font: &str,
    flow: &Flow,
    max_w: f32,
    run: &mut [StyledSpan<'_>],
) -> Result<(), MeasureError> {
    if flow.images.is_empty() {
        return Ok(());
    }
    // Only a paragraph holding an image
    // pays for this buffer, which is
    // why it is not one of the walk's.
    let mut scratch = Measured::default();
    for (slot, img) in flow.images.iter().enumerate() {
        let probe = StyledSpan {
            text: IMAGE_SPACER,
            font,
            size: img.em,
            weight: img.style.weight,
            italic: img.style.italic,
            color: img.style.color,
        };
        m.measure(MeasureRun { spans: &[probe], max_w }, &mut scratch)?;
        // The span's own box, not
        // `metrics.w`: DirectWrite's
        // aggregate width excludes
        // trailing whitespace, and a
        // lone no-break space is
        // nothing but that.
        let per_size = |px: f32| if img.em > 0.0 { px / img.em } else { 0.0 };
        let advance = per_size(scratch.spans.first().map_or(0.0, |b| b.w));
        let ascent = per_size(scratch.lines.first().map_or(0.0, |l| l.baseline));
        // A raised image needs its rise
        // above the baseline as well as
        // its own height; a lowered one
        // hangs into the descent, which
        // is what a lowered span does
        // too and what neither reserves.
        let rise = img.h + img.style.shift.max(0.0);
        let riser = if ascent > 0.0 { rise / ascent } else { rise };
        // A measurer that charges
        // nothing for a no-break space
        // can reserve nothing exactly,
        // so the spacer keeps the em it
        // was built with: some room for
        // the image beats none.
        let spacer = if advance > 0.0 && img.spacers > 0 {
            (img.w / (advance * img.spacers as f32)).min(riser)
        } else {
            img.em
        };
        for (span, asked) in flow.spans.iter().zip(run.iter_mut()) {
            if span.image != slot as u32 {
                continue;
            }
            asked.size = if span.filler { riser } else { spacer };
        }
    }
    Ok(())
}

/// The paragraph's images, as elements.
///
/// Pure: the lines already carry the
/// room [`measure_images`] bought, so
/// this only reads back where each
/// spacer landed and hangs the image
/// off that line's baseline.
///
/// One element per image rather than a
/// span of the paragraph, because an
/// image is a replaced element: it has
/// a rect and a media key and no text
/// (`ElemKind::Image`). Its `advance`
/// is zero - the paragraph's own
/// advance already counts the line the
/// riser grew - so it stacks nothing
/// and shifts nothing after it.
fn place_images(
    flow: &Flow,
    measured: &Measured,
    pen: (f32, f32),
    line_at: impl Fn(LineBox) -> f32,
) -> Vec<SceneElem> {
    let mut out = Vec::new();
    for (slot, img) in flow.images.iter().enumerate() {
        let spacer = |b: &&SpanBox| {
            flow.spans
                .get(b.span as usize)
                .is_some_and(|s| s.image == slot as u32 && !s.filler)
        };
        // The first line the reservation
        // landed on. Its spacers are
        // non-breaking glue, so they
        // cannot be split across two -
        // but a measurer that overflows
        // rather than looping may still
        // report one box per fragment.
        let Some(line) = measured.spans.iter().filter(spacer).map(|b| b.line).min() else {
            continue;
        };
        let Some(geom) = measured.lines.get(line as usize) else {
            continue;
        };
        let left = measured
            .spans
            .iter()
            .filter(spacer)
            .filter(|b| b.line == line)
            .fold(f32::MAX, |l, b| l.min(b.x));
        // Its bottom on the line's own
        // baseline, raised by whatever
        // `verticalAlign` asked for -
        // ticket 07's own resolution,
        // against the line the image
        // landed on, with the image's box
        // as the span height a
        // line-relative value aligns.
        let shift = shift_on(img.style, *geom, img.h);
        let rect = SceneRect {
            x: line_at(*geom) + left,
            y: pen.1 + geom.y + geom.baseline - shift - img.h,
            w: img.w,
            h: img.h,
        };
        // The `alt` fallback as one
        // ordinary span, so a bin that
        // cannot decode the asset draws
        // this element exactly as it
        // draws any other and needs no
        // second text path.
        let spans = if img.alt.is_empty() {
            Vec::new()
        } else {
            vec![ElemSpan {
                at: 0,
                len: img.alt.len() as u32,
                color: img.style.color,
                size: img.em,
                weight: img.style.weight,
                italic: img.style.italic,
                shift: 0.0,
            }]
        };
        out.push(SceneElem {
            kind: ElemKind::Image,
            text: img.alt.clone(),
            color: img.style.color,
            font_size: img.em,
            weight: img.style.weight,
            italic: img.style.italic,
            top_gap: 0.0,
            wrap_w: img.w.max(1.0),
            align: Align::Leading,
            pen: (rect.x, rect.y),
            rect,
            lines: 0,
            advance: 0.0,
            spans,
            ruby: Vec::new(),
            marker: Vec::new(),
            block_box: None,
            inline_boxes: Vec::new(),
            origin: Some(GlossOrigin {
                dict_id: flow.dict_id,
                entry_id: flow.entry_id,
                path: img.path,
            }),
            image: Some(img.scene.clone()),
        });
    }
    out
}

/// One image's box, by the ladder.
///
/// What the node declared, then what
/// the build recorded, then a square
/// of the text it sits in. No decode
/// at any rung - that is the whole
/// reason ticket 03 reads an intrinsic
/// size out of the container header at
/// extraction time.
///
/// The middle arms are the ones that
/// earn their keep. `height: 1em` and
/// no width is the shape 字通 and
/// 三省堂 both write, and one length
/// plus a ratio is the other length -
/// which is why the media row carries
/// `aspect` as its own column rather
/// than dividing on read. With no
/// ratio to hand a single length gives
/// a square, which at least sits on
/// the line at the size the dictionary
/// asked for.
fn image_size(
    doc: &GlossDoc,
    id: NodeId,
    em: f32,
    recorded: Option<Intrinsic>,
) -> (f32, f32) {
    // `em` multiplies the text the
    // image sits in; `px` is a scene
    // pixel. The absent field is `em`,
    // because the schema's own numeric
    // lengths are em multipliers - the
    // same convention [`length_px`]
    // reads - and Yomitan renders
    // `width`/`height` as ems.
    let unit = match doc.attr_of(id, "sizeUnits").and_then(|v| doc.scalar_str(v)) {
        Some("px") => 1.0,
        _ => em,
    };
    let declared = |name| image_len(doc, id, name).map(|n| (n * unit).min(IMAGE_MAX_PX));
    let aspect = recorded.map(|size| size.aspect).filter(|a| a.is_finite() && *a > 0.0);
    match (declared("width"), declared("height")) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w / aspect.unwrap_or(1.0)),
        (None, Some(h)) => (h * aspect.unwrap_or(1.0), h),
        (None, None) => match recorded {
            Some(size) => (size.width, size.height),
            None => (em * IMAGE_FALLBACK_EM, em * IMAGE_FALLBACK_EM),
        },
    }
}

/// One declared length, as a bare
/// number.
///
/// The schema's `width` and `height`
/// are numbers whose unit is the
/// node's `sizeUnits`, so this is not
/// [`css_len`]'s job: there is no unit
/// suffix to read here. A string is
/// still accepted, because a
/// dictionary converter writing `"1"`
/// meant one. Zero and below are no
/// size at all, which drops to the
/// next rung rather than collapsing
/// the image.
fn image_len(doc: &GlossDoc, id: NodeId, name: &str) -> Option<f32> {
    let value = doc.attr_of(id, name)?;
    let n = match value {
        Scalar::Num(n) => n as f32,
        _ => doc.scalar_str(value)?.trim().parse::<f32>().ok()?,
    };
    finite(n).filter(|n| *n > 0.0)
}

/// The text an image stands in for.
///
/// `alt` then `title`, and each from
/// the node's attributes then from its
/// `data` map. Both places are real:
/// 三省堂 writes `title` beside
/// `sizeUnits` as an attribute, and
/// Jitendex writes
/// `data: {"gaiji": "", "alt":
/// "［対義語］"}` - so reading one
/// place would lose one dictionary's
/// answer to the same question.
fn image_alt(doc: &GlossDoc, id: NodeId) -> String {
    for name in ["alt", "title"] {
        for found in [doc.attr_of(id, name), doc.data_of(id, name)] {
            match found.and_then(|v| doc.scalar_str(v)) {
                Some(text) if !text.trim().is_empty() => return text.trim().to_string(),
                _ => {}
            }
        }
    }
    String::new()
}

/// `appearance`, of which the schema
/// has two values.
fn image_appearance(doc: &GlossDoc, id: NodeId) -> Appearance {
    match doc.attr_of(id, "appearance").and_then(|v| doc.scalar_str(v)) {
        Some("monochrome") => Appearance::Monochrome,
        _ => Appearance::Auto,
    }
}

/// One boolean image field.
///
/// `None` for an absent field and for
/// one this build cannot read, so each
/// caller states its own default
/// rather than sharing a wrong one.
fn image_flag(doc: &GlossDoc, id: NodeId, name: &str) -> Option<bool> {
    match doc.attr_of(id, name)? {
        Scalar::Bool(b) => Some(b),
        _ => None,
    }
}

/// How many [`IMAGE_SPACER`]s one
/// image reserves with.
///
/// From the aspect ratio alone, so it
/// is decided while the paragraph is
/// built and is the same number in
/// every font - the *size* of them is
/// what [`measure_images`] solves
/// against the face that is actually
/// installed.
fn image_spacers(w: f32, h: f32) -> usize {
    if !(w > 0.0 && h > 0.0) {
        return 1;
    }
    let wanted = (IMAGE_SPACERS_PER_ASPECT * w / h).ceil();
    if !wanted.is_finite() || wanted < 1.0 {
        return 1;
    }
    (wanted as usize).min(IMAGE_SPACER_MAX)
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
    /// One table of a gloss tree.
    ///
    /// A grid rather than a run: its
    /// cells are measured and placed
    /// by [`Pass::table`], which is
    /// the only element that produces
    /// more than one [`SceneElem`].
    Table(Grid),
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

/// The two font sizes a CSS length
/// can be a fraction of.
///
/// `em` and `%` are the element's own
/// and `rem` is the document root's -
/// two different numbers on any node
/// that changed its size, so one
/// `f32` cannot answer both.
///
/// The popup's root is the theme's
/// body size, because that is what
/// Yomitan's root font size is.
/// `ext/css/display.css` only
/// *declares* `--font-size: calc(1px
/// * 14)` and applies it to no
/// element; the size that reaches the
/// document comes from
/// `ext/js/display/display.js`'s
/// `setFontOptions`, which writes the
/// reader's own font-size setting onto
/// `documentElement.style.fontSize`.
/// So `0.4rem` is four tenths of the
/// body text a Yomitan reader chose,
/// and it scales when they scale the
/// popup. Pinning it to
/// [`YOMITAN_BASE_PX`] would freeze it
/// at Yomitan's default instead -
/// right for a reader who never
/// touched the setting, wrong for
/// everyone else, and the same
/// mistake the `px` arm of
/// [`css_len`] exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ems {
    /// The element's own resolved font
    /// size, which `em` and `%` are a
    /// fraction of.
    own: f32,
    /// The panel's body size: the
    /// popup's root font size, which
    /// `rem` is a fraction of.
    root: f32,
}

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

/// An image's index in
/// [`Flow::images`], or no image at
/// all.
const NO_IMAGE: u32 = u32::MAX;

/// What an image's inline room is
/// bought with: U+00A0 NO-BREAK
/// SPACE.
///
/// An inline image is a replaced
/// element, and the measurement seam
/// takes styled spans and no boxes
/// (ADR-0013) - so the only way an
/// image can occupy room on a line is
/// to *be* a span the measurer
/// charges for. Growing the line
/// boxes afterwards would fool
/// nobody: both bins re-measure an
/// element's own spans to paint it and
/// would get the ungrown lines back.
///
/// Three properties earn this
/// character the job. It carries no
/// ink, so nothing shows through a
/// transparent asset. It has an
/// advance, which is the whole point.
/// And it is *non-breaking glue* in
/// UAX #14, so no wrap can split one
/// image's reservation across two
/// lines, and no wrap can separate it
/// from the word it belongs to - an
/// image mid-sentence therefore wraps
/// with the text and forces no break.
///
/// Not U+2060: that has zero advance,
/// which is why it is the *riser*
/// below and not this.
const IMAGE_SPACER: &str = "\u{a0}";

/// What an image's line height is
/// bought with, and it is
/// [`RUBY_FILLER`]'s character for
/// [`RUBY_FILLER`]'s reason: zero
/// advance, no break opportunity, and
/// its own size sets its line's height.
///
/// A separate span from
/// [`IMAGE_SPACER`] because a span's
/// advance and its line height are
/// both its size, and an image needs
/// them decided independently: a wide
/// short banner must not make its line
/// as tall as it is wide.
///
/// It also earns the image its
/// paragraph. [`IMAGE_SPACER`] is
/// *whitespace*, so a paragraph
/// holding nothing but an image would
/// measure as empty and be dropped
/// ([`Paragraphs::flush`]); U+2060 is
/// not whitespace, so the riser is
/// what says the paragraph has
/// content. [`trim`] needs no such
/// guard: it trims a named set of
/// space characters that U+00A0 is
/// deliberately not in.
const IMAGE_RISER: &str = RUBY_FILLER;

/// No-break spaces per unit of an
/// image's aspect ratio.
///
/// The count is fixed while the
/// paragraph is built and the *size*
/// is solved once the measurer has
/// been asked, because only it knows
/// what one of these advances
/// ([`measure_images`]). The count
/// still has to be generous enough
/// that the solved size stays under
/// the size the riser asked for -
/// otherwise the spacer, not the
/// image, would decide the line's
/// height.
///
/// Four is that bound for every real
/// face: a no-break space is a space,
/// a space is between a quarter and a
/// third of an em, and a face's ascent
/// is at most 0.85 em - so a space is
/// never less than a quarter of the
/// ascent one of these has to fit
/// inside. Where a face does go
/// narrower, [`measure_images`] clamps
/// the size instead of letting the
/// line grow, and the reservation
/// comes out a few percent short of
/// the image rather than the line
/// coming out several times too tall.
const IMAGE_SPACERS_PER_ASPECT: f32 = 4.0;

/// The most no-break spaces one image
/// may reserve with.
///
/// A dictionary's declared size is
/// arbitrary author input, so the
/// aspect ratio is too, and 64 spans
/// per image is already far past any
/// asset a dictionary ships. Beyond
/// it the reservation is short, which
/// costs the image some of its room
/// and costs the panel nothing.
const IMAGE_SPACER_MAX: usize = 64;

/// The box a monochrome vector may
/// have and still be rasterised for
/// tinting, per axis, in ems.
///
/// Hoshi Reader's bound. A gaiji is
/// `height: 1em`; four ems on a side
/// admits the largest of them and
/// refuses an illustration, which is
/// the distinction the bound is for.
const IMAGE_TINT_EM: f32 = 4.0;

/// Device pixels per scene pixel a
/// tinted vector rasterises at, over
/// the device pixel ratio.
///
/// Twice, so a mask standing in for a
/// character is not the one thing on
/// the panel that looks soft.
const IMAGE_TINT_SCALE: f32 = 2.0;

/// The longest edge a tinted raster
/// may reach, in pixels.
const IMAGE_TINT_CLAMP: f32 = 256.0;

/// An image's fallback size, in ems:
/// a square of the text it sits in.
///
/// The last rung of the sizing ladder,
/// reached only when the node declares
/// no size *and* the store recorded
/// none - which is to say when there
/// are no bytes either, so what this
/// sizes is the placeholder box.
const IMAGE_FALLBACK_EM: f32 = 1.0;

/// The largest box a declared size may
/// resolve to, per axis, in pixels.
///
/// `dict::media` already refuses a
/// *recorded* dimension past this, and
/// for the same reason: a declared
/// 4 294 967 295 is a corrupt file or
/// a hostile one, not content, and no
/// dictionary asset in the census is
/// anywhere near it. Clamping here is
/// what keeps author input from
/// setting a line's height through the
/// riser [`measure_images`] sizes.
const IMAGE_MAX_PX: f32 = 65_536.0;

/// Yomitan's own list indent, per
/// level, as a multiple of the list's
/// own em.
///
/// `--list-padding1` in
/// `ext/css/display.css`, which
/// Yomitan puts on every list a
/// glossary contains. Nested levels
/// *reuse* it rather than stepping it
/// down, so a list two deep sits at
/// twice this and not at some halved
/// second value; the spec's defaults
/// table states it as `1.4em`.
const LIST_INDENT_EM: f32 = 1.4;

/// `list-style-type: disc`, CSS's
/// initial value: U+2022 BULLET.
const DISC_MARKER: &str = "\u{2022}";
/// `circle`: U+25E6 WHITE BULLET.
const CIRCLE_MARKER: &str = "\u{25E6}";
/// `square`: U+25AA BLACK SMALL
/// SQUARE.
const SQUARE_MARKER: &str = "\u{25AA}";

/// What sits between a marker and the
/// item text after it.
///
/// The marker box's own trailing gap,
/// and it is inside the box wherever
/// that box goes. A stacked item hangs
/// its marker in the gutter
/// ([`MarkerRun`]) with the box's right
/// edge on the list's content edge, so
/// the gap is what holds the glyph off
/// the text; a compact item has no
/// gutter and writes the same marker
/// as its paragraph's first span, so
/// the gap is a character on the line.
/// One label serves both, which is why
/// [`Marker::label`] appends it and
/// neither placement has to. It is the
/// gap ticket 16's row number already
/// writes after its `1.`, so a
/// numbered row and a numbered item
/// read alike.
const MARKER_GAP: &str = " ";

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
    /// Every list level above it,
    /// added up.
    ///
    /// Inherited rather than part of
    /// `style`, and that is the whole
    /// of the difference: a box
    /// property belongs to the node
    /// that declared it, and a
    /// paragraph carries only the box
    /// of the block that *opened* it,
    /// so an indent kept there would
    /// lose every level but the
    /// innermost - `ul > li > ul > li`
    /// would indent once.
    /// [`Block::inherited`] resets the
    /// box and keeps this, so the
    /// levels add up for the price of
    /// one `+=` per list.
    ///
    /// A separate term from
    /// `style.padding.left` rather
    /// than folded into it, because a
    /// dictionary declaring
    /// `paddingLeft` on an `li` gets
    /// both - Jitendex writes exactly
    /// that - and because this indent
    /// is the *list's* padding and so
    /// shifts an item's border box
    /// rather than insetting the text
    /// inside it.
    indent: f32,
}

impl Block {
    /// What a child inherits: the
    /// inherited half, and an empty
    /// box.
    ///
    /// Also what an *inline* node's own
    /// line carries. A box belongs to
    /// the node that declared it, and
    /// an inline node's box is drawn
    /// around its own run rather than
    /// around the paragraph
    /// ([`Paragraphs::node`]), so the
    /// line a `data.content` marker
    /// opens on a `span` takes the
    /// inherited half and no box.
    fn inherited(self) -> Block {
        Block { style: BoxStyle::default(), ..self }
    }
}

/// What a list draws beside each of
/// its items.
///
/// CSS's `list-style-type` has three
/// shapes this build answers and one
/// it declines: a bullet glyph, a
/// counter, a literal string, and the
/// locale counter algorithms the spec
/// puts out of scope. A keyword it
/// cannot read therefore falls back
/// to the initial value for the
/// list's own tag, which is a marker
/// of the wrong shape rather than no
/// marker at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker<'a> {
    /// One glyph, the same on every
    /// item.
    Glyph(&'static str),
    /// `decimal`: the item's own
    /// ordinal, from one.
    Decimal,
    /// A CSS string counter, drawn
    /// verbatim on every item - which
    /// is how a dictionary gets its
    /// own ①. No counter algorithm
    /// runs over it and no suffix is
    /// added to it, which is the whole
    /// of what verbatim means here.
    Literal(&'a str),
    /// `none`, and an empty string
    /// counter with it.
    None,
}

impl Marker<'_> {
    /// The `n`th item's marker text,
    /// gap and all.
    fn label(self, n: usize) -> Option<String> {
        match self {
            Marker::Glyph(glyph) => Some(format!("{glyph}{MARKER_GAP}")),
            // CSS's `decimal` counter
            // carries a `.` suffix, which
            // is also what ticket 16's row
            // number writes.
            Marker::Decimal => Some(format!("{n}.{MARKER_GAP}")),
            Marker::Literal(text) => Some(format!("{text}{MARKER_GAP}")),
            Marker::None => None,
        }
    }
}

/// One list's or one item's
/// `listStyleType`, as the walk spends
/// it.
///
/// `fallback` is the value already in
/// force, which is what makes this
/// both the list's resolver and the
/// item's: CSS *inherits*
/// `list-style-type` and draws the
/// marker at the element with
/// `display: list-item`, so an item's
/// own declaration wins over its
/// list's and a declaration this build
/// cannot read leaves the inherited
/// one standing - the same rule
/// [`apply_style`] follows.
///
/// The item half is not a corner case.
/// Jitendex declares `listStyleType`
/// on the `li` in every one of the
/// 38 381 entries that carries one -
/// 97 150 nodes, all of them `li`,
/// values `"①"` through `"㊿"` - and
/// its ①②③ sense numbering is nothing
/// but that. The list half is what
/// ticket 17 will feed from a
/// dictionary's own `styles.css`,
/// where Jitendex writes
/// `ul[data-sc-content="sense-groups"]
/// { list-style-type: "＊" }`.
///
/// Independent of any layout mode by
/// construction: it takes a document,
/// a node and a fallback, so ticket
/// 14's compact list and this one
/// resolve the same marker (see
/// [`Paragraphs::stack_items`]).
fn marker_of<'a>(doc: &'a GlossDoc, id: NodeId, fallback: Marker<'a>) -> Marker<'a> {
    let Some(text) = doc.style_of(id, StyleKey::ListStyleType).and_then(|v| doc.scalar_str(v))
    else {
        return fallback;
    };
    let text = text.trim();
    if let Some(literal) = quoted(text) {
        // An empty counter is CSS's own
        // way of asking for no marker.
        return if literal.is_empty() { Marker::None } else { Marker::Literal(literal) };
    }
    match text {
        "disc" => Marker::Glyph(DISC_MARKER),
        "circle" => Marker::Glyph(CIRCLE_MARKER),
        // Not in the census, and one
        // arm: falling back to `disc`
        // for it would draw a round
        // bullet where the author asked
        // for a square one, which is
        // worse than reading the third
        // of CSS's three bullet
        // keywords.
        "square" => Marker::Glyph(SQUARE_MARKER),
        "decimal" => Marker::Decimal,
        // Likewise the one keyword whose
        // fallback would *add* ink: an
        // author writing `none` removed
        // the marker, and `disc` would
        // hand it back.
        "none" => Marker::None,
        // Every remaining keyword is a
        // locale counter algorithm -
        // `lower-roman`, `katakana`,
        // `cjk-ideographic` - which the
        // spec puts out of scope, so the
        // value already in force stands.
        _ => fallback,
    }
}

/// One list's or one item's marker,
/// under the styling gate.
///
/// The third and last reader of a
/// node's resolved style record, and
/// therefore the third arm of the one
/// gate ticket 14's "honour dictionary
/// styling" setting closes - the other
/// two go through
/// [`Paragraphs::declarations`].
/// `listStyleType` is a declaration
/// like any other, so styling off must
/// not let it through: a dictionary
/// whose ①②③ numbering *is* its
/// `listStyleType` (Jitendex, 97 150
/// nodes) has to draw a browser's own
/// bullet instead, or "renders a
/// styled dictionary identically to an
/// unstyled one" would not hold.
///
/// A wrapper rather than a fourth
/// parameter on [`marker_of`], which
/// stays a pure question about a
/// document and a node and knows
/// nothing about a setting.
fn styled_marker<'a>(
    doc: &'a GlossDoc,
    id: NodeId,
    fallback: Marker<'a>,
    styling: bool,
) -> Marker<'a> {
    if styling {
        marker_of(doc, id, fallback)
    } else {
        fallback
    }
}

/// A list tag's initial
/// `list-style-type`.
///
/// The one thing `ol` and `ul` differ
/// in, and the bottom of the
/// inheritance chain [`marker_of`]
/// walks.
fn initial_marker(ordered: bool) -> Marker<'static> {
    if ordered {
        Marker::Decimal
    } else {
        Marker::Glyph(DISC_MARKER)
    }
}

/// A CSS `<string>`, unquoted.
///
/// Either quote character, because CSS
/// takes either and a dictionary
/// writing its style into JSON reaches
/// for whichever its own escaping
/// makes easy: the census holds both
/// `"'①'"` and `"\"◍\""`.
fn quoted(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    text.strip_prefix(quote)?.strip_suffix(quote)
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
    ///
    /// An [`IMAGE_RISER`] sets it for
    /// the same reason and reads the
    /// same way: zero advance, no ink,
    /// there only to be the tallest
    /// span on its line.
    filler: bool,
    /// The image whose room this span
    /// reserves, or [`NO_IMAGE`].
    ///
    /// Set on both of an image's spans:
    /// the [`IMAGE_SPACER`] that buys
    /// its width, which is the one
    /// [`place_images`] reads a box
    /// back off, and the
    /// [`IMAGE_RISER`]s that buy its
    /// height, which carry `filler`
    /// too.
    image: u32,
}

/// One image, before it is placed.
///
/// Sized here, while the tree is being
/// walked, because the sizing ladder
/// is arithmetic over what the node
/// declared and what the media row
/// recorded - no decode, no measurer,
/// no I/O (`dict::media`).
#[derive(Clone)]
struct FlowImage {
    /// The resolved box, in the
    /// panel's own pixels.
    w: f32,
    h: f32,
    /// The em the image sits in, which
    /// is what its `4em` tint bound is
    /// measured against.
    em: f32,
    /// Its `verticalAlign`, already
    /// resolved as far as the em alone
    /// can take it - the rest is
    /// [`shift_on`]'s, against the line
    /// the image landed on.
    style: Inline,
    /// [`IMAGE_SPACER`]s reserving its
    /// width, so [`measure_images`] can
    /// solve their size.
    spacers: usize,
    /// The `alt` fallback, empty when
    /// the node named none.
    alt: String,
    /// The image node itself, so a hit
    /// on it resolves to the node and
    /// not to the paragraph around it.
    path: Option<NodePath>,
    /// What a bin needs to paint it.
    scene: SceneImage,
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

/// One list marker, before it is
/// placed.
///
/// Held beside the paragraph rather
/// than in it, for the reason
/// [`MarkerRun`] gives: CSS's
/// `outside` marker box takes no room
/// on the line, so putting it in the
/// run would advance the pen past it
/// and put every continuation line of
/// a wrapped item under the bullet.
#[derive(Clone)]
struct FlowMarker {
    /// The label, its
    /// [`MARKER_GAP`] included.
    text: String,
    /// The item's own resolved style:
    /// CSS's `::marker` inherits from
    /// the element with `display:
    /// list-item`, not from the markup
    /// inside it.
    style: Inline,
    /// [`Block::indent`] as of the
    /// list that owed it, which is
    /// that list's content edge and so
    /// where the marker box's right
    /// edge goes.
    ///
    /// Carried rather than recomputed
    /// from the paragraph, because an
    /// item whose whole content is a
    /// nested list shares one line
    /// with its inner item and the two
    /// markers hang in two different
    /// gutters.
    indent: f32,
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
    /// One per list marker owed to
    /// this paragraph's first line,
    /// outermost list first.
    marker: Vec<FlowMarker>,
    /// One per image node this
    /// paragraph reached, indexed by
    /// [`FlowSpan::image`].
    images: Vec<FlowImage>,
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
                    && first.ruby == NO_RUBY
                    && first.image == NO_IMAGE =>
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
                    image: NO_IMAGE,
                },
            ),
        }
    }
}

/// What one term-bank row's images can
/// be sized from.
///
/// The dictionary that shipped them -
/// half of a [`MediaKey`], the other
/// half being the node's own `path` -
/// and the intrinsic sizes the build
/// recorded for the ones it could
/// store. Resolved before layout runs
/// and carried on the presentation
/// (`present::GlossEntry::media`),
/// because `scene` is a measure pass
/// with no database behind it and an
/// image must not put a query on the
/// paint path.
///
/// An empty table is a real state and
/// not a missing input: a tree parsed
/// from a string - a demo, a geometry
/// fixture, an Anki round trip - has
/// no store behind it, and its images
/// take the `alt`-text rung.
#[derive(Clone, Copy)]
struct Assets<'a> {
    dict_id: i64,
    sizes: &'a [(String, Intrinsic)],
}

impl Assets<'_> {
    /// One asset's recorded size.
    ///
    /// A linear scan, because a row
    /// names a handful of assets - 字通
    /// averages more than four - and a
    /// map would cost more to build
    /// than these comparisons cost to
    /// run.
    fn size(&self, path: &str) -> Option<Intrinsic> {
        self.sizes.iter().find(|(p, _)| p == path).map(|(_, size)| *size)
    }
}

/// What the render settings leave for
/// the gloss walk to spend.
///
/// The decision table's own record,
/// minus the one knob that was
/// already a parameter: compact mode
/// is [`Paragraphs::stack_items`],
/// which the list pass built and
/// which the settings only flip.
/// Bundled because three more
/// arguments on [`paragraphs`] is
/// where an argument gets passed in
/// the wrong slot, and `Copy` because
/// a table cell's paragraph pass is
/// handed the same one the panel's
/// stream got.
///
/// Resolved once, in
/// [`build_elements`]. Nothing here
/// reads a config type and nothing
/// below here reads a setting.
#[derive(Clone, Copy)]
struct Render {
    /// Which editorial roles reach the
    /// panel.
    ///
    /// The card renderer's own
    /// vocabulary and a separate
    /// *value* of it: hiding examples
    /// on screen must not strip them
    /// from a mined card, which takes
    /// [`RoleFilter::CARD`] and no
    /// setting at all.
    roles: RoleFilter,
    /// Do a dictionary's own
    /// declarations apply?
    ///
    /// Spent by
    /// [`Paragraphs::declarations`]
    /// and [`styled_marker`], which
    /// are the only three places a
    /// resolved style record is read.
    styling: bool,
    /// Do image assets reach the
    /// panel?
    images: bool,
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
    out: Vec<Piece>,
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
    /// List markers owed to the next
    /// run of text worth marking,
    /// outermost list first.
    ///
    /// Owed rather than pushed, because
    /// the paragraph an item's marker
    /// belongs to is the one the item's
    /// first text lands in - for
    /// `<li><div>x</div></li>` the
    /// `div`'s, not the `li`'s. A
    /// marker pushed where it is
    /// resolved would be flushed out as
    /// a paragraph of its own holding
    /// nothing but a bullet.
    ///
    /// More than one is owed when an
    /// item's whole content is a nested
    /// list: both levels' markers then
    /// share the one line they have
    /// between them, at their own two
    /// gutters, as a browser draws
    /// them.
    pending_marker: Vec<FlowMarker>,
    /// Does a list stack its items?
    ///
    /// The one thing ticket 14's
    /// compact mode changes about a
    /// list, and the only field in this
    /// pass that may know a layout mode
    /// exists: `true` gives every item
    /// its own paragraph and the
    /// indent, `false` joins the items
    /// into the paragraph already open
    /// with [`ITEM_SEPARATOR`] between
    /// them.
    ///
    /// Marker *resolution* sits above
    /// it and is shared by both:
    /// [`marker_of`] reads the list's
    /// own `listStyleType` and
    /// [`Marker::label`] writes the
    /// `n`th label, neither of them
    /// aware of this flag. Marker
    /// *placement* is the one thing
    /// that cannot be: `true` hangs the
    /// marker in the gutter the indent
    /// made ([`MarkerRun`]), and
    /// `false` has neither a gutter nor
    /// a line of its own to hang it
    /// beside, so a compact item writes
    /// its marker inline as its
    /// paragraph's first span. See
    /// [`Paragraphs::mark`].
    ///
    /// Wired from `popup.layout_mode`
    /// by [`build_elements`]:
    /// `LayoutMode::Roomy` stacks and
    /// `LayoutMode::Compact` joins.
    stack_items: bool,
    /// What else the render settings
    /// leave for this walk to spend.
    render: Render,
    /// The media store's answers for
    /// the row being walked.
    assets: Assets<'a>,
    /// Every image seen so far;
    /// [`Flow`] gets the ones that
    /// landed in it, renumbered.
    images: Vec<FlowImage>,
    /// What a table cell's border
    /// draws in, where the dictionary
    /// declares none.
    ///
    /// Yomitan gives a cell
    /// `border-color:
    /// var(--text-color-light2)`
    /// rather than the `currentColor`
    /// CSS would otherwise use - the
    /// middle rung of its three-step
    /// text ladder, so a grid reads as
    /// a grid without shouting. This
    /// panel's ladder is `body_text`,
    /// `collapsed_text`,
    /// `dimmed_text`, and its middle
    /// rung carries `#969aa0` dark and
    /// `#64666e` light against
    /// Yomitan's own `#999999` and
    /// `#666666`: the same rung, and
    /// very nearly the same colour.
    rule: Rgb,
    /// What a header cell's background
    /// draws in.
    ///
    /// Yomitan gives `thead` and `th`
    /// `background-color:
    /// var(--background-color-dark1)`,
    /// which is its palette's one step
    /// off the panel fill. This
    /// theme's one step off the panel
    /// fill is `separator`, the colour
    /// every other hairline and tint
    /// in the panel already uses -
    /// `#323238` against Yomitan's own
    /// `#333333` on a near-identical
    /// dark background.
    tint: Rgb,
    /// The popup's root font size: what
    /// a `rem` in a dictionary's own
    /// declarations is a fraction of
    /// ([`Ems`]).
    ///
    /// The theme's body size, taken
    /// once here rather than re-derived
    /// per node, because a node's own
    /// em changes as the walk descends
    /// and the root's does not.
    root_em: f32,
}

/// One row's gloss tree, laid out as
/// paragraphs and tables at `top_gap`
/// apart.
///
/// `stack_items` is
/// [`Paragraphs::stack_items`], the
/// compact-layout knob, and `render`
/// is the rest of the render
/// settings' decision table
/// ([`Render`]). Both are resolved by
/// [`build_elements`] and neither is
/// re-read below this point.
///
/// The theme rather than a resolved
/// [`Inline`], because a table cell's
/// Yomitan defaults need two colours
/// the body role does not carry - the
/// cell rule and the header tint (see
/// [`Paragraphs::cell_defaults`]).
fn paragraphs(
    doc: &GlossDoc,
    theme: &Theme,
    top_gap: f32,
    stack_items: bool,
    assets: Assets<'_>,
    render: Render,
) -> Vec<Piece> {
    let base = Inline::body(theme);
    let mut p = Paragraphs {
        doc,
        out: Vec::new(),
        cur: Flow::default(),
        links: Vec::new(),
        pending_sep: false,
        rubies: Vec::new(),
        open_ruby: NO_RUBY,
        barrier: false,
        pending_marker: Vec::new(),
        stack_items,
        render,
        assets,
        images: Vec::new(),
        rule: theme.collapsed_text,
        tint: theme.separator,
        root_em: theme.body_size,
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
    for piece in &mut p.out {
        piece.top_gap(top_gap);
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
            // A `type: image` item is an
            // image node with no tag, and
            // the same replaced element:
            // it takes room on the line
            // the item lands on rather
            // than opening one.
            ItemType::Image => self.image(id, ctx),
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

    /// Does this node's editorial role
    /// reach the panel?
    ///
    /// The render settings' role half,
    /// filtered on the same [`Role`]
    /// the card renderer filters on -
    /// with the popup's own answer and
    /// not the card's, which is what
    /// lets a user hide examples on
    /// screen and keep them on a mined
    /// card.
    ///
    /// One clause, over the role the
    /// parser classified. Ticket 15
    /// deleted the name-matched
    /// part-of-speech marker and the
    /// six-name drop list that used to
    /// stand beside this call: the
    /// role is now on the node for
    /// every dictionary, so
    /// `RoleFilter::allows` is the
    /// whole gate here, in
    /// `gloss::html`, and in
    /// `gloss::plain`.
    ///
    /// [`Role`]: crate::dict::gloss::Role
    fn shows(&self, id: NodeId) -> bool {
        self.render.roles.allows(self.doc.role(id))
    }

    /// One node.
    fn node(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let node = *doc.node(id);
        if !self.shows(id) {
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
            // A table is a grid rather
            // than a run of lines, and
            // every word in one belongs
            // to a cell, so the whole
            // subtree is taken over at
            // once - down to the `tr`,
            // which is a block here only
            // because a row breaks a
            // line for the plain-text
            // walk.
            Tag::Table => return self.table(id, ctx),
            _ => {}
        }
        if node.kind == Kind::Image {
            return self.image(id, ctx);
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
        //
        // The box follows the **tag**,
        // though, and not the line
        // break. `has_marker` is a
        // line-break rule inherited from
        // the plain-text walk: ticket 01
        // gave a `data.content` node a
        // block mark because that mark
        // is what separated senses
        // before a tree existed. What
        // decides where a box goes is
        // the schema's own block/inline
        // division, by tag, and `span`
        // is inline - so a `span`
        // carrying `data.content` opens
        // its line with no box of its
        // own and draws its box once,
        // below, as the pill it is.
        // Answering both questions with
        // one test gave such a node the
        // *same* resolved `BoxStyle`
        // twice, once as `block_box` and
        // once in `inline_boxes`, and a
        // bin looping over
        // `SceneElem::boxes()` painted
        // Jitendex's
        // `span[data-sc-class="tag"]`
        // twice.
        if node.tag.is_block() {
            self.open(ctx.path, block);
        } else if doc.has_marker(id) {
            self.open(ctx.path, block.inherited());
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
        // breaking one. Ink is the whole
        // of the gate, because the seam
        // takes styled spans and no
        // boxes (ADR-0013): an inline
        // box cannot push its neighbours
        // aside, so a margin or a
        // padding with nothing to draw
        // resolves to nothing - whatever
        // its node marks.
        if !node.tag.is_block() && block.style.paints() {
            return self.pill(id, next, block.style);
        }
        // A list marks and indents its
        // own children, so it takes
        // them over from `children`.
        if node.kind == Kind::List {
            return self.list(id, next, node.tag == Tag::Ol);
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

    /// One image node.
    ///
    /// An image is a *character*, not
    /// an illustration: 427 786 census
    /// nodes carry a gaiji marker and
    /// sit at `height: 1em` in the
    /// middle of a definition. So it
    /// takes room on the line it lands
    /// on and opens no line of its own:
    /// `Tag::Img` is inline, and this
    /// keeps it that way.
    ///
    /// The ladder, in the order the
    /// ticket names it. With bytes
    /// behind the path the image is an
    /// element of its own, sized from
    /// what the node declared or from
    /// what the build recorded. With no
    /// bytes there is nothing to
    /// composite, so the `alt` text
    /// goes into the flow instead -
    /// which is the *better* rung, not
    /// a worse one: real text wraps
    /// with the sentence around it. And
    /// with neither, a placeholder box
    /// of one em. Never nothing:
    /// nothing is a hole in a word.
    fn image(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let style = self.styled(id, ctx.inline);
        let path = doc
            .attr_of(id, "path")
            .and_then(|v| doc.scalar_str(v))
            .filter(|p| !p.is_empty());
        let recorded = path.and_then(|p| self.assets.size(p));
        let (w, h) = image_size(doc, id, style.size, recorded);
        let alt = image_alt(doc, id);
        // "Show images: off" takes the
        // ladder's own text rung rather
        // than cutting the node out.
        // `alt` is the text alternative
        // HTML defines for exactly this,
        // and an image node is a
        // *character* far more often
        // than an illustration
        // (427 786 census nodes carry a
        // gaiji marker), so a node cut
        // out whole would leave a hole
        // in a word. With no `alt` there
        // is nothing to stand in and the
        // node draws nothing: no
        // element, no reservation, and
        // no rect left behind, which is
        // the whole of what the setting
        // asks for.
        if !self.render.images {
            if !alt.is_empty() {
                self.text(&alt, style, ctx.link);
            }
            return;
        }
        if recorded.is_none() && !alt.is_empty() {
            return self.text(&alt, style, ctx.link);
        }
        let scene = SceneImage {
            // Only a stored asset gets a
            // key. Handing a bin a key
            // with no row behind it would
            // buy a decode attempt and a
            // cache entry for an answer
            // this walk already has.
            key: recorded
                .and(path)
                .map(|p| MediaKey::new(self.assets.dict_id, p)),
            format: recorded.map(|size| size.format),
            appearance: image_appearance(doc, id),
            // Yomitan's default is to
            // draw the backing; every
            // image node in the census's
            // samples turns it off.
            background: image_flag(doc, id, "background").unwrap_or(true),
            collapsed: image_flag(doc, id, "collapsed").unwrap_or(false),
            collapsible: image_flag(doc, id, "collapsible").unwrap_or(false),
        };
        self.reserve(
            FlowImage {
                w,
                h,
                em: style.size,
                style,
                spacers: image_spacers(w, h),
                alt,
                path: ctx.path,
                scene,
            },
            ctx.link,
        );
    }

    /// Buys one image its room on the
    /// line it lands on.
    ///
    /// Two spans, because a span's
    /// advance and its line height are
    /// the same number - its size - and
    /// an image needs them apart: the
    /// [`IMAGE_SPACER`] run is charged
    /// for the width and the
    /// [`IMAGE_RISER`] for the height.
    /// [`measure_images`] solves both
    /// sizes once the measurer has said
    /// what one of each costs.
    ///
    /// The spans are pushed rather than
    /// coalesced, and a barrier is left
    /// behind them: an image's
    /// reservation must be exactly its
    /// own, or [`place_images`] would
    /// read a box that included the
    /// word beside it.
    fn reserve(&mut self, img: FlowImage, link: u32) {
        // An image is worth an item
        // separator and worth a list
        // marker, for the same reason
        // text is: it is content, and
        // `<li><img></li>` draws its
        // bullet.
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, img.style, link);
        }
        self.mark();
        let slot = self.images.len() as u32;
        let (style, spacers) = (img.style, img.spacers);
        self.images.push(img);
        self.raw(&IMAGE_SPACER.repeat(spacers), style, link, slot, false);
        self.raw(IMAGE_RISER, style, link, slot, true);
        self.barrier = true;
    }

    /// One of an image's own spans,
    /// joined to nothing.
    ///
    /// `link` rides along so an image
    /// inside a cross-reference is part
    /// of that link's hit target - a
    /// gaiji in a "see also" is as
    /// clickable as the word beside it.
    /// `ruby` does not: an image is
    /// never a ruby base, and claiming
    /// a slot would centre a reading
    /// over a picture.
    fn raw(&mut self, text: &str, style: Inline, link: u32, image: u32, filler: bool) {
        let at = self.cur.text.len() as u32;
        self.cur.text.push_str(text);
        self.cur.spans.push(FlowSpan {
            at,
            len: text.len() as u32,
            style,
            link,
            ruby: NO_RUBY,
            filler,
            image,
        });
    }

    /// One `ul` or `ol`: its items,
    /// each marked and one level in.
    ///
    /// The list is where a marker
    /// starts: `listStyleType` is
    /// declared on it and an ordinal
    /// counts its own items, so both
    /// are resolved once here. Each
    /// item then resolves its own
    /// `listStyleType` against the
    /// list's, because CSS inherits the
    /// property and the item is what
    /// draws the marker.
    ///
    /// The indent goes on the list too,
    /// exactly where Yomitan's
    /// stylesheet puts it, which is
    /// what lets it accumulate without
    /// this walk counting levels: an
    /// item inherits its list's indent
    /// ([`Block::indent`]) and a list
    /// inside an item adds another
    /// [`LIST_INDENT_EM`] to it. It is
    /// also the gutter each marker
    /// hangs in, which is why the
    /// resolved value is handed to
    /// [`Paragraphs::owe`] with the
    /// label.
    fn list(&mut self, id: NodeId, ctx: Ctx, ordered: bool) {
        let doc = self.doc;
        let ctx = Ctx {
            block: Block {
                indent: ctx.block.indent + LIST_INDENT_EM * ctx.inline.size,
                ..ctx.block
            },
            ..ctx
        };
        // Nothing to mark: `display:
        // list-item` is what earns a
        // marker and only an `li` has
        // it, so a list holding none is
        // walked as any other block -
        // which keeps `children`'s
        // bare-string rule over it.
        if !doc.children(id).any(|child| doc.node(child).kind == Kind::ListItem) {
            return self.children(id, ctx);
        }
        // The list's own declaration
        // over its tag's initial value.
        // Each item then resolves again
        // against this, because the
        // property is inherited and the
        // item is what draws the marker.
        let styling = self.render.styling;
        let inherited = styled_marker(doc, id, initial_marker(ordered), styling);
        let mut n = 0usize;
        for (i, child) in doc.children(id).enumerate() {
            let at = ctx.at(i);
            if doc.node(child).kind != Kind::ListItem {
                self.node(child, at);
                continue;
            }
            n += 1;
            if let Some(label) = styled_marker(doc, child, inherited, styling).label(n) {
                // The item's own resolved
                // style: CSS's `::marker`
                // inherits from the item, not
                // from the `b` the item's text
                // may sit inside. The indent
                // is this list's, because the
                // gutter the marker hangs in
                // is this list's padding and
                // not the item's.
                let style = self.styled(child, ctx.inline);
                self.owe(label, style, ctx.block.indent);
            }
            if self.stack_items {
                self.node(child, at);
            } else {
                // Ticket 14's compact list:
                // the item's content joins the
                // paragraph already open,
                // separated as the panel has
                // always separated glossary
                // items. The `li` opens no
                // line here, so it draws no
                // block box either - it is
                // inline content, and CSS
                // draws no block box around
                // inline content.
                self.pending_sep = true;
                let inline = self.styled(child, ctx.inline);
                let link = self.link_of(child, ctx.link);
                self.children(child, Ctx { inline, link, ..at });
            }
            // A marker never outlives the
            // item that owes it: an item
            // holding no text at all draws
            // none, rather than marking the
            // next item twice.
            self.pending_marker.clear();
        }
    }

    /// One `table`: its rows, as a
    /// grid.
    ///
    /// The table is also the block
    /// container Yomitan wraps one in
    /// (`.gloss-sc-table-container {
    /// display: block }`): it closes
    /// the paragraph before it and
    /// opens none of its own, because
    /// every word in a table belongs
    /// to a cell.
    ///
    /// A `table` inside a cell is a
    /// table inside a cell: a cell's
    /// content is a list of
    /// [`Piece`]s, and [`Pass::block`]
    /// lays a nested grid out through
    /// the same two methods the outer
    /// one used.
    fn table(&mut self, id: NodeId, ctx: Ctx) {
        // A marker owed to an item
        // whose only content is a table
        // has no line inside the table
        // to hang beside, so it takes
        // the line above - which is
        // where a browser puts a
        // `::marker` beside a block too.
        // Inline, deliberately: a
        // hanging marker needs a line
        // box to share a baseline with,
        // and the paragraph this opens
        // would have none of its own.
        // A gutter with no line beside
        // it would drop the bullet.
        self.mark_inline();
        self.flush();
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        let inner = Ctx {
            inline,
            // A cell pays the box and the
            // list indent once, through
            // the table that holds it -
            // the same reason
            // [`Block::inherited`] hands
            // a child an empty box.
            block: Block { indent: 0.0, ..block.inherited() },
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        let mut rows = Vec::new();
        self.rows_of(id, inner, false, &mut rows);
        let (cells, rows, cols) = place(rows);
        // A table of nothing draws
        // nothing, rather than an empty
        // rule.
        if cells.is_empty() {
            return;
        }
        self.out.push(Piece::Table(Grid {
            top_gap: 0.0,
            cells,
            rows,
            cols,
            block,
            base: inline,
            path: ctx.path,
            dict_id: 0,
            entry_id: 0,
        }));
    }

    /// Every row under a `table` or a
    /// row group, in document order.
    ///
    /// `thead`, `tbody` and `tfoot` are
    /// scaffolding rather than
    /// structure, and the parser
    /// already classes all three
    /// `Kind::Table`, so they
    /// contribute their rows and
    /// nothing else. A `thead` also
    /// carries Yomitan's header styling
    /// to the cells inside it, which is
    /// how a `td` in a header row comes
    /// out bold and tinted. `tfoot` is
    /// out of this ticket's scope and
    /// contributes its rows plainly,
    /// which loses no text and invents
    /// no styling for a shape the
    /// census counts zero of.
    ///
    /// Content a dictionary wrote
    /// outside any cell becomes an
    /// implied cell in an implied row,
    /// so a malformed table loses none
    /// of it. An implied cell holding
    /// only the whitespace between two
    /// written cells is dropped, and
    /// with it the column it would
    /// otherwise have invented.
    fn rows_of(&mut self, id: NodeId, ctx: Ctx, head: bool, out: &mut Vec<Vec<Cell>>) {
        let doc = self.doc;
        let mut stray: Vec<Cell> = Vec::new();
        for (i, child) in doc.children(id).enumerate() {
            let at = ctx.at(i);
            let tag = doc.node(child).tag;
            let row_level = matches!(tag, Tag::Tr | Tag::Thead | Tag::Tbody | Tag::Tfoot);
            if row_level && !stray.is_empty() {
                out.push(std::mem::take(&mut stray));
            }
            match tag {
                Tag::Tr => {
                    let row = self.row(child, at, head);
                    if !row.is_empty() {
                        out.push(row);
                    }
                }
                Tag::Thead | Tag::Tbody | Tag::Tfoot => {
                    let inline = self.styled(child, ctx.inline);
                    let block = self.boxed(child, ctx.block, inline);
                    let inner = Ctx { inline, block: block.inherited(), ..at };
                    self.rows_of(child, inner, head || tag == Tag::Thead, out);
                }
                Tag::Td | Tag::Th => stray.push(self.cell(child, at, head, false)),
                _ => stray.extend(self.implied(child, at, head)),
            }
        }
        if !stray.is_empty() {
            out.push(stray);
        }
    }

    /// One `tr`: its cells, in order.
    ///
    /// A row's own box is not drawn.
    /// Under `border-collapse` a row's
    /// border collapses into its
    /// cells', which is where this grid
    /// resolves it, and Yomitan
    /// declares no `tr` rule at all -
    /// so the one real dictionary that
    /// writes a `tr` border writes a
    /// width with no style, which draws
    /// nothing in a browser either.
    fn row(&mut self, id: NodeId, ctx: Ctx, head: bool) -> Vec<Cell> {
        let doc = self.doc;
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        let inner = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        let mut out = Vec::new();
        for (i, child) in doc.children(id).enumerate() {
            let at = inner.at(i);
            match doc.node(child).tag {
                Tag::Td | Tag::Th => out.push(self.cell(child, at, head, false)),
                _ => out.extend(self.implied(child, at, head)),
            }
        }
        out
    }

    /// A cell for content written
    /// outside one, if it renders
    /// anything.
    ///
    /// An explicit empty `<td>` is a
    /// blank in a paradigm and keeps
    /// its border; an implied cell
    /// holding nothing is the
    /// whitespace a dictionary left
    /// between two written cells, and
    /// a border round it would invent a
    /// column. The node's own
    /// declarations are not read into
    /// the cell's box either: they
    /// belong to the node, which
    /// [`Paragraphs::node`] resolves
    /// again inside the cell, and
    /// taking them twice would pay a
    /// margin twice.
    fn implied(&mut self, id: NodeId, ctx: Ctx, head: bool) -> Option<Cell> {
        let cell = self.cell(id, ctx, head, true);
        (!cell.body.is_empty()).then_some(cell)
    }

    /// One cell: a `td`, a `th`, or a
    /// slot implied by content written
    /// outside one.
    ///
    /// The cell opens a paragraph of
    /// its own rather than joining the
    /// one before it, which `td` and
    /// `th` do not do anywhere else in
    /// this walk: they are in neither
    /// [`Tag::is_block`] nor
    /// [`Tag::is_inline`] precisely
    /// because a cell is a grid problem
    /// and not a line-break one. That
    /// paragraph carries the cell's own
    /// address, so a hit inside a
    /// conjugation table resolves to
    /// that cell's subtree.
    fn cell(&mut self, id: NodeId, ctx: Ctx, head: bool, implied: bool) -> Cell {
        let doc = self.doc;
        let inline = self.styled(id, ctx.inline);
        // Yomitan tints a `th` and
        // everything inside a `thead`.
        // The bold half of the same rule
        // is [`tag_style`]'s, so a `td`
        // in a header row inherits its
        // weight from the `thead` and no
        // second rule belongs here.
        let tint = head || doc.node(id).tag == Tag::Th;
        let mut block = self.cell_defaults(ctx.block, inline, tint);
        if !implied {
            block = self.declared(id, block, inline.size);
        }
        let inner = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        // The cell's own paragraphs,
        // collected off to one side: the
        // grid measures each cell
        // separately, so they cannot go
        // into the stream the panel is
        // stacking.
        let outer = std::mem::take(&mut self.out);
        self.open(ctx.path, block.inherited());
        if implied {
            self.node(id, ctx);
        } else {
            self.children(id, inner);
        }
        self.flush();
        let mut body = std::mem::replace(&mut self.out, outer);
        for (i, piece) in body.iter_mut().enumerate() {
            // A cell's first paragraph
            // starts at the cell's own
            // top; the ones under it are
            // spaced as the panel spaces
            // every other stacked
            // paragraph.
            piece.top_gap(if i == 0 { 0.0 } else { LINE_GAP });
        }
        Cell {
            body,
            style: block.style,
            base: inline,
            at: (0, 0),
            span: (span_of(doc, id, "rowSpan"), span_of(doc, id, "colSpan")),
            path: ctx.path,
        }
    }

    /// Yomitan's own cell box, before
    /// the dictionary speaks.
    ///
    /// The spec's defaults table,
    /// verbatim: a solid border of
    /// [`cell_border`] on every edge in
    /// the panel's rule colour,
    /// [`CELL_PADDING`] of padding, and
    /// a tinted background on a header
    /// cell. `vertical-align: top` is
    /// the other half of that table and
    /// needs no field - it is what
    /// [`Pass::table`] does by placing
    /// a short cell at its row's top.
    fn cell_defaults(&self, parent: Block, inline: Inline, tint: bool) -> Block {
        let em = inline.size;
        Block {
            style: BoxStyle {
                border: Edges::all(cell_border(em)),
                border_style: Edges::all(BorderStyle::Solid),
                border_color: self.rule,
                padding: Edges::all(em * CELL_PADDING),
                background: tint.then_some(self.tint),
                ..BoxStyle::default()
            },
            ..parent
        }
    }

    /// Owes `label` to the next run of
    /// text, in `style`, hanging in the
    /// gutter `indent` opened.
    ///
    /// Appended when a marker is
    /// already owed, which is an item
    /// whose only content is a nested
    /// list: both levels' markers then
    /// share the one line they have
    /// between them. Each keeps its own
    /// style and its own gutter, so the
    /// two hang one level apart, as a
    /// browser draws them.
    fn owe(&mut self, label: String, style: Inline, indent: f32) {
        self.pending_marker.push(FlowMarker { text: label, style, indent });
    }

    /// Spends the markers owed to the
    /// run about to be pushed, the way
    /// the layout mode asks for.
    ///
    /// Stacked items have a gutter, so
    /// the marker hangs in it and the
    /// item's text - every line of it -
    /// keeps the item's own indent.
    /// Compact items have no gutter and
    /// no line of their own, so the
    /// marker is written inline. This
    /// is the one place
    /// [`Paragraphs::stack_items`]
    /// reaches a marker: what the
    /// marker *says* is resolved above
    /// it and shared.
    fn mark(&mut self) {
        if self.stack_items {
            self.hang();
        } else {
            self.mark_inline();
        }
    }

    /// Hands the owed markers to the
    /// paragraph, out of its flow.
    ///
    /// Nothing about the run changes,
    /// which is the whole point: the
    /// markers are placed against the
    /// first line box the wrap produces
    /// ([`place_markers`]) and no line
    /// box is touched, so a bin
    /// re-measuring the element's own
    /// spans gets exactly these lines
    /// back.
    fn hang(&mut self) {
        self.cur.marker.append(&mut self.pending_marker);
    }

    /// Writes the owed markers as the
    /// paragraph's leading spans.
    ///
    /// `list-style-position: inside`,
    /// for the two cases with no gutter
    /// to hang in: a compact list, and
    /// an item whose only content is a
    /// table.
    ///
    /// Each its own span at both ends,
    /// and out of both the open
    /// reading's slot and the link
    /// around it: a marker joined to a
    /// ruby base would hand the reading
    /// `\u{2022} 猫` to centre over, a
    /// marker joined to the item's `b`
    /// would come out bold, and
    /// `::marker` is no part of an
    /// `<a>`.
    fn mark_inline(&mut self) {
        if self.pending_marker.is_empty() {
            return;
        }
        let owed = std::mem::take(&mut self.pending_marker);
        let slot = std::mem::replace(&mut self.open_ruby, NO_RUBY);
        for mark in owed {
            self.barrier = true;
            self.push(&mark.text, mark.style, NO_LINK);
        }
        self.barrier = true;
        self.open_ruby = slot;
    }

    /// Appends text, paying any owed
    /// item separator and list marker
    /// first.
    fn text(&mut self, text: &str, style: Inline, link: u32) {
        if text.is_empty() {
            return;
        }
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, style, link);
        }
        // A marker waits for text worth
        // marking: `<li><br>a</li>` puts
        // it on the `a` rather than on
        // the break, and an item holding
        // only the whitespace a
        // dictionary left between two
        // nodes draws none at all.
        if !text.trim().is_empty() {
            self.mark();
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
                    && last.image == NO_IMAGE
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
                image: NO_IMAGE,
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
            image: NO_IMAGE,
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
            // A marker needs a line to
            // hang beside, and a dropped
            // paragraph has none. Only
            // reachable through a
            // paragraph an item opened
            // and never filled, which
            // draws no marker either way.
            self.cur.marker.clear();
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
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for span in &mut flow.spans {
            if span.image == NO_IMAGE {
                continue;
            }
            span.image = match seen.iter().find(|(old, _)| *old == span.image) {
                Some((_, new)) => *new,
                None => {
                    let new = flow.images.len() as u32;
                    flow.images.push(self.images[span.image as usize].clone());
                    seen.push((span.image, new));
                    new
                }
            };
        }
        self.out.push(Piece::Flow(flow));
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

    /// A node's resolved style record,
    /// under the styling gate.
    ///
    /// **The one gate the "honour
    /// dictionary styling" setting
    /// closes**, and a method rather
    /// than a check at each reader for
    /// exactly that reason: off has to
    /// mean *neither* inline `style`
    /// nor a dictionary's own
    /// `styles.css`, and ticket 17
    /// folds the sheet into this same
    /// record rather than a second
    /// one. So a source added there is
    /// gated by construction, and
    /// nothing downstream learns that
    /// a setting exists.
    ///
    /// An empty record is every
    /// property unset, which leaves
    /// HTML's own stylesheet
    /// ([`tag_style`]) and the theme's
    /// own font and colours standing.
    /// A `b` stays bold with styling
    /// off: markup is what an entry
    /// *says*, where a `style` object
    /// is how a dictionary chose to
    /// draw it.
    ///
    /// [`styled_marker`] is the third
    /// reader of the same record and
    /// the same flag - the marker is
    /// resolved by a free function
    /// walking a document, so it
    /// cannot come through here.
    fn declarations(&self, id: NodeId) -> &[(StyleKey, Scalar)] {
        if self.render.styling {
            self.doc.style(id)
        } else {
            &[]
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
    /// ticket 17 also feeds from the
    /// dictionary's own `styles.css`.
    /// Both arrive through
    /// [`Paragraphs::declarations`],
    /// so the styling setting turns
    /// off both at once.
    fn styled(&self, id: NodeId, parent: Inline) -> Inline {
        let doc = self.doc;
        let mut style = tag_style(doc.node(id).tag, parent);
        let record = self.declarations(id);
        for (i, (key, value)) in record.iter().enumerate() {
            // First occurrence wins,
            // which is the answer
            // `GlossDoc::style_of`
            // gives every other reader
            // of the record.
            if record[..i].iter().any(|(seen, _)| seen == key) {
                continue;
            }
            apply_style(doc, *key, *value, self.ems(parent.size), &mut style);
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
        let start = Block {
            style: BoxStyle { border_color: inline.color, ..BoxStyle::default() },
            ..parent
        };
        self.declared(id, start, inline.size)
    }

    /// The two bases a length in this
    /// node's declarations resolves
    /// against: the node's own em, and
    /// the panel's.
    fn ems(&self, own: f32) -> Ems {
        Ems { own, root: self.root_em }
    }

    /// One node's own declarations,
    /// folded over the box it starts
    /// from.
    ///
    /// Split out because a table cell
    /// starts from Yomitan's own grid
    /// defaults rather than from an
    /// empty box
    /// ([`Paragraphs::cell_defaults`]),
    /// and the dictionary's last word
    /// must be applied the same way to
    /// both. `own` is the node's *own*
    /// resolved font size, because a
    /// box length is a fraction of it -
    /// of the panel's, for a `rem`
    /// ([`Ems`]).
    fn declared(&self, id: NodeId, mut block: Block, own: f32) -> Block {
        let doc = self.doc;
        let em = self.ems(own);
        let record = self.declarations(id);
        for (i, (key, value)) in record.iter().enumerate() {
            // First occurrence wins, as
            // it does for a span.
            if record[..i].iter().any(|(seen, _)| seen == key) {
                continue;
            }
            apply_box(doc, *key, *value, em, &mut block);
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
        // Yomitan writes the weight on
        // `thead` as well as on `th`, so
        // a plain `td` in a header row
        // inherits it - which is the
        // whole of why `thead` is here,
        // since a `thead` has no text of
        // its own.
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
        Tag::B | Tag::Strong | Tag::Th | Tag::Thead | Tag::Summary => {
            style.weight = BOLD_WEIGHT;
        }
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
/// properties are ticket 08's,
/// `listStyleType` is a list's own and
/// [`marker_of`] reads it straight off
/// the same record, and a value this
/// build cannot read leaves the
/// inherited one standing rather than
/// guessing.
fn apply_style(doc: &GlossDoc, key: StyleKey, value: Scalar, em: Ems, out: &mut Inline) {
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
            if let Some((shift, align)) = align_of(doc, value, em.own) {
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
/// it, `listStyleType` is resolved by
/// [`marker_of`] and is no part of a
/// box, and a value
/// this build cannot read leaves the
/// box as it was rather than taking
/// half a declaration.
///
/// CSS forbids a negative padding,
/// border width and radius and allows
/// a negative margin, so each arm
/// clamps exactly what CSS clamps.
fn apply_box(doc: &GlossDoc, key: StyleKey, value: Scalar, em: Ems, out: &mut Block) {
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
fn box_len(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<f32> {
    match value {
        Scalar::Num(n) => finite(em.own * n as f32),
        Scalar::Text(span) => css_len(doc.span(span), em),
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// One to four box lengths, as
/// [`edges_of`] splits them.
fn box_edges(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<Edges<f32>> {
    match value {
        Scalar::Num(n) => Some(Edges::all(finite(em.own * n as f32)?)),
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
fn length_px(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<f32> {
    let px = match value {
        Scalar::Num(n) => em.own * n as f32,
        Scalar::Text(span) => css_len(doc.span(span), em)?,
        Scalar::Bool(_) | Scalar::Null => return None,
    };
    (px.is_finite() && px > 0.0).then_some(px)
}

/// One CSS length with its own unit,
/// sign and zero intact.
///
/// `em` and `%` are relative to the em
/// the length sits in, `rem` to the
/// panel's own body size ([`Ems`]),
/// and `px` to Yomitan's own base (see
/// [`YOMITAN_BASE_PX`]) so that an
/// absolute pixel scales with the
/// panel instead of shrinking on a
/// dense screen. A bare number is an
/// em multiplier, as the schema's own
/// numeric values are.
///
/// Those four are every length unit
/// real dictionaries write. Counted
/// over the corpus of
/// `docs/research/dict-shapes.md`:
/// inside inline `style` objects, `em`
/// 5 690 564, `px` 151 516, `rem`
/// 3 096 and no `%` at all; inside
/// stylesheet declaration values, `em`
/// 1 377, `px` 84 and `rem` 33. What
/// is left unread is `s` (4), `deg`
/// (3) and `fr` (3), none of them a
/// length and all three on properties
/// this build does not map -
/// `transition`, `transform` and
/// `grid-template-*`.
fn css_len(text: &str, em: Ems) -> Option<f32> {
    let text = text.trim();
    let at = text
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+'))
        .unwrap_or(text.len());
    let n: f32 = text[..at].parse().ok()?;
    finite(match text[at..].trim() {
        "em" | "" => em.own * n,
        "rem" => em.root * n,
        "%" => em.own * n / 100.0,
        "px" => em.own * n / YOMITAN_BASE_PX,
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
///
/// **The one place the render settings
/// are read.** [`RenderSettings`]'
/// four facts resolve here into the
/// two things the gloss walk takes -
/// a list's layout and a [`Render`]
/// record - and nothing below this
/// point consults a setting again.
/// That is the spec's "decision
/// table, not a style engine":
/// Yomitan drives the same handful of
/// root attributes into fixed CSS
/// rules rather than offering a
/// cascade.
fn build_elements(
    p: &Presentation,
    theme: &Theme,
    show_back: bool,
    side_panel: bool,
    render: RenderSettings,
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
                let assets = Assets { dict_id: block.dict_id, sizes: &entry.media };
                let mut pieces = paragraphs(
                    &entry.doc,
                    theme,
                    LINE_GAP,
                    render.stack_items,
                    assets,
                    Render {
                        roles: render.roles,
                        styling: render.styling,
                        images: render.images,
                    },
                );
                if pieces.is_empty() {
                    continue;
                }
                // The row behind every
                // paragraph of it, which the
                // tree itself cannot know.
                // With the node path each
                // paragraph already carries,
                // this is what turns "select
                // the text" into "select sense
                // 3 of 大辞林" (stories 45
                // and 46).
                for piece in &mut pieces {
                    piece.stamp(block.dict_id, entry.entry_id);
                }
                if numbered {
                    // Yomitan's number is the
                    // `<li>` marker, outside the
                    // content, so a row that opens
                    // with a table takes its
                    // number on the line above
                    // rather than losing it. Every
                    // other row prefixes the
                    // paragraph it already has,
                    // which is what the geometry
                    // goldens hold.
                    if !matches!(pieces.first(), Some(Piece::Flow(_))) {
                        pieces.insert(
                            0,
                            Piece::Flow(Flow {
                                top_gap: LINE_GAP,
                                dict_id: block.dict_id,
                                entry_id: entry.entry_id,
                                ..Flow::default()
                            }),
                        );
                    }
                    if let Some(Piece::Flow(flow)) = pieces.first_mut() {
                        flow.number(i + 1, Inline::body(theme));
                    }
                }
                out.extend(pieces.into_iter().map(|piece| match piece {
                    Piece::Flow(flow) => Elem::Gloss(flow),
                    Piece::Table(grid) => Elem::Table(grid),
                }));
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

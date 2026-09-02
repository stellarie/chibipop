//! The measured scene, as plain data.
//!
//! **One reason to change:** what a bin paints and what a geometry snapshot
//! pins. Everything here is comparable data with no measurer, no document
//! and no setting behind it, because both readers need exactly that - a bin
//! walks it to paint, and the Windows golden capture walks it to serialise.
//! A field added here is a field thirteen goldens change for.
//!
//! One pixel space and no scale factor: core never sees one. Windows hands
//! in DIPs because its Direct2D target carries the DPI, Linux hands in
//! device pixels; either way the conversion is the bin's.

use crate::controller::HitAction;
use crate::dict::gloss::NodePath;
use crate::dict::media::{MediaFormat, MediaKey};
use super::measure::StyledSpan;
use super::style::{BoxStyle, Inline};

/// A colour, as `Theme` carries it.
pub type Rgb = (u8, u8, u8);

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
    /// One accent, as marked kana.
    ///
    /// A reading whose high mora run carries an overline and whose fall
    /// carries a tick, both of them [`inline_boxes`] with one border edge
    /// each - which is what the notation *is*, and why it needs no SVG. Its
    /// `text` is the reading and then the dictionaries that gave the accent,
    /// so it has two spans where the panel's other chrome has one.
    ///
    /// [`inline_boxes`]: SceneElem::inline_boxes
    Pitch,
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
    /// One block's own box.
    ///
    /// Its `rect` and its `block_box`
    /// are the block's border box, and
    /// it is textless: the paragraphs
    /// the block emits are the
    /// elements that follow it, each
    /// with its own address. So a
    /// bordered `div` holding three
    /// paragraphs is one border around
    /// three runs, which is what a
    /// browser draws. It leads them in
    /// draw order, so a declared
    /// background sits under every one
    /// of them.
    Block,
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
    ///
    /// [`collapsed`]: super::table::collapsed
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
            ElemKind::Pitch => "Pitch",
            ElemKind::BackButton => "BackButton",
            ElemKind::Image => "Image",
            ElemKind::Block => "Block",
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
///
/// [`MeasureRun`]: super::measure::MeasureRun
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
    /// is the arithmetic it makes
    /// possible. Zero for every span
    /// that sits on its line's own
    /// baseline, which is nearly all
    /// of them.
    pub shift: f32,
}

/// One reading's box, over its base.
///
/// CSS Ruby's own *annotation box*,
/// which is why the name says box: it
/// is placed geometry, not a span of
/// any run.
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
///
/// [`LineBox`]: super::measure::LineBox
#[derive(Debug, Clone, PartialEq)]
pub struct RubyBox {
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

impl RubyBox {
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

/// One list marker's box, hanging in
/// its list's gutter.
///
/// CSS's own *marker box*, which is
/// why the name says box: what goes
/// in it is `list-style-type`'s
/// resolved value, and that stays
/// private to layout.
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
/// origin and every line of it
/// therefore starts at the same x.
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
///
/// [`LineBox`]: super::measure::LineBox
/// [`LIST_INDENT_EM`]: super::marker::LIST_INDENT_EM
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerBox {
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

impl MarkerBox {
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
    ///
    /// Its leading edge is the widest
    /// line's, [`align`] already
    /// applied, so an aligned element
    /// has `rect.x` right of `pen.0` by
    /// the line's own slack. Nothing
    /// run-relative is measured from
    /// it: a [`RubyBox`] carries that
    /// same slack in its own `x`, and a
    /// [`MarkerBox`] hangs in the
    /// gutter off the content edge with
    /// no slack at all, so both draw
    /// from [`pen`] and adding them to
    /// `rect.x` would count it twice.
    ///
    /// [`align`]: Self::align
    /// [`pen`]: Self::pen
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
    /// [`RubyBox`].
    pub ruby: Vec<RubyBox>,
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
    /// [`MarkerBox`].
    pub marker: Vec<MarkerBox>,
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
    /// breaking one.
    ///
    /// Pure decoration - no geometry the
    /// walk stacked depends on these -
    /// and one of them is not the whole
    /// of its box. The horizontal room
    /// its margin, border and padding
    /// reserve is *advance in the run*,
    /// bought before the wrap and
    /// therefore already in `spans`,
    /// `rect` and `advance`; a rect here
    /// is what draws inside that room.
    /// So a box with no ink has no entry
    /// and still spaces its content out.
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

    /// This element's box at the scroll it is being drawn at.
    ///
    /// The scene's rects are in unscrolled panel space, like every other
    /// rect it reports, and an image element's `pen` is its own top-left -
    /// so the whole box moves by the same offset the pen did.
    ///
    /// Here rather than in each bin because both bins had the same three
    /// lines, doc comment included, and a rect that moved in one of them
    /// and not the other would be an image drawn at last frame's scroll.
    pub fn rect_at(&self, pen_y: f32) -> SceneRect {
        SceneRect { y: pen_y, ..self.rect }
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
/// realisation is per-bin. Windows
/// gives the affordance its own
/// window, sized by that window's own
/// font, and takes only the strip; a
/// bin painting in the panel uses
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

    /// This scene's scrollbar thumb, in a `view_h`-tall panel padded by
    /// `pad`, as `(top, height)`.
    ///
    /// The scene knows its own content height and both bins derive the
    /// same two numbers from it - the track is the view less the padding
    /// at each end, the total is the body plus that padding - so the
    /// derivation belongs where `used_h` does. Three call sites had it
    /// written out, and a fourth would have got it subtly wrong.
    ///
    /// Integral because a thumb is whole pixels: core does this
    /// arithmetic in `i32` and each bin rounds its own view height and
    /// scroll on the way in.
    pub fn scrollbar_thumb(&self, pad: i32, view_h: i32, scroll: i32) -> Option<(i32, i32)> {
        let total = self.used_h.ceil() as i32 + 2 * pad;
        super::scrollbar_thumb(view_h - 2 * pad, total, view_h, scroll)
    }
}

/// Is a box worth painting?
pub(super) fn on_panel(y: f32, h: f32, slack: f32, view_h: f32) -> bool {
    y + h + slack > 0.0 && y - slack < view_h
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
pub(super) fn box_elem(
    kind: ElemKind,
    base: Inline,
    align: Align,
    origin: GlossOrigin,
) -> SceneElem {
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

/// The box a monochrome vector may
/// have and still be rasterised for
/// tinting, per axis, in ems.
///
/// Hoshi Reader's bound. A gaiji is
/// `height: 1em`; four ems on a side
/// admits the largest of them and
/// refuses an illustration, which is
/// the distinction the bound is for.
pub(super) const IMAGE_TINT_EM: f32 = 4.0;

/// Device pixels per scene pixel a
/// tinted vector rasterises at, over
/// the device pixel ratio.
///
/// Twice, so a mask standing in for a
/// character is not the one thing on
/// the panel that looks soft.
pub(super) const IMAGE_TINT_SCALE: f32 = 2.0;

/// The longest edge a tinted raster
/// may reach, in pixels.
pub(super) const IMAGE_TINT_CLAMP: f32 = 256.0;

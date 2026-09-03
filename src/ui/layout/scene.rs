//! The measured scene stores plain panel data.
//!
//! A bin paints the scene, and a geometry snapshot records it. Both readers
//! need the same values, so this module stores no measurer, document, or setting.
//!
//! A bin walks the scene to paint it. The Windows golden capture walks it to
//! serialize it. If you add a field, thirteen goldens change.
//!
//! Core uses one pixel space because it has no scale factor. Windows provides
//! DIPs because its Direct2D target carries DPI. Linux provides device pixels.
//! Each bin converts coordinates for its output.

use crate::controller::HitAction;
use crate::dict::gloss::NodePath;
use crate::dict::media::{MediaFormat, MediaKey};
use super::measure::StyledSpan;
use super::style::{BoxStyle, Inline};

/// A `Theme` color in RGB form.
pub type Rgb = (u8, u8, u8);

/// A rectangle in panel space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SceneRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The horizontal position of a run in its wrap box.
///
/// Panel chrome uses `Leading` for every run except the frequency corner.
/// A gloss gets `Center` or `Trailing` from `textAlign`. Seven of the 72 census
/// Dictionaries declare `textAlign` on 249 242 nodes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Leading,
    Center,
    /// The frequency corner, or `textAlign: right`/`end`.
    Trailing,
}

impl Align {
    /// The fraction of a line's slack before its content.
    ///
    /// The method returns one fraction for two offsets. The line offset and the
    /// whole ink box offset use the same arithmetic with different widths.
    pub fn slack_before(self) -> f32 {
        match self {
            Align::Leading => 0.0,
            Align::Center => 0.5,
            Align::Trailing => 1.0,
        }
    }
}

/// The kind of a scene element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemKind {
    /// A rule without a text run.
    Separator,
    /// A right-aligned element that does not advance `y`.
    Corner,
    Text,
    /// A clickable collapsed row.
    Collapsed,
    /// An element with one click target for each character.
    Headword,
    /// One accent rendered as marked kana.
    ///
    /// The high mora run of a reading has an overline. The falling mora has a
    /// tick. Each mark uses one [`inline_boxes`] entry with one border edge.
    /// This notation shows the accent directly, so it needs no SVG.
    /// The `text` field contains the reading and its source Dictionary names.
    /// The element has two spans. Other panel chrome has one span.
    ///
    /// [`inline_boxes`]: SceneElem::inline_boxes
    Pitch,
    BackButton,
    /// An asset that a bin composites inline.
    ///
    /// Its `rect` is the resolved image box. Its `image` names the asset.
    /// See [`SceneImage`]. Its `text` and `spans` contain the `alt` fallback
    /// that a bin draws when the asset does not decode. It advances no `y`
    /// because the paragraph around it reserves its line.
    Image,
    /// The box of one block.
    ///
    /// Its `rect` and `block_box` are the block's border box. It contains no
    /// text. Its paragraphs follow it, and each paragraph has its own address.
    /// A browser draws a bordered `div` with three paragraphs as one border
    /// around all three runs. The block precedes the runs in draw order, so its
    /// declared background lies beneath them.
    Block,
    /// A table's block container.
    ///
    /// Its `rect` covers the whole grid. Its `block_box` is the declared
    /// `table` box. It contains no text because each table word belongs to a
    /// cell. It precedes the cells in draw order, so the table background lies
    /// beneath them.
    Table,
    /// One cell's box.
    ///
    /// Its `rect` and `block_box` are the cell's border box. The [`collapsed`]
    /// function resolves the collapse into the box's edge widths. The cell
    /// contains no text. Its paragraphs follow it, and each paragraph has its
    /// own address. A cell with two blocks has two runs inside one border.
    ///
    /// [`collapsed`]: super::table::collapsed
    Cell,
    /// The selection checkbox of an Entry header or a collapsed row.
    ///
    /// The element draws only its `block_box`: an accent border, an accent
    /// fill for `All`, a dimmed fill for `Partial`, no fill for `None`. It has
    /// no text, so both bins paint it through the box loop they already run.
    /// A layout builds it only when [`SceneRequest::selection`] is `Some`.
    ///
    /// [`SceneRequest::selection`]: super::SceneRequest::selection
    Check,
}

impl ElemKind {
    /// The stable element name for geometry snapshots.
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
            ElemKind::Check => "Check",
        }
    }
}

/// One styled piece of an element.
///
/// A span identifies a byte range in [`SceneElem::text`] and the style that
/// paints it. An element with mixed styles uses one string and a flat `Copy`
/// vector. A bin walks that vector to rebuild the exact [`MeasureRun`] that
/// produced the scene.
///
/// A span has no font family because structured content cannot change the
/// family. Every span in a panel uses the theme family.
///
/// [`MeasureRun`]: super::measure::MeasureRun
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElemSpan {
    /// Byte offset in the text.
    pub at: u32,
    pub len: u32,
    pub color: Rgb,
    pub size: f32,
    /// DirectWrite weight from 100 through 900.
    pub weight: u16,
    pub italic: bool,
    /// Baseline shift. A positive value moves upward.
    ///
    /// Layout resolves `verticalAlign` against the span's line and stores the
    /// result here. The seam reports the baseline. This field stores the offset
    /// from that baseline. Most spans use their line baseline, so this shift is
    /// zero for almost every span.
    pub shift: f32,
}

/// One reading box over its base.
///
/// CSS Ruby calls this box an *annotation box*. The name states its purpose.
/// Layout places the box outside the run, not as a span.
///
/// Layout places the box outside the element's flow and text. A reading takes
/// no horizontal space from the line below it. Layout measures the reading
/// once. A bin does not measure the reading again as part of the run. A bin
/// draws it at this box.
///
/// `x` and `y` are run-relative, like the [`LineBox`] that layout placed the
/// reading against. A bin adds [`SceneElem::pen`] and draws the box. The box
/// has no bidi or complex-script position. The reading stays centered over the
/// horizontal extent of its base.
///
/// [`LineBox`]: super::measure::LineBox
#[derive(Debug, Clone, PartialEq)]
pub struct RubyBox {
    /// The reading itself.
    pub text: String,
    /// Leading edge in run-relative space.
    pub x: f32,
    /// Top edge in run-relative space.
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub size: f32,
    pub color: Rgb,
    /// DirectWrite weight from 100 through 900.
    pub weight: u16,
    pub italic: bool,
}

impl RubyBox {
    /// The run that a bin uses to draw the reading.
    ///
    /// The run has one span and one line at [`SceneElem::wrap_w`]. Layout
    /// measures the reading at that width. If a reading exceeds the panel
    /// width, both paths wrap it the same way. Its slot matches the wrapped
    /// result height.
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

/// One list marker box in its list gutter.
///
/// CSS calls this box the *marker box*. It holds the resolved
/// `list-style-type` value. Layout keeps that value private.
///
/// Layout places the box outside the element's flow and text, like a reading.
/// It represents the CSS value `list-style-position: outside`. The marker box
/// sits *beside* the item's principal box, not inside it. It takes no horizontal
/// space from any line.
/// Every item line starts at the item's indent. This includes the first line
/// and each continuation. If a marker sat inside the run, a wrapped item's
/// second line would start under the bullet. A [`SceneElem`] is one run at one
/// wrap width, and a bin paints it from one origin. Every line in that run
/// starts at the same x.
///
/// `x` and `y` are run-relative, like the [`LineBox`] that layout placed the
/// marker against. A bin adds [`SceneElem::pen`] and draws the marker. `x` is
/// normally *negative*. The gutter is left of the content edge where the pen
/// sits. [`LIST_INDENT_EM`] provides the list's space, and the item reserves it.
///
/// [`LineBox`]: super::measure::LineBox
/// [`LIST_INDENT_EM`]: super::marker::LIST_INDENT_EM
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerBox {
    /// The marker with its trailing gap.
    pub text: String,
    /// Leading edge in run-relative space, usually negative.
    pub x: f32,
    /// Top edge in run-relative space.
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub size: f32,
    pub color: Rgb,
    /// DirectWrite weight from 100 through 900.
    pub weight: u16,
    pub italic: bool,
}

impl MarkerBox {
    /// The run that a bin uses to draw the marker.
    ///
    /// The run has one span and one line at [`SceneElem::wrap_w`]. Layout
    /// measures the marker at that width. If a marker exceeds the panel width,
    /// both paths break it the same way.
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

/// How a painter paints an asset.
///
/// This enum has the schema's two values. Fifteen of the 72 census Dictionaries
/// declare `monochrome` on 117 687 nodes. The field marks an asset as a *mask*.
/// A mask has black ink on transparency and targets a light page. An unchanged
/// mask stays invisible on a dark panel. The scene stores this field because a
/// bin needs it to choose the paint path. The media store keeps no separate copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Appearance {
    /// This variant composites the asset's pixels. It represents the schema's
    /// `auto` value and applies when a node omits the field.
    #[default]
    Auto,
    /// A mask that a painter tints with the text color.
    Monochrome,
}

/// How a painter handles pixels from a monochrome asset.
///
/// Core selects one path for each asset, so both bins use the same path.
/// One table-driven test enforces the bound instead of separate tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    /// Composite pixels without a tint.
    None,
    /// Multiply pixel alpha by the text color in place.
    Alpha,
    /// Rasterize the vector at the given pixel size before the painter
    /// multiplies its alpha.
    Raster(u32, u32),
}

/// One asset that a scene element names.
///
/// This type stores no pixels or bytes. Core gets the `rect` from the intrinsic
/// size in the media row and gets the key for the bytes. Extraction records the
/// sizes for this reason. A bin decodes the bytes behind its surface cache.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneImage {
    /// The asset to composite, or `None` for the placeholder rung.
    ///
    /// `None` means that the media store has no row. The row can lack bytes or a
    /// readable intrinsic size. Layout also reaches this state when the node has
    /// no `alt` text for the flow. The element then becomes a box that a bin
    /// outlines. This box is the last rung in the ladder. It is never empty
    /// because an empty result leaves a hole in a word.
    pub key: Option<MediaKey>,
    /// The format from the media row. It tells the tint path whether it must
    /// rasterize a vector first.
    pub format: Option<MediaFormat>,
    pub appearance: Appearance,
    /// This field tells the painter whether to apply the schema's `background`
    /// fill. If it is `false`, the painter skips the backing fill. Every image
    /// node in the census samples requests `false`.
    pub background: bool,
    /// The scene stores these fields, but no code uses them. The specification
    /// has no hover-to-reveal control. The scene keeps the fields so a later
    /// specification need not derive them again.
    pub collapsed: bool,
    pub collapsible: bool,
}

impl SceneImage {
    /// Select the paint path for this asset, a resolved box, `em`, and device
    /// pixel ratio.
    ///
    /// Hoshi Reader sets this bound because rasterize-then-tint costs more.
    /// A vector has no pixels until code selects a size. If code used the panel
    /// size, a default-object SVG of 300x150 pixels would rasterize again after
    /// every theme change. This path therefore applies only to a gaiji-sized
    /// box no larger than [`IMAGE_TINT_EM`] on either axis.
    /// The code rasterizes at twice the device pixel ratio to keep the mark
    /// legible. It limits the longest edge to [`IMAGE_TINT_CLAMP`]. A larger
    /// monochrome SVG asset composites without a tint, as Hoshi Reader
    /// specifies. An illustration is not a mask.
    pub fn tint(&self, rect: SceneRect, em: f32, dpr: f32) -> Tint {
        if self.appearance != Appearance::Monochrome {
            return Tint::None;
        }
        if self.format != Some(MediaFormat::Svg) {
            // A raster mask already has pixels. The painter needs one pass
            // over the cached surface.
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
///
/// This is a border box in panel space. A fill covers the box. A stroke with
/// the used width follows its inside edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElemBox {
    /// The border box in panel space. A fill covers this box. A stroke with the
    /// used width follows its inside edge.
    pub rect: SceneRect,
    pub style: BoxStyle,
}

/// The Dictionary location of a scene element.
///
/// A hit must carry this stable identity. A hit on a sense means "sense 3 of
/// 大辞林", not a character range. `dict_id` and `entry_id` identify the term
/// bank row. `path` identifies the subtree in that row's parsed tree.
/// [`Selection::Nodes`] consumes that path. A picker that uses this type gives
/// Anki formatted markup for the selected senses, not flattened text.
///
/// The block pass descends the tree and creates the origin in that pass. It
/// extends the path once per level with [`NodePath::child`]. No later code
/// searches for the path.
///
/// [`Selection::Nodes`]: crate::dict::gloss::Selection::Nodes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlossOrigin {
    pub dict_id: i64,
    pub entry_id: i64,
    /// The ordinal of this element's [`GlossEntry`] in the Card, from
    /// [`crate::select::entries`]. A selection keys on this ordinal because
    /// a demo or a fixture gives every row [`NO_ROW`] as `entry_id`.
    ///
    /// [`GlossEntry`]: crate::present::GlossEntry
    /// [`NO_ROW`]: crate::present::NO_ROW
    pub entry: u32,
    /// The node that this element renders, or `None` when it has no address.
    ///
    /// A [`NodePath`] reaches 16 levels and 65 536 siblings. Past either limit,
    /// it returns nothing. It never returns a path for an ancestor. An
    /// unaddressable node is a correct selection result. A silently aliased
    /// node is not.
    pub path: Option<NodePath>,
}

/// One run of [`SceneElem::text`] and the Dictionary leaf that supplied it.
///
/// `at..at + len` is a byte range in the element text. For a text leaf it
/// maps to bytes `byte..byte + len` of that leaf. For a ruby node, `atomic`
/// is true, the run is the base text, and `byte` is zero: a selection takes
/// the whole reading or none of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextSource {
    pub at: u32,
    pub len: u32,
    pub path: NodePath,
    pub byte: u32,
    pub atomic: bool,
}

/// One measured element with a position.
///
/// This plain data contains everything a bin needs to paint the element and
/// everything a snapshot needs to compare it.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneElem {
    pub kind: ElemKind,
    /// Empty for the `Separator` element kind.
    pub text: String,
    pub color: Rgb,
    /// Zero for the `Separator` element kind.
    pub font_size: f32,
    /// DirectWrite weight from 100 through 900.
    ///
    /// A rule uses `REGULAR_WEIGHT` because it has no text.
    pub weight: u16,
    pub italic: bool,
    /// Gap that layout adds above this element.
    pub top_gap: f32,
    /// Wrap width that layout gives to the text.
    pub wrap_w: f32,
    pub align: Align,
    /// The point where the wrap box starts. A bin gives this point to its text
    /// engine.
    pub pen: (f32, f32),
    /// The measured ink box.
    ///
    /// Its leading edge is the widest line's edge after [`align`] applies. An
    /// aligned element puts `rect.x` to the right of `pen.0` by the line's
    /// slack. A run-relative box does not use `rect` as its origin. A
    /// [`RubyBox`] stores the same slack in its `x`. A [`MarkerBox`] hangs in
    /// the gutter from the content edge without slack. Both boxes draw from
    /// [`pen`]. If a bin adds either box to `rect.x`, it counts the slack twice.
    ///
    /// [`align`]: Self::align
    /// [`pen`]: Self::pen
    pub rect: SceneRect,
    /// Number of wrapped lines.
    pub lines: u32,
    /// Distance that the layout walk adds to `y`.
    pub advance: f32,
    /// Styled pieces for this element, in reading order.
    ///
    /// Panel chrome has one span for each element. A gloss has one span for each
    /// style change. The vector is empty for `Separator` because that element
    /// has no text.
    pub spans: Vec<ElemSpan>,
    /// Readings for this element. Each box sits over its base.
    ///
    /// The vector is empty except for a gloss paragraph with `ruby`. Fourteen of
    /// 72 census Dictionaries have `ruby`. A reading is not in `text` or
    /// `spans` because it does not flow. See [`RubyBox`].
    pub ruby: Vec<RubyBox>,
    /// List markers for this element. Each box sits in its list gutter.
    ///
    /// The vector is empty except for the first paragraph of a list item. It
    /// has more than one marker only when the item's entire content is a nested
    /// list. Markers from the two levels share one line and hang in different
    /// gutters. A browser draws this shape, and Jitendex uses the same shape.
    /// A marker is not in `text` or `spans` because it does not flow. See
    /// [`MarkerBox`].
    pub marker: Vec<MarkerBox>,
    /// The box that this element *is*, or `None` when it draws no box.
    ///
    /// Its rect is the border box. The element's own `pen`, `wrap_w`, and
    /// `advance` include the margin, border, and padding around it. A bin paints
    /// this box. It does not change the other fields.
    pub block_box: Option<ElemBox>,
    /// Boxes that a bin paints around runs *inside* this element. The vector has
    /// one box for each line that a run touches.
    ///
    /// A pill is an inline box because that is its definition. A pill keeps its
    /// place on its line and does not break a line.
    ///
    /// These boxes provide decoration only. The layout walk does not use them
    /// for stacked geometry. One box here is not the run's complete box. The
    /// run reserves horizontal space for its margin, border, and padding before
    /// it wraps. Therefore, `spans`, `rect`, and `advance` already contain that
    /// space. A rect here draws inside that space. A box without ink has no entry,
    /// but it still spaces its content.
    pub inline_boxes: Vec<ElemBox>,
    /// The `GlossOrigin` for this element, or `None` for the panel's own chrome.
    pub origin: Option<GlossOrigin>,
    /// The asset that an [`ElemKind::Image`] element composites, or `None` for
    /// every other kind.
    ///
    /// This field stays separate from `kind` because [`ElemKind`] is `Copy`, and
    /// a [`MediaKey`] owns the path. The path is a dictionary-authored string
    /// and must round-trip verbatim. The scene stores the path once instead of
    /// copying it for every element on every frame.
    pub image: Option<SceneImage>,
    /// Which leaf bytes each run of `text` came from, in text order.
    ///
    /// A selection addresses a Dictionary node and a byte in it, not a screen
    /// position. This table maps a hit in `text` back to that address and maps
    /// a stored selection forward to a highlight. Separators, markers, pill
    /// spacers, ruby fillers, image spacers, and row numbers have no source.
    /// Panel chrome has no source. See [`TextSource`].
    pub sources: Vec<TextSource>,
}

impl SceneElem {
    /// The run that a bin measures again and paints for this element.
    ///
    /// The run uses the scene's spans in the same order and at the same width.
    /// The ink then lands where the hit rects specify. The bin supplies `font`
    /// because it knows the installed family.
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

    /// Every box for the painter, from outermost to innermost.
    ///
    /// The element's own box comes before its inner runs. A pill therefore draws
    /// over the block that contains it, and both draw beneath the text. The
    /// iterator gives a bin one loop, so both bins use one box order.
    pub fn boxes(&self) -> impl Iterator<Item = &ElemBox> {
        self.block_box.iter().chain(self.inline_boxes.iter())
    }

    /// This element's box at the scroll position that a bin uses.
    ///
    /// The scene reports all rects in unscrolled panel space. For an image
    /// element, `pen` is its top-left corner, so the whole box moves with it.
    ///
    /// This method stays here because both bins need the same calculation. If
    /// one bin moves a rect and the other does not, an image can use the
    /// previous frame's scroll.
    pub fn rect_at(&self, pen_y: f32) -> SceneRect {
        SceneRect { y: pen_y, ..self.rect }
    }
}

/// A clickable region in the panel.
///
/// `None` for `x` or `w` makes the target span the panel. A row therefore stays
/// clickable across the full panel width.
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
    /// The heading has `None`.
    pub idx: Option<usize>,
    pub text: String,
    pub color: Rgb,
    /// The row position is local to the column. Add `SidePanel::origin_y` to get
    /// the panel-space position.
    pub y: f32,
    pub h: f32,
}

/// The "See also" column.
#[derive(Debug, Clone, PartialEq)]
pub struct SidePanel {
    /// The column's top edge and rule.
    pub origin_y: f32,
    /// The vertical rule's left edge.
    pub rule_x: f32,
    pub rule_w: f32,
    pub col_x: f32,
    pub col_w: f32,
    /// The column height.
    pub height: f32,
    pub rows: Vec<SideRow>,
}

impl SidePanel {
    /// Return rows that remain visible after the scroll.
    pub fn visible(&self, scroll: f32, view_h: f32) -> impl Iterator<Item = PaintedRow<'_>> {
        let origin_y = self.origin_y;
        self.rows.iter().filter_map(move |row| {
            let y = origin_y + row.y - scroll;
            on_panel(y, row.h, 0.0, view_h).then_some(PaintedRow { row, y })
        })
    }
}

/// One row that a bin can paint.
#[derive(Debug, Clone, Copy)]
pub struct PaintedRow<'a> {
    pub row: &'a SideRow,
    /// Top edge after the scroll offset.
    pub y: f32,
}

/// The Anki affordance slot.
///
/// Core reserves the strip below the panel and provides its label. Each bin
/// builds the affordance in its own way. Windows gives it a separate window,
/// and that window sets its font size. Windows uses only the strip. A bin that
/// paints in the panel uses the whole `rect`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnkiSlot {
    pub label: String,
    pub color: Rgb,
    pub rect: SceneRect,
}

/// One popup after layout.
///
/// Layout uses its own pixel space for heights and widths. A bin scales them for
/// output with `panel_w`, `view_h`, and `content_h`.
#[derive(Debug, Clone, PartialEq)]
pub struct PopupScene {
    /// Inner padding at both origins.
    pub origin: f32,
    /// The main column's width.
    pub content_w: f32,
    /// Elements in draw order.
    pub elems: Vec<SceneElem>,
    /// Targets for the main column, in draw order.
    ///
    /// The side column keeps its rows in `side`. `hit_targets` builds targets for
    /// both columns and returns them in paint order.
    pub hits: Vec<HitTarget>,
    pub side: Option<SidePanel>,
    pub anki: Option<AnkiSlot>,
    /// Height of the main column before padding. Layout sets this height.
    pub used_h: f32,
    /// Body height plus padding before any clamp.
    pub content_h: f32,
    /// `content_h` limited to the box height.
    pub view_h: f32,
    /// Width that the panel requests, or `None` to keep the offered width.
    /// `None` means the main column fills the width, so no side column exists.
    pub panel_w: Option<f32>,
    /// Selection highlight boxes in the same unscrolled panel space as
    /// `elems[].rect`, one box per touched line.
    ///
    /// A bin fills each box with `theme.accent` at [`HIGHLIGHT_ALPHA`] after
    /// the panel background and before every element. Layout computes the
    /// boxes because only layout knows which run bytes a selection covers.
    /// The vector is empty when the request carried no selection.
    ///
    /// [`HIGHLIGHT_ALPHA`]: super::HIGHLIGHT_ALPHA
    pub highlights: Vec<SceneRect>,
}

/// One element that a bin can paint.
///
/// `pen` origin after the scroll offset applies.
#[derive(Debug, Clone, Copy)]
pub struct Painted<'a> {
    pub elem: &'a SceneElem,
    /// Scroll-applied pen origin.
    pub pen: (f32, f32),
}

impl PopupScene {
    /// Elements to paint in order.
    ///
    /// This method applies the scroll and removes elements outside the panel.
    /// The bins do not apply this filter. D2D clips, but tiny-skia does not.
    /// Both bins must therefore paint the same panel.
    pub fn visible(&self, scroll: f32, view_h: f32) -> impl Iterator<Item = Painted<'_>> {
        self.elems.iter().filter_map(move |elem| {
            let pen = (elem.pen.0, elem.pen.1 - scroll);
            // Ink can extend beyond the measured box. One em of slack keeps a
            // boundary element's ascender visible.
            on_panel(pen.1, elem.rect.h, elem.font_size, view_h)
                .then_some(Painted { elem, pen })
        })
    }

    /// Every target in paint order.
    ///
    /// The main column comes first, then the side column. A hit test takes the
    /// first match, so this order must equal paint order.
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

    /// This scene's scrollbar thumb as `(top, height)` for a panel of `view_h`.
    /// The panel has `pad` pixels of padding.
    ///
    /// The scene knows its content height, so both bins derive these values the
    /// same way. The track equals the view minus padding at both ends. The total
    /// equals the body plus that padding. This arithmetic belongs with `used_h`.
    /// Three call sites once held these lines. A fourth call site could get them
    /// wrong.
    ///
    /// The result uses whole pixels because a thumb uses whole pixels. Core does
    /// this arithmetic with `i32`. Each bin rounds its view height and scroll
    /// before it calls this method.
    pub fn scrollbar_thumb(&self, pad: i32, view_h: i32, scroll: i32) -> Option<(i32, i32)> {
        let total = self.used_h.ceil() as i32 + 2 * pad;
        super::scrollbar_thumb(view_h - 2 * pad, total, view_h, scroll)
    }
}

/// Tell whether a bin must paint this box.
pub(super) fn on_panel(y: f32, h: f32, slack: f32, view_h: f32) -> bool {
    y + h + slack > 0.0 && y - slack < view_h
}

/// A textless element before the grid assigns its position.
///
/// A table and a cell are boxes, not runs. They contain no text or spans. The
/// grid defines their geometry, not the measurer. Layout adds each box as a
/// placeholder because a container must precede its contents in draw order.
/// Layout sets the container's extent after it places all boxes.
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
        sources: Vec::new(),
    }
}

/// The largest box that lets a tint rasterize a monochrome vector on each axis,
/// in ems.
///
/// Hoshi Reader sets this bound. A gaiji has `height: 1em`. Four ems on each
/// side admit the largest gaiji and reject an illustration. This bound keeps
/// that distinction.
pub(super) const IMAGE_TINT_EM: f32 = 4.0;

/// The factor for device pixels per scene pixel for a tinted vector, in addition
/// to the device pixel ratio.
///
/// The factor is two. A mask that replaces a character must not become the
/// panel's only soft element.
pub(super) const IMAGE_TINT_SCALE: f32 = 2.0;

/// The maximum longest edge of a tinted raster, in pixels.
pub(super) const IMAGE_TINT_CLAMP: f32 = 256.0;

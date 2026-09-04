//! This module interprets CSS declarations for internal dictionary content.
//!
//! **One reason to change:** Add a property, unit, or notation only when a
//! real dictionary uses it. Use the census in
//! `docs/research/dict-shapes.md` to identify such values. When you add a
//! property, update [`apply_style`] or [`apply_box`]. Add a parser for the
//! property's value.
//!
//! This module does not replace `crate::ui::css`. That module maps a user
//! theme to [`Theme`]. This module interprets dictionary style declarations.
//! The theme defines the panel appearance. A declaration here defines one
//! gloss node's appearance.
//!
//! This module produces two records: [`Inline`] and [`Block`].
//! [`Inline`] holds every property of [`StyledSpan`](super::measure::StyledSpan)
//! except font family. [`Block`] holds box properties and inherited CSS
//! properties. CSS does not inherit box style. Therefore, a nested block
//! does not repeat its parent's margin.

use crate::dict::gloss::{GlossDoc, NodeId, Scalar, StyleKey, Tag};
use crate::ui::theme::Theme;
use super::gloss::Paragraphs;
use super::measure::LineBox;
use super::scene::{Align, Rgb};

/// One value for each box edge, in CSS order.
///
/// This generic struct supports two box model types.
/// It stores numbers for margin, padding, and border width.
/// It stores [`BorderStyle`] values for edge styles.
/// One shorthand parser handles both types. For example, `padding:
/// 0.2em 0.3em` and `border-style: none none none solid` use the
/// same four-value grammar. Dictionaries use both formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Edges<T> {
    /// Set all four edges to the same value.
    ///
    /// This method matches a one-value CSS shorthand declaration.
    pub fn all(v: T) -> Edges<T> {
        Edges { top: v, right: v, bottom: v, left: v }
    }
}

impl Edges<f32> {
    /// Add top and bottom edges.
    ///
    /// This sum is the vertical space that a box adds to its content.
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }

    /// Add left and right edges.
    ///
    /// This sum is the horizontal space that a box adds to its content.
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// Set negative edge values to zero.
    ///
    /// CSS sets negative padding and border width to zero.
    /// CSS permits negative margin values.
    pub fn non_negative(self) -> Edges<f32> {
        Edges {
            top: self.top.max(0.0),
            right: self.right.max(0.0),
            bottom: self.bottom.max(0.0),
            left: self.left.max(0.0),
        }
    }
}

/// The border style for one edge.
///
/// CSS uses `none` as the initial value. This value sets the used width
/// of the edge to zero. This enum also uses `none` as its default.
/// A dictionary that declares a border width without a style draws no
/// border.
///
/// The styles `groove`, `ridge`, `inset`, and `outset` map to
/// [`Solid`]. Browsers draw these styles as solid lines at small
/// widths. The style `hidden` maps to [`None`].
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
    /// Return a stable name for snapshots.
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

/// Margin, padding, border, and background fill for one box.
///
/// The census in `docs/research/dict-shapes.md` lists these properties
/// among common dictionary styles. Dictionaries use margin, padding,
/// border style, border width, and border radius for badges and pills.
///
/// CSS does not inherit box model properties. A nested block that
/// inherited a margin would apply that margin twice.
///
/// Only inline `style` attributes currently populate this struct.
/// A dictionary's `styles.css` file resolves into `GlossDoc`.
/// Therefore, this struct does not process external CSS files directly.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BoxStyle {
    pub margin: Edges<f32>,
    pub padding: Edges<f32>,
    /// Declared border width for each edge.
    ///
    /// See [`border_used`](Self::border_used) for the rendered width.
    pub border: Edges<f32>,
    pub border_style: Edges<BorderStyle>,
    /// One color for all four edges.
    ///
    /// The initial CSS value is `currentColor`. This struct seeds
    /// the color from the resolved text color of the node. If a
    /// border specifies only width and style, it still renders.
    pub border_color: Rgb,
    /// One corner radius for all four corners.
    pub radius: f32,
    /// Background color.
    ///
    /// `None` indicates a transparent background.
    /// In CSS, transparency is the initial value.
    pub background: Option<Rgb>,
}

impl BoxStyle {
    /// Return the border widths that appear on screen.
    ///
    /// The style `border-style: none` sets the edge width to zero.
    /// For example, `border-style: none none none solid` draws
    /// only a left border line.
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

    /// Check whether the box draws a background or a border.
    ///
    /// A painter calls this method before it builds a path.
    /// A box that adds only empty space draws nothing.
    pub fn paints(self) -> bool {
        self.background.is_some() || self.border_used() != Edges::default()
    }

    /// Check whether this box adds margins, padding, or borders.
    ///
    /// Return `true` when box geometry changes the layout advance, pen
    /// position, or wrap width. A gloss node without box properties gains
    /// no extra space when it measures or stacks.
    pub fn spaces(self) -> bool {
        let b = self.border_used();
        self.margin != Edges::default()
            || self.padding != Edges::default()
            || b != Edges::default()
    }

    /// Check whether this box occupies space or paints content.
    ///
    /// Every gloss paragraph resolves a box style. The border color
    /// defaults to the text color, but an empty box paints nothing.
    /// The renderer keeps boxes that occupy space or draw ink.
    pub fn exists(self) -> bool {
        self.spaces() || self.paints()
    }

    /// Return one border width when all edges match and render.
    ///
    /// Return `Some(width)` when all four edges have the same non-zero
    /// width. A single rounded path requires equal border widths.
    /// If edge widths differ, or an edge uses style `none`, this method
    /// returns `None`. When this method returns `None`, the painter draws
    /// each edge as an individual rectangle.
    ///
    /// This method uses one border rule for all platform renderers.
    /// It prevents behavior differences between Direct2D and tiny-skia.
    pub fn even_border(self) -> Option<f32> {
        let e = self.border_used();
        let even = e.top == e.right && e.right == e.bottom && e.bottom == e.left;
        (even && e.top > 0.0).then_some(e.top)
    }
}

/// The Yomitan base font size in CSS pixels.
///
/// The popup does not use this fixed size. The user theme controls the
/// font size. Yomitan stylesheets calculate lengths from this base value.
/// The Yomitan specification defines cell borders as `1em / 14`.
/// Thus, a declared `12px` size corresponds to twelve-fourteenths of an em.
/// Pixel lengths scale with the popup on a screen with high pixel density.
pub(super) const YOMITAN_BASE_PX: f32 = 14.0;

/// The two font sizes for relative CSS lengths.
///
/// The units `em` and `%` use the element font size.
/// The unit `rem` uses the document root font size.
/// A child element can change its font size, so this struct stores both values.
///
/// The root font size is the theme body size for the panel. Yomitan sets
/// this size from the user font setting. Specifically, `setFontOptions` in
/// `ext/js/display/display.js` writes the user setting to
/// `documentElement.style.fontSize`. Therefore, `0.4rem` scales with the
/// user's popup size. If code set `rem` to [`YOMITAN_BASE_PX`], the size
/// would not scale when the user changes settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ems {
    /// The resolved font size of the element.
    ///
    /// The units `em` and `%` use this value as a base.
    pub(super) own: f32,
    /// The root font size of the popup, which is the panel body size.
    ///
    /// The unit `rem` uses this value as a base.
    pub(super) root: f32,
}

/// The ratio between adjacent CSS absolute-size keywords.
///
/// The keywords `smaller` and `larger` use this ratio.
/// HTML user agent styles assign `smaller` to `small`, `sub`, and `sup`.
/// They assign `larger` to `big`.
///
/// Code multiplies or divides by this constant. One constant prevents
/// roundoff errors when code changes a size by one step.
pub(super) const FONT_STEP: f32 = 1.2;

/// The numeric weight for `font-weight: normal`.
///
/// This value is the initial CSS font weight.
/// Horizontal rules also use this weight because separators have no text.
pub(super) const REGULAR_WEIGHT: u16 = 400;

/// The numeric weight for `font-weight: bold`.
///
/// This value matches the DirectWrite weight scale. HTML assigns this
/// weight to `b` and `strong`. The spec assigns it to a table header cell.
pub(super) const BOLD_WEIGHT: u16 = 700;

/// One step in the CSS relative font-weight table.
///
/// In the 400 to 900 range used by dictionaries, `bolder` and `lighter`
/// change the weight by exactly this amount.
pub(super) const WEIGHT_STEP: u16 = 300;

/// The vertical shift for `vertical-align: super` as a fraction of em.
///
/// CSS defines this position from font metrics. The measurement seam
/// reports line and span geometry instead of font tables.
/// Text engines use one third of an em upward as the standard default
/// when a font has no such metric.
pub(super) const SUPER_RISE: f32 = 1.0 / 3.0;

/// The vertical shift for `vertical-align: sub` as a fraction of em.
///
/// Text engines use one fifth of an em downward as the standard default.
pub(super) const SUB_DROP: f32 = 1.0 / 5.0;

/// The vertical alignment modes for a span relative to its line.
///
/// The values `baseline`, `sub`, and `super` depend only on font size.
/// The tree walk resolves them directly into [`Inline::shift`].
/// Other alignments depend on line dimensions. The layout resolves them
/// after measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VAlign {
    /// Shift is already calculated in [`Inline::shift`].
    Fixed,
    /// Top edge aligns with the top edge of the line.
    TextTop,
    /// Bottom edge aligns with the bottom edge of the line.
    TextBottom,
    /// Vertical center aligns half an x-height above the baseline.
    Middle,
}

/// The resolved inline style for one text span in a paragraph.
///
/// This struct holds every property of [`StyledSpan`] except font family.
/// Structured content cannot change the font family.
/// This struct also records vertical alignment within a line.
/// The layout combines adjacent text segments with identical styles into
/// one span.
///
/// [`StyledSpan`]: super::measure::StyledSpan
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Inline {
    pub(super) size: f32,
    pub(super) weight: u16,
    pub(super) italic: bool,
    pub(super) color: Rgb,
    /// Vertical baseline shift. Positive values move upward.
    pub(super) shift: f32,
    pub(super) align: VAlign,
}

impl Inline {
    /// Create default inline style from theme settings.
    ///
    /// A gloss node inherits these values before local styles apply.
    pub(super) fn body(theme: &Theme) -> Inline {
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

/// The resolved block-level style for one node in a paragraph.
///
/// This struct gives a block the properties that [`Inline`] gives a span.
/// CSS defines different inheritance rules for the two parts.
/// CSS does not inherit `style`. Therefore, each node starts with an empty
/// box, and a nested block does not repeat its parent's margin.
/// CSS inherits `align` and `pre_line`. Therefore, a centered block centers
/// the blocks inside it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Block {
    /// The box that this node declares.
    pub(super) style: BoxStyle,
    pub(super) align: Align,
    /// True when the node sets `whiteSpace: pre-line`.
    pub(super) pre_line: bool,
    /// The total indent from every list level above this node.
    ///
    /// This field is inherited, but it is not part of `style`.
    /// A box property belongs to the node that declares it.
    /// A paragraph carries only the box of the block that opened it.
    ///
    /// An indent in `style` would keep only the innermost level.
    /// The path `ul > li > ul > li` would then indent once.
    /// [`Block::inherited`] clears the box and keeps this field.
    /// Each list level adds to the total with one `+=` operation.
    ///
    /// This field stays separate from `style.padding.left`.
    /// A dictionary can declare `paddingLeft` on an `li`, and that node
    /// gets both values. Jitendex writes exactly that style.
    /// This indent is the list padding. It moves an item's border box.
    /// It does not inset the text inside that box.
    pub(super) indent: f32,
}

impl Block {
    /// Return the style that a child inherits: inherited properties and
    /// an empty box.
    ///
    /// An inline node carries this style on its line. A box belongs to the
    /// node that declares it. The renderer draws an inline node's box around
    /// its own content, not around the paragraph. See [`Paragraphs::node`].
    /// Therefore, a `data.content` marker that opens a `span` gets the
    /// inherited properties and no box.
    pub(super) fn inherited(self) -> Block {
        Block { style: BoxStyle::default(), ..self }
    }
}

/// Strip quote characters from a CSS `<string>`.
///
/// This function accepts both quote characters because CSS accepts both.
/// A dictionary writes its style into JSON. The dictionary selects the
/// quote character that needs fewer escape characters.
/// The census contains both `"'①'"` and `"\"◍\""`.
pub(super) fn quoted(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    text.strip_prefix(quote)?.strip_suffix(quote)
}

/// Apply the HTML user agent style for tags that structured content permits.
///
/// CSS does not inherit `verticalAlign`. Therefore, each node starts on
/// its line baseline. A `sup` inside a `sup` rises once.
pub(super) fn tag_style(tag: Tag, parent: Inline) -> Inline {
    let mut style = Inline { shift: 0.0, align: VAlign::Fixed, ..parent };
    match tag {
        // The defaults table in the spec makes a header cell bold.
        // Its colored background is a box property. The renderer resolves it
        // separately. Yomitan sets the weight on `thead` and `th`. Therefore,
        // a plain `td` in a header row inherits the weight. This is the only
        // reason to include `thead` here, because `thead` has no text of its
        // own.
        //
        // A `summary` is the heading of its `details` element. The same table
        // gives it the same weight. The panel expands the pair and shows no
        // disclosure control. See Out of Scope in the spec. The weight
        // therefore tells the reader which block is the label.
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

/// Apply one resolved style property to a span style.
///
/// This function handles only properties that a styled span carries.
/// [`apply_box`] handles box properties. `listStyleType` belongs to a list,
/// and [`marker_of`] reads it from the same record.
/// If this build cannot read a value, it keeps the inherited value.
/// It does not guess a replacement.
///
/// [`marker_of`]: super::marker::marker_of
pub(super) fn apply_style(doc: &GlossDoc, key: StyleKey, value: Scalar, em: Ems, out: &mut Inline) {
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

/// Apply one resolved style property to a block box.
///
/// This function handles properties that the census marks as common and
/// that earlier builds did not support. See `docs/research/dict-shapes.md`.
/// It handles no other property.
///
/// `color` belongs to a span, and [`apply_style`] resolves it.
/// [`marker_of`] resolves `listStyleType`, which is not part of a box.
/// If this build cannot read a value, the box keeps its earlier state.
/// The function never applies part of a declaration.
///
/// CSS forbids negative padding, border width, and radius.
/// CSS permits negative margin. Each match arm clamps the values that
/// CSS clamps.
///
/// [`marker_of`]: super::marker::marker_of
pub(super) fn apply_box(doc: &GlossDoc, key: StyleKey, value: Scalar, em: Ems, out: &mut Block) {
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

/// Return a box length in the panel's pixels.
///
/// A box length can be zero or negative. [`length_px`] permits neither
/// value. A declaration of `marginRight: 0` can override a wider rule,
/// and Jitendex writes exactly that style.
/// CSS also permits a negative margin. If this function removed either
/// value, the wider rule would remain effective without an error message.
pub(super) fn box_len(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<f32> {
    match value {
        Scalar::Num(n) => finite(em.own * n as f32),
        Scalar::Text(span) => css_len(doc.span(span), em),
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// Return one to four box lengths in the order that [`edges_of`] uses.
pub(super) fn box_edges(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<Edges<f32>> {
    match value {
        Scalar::Num(n) => Some(Edges::all(finite(em.own * n as f32)?)),
        Scalar::Text(span) => edges_of(doc.span(span), |s| css_len(s, em)),
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// Return one border style for each edge.
///
/// This function uses the shorthand grammar that [`edges_of`] reads.
/// A dictionary writes `border-style: none none none solid`.
/// That declaration draws a left border line and nothing else.
pub(super) fn border_styles(doc: &GlossDoc, value: Scalar) -> Option<Edges<BorderStyle>> {
    edges_of(doc.scalar_str(value)?, border_style_of)
}

/// Read the CSS one-to-four-value edge grammar in CSS order.
///
/// The number of values selects the assignment:
///
/// 1. One value sets all four edges.
/// 2. Two values set the vertical edges, then the horizontal edges.
/// 3. Three values set the top, right, and bottom edges. The left edge
///    copies the right edge.
/// 4. Four values set each edge in order.
///
/// A fifth value makes the declaration invalid. A value that this build
/// cannot read also makes it invalid. The box keeps its earlier state and
/// does not apply part of a shorthand.
pub(super) fn edges_of<T: Copy>(text: &str, one: impl Fn(&str) -> Option<T>) -> Option<Edges<T>> {
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

/// Read one `border-style` keyword.
///
/// The keywords `groove`, `ridge`, `inset`, and `outset` resolve to
/// `solid`. A browser draws these keywords as solid lines at the
/// small widths that dictionaries use.
/// A solid border better matches the author's intent than no border.
/// The keyword `hidden` acts as `none` plus a table border-conflict rule.
/// This renderer does not implement the cascade for that rule.
pub(super) fn border_style_of(word: &str) -> Option<BorderStyle> {
    Some(match word {
        "none" | "hidden" => BorderStyle::None,
        "solid" | "groove" | "ridge" | "inset" | "outset" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => return None,
    })
}

/// Read `textAlign` into the alignment that the scene carries.
///
/// The keywords `start` and `left` map to one value.
/// The keywords `end` and `right` map to another value.
/// The spec excludes bidirectional text as Out of Scope for this product.
/// The keyword `justify` keeps the inherited alignment.
/// It does not map to `start` because this build never justifies text.
pub(super) fn text_align_of(doc: &GlossDoc, value: Scalar) -> Option<Align> {
    Some(match doc.scalar_str(value)?.trim() {
        "start" | "left" => Align::Leading,
        "center" => Align::Center,
        "end" | "right" => Align::Trailing,
        _ => return None,
    })
}

/// Read the `whiteSpace` property. This build accepts `pre-line` and
/// `normal`.
///
/// The value `pre-line` keeps dictionary newlines, and the text still
/// wraps. The value `normal` restores normal line wrap.
/// Both text engines behave this way when a string contains a newline.
/// Therefore, support for `pre-line` needs only paragraph edge trim.
/// The values `pre`, `pre-wrap`, and `nowrap` specify no-wrap behavior
/// that the measurement seam cannot provide. Therefore, they leave the
/// paragraph unchanged.
pub(super) fn pre_line_of(doc: &GlossDoc, value: Scalar) -> Option<bool> {
    match doc.scalar_str(value)?.trim() {
        "pre-line" => Some(true),
        "normal" => Some(false),
        _ => None,
    }
}

/// Return a CSS length in the panel's pixels.
///
/// A number follows the Yomitan em-multiplier convention. The HTML
/// renderer prints the same number with an `em` suffix.
/// A string carries its own unit, and [`css_len`] reads that unit.
/// Zero and negative values are not valid font sizes. Therefore, a
/// `fontSize: 0` declaration keeps the inherited size.
pub(super) fn length_px(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<f32> {
    let px = match value {
        Scalar::Num(n) => em.own * n as f32,
        Scalar::Text(span) => css_len(doc.span(span), em)?,
        Scalar::Bool(_) | Scalar::Null => return None,
    };
    (px.is_finite() && px > 0.0).then_some(px)
}

/// Read one CSS length. Keep its unit, sign, and zero value.
///
/// The units `em` and `%` are relative to the em that holds the length.
/// The unit `rem` is relative to the panel body size. See [`Ems`].
///
/// The unit `px` is relative to the Yomitan base size. See
/// [`YOMITAN_BASE_PX`]. This base makes an absolute pixel scale with the
/// panel. Therefore, the pixel does not shrink on a dense screen.
///
/// A bare number is an em multiplier, like the numeric values in the schema.
///
/// Real dictionaries use no other length unit. The corpus in
/// `docs/research/dict-shapes.md` gives these counts. Inline `style` objects
/// use `em` 5 690 564, `px` 151 516, `rem` 3 096, and no `%`.
/// Stylesheet declaration values use `em` 1 377, `px` 84, and `rem` 33.
///
/// This function does not read `s` (4), `deg` (3), or `fr` (3).
/// None of these units is a length. All three appear on properties that
/// this build does not map: `transition`, `transform`, and
/// `grid-template-*`.
pub(super) fn css_len(text: &str, em: Ems) -> Option<f32> {
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

/// Return a length that a box can use.
pub(super) fn finite(px: f32) -> Option<f32> {
    px.is_finite().then_some(px)
}

/// Read `fontWeight` on the DirectWrite scale.
///
/// CSS and fontdb use the same scale.
pub(super) fn weight_of(doc: &GlossDoc, value: Scalar, inherited: u16) -> Option<u16> {
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

/// Read `fontStyle`. Treat `oblique` as italic.
///
/// Neither engine creates a slant. A font family with one face for both
/// styles is normal.
pub(super) fn italic_of(doc: &GlossDoc, value: Scalar) -> Option<bool> {
    match doc.scalar_str(value)?.trim() {
        "italic" | "oblique" => Some(true),
        "normal" => Some(false),
        _ => None,
    }
}

/// Read `verticalAlign` and resolve it against the element's em size.
///
/// A line box here holds one line of text. Therefore, the line edges and
/// the text edges are the same two edges. The value `top` cannot differ
/// from `text-top`, and `bottom` cannot differ from `text-bottom`.
pub(super) fn align_of(doc: &GlossDoc, value: Scalar, em: f32) -> Option<(f32, VAlign)> {
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

/// Compute one span's baseline shift on the line where it lands.
///
/// [`VAlign::Fixed`] needs no line. The other values are CSS line-relative
/// values. The measurement seam reports only line height and baseline
/// position. Therefore, a span's own ascent uses the same
/// height-to-ascent ratio as its line.
pub(super) fn shift_on(style: Inline, line: LineBox, span_h: f32) -> f32 {
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
        // CSS places the box middle at half an x-height above the baseline.
        // The seam reports no x-height. Therefore, this code uses half the
        // ascent as an estimate of one x-height.
        VAlign::Middle => ascent / 4.0 - (own_ascent - own_descent) / 2.0,
        VAlign::Fixed => style.shift,
    }
}

/// Two hex digits at `at`, as a byte.
pub(super) fn hex_pair(bytes: &[u8], at: usize) -> Option<u8> {
    let hi = hex_digit(*bytes.get(at)?)?;
    let lo = hex_digit(*bytes.get(at + 1)?)?;
    Some(hi << 4 | lo)
}

pub(super) fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Read a CSS color in the form that the scene carries.
///
/// This function reads hex colors in all three lengths, `rgb()`, and
/// `rgba()`. It also reads the sixteen names that CSS has had since
/// level 1, plus `orange`. Dictionary `color` values use this full set.
///
/// This function ignores any alpha channel. `Rgb` is the scene's color
/// type, and the panel composites its own opacity once at the end.
/// If this function cannot read a value, the color keeps its inherited value.
pub(super) fn color_of(doc: &GlossDoc, value: Scalar) -> Option<Rgb> {
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
pub(super) fn hex_color(hex: &[u8]) -> Option<Rgb> {
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

/// Read the sixteen CSS level 1 names, both spellings of `gray`, and
/// `orange`.
pub(super) fn named_color(name: &str) -> Option<Rgb> {
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

/// This module contains the style half of the gloss walk. It folds one
/// node's declarations into the two records that the node inherits.
///
/// These methods belong to [`Paragraphs`] instead of free functions.
/// All five methods read the same two values from the walk: the style gate
/// and the theme body size. A free function would need both values at each
/// call.
impl Paragraphs<'_> {
    /// The resolved style record for one node, under the style gate.
    ///
    /// The "honor dictionary styling" setting controls this gate.
    /// This method applies the gate once. It does not repeat the check for each reader.
    /// When the setting is off, neither inline `style` nor dictionary
    /// `styles.css` reaches the record. That stylesheet resolves into the
    /// same record, not a second record. A new source added here therefore
    /// uses the gate, and downstream code does not need the setting.
    ///
    /// One declaration bypasses this gate: `display`. It changes where a line
    /// breaks, and [`GlossDoc::is_block`] reads it for every renderer. The
    /// plain-text and HTML renderers have no setting, so the popup must agree
    /// with them at either value of the setting.
    ///
    /// An empty record leaves every property unset. Only two sources remain
    /// visible: HTML's own stylesheet ([`tag_style`]) and the theme's font
    /// and colors. A `b` stays bold when style support is off because markup
    /// states what an entry says. A `style` object states how a dictionary
    /// chooses to draw it.
    ///
    /// [`styled_marker`] reads the same record under the same gate. A free
    /// function walks the whole document, so it cannot call this method.
    ///
    /// [`styled_marker`]: super::marker::styled_marker
    pub(super) fn declarations(&self, id: NodeId) -> &[(StyleKey, Scalar)] {
        if self.render.styling {
            self.doc.style(id)
        } else {
            &[]
        }
    }

    /// The resolved inline style for one node.
    ///
    /// This method applies HTML's stylesheet first. Thus, a `b` is bold and a
    /// `sup` is a raised `smaller`. It then applies the node's resolved style
    /// record. The dictionary author's record has final precedence in the
    /// cascade, and the dictionary's `styles.css` also feeds it.
    /// Both inline `style` and `styles.css` arrive through
    /// [`Paragraphs::declarations`]. Therefore, the setting disables both
    /// sources at once.
    ///
    /// The size stops at [`Paragraphs::cap`], the panel headword size. A
    /// dictionary heading at `1.5em` would outgrow the headword above it and
    /// read as a second headword. The cap applies to the resolved value, so a
    /// `big` inside a `big` and a stylesheet `em` meet the same limit. A
    /// child resolves its own lengths against the capped size, because that
    /// size is the em that the child sees.
    pub(super) fn styled(&self, id: NodeId, parent: Inline) -> Inline {
        let doc = self.doc;
        let mut style = tag_style(doc.node(id).tag, parent);
        let record = self.declarations(id);
        for (i, (key, value)) in record.iter().enumerate() {
            // The first occurrence takes precedence. This matches the result
            // that `GlossDoc::style_of` gives to every other record reader.
            if record[..i].iter().any(|(seen, _)| seen == key) {
                continue;
            }
            apply_style(doc, *key, *value, self.ems(parent.size), &mut style);
        }
        style.size = style.size.min(self.cap);
        style
    }

    /// The resolved block-level style for one node.
    ///
    /// The inherited half comes from the parent. The box starts empty
    /// because CSS does not inherit it. The `inline` parameter holds this
    /// node's resolved text style. It has two purposes.
    ///
    /// First, `inline` seeds `border-color` because CSS sets its initial
    /// value to `currentColor`. Second, `inline` supplies the em that
    /// resolves a box length. For example, `padding: 0.25em` uses one
    /// quarter of the element's own font size. But `fontSize: 0.8em`
    /// uses eight tenths of the parent's font size.
    ///
    /// A dictionary's `styles.css` also feeds this method because it adds
    /// to the same resolved record. Therefore, this method does not need
    /// separate CSS logic.
    pub(super) fn boxed(&self, id: NodeId, parent: Block, inline: Inline) -> Block {
        let start = Block {
            style: BoxStyle { border_color: inline.color, ..BoxStyle::default() },
            ..parent
        };
        self.declared(id, start, inline.size)
    }

    /// Return the two base sizes that resolve lengths in this node's
    /// declarations: the node's own em and the panel's em.
    pub(super) fn ems(&self, own: f32) -> Ems {
        Ems { own, root: self.root_em }
    }

    /// Fold this node's declarations into the box where it starts.
    ///
    /// This method stays separate from [`Paragraphs::boxed`] because a
    /// table cell starts from Yomitan's grid defaults
    /// ([`Paragraphs::cell_defaults`]), not from an empty box.
    /// The dictionary's resolved declarations must apply in both cases.
    ///
    /// The `own` parameter is the node's resolved font size. A box length
    /// is a fraction of `own`, or of the panel's root em for a `rem` unit.
    /// See [`Ems`].
    pub(super) fn declared(&self, id: NodeId, mut block: Block, own: f32) -> Block {
        let doc = self.doc;
        let em = self.ems(own);
        let record = self.declarations(id);
        for (i, (key, value)) in record.iter().enumerate() {
            // The first occurrence takes precedence here, as it does for a span.
            if record[..i].iter().any(|(seen, _)| seen == key) {
                continue;
            }
            apply_box(doc, *key, *value, em, &mut block);
        }
        block
    }
}

//! What one CSS declaration means to this renderer.
//!
//! **One reason to change:** a property, a unit or a notation real
//! dictionaries write that this build does not yet read. The census in
//! `docs/research/dict-shapes.md` is what decides that, and it decides
//! every item in this file at once - a new property needs an arm in
//! [`apply_style`] or [`apply_box`] *and* a grammar to read its value with,
//! so splitting the two apart would put one change in two files.
//!
//! Not `crate::ui::css`, which maps a *user's* theme file onto [`Theme`].
//! This is the dictionary's own styling, and the two never meet: a theme
//! decides what the panel looks like, a declaration here decides what one
//! gloss node looks like inside it.
//!
//! Two records come out: [`Inline`], which is everything a
//! [`StyledSpan`](super::measure::StyledSpan) carries bar the family, and
//! [`Block`], which is the box and the two properties CSS inherits. The
//! split is CSS's own: `style` is not inherited, so a nested block never
//! pays its parent's margin twice.

use crate::dict::gloss::{GlossDoc, NodeId, Scalar, StyleKey, Tag};
use crate::ui::theme::Theme;
use super::gloss::Paragraphs;
use super::measure::LineBox;
use super::scene::{Align, Rgb};

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
pub(super) const YOMITAN_BASE_PX: f32 = 14.0;

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
pub(super) struct Ems {
    /// The element's own resolved font
    /// size, which `em` and `%` are a
    /// fraction of.
    pub(super) own: f32,
    /// The panel's body size: the
    /// popup's root font size, which
    /// `rem` is a fraction of.
    pub(super) root: f32,
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
pub(super) const FONT_STEP: f32 = 1.2;

/// `font-weight: normal`, likewise,
/// and CSS's own initial value.
///
/// Also what a rule carries: a
/// separator has no text to weight,
/// and zero is no weight.
pub(super) const REGULAR_WEIGHT: u16 = 400;

/// `font-weight: bold`, on
/// DirectWrite's scale. HTML's
/// default for `b` and `strong`, and
/// the spec's default for a table
/// header cell.
pub(super) const BOLD_WEIGHT: u16 = 700;

/// One step of CSS's relative-weight
/// table, which over the 400-to-900
/// range dictionaries use is exactly
/// what `bolder` and `lighter` mean.
pub(super) const WEIGHT_STEP: u16 = 300;

/// `vertical-align: super`, as a
/// fraction of the em it is raised
/// inside.
///
/// CSS defines it as "the appropriate
/// superscript position", which is a
/// face metric, and the seam reports
/// line and span geometry rather than
/// a face's tables. A third of an em
/// up and a fifth down are the
/// fallbacks a text engine uses for a
/// face that declares neither.
pub(super) const SUPER_RISE: f32 = 1.0 / 3.0;

/// `vertical-align: sub`, likewise.
pub(super) const SUB_DROP: f32 = 1.0 / 5.0;

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
pub(super) enum VAlign {
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
///
/// [`StyledSpan`]: super::measure::StyledSpan
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Inline {
    pub(super) size: f32,
    pub(super) weight: u16,
    pub(super) italic: bool,
    pub(super) color: Rgb,
    /// Baseline shift, up positive.
    pub(super) shift: f32,
    pub(super) align: VAlign,
}

impl Inline {
    /// The body role: what a gloss
    /// inherits before any node of it
    /// has spoken.
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
pub(super) struct Block {
    /// This node's own box.
    pub(super) style: BoxStyle,
    pub(super) align: Align,
    /// `whiteSpace: pre-line`.
    pub(super) pre_line: bool,
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
    pub(super) indent: f32,
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
    pub(super) fn inherited(self) -> Block {
        Block { style: BoxStyle::default(), ..self }
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
pub(super) fn quoted(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    text.strip_prefix(quote)?.strip_suffix(quote)
}

/// HTML's own stylesheet, for the tags
/// structured content admits.
///
/// `verticalAlign` is not inherited -
/// CSS says so - so every node starts
/// back on its line's baseline and a
/// `sup` inside a `sup` is raised
/// once.
pub(super) fn tag_style(tag: Tag, parent: Inline) -> Inline {
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
pub(super) fn box_len(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<f32> {
    match value {
        Scalar::Num(n) => finite(em.own * n as f32),
        Scalar::Text(span) => css_len(doc.span(span), em),
        Scalar::Bool(_) | Scalar::Null => None,
    }
}

/// One to four box lengths, as
/// [`edges_of`] splits them.
pub(super) fn box_edges(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<Edges<f32>> {
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
pub(super) fn border_styles(doc: &GlossDoc, value: Scalar) -> Option<Edges<BorderStyle>> {
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
pub(super) fn text_align_of(doc: &GlossDoc, value: Scalar) -> Option<Align> {
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
pub(super) fn pre_line_of(doc: &GlossDoc, value: Scalar) -> Option<bool> {
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
pub(super) fn length_px(doc: &GlossDoc, value: Scalar, em: Ems) -> Option<f32> {
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

/// A length a box can use.
pub(super) fn finite(px: f32) -> Option<f32> {
    px.is_finite().then_some(px)
}

/// `fontWeight` on DirectWrite's
/// scale, which is also CSS's and
/// fontdb's.
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

/// `fontStyle`. Oblique is italic:
/// neither engine synthesizes a slant
/// and a family that has one face for
/// both is the normal case.
pub(super) fn italic_of(doc: &GlossDoc, value: Scalar) -> Option<bool> {
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
/// same proportion.
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
        // CSS puts the box's middle half
        // an x-height above the
        // baseline; with no x-height in
        // the seam, half the ascent is
        // the usual stand-in for one.
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

/// CSS level 1's sixteen, their two
/// British spellings, and `orange`.
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

/// The style half of the gloss walk: one node's declarations folded into
/// the two records it inherits.
///
/// Methods on [`Paragraphs`] rather than free functions because all five
/// read the same two things off the walk - the styling gate and the
/// theme's own body size - and a free function would have taken both as
/// arguments at every call.
impl Paragraphs<'_> {
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
    ///
    /// [`styled_marker`]: super::marker::styled_marker
    pub(super) fn declarations(&self, id: NodeId) -> &[(StyleKey, Scalar)] {
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
    pub(super) fn styled(&self, id: NodeId, parent: Inline) -> Inline {
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
    pub(super) fn boxed(&self, id: NodeId, parent: Block, inline: Inline) -> Block {
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
    pub(super) fn ems(&self, own: f32) -> Ems {
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
    pub(super) fn declared(&self, id: NodeId, mut block: Block, own: f32) -> Block {
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

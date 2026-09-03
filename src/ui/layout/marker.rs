//! Defines the marker beside each list item and its position.
//!
//! **One reason to change:** change the CSS list marker rules for this build.
//! These rules resolve `list-style-type`, the counter, and the gutter for the marker box.
//!
//! CSS treats marker resolution and placement as one topic.
//! CSS inherits `list-style-type`.
//! CSS draws the marker on an element with `display: list-item`.
//! An item declaration therefore overrides the list declaration.
//! `list-style-position: outside` places the marker in a gutter.
//! The list indent already reserves that gutter.
//! The marker does not sit on the item's first line.

use crate::dict::gloss::{GlossDoc, Kind, NodeId, StyleKey};
use super::flow::{Ctx, Flow, NO_LINK};
use super::gloss::Paragraphs;
use super::measure::{MeasureError, MeasureRun, Measured, StyledSpan, TextMeasure};
use super::ruby::NO_RUBY;
use super::scene::MarkerBox;
use super::style::{quoted, Block, Inline};

/// Geometry for one measured list marker.
///
/// [`measure_markers`] computes these values, and [`place_markers`] uses them.
/// `baseline` is an absolute distance, not a fraction of height.
/// The marker baseline aligns with the item's first baseline.
/// Layout subtracts the two baselines directly.
#[derive(Clone, Copy, Default)]
pub(super) struct MarkerMetrics {
    pub(super) w: f32,
    pub(super) h: f32,
    /// Distance from the run top to the marker baseline.
    pub(super) baseline: f32,
}

/// Measures all markers for a paragraph.
///
/// The function measures one run for each marker.
/// It leaves the paragraph run unchanged.
/// [`measure_readings`] must run first because ruby text increases line height.
/// A marker stays in the gutter that [`LIST_INDENT_EM`] reserves for the list.
/// A marker takes no horizontal space from a line and adds no height.
/// It needs no filler span, second measurement, or edit to a line box.
///
/// The function uses the paragraph wrap width, not the gutter width.
/// A visible marker contains one glyph, counter, or string.
/// A marker can extend beyond the gutter edge, like browser text `10.`.
///
/// [`measure_readings`]: super::ruby::measure_readings
pub(super) fn measure_markers(
    m: &mut dyn TextMeasure,
    font: &str,
    flow: &Flow,
    max_w: f32,
) -> Result<Vec<MarkerMetrics>, MeasureError> {
    if flow.marker.is_empty() {
        return Ok(Vec::new());
    }
    // Only a paragraph that contains a marker needs this scratch buffer.
    // The tree walk does not share this buffer.
    let mut scratch = Measured::default();
    let mut out = vec![MarkerMetrics::default(); flow.marker.len()];
    for (slot, mark) in flow.marker.iter().enumerate() {
        m.measure(
            MeasureRun { spans: &[marker_span(font, mark)], max_w },
            &mut scratch,
        )?;
        let first = scratch.lines.first().copied().unwrap_or_default();
        out[slot] =
            MarkerMetrics { w: scratch.metrics.w, h: scratch.metrics.h, baseline: first.baseline };
    }
    Ok(out)
}

/// Places all markers for a paragraph.
///
/// This pure function reads two values from the measurer:
/// 1. The first line box, for the shared baseline.
/// 2. The measured width of each marker.
///
/// `lead_left` is the paragraph's left offset.
/// It includes every parent list level, paragraph margin, border, and padding.
/// `pen_x` is the horizontal position from that offset.
/// Layout aligns each marker box's right edge with the list content edge.
/// [`FlowMarker::indent`] stores that edge.
/// `paddingLeft` moves item text but leaves the bullet position fixed.
/// The marker box includes [`MARKER_GAP`].
/// The gap separates the marker from item text in the gutter.
///
/// Alignment does not move the marker box.
/// `textAlign` moves line boxes, but a marker box is not a line box.
pub(super) fn place_markers(
    flow: &Flow,
    boxes: &[MarkerMetrics],
    measured: &Measured,
    lead_left: f32,
    pen_x: f32,
) -> Vec<MarkerBox> {
    let Some(line) = measured.lines.first().copied() else {
        return Vec::new();
    };
    flow.marker
        .iter()
        .zip(boxes)
        .filter(|(_, box_)| box_.h > 0.0)
        .map(|(mark, box_)| MarkerBox {
            text: mark.text.clone(),
            // Keep the marker inside the panel.
            // If its width exceeds the gutter, align it with the left edge.
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

/// Creates one marker span for the measurement seam.
pub(super) fn marker_span<'a>(font: &'a str, mark: &'a FlowMarker) -> StyledSpan<'a> {
    StyledSpan {
        text: &mark.text,
        font,
        size: mark.style.size,
        weight: mark.style.weight,
        italic: mark.style.italic,
        color: mark.style.color,
    }
}

/// List indent at each level, as a multiple of the list em unit.
///
/// This value matches `--list-padding1` in `ext/css/display.css` from Yomitan.
/// Yomitan applies this rule to every glossary list.
/// Nested levels reuse this value and do not reduce it.
/// A list at depth two uses twice this indent.
/// The specification defaults table sets this value to `1.4em`.
pub(super) const LIST_INDENT_EM: f32 = 1.4;

/// `list-style-type: disc`, the initial value in CSS: U+2022 BULLET.
pub(super) const DISC_MARKER: &str = "\u{2022}";

/// `circle`: U+25E6 WHITE BULLET.
pub(super) const CIRCLE_MARKER: &str = "\u{25E6}";

/// `square`: U+25AA BLACK SMALL SQUARE.
pub(super) const SQUARE_MARKER: &str = "\u{25AA}";

/// Space between a marker and item text.
///
/// The marker box contains this final space.
/// A stacked item places the marker in the gutter ([`MarkerBox`]).
/// Its right edge touches the list content edge.
/// The gap separates the glyph from the text.
/// A compact item has no gutter.
/// It writes the marker as the paragraph's first span.
/// The gap then becomes a space character on the line.
/// One label format serves both layouts.
/// [`Marker::label`] adds the gap, so placement functions do not add it.
/// The gap matches the space after `1.` in a row number.
pub(super) const MARKER_GAP: &str = " ";

/// Resolved `list-style-type` value for a list item.
///
/// CSS supports three marker shapes:
/// 1. A bullet glyph.
/// 2. An ordinal counter.
/// 3. A literal string.
///
/// The build does not support locale counter algorithms.
/// An unknown keyword uses the initial value for the list tag.
/// The item receives a fallback marker instead of no marker.
///
/// This enum represents marker content, not the marker box.
/// [`MarkerBox`] represents the scene box and holds a rectangle.
/// This enum remains inside the layout module.
///
/// [`MarkerBox`]: super::scene::MarkerBox
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Marker<'a> {
    /// One glyph, identical on every item.
    Glyph(&'static str),
    /// `decimal`: the item's ordinal number, from one.
    Decimal,
    /// A CSS string counter that appears verbatim on every item.
    ///
    /// A dictionary uses this variant for symbols such as ①.
    /// The system runs no counter algorithm and adds no suffix.
    Literal(&'a str),
    /// `none`, or an empty string counter.
    None,
}

impl Marker<'_> {
    /// The marker text for the `n`th item with the gap.
    pub(super) fn label(self, n: usize) -> Option<String> {
        match self {
            Marker::Glyph(glyph) => Some(format!("{glyph}{MARKER_GAP}")),
            // The CSS `decimal` counter uses a `.` suffix.
            // The row number uses the same suffix.
            Marker::Decimal => Some(format!("{n}.{MARKER_GAP}")),
            Marker::Literal(text) => Some(format!("{text}{MARKER_GAP}")),
            Marker::None => None,
        }
    }
}

/// Resolves `listStyleType` for a list or item.
///
/// `fallback` is the active value.
/// The function resolves the list value and the item value.
/// CSS inherits `list-style-type` and draws the marker on `display: list-item`.
/// An item declaration overrides its list declaration.
/// An invalid declaration keeps the inherited value, as in [`apply_style`].
///
/// Items often declare this property.
/// Jitendex declares `listStyleType` on every `li` node in 38,381 entries.
/// These entries contain 97,150 `li` nodes with values from `"①"` to `"㊿"`.
/// This declaration supplies ordinal values for senses: ①②③.
/// A dictionary stylesheet can provide a list declaration.
/// For example, Jitendex writes `ul[data-sc-content="sense-groups"] { list-style-type: "＊" }`.
///
/// This function does not depend on layout mode.
/// It accepts a document, node, and fallback.
/// A compact list and a stacked list resolve the same marker.
/// See [`Paragraphs::stack_items`].
///
/// [`apply_style`]: super::style::apply_style
pub(super) fn marker_of<'a>(doc: &'a GlossDoc, id: NodeId, fallback: Marker<'a>) -> Marker<'a> {
    let Some(text) = doc.style_of(id, StyleKey::ListStyleType).and_then(|v| doc.scalar_str(v))
    else {
        return fallback;
    };
    let text = text.trim();
    if let Some(literal) = quoted(text) {
        // An empty counter removes the marker in CSS.
        return if literal.is_empty() { Marker::None } else { Marker::Literal(literal) };
    }
    match text {
        "disc" => Marker::Glyph(DISC_MARKER),
        "circle" => Marker::Glyph(CIRCLE_MARKER),
        // The corpus has no `square` keyword.
        // A `disc` fallback would draw a round bullet instead of a square bullet.
        // Handle the keyword directly.
        "square" => Marker::Glyph(SQUARE_MARKER),
        "decimal" => Marker::Decimal,
        // Do not add ink for `none`.
        // The author removed the marker with `none`.
        // A `disc` fallback would restore it incorrectly.
        "none" => Marker::None,
        // Other keywords select locale counter algorithms.
        // Examples include `lower-roman`, `katakana`, and `cjk-ideographic`.
        // The specification excludes these algorithms.
        // Keep the current fallback value.
        _ => fallback,
    }
}

/// Resolves a list or item marker with the style setting.
///
/// This function reads a node's resolved style record.
/// When the "honour dictionary styling" setting is off, this gate ignores the style.
/// [`Paragraphs::declarations`] provides the other style gates.
/// `listStyleType` is a standard CSS declaration.
/// With style off, the engine ignores this declaration.
/// A dictionary with ①②③ markers, such as Jitendex, then uses standard browser bullets.
/// This behavior keeps unstyled output consistent.
///
/// This function wraps [`marker_of`].
/// [`marker_of`] handles document data and node data without settings.
pub(super) fn styled_marker<'a>(
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

/// Returns the initial `list-style-type` for a list tag.
///
/// This value distinguishes `ol` from `ul`.
/// It starts the inheritance chain for [`marker_of`].
pub(super) fn initial_marker(ordered: bool) -> Marker<'static> {
    if ordered {
        Marker::Decimal
    } else {
        Marker::Glyph(DISC_MARKER)
    }
}

/// One list marker before placement.
///
/// Layout holds this marker beside the paragraph, not in the paragraph text.
/// See [`MarkerBox`].
/// A marker with `list-style-position: outside` takes no horizontal space on the line.
/// If the marker enters the text run, the pen advances past it.
/// The wrapped item continuation lines then start below the bullet.
#[derive(Clone)]
pub(super) struct FlowMarker {
    /// Label text with [`MARKER_GAP`].
    pub(super) text: String,
    /// Resolved item style.
    /// The CSS `::marker` element inherits from the `display: list-item` element.
    /// It does not inherit from markup inside the item.
    pub(super) style: Inline,
    /// Content edge of the list from [`Block::indent`].
    /// The marker box's right edge aligns with this position.
    ///
    /// The marker stores this value directly.
    /// Layout does not recalculate it from the paragraph.
    /// When an item contains only a nested list, both items share one line.
    /// Their markers hang in two different gutters.
    pub(super) indent: f32,
}

/// Walks list elements in gloss content: `ul`, `ol`, list items, and markers.
impl Paragraphs<'_> {
    /// Walks one `ul` or `ol` element with indented items and markers.
    ///
    /// The list element defines marker settings.
    /// It declares `listStyleType` and counts child items.
    /// Layout resolves both values at the list level.
    /// Each item resolves its own `listStyleType` against the list value.
    /// CSS inherits this property, and the item renders the marker.
    ///
    /// The list also defines the indent.
    /// This placement matches the Yomitan stylesheet.
    /// Nested levels add the indent without explicit depth counting.
    /// An item inherits the list indent ([`Block::indent`]).
    /// A nested list adds another [`LIST_INDENT_EM`] unit.
    /// This indent forms the marker gutter.
    /// Layout sends this value to [`Paragraphs::owe`].
    pub(super) fn list(&mut self, id: NodeId, ctx: Ctx, ordered: bool, pad_left: f32) {
        let doc = self.doc;
        // A list with explicit left padding replaces the default gutter.
        // An author `padding-left` declaration replaces `padding-inline-start`.
        // It also replaces `--list-padding1` in Yomitan.
        // The box already accounts for this padding in [`Paragraphs::wrap`].
        // Do not add an indent for this level.
        // Only Jitendex declares `padding-left: 0.25em` in the 97-archive test corpus.
        let level =
            if pad_left > 0.0 { 0.0 } else { LIST_INDENT_EM * ctx.inline.size };
        let ctx = Ctx {
            block: Block { indent: ctx.block.indent + level, ..ctx.block },
            ..ctx
        };
        // Only `li` elements have `display: list-item` and show markers.
        // If a list has no `li` children, walk it as a standard block.
        // This preserves the standard text rules of `children`.
        if !doc.children(id).any(|child| doc.node(child).kind == Kind::ListItem) {
            return self.children(id, ctx);
        }
        // The list declaration overrides the tag's initial value.
        // Each item resolves its property against this value.
        // The property inherits, and the item displays the marker.
        let styling = self.render.styling;
        let inherited = styled_marker(doc, id, initial_marker(ordered), styling);
        let mut n = 0usize;
        let mut prose = doc.prose(id);
        for (i, child) in doc.children(id).enumerate() {
            let at = ctx.at(i);
            if doc.node(child).kind != Kind::ListItem {
                self.node(child, at, prose);
                prose = prose || doc.inline_prose(child);
                continue;
            }
            n += 1;
            if let Some(label) = styled_marker(doc, child, inherited, styling).label(n) {
                // The marker inherits the item's style.
                // The CSS `::marker` element does not inherit from inner tags such as `b`.
                // The indent belongs to this list.
                // The marker hangs in this list's padding, not the item's.
                let style = self.styled(child, ctx.inline);
                self.owe(label, style, ctx.block.indent);
            }
            if self.stack_items {
                self.node(child, at, prose);
            } else {
                // In compact mode, item content appends to the current paragraph.
                // Standard glossary item separators apply.
                // The `li` starts no new line and draws no block box.
                // Its content is inline, so CSS draws no block box around it.
                self.pending_sep = true;
                let inline = self.styled(child, ctx.inline);
                let link = self.link_of(child, ctx.link);
                self.children(child, Ctx { inline, link, ..at });
            }
            // Clear pending markers for an empty item.
            // An item without text shows no marker.
            // It must not place two markers on the next item.
            self.pending_marker.clear();
        }
    }

    /// Adds `label` with `style` to the next text run and hangs it at `indent`.
    ///
    /// This function appends to pending markers.
    /// When an item contains only a nested list, both markers share one line.
    /// Each marker keeps its style and gutter indent.
    /// The markers hang one level apart, as in a browser.
    pub(super) fn owe(&mut self, label: String, style: Inline, indent: f32) {
        self.pending_marker.push(FlowMarker { text: label, style, indent });
    }
    /// Applies pending markers to the next text run for the active layout mode.
    ///
    /// Stacked items use a gutter.
    /// The marker hangs in the gutter, and every item text line keeps its indent.
    /// Compact items have no gutter or separate marker line.
    /// Layout writes their markers inline.
    /// [`Paragraphs::stack_items`] selects this mode.
    /// Both modes use the same marker text.
    pub(super) fn mark(&mut self) {
        if self.stack_items {
            self.hang();
        } else {
            self.mark_inline();
        }
    }
    /// Attaches pending markers to the paragraph outside the text flow.
    ///
    /// The text run stays unchanged.
    /// Layout places markers against the first line box of the wrapped text.
    /// See [`place_markers`].
    /// The line boxes stay unchanged.
    /// A platform measurement of the spans reproduces the same line geometry.
    pub(super) fn hang(&mut self) {
        self.cur.marker.append(&mut self.pending_marker);
    }

    /// Writes pending markers as first inline spans in the paragraph.
    ///
    /// This matches `list-style-position: inside`.
    /// Layout uses this mode when no gutter exists:
    /// 1. Compact lists.
    /// 2. List items that contain only a table.
    ///
    /// Each marker forms its own span.
    /// The marker stays outside open ruby slots and hyperlinks.
    /// If code merged it with ruby base text, the ruby text would center over `\u{2022} 猫`.
    /// If code merged it with a `b` tag, the marker would become bold.
    /// An `<a>` tag must not include `::marker`.
    pub(super) fn mark_inline(&mut self) {
        if self.pending_marker.is_empty() {
            return;
        }
        let owed = std::mem::take(&mut self.pending_marker);
        let slot = std::mem::replace(&mut self.open_ruby, NO_RUBY);
        for mark in owed {
            self.barrier = true;
            self.push(&mark.text, mark.style, NO_LINK, None);
        }
        self.barrier = true;
        self.open_ruby = slot;
    }
}

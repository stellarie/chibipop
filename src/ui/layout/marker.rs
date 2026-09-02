//! What a list draws beside each of its items, and where it hangs.
//!
//! **One reason to change:** CSS's list-marker rules as this build answers
//! them - `list-style-type`'s resolved value, the counter, and the gutter
//! the marker box sits in.
//!
//! The resolution and the placement are one topic because CSS makes them
//! one: `list-style-type` is *inherited* and the marker is drawn at the
//! element with `display: list-item`, so an item's own declaration wins
//! over its list's - and `list-style-position: outside` is what puts the
//! resolved marker in a gutter the list's own indent already paid for
//! rather than on the item's first line.

use crate::dict::gloss::{GlossDoc, Kind, NodeId, StyleKey};
use super::flow::{Ctx, Flow, NO_LINK};
use super::gloss::Paragraphs;
use super::measure::{MeasureError, MeasureRun, Measured, StyledSpan, TextMeasure};
use super::ruby::NO_RUBY;
use super::scene::MarkerBox;
use super::style::{quoted, Block, Inline};

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
pub(super) struct MarkerMetrics {
    pub(super) w: f32,
    pub(super) h: f32,
    /// How far below the run's top edge
    /// the marker's baseline sits.
    pub(super) baseline: f32,
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
/// enforces.
///
/// Measured at the paragraph's wrap
/// width and not at the gutter's: a
/// marker is one counter or one
/// authored string and is free to
/// overhang the gutter, exactly as a
/// browser lets `10.` overhang a
/// narrow one.
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
    // Only a paragraph that carries a
    // marker pays for this buffer,
    // which is why it is not one of
    // the walk's.
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
pub(super) const LIST_INDENT_EM: f32 = 1.4;

/// `list-style-type: disc`, CSS's
/// initial value: U+2022 BULLET.
pub(super) const DISC_MARKER: &str = "\u{2022}";

/// `circle`: U+25E6 WHITE BULLET.
pub(super) const CIRCLE_MARKER: &str = "\u{25E6}";

/// `square`: U+25AA BLACK SMALL
/// SQUARE.
pub(super) const SQUARE_MARKER: &str = "\u{25AA}";

/// What sits between a marker and the
/// item text after it.
///
/// The marker box's own trailing gap,
/// and it is inside the box wherever
/// that box goes. A stacked item hangs
/// its marker in the gutter
/// ([`MarkerBox`]) with the box's right
/// edge on the list's content edge, so
/// the gap is what holds the glyph off
/// the text; a compact item has no
/// gutter and writes the same marker
/// as its paragraph's first span, so
/// the gap is a character on the line.
/// One label serves both, which is why
/// [`Marker::label`] appends it and
/// neither placement has to. It is the
/// gap the row number already writes
/// after its `1.`, so a numbered row
/// and a numbered item read alike.
pub(super) const MARKER_GAP: &str = " ";

/// What a list draws beside each of
/// its items: `list-style-type`'s
/// resolved value.
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
///
/// The marker's *content*, and not
/// the box it is drawn in: CSS keeps
/// those two apart and so does this
/// module. [`MarkerBox`] is the box,
/// which is the scene's own data and
/// carries a rect; this is what goes
/// in it, and never leaves layout.
///
/// [`MarkerBox`]: super::scene::MarkerBox
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Marker<'a> {
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
    pub(super) fn label(self, n: usize) -> Option<String> {
        match self {
            Marker::Glyph(glyph) => Some(format!("{glyph}{MARKER_GAP}")),
            // CSS's `decimal` counter
            // carries a `.` suffix, which
            // is also what the row number
            // writes.
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
/// will come from a dictionary's own
/// `styles.css`, where Jitendex writes
/// `ul[data-sc-content="sense-groups"]
/// { list-style-type: "＊" }`.
///
/// Independent of any layout mode by
/// construction: it takes a document,
/// a node and a fallback, so a
/// compact list and this one resolve
/// the same marker (see
/// [`Paragraphs::stack_items`]).
///
/// [`apply_style`]: super::style::apply_style
pub(super) fn marker_of<'a>(doc: &'a GlossDoc, id: NodeId, fallback: Marker<'a>) -> Marker<'a> {
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
/// gate the "honour dictionary
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

/// A list tag's initial
/// `list-style-type`.
///
/// The one thing `ol` and `ul` differ
/// in, and the bottom of the
/// inheritance chain [`marker_of`]
/// walks.
pub(super) fn initial_marker(ordered: bool) -> Marker<'static> {
    if ordered {
        Marker::Decimal
    } else {
        Marker::Glyph(DISC_MARKER)
    }
}

/// One list marker, before it is
/// placed.
///
/// Held beside the paragraph rather
/// than in it, for the reason
/// [`MarkerBox`] gives: CSS's
/// `outside` marker box takes no room
/// on the line, so putting it in the
/// run would advance the pen past it
/// and put every continuation line of
/// a wrapped item under the bullet.
#[derive(Clone)]
pub(super) struct FlowMarker {
    /// The label, its
    /// [`MARKER_GAP`] included.
    pub(super) text: String,
    /// The item's own resolved style:
    /// CSS's `::marker` inherits from
    /// the element with `display:
    /// list-item`, not from the markup
    /// inside it.
    pub(super) style: Inline,
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
    pub(super) indent: f32,
}

/// The list half of the gloss walk: a `ul` or an `ol`, its items, and the
/// marker each item is owed.
impl Paragraphs<'_> {
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
    pub(super) fn list(&mut self, id: NodeId, ctx: Ctx, ordered: bool, pad_left: f32) {
        let doc = self.doc;
        // A list that declares its own
        // left padding replaces the
        // default gutter rather than
        // adding to it, exactly as an
        // author's `padding-left`
        // replaces the UA's
        // `padding-inline-start` and
        // Yomitan's own
        // `--list-padding1` rule. The
        // box already paid the declared
        // padding ([`Paragraphs::wrap`]),
        // so the level costs nothing
        // more here. Jitendex's glossary
        // list (`padding-left: 0.25em`)
        // is the one list in the
        // 97-archive corpus that
        // declares any.
        let level =
            if pad_left > 0.0 { 0.0 } else { LIST_INDENT_EM * ctx.inline.size };
        let ctx = Ctx {
            block: Block { indent: ctx.block.indent + level, ..ctx.block },
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
                self.node(child, at, prose);
            } else {
                // The list in compact mode:
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
    pub(super) fn owe(&mut self, label: String, style: Inline, indent: f32) {
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
    pub(super) fn mark(&mut self) {
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
    pub(super) fn hang(&mut self) {
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
    pub(super) fn mark_inline(&mut self) {
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
}

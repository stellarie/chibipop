//! This module reserves line space for inline box edges.
//! It places each box rect in that reserved space.
//!
//! **One reason to change:** Change this module when an inline box edge
//! changes its line.
//!
//! These operations make one layout decision in two places.
//! A reading ([`ruby`](super::ruby)) and an image ([`image`](super::image))
//! use the same decision.
//! The measurement seam accepts styled spans, but it does not accept boxes
//! (ARCHITECTURE.md#popup-and-measurement).
//! Therefore, spans must reserve line space for box edges.
//! The code gets each box rect from those spans.
//! This method keeps the painted background equal to the text advance.
//! Change both operations when one operation changes.
//!
//! An earlier pass supplied only the drawn rect and treated line space as
//! unavailable.
//! Later work showed that spans can reserve the space.
//! A reading uses a zero-advance filler for vertical space.
//! An image uses no-break spaces for horizontal space after one probe.
//! This module applies the image spacer method to four box edges.
//!
//! # Only the horizontal axis
//!
//! CSS lets vertical margin, border, and padding paint without a line-height
//! change.
//! [`place_pills`] makes each rect extend vertically across nearby lines.
//! The paragraph keeps the same stack as a paragraph without the inline box.
//! Only horizontal edges reserve line space.
//!
//! # Reservation costs
//!
//! The reservation has two costs.
//! Both costs add invisible space, but neither moves a glyph.
//!
//! 1. A `margin-right` at a paragraph end reserves its space.
//!    The run width is then larger than its ink width.
//!    A browser line box has the same result.
//!    DirectWrite removes whitespace at the end from line width, but
//!    cosmic-text keeps it.
//!    Thus, the platform bins differ by one margin when a paragraph ends
//!    in a pill.
//! 2. An inline box that contains a block draws nothing
//!    ([`Paragraphs::pill`]).
//!    The block starts a new paragraph and ends the paragraph that measured
//!    the box.
//!    The reserved space stays in the old paragraph and has no ink.
//!    Thus, the old paragraph has a small gap at its end.
//!    The pass removes the paragraph when it contains only whitespace.
//!
//! [`Paragraphs::pill`]: super::gloss::Paragraphs::pill

use super::flow::{Flow, InlineBox};
use super::measure::{
    LineBox, MeasureError, MeasureRun, Measured, StyledSpan, TextMeasure,
};
use super::pass::{span_cover, Cover};
use super::scene::{ElemBox, SceneRect};
use super::style::{shift_on, BoxStyle};

/// The pill spacer reserves space for an inline box.
/// It is U+00A0 NO-BREAK SPACE, which [`IMAGE_SPACER`] also uses.
///
/// This character has no ink.
/// Only the box background appears in the reserved space.
/// Its advance supplies the needed width.
/// UAX #14 also defines it as nonbreaking glue.
/// A line break cannot occur before or after it.
/// Therefore, a line wrap cannot separate a pill spacer from its padding.
/// A `margin-right` can separate a word from its pill.
/// A wrap cannot move the word to the next line and leave the margin on this
/// line.
/// A normal space can permit this result on a narrow panel.
/// The image spacer and the pill spacer prevent this result.
///
/// The glue also affects the next word by design.
/// A pill and the next word wrap as one unit.
/// An ordinary space before a pill can break.
/// UAX #14 makes `SP × GL` a break opportunity.
/// Therefore, a pill sequence can wrap between pills.
///
/// [`IMAGE_SPACER`]: super::image::IMAGE_SPACER
pub(super) const PILL_SPACER: &str = "\u{a0}";

/// This constant sets the no-break space count for each em of space.
/// It uses the value and rationale of [`IMAGE_SPACERS_PER_ASPECT`].
///
/// The paragraph build fixes the count because a no-break space is text.
/// Both bins measure an element's text again before paint.
/// The paragraph build does not fix the size.
/// Only the measurer knows the advance of one no-break space.
/// Therefore, [`measure_pills`] calculates the size.
/// The count must keep the calculated size below the box em.
/// Otherwise, the pill spacer sets the line height instead of the text.
///
/// A count of four meets this limit for every real font.
/// A no-break space is one-quarter to one-third of an em wide.
/// Thus, four no-break spaces reserve one em at a maximum size of one em.
/// If a font uses a narrower space, [`measure_pills`] limits the size.
/// The line height does not increase.
/// The reservation is then a few percent short.
/// This reduces the declared padding by a small amount.
/// The code gets the drawn rect from the reserved space, so the line does not
/// change.
///
/// [`IMAGE_SPACERS_PER_ASPECT`]: super::image::IMAGE_SPACERS_PER_ASPECT
pub(super) const PILL_SPACERS_PER_EM: f32 = 4.0;

/// This constant sets the maximum pill spacer count for one edge.
///
/// An author can declare any length, so the space-to-em ratio has no fixed
/// maximum.
/// A limit of 64 spans for each edge exceeds all pills in the Dictionary
/// census.
/// The widest Jitendex edge is `margin-right: 0.5em` and needs two spans.
/// Above the limit, the reservation is short.
/// This reduces the padding but does not affect the panel.
pub(super) const PILL_SPACER_MAX: usize = 64;

/// Returns the four spaces that inline box edges reserve on the line.
/// The paragraph uses this order: left margin, left border plus padding,
/// right padding plus border, and right margin.
///
/// Each side uses two spans because the margin stays outside the box.
/// The drawn rect extends from the left border to the right border.
/// Therefore, a cover query for this inner run cannot include the margins.
///
/// A negative margin reserves no space.
/// CSS permits negative margins, and [`box_len`] keeps the value.
/// A span cannot have a negative advance.
/// The measurement seam needs glyph placement, not run placement, to move
/// later text across the box.
/// Therefore, a negative margin keeps its paint position, but reserves no
/// space.
/// No Dictionary in the census uses a negative margin.
/// [`apply_box`] already clamps padding and border widths to CSS limits.
///
/// [`box_len`]: super::style::box_len
/// [`apply_box`]: super::style::apply_box
pub(super) fn rooms(style: BoxStyle) -> [f32; 4] {
    let (m, p, b) = (style.margin, style.padding, style.border_used());
    [m.left, b.left + p.left, p.right + b.right, m.right]
        .map(|room| if room > 0.0 && room.is_finite() { room } else { 0.0 })
}

/// Reports whether an inline box reserves horizontal space beyond its content.
///
/// This function and `BoxStyle::paints` decide whether an inline box exists.
/// Before this function, a margin without paint had no way to reserve space.
/// A Jitendex pill has both a fill and a `margin-right`.
/// A Dictionary can use margins alone to separate two inline labels.
/// Such a box paints no pixel, but the code must build it.
pub(super) fn reserves(style: BoxStyle) -> bool {
    rooms(style).iter().any(|&room| room > 0.0)
}

/// Returns the [`PILL_SPACER`] count that one edge uses.
///
/// The space-to-em ratio alone sets this count.
/// Therefore, the paragraph build can set one count for all fonts.
/// [`measure_pills`] calculates the pill spacer size for the installed face.
///
/// Any positive space gets at least one pill spacer.
/// A space narrower than one-quarter of an em gets one span that shrinks to fit.
/// Zero space or an infinite declaration gets no pill spacer.
/// For a NaN em, `max` returns the finite operand.
pub(super) fn spacers(room: f32, em: f32) -> usize {
    if room <= 0.0 || !room.is_finite() {
        return 0;
    }
    if em <= 0.0 {
        return 1;
    }
    let want = (room / em * PILL_SPACERS_PER_EM).ceil();
    (want.max(1.0) as usize).min(PILL_SPACER_MAX)
}

/// Returns each pill spacer span in a paragraph with its needed space.
///
/// This function finds spans from the box indices, `from` and `to`.
/// It does not store a marker on each span.
/// The four pill spacer positions follow the paragraph structure.
/// The box style identifies the positions that exist.
/// [`Paragraphs::pill`] writes the left margin immediately before `from`.
/// It writes the left border and padding at `from`.
/// It writes the right padding and border at `to - 1`.
/// It writes the right margin at `to`.
/// It writes a span only when [`rooms`] reports positive space there.
/// Therefore, a nested pill finds its own four positions without an extra
/// index.
///
/// [`Paragraphs::pill`]: super::gloss::Paragraphs::pill
fn spacer_spans(pill: &InlineBox) -> impl Iterator<Item = (usize, f32)> {
    let room = rooms(pill.style);
    let (from, to) = (pill.from as usize, pill.to as usize);
    let at = [from.checked_sub(1), Some(from), to.checked_sub(1), Some(to)];
    at.into_iter().zip(room).filter_map(|(at, room)| match at {
        Some(at) if room > 0.0 => Some((at, room)),
        _ => None,
    })
}

/// Sets the pill spacer sizes for all inline boxes in a paragraph.
///
/// The code calls this function before it measures the paragraph.
/// This function makes each horizontal margin, border, and padding reserve
/// line space.
/// The measurement seam accepts styled spans, but not boxes.
/// Therefore, box edges can reserve space only through spans that the measurer
/// measures.
/// The code cannot increase line boxes after measurement.
/// Both bins measure the element spans again for paint and get the original
/// lines.
/// The ruby filler and image spacer use the same method for the same reason.
///
/// The code needs the advance of one [`PILL_SPACER`] for each size unit.
/// Only a measurer can supply this ratio.
/// One probe per box supplies the ratio because all four pill spacers use one
/// style.
///
/// The equation is `n * u * s` for `n` spacers, unit advance `u`, and size `s`.
/// The declared edge space determines `s`.
/// The code limits `s` to the box em.
/// This limit keeps the line height at or below the text height when a margin
/// is wide.
/// Above this limit, the reservation is a few percent narrow
/// ([`PILL_SPACERS_PER_EM`]).
/// [`place_pills`] uses the reserved space, not the declared edge space, for
/// the rect.
pub(super) fn measure_pills(
    m: &mut dyn TextMeasure,
    flow: &Flow,
    max_w: f32,
    run: &mut [StyledSpan<'_>],
) -> Result<(), MeasureError> {
    if flow.inline.is_empty() {
        return Ok(());
    }
    // Allocate this buffer only for a paragraph with an inline box.
    // The paragraph walk does not store it.
    let mut scratch = Measured::default();
    for pill in &flow.inline {
        // Probe once for each box, not for each edge.
        // All four pill spacers use the style that the node resolves.
        // The advance of a no-break space changes in direct proportion to its
        // size.
        let mut unit: Option<f32> = None;
        for (at, room) in spacer_spans(pill) {
            let (Some(span), Some(&asked)) = (flow.spans.get(at), run.get(at)) else {
                continue;
            };
            // The paragraph walk resolves this size.
            // A pill spacer uses the em of its box.
            let em = span.style.size;
            let per_size = match unit {
                Some(per_size) => per_size,
                None => {
                    let probe = StyledSpan { text: PILL_SPACER, size: em, ..asked };
                    m.measure(MeasureRun { spans: &[probe], max_w }, &mut scratch)?;
                    // Read the span box instead of `metrics.w`.
                    // DirectWrite excludes whitespace at the end from the
                    // total width.
                    // One no-break space contains only this whitespace.
                    let px = scratch.spans.first().map_or(0.0, |b| b.w);
                    *unit.insert(if em > 0.0 { px / em } else { 0.0 })
                }
            };
            // A measurer can report zero advance for a no-break space.
            // Keep the original em size in that case.
            // Do not divide by zero.
            if per_size <= 0.0 {
                continue;
            }
            // The paragraph count, not a new result from `spacers`, determines
            // the size.
            // Both bins measure the paragraph text again, so this count must
            // stay exact.
            let n = (span.len as usize / PILL_SPACER.len()).max(1) as f32;
            run[at].size = (room / (n * per_size)).min(em);
        }
    }
    Ok(())
}

/// Returns the paragraph inline boxes as rects.
///
/// This pure function reads the space that [`measure_pills`] reserves.
/// It gets each box rect from the final position of the box run.
///
/// **The rect equals the run.**
/// Horizontally, the rect covers spans from the left border through the right
/// border.
/// It has no outset because these spans are the border and padding.
/// Therefore, the background cannot extend past the reserved advance.
/// This remains true for any no-break space advance that the font face supplies.
/// A wrapped box paints left padding on its first fragment and right padding
/// on its last fragment.
/// CSS defines this result for a broken inline box.
///
/// Vertically, CSS defines the rect as an outset.
/// Vertical padding and borders paint, but do not change line height.
/// The rect extends across nearby lines, but does not move them.
///
/// The code creates one rect for each line that the run touches.
/// Therefore, a pill that wraps across two lines paints on both lines.
/// Only a box with ink enters the scene.
/// `inline_boxes` contains only decoration.
/// A box that only reserves space already has its full effect in the spans.
pub(super) fn place_pills(
    flow: &Flow,
    measured: &Measured,
    cover: &mut Vec<Cover>,
    pen: (f32, f32),
    line_at: impl Fn(LineBox) -> f32,
) -> Vec<ElemBox> {
    let mut out = Vec::new();
    for pill in &flow.inline {
        if !pill.style.paints() {
            continue;
        }
        span_cover(measured, cover, |b| b >= pill.from && b < pill.to);
        let (p, b) = (pill.style.padding, pill.style.border_used());
        for &(line, left, right, span_h) in cover.iter() {
            let line = measured.lines[line as usize];
            let shift = flow
                .spans
                .get(pill.from as usize)
                .map_or(0.0, |s| shift_on(s.style, line, span_h));
            // Get the text box for the run on this line.
            // The line-height and baseline metrics locate the run top.
            let ascent = if line.h > 0.0 {
                span_h * line.baseline / line.h
            } else {
                span_h
            };
            let top = line.y + line.baseline - shift - ascent;
            out.push(ElemBox {
                rect: SceneRect {
                    x: line_at(line) + left,
                    y: pen.1 + top - p.top - b.top,
                    w: right - left,
                    h: span_h + p.vertical() + b.vertical(),
                },
                style: pill.style,
            });
        }
    }
    out
}

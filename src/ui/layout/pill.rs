//! One inline box: how its own edges take room on a line, and where the
//! rect drawn around them lands.
//!
//! **One reason to change:** what an inline box's margin, border and
//! padding do to the line they sit on.
//!
//! Both halves are here because they are one decision made twice, exactly
//! as a reading's are ([`ruby`](super::ruby)) and an image's are
//! ([`image`](super::image)). The room comes from the same trick and for
//! the same reason: the measurement seam takes styled spans and no boxes
//! (ADR-0013), so an inline box's own edges can only occupy a line by
//! *being* spans the measurer charges for. The rect is then read back off
//! the very spans that bought the room, so the background a bin paints and
//! the advance the text took cannot disagree. Change either and the other
//! is wrong.
//!
//! Ticket 08 shipped the drawn rect alone and recorded the room as
//! impossible; two tickets later it was not. Ticket 11 buys a reading
//! *vertical* room with a zero-advance filler and ticket 12 buys an image
//! *horizontal* room with a run of no-break spaces solved from one probe.
//! This is ticket 12's mechanism over a box's four edges instead of one
//! replaced element's width.
//!
//! # The horizontal axis only, and that is CSS
//!
//! An inline box's vertical margin, border and padding paint but do not
//! affect line height, which is what this renderer already did and still
//! does: the rect [`place_pills`] draws is outset vertically over its
//! neighbours' lines and the paragraph stacks as though the box were not
//! there. Only the horizontal axis reserves, because only the horizontal
//! axis is what CSS says an inline box takes from the line it sits on.
//!
//! # What the reservation costs
//!
//! Two things, both of them invisible and neither of them a glyph out of
//! place.
//!
//! A box whose `margin-right` lands at the end of a paragraph reserves it
//! anyway, so the run reports that much more width than its ink. A
//! browser's line box does the same; DirectWrite drops trailing
//! whitespace from a line's own width and cosmic-text keeps it, so the
//! two bins differ by one margin on a paragraph that ends in a pill.
//!
//! And an inline box holding a *block* draws nothing at all
//! ([`Paragraphs::pill`]): the block opens a paragraph, which sends the
//! one the box was being measured against out from under it, and the room
//! already written goes with it. It is whitespace with no ink, so the
//! paragraph it leaves in either draws it as a few pixels of trailing gap
//! or - being whitespace-only - is dropped whole.
//!
//! [`Paragraphs::pill`]: super::gloss::Paragraphs::pill

use super::flow::{Flow, InlineBox};
use super::measure::{
    LineBox, MeasureError, MeasureRun, Measured, StyledSpan, TextMeasure,
};
use super::pass::{span_cover, Cover};
use super::scene::{ElemBox, SceneRect};
use super::style::{shift_on, BoxStyle};

/// What an inline box's room is bought
/// with: U+00A0 NO-BREAK SPACE, and it
/// is [`IMAGE_SPACER`]'s character for
/// [`IMAGE_SPACER`]'s reasons.
///
/// It carries no ink, so a box's own
/// background is all that shows in the
/// room it reserves. It has an
/// advance, which is the whole point.
/// And it is *non-breaking glue* in
/// UAX #14 - a break is forbidden both
/// before and after it - so no wrap
/// can split a pill from its own
/// padding, and none can put the word
/// a `margin-right` separates from the
/// pill on the next line while the gap
/// stays behind on this one. A
/// breakable space would have done the
/// second thing on any panel narrow
/// enough, which is the failure the
/// image spacer already refuses.
///
/// The glue cuts the other way too,
/// and deliberately: a pill and the
/// one word after it wrap as a unit.
/// The space *before* a pill is
/// ordinarily a real space, and
/// `SP × GL` is a break opportunity,
/// so a run of pills still wraps
/// between them.
///
/// [`IMAGE_SPACER`]: super::image::IMAGE_SPACER
pub(super) const PILL_SPACER: &str = "\u{a0}";

/// No-break spaces per em of room, and
/// it is [`IMAGE_SPACERS_PER_ASPECT`]'s
/// number for that constant's reason.
///
/// The count is fixed while the
/// paragraph is built, because it is
/// *text* and both bins re-measure an
/// element's own text to paint it; the
/// *size* is solved once the measurer
/// has been asked, because only it
/// knows what one of these advances
/// ([`measure_pills`]). The count has
/// to be generous enough that the
/// solved size stays under the em the
/// box sits in - otherwise the spacer,
/// and not the text, would decide its
/// line's height.
///
/// Four is that bound for every real
/// face: a no-break space is a space,
/// and a space is between a quarter and
/// a third of an em, so four of them
/// reserve one em at no more than one
/// em of size. Where a face does go
/// narrower, [`measure_pills`] clamps
/// the size instead of letting the line
/// grow, and the reservation comes out
/// a few percent short - which costs
/// the box a sliver of its declared
/// padding and costs the line nothing,
/// because the rect is read back off
/// the room that was actually bought.
///
/// [`IMAGE_SPACERS_PER_ASPECT`]: super::image::IMAGE_SPACERS_PER_ASPECT
pub(super) const PILL_SPACERS_PER_EM: f32 = 4.0;

/// The most no-break spaces one edge
/// may reserve with.
///
/// A declared length is arbitrary
/// author input, so the ratio of room
/// to em is too, and 64 spans per edge
/// is already far past any pill a
/// dictionary draws - Jitendex's own
/// widest edge is `margin-right: 0.5em`,
/// which is two. Beyond the cap the
/// reservation is short, which costs the
/// box some of its padding and costs the
/// panel nothing.
pub(super) const PILL_SPACER_MAX: usize = 64;

/// The four rooms an inline box's own
/// edges owe its line, in the order the
/// paragraph writes them: the left
/// margin, the left border plus
/// padding, the right padding plus
/// border, the right margin.
///
/// Two spans per side rather than one,
/// because the margin is the one edge
/// that is *outside* the box: the rect
/// drawn is the run from the left
/// border to the right one, so the
/// margins have to sit where a cover of
/// that run cannot reach them.
///
/// A negative margin buys nothing. CSS
/// allows one and [`box_len`] keeps it,
/// but a span cannot advance backwards
/// and pulling the following text over
/// the box would need the seam to place
/// glyphs rather than runs (ADR-0013) -
/// so a negative margin still draws
/// where it always did and reserves
/// what it always reserved, nothing.
/// No census dictionary writes one.
/// Padding and border widths are
/// already non-negative: [`apply_box`]
/// clamps exactly what CSS clamps.
///
/// [`box_len`]: super::style::box_len
/// [`apply_box`]: super::style::apply_box
pub(super) fn rooms(style: BoxStyle) -> [f32; 4] {
    let (m, p, b) = (style.margin, style.padding, style.border_used());
    [m.left, b.left + p.left, p.right + b.right, m.right]
        .map(|room| if room > 0.0 && room.is_finite() { room } else { 0.0 })
}

/// Does this box take horizontal room
/// its content does not?
///
/// The gate on an inline box existing
/// at all, beside `BoxStyle::paints`:
/// before this a margin with nothing to
/// draw resolved to nothing, because
/// nothing could have spent it.
/// Jitendex's pill is both - a fill and
/// a `margin-right` - but a dictionary
/// spacing two inline labels apart with
/// margins alone is a box that paints
/// no pixel and still has to be built.
pub(super) fn reserves(style: BoxStyle) -> bool {
    rooms(style).iter().any(|&room| room > 0.0)
}

/// How many [`PILL_SPACER`]s one edge
/// reserves with.
///
/// From the ratio of room to em alone,
/// so it is decided while the paragraph
/// is built and is the same number in
/// every font - the *size* of them is
/// what [`measure_pills`] solves
/// against the face that is actually
/// installed.
///
/// One is the floor for any room at
/// all: an edge narrower than a quarter
/// of an em still gets a span, sized
/// down to it. No room, or a room a
/// declaration overflowed to infinity,
/// gets none - and a NaN em falls
/// through to `max`, which returns the
/// finite operand.
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

/// Every span of a paragraph that buys
/// an inline box room, with the room it
/// owes.
///
/// Found from the box's own two indices
/// rather than a marker per span,
/// because the positions are structural
/// and the style says which of them
/// exist: [`Paragraphs::pill`] writes
/// the left margin immediately before
/// `from`, the left border and padding
/// *at* `from`, the right padding and
/// border at `to - 1` and the right
/// margin at `to`, and writes each one
/// exactly when [`rooms`] is positive
/// there. So a pill nested inside
/// another still finds its own four,
/// and no span has to carry a fourth
/// side-table index.
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

/// Every inline box of a paragraph,
/// given the room its edges need.
///
/// Called *before* the paragraph is
/// measured, and it is the whole of how
/// an inline box's horizontal margin,
/// border and padding occupy a line.
/// The measurement seam takes styled
/// spans and no boxes (ADR-0013), so a
/// box's edges can only take room by
/// *being* spans the measurer charges
/// for - and growing the line boxes
/// afterwards would fool nobody,
/// because both bins re-measure an
/// element's own spans to paint it and
/// would get the ungrown lines back.
/// Ticket 11's ruby filler and ticket
/// 12's image spacer are the same trick
/// for the same reason.
///
/// So the run asks, and this decides
/// what it asks for. One ratio is
/// needed and only a measurer knows it:
/// what one [`PILL_SPACER`] advances
/// per unit of size. One probe per box
/// answers it, since all four of a
/// box's spacers carry one style.
///
/// Then the arithmetic. `n` spacers at
/// size `s` advance `n * u * s`, so the
/// room the edge declared fixes `s`
/// exactly. It is capped at the em the
/// box sits in, which is what keeps a
/// wide margin from making its line
/// taller than its text: past that cap
/// the reservation comes out a few
/// percent narrow instead
/// ([`PILL_SPACERS_PER_EM`]), and
/// [`place_pills`] draws the rect the
/// room that was bought describes
/// rather than the one that was asked
/// for.
pub(super) fn measure_pills(
    m: &mut dyn TextMeasure,
    flow: &Flow,
    max_w: f32,
    run: &mut [StyledSpan<'_>],
) -> Result<(), MeasureError> {
    if flow.inline.is_empty() {
        return Ok(());
    }
    // Only a paragraph holding an
    // inline box pays for this buffer,
    // which is why it is not one of the
    // walk's.
    let mut scratch = Measured::default();
    for pill in &flow.inline {
        // One probe per box, not per
        // edge: all four of a box's
        // spacers carry the one style
        // its node resolved, and a
        // no-break space's advance is
        // proportional to its size.
        let mut unit: Option<f32> = None;
        for (at, room) in spacer_spans(pill) {
            let (Some(span), Some(&asked)) = (flow.spans.get(at), run.get(at)) else {
                continue;
            };
            // The size the walk resolved,
            // which for a spacer is the em
            // its box sits in.
            let em = span.style.size;
            let per_size = match unit {
                Some(per_size) => per_size,
                None => {
                    let probe = StyledSpan { text: PILL_SPACER, size: em, ..asked };
                    m.measure(MeasureRun { spans: &[probe], max_w }, &mut scratch)?;
                    // The span's own box, not
                    // `metrics.w`: DirectWrite's
                    // aggregate width excludes
                    // trailing whitespace, and a
                    // lone no-break space is
                    // nothing but that.
                    let px = scratch.spans.first().map_or(0.0, |b| b.w);
                    *unit.insert(if em > 0.0 { px / em } else { 0.0 })
                }
            };
            // A measurer that charges
            // nothing for a no-break space
            // can reserve nothing exactly,
            // so the spacer keeps the em it
            // was built with rather than
            // dividing by zero.
            if per_size <= 0.0 {
                continue;
            }
            // However many the walk wrote,
            // and not what `spacers` would
            // answer again: the text is
            // what both bins re-measure,
            // so the count in it is the
            // one the size has to solve
            // against.
            let n = (span.len as usize / PILL_SPACER.len()).max(1) as f32;
            run[at].size = (room / (n * per_size)).min(em);
        }
    }
    Ok(())
}

/// The paragraph's inline boxes, as
/// rects.
///
/// Pure: the lines already carry the
/// room [`measure_pills`] bought, so
/// this only reads back where each box's
/// own run landed.
///
/// **The rect is the run.** Horizontally
/// it is exactly the cover of the spans
/// from the box's left border to its
/// right one - no outset - because those
/// spans *are* its border and padding.
/// So the drawn background cannot reach
/// past the advance that was reserved
/// for it, whatever the face charged for
/// a no-break space, and a box that
/// wrapped draws its left padding on the
/// first fragment and its right padding
/// on the last, which is what CSS says a
/// broken inline box does.
///
/// Vertically it *is* an outset, and
/// that is CSS too: an inline box's
/// vertical padding and border paint but
/// do not affect line height, so the
/// rect grows over its neighbours'
/// lines rather than pushing them apart.
///
/// One rect per line the run touches, so
/// a pill broken across two lines is
/// drawn on both. Only a box with ink
/// reaches the scene: `inline_boxes` is
/// decoration, and a box that spaces its
/// content out has already spent
/// everything it had on the spans above.
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
            // The run's own text box on
            // this line, from the two
            // facts the seam reports about
            // a line: how tall it is and
            // how far down it the baseline
            // sits.
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

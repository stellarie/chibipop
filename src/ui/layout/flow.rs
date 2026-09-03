//! This module stores one paragraph of inline content while the block walk builds it.
//!
//! **One reason to change:** Change this module when the paragraph must carry more data.
//! The block walk passes that data to the measure pass.
//!
//! [`Flow`] contains one `String` and a flat vector of styled byte ranges.
//! It also contains one side table for each item that does not flow.
//!
//! These side tables keep the seam clear.
//! The measurer accepts styled spans but no boxes.
//! A reading, marker, or image has its own position, so it cannot be a paragraph span.
//!
//! Each related module places its items:
//! [`ruby`](super::ruby), [`marker`](super::marker), and [`image`](super::image).

use crate::controller::HitAction;
use crate::dict::gloss::NodePath;
use crate::ui::theme::Theme;
use super::image::{FlowImage, NO_IMAGE};
use super::marker::FlowMarker;
use super::measure::StyledSpan;
use super::ruby::{FlowRuby, NO_RUBY};
use super::scene::TextSource;
use super::style::{Block, BoxStyle, Inline};

/// An index in [`Flow::links`], or `NO_LINK` when no link exists.
pub(super) const NO_LINK: u32 = u32::MAX;

/// Values that a node inherits from its parent.
///
/// The block walk passes four values when it enters a child.
/// This struct gives the recursive call one named value for each slot.
/// `Copy` lets each child replace one field in its parent's context.
#[derive(Debug, Clone, Copy)]
pub(super) struct Ctx {
    pub(super) inline: Inline,
    pub(super) block: Block,
    /// The parent `<a>`, or [`NO_LINK`].
    pub(super) link: u32,
    /// The node address.
    ///
    /// `None` means that a path goes beyond 16 levels or reaches the 65 536th sibling.
    /// [`NodePath`] then addresses no node instead of an ancestor.
    pub(super) path: Option<NodePath>,
}

impl Ctx {
    /// Returns the context for child `i` of the addressed node.
    ///
    /// Each level adds one step to the path.
    /// Each scene element therefore stores a path with one `u16` write.
    pub(super) fn at(self, i: usize) -> Ctx {
        Ctx { path: self.path.and_then(|p| p.child(i)), ..self }
    }
}

/// One span in a [`Flow`].
pub(super) struct FlowSpan {
    pub(super) at: u32,
    pub(super) len: u32,
    pub(super) style: Inline,
    /// The parent `<a>`.
    pub(super) link: u32,
    /// Index of this ruby base's reading, or [`NO_RUBY`].
    ///
    /// This value also keeps the base separate from adjacent text.
    /// For example, `漢` in `常用漢字` must stay separate so its reading can sit above it.
    /// [`Paragraphs::push`] joins two runs only when each value matches.
    ///
    /// [`Paragraphs::push`]: super::gloss::Paragraphs::push
    pub(super) ruby: u32,
    /// Does this span reserve line height instead of text space?
    ///
    /// A [`RUBY_FILLER`] is the tallest span on its line.
    /// The measurer then reserves the reading slot.
    /// See [`measure_readings`].
    /// The filler draws no ink, advances no text, and is never a base.
    ///
    /// An [`IMAGE_RISER`] has the same purpose.
    /// It has zero advance and no ink.
    /// It becomes the tallest span on its line.
    ///
    /// [`RUBY_FILLER`]: super::ruby::RUBY_FILLER
    /// [`measure_readings`]: super::ruby::measure_readings
    /// [`IMAGE_RISER`]: super::image::IMAGE_RISER
    pub(super) filler: bool,
    /// The image for which this span reserves room, or [`NO_IMAGE`].
    ///
    /// Both image span types set this value.
    /// [`IMAGE_SPACER`] reserves the width.
    /// [`place_images`] gets the box from this span.
    /// Each [`IMAGE_RISER`] reserves the height and sets `filler`.
    ///
    /// [`IMAGE_SPACER`]: super::image::IMAGE_SPACER
    /// [`place_images`]: super::image::place_images
    /// [`IMAGE_RISER`]: super::image::IMAGE_RISER
    pub(super) image: u32,
}

/// One inline box around a range of paragraph spans.
///
/// A pill is a `span` with padding, background, border, or margin.
/// The pill keeps its position on each line and does not create a line break.
/// Jitendex uses pills for part-of-speech labels.
/// Between 11 and 14 census Dictionaries use the same form.
///
/// This type stores a span range instead of a rect because the measurer sets the run position.
/// The seam reports one box for each span on each line.
/// One range therefore becomes one [`ElemBox`] on each line that it touches.
///
/// **The range is the border box.**
/// The `from` span reserves the left border and padding.
/// The `to - 1` span reserves the right border and padding.
/// See [`measure_pills`].
/// The two margins stay outside this range because margins stay outside the box.
/// [`place_pills`] uses the range to find the rect.
/// The measure pass uses the box edges to reserve the room.
///
/// [`ElemBox`]: super::scene::ElemBox
/// [`measure_pills`]: super::pill::measure_pills
/// [`place_pills`]: super::pill::place_pills
pub(super) struct InlineBox {
    /// The first span that the box surrounds.
    pub(super) from: u32,
    /// One span past the right border edge.
    pub(super) to: u32,
    pub(super) style: BoxStyle,
}

/// One paragraph of inline content.
///
/// The measure pass treats this structure as one unit.
/// It sends one [`MeasureRun`] and receives one [`SceneElem`].
/// The text stays in one string, so the seam receives the complete paragraph.
/// A span boundary does not set a line boundary.
/// Each span is a byte range in the string.
///
/// [`MeasureRun`]: super::measure::MeasureRun
/// [`SceneElem`]: super::scene::SceneElem
#[derive(Default)]
pub(super) struct Flow {
    /// The gap above this paragraph.
    pub(super) top_gap: f32,
    pub(super) text: String,
    pub(super) spans: Vec<FlowSpan>,
    /// This table records the Dictionary leaf bytes for each text range.
    ///
    /// The layout pass keeps this table separate from spans because one styled span
    /// can contain text from several leaves.
    pub(super) sources: Vec<TextSource>,
    /// One entry for each `<a>` with a hit target.
    /// [`FlowSpan::link`] stores its index.
    pub(super) links: Vec<HitAction>,
    /// One entry for each ruby base. [`FlowSpan::ruby`] stores its index.
    pub(super) ruby: Vec<FlowRuby>,
    /// One entry for each list marker on the first line.
    /// The outermost list appears first.
    pub(super) marker: Vec<FlowMarker>,
    /// One [`FlowImage`] for each image node in this paragraph.
    /// [`FlowSpan::image`] stores its index.
    pub(super) images: Vec<FlowImage>,
    /// The block for this paragraph.
    /// It holds the box, alignment, and newline rule.
    pub(super) block: Block,
    /// The node that this paragraph renders, or `None` when it has no address.
    ///
    /// The block that opened this paragraph supplies the subtree for a hit.
    /// `render_html(Selection::Nodes)` then reproduces that sense.
    pub(super) path: Option<NodePath>,
    /// Boxes that the paragraph draws around its runs.
    pub(super) inline: Vec<InlineBox>,
    /// The term-bank row that this paragraph renders.
    ///
    /// [`build_elements`] stamps these IDs because the tree does not know its row.
    /// These IDs and `path` form all of [`GlossOrigin`].
    ///
    /// [`build_elements`]: super::chrome::build_elements
    /// [`GlossOrigin`]: super::scene::GlossOrigin
    pub(super) dict_id: i64,
    pub(super) entry_id: i64,
    /// This field stores the flattened Entry ordinal for the current Card.
    pub(super) entry: u32,

}
impl Flow {
    /// Returns the spans in the form that the seam takes.
    ///
    /// The iterator slices each span from the single text string.
    /// It gives every span the same `font` and its own style values.
    pub(super) fn styled_spans<'a>(
        &'a self,
        font: &'a str,
    ) -> impl Iterator<Item = StyledSpan<'a>> {
        self.spans.iter().map(move |s| StyledSpan {
            text: &self.text[s.at as usize..(s.at + s.len) as usize],
            font,
            size: s.style.size,
            weight: s.style.weight,
            italic: s.style.italic,
            color: s.style.color,
        })
    }

    /// The element's base style.
    ///
    /// For a gloss with one plain string, the first span has the body role and supplies this style.
    /// An empty flow uses `Inline::body(theme)`.
    pub(super) fn base(&self, theme: &Theme) -> Inline {
        self.spans.first().map_or_else(|| Inline::body(theme), |s| s.style)
    }

    /// Prefixes this paragraph with a matched row's number.
    ///
    /// Yomitan gives one number to each matched term-bank row.
    /// The number belongs to the row's first paragraph, not to each sibling block inside it.
    /// The method uses the body's style for the number.
    /// It joins the number to the next span when its style, link, ruby, and image values match.
    /// Otherwise, it creates a separate span.
    pub(super) fn number(&mut self, n: usize, style: Inline) {
        let label = format!("{n}. ");
        let shift = label.len() as u32;
        self.text.insert_str(0, &label);
        for span in &mut self.spans {
            span.at += shift;
        }
        for source in &mut self.sources {
            source.at += shift;
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

/// Removes edge whitespace and empty spans from a paragraph.
///
/// The function rebuilds the text instead of slicing it in place.
/// Each span stores a byte range in the text, so every later span moves after a cut.
/// The function removes spans that become empty.
/// This keeps a separator node between two blocks from having the width of one space.
///
/// Removing a span moves every later index.
/// Each [`InlineBox`] names its run by index, so the function renumbers the boxes.
/// The measure pass uses these indices to find the room that an inline box reserves.
/// Without the renumber, leading whitespace before a pill could place the pill incorrectly.
/// A stale pair could also reserve room for another span.
pub(super) fn trim(flow: &mut Flow) {
    // `white-space: pre-line` keeps a segment break and collapses all other whitespace.
    // A paragraph that declares it keeps the newline at its edge.
    // It removes only the spaces around it.
    let edge: &[char] =
        if flow.block.pre_line { &[' ', '\t'] } else { &[' ', '\t', '\n', '\r'] };
    let start = flow.text.len() - flow.text.trim_start_matches(edge).len();
    let end = flow.text.trim_end_matches(edge).len();
    if start == 0 && end == flow.text.len() {
        return;
    }
    let (start, end) = (start as u32, end as u32);
    flow.text = flow.text[start as usize..end as usize].to_string();
    // Each old span boundary gets one new index.
    // The index counts old spans before the boundary that remain.
    // The ranges above therefore stay half-open.
    // `to` can name one past the last span, so add a boundary at the end.
    let mut moved = Vec::with_capacity(flow.spans.len() + 1);
    let mut kept = 0u32;
    for span in &flow.spans {
        moved.push(kept);
        let at = span.at.max(start);
        let to = (span.at + span.len).min(end);
        kept += u32::from(to > at);
    }
    moved.push(kept);
    for pill in &mut flow.inline {
        pill.from = moved[pill.from as usize];
        pill.to = moved[pill.to as usize];
    }
    flow.spans.retain_mut(|span| {
        let at = span.at.max(start);
        let to = (span.at + span.len).min(end);
        span.at = at.saturating_sub(start);
        span.len = to.saturating_sub(at);
        span.len > 0
    });
    flow.sources.retain_mut(|source| {
        let old_at = source.at;
        let at = old_at.max(start);
        let to = (old_at + source.len).min(end);
        if !source.atomic {
            source.byte += at.saturating_sub(old_at);
        }
        source.at = at.saturating_sub(start);
        source.len = to.saturating_sub(at);
        source.len > 0
    });
}

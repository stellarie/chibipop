//! One paragraph of inline content, as the block walk accumulates it.
//!
//! **One reason to change:** what a paragraph has to carry from the walk
//! that builds it to the pass that measures it. That is the whole of this
//! module: [`Flow`] is a `String`, a flat vector of styled byte ranges over
//! it, and one side table per thing that does *not* flow.
//!
//! Why the side tables exist at all is ADR-0013's seam: the measurer takes
//! styled spans and no boxes, so anything with a position of its own - a
//! reading, a marker, an image - cannot be a span of the paragraph. Each
//! rides beside it and is placed by its own module
//! ([`ruby`](super::ruby), [`marker`](super::marker),
//! [`image`](super::image)).

use crate::controller::HitAction;
use crate::dict::gloss::NodePath;
use crate::ui::theme::Theme;
use super::image::{FlowImage, NO_IMAGE};
use super::marker::FlowMarker;
use super::measure::StyledSpan;
use super::ruby::{FlowRuby, NO_RUBY};
use super::style::{Block, BoxStyle, Inline};

/// A link's index in [`Flow::links`],
/// or no link at all.
pub(super) const NO_LINK: u32 = u32::MAX;

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
pub(super) struct Ctx {
    pub(super) inline: Inline,
    pub(super) block: Block,
    /// The `<a>` it sits inside, or
    /// [`NO_LINK`].
    pub(super) link: u32,
    /// This node's own address, or
    /// `None` once a step no longer
    /// fits - past 16 levels or the
    /// 65 536th sibling, where a
    /// [`NodePath`] addresses nothing
    /// rather than aliasing an
    /// ancestor.
    pub(super) path: Option<NodePath>,
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
    pub(super) fn at(self, i: usize) -> Ctx {
        Ctx { path: self.path.and_then(|p| p.child(i)), ..self }
    }
}

/// One span of a [`Flow`].
pub(super) struct FlowSpan {
    pub(super) at: u32,
    pub(super) len: u32,
    pub(super) style: Inline,
    /// The `<a>` it sits inside.
    pub(super) link: u32,
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
    ///
    /// [`Paragraphs::push`]: super::gloss::Paragraphs::push
    pub(super) ruby: u32,
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
    ///
    /// [`RUBY_FILLER`]: super::ruby::RUBY_FILLER
    /// [`measure_readings`]: super::ruby::measure_readings
    /// [`IMAGE_RISER`]: super::image::IMAGE_RISER
    pub(super) filler: bool,
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
    ///
    /// [`IMAGE_SPACER`]: super::image::IMAGE_SPACER
    /// [`place_images`]: super::image::place_images
    /// [`IMAGE_RISER`]: super::image::IMAGE_RISER
    pub(super) image: u32,
}

/// One inline box, over a run of the
/// paragraph's spans.
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
///
/// [`ElemBox`]: super::scene::ElemBox
pub(super) struct InlineBox {
    /// First span it covers.
    pub(super) from: u32,
    /// One past the last.
    pub(super) to: u32,
    pub(super) style: BoxStyle,
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
///
/// [`MeasureRun`]: super::measure::MeasureRun
/// [`SceneElem`]: super::scene::SceneElem
#[derive(Default)]
pub(super) struct Flow {
    /// Gap owed above it.
    pub(super) top_gap: f32,
    pub(super) text: String,
    pub(super) spans: Vec<FlowSpan>,
    /// One per `<a>` that earned a hit
    /// target, indexed by
    /// [`FlowSpan::link`].
    pub(super) links: Vec<HitAction>,
    /// One per ruby base, indexed by
    /// [`FlowSpan::ruby`].
    pub(super) ruby: Vec<FlowRuby>,
    /// One per list marker owed to
    /// this paragraph's first line,
    /// outermost list first.
    pub(super) marker: Vec<FlowMarker>,
    /// One per image node this
    /// paragraph reached, indexed by
    /// [`FlowSpan::image`].
    pub(super) images: Vec<FlowImage>,
    /// The block this paragraph is:
    /// its box, its alignment, and
    /// whether it preserves newlines.
    pub(super) block: Block,
    /// The node the paragraph renders,
    /// or `None` when it has no
    /// address.
    ///
    /// The block that opened it, so
    /// that a hit resolves to a
    /// subtree and
    /// `render_html(Selection::Nodes)`
    /// reproduces exactly that sense.
    pub(super) path: Option<NodePath>,
    /// Boxes to draw around runs
    /// inside it.
    pub(super) inline: Vec<InlineBox>,
    /// The term-bank row this
    /// paragraph renders.
    ///
    /// Stamped by [`build_elements`],
    /// which is where the ids are in
    /// hand: the tree itself does not
    /// know which row stored it.
    /// Together with `path` this is the
    /// whole of [`GlossOrigin`].
    ///
    /// [`build_elements`]: super::chrome::build_elements
    /// [`GlossOrigin`]: super::scene::GlossOrigin
    pub(super) dict_id: i64,
    pub(super) entry_id: i64,
}

impl Flow {
    /// Its spans, as the seam takes
    /// them.
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

    /// The element's own style: its
    /// first span's, which for a gloss
    /// that is one plain string is the
    /// body role and nothing else.
    pub(super) fn base(&self, theme: &Theme) -> Inline {
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
    pub(super) fn number(&mut self, n: usize, style: Inline) {
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
pub(super) fn trim(flow: &mut Flow) {
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

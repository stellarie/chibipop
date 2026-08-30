//! One reading, over the base it annotates.
//!
//! **One reason to change:** how a reading buys the room above its base,
//! and where in that room it sits.
//!
//! Both halves are here because they are one decision made twice. A
//! reading is not in the paragraph's text, so the measurer would give its
//! line the height the base alone needs - and growing the line boxes
//! afterwards would fool nobody, because a bin re-measures the same spans
//! to paint them and would get the ungrown lines back. So the run itself
//! asks for the taller line, through a zero-advance filler span
//! ([`RUBY_FILLER`]), and the placement then reads back where the base
//! landed. Change either and the other is wrong.

use crate::dict::gloss::{NodeId, Tag};
use super::flow::{Ctx, Flow, FlowSpan, NO_LINK};
use super::gloss::{text_of, Paragraphs};
use super::image::NO_IMAGE;
use super::measure::{MeasureError, MeasureRun, Measured, SpanBox, StyledSpan, TextMeasure};
use super::scene::RubyBox;
use super::style::Inline;

/// One reading, measured.
///
/// What [`measure_readings`] learns
/// and [`place_ruby`] spends.
#[derive(Clone, Copy, Default)]
pub(super) struct RubyMetrics {
    pub(super) w: f32,
    pub(super) h: f32,
    /// The fraction of a line that
    /// sits above its baseline.
    ///
    /// A face metric, and the seam
    /// reports it rather than the
    /// face's tables: `baseline / h`
    /// on any line of any run in this
    /// font. Noto Sans CJK answers
    /// 0.81; the layout tests' fake
    /// answers 0.5. Nothing here may
    /// assume either.
    pub(super) ascent: f32,
}

/// Every reading of a paragraph,
/// measured, and its filler span
/// sized.
///
/// Called *before* the paragraph is
/// measured, and it is what buys the
/// reading its slot. A reading is not
/// in the paragraph's text, so the
/// measurer would give its line the
/// height the base alone needs - and
/// growing the line boxes afterwards
/// would fool nobody, because a bin
/// re-measures the same spans to
/// paint them and would get the
/// ungrown lines back.
///
/// So the run itself asks for the
/// taller line, through the one rule
/// both engines already answer to and
/// ADR-0013 already documents: a line
/// is as tall as its tallest span. A
/// [`RUBY_FILLER`] beside each base
/// carries no ink and no advance and
/// exists only to be that span. Both
/// the scene and the bin's re-measure
/// therefore see one set of line
/// boxes, and `metrics.h` counts the
/// readings without anything after
/// the fact touching it.
///
/// Its size is decided here rather
/// than while the paragraph is built,
/// because the growth a line hands to
/// the space *above* its baseline is
/// the face's ascent share
/// ([`RubyMetrics::ascent`]) and only a
/// measurer knows it.
pub(super) fn measure_readings(
    m: &mut dyn TextMeasure,
    font: &str,
    flow: &Flow,
    max_w: f32,
    run: &mut [StyledSpan<'_>],
) -> Result<Vec<RubyMetrics>, MeasureError> {
    if flow.ruby.is_empty() {
        return Ok(Vec::new());
    }
    // Only a paragraph that holds ruby
    // pays for this buffer, which is
    // why it is not one of the walk's.
    let mut scratch = Measured::default();
    let mut out = vec![RubyMetrics::default(); flow.ruby.len()];
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        if ruby.text.is_empty() {
            continue;
        }
        m.measure(
            MeasureRun { spans: &[ruby_span(font, ruby)], max_w },
            &mut scratch,
        )?;
        let first = scratch.lines.first().copied().unwrap_or_default();
        out[slot] = RubyMetrics {
            w: scratch.metrics.w,
            h: scratch.metrics.h,
            ascent: if first.h > 0.0 { first.baseline / first.h } else { 0.0 },
        };
    }
    // A line of height `h` gives
    // `ascent * h` to the space above
    // its baseline, and a base of its
    // own size already claims its own
    // ascent of that. So a line has to
    // grow by `reading / ascent` for
    // the reading to fit above the
    // base - and a line's height is
    // proportional to its tallest
    // span's size in both engines,
    // which turns the growth into a
    // size.
    for (span, asked) in flow.spans.iter().zip(run.iter_mut()) {
        let Some(read) = flow.ruby.get(span.ruby as usize).filter(|_| span.filler) else {
            continue;
        };
        let box_ = out[span.ruby as usize];
        let grown = if box_.ascent > 0.0 {
            read.style.size / box_.ascent
        } else {
            read.style.size
        };
        asked.size = span.style.size + grown;
    }
    Ok(out)
}

/// The paragraph's readings, placed.
///
/// Pure: the lines already carry the
/// slot [`measure_readings`] bought,
/// so this only decides where in it
/// each reading sits.
pub(super) fn place_ruby(
    flow: &Flow,
    read: &[RubyMetrics],
    measured: &Measured,
    wrap_w: f32,
    slack: f32,
) -> Vec<RubyBox> {
    let mut out = Vec::new();
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        let box_ = read[slot];
        if ruby.text.is_empty() || box_.h <= 0.0 {
            continue;
        }
        let base = |b: &&SpanBox| {
            flow.spans
                .get(b.span as usize)
                .is_some_and(|s| s.ruby == slot as u32 && !s.filler)
        };
        // The first line the base
        // landed on, and its extent
        // there: a base that wrapped
        // keeps its reading over its
        // head rather than over its
        // tail. A base is a character
        // or two, so it practically
        // never wraps.
        let Some(line) = measured.spans.iter().filter(base).map(|b| b.line).min() else {
            continue;
        };
        let Some(geom) = measured.lines.get(line as usize) else {
            continue;
        };
        let (left, right, tallest) = measured
            .spans
            .iter()
            .filter(base)
            .filter(|b| b.line == line)
            .fold((f32::MAX, 0.0f32, 0.0f32), |(l, r, h), b| {
                (l.min(b.x), r.max(b.x + b.w), h.max(b.h))
            });
        // Its bottom against the base's
        // own ink top, which is the
        // base's ascent up from the
        // line's baseline. Clamped into
        // the line, so a measurer that
        // gave the line no extra room
        // draws the reading small and
        // high rather than off the
        // paragraph.
        let floor = geom.baseline - box_.ascent * tallest;
        let y = geom.y + (floor - box_.h).max(0.0);
        // Centred over the base, and
        // never off the panel's left
        // edge: a reading wider than its
        // base overhangs it, which is
        // what a browser does too. The
        // line's own alignment slack
        // comes first, because the base
        // it sits over moved by it.
        let indent = (wrap_w - geom.w).max(0.0) * slack;
        let x = (indent + left + (right - left - box_.w) / 2.0).max(0.0);
        out.push(RubyBox {
            text: ruby.text.clone(),
            x,
            y,
            w: box_.w,
            h: box_.h,
            size: ruby.style.size,
            color: ruby.style.color,
            weight: ruby.style.weight,
            italic: ruby.style.italic,
        });
    }
    out
}

/// One reading, as the seam takes it.
pub(super) fn ruby_span<'a>(font: &'a str, ruby: &'a FlowRuby) -> StyledSpan<'a> {
    StyledSpan {
        text: &ruby.text,
        font,
        size: ruby.style.size,
        weight: ruby.style.weight,
        italic: ruby.style.italic,
        color: ruby.style.color,
    }
}

/// A reading's size, as a fraction of
/// the base it sits over.
///
/// Yomitan's own
/// `ext/css/structured-content.css` -
/// the spec's stated source of
/// defaults - declares no `ruby`,
/// `rt` or `rp` rule at all, so what
/// a reader sees in Yomitan is the
/// browser's own default, and both
/// engines Yomitan runs in give `rt`
/// `font-size: 50%`. This is
/// therefore Yomitan's drawn default
/// by exactly the route
/// [`tag_style`] already takes for
/// `b` and `sup`: HTML's own
/// stylesheet, and not a number
/// invented here.
///
/// Not [`FONT_STEP`]: `smaller` is a
/// 1.2 step and a reading is a half,
/// which is the point - furigana is
/// meant to read as an annotation
/// rather than as small text.
///
/// [`tag_style`]: super::style::tag_style
/// [`FONT_STEP`]: super::style::FONT_STEP
pub(super) const RUBY_RATIO: f32 = 0.5;

/// A base's index in [`Flow::ruby`],
/// or no reading at all.
pub(super) const NO_RUBY: u32 = u32::MAX;

/// What a ruby base's slot is bought
/// with: U+2060 WORD JOINER.
///
/// A reading has to have room above
/// its base that the line above must
/// not overlap, and the only thing
/// that grows a line is a taller span
/// on it (ADR-0013). This character
/// is that span. Two properties earn
/// it the job, and both were probed
/// against the real shaper rather
/// than assumed: it shapes to a glyph
/// of zero advance, so it costs the
/// line no width, and it is a *word
/// joiner*, so no wrap can separate
/// it from the base whose line it
/// grows.
///
/// Not U+200B ZERO WIDTH SPACE, which
/// measures the same and is a break
/// *opportunity*: it would let a line
/// break between a base and its own
/// filler.
///
/// It does mean an element's `text`
/// holds one invisible character per
/// reading. That is the honest
/// record: the text is what the bin
/// re-measures and shapes, and the
/// alternative - line boxes grown
/// after the wrap - is geometry the
/// paint would not reproduce.
pub(super) const RUBY_FILLER: &str = "\u{2060}";

/// One reading, before it is placed.
///
/// Held beside the paragraph rather
/// than in it: a reading takes no
/// horizontal room from the line it
/// sits on, so putting it in the run
/// would advance the pen past it and
/// let the wrap break it away from
/// the base it belongs to.
#[derive(Clone)]
pub(super) struct FlowRuby {
    pub(super) text: String,
    pub(super) style: Inline,
}

/// The ruby half of the gloss walk: a `ruby` subtree, its base's slot,
/// and the reading that fills it.
impl Paragraphs<'_> {
    /// Buys the open slot's reading its
    /// room, if the slot has a base to
    /// hang it over.
    ///
    /// A [`RUBY_FILLER`] span of its
    /// own, appended straight after the
    /// base: the character is a word
    /// joiner, so no wrap can put it on
    /// a different line from the base
    /// whose line it is there to grow.
    /// Its size is left as the base's
    /// and raised by
    /// [`measure_readings`], which is
    /// the first point at which the
    /// reading's height is known.
    ///
    /// Nothing is appended for a slot
    /// no base text reached: an empty
    /// ruby would otherwise buy a
    /// taller line for a reading with
    /// nothing to read.
    pub(super) fn push_filler(&mut self, style: Inline) {
        let slot = self.open_ruby;
        if !self.cur.spans.iter().any(|s| s.ruby == slot && !s.filler) {
            return;
        }
        let at = self.cur.text.len() as u32;
        self.cur.text.push_str(RUBY_FILLER);
        self.cur.spans.push(FlowSpan {
            at,
            len: RUBY_FILLER.len() as u32,
            style,
            link: NO_LINK,
            ruby: slot,
            filler: true,
            image: NO_IMAGE,
        });
        // The run after it is its own
        // span: joining it would give a
        // whole word the filler's size.
        self.barrier = true;
    }

    /// One `ruby` node: bases in the
    /// flow, readings above them.
    ///
    /// One slot per `rt` and not per
    /// `ruby`, because
    /// `<ruby>漢<rt>かん</rt>字<rt>じ
    /// </rt></ruby>` is two pairings in
    /// one node - which is how a
    /// dictionary writes per-character
    /// furigana. Each slot is opened
    /// before the base text that will
    /// wear it and closed by the `rt`
    /// that follows, so a base is
    /// stamped with its own reading and
    /// not with its neighbour's.
    pub(super) fn ruby(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let style = self.styled(id, ctx.inline);
        let link = self.link_of(id, ctx.link);
        let slot = self.open_slot(style);
        let outer = std::mem::replace(&mut self.open_ruby, slot);
        // `rp` holds the parentheses
        // HTML wrote for a renderer
        // that cannot draw ruby. This
        // one can, so they are held
        // back and spent only if no
        // reading ever arrives, which
        // is the whole of the fallback
        // this tag exists for.
        let mut fallback: Vec<(String, Inline)> = Vec::new();
        let mut read = false;
        let inner = Ctx { inline: style, link, ..ctx };
        for (i, child) in doc.children(id).enumerate() {
            match doc.node(child).tag {
                Tag::Rt => {
                    if self.reading(child) {
                        read = true;
                        self.push_filler(style);
                    }
                    self.open_ruby = self.open_slot(style);
                }
                Tag::Rp => fallback.push((text_of(doc, child), self.styled(child, style))),
                _ => self.node(child, inner.at(i)),
            }
        }
        self.open_ruby = outer;
        if !read {
            for (text, style) in &fallback {
                self.text(text, *style, link);
            }
        }
    }

    /// A fresh slot, for the base text
    /// that follows it.
    ///
    /// Carries the style the reading
    /// will inherit, already stepped
    /// down to [`RUBY_RATIO`], so that
    /// a `fontSize` on the `rt` itself
    /// is relative to the reading's
    /// size as CSS says and not to the
    /// base's.
    pub(super) fn open_slot(&mut self, base: Inline) -> u32 {
        self.rubies.push(FlowRuby {
            text: String::new(),
            style: Inline { size: base.size * RUBY_RATIO, ..base },
        });
        self.rubies.len() as u32 - 1
    }

    /// One `rt`, into the slot its base
    /// was stamped with.
    ///
    /// One run, however many wrappers a
    /// dictionary put inside the `rt`:
    /// a reading is one to four kana
    /// over a single base, so a style
    /// change part-way through it has
    /// nowhere to go. The `rt`'s own
    /// resolved style is kept, so a
    /// dictionary that colours its
    /// readings is honoured.
    ///
    /// `false` for an `rt` that renders
    /// nothing, which leaves the `rp`
    /// fallback still owed.
    pub(super) fn reading(&mut self, id: NodeId) -> bool {
        let text = text_of(self.doc, id);
        if text.trim().is_empty() {
            return false;
        }
        let slot = self.open_ruby as usize;
        let style = self.styled(id, self.rubies[slot].style);
        self.rubies[slot] = FlowRuby { text, style };
        true
    }
}

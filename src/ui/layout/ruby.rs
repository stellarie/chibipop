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
use super::image::{image_rise, NO_IMAGE};
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
    let gaiji = gaiji_bases(flow, run);
    // What each stack of bands asks
    // for, charged to the base slot
    // they all share. Every band of one
    // stack asks its line for the
    // *whole* stack, so the tallest
    // span on the line reserves all of
    // it however many bands reached it
    // - and the bands cannot land on
    // different lines, because each
    // filler is a word joiner beside
    // the one before it.
    let base_of = base_slots(flow);
    let mut stack = vec![0.0f32; flow.ruby.len()];
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        if !ruby.text.is_empty() {
            stack[base_of[slot] as usize] += ruby.style.size;
        }
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
        if !span.filler || span.ruby as usize >= flow.ruby.len() {
            continue;
        }
        let box_ = out[span.ruby as usize];
        let asked_h = stack[base_of[span.ruby as usize] as usize];
        let grown = if box_.ascent > 0.0 { asked_h / box_.ascent } else { asked_h };
        asked.size = span.style.size.max(gaiji[span.ruby as usize]) + grown;
    }
    Ok(out)
}

/// What a slot's *gaiji* base already
/// asks its line for, as a span size,
/// and zero for a slot no image base
/// reached.
///
/// A text base needs nothing here: it
/// asks its line for its own size, and
/// the filler beside it carries that
/// size already. An image asks through
/// its [`IMAGE_RISER`] instead - the
/// span it bought its rise above the
/// baseline with, solved by
/// [`measure_images`], which is why the
/// image pass runs first. Take the
/// base's *text* size for a gaiji and
/// the slot comes out short by the
/// difference between the asset's
/// height and its line's text ascent,
/// and [`place_ruby`] clamps the
/// reading down onto the picture.
///
/// [`IMAGE_RISER`]: super::image::IMAGE_RISER
/// [`measure_images`]: super::image::measure_images
fn gaiji_bases(flow: &Flow, run: &[StyledSpan<'_>]) -> Vec<f32> {
    let mut risers = vec![0.0f32; flow.images.len()];
    for (span, asked) in flow.spans.iter().zip(run.iter()) {
        if !span.filler {
            continue;
        }
        if let Some(riser) = risers.get_mut(span.image as usize) {
            *riser = riser.max(asked.size);
        }
    }
    let mut out = vec![0.0f32; flow.ruby.len()];
    for span in flow.spans.iter().filter(|s| !s.filler) {
        let Some(riser) = risers.get(span.image as usize).copied() else {
            continue;
        };
        if let Some(slot) = out.get_mut(span.ruby as usize) {
            *slot = slot.max(riser);
        }
    }
    out
}

/// The base each slot's reading
/// annotates: its own slot, or - for a
/// band past the first - the slot whose
/// base the whole stack shares.
///
/// A band names its base by counting
/// outwards rather than by index
/// ([`FlowRuby::band`]), so this is the
/// one place the count is turned back
/// into a slot. One forward scan is
/// enough because a stack is
/// contiguous: [`Paragraphs::ruby`]
/// pushes each band's filler straight
/// after the one before it, and a
/// paragraph's renumbering
/// ([`Paragraphs::flush`]) keeps spans
/// in the order it found them.
///
/// [`Paragraphs::ruby`]: super::gloss::Paragraphs
/// [`Paragraphs::flush`]: super::gloss::Paragraphs
fn base_slots(flow: &Flow) -> Vec<u32> {
    let mut out = Vec::with_capacity(flow.ruby.len());
    let mut base = 0u32;
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        if ruby.band == 0 {
            base = slot as u32;
        }
        out.push(base);
    }
    out
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
    let base_of = base_slots(flow);
    let mut out = Vec::new();
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        let box_ = read[slot];
        if ruby.text.is_empty() || box_.h <= 0.0 {
            continue;
        }
        // The slot whose base this
        // reading annotates: its own,
        // or - past the first band -
        // the one the stack shares.
        let anchor = base_of[slot];
        // Which of the slot's spans the
        // reading is placed against.
        // Its base, normally. A `<ruby>`
        // that reached no base at all
        // still draws its reading and
        // its line still grew for it;
        // what it has no claim to is a
        // position over a base, because
        // there is none - so it is placed
        // against its own filler, which
        // stands at the pen where a base
        // would have begun and measures
        // nothing.
        let based = flow.spans.iter().any(|s| s.ruby == anchor && !s.filler);
        let base = |b: &&SpanBox| {
            flow.spans
                .get(b.span as usize)
                .is_some_and(|s| s.ruby == anchor && s.filler != based)
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
        // How far above the line's
        // baseline each base's own ink
        // reaches. A text base gives its
        // ascent share of its line box; a
        // gaiji base gives the rise
        // `place_images` puts the picture
        // at, which is what lands the
        // reading on the image's own top
        // edge and not on the ascent of
        // the no-break spaces reserving
        // it.
        let rise = |b: &SpanBox| match flow
            .spans
            .get(b.span as usize)
            .and_then(|s| flow.images.get(s.image as usize))
        {
            Some(img) => image_rise(img, *geom, wrap_w),
            None => box_.ascent * b.h,
        };
        let (left, right, top) = measured
            .spans
            .iter()
            .filter(base)
            .filter(|b| b.line == line)
            // `f32::MIN` and not zero:
            // a `verticalAlign` that
            // lowers a gaiji puts its
            // top *below* the baseline,
            // and the reading belongs
            // over the picture there
            // too.
            .fold((f32::MAX, 0.0f32, f32::MIN), |(l, r, up), b| {
                (l.min(b.x), r.max(b.x + b.w), up.max(rise(b)))
            });
        // Its bottom against that ink
        // top, or against the band below
        // it: the stack's own height is
        // what stands between the base
        // and this band's floor.
        // Clamped into the line, so a
        // measurer that gave the line no
        // extra room draws the reading
        // small and high rather than off
        // the paragraph.
        let stacked: f32 =
            (anchor as usize..=slot).map(|j| read[j].h).sum();
        let floor = geom.baseline - top;
        let y = geom.y + (floor - stacked).max(0.0);
        // Centred over the base, and
        // inside the content column on
        // both sides. A reading wider
        // than its base overhangs it,
        // which is what a browser does
        // too - but only *within* a
        // line. At a line's own edge
        // CSS Ruby Level 1 §5.2 lets a
        // user agent pull the annotation
        // back to that edge, and
        // Chromium 151 was measured
        // doing it: with the ruby
        // mid-line its `rt` box stands
        // 4.00 px left of the `ruby` box
        // and 4.02 px right of it, and
        // at a line end the `rt` runs
        // 371.28 to 393.78 against a
        // `ruby` box of 375.02 to
        // 393.77 - hung left of its base
        // and stopped at the line's
        // edge. Without the right-hand
        // pull, 岩波's `しゅくすい・ふつ
        // かよい` over a split `宿酔`
        // put 2.38 px of kana outside
        // the *panel*, which one bin
        // clips away and the other
        // paints off the rounded rect.
        //
        // The line's own alignment slack
        // comes first, because the base
        // it sits over moved by it.
        //
        // Flush with the pen instead when
        // no base reached the slot: CSS
        // gives the annotation an
        // anonymous *empty* base, so the
        // ruby box is the annotation's
        // own width and the annotation
        // starts at its left edge.
        // Centring on a zero-width point
        // would draw the reading half its
        // width back over the text before
        // it.
        let indent = (wrap_w - geom.w).max(0.0) * slack;
        let over = if based { (right - left - box_.w) / 2.0 } else { 0.0 };
        // The right pull before the left
        // clamp: a reading wider than the
        // whole column has no place that
        // holds it, and starting it at
        // the column's own left edge is
        // the most of it a reader gets.
        let x = (indent + left + over).min(wrap_w - box_.w).max(0.0);
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
    /// Which annotation band this
    /// reading is, counted outwards
    /// from the base: `0` for the one
    /// nearest it.
    ///
    /// The HTML ruby model reads a run
    /// of `rt` after one base as one
    /// independent annotation level
    /// each
    /// (<https://www.w3.org/TR/html-ruby-extensions/>),
    /// and 岩波国語辞典　第八版 writes
    /// 17 of them - `<ruby>七色<rt>なな
    /// いろ</rt><rt>しちしょく</rt>
    /// </ruby>` states both readings of
    /// a cross-referenced headword. A
    /// band past the first has no base
    /// span of its own and shares the
    /// one belonging to the nearest
    /// band `0` before it
    /// ([`base_slots`]).
    ///
    /// A count and not an index,
    /// deliberately: a paragraph
    /// renumbers its slots onto its own
    /// list ([`Paragraphs::flush`]), and
    /// a slot index stored here would
    /// have to be renumbered with them.
    /// Order survives that renumbering
    /// because a band's filler is
    /// pushed straight after the one
    /// before it, so a stack stays
    /// contiguous and in order.
    ///
    /// [`Paragraphs::flush`]: super::gloss::Paragraphs::flush
    pub(super) band: u32,
}

/// The ruby half of the gloss walk: a `ruby` subtree, its base's slot,
/// and the reading that fills it.
impl Paragraphs<'_> {
    /// Buys the open slot's reading its
    /// room.
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
    /// A slot no base reached gets one
    /// too. A reading with no base still
    /// has to be drawn, so its line
    /// still has to be tall enough for
    /// it - CSS gives such an annotation
    /// an anonymous empty ruby base and
    /// a browser draws it. What it has
    /// no claim to is a *position over a
    /// base*, and that is
    /// [`place_ruby`]'s to withhold: the
    /// filler stands at the pen the base
    /// would have begun at, and the
    /// reading is placed flush with it
    /// rather than centred on nothing.
    pub(super) fn push_filler(&mut self, style: Inline) {
        let slot = self.open_ruby;
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
    ///
    /// An `rt` whose slot no base text
    /// reached is the *next band* over
    /// the base the `rt` before it
    /// found, which is the tabular form
    /// of double-sided ruby: one base,
    /// then every annotation that
    /// belongs to it
    /// ([`FlowRuby::band`]). The two
    /// cases are told apart by the one
    /// fact that distinguishes them - a
    /// base span carrying the open slot
    /// - so `<ruby>漢<rt>かん</rt>字
    ///   <rt>じ</rt></ruby>` still pairs
    ///   each reading with its own
    ///   character, and a `<ruby>` that
    ///   reached no base at all still
    ///   opens at band zero.
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
        let mut band = 0u32;
        let inner = Ctx { inline: style, link, ..ctx };
        let mut prose = doc.prose(id);
        for (i, child) in doc.children(id).enumerate() {
            match doc.node(child).tag {
                Tag::Rt => {
                    // Its own base resets the
                    // stack; an `rt` following
                    // an `rt` climbs it.
                    let slot = self.open_ruby;
                    let based = self.cur.spans.iter().any(|s| s.ruby == slot && !s.filler);
                    if self.reading(child) {
                        band = if based { 0 } else { band + u32::from(read) };
                        self.rubies[slot as usize].band = band;
                        read = true;
                        self.push_filler(style);
                    }
                    self.open_ruby = self.open_slot(style);
                }
                Tag::Rp => fallback.push((text_of(doc, child), self.styled(child, style))),
                _ => {
                    self.node(child, inner.at(i), prose);
                    prose = prose || doc.inline_prose(child);
                }
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
            band: 0,
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
        self.rubies[slot] = FlowRuby { text, style, band: 0 };
        true
    }
}

//! One reading annotation sits over its ruby base text.
//!
//! **One reason to change:** Change how a reading reserves vertical space
//! above its ruby base and where the reading sits in that space.
//!
//! Measurement and placement make one decision.
//! The reading text is not part of the paragraph text run.
//! Therefore, the measurer gives the line only the height that the ruby base
//! needs.
//! A later measurement cannot see a change to line boxes after the wrap.
//! A platform bin measures the same spans again to paint them.
//! That measurement returns the original lines.
//! Therefore, the text run itself must produce the taller line.
//! The text run uses a zero-advance filler span ([`RUBY_FILLER`]).
//! Placement reads the final position of the ruby base.
//! Change both parts together.

use crate::dict::gloss::{NodeId, Tag};
use super::flow::{Ctx, Flow, FlowSpan, NO_LINK};
use super::gloss::{text_of, Paragraphs};
use super::image::{image_rise, NO_IMAGE};
use super::measure::{MeasureError, MeasureRun, Measured, SpanBox, StyledSpan, TextMeasure};
use super::scene::RubyBox;
use super::style::Inline;

/// This structure stores one measured reading.
///
/// [`measure_readings`] calculates these metrics, and [`place_ruby`] uses them.
#[derive(Clone, Copy, Default)]
pub(super) struct RubyMetrics {
    pub(super) w: f32,
    pub(super) h: f32,
    /// The fraction of the line height above the baseline.
    ///
    /// The measurement seam provides this font metric as `baseline / h`.
    /// Noto Sans CJK returns 0.81.
    /// The fake measurer in tests returns 0.5.
    /// Do not assume fixed values for this metric.
    pub(super) ascent: f32,
}

/// Measures all readings in a paragraph and sizes their filler spans.
///
/// Layout calls this function before it measures the paragraph.
/// This call reserves vertical space for each reading.
/// The reading text is not in the paragraph text run.
/// Therefore, the measurer gives the line only the height that the ruby base
/// needs.
/// A later measurement cannot see a change to line boxes after the wrap.
/// A platform bin measures the same spans again to paint them.
/// That measurement returns the original lines.
///
/// Therefore, the text run itself must produce the taller line.
/// Both engines and the measurement seam obey one rule.
/// A line is as tall as its tallest span.
/// A [`RUBY_FILLER`] span beside each ruby base has no ink or advance.
/// The filler exists only to become that tallest span.
/// The scene and the second platform measurement see one set of line boxes.
/// Therefore, `metrics.h` counts the readings, and no later pass edits it.
///
/// Layout sets the filler size here, not while it builds the paragraph.
/// Space above the baseline of a line equals the ascent share of the font.
/// See [`RubyMetrics::ascent`].
/// Only a measurer can provide that share.
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
    // Allocate scratch only for a paragraph with ruby.
    // The paragraph walk does not share this buffer.
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
    // Reserve space for each stack of bands at the shared ruby base slot.
    // Each band adds its own height to the stack total.
    // The tallest span on the line reserves space for all bands.
    // Word joiner characters keep every band in a stack on one line.
    let base_of = base_slots(flow);
    let mut stack = vec![0.0f32; flow.ruby.len()];
    for (slot, ruby) in flow.ruby.iter().enumerate() {
        if !ruby.text.is_empty() {
            stack[base_of[slot] as usize] += ruby.style.size;
        }
    }
    // A line with height `h` has `ascent * h` space above the baseline.
    // Ruby base text uses its own ascent within that space.
    // The line must grow by `reading / ascent` to fit the reading text.
    // Both text engines set line height in proportion to the largest span size.
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

/// Returns the span size that the gaiji base of a slot already reserves on its
/// line.
///
/// The function returns zero for a slot that no image base reaches.
/// A text base needs nothing here.
/// A text base already reserves its own size on its line.
/// The filler span beside it already carries that size.
/// An image base reserves its size through its [`IMAGE_RISER`] instead.
/// That span reserves the image rise above the baseline.
/// [`measure_images`] calculates that rise, so the image pass runs first.
///
/// Do not use the text size of the base for a gaiji.
/// The slot then becomes too small by one exact amount.
/// That amount is the asset height minus the text ascent of its line.
/// [`place_ruby`] then clamps the reading down onto the picture.
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

/// Returns the ruby base slot for each reading slot.
///
/// For outer bands in a stack, this is the shared base slot.
/// [`FlowRuby::band`] counts outward from the ruby base text.
/// Stacks are contiguous in memory.
/// [`Paragraphs::ruby`] appends filler spans in order.
/// [`Paragraphs::flush`] keeps the span order.
/// Therefore, one forward scan resolves all base slots.
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

/// Places all readings in a paragraph.
///
/// This pure function positions readings in the space that [`measure_readings`]
/// reserves.
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
        // The base slot is this reading's slot or the stack's root slot.
        let anchor = base_of[slot];
        // Select the span that anchors the reading.
        // A `<ruby>` without a ruby base still renders its reading.
        // The line height increases for this reading.
        // In this case, layout anchors the reading to its filler span.
        // The filler span starts at the pen and has zero width.
        let based = flow.spans.iter().any(|s| s.ruby == anchor && !s.filler);
        let base = |b: &&SpanBox| {
            flow.spans
                .get(b.span as usize)
                .is_some_and(|s| s.ruby == anchor && s.filler != based)
        };
        // Find the first line that contains the ruby base and its horizontal
        // range.
        // If the ruby base wraps across lines, keep the reading on the first
        // line.
        // A ruby base usually has one or two characters and rarely wraps.
        let Some(line) = measured.spans.iter().filter(base).map(|b| b.line).min() else {
            continue;
        };
        let Some(geom) = measured.lines.get(line as usize) else {
            continue;
        };
        // Measure the distance from the baseline to the top of the base ink.
        // For text bases, this equals the ascent share of the line box.
        // For gaiji bases, this equals the image rise from `place_images`.
        // This places the reading on the top edge of the image.
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
            // Use `f32::MIN`.
            // A `verticalAlign` rule can place the top of a gaiji below the
            // baseline.
            // Keep the reading above the image.
            .fold((f32::MAX, 0.0f32, f32::MIN), |(l, r, up), b| {
                (l.min(b.x), r.max(b.x + b.w), up.max(rise(b)))
            });
        // Align the bottom of the reading with the top of the base ink or lower
        // band.
        // The stack height separates the base from this band floor.
        // Clamp the vertical position into the line.
        // If the line did not expand, the reading renders inside the line.
        let stacked: f32 =
            (anchor as usize..=slot).map(|j| read[j].h).sum();
        let floor = geom.baseline - top;
        let y = geom.y + (floor - stacked).max(0.0);
        // Center the reading over the ruby base and keep it inside the content
        // column.
        // A reading wider than its base overhangs the base.
        // A browser does the same, but only inside a line.
        // CSS Ruby Level 1 section 5.2 differs at a line edge.
        // The specification lets a user agent pull the annotation back to that
        // edge.
        // A measurement of Chromium 151 records that pull.
        // With the ruby mid-line, the `rt` box stands 4.00 px left of the
        // `ruby` box.
        // It also stands 4.02 px right of the `ruby` box.
        // At a line end, the `rt` box runs 371.28 to 393.78.
        // The `ruby` box there runs 375.02 to 393.77.
        // The `rt` box extends left of its base and stops at the line edge.
        //
        // Without the right-hand pull, a wide reading can leave the panel.
        // The 岩波 reading `しゅくすい・ふつかよい` over the split `宿酔` shows this.
        // That reading put 2.38 px of kana outside the panel.
        // One bin clips those pixels away.
        // The other bin paints them outside the rounded rectangle.
        //
        // Apply the alignment slack of the line first, because it moves the base.
        //
        // If no ruby base reaches the slot, align the reading with the pen.
        // CSS gives such an annotation an anonymous empty ruby base.
        // The ruby box then has the annotation width.
        // The annotation starts at the left edge of that box.
        // A center at a zero-width point moves the reading half its width too
        // far left.
        // The reading then covers the preceding text.
        let indent = (wrap_w - geom.w).max(0.0) * slack;
        let over = if based { (right - left - box_.w) / 2.0 } else { 0.0 };
        // Apply the right pull before the left clamp.
        // No position can hold a reading wider than the whole column.
        // The left column edge shows as much of the reading as possible.
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

/// Makes one reading span for the measurement seam.
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

/// This constant sets the reading size as a fraction of the ruby base that it
/// sits over.
///
/// The specification names `ext/css/structured-content.css` as the source of
/// defaults.
/// That Yomitan stylesheet declares no `ruby`, `rt`, or `rp` rule at all.
/// Therefore, a Yomitan reader sees the browser default.
/// Both engines that Yomitan runs in give `rt` a `font-size` of 50%.
/// Therefore, this constant gives the drawn default of Yomitan.
/// [`tag_style`] reaches the defaults for `b` and `sup` through the same route.
/// That route is the stylesheet of HTML, not a number invented here.
///
/// This constant is not [`FONT_STEP`].
/// The `smaller` step is 1.2, but a reading is one-half.
/// That difference is deliberate.
/// Furigana must appear as an annotation, not as small text.
///
/// [`tag_style`]: super::style::tag_style
/// [`FONT_STEP`]: super::style::FONT_STEP
pub(super) const RUBY_RATIO: f32 = 0.5;

/// An index in [`Flow::ruby`], or a sentinel for no reading.
pub(super) const NO_RUBY: u32 = u32::MAX;

/// This character reserves the slot of a ruby base: U+2060 WORD JOINER.
///
/// A reading needs space above its ruby base, and the line above must not
/// overlap it.
/// Only a taller span on a line grows that line.
/// This character supplies that span.
/// The choice depends on two properties.
/// A probe against the real shaper confirmed both properties.
/// The code did not assume them.
/// 1. The character shapes to a glyph of zero advance, so it adds no width to
///    the line.
/// 2. The character is a word joiner, so no wrap separates it from its base.
///
/// Do not use U+200B ZERO WIDTH SPACE.
/// That character measures the same, but it creates a break opportunity.
/// It lets a line break come between a ruby base and its filler.
///
/// This choice puts one invisible character in the `text` of an element for
/// each reading.
/// Every later pass sees this cost.
/// The platform bin measures and shapes exactly that text.
/// The alternative grows line boxes after the wrap.
/// The paint pass does not reproduce that geometry.
pub(super) const RUBY_FILLER: &str = "\u{2060}";

/// This structure stores one reading annotation before placement.
///
/// Layout stores readings outside the paragraph text run.
/// A reading does not take horizontal space on its line.
/// If layout stores it in the text run, the pen advances, and a line break can
/// separate it.
#[derive(Clone)]
pub(super) struct FlowRuby {
    pub(super) text: String,
    pub(super) style: Inline,
    /// This field stores the annotation band for this reading.
    /// It counts bands outward from the ruby base.
    /// The HTML ruby model treats each `rt` after one base as a separate band.
    /// Band `0` is nearest to the ruby base.
    /// Each band is an independent annotation level.
    /// See <https://www.w3.org/TR/html-ruby-extensions/>.
    /// 岩波国語辞典　第八版 writes 17 of these stacks.
    /// The markup `<ruby>七色<rt>なないろ</rt><rt>しちしょく</rt></ruby>` is one
    /// stack.
    /// It states both readings of a cross-referenced headword.
    /// A band after the first has no base span of its own.
    /// It shares the base span of the nearest band `0` before it
    /// ([`base_slots`]).
    ///
    /// This field stores a count, not an index, by design.
    /// A paragraph renumbers its slots in its own list ([`Paragraphs::flush`]).
    /// A slot index stored here changes with the list.
    /// A count stays unchanged because a stack stays contiguous.
    /// Layout pushes the filler of each band directly after the previous
    /// filler.
    ///
    /// [`Paragraphs::flush`]: super::gloss::Paragraphs::flush
    pub(super) band: u32,
}

/// Walks ruby content in the gloss.
/// It handles a `ruby` subtree, its base slot, and the reading that fills
/// that slot.
impl Paragraphs<'_> {
    /// Reserves vertical space for the reading in the open slot.
    ///
    /// The function appends a [`RUBY_FILLER`] span directly after its ruby
    /// base.
    /// The character is a word joiner.
    /// Therefore, no wrap puts the filler on a different line from its ruby
    /// base.
    /// The filler grows the base line.
    /// The function leaves the filler size equal to the base size.
    /// [`measure_readings`] raises that size later.
    /// That call sets the reading height for the first time.
    ///
    /// The function also adds a filler span to a slot that no ruby base reaches.
    /// A reading with no ruby base still needs paint, so its line still needs
    /// height.
    /// CSS gives such an annotation an anonymous empty ruby base, and a browser
    /// draws it.
    /// The annotation has no position over a ruby base.
    /// [`place_ruby`] withholds that position.
    /// The filler starts at the pen where a ruby base starts.
    /// [`place_ruby`] aligns the reading flush with the filler, not centered on
    /// a zero-width point.
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
        // The next text must start in a new span.
        // If the next span merges with the filler, regular words take the
        // filler size.
        self.barrier = true;
    }

    /// Walks one `ruby` node and puts bases in the flow with readings above
    /// them.
    ///
    /// Layout creates one slot for each `rt`, not one slot for each `ruby`.
    /// The node `<ruby>漢<rt>かん</rt>字<rt>じ</rt></ruby>` holds two pairings.
    /// A dictionary writes per-character furigana in exactly this shape.
    /// Layout opens each slot before the base text that carries it.
    /// Layout closes each slot at the next `rt`.
    /// Therefore, each base carries its own reading, not a neighbor's reading.
    ///
    /// An `rt` whose slot has no ruby base is the next band above the earlier
    /// base.
    /// That shape is the tabular form of double-sided ruby.
    /// It states one base, then every annotation for that base.
    /// See [`FlowRuby::band`].
    /// A base span that carries the open slot separates the two cases.
    /// Therefore, `<ruby>漢<rt>かん</rt>字<rt>じ</rt></ruby>` still pairs each
    /// reading with its own character.
    /// A `<ruby>` that reached no base still opens at band zero.
    pub(super) fn ruby(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let (style, link) = self.enter(id, ctx);
        let slot = self.open_slot(style);
        let outer = std::mem::replace(&mut self.open_ruby, slot);
        let outer_path = std::mem::replace(&mut self.ruby_path, ctx.path);
        // The `rp` element holds parentheses that HTML provides for a renderer
        // that cannot draw ruby.
        // This renderer can draw ruby, so layout does not add `rp` text yet.
        // Layout adds that text only when no reading arrives.
        // That case is the fallback that this tag provides.
        let mut fallback: Vec<(String, Inline)> = Vec::new();
        let mut read = false;
        let mut band = 0u32;
        let inner = Ctx { inline: style, link, ..ctx };
        let mut prose = doc.prose(id);
        for (i, child) in doc.children(id).enumerate() {
            match doc.node(child).tag {
                Tag::Rt => {
                    // A ruby base in the current slot resets the stack.
                    // An `rt` after another `rt` climbs the stack.
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
        self.ruby_path = outer_path;
        if !read {
            for (text, style) in &fallback {
                self.text(text, *style, link, None, 0);
            }
        }
    }

    /// Creates a fresh slot for the base text that follows it.
    ///
    /// The slot carries the style that the reading inherits.
    /// Layout already reduces that style by [`RUBY_RATIO`].
    /// Therefore, a `fontSize` on `rt` is relative to the size of the reading.
    /// CSS states that rule, and the base size does not apply.
    pub(super) fn open_slot(&mut self, base: Inline) -> u32 {
        self.rubies.push(FlowRuby {
            text: String::new(),
            style: Inline { size: base.size * RUBY_RATIO, ..base },
            band: 0,
        });
        self.rubies.len() as u32 - 1
    }

    /// Records one `rt` in the slot that its ruby base carries.
    ///
    /// The function makes one run, regardless of wrappers inside `rt`.
    /// A reading has one to four kana over one ruby base.
    /// Therefore, the function cannot represent a style change inside a
    /// reading.
    /// The function keeps the resolved style of `rt` itself.
    /// Therefore, the engine obeys a dictionary that colors its readings.
    ///
    /// Returns `false` for an `rt` that paints nothing.
    /// That result leaves the `rp` fallback pending.
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

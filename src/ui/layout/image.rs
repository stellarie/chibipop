//! One inline asset: how big it is, and how it takes room on a line.
//!
//! **One reason to change:** the schema's image node - its declared size,
//! its `alt` fallback, its `appearance` - and the ladder that resolves one
//! into a box.
//!
//! No decode at any rung, which is the whole reason `dict::media` reads an
//! intrinsic size out of the container header at extraction time: `scene`
//! is a measure pass with no database behind it, and an image must not put
//! a query on the paint path.
//!
//! The room comes from the same trick [`ruby`](super::ruby) uses and for
//! the same reason: the measurement seam takes styled spans and no boxes
//! (ARCHITECTURE.md#popup-and-measurement), so a replaced element can only
//! occupy a line by *being* a span the measurer charges for.

use crate::dict::gloss::{GlossDoc, NodeId, NodePath, Scalar};
use crate::dict::media::{Intrinsic, MediaKey};
use super::flow::{Ctx, Flow};
use super::gloss::{Paragraphs, ITEM_SEPARATOR};
use super::measure::{LineBox, MeasureError, MeasureRun, Measured, SpanBox, StyledSpan, TextMeasure};
use super::ruby::RUBY_FILLER;
use super::scene::{
    Align, Appearance, ElemKind, ElemSpan, GlossOrigin, SceneElem, SceneImage, SceneRect,
};
use super::style::{finite, shift_on, Inline};

/// Every image of a paragraph, given
/// the room it needs.
///
/// Called *before* the paragraph is
/// measured, and it is the whole of
/// how an inline image occupies a
/// line. The measurement seam takes
/// styled spans and no boxes, so a
/// replaced element can only take room
/// by *being* a span the measurer
/// charges for - and editing the line
/// boxes afterwards would fool nobody,
/// because both bins re-measure an
/// element's own spans to paint it and
/// would get the ungrown lines back.
/// The ruby filler is the same trick
/// for the same reason.
///
/// So the run asks, and this decides
/// what it asks for. Two ratios are
/// needed and only a measurer knows
/// either: what one [`IMAGE_SPACER`]
/// advances per unit of size, and how
/// far down a line its baseline sits.
/// One probe answers both.
///
/// Then the arithmetic. `n` spacers at
/// size `s` advance `n * u * s`, so
/// the width the fitted box has fixes
/// `s` exactly. A span of size `r`
/// gives `asc * r` to the space above
/// its line's baseline, so the height
/// fixes the riser. The spacer is
/// capped at the riser, which is what
/// keeps a wide short banner from
/// making its line as tall as it is
/// wide: past that cap the reservation
/// comes out a few percent narrow
/// instead ([`IMAGE_SPACERS_PER_ASPECT`]).
///
/// `room` is the width the paragraph
/// wraps at, and it is both what the
/// probe is measured against and what
/// [`image_box`] fits the picture to,
/// so a picture reserves exactly the
/// room it will be drawn in.
pub(super) fn measure_images(
    m: &mut dyn TextMeasure,
    font: &str,
    flow: &Flow,
    room: f32,
    run: &mut [StyledSpan<'_>],
) -> Result<(), MeasureError> {
    if flow.images.is_empty() {
        return Ok(());
    }
    // Only a paragraph holding an image
    // pays for this buffer, which is
    // why it is not one of the walk's.
    let mut scratch = Measured::default();
    for (slot, img) in flow.images.iter().enumerate() {
        let probe = StyledSpan {
            text: IMAGE_SPACER,
            font,
            size: img.em,
            weight: img.style.weight,
            italic: img.style.italic,
            color: img.style.color,
        };
        m.measure(MeasureRun { spans: &[probe], max_w: room }, &mut scratch)?;
        // The span's own box, not
        // `metrics.w`: DirectWrite's
        // aggregate width excludes
        // trailing whitespace, and a
        // lone no-break space is
        // nothing but that.
        let per_size = |px: f32| if img.em > 0.0 { px / img.em } else { 0.0 };
        let advance = per_size(scratch.spans.first().map_or(0.0, |b| b.w));
        let ascent = per_size(scratch.lines.first().map_or(0.0, |l| l.baseline));
        let (w, h) = image_box(img, room);
        // A raised image needs its rise
        // above the baseline as well as
        // its own height; a lowered one
        // hangs into the descent, which
        // is what a lowered span does
        // too and what neither reserves.
        let rise = h + img.style.shift.max(0.0);
        let riser = if ascent > 0.0 { rise / ascent } else { rise };
        // A measurer that charges
        // nothing for a no-break space
        // can reserve nothing exactly,
        // so the spacer keeps the em it
        // was built with: some room for
        // the image beats none.
        let spacer = if advance > 0.0 && img.spacers > 0 {
            (w / (advance * img.spacers as f32)).min(riser)
        } else {
            img.em
        };
        for (span, asked) in flow.spans.iter().zip(run.iter_mut()) {
            if span.image != slot as u32 {
                continue;
            }
            asked.size = if span.filler { riser } else { spacer };
        }
    }
    Ok(())
}

/// The paragraph's images, as elements.
///
/// Pure: the lines already carry the
/// room [`measure_images`] bought, so
/// this only reads back where each
/// spacer landed and hangs the image
/// off that line's baseline.
/// One element per image rather than a
/// span of the paragraph, because an
/// image is a replaced element: it has
/// a rect and a media key and no text
/// (`ElemKind::Image`). Its `advance`
/// is zero - the paragraph's own
/// advance already counts the line the
/// riser grew - so it stacks nothing
/// and shifts nothing after it.
///
/// `room` is the same width
/// [`measure_images`] reserved
/// against, and both fit the picture
/// through [`image_box`], so what is
/// drawn is exactly what was bought.
pub(super) fn place_images(
    flow: &Flow,
    measured: &Measured,
    pen: (f32, f32),
    room: f32,
    line_at: impl Fn(LineBox) -> f32,
) -> Vec<SceneElem> {
    let mut out = Vec::new();
    for (slot, img) in flow.images.iter().enumerate() {
        let spacer = |b: &&SpanBox| {
            flow.spans
                .get(b.span as usize)
                .is_some_and(|s| s.image == slot as u32 && !s.filler)
        };
        // The first line the reservation
        // landed on. Its spacers are
        // non-breaking glue, so they
        // cannot be split across two -
        // but a measurer that overflows
        // rather than looping may still
        // report one box per fragment.
        let Some(line) = measured.spans.iter().filter(spacer).map(|b| b.line).min() else {
            continue;
        };
        let Some(geom) = measured.lines.get(line as usize) else {
            continue;
        };
        let left = measured
            .spans
            .iter()
            .filter(spacer)
            .filter(|b| b.line == line)
            .fold(f32::MAX, |l, b| l.min(b.x));
        // Its bottom on the line's own
        // baseline, raised by whatever
        // `verticalAlign` asked for -
        // `shift_on`'s own resolution,
        // against the line the image
        // landed on, with the image's box
        // as the span height a
        // line-relative value aligns.
        let (w, h) = image_box(img, room);
        let rect = SceneRect {
            x: line_at(*geom) + left,
            y: pen.1 + geom.y + geom.baseline - image_rise(img, *geom, room),
            w,
            h,
        };
        // The `alt` fallback as one
        // ordinary span, so a bin that
        // cannot decode the asset draws
        // this element exactly as it
        // draws any other and needs no
        // second text path.
        let spans = if img.alt.is_empty() {
            Vec::new()
        } else {
            vec![ElemSpan {
                at: 0,
                len: img.alt.len() as u32,
                color: img.style.color,
                size: img.em,
                weight: img.style.weight,
                italic: img.style.italic,
                shift: 0.0,
            }]
        };
        out.push(SceneElem {
            kind: ElemKind::Image,
            text: img.alt.clone(),
            color: img.style.color,
            font_size: img.em,
            weight: img.style.weight,
            italic: img.style.italic,
            top_gap: 0.0,
            wrap_w: w.max(1.0),
            align: Align::Leading,
            pen: (rect.x, rect.y),
            rect,
            lines: 0,
            advance: 0.0,
            spans,
            ruby: Vec::new(),
            marker: Vec::new(),
            block_box: None,
            inline_boxes: Vec::new(),
            origin: Some(GlossOrigin {
                dict_id: flow.dict_id,
                entry_id: flow.entry_id,
                path: img.path,
            }),
            image: Some(img.scene.clone()),
        });
    }
    out
}

/// One image's box, fitted to the room
/// its block was given.
///
/// A declared size is a demand and not
/// an answer. `image_size` resolves
/// what the node asked for and clamps
/// only at [`IMAGE_MAX_PX`], so a
/// picture 現代国語例解辞典 declares
/// `12.72em` wide was drawn 190.8 px
/// wide inside whatever column it
/// landed in - over the cell beside it
/// where the table had one, and off
/// the panel where it did not. Text in
/// that cell rewraps when
/// [`Pass::columns`] scales its track;
/// a picture has nothing to rewrap, so
/// the fit has to be here.
///
/// Yomitan asks for the same thing
/// twice, `max-width: 100%` on
/// `.gloss-image-link` and again on
/// `.gloss-image-container`, and then
/// clips what is left over with
/// `overflow: hidden`. This build has
/// no clip, and its painters stretch
/// an asset into the rect they are
/// given, so a width taken alone would
/// squash a scanned illustration
/// instead of cropping it. Both axes
/// scale by the one factor: the reader
/// gets the whole picture, smaller,
/// inside its own cell.
///
/// Negated rather than reversed, so a
/// room or a width that is not a
/// number leaves the declared box
/// exactly as it was.
///
/// [`Pass::columns`]: super::pass::Pass::columns
pub(super) fn image_box(img: &FlowImage, room: f32) -> (f32, f32) {
    if !(img.w > room && room > 0.0) {
        return (img.w, img.h);
    }
    (room, img.h * (room / img.w))
}

/// How far above its line's baseline
/// one image's box reaches.
///
/// Its own fitted height plus whatever
/// `verticalAlign` asked for -
/// [`shift_on`]'s own resolution,
/// against the line the image landed
/// on, with the image's box as the
/// span height a line-relative value
/// aligns.
///
/// One function rather than the same
/// two terms in two places, because
/// two passes ask the same question of
/// it: [`place_images`] puts the
/// picture's bottom this far above the
/// baseline, and [`place_ruby`] puts a
/// reading's bottom on the top edge
/// that leaves. A reading over a gaiji
/// has to follow the picture, so where
/// the picture sits is one decision -
/// which is why `room` reaches here
/// too: a mark over a picture its
/// column shrank has to come down with
/// it.
///
/// [`place_ruby`]: super::ruby::place_ruby
pub(super) fn image_rise(img: &FlowImage, line: LineBox, room: f32) -> f32 {
    let (_, h) = image_box(img, room);
    h + shift_on(img.style, line, h)
}

/// One image's box, by the ladder.
///
/// What the node declared, then what
/// the build recorded, then a square
/// of the text it sits in. No decode
/// at any rung - that is the whole
/// reason the intrinsic size is read
/// out of the container header at
/// extraction time.
///
/// The middle arms are the ones that
/// earn their keep. `height: 1em` and
/// no width is the shape 字通 and
/// 三省堂 both write, and one length
/// plus a ratio is the other length -
/// which is why the media row carries
/// `aspect` as its own column rather
/// than dividing on read. With no
/// ratio to hand a single length gives
/// a square, which at least sits on
/// the line at the size the dictionary
/// asked for.
pub(super) fn image_size(
    doc: &GlossDoc,
    id: NodeId,
    em: f32,
    recorded: Option<Intrinsic>,
) -> (f32, f32) {
    // `em` multiplies the text the
    // image sits in; `px` is a scene
    // pixel. The absent field is `em`,
    // because the schema's own numeric
    // lengths are em multipliers - the
    // same convention [`length_px`]
    // reads - and Yomitan renders
    // `width`/`height` as ems.
    let unit = match doc.attr_of(id, "sizeUnits").and_then(|v| doc.scalar_str(v)) {
        Some("px") => 1.0,
        _ => em,
    };
    let declared = |name| image_len(doc, id, name).map(|n| (n * unit).min(IMAGE_MAX_PX));
    let aspect = recorded.map(|size| size.aspect).filter(|a| a.is_finite() && *a > 0.0);
    match (declared("width"), declared("height")) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w / aspect.unwrap_or(1.0)),
        (None, Some(h)) => (h * aspect.unwrap_or(1.0), h),
        (None, None) => match recorded {
            Some(size) => (size.width, size.height),
            None => (em * IMAGE_FALLBACK_EM, em * IMAGE_FALLBACK_EM),
        },
    }
}

/// One declared length, as a bare
/// number.
///
/// The schema's `width` and `height`
/// are numbers whose unit is the
/// node's `sizeUnits`, so this is not
/// [`css_len`]'s job: there is no unit
/// suffix to read here. A string is
/// still accepted, because a
/// dictionary converter writing `"1"`
/// meant one. Zero and below are no
/// size at all, which drops to the
/// next rung rather than collapsing
/// the image.
///
/// [`css_len`]: super::style::css_len
pub(super) fn image_len(doc: &GlossDoc, id: NodeId, name: &str) -> Option<f32> {
    let value = doc.attr_of(id, name)?;
    let n = match value {
        Scalar::Num(n) => n as f32,
        _ => doc.scalar_str(value)?.trim().parse::<f32>().ok()?,
    };
    finite(n).filter(|n| *n > 0.0)
}

/// The text an image stands in for.
///
/// `alt` then `title`, and each from
/// the node's attributes then from its
/// `data` map. Both places are real:
/// 三省堂 writes `title` beside
/// `sizeUnits` as an attribute, and
/// Jitendex writes
/// `data: {"gaiji": "", "alt":
/// "［対義語］"}` - so reading one
/// place would lose one dictionary's
/// answer to the same question.
pub(super) fn image_alt(doc: &GlossDoc, id: NodeId) -> String {
    for name in ["alt", "title"] {
        for found in [doc.attr_of(id, name), doc.data_of(id, name)] {
            match found.and_then(|v| doc.scalar_str(v)) {
                Some(text) if !text.trim().is_empty() => return text.trim().to_string(),
                _ => {}
            }
        }
    }
    String::new()
}

/// `appearance`, of which the schema
/// has two values.
pub(super) fn image_appearance(doc: &GlossDoc, id: NodeId) -> Appearance {
    match doc.attr_of(id, "appearance").and_then(|v| doc.scalar_str(v)) {
        Some("monochrome") => Appearance::Monochrome,
        _ => Appearance::Auto,
    }
}

/// One boolean image field.
///
/// `None` for an absent field and for
/// one this build cannot read, so each
/// caller states its own default
/// rather than sharing a wrong one.
pub(super) fn image_flag(doc: &GlossDoc, id: NodeId, name: &str) -> Option<bool> {
    match doc.attr_of(id, name)? {
        Scalar::Bool(b) => Some(b),
        _ => None,
    }
}

/// How many [`IMAGE_SPACER`]s one
/// image reserves with.
///
/// From the aspect ratio alone, so it
/// is decided while the paragraph is
/// built and is the same number in
/// every font - the *size* of them is
/// what [`measure_images`] solves
/// against the face that is actually
/// installed.
pub(super) fn image_spacers(w: f32, h: f32) -> usize {
    if !(w > 0.0 && h > 0.0) {
        return 1;
    }
    let wanted = (IMAGE_SPACERS_PER_ASPECT * w / h).ceil();
    if !wanted.is_finite() || wanted < 1.0 {
        return 1;
    }
    (wanted as usize).min(IMAGE_SPACER_MAX)
}

/// An image's index in
/// [`Flow::images`], or no image at
/// all.
pub(super) const NO_IMAGE: u32 = u32::MAX;

/// What an image's inline room is
/// bought with: U+00A0 NO-BREAK
/// SPACE.
///
/// An inline image is a replaced
/// element, and the measurement seam
/// takes styled spans and no boxes -
/// so the only way an image can occupy
/// room on a line is to *be* a span
/// the measurer charges for. Growing
/// the line boxes afterwards would
/// fool nobody: both bins re-measure
/// an element's own spans to paint it
/// and would get the ungrown lines
/// back.
///
/// Three properties earn this
/// character the job. It carries no
/// ink, so nothing shows through a
/// transparent asset. It has an
/// advance, which is the whole point.
/// And it is *non-breaking glue* in
/// UAX #14, so no wrap can split one
/// image's reservation across two
/// lines, and no wrap can separate it
/// from the word it belongs to - an
/// image mid-sentence therefore wraps
/// with the text and forces no break.
///
/// Not U+2060: that has zero advance,
/// which is why it is the *riser*
/// below and not this.
pub(super) const IMAGE_SPACER: &str = "\u{a0}";

/// What an image's line height is
/// bought with, and it is
/// [`RUBY_FILLER`]'s character for
/// [`RUBY_FILLER`]'s reason: zero
/// advance, no break opportunity, and
/// its own size sets its line's height.
///
/// A separate span from
/// [`IMAGE_SPACER`] because a span's
/// advance and its line height are
/// both its size, and an image needs
/// them decided independently: a wide
/// short banner must not make its line
/// as tall as it is wide.
///
/// It also earns the image its
/// paragraph. [`IMAGE_SPACER`] is
/// *whitespace*, so a paragraph
/// holding nothing but an image would
/// measure as empty and be dropped
/// ([`Paragraphs::flush`]); U+2060 is
/// not whitespace, so the riser is
/// what says the paragraph has
/// content. [`trim`] needs no such
/// guard: it trims a named set of
/// space characters that U+00A0 is
/// deliberately not in.
///
/// [`trim`]: super::flow::trim
pub(super) const IMAGE_RISER: &str = RUBY_FILLER;

/// No-break spaces per unit of an
/// image's aspect ratio.
///
/// The count is fixed while the
/// paragraph is built and the *size*
/// is solved once the measurer has
/// been asked, because only it knows
/// what one of these advances
/// ([`measure_images`]). The count
/// still has to be generous enough
/// that the solved size stays under
/// the size the riser asked for -
/// otherwise the spacer, not the
/// image, would decide the line's
/// height.
///
/// Four is that bound for every real
/// face: a no-break space is a space,
/// a space is between a quarter and a
/// third of an em, and a face's ascent
/// is at most 0.85 em - so a space is
/// never less than a quarter of the
/// ascent one of these has to fit
/// inside. Where a face does go
/// narrower, [`measure_images`] clamps
/// the size instead of letting the
/// line grow, and the reservation
/// comes out a few percent short of
/// the image rather than the line
/// coming out several times too tall.
pub(super) const IMAGE_SPACERS_PER_ASPECT: f32 = 4.0;

/// The most no-break spaces one image
/// may reserve with.
///
/// A dictionary's declared size is
/// arbitrary author input, so the
/// aspect ratio is too, and 64 spans
/// per image is already far past any
/// asset a dictionary ships. Beyond
/// it the reservation is short, which
/// costs the image some of its room
/// and costs the panel nothing.
pub(super) const IMAGE_SPACER_MAX: usize = 64;

/// An image's fallback size, in ems:
/// a square of the text it sits in.
///
/// The last rung of the sizing ladder,
/// reached only when the node declares
/// no size *and* the store recorded
/// none - which is to say when there
/// are no bytes either, so what this
/// sizes is the placeholder box.
pub(super) const IMAGE_FALLBACK_EM: f32 = 1.0;

/// The largest box a declared size may
/// resolve to, per axis, in pixels.
///
/// `dict::media` already refuses a
/// *recorded* dimension past this, and
/// for the same reason: a declared
/// 4 294 967 295 is a corrupt file or
/// a hostile one, not content, and no
/// dictionary asset in the census is
/// anywhere near it. Clamping here is
/// what keeps author input from
/// setting a line's height through the
/// riser [`measure_images`] sizes.
pub(super) const IMAGE_MAX_PX: f32 = 65_536.0;

/// One image, before it is placed.
///
/// Sized here, while the tree is being
/// walked, because the sizing ladder
/// is arithmetic over what the node
/// declared and what the media row
/// recorded - no decode, no measurer,
/// no I/O (`dict::media`).
#[derive(Clone)]
pub(super) struct FlowImage {
    /// The resolved box, in the
    /// panel's own pixels.
    pub(super) w: f32,
    pub(super) h: f32,
    /// The em the image sits in, which
    /// is what its `4em` tint bound is
    /// measured against.
    pub(super) em: f32,
    /// Its `verticalAlign`, already
    /// resolved as far as the em alone
    /// can take it - the rest is
    /// [`shift_on`]'s, against the line
    /// the image landed on.
    pub(super) style: Inline,
    /// [`IMAGE_SPACER`]s reserving its
    /// width, so [`measure_images`] can
    /// solve their size.
    pub(super) spacers: usize,
    /// The `alt` fallback, empty when
    /// the node named none.
    pub(super) alt: String,
    /// The image node itself, so a hit
    /// on it resolves to the node and
    /// not to the paragraph around it.
    pub(super) path: Option<NodePath>,
    /// What a bin needs to paint it.
    pub(super) scene: SceneImage,
}

/// The image half of the gloss walk: an `img` node sized and reserved, or
/// its `alt` text put in the flow instead.
impl Paragraphs<'_> {
    /// One image node.
    ///
    /// An image is a *character*, not
    /// an illustration: 427 786 census
    /// nodes carry a gaiji marker and
    /// sit at `height: 1em` in the
    /// middle of a definition. So it
    /// takes room on the line it lands
    /// on and opens no line of its own:
    /// `Tag::Img` is inline, and this
    /// keeps it that way.
    ///
    /// The ladder, in the order this
    /// module tries it. With bytes
    /// behind the path the image is an
    /// element of its own, sized from
    /// what the node declared or from
    /// what the build recorded. With no
    /// bytes there is nothing to
    /// composite, so the `alt` text
    /// goes into the flow instead -
    /// which is the *better* rung, not
    /// a worse one: real text wraps
    /// with the sentence around it. And
    /// with neither, a placeholder box
    /// of one em. Never nothing:
    /// nothing is a hole in a word.
    pub(super) fn image(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let style = self.styled(id, ctx.inline);
        let path = doc
            .attr_of(id, "path")
            .and_then(|v| doc.scalar_str(v))
            .filter(|p| !p.is_empty());
        let recorded = path.and_then(|p| self.assets.size(p));
        let (w, h) = image_size(doc, id, style.size, recorded);
        let alt = image_alt(doc, id);
        // "Show images: off" takes the
        // ladder's own text rung rather
        // than cutting the node out.
        // `alt` is the text alternative
        // HTML defines for exactly this,
        // and an image node is a
        // *character* far more often
        // than an illustration
        // (427 786 census nodes carry a
        // gaiji marker), so a node cut
        // out whole would leave a hole
        // in a word. With no `alt` there
        // is nothing to stand in and the
        // node draws nothing: no
        // element, no reservation, and
        // no rect left behind, which is
        // the whole of what the setting
        // asks for.
        if !self.render.images {
            if !alt.is_empty() {
                self.text(&alt, style, ctx.link);
            }
            return;
        }
        if recorded.is_none() && !alt.is_empty() {
            return self.text(&alt, style, ctx.link);
        }
        let scene = SceneImage {
            // Only a stored asset gets a
            // key. Handing a bin a key
            // with no row behind it would
            // buy a decode attempt and a
            // cache entry for an answer
            // this walk already has.
            key: recorded
                .and(path)
                .map(|p| MediaKey::new(self.assets.dict_id, p)),
            format: recorded.map(|size| size.format),
            appearance: image_appearance(doc, id),
            // Yomitan's default is to
            // draw the backing; every
            // image node in the census's
            // samples turns it off.
            background: image_flag(doc, id, "background").unwrap_or(true),
            collapsed: image_flag(doc, id, "collapsed").unwrap_or(false),
            collapsible: image_flag(doc, id, "collapsible").unwrap_or(false),
        };
        self.reserve(
            FlowImage {
                w,
                h,
                em: style.size,
                style,
                spacers: image_spacers(w, h),
                alt,
                path: ctx.path,
                scene,
            },
            ctx.link,
        );
    }

    /// Buys one image its room on the
    /// line it lands on.
    ///
    /// Two spans, because a span's
    /// advance and its line height are
    /// the same number - its size - and
    /// an image needs them apart: the
    /// [`IMAGE_SPACER`] run is charged
    /// for the width and the
    /// [`IMAGE_RISER`] for the height.
    /// [`measure_images`] solves both
    /// sizes once the measurer has said
    /// what one of each costs.
    ///
    /// The spans are pushed rather than
    /// coalesced, and a barrier is left
    /// behind them: an image's
    /// reservation must be exactly its
    /// own, or [`place_images`] would
    /// read a box that included the
    /// word beside it.
    pub(super) fn reserve(&mut self, img: FlowImage, link: u32) {
        // An image is worth an item
        // separator and worth a list
        // marker, for the same reason
        // text is: it is content, and
        // `<li><img></li>` draws its
        // bullet.
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, img.style, link);
        }
        self.mark();
        let slot = self.images.len() as u32;
        let (style, spacers) = (img.style, img.spacers);
        self.images.push(img);
        self.raw(&IMAGE_SPACER.repeat(spacers), style, link, slot, false);
        self.raw(IMAGE_RISER, style, link, slot, true);
        self.barrier = true;
    }
}

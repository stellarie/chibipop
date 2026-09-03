//! One inline image: its size and the room that it reserves on a line.
//!
//! **One reason to change:** change the schema rules for an image node.
//! The node declares a size, an `alt` fallback, and an `appearance`.
//! This module resolves those values into an image box.
//!
//! No size rung decodes the image.
//! `dict::media` reads the intrinsic size from the container header at extraction.
//! `scene` measures content and has no database.
//! An image must not cause a database query on the paint path.
//!
//! The reservation uses the technique in [`ruby`](super::ruby) for the same reason.
//! The measurement seam takes styled spans, not boxes
//! (see ARCHITECTURE.md#popup-and-measurement).
//! A replaced element occupies a line only as a span that the measurer charges for.

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

/// Computes the room that each image needs in a paragraph.
///
/// Layout calls this function before it measures the paragraph.
/// This call lets an inline image occupy a line.
/// The measurement seam takes styled spans, not boxes.
/// Therefore, a replaced element can occupy a line only as a span that the measurer charges for.
/// A later change to line boxes cannot affect the line wrap.
/// Both platform bins re-measure each element's spans before they paint it.
/// That measurement returns the original lines without growth.
/// The ruby filler uses the same technique.
///
/// The paragraph run needs a spacer size, and this function computes it.
/// The measurer provides two ratios:
/// the advance of one [`IMAGE_SPACER`] per unit of size, and the baseline distance.
/// One probe measurement provides both ratios.
///
/// The arithmetic follows. `n` spacers at size `s` advance `n * u * s`.
/// Therefore, the fitted box width determines `s`.
/// A span of size `r` provides `asc * r` above its line baseline.
/// Therefore, the image height determines the riser.
/// This function caps spacer size at riser size.
/// Without this cap, a wide, short banner would make its line as tall as its width.
/// After the cap, the reservation becomes a few percent narrower instead
/// ([`IMAGE_SPACERS_PER_ASPECT`]).
///
/// `room` is the paragraph wrap width.
/// The probe uses this width, and [`image_box`] fits the picture to this width.
/// Therefore, the picture reserves the room that the layout later draws.
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
    // Only a paragraph that contains an image needs this scratch buffer.
    // The tree walk does not own this buffer.
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
        // Use the span's own box instead of `metrics.w`.
        // DirectWrite excludes end whitespace from aggregate width.
        // A lone no-break space counts as end whitespace.
        let per_size = |px: f32| if img.em > 0.0 { px / img.em } else { 0.0 };
        let advance = per_size(scratch.spans.first().map_or(0.0, |b| b.w));
        let ascent = per_size(scratch.lines.first().map_or(0.0, |l| l.baseline));
        let (w, h) = image_box(img, room);
        // A raised image needs its height and its rise above the baseline.
        // A lowered image enters the descent, like a lowered text span.
        // Neither case reserves descent space.
        let rise = h + img.style.shift.max(0.0);
        let riser = if ascent > 0.0 { rise / ascent } else { rise };
        // If the measurer gives a no-break space no advance, exact reservation is impossible.
        // Keep the image's em size in that case.
        // Some room for the image is better than no room.
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

/// Returns the paragraph images as scene elements.
///
/// This pure function reads the room that [`measure_images`] reserved.
/// It finds each spacer and places the image from that line's baseline.
///
/// The function returns one scene element for each image, not a paragraph span.
/// An image is a replaced element with a rect and a media key, but no text (`ElemKind::Image`).
/// Its `advance` is zero.
/// The paragraph advance already includes the line growth from the riser.
/// Therefore, the image element adds no stack height and shifts no later element.
///
/// `room` is the width that [`measure_images`] used.
/// Both functions fit the picture through [`image_box`].
/// Therefore, the drawn picture matches the reserved picture.
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
        // This is the first line that contains the reservation.
        // Its spacers are non-breaking glue, so a wrap cannot split them.
        // Some measurers retry after content overflows.
        // Other measurers allow overflow and can report one box per fragment.
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
        // The image bottom sits on the line baseline.
        // `verticalAlign` raises it by the amount that `shift_on` resolves.
        // `shift_on` resolves that amount against the line that contains the image.
        // It uses the image box as the span height for line-relative alignment.
        let (w, h) = image_box(img, room);
        let rect = SceneRect {
            x: line_at(*geom) + left,
            y: pen.1 + geom.y + geom.baseline - image_rise(img, *geom, room),
            w,
            h,
        };
        // The `alt` fallback becomes an ordinary span.
        // A platform bin that cannot decode the asset then paints it like any other element.
        // It needs no second text path.
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
            font_size: img.style.size,
            weight: img.style.weight,
            italic: img.style.italic,
            top_gap: 0.0,
            wrap_w: 0.0,
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
                entry: flow.entry,
                path: img.path,
            }),
            image: Some(img.scene.clone()),
            sources: Vec::new(),
        });
    }
    out
}

/// Returns an image box that fits the room assigned to its block.
///
/// A declared size is a request, not a final size.
/// `image_size` resolves the node request and clamps each axis at [`IMAGE_MAX_PX`].
/// For example, 現代国語例解辞典 declares one picture as `12.72em` wide.
/// The layout drew that picture at 190.8 px inside its assigned column.
/// The column can be a table cell beside the picture or the panel width.
/// Text in a cell rewraps when [`Pass::columns`] scales its track.
/// A picture has no text to rewrap, so this function must fit it.
///
/// Yomitan sets `max-width: 100%` on the image link box, `.gloss-image-link`, and
/// on `.gloss-image-container`.
/// It clips overflow with `overflow: hidden`, so the picture keeps its proportions.
/// This build applies no clip.
/// Its painters stretch an asset into the received rect.
/// A width-only constraint would squash a scanned illustration.
/// This function scales both axes together, so the reader sees the full picture inside its cell.
///
/// This function negates the fit condition instead of reversing the comparison.
/// A non-number room or width leaves the declared box unchanged.
///
/// [`Pass::columns`]: super::pass::Pass::columns
pub(super) fn image_box(img: &FlowImage, room: f32) -> (f32, f32) {
    if !(img.w > room && room > 0.0) {
        return (img.w, img.h);
    }
    (room, img.h * (room / img.w))
}

/// Returns the distance from a line baseline to an image box top.
///
/// The result is the fitted image height plus the `verticalAlign` shift.
/// [`shift_on`] resolves that shift against the line that contains the image.
/// The image box provides the span height for line-relative alignment.
///
/// Two passes ask for this distance.
/// [`place_images`] places the picture's bottom this far above the baseline.
/// [`place_ruby`] places the ruby text's bottom on the top edge that this function returns.
/// Ruby text over a gaiji must follow the picture.
/// One function keeps this position decision consistent.
/// `room` reaches this function because a narrower column changes image height.
/// A ruby mark over that image must move down with it.
///
/// [`place_ruby`]: super::ruby::place_ruby
pub(super) fn image_rise(img: &FlowImage, line: LineBox, room: f32) -> f32 {
    let (_, h) = image_box(img, room);
    h + shift_on(img.style, line, h)
}

/// Returns an image box from the size ladder.
///
/// The ladder tries the node declaration, the recorded intrinsic size, and a square of the text.
/// No rung decodes the image.
/// The build reads intrinsic size from the container header at extraction.
///
/// The two middle match arms handle a common form.
/// 字通 and 三省堂 write `height: 1em` without a width.
/// One length and a ratio determine the other length.
/// The media row stores `aspect` as its own column for this reason.
/// Otherwise, each row read would divide two lengths.
/// Without a ratio, one length produces a square.
/// The square keeps the image on the line at the declared size.
pub(super) fn image_size(
    doc: &GlossDoc,
    id: NodeId,
    em: f32,
    recorded: Option<Intrinsic>,
) -> (f32, f32) {
    // `em` multiplies the size of the text that contains the image.
    // `px` represents one scene pixel.
    // An absent `sizeUnits` field uses `em`.
    // The schema treats numeric lengths as em multipliers by convention.
    // [`length_px`] uses that convention, and Yomitan renders `width`/`height` as ems.
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

/// Returns one declared length as a bare number.
///
/// The schema stores `width` and `height` as numbers.
/// Their unit comes from the node's separate `sizeUnits` attribute.
/// This function does not parse a unit suffix, unlike [`css_len`].
/// It accepts a string because a dictionary converter that writes `"1"` means one.
/// A value at or below zero provides no size.
/// The size ladder then tries its next rung.
/// The image does not collapse to zero.
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

/// Returns the text that represents an image.
///
/// This function tries `alt` and then `title`.
/// For each name, it checks node attributes before the `data` map.
/// Both locations occur in dictionary data.
/// 三省堂 writes `title` beside `sizeUnits` as an attribute.
/// Jitendex writes `data: {"gaiji": "", "alt": "［対義語］"}` instead.
/// One location alone would lose a dictionary value.
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

/// Returns `appearance`. The schema defines two values for this field.
pub(super) fn image_appearance(doc: &GlossDoc, id: NodeId) -> Appearance {
    match doc.attr_of(id, "appearance").and_then(|v| doc.scalar_str(v)) {
        Some("monochrome") => Appearance::Monochrome,
        _ => Appearance::Auto,
    }
}

/// Returns one boolean image field.
///
/// This function returns `None` for an absent field or an unreadable field.
/// Each caller supplies its own default.
/// A shared default could be wrong for one caller.
pub(super) fn image_flag(doc: &GlossDoc, id: NodeId, name: &str) -> Option<bool> {
    match doc.attr_of(id, name)? {
        Scalar::Bool(b) => Some(b),
        _ => None,
    }
}

/// Returns the number of [`IMAGE_SPACER`] values that one image reserves.
///
/// The aspect ratio alone determines this count.
/// The tree walk fixes the count while it builds the paragraph.
/// Every font uses the same count, but spacer size can differ.
/// [`measure_images`] solves that size for the installed font.
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

/// An image slot in [`Flow::images`], or no image when the value is `u32::MAX`.
pub(super) const NO_IMAGE: u32 = u32::MAX;

/// Provides an image's inline room: U+00A0 NO-BREAK SPACE.
///
/// An inline image is a replaced element.
/// The measurement seam takes styled spans, not boxes.
/// Therefore, an image can occupy line room only as a span that the measurer charges for.
/// A later change to line boxes cannot affect the line wrap.
/// Both platform bins re-measure an element's spans before they paint it.
/// That measurement returns the original lines without growth.
///
/// This character has three required properties.
/// It has no ink, so no text shows through a transparent asset.
/// It has an advance, so it reserves room.
/// It acts as *non-breaking glue* under UAX #14.
/// A wrap cannot split one image reservation across lines.
/// A wrap cannot separate the reservation from its word.
/// An image in a sentence therefore wraps with nearby text and does not force a break.
///
/// This constant is not U+2060.
/// U+2060 has zero advance.
/// U+2060 serves as the *riser* below, not as this spacer.
pub(super) const IMAGE_SPACER: &str = "\u{a0}";

/// Provides an image's line height.
/// This uses the same character as [`RUBY_FILLER`] for the same reason.
/// It has zero advance and no break opportunity.
/// Its size sets the line height.
///
/// This is a separate span from [`IMAGE_SPACER`].
/// The size of a span sets its advance and its line height.
/// An image needs these effects to resolve independently.
/// A wide, short banner must not make its line as tall as its width.
///
/// This character also gives the image its own paragraph.
/// [`IMAGE_SPACER`] is *whitespace*.
/// A paragraph with only spacer spans measures as empty, and [`Paragraphs::flush`] drops it.
/// U+2060 is not whitespace, so the riser tells the code that the paragraph has content.
/// [`trim`] needs no guard.
/// [`trim`] removes a named set of space characters, and U+00A0 is not in that set.
///
/// [`trim`]: super::flow::trim
pub(super) const IMAGE_RISER: &str = RUBY_FILLER;

/// Number of no-break spaces per unit of an image's aspect ratio.
///
/// The tree walk fixes this count while it builds the paragraph.
/// Only the measurer knows the advance of one spacer ([`measure_images`]).
/// It solves the spacer size once after it measures the font.
/// The count must keep that size below the riser size.
/// Otherwise, the spacer would set line height instead of the image.
///
/// Four is the bound for every real font face.
/// A no-break space is a space.
/// A space has a width between one quarter and one third of an em.
/// A face ascent is at most 0.85 em.
/// Therefore, a space is never narrower than one quarter of the ascent that the spacers need.
/// If a face is narrower, [`measure_images`] caps spacer size with no line-height change.
/// The reservation then becomes a few percent narrower than the image.
/// The line does not become several times too tall.
pub(super) const IMAGE_SPACERS_PER_ASPECT: f32 = 4.0;

/// Maximum no-break spaces that one image can reserve.
///
/// A dictionary declaration is arbitrary author input, so its aspect ratio is arbitrary.
/// 64 spans per image exceeds every asset in the real dictionary census.
/// Beyond this limit, the reservation becomes smaller than the image.
/// The image loses some room, but the panel keeps its size.
pub(super) const IMAGE_SPACER_MAX: usize = 64;

/// Fallback image size in ems: a square of the text around it.
///
/// This is the last size ladder rung.
/// The code reaches it when the node declares no size and the store records no size.
/// That combination also means no image bytes exist.
/// Therefore, this constant sizes only the placeholder box.
pub(super) const IMAGE_FALLBACK_EM: f32 = 1.0;

/// Largest box size that a declared value can resolve to on one axis.
///
/// `dict::media` rejects a recorded dimension above this limit for the same reason.
/// A declared value of 4 294 967 295 indicates a corrupt or hostile file, not real content.
/// No dictionary asset in the census approaches this value.
/// Without this clamp, bad author input could set line height through the riser that
/// [`measure_images`] sizes.
pub(super) const IMAGE_MAX_PX: f32 = 65_536.0;

/// One image before layout places it.
///
/// This struct records image size at the tree walk.
/// The size ladder uses arithmetic from the node declaration and the media row.
/// It needs no decode, measurer, or I/O (`dict::media`).
#[derive(Clone)]
pub(super) struct FlowImage {
    /// Resolved box in the panel's own pixels.
    pub(super) w: f32,
    pub(super) h: f32,
    /// The em value that sizes the image.
    /// The `4em` tint bound uses this value.
    pub(super) em: f32,
    /// The `verticalAlign` shift resolved from the image's em unit.
    /// [`shift_on`] resolves line-relative alignment against the image's line.
    pub(super) style: Inline,
    /// [`IMAGE_SPACER`] values that reserve the width.
    /// [`measure_images`] solves their size.
    pub(super) spacers: usize,
    /// The `alt` fallback.
    /// It is empty when the node declares none.
    pub(super) alt: String,
    /// The image node path.
    /// A hit on the image resolves to this node instead of the nearby paragraph.
    pub(super) path: Option<NodePath>,
    /// Data that a platform bin needs to paint the image.
    pub(super) scene: SceneImage,
}

/// Handles the image part of the gloss walk.
/// An `img` node is sized and reserved, or its `alt` text enters the flow.
impl Paragraphs<'_> {
    /// Handles one image node.
    ///
    /// An image acts as a *character*, not an illustration.
    /// 427 786 census nodes carry a gaiji marker and use `height: 1em` inside a definition.
    /// Therefore, an image reserves room on its line and opens no line of its own.
    /// `Tag::Img` is inline, and this method preserves that behavior.
    ///
    /// This method tries the size ladder in order.
    /// With bytes at the path, the image becomes its own element.
    /// It uses the node declaration or the recorded media size.
    /// Without bytes, no asset exists to composite, so the `alt` text enters the flow.
    /// This is the better rung because real text wraps with the nearby sentence.
    /// Without bytes or `alt` text, the method uses a one em placeholder box.
    /// It never renders nothing because nothing would leave a hole in a word.
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
        // "Show images: off" selects the text rung instead of node removal.
        // `alt` is the HTML text alternative for this case.
        // An image node acts as a *character* more often than an illustration.
        // 427 786 census nodes carry a gaiji marker.
        // If code removes the whole node, it leaves a hole in a word.
        // Without `alt`, the node draws no element, reserves no room, and leaves no rect.
        // This result matches the setting.
        if !self.render.images {
            if !alt.is_empty() {
                self.text(&alt, style, ctx.link, None, 0);
            }
            return;
        }
        if recorded.is_none() && !alt.is_empty() {
            return self.text(&alt, style, ctx.link, None, 0);
        }
        let scene = SceneImage {
            // Give a key only to a stored asset.
            // A key without a media row would send the platform bin to decode an asset
            // with no row and create a cache entry.
            // The walk already has the correct result.
            key: recorded
                .and(path)
                .map(|p| MediaKey::new(self.assets.dict_id, p)),
            format: recorded.map(|size| size.format),
            appearance: image_appearance(doc, id),
            // Yomitan uses the backing by default.
            // Every image node in the census turns it off.
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

    /// Reserves room for one image on its line.
    ///
    /// The size of a span sets both its advance and its line height.
    /// The image needs these effects to resolve independently.
    /// The [`IMAGE_SPACER`] span reserves width.
    /// The [`IMAGE_RISER`] span reserves height.
    /// [`measure_images`] solves both sizes after the measurer provides their advances.
    ///
    /// This method keeps the spans separate and leaves a barrier after them.
    /// The image reservation must contain only its own spans.
    /// Otherwise, [`place_images`] would read a box that also contains the adjacent word.
    pub(super) fn reserve(&mut self, img: FlowImage, link: u32) {
        // An image counts as an item separator and a list marker, like text.
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, img.style, link, None);
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

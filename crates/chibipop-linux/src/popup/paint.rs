//! The panel raster: one `PopupScene` into one premultiplied buffer.
//!
//! The Linux twin of the Windows Direct2D `paint_once`, same paint
//! order, two deliberate differences. Windows clips its window to a
//! `CreateRoundRectRgn` and asks the compositor for a constant
//! `LWA_ALPHA`; a layer surface has neither, so the rounded rect *is*
//! the silhouette - everything outside it stays fully transparent -
//! and the constant alpha is reproduced per pixel by fading the
//! finished frame once, at the end (ADR-0004). The corners therefore
//! fade instead of being hard-clipped, which is the one place this
//! looks better than Win32, at the cost of one pass over the buffer.
//!
//! The Anki affordance is painted here too. Windows only gives it a
//! separate HWND because a `WS_EX_TRANSPARENT` popup cannot receive a
//! click; our panel serves it from its own input region, so the slot
//! core reserved is just another strip of this buffer (ADR-0004).
//!
//! Everything arriving here is already in physical pixels: the scene
//! was laid out at the output's fractional scale and the theme is
//! `physical_theme`. [`Panel::scale`] is *only* for the lengths the
//! theme does not carry - hairlines and `SCROLLBAR_W`.
//!
//! Channel order is not this module's business. tiny-skia calls the
//! buffer RGBA, `wl_shm`'s `Argb8888` is BGRA little-endian, and the
//! surface code owns that reinterpretation when it hands the pixmap
//! over. Nothing here would notice: solid fills go through one
//! `Color` constructor and the fade scales all four channels by the
//! same factor.

use crate::popup::{DrawRun, PanelText, PANEL_ALPHA};
use chibipop::ui::layout::StyledSpan;
use chibipop::ui::layout::{Align, ElemBox, ElemKind, Measured, MeasureRun};
use chibipop::ui::layout::{PopupScene, Rgb, SceneElem, SceneImage, SceneRect};
use chibipop::ui::theme::{Theme, SCROLLBAR_W};
// The decoded-surface cache lives beside the rest of the popup on disk and
// is declared by the lib face rather than by `popup/mod.rs`, so that its
// own tests can link against a real built database. Reaching it by the
// package's library name is what keeps it compiled - and its tests run -
// exactly once.
use chibipop_linux::media::MediaSurfaces;
use tiny_skia::{Color, FillRule, FilterQuality, Paint, Path, PathBuilder, Pattern};
use tiny_skia::{Pixmap, PixmapMut, Rect, Shader, SpreadMode, Stroke, Transform};

/// Everything one frame needs.
pub struct Panel<'a> {
    pub scene: &'a PopupScene,
    /// The physical-pixel theme (`popup::physical_theme`).
    pub theme: &'a Theme,
    /// Scroll offset, physical px.
    pub scroll: f32,
    /// The output's fractional scale: hairlines only.
    pub scale: f32,
}

/// Paint one frame into `target` (whose size IS the surface size).
///
/// `media` is the painter's own decoded-asset cache, and `None` is a real
/// state: a session whose dictionary database this build cannot open still
/// paints, with every image on the `alt`-text rung of its ladder.
pub fn panel(
    p: &Panel<'_>,
    text: &mut dyn PanelText,
    media: Option<&mut MediaSurfaces>,
    target: &mut PixmapMut<'_>,
) {
    let (w, h) = (target.width() as f32, target.height() as f32);
    let scene = p.scene;
    let theme = p.theme;
    let mut media = media;

    // 1. Transparent, not `Clear(background)`: outside the rounded
    // rect there is no window region to hide the overdraw.
    transparent(target);

    // 2/3. The panel and its edge. The stroke path is inset by half
    // the stroke width, since a centred stroke on the buffer edge
    // loses its outer half - Win32 could stroke the window rect
    // itself and let the region take the difference.
    let radius = theme.corner_radius as f32;
    if let Some(shape) = rounded(0.0, 0.0, w, h, radius) {
        let bg = solid(theme.background);
        target.fill_path(&shape, &bg, FillRule::Winding, Transform::identity(), None);
    }
    // The theme's border, already in physical pixels, and never
    // thinner than a pixel: at the default 1.0 DIP this is exactly
    // the hairline Windows strokes.
    let sw = theme.border_width.max(1.0);
    let inset = sw / 2.0;
    if let Some(edge) = rounded(inset, inset, w - sw, h - sw, radius - inset) {
        let stroke = Stroke { width: sw, ..Stroke::default() };
        target.stroke_path(&edge, &solid(theme.border), &stroke, Transform::identity(), None);
    }

    // 4. Elements, in the scene's order, scroll applied and off-panel
    // runs already culled by core. One paragraph is one run, however
    // many styles it holds, so the wrap the scene measured is the wrap
    // that paints (ADR-0013).
    let mut spans: Vec<StyledSpan<'_>> = Vec::new();
    let mut shifts: Vec<f32> = Vec::new();
    for painted in scene.visible(p.scroll, scene.view_h) {
        let elem = painted.elem;
        if elem.kind == ElemKind::Separator {
            fill(target, elem.rect.x, painted.pen.1, elem.rect.w, elem.rect.h, elem.color);
            continue;
        }
        // Boxes first, under the text: a pill's fill is behind the
        // label it tints, and a block's border frames the paragraph
        // inside it. The scene resolved every rect, so this walk
        // decides nothing - it only draws.
        let dy = painted.pen.1 - elem.pen.1;
        for b in elem.boxes() {
            box_of(target, b, dy);
        }
        // An image, and its ladder: the asset if this build can decode
        // it, then the `alt` run the element already carries, then an
        // outlined box. Never nothing, because nothing is a hole in a
        // word.
        if let Some(img) = &elem.image {
            if asset(target, elem, img, painted.pen.1, media.as_deref_mut(), theme) {
                continue;
            }
            if elem.spans.is_empty() {
                placeholder(target, elem.rect_at(painted.pen.1), elem.color, p.scale);
                continue;
            }
        }
        // Nothing to shape: an empty table cell draws its border and no
        // text, and the seam takes no empty run.
        if elem.spans.is_empty() {
            continue;
        }
        // `Trailing` is the frequency corner, and a `textAlign` on a
        // gloss. DirectWrite gets the whole wrap box plus an
        // alignment flag; a `DrawRun` carries no alignment, so we
        // hand over the ink box the scene already aligned instead.
        // Same pixels for a box holding one line, which is every
        // aligned box the corpus draws - a corner, an attribution,
        // a centred cell. A *wrapped* aligned paragraph puts its
        // shorter lines at the ink box's left edge rather than
        // centring each: `DrawRun` would have to carry alignment for
        // that, which is a change to the Linux text seam and not to
        // the box model.
        let (origin, max_w) = match elem.align {
            Align::Trailing | Align::Center => {
                ((elem.rect.x, painted.pen.1), elem.rect.w.max(1.0))
            }
            Align::Leading => (painted.pen, elem.wrap_w),
        };
        spans.clear();
        spans.extend(elem.styled_spans(&theme.font_name));
        shifts.clear();
        shifts.extend(elem.spans.iter().map(|s| s.shift));
        text.draw_run(DrawRun { spans: &spans, shifts: &shifts, max_w, origin }, target);
        // The readings, over the bases
        // they were placed against. Not
        // spans of the run above: a
        // reading takes no horizontal
        // room from the line it sits on,
        // so it is drawn where the scene
        // put it and at the width the
        // scene measured it at, which is
        // the element's own.
        for run in &elem.ruby {
            let span = run.styled_span(&theme.font_name);
            text.draw_run(
                DrawRun {
                    spans: &[span],
                    shifts: &[0.0],
                    max_w: elem.wrap_w,
                    origin: (origin.0 + run.x, origin.1 + run.y),
                },
                target,
            );
        }
        // The list markers, in the
        // gutters their lists opened.
        // Against `painted.pen` and not
        // against `origin`: a marker box
        // hangs off the item's *content*
        // edge, which is the pen,
        // whatever the text inside the
        // item aligns to. `textAlign`
        // moves line boxes, and a marker
        // box is not one.
        for run in &elem.marker {
            let span = run.styled_span(&theme.font_name);
            text.draw_run(
                DrawRun {
                    spans: &[span],
                    shifts: &[0.0],
                    max_w: elem.wrap_w,
                    origin: (painted.pen.0 + run.x, painted.pen.1 + run.y),
                },
                target,
            );
        }
    }

    // 5. The "See also" column. Its rule divides the whole panel, not
    // just the column, so it runs to the bottom padding - of the body
    // view, which on Windows is the window and here stops short of
    // the Anki strip.
    if let Some(side) = &scene.side {
        let bottom = scene.view_h - theme.padding as f32;
        let rule_h = bottom - side.origin_y;
        fill(target, side.rule_x, side.origin_y, side.rule_w, rule_h, theme.separator);
        for painted in side.visible(p.scroll, scene.view_h) {
            let span = side_span(theme, &painted.row.text, painted.row.color);
            text.draw_run(
                DrawRun {
                    spans: &[span],
                    shifts: &[0.0],
                    max_w: side.col_w,
                    origin: (side.col_x, painted.y),
                },
                target,
            );
        }
    }

    // 6. The thumb, and never a track. Core does this arithmetic in whole
    // pixels, so the scene's f32 heights round on the way in - and core
    // derives the track and the content height off the scene itself, so
    // this painter and the Windows one cannot disagree about what the
    // padding does to either.
    let pad = theme.padding;
    let view = scene.view_h.round() as i32;
    let at = p.scroll.round() as i32;
    if let Some((top, thumb_h)) = scene.scrollbar_thumb(pad, view, at) {
        // The one length the painter scales itself: `SCROLLBAR_W` is
        // a logical constant, unlike everything in `theme`.
        let bar_w = (SCROLLBAR_W as f32 * p.scale).round().max(1.0);
        let x = w - (pad / 2) as f32 - bar_w;
        fill(target, x, (pad + top) as f32, bar_w, thumb_h as f32, theme.dimmed_text);
    }

    // 7. The Anki strip: a rule across its top, then the label,
    // centred in it. Centring needs a measurement, which is why
    // `PanelText` is a `TextMeasure` supertrait - and a measurer that
    // refuses costs us the centring, never the frame.
    if let Some(anki) = &scene.anki {
        let rect = anki.rect;
        fill(target, rect.x, rect.y, rect.w, hairline(p.scale), theme.separator);
        let span = side_span(theme, &anki.label, anki.color);
        let mut scratch = Measured::default();
        let measured = text
            .measure(MeasureRun { spans: &[span], max_w: rect.w }, &mut scratch)
            .map(|()| scratch.metrics);
        let x = match measured {
            Ok(m) => rect.x + ((rect.w - m.w) / 2.0).max(0.0),
            Err(_) => rect.x,
        };
        text.draw_run(
            DrawRun {
                spans: &[span],
                shifts: &[0.0],
                max_w: rect.w.max(1.0),
                origin: (x, rect.y),
            },
            target,
        );
    }

    // 8. Windows' constant window alpha, per pixel.
    fade(target, PANEL_ALPHA);
}

/// The collapsed role's span, which the side column and the Anki label
/// both draw in.
///
/// Neither comes off a `SceneElem`, so neither has a span list of its
/// own: core names their text and their colour, and the role supplies
/// the rest - the same shape `layout::side_span` measures them at.
fn side_span<'a>(theme: &'a Theme, text: &'a str, color: Rgb) -> StyledSpan<'a> {
    StyledSpan {
        text,
        font: &theme.font_name,
        size: theme.collapsed_size,
        weight: theme.collapsed_weight,
        italic: theme.collapsed_italic,
        color,
    }
}

/// The hide frame: a fully transparent buffer.
///
/// The surface is never unmapped (ADR-0004), so Hyprland's layer
/// animation never fires and show/hide stays instant. Zeroed bytes
/// *are* transparent premultiplied, whatever the channel order, and
/// zeroing sidesteps `pixels_mut`'s cast of a buffer this module did
/// not size.
pub fn transparent(target: &mut PixmapMut<'_>) {
    let n = pixel_bytes(target);
    target.data_mut()[..n].fill(0);
}

/// Total surface height for a scene: the body view plus the Anki
/// strip.
///
/// One surface holds both, unlike Windows, where the affordance is a
/// second window and the popup's height is the view alone. Placement
/// asks for this, not `view_h`.
pub fn surface_height(scene: &PopupScene) -> f32 {
    scene.view_h + scene.anki.as_ref().map_or(0.0, |a| a.rect.h)
}

/// Windows strokes 1.0 DIP; the physical equivalent is one scaled
/// pixel, never thinner than a pixel or antialiasing turns a border
/// into a smudge.
fn hairline(scale: f32) -> f32 {
    scale.max(1.0)
}

/// Bytes of `target` that are actually pixels.
///
/// `PixmapMut::from_bytes` accepts an oversized slice - an shm pool
/// hands out one sized by its own stride - so whole-buffer loops take
/// their length from the size and not from the slice.
fn pixel_bytes(target: &PixmapMut<'_>) -> usize {
    target.width() as usize * target.height() as usize * 4
}

/// Scales every channel of every pixel by `alpha/255`.
///
/// Bytes, not `pixels_mut`: the premultiplied constructor that skips
/// the `rgb <= a` check is crate-private in tiny-skia, and the check
/// is pointless here - one monotonic factor applied to all four
/// channels preserves the invariant and cannot care which channel is
/// which.
fn fade(target: &mut PixmapMut<'_>, alpha: u8) {
    let n = pixel_bytes(target);
    let a = u32::from(alpha);
    for b in &mut target.data_mut()[..n] {
        // Skia's rounding `c * a / 255`, without a divide.
        let prod = u32::from(*b) * a + 128;
        *b = ((prod + (prod >> 8)) >> 8) as u8;
    }
}

/// An opaque paint. Translucency belongs to [`fade`], once, at the
/// end - a translucent fill here would blend against the frame under
/// it and stack.
fn solid((r, g, b): Rgb) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(Color::from_rgba8(r, g, b, 255)),
        ..Paint::default()
    }
}

/// Fills one box, or nothing when it is degenerate. `Rect` rejects a
/// non-finite edge, so this is total.
fn fill(target: &mut PixmapMut<'_>, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(rect) = Rect::from_xywh(x, y, w, h) else { return };
    target.fill_rect(rect, &solid(color), Transform::identity(), None);
}

/// Circular-arc approximation: the cubic handle length for a quarter
/// circle of radius 1.
const ARC: f32 = 0.552_284_8;

/// A rounded rect as a path, matching D2D's elliptical corners.
///
/// `None` for a box with no area, which tiny-skia would refuse to
/// fill anyway. The radius is clamped to the box so a popup shorter
/// than its corner radius degrades to a stadium instead of folding
/// the path inside out.
fn rounded(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<Path> {
    let rect = Rect::from_xywh(x, y, w, h)?;
    let r = radius.min(w / 2.0).min(h / 2.0);
    if r <= 0.0 {
        return Some(PathBuilder::from_rect(rect));
    }
    let (l, t, right, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let c = r * ARC;
    let mut p = PathBuilder::new();
    p.move_to(l + r, t);
    p.line_to(right - r, t);
    p.cubic_to(right - r + c, t, right, t + r - c, right, t + r);
    p.line_to(right, b - r);
    p.cubic_to(right, b - r + c, right - r + c, b, right - r, b);
    p.line_to(l + r, b);
    p.cubic_to(l + r - c, b, l, b - r + c, l, b - r);
    p.line_to(l, t + r);
    p.cubic_to(l, t + r - c, l + r - c, t, l + r, t);
    p.close();
    p.finish()
}

/// One scene box: its fill, then its border.
///
/// `dy` is the scroll the element was drawn at, since a box's rect is
/// in unscrolled panel space like every other rect the scene reports.
///
/// A stroke is inset by half its width so the border sits inside the
/// box, which is what CSS's border box means and what the panel's own
/// edge already does. Whether one stroke will do is
/// `BoxStyle::even_border`'s answer, not this painter's: a rounded
/// corner between two different widths has no single path, and the only
/// asymmetric border in the census corpus is a one-sided rule, which a
/// rect draws exactly.
///
/// Every drawn style strokes solid. The scene carries `dashed`,
/// `dotted` and `double` faithfully for a later painter; the corpus's
/// only observed values are `solid` and `none`.
fn box_of(target: &mut PixmapMut<'_>, b: &ElemBox, dy: f32) {
    let (x, y, w, h) = (b.rect.x, b.rect.y + dy, b.rect.w, b.rect.h);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = b.style.radius;
    if let Some(bg) = b.style.background {
        if let Some(shape) = rounded(x, y, w, h, radius) {
            let paint = solid(bg);
            target.fill_path(&shape, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    let color = b.style.border_color;
    // One stroke or four fills is core's call, not the painter's, so
    // both bins answer it the same way (`BoxStyle::even_border`).
    if let Some(width) = b.style.even_border() {
        let inset = width / 2.0;
        if let Some(edge) =
            rounded(x + inset, y + inset, w - width, h - width, radius - inset)
        {
            let stroke = Stroke { width, ..Stroke::default() };
            target.stroke_path(&edge, &solid(color), &stroke, Transform::identity(), None);
        }
        return;
    }
    let e = b.style.border_used();
    fill(target, x, y, w, e.top, color);
    fill(target, x, y + h - e.bottom, w, e.bottom, color);
    fill(target, x, y, e.left, h, color);
    fill(target, x + w - e.right, y, e.right, h, color);
}

/// One asset, composited into its resolved box. `false` means the ladder
/// has to fall through.
///
/// The backing fill first, and only when the node asked for it: Yomitan
/// draws one behind a transparent asset and every image node in the
/// census's samples turns it off, so this is nearly always skipped. The
/// panel's own background is the honest backing here - a light grey behind
/// a gaiji would be the one opaque patch on a dark theme.
///
/// Scaled with a pattern rather than `draw_pixmap`, because the resolved
/// box is fractional: `draw_pixmap` translates by whole pixels, and a gaiji
/// half a pixel off its baseline is visible next to the text it stands in
/// for.
fn asset(
    target: &mut PixmapMut<'_>,
    elem: &SceneElem,
    img: &SceneImage,
    pen_y: f32,
    media: Option<&mut MediaSurfaces>,
    theme: &Theme,
) -> bool {
    let rect = elem.rect_at(pen_y);
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return false;
    }
    let (Some(key), Some(cache)) = (img.key.as_ref(), media) else {
        return false;
    };
    // The tint is core's decision, so both bins take the expensive path
    // on exactly the same assets (`SceneImage::tint`).
    let tint = img.tint(elem.rect, elem.font_size, 1.0);
    let Ok(pixmap) = cache.surface(key, tint, elem.color) else {
        return false;
    };
    if img.background {
        fill(target, rect.x, rect.y, rect.w, rect.h, theme.background);
    }
    composite(target, pixmap, rect);
    true
}

/// One decoded pixmap into one box, scaled to fill it.
fn composite(target: &mut PixmapMut<'_>, pixmap: &Pixmap, rect: SceneRect) {
    let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(box_) = Rect::from_xywh(rect.x, rect.y, rect.w, rect.h) else { return };
    let place = Transform::from_scale(rect.w / w, rect.h / h).post_translate(rect.x, rect.y);
    // `Pad` rather than `Repeat`: a bilinear sample at the box's own edge
    // reaches half a texel past it, and wrapping would fetch the far side
    // of the asset - a bright seam down one edge of every gaiji.
    let paint = Paint {
        shader: Pattern::new(
            pixmap.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            1.0,
            place,
        ),
        anti_alias: true,
        ..Paint::default()
    };
    target.fill_rect(box_, &paint, Transform::identity(), None);
}

/// The last rung: an outlined box where the character should be.
///
/// Outlined and not filled, because a solid block reads as a censored
/// glyph while an empty frame reads as a missing one - and because the
/// asset it stands in for is usually a character drawn in this very
/// colour.
fn placeholder(target: &mut PixmapMut<'_>, rect: SceneRect, color: Rgb, scale: f32) {
    let edge = hairline(scale);
    if rect.w <= 2.0 * edge || rect.h <= 2.0 * edge {
        // Too small to outline: a filled speck is still visibly ink.
        fill(target, rect.x, rect.y, rect.w, rect.h, color);
        return;
    }
    fill(target, rect.x, rect.y, rect.w, edge, color);
    fill(target, rect.x, rect.y + rect.h - edge, rect.w, edge, color);
    fill(target, rect.x, rect.y, edge, rect.h, color);
    fill(target, rect.x + rect.w - edge, rect.y, edge, rect.h, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::dict::media::{MediaFormat, MediaKey};
    use chibipop::ui::layout::{AnkiSlot, Appearance, BorderStyle, BoxStyle, Edges};
    use chibipop::ui::layout::{ElemSpan, GlyphBox, LineBox, MarkerBox, RubyBox};
    use chibipop::ui::layout::{MeasureError, Metrics, SidePanel, SideRow, SpanBox, TextMeasure};
    use tiny_skia::Pixmap;

    /// One run the painter asked for.
    ///
    /// The whole span list, not the first span: an element carrying
    /// mixed styling must reach the engine whole, and a bin that
    /// painted only its head would still pass every assertion about
    /// its head.
    #[derive(Debug, Clone, PartialEq)]
    struct Recorded {
        spans: Vec<RecordedSpan>,
        max_w: f32,
        origin: (f32, f32),
    }

    /// One span of one recorded run.
    #[derive(Debug, Clone, PartialEq)]
    struct RecordedSpan {
        text: String,
        size: f32,
        weight: u16,
        italic: bool,
        color: Rgb,
        shift: f32,
    }

    impl Recorded {
        /// What a reader would see.
        fn text(&self) -> String {
            self.spans.iter().map(|s| s.text.as_str()).collect()
        }

        /// Its one span, for a run that
        /// carries exactly one.
        fn one(&self) -> &RecordedSpan {
            assert_eq!(1, self.spans.len(), "expected a single-span run");
            &self.spans[0]
        }
    }

    /// Fixed metrics and a record of what was drawn: the same trick
    /// core's layout tests use, so the painter needs no font stack.
    #[derive(Default)]
    struct Fake {
        runs: Vec<Recorded>,
        /// Every measurement fails: the no-JP-font degrade path.
        broken: bool,
    }

    impl TextMeasure for Fake {
        fn measure(
            &mut self,
            run: MeasureRun<'_>,
            out: &mut Measured,
        ) -> Result<(), MeasureError> {
            out.clear();
            if self.broken {
                return Err(MeasureError::new("no fonts"));
            }
            // Half an em per character, laid end to end, and a line
            // broken whenever the whole run would not fit: enough
            // geometry for the painter's assertions, no font stack.
            let mut x = 0.0f32;
            for (i, span) in run.spans.iter().enumerate() {
                let w = span.text.chars().count() as f32 * span.size * 0.5;
                out.spans.push(SpanBox {
                    span: i as u32,
                    line: 0,
                    x,
                    w,
                    h: span.size * 1.4,
                });
                x += w;
            }
            let lines = if run.max_w > 0.0 { (x / run.max_w).ceil().max(1.0) } else { 1.0 };
            let h = out.spans.iter().fold(0.0f32, |a, b| a.max(b.h)).max(1.4);
            for line in 0..lines as u32 {
                out.lines.push(LineBox {
                    y: line as f32 * h,
                    w: x.min(run.max_w.max(1.0)),
                    h,
                    baseline: h,
                });
            }
            out.metrics = Metrics { w: x, h: lines * h, lines: lines as u32 };
            Ok(())
        }

        fn caret_boxes(
            &mut self,
            run: MeasureRun<'_>,
            at: &[u32],
            out: &mut Vec<GlyphBox>,
        ) -> Result<(), MeasureError> {
            let size = run.spans.first().map_or(0.0, |s| s.size);
            let adv = size * 0.5;
            out.extend(at.iter().map(|i| GlyphBox {
                x: *i as f32 * adv,
                y: 0.0,
                w: adv,
                h: size * 1.4,
            }));
            Ok(())
        }
    }

    impl PanelText for Fake {
        fn draw_run(&mut self, run: DrawRun<'_>, target: &mut PixmapMut<'_>) {
            self.runs.push(Recorded {
                spans: run
                    .spans
                    .iter()
                    .enumerate()
                    .map(|(i, s)| RecordedSpan {
                        text: s.text.to_string(),
                        size: s.size,
                        weight: s.weight,
                        italic: s.italic,
                        color: s.color,
                        shift: run.shifts.get(i).copied().unwrap_or(0.0),
                    })
                    .collect(),
                max_w: run.max_w,
                origin: run.origin,
            });
            // One opaque pixel at the pen, so a test can see that a
            // run reached the buffer at all.
            let (x, y) = run.origin;
            if x < 0.0 || y < 0.0 || x >= target.width() as f32 || y >= target.height() as f32 {
                return;
            }
            let i = (y as usize * target.width() as usize + x as usize) * 4;
            target.data_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }

    /// The premultiplied value a solid channel leaves behind after the
    /// fade.
    fn faded(channel: u8) -> u8 {
        let prod = u32::from(channel) * u32::from(PANEL_ALPHA) + 128;
        ((prod + (prod >> 8)) >> 8) as u8
    }

    /// The premultiplied red channel a solid `color` leaves behind
    /// after the fade.
    fn faded_red(color: Rgb) -> u8 {
        faded(color.0)
    }

    fn text_elem(text: &str, pen: (f32, f32), align: Align) -> SceneElem {
        styled_elem(pen, align, &[(text, 15.0, 400, false, 0.0)])
    }

    /// An element carrying one span per
    /// `(text, size, weight, italic, shift)`.
    fn styled_elem(
        pen: (f32, f32),
        align: Align,
        parts: &[(&str, f32, u16, bool, f32)],
    ) -> SceneElem {
        let mut text = String::new();
        let mut spans = Vec::new();
        for &(part, size, weight, italic, shift) in parts {
            spans.push(ElemSpan {
                at: text.len() as u32,
                len: part.len() as u32,
                color: (210, 212, 218),
                size,
                weight,
                italic,
                shift,
            });
            text.push_str(part);
        }
        let head = spans[0];
        SceneElem {
            kind: ElemKind::Text,
            text,
            color: head.color,
            font_size: head.size,
            weight: head.weight,
            italic: head.italic,
            top_gap: 0.0,
            wrap_w: 176.0,
            align,
            pen,
            rect: SceneRect { x: pen.0, y: pen.1, w: 100.0, h: 20.0 },
            lines: 1,
            advance: 20.0,
            spans,
            ruby: Vec::new(),
            marker: Vec::new(),
            block_box: None,
            inline_boxes: Vec::new(),
            origin: None,
            image: None,
        }
    }

    /// A 200x100 body: one run, nothing else.
    fn plain_scene() -> PopupScene {
        PopupScene {
            origin: 12.0,
            content_w: 176.0,
            elems: vec![text_elem("gloss", (12.0, 12.0), Align::Leading)],
            hits: Vec::new(),
            side: None,
            anki: None,
            used_h: 40.0,
            content_h: 100.0,
            view_h: 100.0,
            panel_w: None,
        }
    }

    fn painter<'a>(scene: &'a PopupScene, theme: &'a Theme, scroll: f32, scale: f32) -> Panel<'a> {
        Panel { scene, theme, scroll, scale }
    }

    #[test]
    fn the_rounded_rect_is_the_silhouette_and_the_whole_frame_carries_panel_alpha() {
        let theme = Theme::dark();
        let scene = plain_scene();
        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let centre = pix.pixel(100, 50).unwrap();
        assert_eq!(PANEL_ALPHA, centre.alpha(), "the panel fades, it is never opaque");
        assert_eq!(faded_red(theme.background), centre.red());

        let corner = pix.pixel(0, 0).unwrap();
        assert_eq!(0, corner.alpha(), "outside the corner radius nothing is drawn at all");
    }

    #[test]
    fn the_hide_frame_leaves_every_pixel_zero() {
        let mut pix = Pixmap::new(8, 4).unwrap();
        pix.fill(Color::from_rgba8(9, 9, 9, 255));
        transparent(&mut pix.as_mut());
        assert!(pix.data().iter().all(|b| *b == 0));
    }

    #[test]
    fn the_surface_is_taller_than_the_view_only_when_an_anki_slot_exists() {
        let mut scene = plain_scene();
        assert_eq!(100.0, surface_height(&scene));
        scene.anki = Some(AnkiSlot {
            label: "Add to Anki".to_string(),
            color: (130, 170, 220),
            rect: SceneRect { x: 0.0, y: 100.0, w: 200.0, h: 28.0 },
        });
        assert_eq!(128.0, surface_height(&scene));
    }

    #[test]
    fn the_trailing_corner_run_is_drawn_at_its_ink_box_not_at_the_pen() {
        let theme = Theme::dark();
        let mut corner = text_elem("F12", (12.0, 20.0), Align::Trailing);
        corner.kind = ElemKind::Corner;
        corner.rect = SceneRect { x: 150.0, y: 20.0, w: 38.0, h: 16.0 };
        let mut scene = plain_scene();
        scene.elems = vec![corner];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let run = text.runs.first().expect("the corner run must be drawn");
        assert_eq!(150.0, run.origin.0, "the scene already right-aligned this box");
        assert_eq!(38.0, run.max_w, "and its width is the box, not the wrap width");
    }

    /// A box carrying nothing but a style with no ink.
    fn elem_box(rect: SceneRect, style: BoxStyle) -> ElemBox {
        ElemBox { rect, style }
    }

    /// A pill's fill and its border both reach the buffer, and the fill
    /// is *under* the text: the box pass runs before the run is drawn.
    #[test]
    fn a_pill_paints_its_fill_and_its_border_under_the_text() {
        let theme = Theme::dark();
        let mut elem = text_elem("noun", (40.0, 40.0), Align::Leading);
        elem.inline_boxes = vec![elem_box(
            SceneRect { x: 30.0, y: 30.0, w: 60.0, h: 40.0 },
            BoxStyle {
                background: Some((255, 0, 0)),
                border: Edges::all(4.0),
                border_style: Edges::all(BorderStyle::Solid),
                border_color: (0, 255, 0),
                radius: 6.0,
                ..BoxStyle::default()
            },
        )];
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        // Inside the border, away from every corner: the fill.
        let inside = pix.pixel(60, 50).unwrap();
        assert_eq!(faded_red((255, 0, 0)), inside.red(), "the fill covers the box");
        assert_eq!(0, inside.green(), "and it is the fill's colour, not the border's");
        // On the border, two pixels in from the left edge.
        let edge = pix.pixel(32, 50).unwrap();
        assert_eq!(faded(255), edge.green(), "the border strokes inside the box");
        assert_eq!(0, edge.red(), "over the fill, not blended with it");
        // Outside it, the panel's own background.
        let outside = pix.pixel(20, 50).unwrap();
        assert_eq!(faded_red(theme.background), outside.red());
        assert_eq!(1, text.runs.len(), "and the text is still drawn, once");
    }

    /// A radius is a radius: the corner of a rounded box is not filled.
    #[test]
    fn a_rounded_box_leaves_its_corners_unfilled() {
        let theme = Theme::dark();
        let mut elem = text_elem("x", (40.0, 40.0), Align::Leading);
        let rect = SceneRect { x: 30.0, y: 30.0, w: 60.0, h: 40.0 };
        elem.block_box =
            Some(elem_box(rect, BoxStyle { background: Some((255, 0, 0)), radius: 12.0,
                ..BoxStyle::default() }));
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!(faded_red((255, 0, 0)), pix.pixel(60, 50).unwrap().red(), "the middle fills");
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(31, 31).unwrap().red(),
            "a 12px radius clears the top-left corner"
        );
    }

    /// `border-style: none none none solid` is a left rule and nothing
    /// else: the asymmetric path fills one edge, not four.
    #[test]
    fn a_one_sided_border_paints_only_that_side() {
        let theme = Theme::dark();
        let mut elem = text_elem("note", (40.0, 40.0), Align::Leading);
        elem.block_box = Some(elem_box(
            SceneRect { x: 30.0, y: 30.0, w: 60.0, h: 40.0 },
            BoxStyle {
                border: Edges::all(4.0),
                border_style: Edges {
                    top: BorderStyle::None,
                    right: BorderStyle::None,
                    bottom: BorderStyle::None,
                    left: BorderStyle::Solid,
                },
                border_color: (0, 255, 0),
                ..BoxStyle::default()
            },
        ));
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!(faded(255), pix.pixel(31, 50).unwrap().green(), "the left rule");
        let right = pix.pixel(88, 50).unwrap();
        assert_eq!(faded(theme.background.1), right.green(), "and no right one");
    }

    /// A box's rect is unscrolled panel space, like every other rect the
    /// scene reports, so the box pass applies the same scroll the text
    /// does - or the border would slide off the paragraph it frames.
    #[test]
    fn a_scrolled_box_moves_with_the_text_it_frames() {
        let theme = Theme::dark();
        let mut elem = text_elem("x", (12.0, 60.0), Align::Leading);
        elem.block_box = Some(elem_box(
            SceneRect { x: 30.0, y: 60.0, w: 60.0, h: 20.0 },
            BoxStyle { background: Some((255, 0, 0)), ..BoxStyle::default() },
        ));
        let mut scene = plain_scene();
        scene.elems = vec![elem];
        scene.content_h = 200.0;

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 40.0, 1.0), &mut text, None, &mut pix.as_mut());

        let run = text.runs.first().expect("the run is drawn scrolled");
        assert_eq!(20.0, run.origin.1, "the text moved up by the scroll");
        assert_eq!(
            faded_red((255, 0, 0)),
            pix.pixel(60, 25).unwrap().red(),
            "and so did the box that frames it"
        );
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(60, 65).unwrap().red(),
            "leaving nothing where it was"
        );
    }

    /// A block's box is a container element that **leads** the
    /// paragraphs it frames, so its fill has to land under a *later*
    /// element's text and not only under its own. Built through
    /// `layout::scene` rather than by hand, because what this proves is
    /// that the two passes agree about draw order on the shape that
    /// used to draw no box at all: a bordered, filled `div` whose first
    /// child carries a `data.content` marker.
    #[test]
    fn a_blocks_box_paints_under_the_paragraphs_that_follow_it() {
        let theme = Theme::dark();
        let body = concat!(
            r##"[{"type":"structured-content","content":"##,
            r##"{"tag":"div","style":{"padding":0.2,"backgroundColor":"#ff0000"},"##,
            r##""content":[{"tag":"span","data":{"content":"misc-info"},"content":"one"},"##,
            r##"{"tag":"div","content":"two"}]}}]"##
        );
        let p = chibipop::present::Presentation {
            top: Some(chibipop::present::Card {
                written: None,
                reading: None,
                pos: Vec::new(),
                freq: None,
                blocks: vec![chibipop::present::GlossBlock::parse("d", body)],
                match_len: 1,
            }),
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: None,
        };
        let mut text = Fake::default();
        let scene = chibipop::ui::layout::scene(
            &chibipop::ui::layout::SceneRequest {
                presentation: &p,
                theme: &theme,
                max_w: 200.0,
                max_h: 400.0,
                show_back: false,
                side_panel: false,
                render: chibipop::ui::layout::RenderSettings::default(),
                anki: None,
            },
            &mut text,
        )
        .expect("the fake never refuses a run");

        let boxes: Vec<&SceneElem> =
            scene.elems.iter().filter(|e| e.kind == ElemKind::Block).collect();
        assert_eq!(1, boxes.len(), "one boxed block, one container element");
        let frame = boxes[0].block_box.expect("a container always carries its box");
        let at = scene
            .elems
            .iter()
            .position(|e| e.kind == ElemKind::Block)
            .expect("the container is in the scene");
        let after: Vec<&SceneElem> = scene.elems[at + 1..]
            .iter()
            .filter(|e| e.kind == ElemKind::Text)
            .collect();
        assert_eq!(2, after.len(), "two paragraphs, both after the box");

        let mut pix = Pixmap::new(200, 400).unwrap();
        text.runs.clear();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        // The fill is under both paragraphs' pens, and it is the box's
        // colour rather than the panel's.
        for para in &after {
            let (x, y) = (para.pen.0 as u32 + 1, para.pen.1 as u32 + 1);
            let px = pix.pixel(x, y).unwrap();
            assert_eq!(
                faded_red((255, 0, 0)),
                px.red(),
                "the fill covers the paragraph at ({x}, {y})"
            );
        }
        // And it stops where the border box stops.
        let below = (frame.rect.y + frame.rect.h + 2.0) as u32;
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(frame.rect.x as u32 + 4, below).unwrap().red(),
            "and nothing under it"
        );
        // Each boxed paragraph drawn once, and the box drew none of
        // them twice: the container is textless.
        for para in &after {
            let drawn = text.runs.iter().filter(|r| r.text() == para.text).count();
            assert_eq!(1, drawn, "{:?} drawn once", para.text);
        }
    }

    /// The scene decides the weight.
    ///
    /// A run painted in a weight core
    /// did not measure it in would be
    /// wrapped one way and drawn
    /// another, and CSS emphasis would
    /// reach the panel not at all. The
    /// weight is the *span's*, not the
    /// element's: that is what lets one
    /// element hold two of them.
    #[test]
    fn a_run_is_drawn_in_the_weight_and_style_the_scene_measured_it_in() {
        let theme = Theme { collapsed_weight: 200, collapsed_italic: true, ..Theme::dark() };
        let elem = styled_elem(
            (12.0, 12.0),
            Align::Leading,
            &[("gloss", 15.0, 700, true, 0.0)],
        );
        let mut scene = plain_scene();
        scene.elems = vec![elem];
        scene.side = Some(SidePanel {
            origin_y: 12.0,
            rule_x: 140.0,
            rule_w: 1.0,
            col_x: 152.0,
            col_w: 42.0,
            height: 60.0,
            rows: vec![SideRow {
                idx: Some(0),
                text: "See also".to_string(),
                color: theme.dimmed_text,
                y: 0.0,
                h: 18.0,
            }],
        });

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let gloss = text.runs.first().expect("the gloss must be drawn");
        assert_eq!(700, gloss.one().weight);
        assert!(gloss.one().italic);
        // The side column is one
        // format, the collapsed role's.
        let side = text.runs.last().expect("a side row must be drawn");
        assert_eq!(theme.collapsed_weight, side.one().weight);
        assert!(side.one().italic);
    }

    /// The whole span list reaches the engine, as one run.
    ///
    /// A bin that handed over only the first span would still draw
    /// something, and the something would be a paragraph with its
    /// emphasis, its colours and its raised marks silently missing.
    #[test]
    fn every_span_of_an_element_reaches_the_engine_in_one_run() {
        let theme = Theme::dark();
        let mut scene = plain_scene();
        scene.elems = vec![styled_elem(
            (12.0, 12.0),
            Align::Leading,
            &[
                ("plain ", 15.0, 400, false, 0.0),
                ("bold", 15.0, 700, false, 0.0),
                ("1", 12.5, 400, false, 5.0),
            ],
        )];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!(1, text.runs.len(), "one run, not one per span");
        let run = &text.runs[0];
        assert_eq!("plain bold1", run.text());
        assert_eq!(
            vec![(400, 15.0, 0.0), (700, 15.0, 0.0), (400, 12.5, 5.0)],
            run.spans.iter().map(|s| (s.weight, s.size, s.shift)).collect::<Vec<_>>()
        );
        assert_eq!(176.0, run.max_w, "and one wrap width for the paragraph");
    }

    /// The reading is drawn, and drawn where the scene put it.
    ///
    /// A bin that painted only `spans` would draw the base and silently
    /// lose the furigana - the same deletion ticket 11 closes in the
    /// layout, one layer further out. It is a second run and not a span of
    /// the first, because a reading takes no horizontal room from the line
    /// it sits on.
    #[test]
    fn a_reading_is_drawn_over_its_base_where_the_scene_placed_it() {
        let theme = Theme::dark();
        let mut elem =
            styled_elem((12.0, 12.0), Align::Leading, &[("猫", 15.0, 400, false, 0.0)]);
        elem.ruby = vec![RubyBox {
            text: "ねこ".into(),
            x: 1.875,
            y: 0.0,
            w: 7.5,
            h: 15.0,
            size: 7.5,
            color: (9, 8, 7),
            weight: 400,
            italic: false,
        }];
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!(2, text.runs.len(), "the base, then its reading");
        assert_eq!("猫", text.runs[0].text());

        let read = &text.runs[1];
        assert_eq!("ねこ", read.text());
        assert_eq!((13.875, 12.0), read.origin, "the element's pen plus the offset");
        assert_eq!(7.5, read.one().size);
        assert_eq!((9, 8, 7), read.one().color);
        assert_eq!(0.0, read.one().shift, "a reading is placed, not shifted");
        assert_eq!(176.0, read.max_w, "at the width the scene measured it at");
    }

    /// The list marker is drawn, and drawn where the scene put it.
    ///
    /// A bin that painted only `spans` would draw the item and silently
    /// lose its bullet. It is a second run and not a span of the first
    /// for the reason ticket 09 hangs it at all: the marker box sits
    /// beside the item's principal box, so every line of the item -
    /// including a continuation - starts at the item's own indent.
    #[test]
    fn a_list_marker_is_drawn_in_the_gutter_where_the_scene_placed_it() {
        let theme = Theme::dark();
        let mut elem =
            styled_elem((33.0, 12.0), Align::Leading, &[("to eat", 15.0, 400, false, 0.0)]);
        elem.marker = vec![MarkerBox {
            text: "\u{2022} ".into(),
            x: -15.0,
            y: 0.0,
            w: 15.0,
            h: 30.0,
            size: 15.0,
            color: (9, 8, 7),
            weight: 400,
            italic: false,
        }];
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!(2, text.runs.len(), "the item, then its marker");
        assert_eq!("to eat", text.runs[0].text());
        assert_eq!((33.0, 12.0), text.runs[0].origin, "the item's own indent");

        let mark = &text.runs[1];
        assert_eq!("\u{2022} ", mark.text());
        assert_eq!((18.0, 12.0), mark.origin, "left of the item, on its first line");
        assert_eq!((9, 8, 7), mark.one().color);
        assert_eq!(0.0, mark.one().shift, "a marker is placed, not shifted");
    }

    /// Colour is per span, not per element.
    #[test]
    fn each_span_paints_in_its_own_colour() {
        let theme = Theme::dark();
        let mut elem = styled_elem(
            (12.0, 12.0),
            Align::Leading,
            &[("black", 15.0, 400, false, 0.0), ("red", 15.0, 400, false, 0.0)],
        );
        elem.spans[1].color = (255, 0, 0);
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let colors: Vec<Rgb> = text.runs[0].spans.iter().map(|s| s.color).collect();
        assert_eq!(vec![(210, 212, 218), (255, 0, 0)], colors);
    }

    #[test]
    fn the_thumb_appears_only_on_overflow_and_its_width_follows_the_scale() {
        let theme = Theme::dark();
        let bg = faded_red(theme.background);
        let thumb = faded_red(theme.dimmed_text);

        // track_h 76, view 100: 40px of content fits, 300 does not.
        let mut short = plain_scene();
        short.used_h = 40.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&short, &theme, 0.0, 1.0), &mut Fake::default(), None, &mut pix.as_mut());
        assert_eq!(bg, pix.pixel(192, 20).unwrap().red(), "content that fits gets no thumb");

        let mut tall = plain_scene();
        tall.used_h = 300.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 0.0, 1.0), &mut Fake::default(), None, &mut pix.as_mut());
        // 4px wide, right edge at 200 - padding/2.
        assert_eq!(thumb, pix.pixel(192, 20).unwrap().red());
        assert_eq!(bg, pix.pixel(187, 20).unwrap().red(), "a 1x bar is four pixels wide");

        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 0.0, 2.0), &mut Fake::default(), None, &mut pix.as_mut());
        assert_eq!(thumb, pix.pixel(187, 20).unwrap().red(), "at 2x it is eight");
    }

    #[test]
    fn the_anki_label_is_centred_over_its_rule() {
        let theme = Theme::dark();
        let mut scene = plain_scene();
        scene.anki = Some(AnkiSlot {
            label: "Add to Anki".to_string(),
            color: (130, 170, 220),
            rect: SceneRect { x: 0.0, y: 100.0, w: 200.0, h: 28.0 },
        });

        let mut pix = Pixmap::new(200, 128).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let label = text.runs.last().expect("the label must be drawn");
        assert_eq!("Add to Anki", label.text());
        // 11 chars at 13px measures 71.5 wide in the fake.
        assert_eq!(64.25, label.origin.0);
        assert_eq!(100.0, label.origin.1);
        assert_eq!(
            faded_red(theme.separator),
            pix.pixel(120, 100).unwrap().red(),
            "the strip is fenced off from the body"
        );
    }

    #[test]
    fn a_measurer_that_refuses_costs_the_centring_and_not_the_frame() {
        let theme = Theme::dark();
        let mut scene = plain_scene();
        scene.anki = Some(AnkiSlot {
            label: "Add to Anki".to_string(),
            color: (130, 170, 220),
            rect: SceneRect { x: 6.0, y: 100.0, w: 188.0, h: 28.0 },
        });

        let mut pix = Pixmap::new(200, 128).unwrap();
        let mut text = Fake { broken: true, ..Fake::default() };
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let label = text.runs.last().expect("the label is still drawn");
        assert_eq!(6.0, label.origin.0, "left-aligned, not half-centred on a zero width");
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(100, 60).unwrap().red(),
            "the panel itself painted"
        );
    }

    #[test]
    fn the_side_rule_divides_the_body_view_and_stops_above_the_anki_strip() {
        let theme = Theme::dark();
        let mut scene = plain_scene();
        scene.side = Some(SidePanel {
            origin_y: 12.0,
            rule_x: 140.0,
            rule_w: 1.0,
            col_x: 152.0,
            col_w: 42.0,
            height: 60.0,
            rows: vec![SideRow {
                idx: Some(0),
                text: "See also".to_string(),
                color: theme.collapsed_text,
                y: 0.0,
                h: 18.0,
            }],
        });
        scene.anki = Some(AnkiSlot {
            label: "Add".to_string(),
            color: (130, 170, 220),
            rect: SceneRect { x: 0.0, y: 100.0, w: 200.0, h: 28.0 },
        });

        let mut pix = Pixmap::new(200, 128).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        let sep = faded_red(theme.separator);
        assert_eq!(sep, pix.pixel(140, 80).unwrap().red(), "the rule runs the panel's height");
        assert_ne!(sep, pix.pixel(140, 112).unwrap().red(), "but never into the Anki strip");
        assert!(
            text.runs.iter().any(|r| r.origin == (152.0, 12.0) && r.text() == "See also"),
            "the column's rows are drawn at the column"
        );
    }

    #[test]
    fn a_one_pixel_target_and_a_scene_scrolled_past_its_end_still_paint() {
        let theme = Theme::dark();
        let scene = plain_scene();
        let mut pix = Pixmap::new(1, 1).unwrap();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut Fake::default(), None, &mut pix.as_mut());

        let mut tall = plain_scene();
        tall.used_h = 900.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 9_000.0, 1.5), &mut Fake::default(), None, &mut pix.as_mut());
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(100, 50).unwrap().red(),
            "an overscrolled scene is an empty panel, not a missing one"
        );
    }

    // ---- inline images ----

    /// An image element carrying `key` and, optionally, an `alt` run.
    fn image_elem(pen: (f32, f32), w: f32, h: f32, alt: &str, img: SceneImage) -> SceneElem {
        let mut elem = styled_elem(pen, Align::Leading, &[(alt, 15.0, 400, false, 0.0)]);
        elem.kind = ElemKind::Image;
        elem.rect = SceneRect { x: pen.0, y: pen.1, w, h };
        elem.wrap_w = w.max(1.0);
        elem.advance = 0.0;
        elem.image = Some(img);
        if alt.is_empty() {
            elem.text.clear();
            elem.spans.clear();
        }
        elem
    }

    /// One asset, as the scene names it.
    fn scene_image(path: Option<&str>, format: Option<MediaFormat>) -> SceneImage {
        SceneImage {
            key: path.map(|p| MediaKey::new(1, p)),
            format,
            appearance: Appearance::Auto,
            background: false,
            collapsed: false,
            collapsible: false,
        }
    }

    /// A scene holding exactly one image element.
    fn image_scene(elem: SceneElem) -> PopupScene {
        PopupScene { elems: vec![elem], ..plain_scene() }
    }

    /// A database built from the committed media fixture archive, so the
    /// painter's composite is asserted against pixels a real build wrote.
    fn built_media(test_name: &str) -> (std::path::PathBuf, MediaDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_linux_paint_media");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let guard = MediaDbGuard(out.clone());
        let archive = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/media/media.zip");
        chibipop::dict::build::build(&[archive], &[], &out, &|_| {})
            .expect("the fixture builds");
        (out, guard)
    }

    struct MediaDbGuard(std::path::PathBuf);

    impl Drop for MediaDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The first rung: a stored, decodable asset composites into the box
    /// the scene resolved, and it is the asset's own pixels rather than
    /// the panel's background.
    ///
    /// The fixture is half-alpha pure blue, so the assertion is also the
    /// channel order: a bin that swapped red for blue would paint every
    /// gaiji in the wrong hue and still pass a "something was drawn" test.
    #[test]
    fn a_stored_asset_composites_into_its_resolved_box() {
        let theme = Theme::dark();
        let (db, _guard) = built_media("composite");
        let mut media = MediaSurfaces::open(&db).expect("the store opens");
        let scene = image_scene(image_elem(
            (40.0, 40.0),
            20.0,
            20.0,
            "",
            scene_image(Some("gaiji/one.png"), Some(MediaFormat::Png)),
        ));
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(
            &painter(&scene, &theme, 0.0, 1.0),
            &mut Fake::default(),
            Some(&mut media),
            &mut pix.as_mut(),
        );

        // Inside the box: the asset, blended over the panel and then
        // faded once with the frame. Half-alpha blue over the dark
        // background leaves a blue channel well above the background's.
        let inside = pix.pixel(50, 50).unwrap();
        let outside = pix.pixel(100, 50).unwrap();
        assert!(
            inside.blue() > outside.blue() + 20,
            "the asset's own blue must reach the buffer: {inside:?} against {outside:?}"
        );
        assert!(inside.red() <= outside.red(), "and not as red: {inside:?}");
    }

    /// The rung a corrupt asset takes, and the one this build can still
    /// reach now that every census format decodes: `gaiji/torn.png` is a
    /// valid 12x7 IHDR over an IDAT that is not a zlib stream, so the media
    /// row is real, the line is laid out from it, and the *painter* is the
    /// one that comes up empty. It draws the element's own `alt` run.
    #[test]
    fn an_undecodable_asset_falls_back_to_its_alt_run() {
        let theme = Theme::dark();
        let (db, _guard) = built_media("no_decoder");
        let mut media = MediaSurfaces::open(&db).expect("the store opens");
        let scene = image_scene(image_elem(
            (40.0, 40.0),
            20.0,
            20.0,
            "\u{5bfe}",
            scene_image(Some("gaiji/torn.png"), Some(MediaFormat::Png)),
        ));
        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(
            &painter(&scene, &theme, 0.0, 1.0),
            &mut text,
            Some(&mut media),
            &mut pix.as_mut(),
        );

        let run = text.runs.first().expect("the alt run must be drawn");
        assert_eq!("\u{5bfe}", run.text());
        assert_eq!((40.0, 40.0), run.origin, "at the image's own box");
    }

    /// The last rung: nothing stored and no `alt` is an outlined box, not
    /// a gap. A hole in a word is the failure this ladder exists to
    /// prevent.
    #[test]
    fn a_missing_asset_with_no_alt_is_an_outlined_box_and_never_a_gap() {
        let theme = Theme::dark();
        let scene = image_scene(image_elem((40.0, 40.0), 20.0, 20.0, "", scene_image(None, None)));
        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert!(text.runs.is_empty(), "no text to draw, and no empty run through the seam");
        let ink = faded_red((210, 212, 218));
        let bg = faded_red(theme.background);
        assert_eq!(ink, pix.pixel(45, 40).unwrap().red(), "the top edge is ink");
        assert_eq!(ink, pix.pixel(45, 59).unwrap().red(), "and the bottom edge");
        assert_eq!(ink, pix.pixel(40, 50).unwrap().red(), "and the left");
        assert_eq!(ink, pix.pixel(59, 50).unwrap().red(), "and the right");
        assert_eq!(bg, pix.pixel(50, 50).unwrap().red(), "outlined, not filled");
    }

    /// A session with no readable dictionary database still paints, with
    /// every image on the `alt` rung: `None` is a state, not a failure.
    #[test]
    fn no_media_store_still_paints_the_alt_text() {
        let theme = Theme::dark();
        let scene = image_scene(image_elem(
            (40.0, 40.0),
            20.0,
            20.0,
            "[\u{5bfe}]",
            scene_image(Some("gaiji/one.png"), Some(MediaFormat::Png)),
        ));
        let mut pix = Pixmap::new(200, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!("[\u{5bfe}]", text.runs.first().expect("the alt run").text());
    }

    /// Story 17 through the painter: a monochrome asset reaches the buffer
    /// in the element's own colour, which is the theme's body text - so a
    /// gaiji authored as black ink is visible on a dark panel.
    #[test]
    fn a_monochrome_asset_paints_in_the_elements_own_colour() {
        let theme = Theme::dark();
        let (db, _guard) = built_media("tinted_paint");
        let mut media = MediaSurfaces::open(&db).expect("the store opens");
        let mut elem = image_elem(
            (40.0, 40.0),
            20.0,
            20.0,
            "",
            scene_image(Some("gaiji/one.png"), Some(MediaFormat::Png)),
        );
        // A mask, in a colour nothing like the asset's own blue.
        elem.color = (255, 0, 0);
        elem.image.as_mut().unwrap().appearance = Appearance::Monochrome;
        let scene = image_scene(elem);
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(
            &painter(&scene, &theme, 0.0, 1.0),
            &mut Fake::default(),
            Some(&mut media),
            &mut pix.as_mut(),
        );

        let inside = pix.pixel(50, 50).unwrap();
        let outside = pix.pixel(100, 50).unwrap();
        assert!(
            inside.red() > outside.red() + 20,
            "the tint colour must reach the buffer: {inside:?} against {outside:?}"
        );
        assert!(
            inside.blue() <= outside.blue(),
            "and the asset's own blue must be gone: {inside:?}"
        );
    }

    /// The scene's rects are unscrolled, so an image moves with its
    /// paragraph - the same rule every other rect the scene reports
    /// follows.
    #[test]
    fn an_image_moves_by_the_scroll_like_every_other_rect() {
        let theme = Theme::dark();
        let mut scene =
            image_scene(image_elem((40.0, 60.0), 20.0, 20.0, "", scene_image(None, None)));
        scene.used_h = 400.0;
        scene.content_h = 400.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&scene, &theme, 30.0, 1.0), &mut Fake::default(), None, &mut pix.as_mut());

        let ink = faded_red((210, 212, 218));
        assert_eq!(ink, pix.pixel(45, 30).unwrap().red(), "the box moved up by the scroll");
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(45, 60).unwrap().red(),
            "and is no longer where it was"
        );
    }
}

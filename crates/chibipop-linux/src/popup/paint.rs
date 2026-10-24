//! This module paints one `PopupScene` into one premultiplied buffer.
//!
//! It matches the Windows Direct2D `paint_once` order with two deliberate differences.
//! Windows clips its window to a `CreateRoundRectRgn` and asks the compositor for
//! constant `LWA_ALPHA`.
//! A layer surface has neither feature.
//! The rounded rect *is* the silhouette, and everything outside it stays fully transparent.
//! The painter applies constant alpha per pixel with one final fade.
//! Corners therefore fade instead of receiving a hard clip.
//! This difference costs one pass over the buffer.
//!
//! The painter also paints the Anki affordance.
//! Windows uses a separate HWND because a `WS_EX_TRANSPARENT` popup cannot receive
//! a click.
//! The Linux panel serves the affordance from its input region.
//! The Core-reserved slot is therefore another strip in this buffer.
//!
//! All input here already uses physical pixels.
//! The scene uses the output's fractional scale, and the theme is `physical_theme`.
//! [`Panel::scale`] is *only* for lengths that the theme does not carry.
//! These lengths are hairlines and `SCROLLBAR_W`.
//!
//! This module does not handle channel order.
//! tiny-skia calls the buffer RGBA.
//! `wl_shm`'s `Argb8888` is BGRA little-endian.
//! The surface code reinterprets the buffer when it passes the pixmap onward.
//! Solid fills use one `Color` constructor.
//! The fade scales all four channels by the same factor.

use crate::popup::{DrawRun, PanelText, PANEL_ALPHA};
use chibipop::ui::layout::StyledSpan;
use chibipop::ui::layout::{Align, ElemBox, ElemKind, Measured, MeasureRun};
use chibipop::ui::layout::{HIGHLIGHT_ALPHA, PopupScene, Rgb, SceneElem, SceneImage, SceneRect};
use chibipop::ui::theme::{Theme, SCROLLBAR_W};
// The decoded-surface cache sits beside the other popup code on disk.
// The library entry point declares it instead of `popup/mod.rs`, so its tests can
// link against a built database.
// The package library name keeps this module compiled and its tests run exactly once.
use chibipop_linux::media::MediaSurfaces;
use tiny_skia::{Color, FillRule, FilterQuality, Paint, Path, PathBuilder, Pattern};
use tiny_skia::{Pixmap, PixmapMut, Rect, Shader, SpreadMode, Stroke, Transform};

/// Stores everything one frame needs.
pub struct Panel<'a> {
    pub scene: &'a PopupScene,
    /// This field stores the physical-pixel theme (`popup::physical_theme`).
    pub theme: &'a Theme,
    /// The scroll offset in physical pixels.
    pub scroll: f32,
    /// The output's fractional scale, used only for hairlines.
    pub scale: f32,
}

/// Paints one frame into `target`, whose size matches the surface size.
///
/// `media` is the painter's decoded-asset cache.
/// `None` is a valid state.
/// A session that cannot open this build's Dictionary still paints every image
/// through the `alt`-text rung of its ladder.
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

    // 1. Start with a transparent buffer, not `Clear(background)`.
    // The rounded rect has no window region to hide overdraw outside it.
    transparent(target);

    // 2/3. Paint the panel and its edge.
    // Inset the stroke path by half its width because a centered stroke at the buffer edge
    // loses its outer half.
    // Win32 can stroke the window rect itself and let the region handle the difference.
    let radius = theme.corner_radius as f32;
    if let Some(shape) = rounded(0.0, 0.0, w, h, radius) {
        let bg = solid(theme.background);
        target.fill_path(&shape, &bg, FillRule::Winding, Transform::identity(), None);
    }
    // The theme border already uses physical pixels and never becomes thinner than one pixel.
    // At the default 1.0 DIP, it matches the hairline that Windows strokes.
    let sw = theme.border_width.max(1.0);
    let inset = sw / 2.0;
    if let Some(edge) = rounded(inset, inset, w - sw, h - sw, radius - inset) {
        let stroke = Stroke { width: sw, ..Stroke::default() };
        target.stroke_path(&edge, &solid(theme.border), &stroke, Transform::identity(), None);
    }

    // 4. Draw selection highlights under every element.
    // The scene stores these boxes in panel space without a scroll offset.
    // Clip them to the body view so a highlight cannot paint the Anki strip.
    for highlight in &scene.highlights {
        let y = highlight.y - p.scroll;
        let top = y.max(0.0);
        let bottom = (y + highlight.h).min(scene.view_h);
        if bottom > top {
            fill_alpha(
                target,
                highlight.x,
                top,
                highlight.w,
                bottom - top,
                theme.accent,
                HIGHLIGHT_ALPHA,
            );
        }
    }

    // 5. Draw elements in scene order with the scroll offset applied.
    // Core already culls off-panel runs.
    // One paragraph remains one run despite its styles, so the painter uses the wrap
    // that the scene measured (ARCHITECTURE.md#popup-and-measurement).

    let mut spans: Vec<StyledSpan<'_>> = Vec::new();
    let mut shifts: Vec<f32> = Vec::new();
    for painted in scene.visible(p.scroll, scene.view_h) {
        let elem = painted.elem;
        if elem.kind == ElemKind::Separator {
            fill(target, elem.rect.x, painted.pen.1, elem.rect.w, elem.rect.h, elem.color);
            continue;
        }
        // Draw boxes before text.
        // A pill's fill sits behind its tinted label, and a block's border frames
        // the paragraph inside it.
        // The scene resolves every rect, so this walk makes no decisions and only draws.
        let dy = painted.pen.1 - elem.pen.1;
        for b in elem.boxes() {
            box_of(target, b, dy);
        }
        // Draw an image through its ladder: the asset if this build can decode it,
        // then the element's `alt` run, then an outlined box.
        // Never draw nothing because a gap would split a word.
        if let Some(img) = &elem.image {
            if asset(target, elem, img, painted.pen.1, media.as_deref_mut(), theme) {
                continue;
            }
            if elem.spans.is_empty() {
                placeholder(target, elem.rect_at(painted.pen.1), elem.color, p.scale);
                continue;
            }
        }
        // Do not shape an empty element.
        // An empty table cell draws its border but no text, and the seam receives no empty run.
        if elem.spans.is_empty() {
            continue;
        }
        // `Trailing` marks the frequency corner, and `textAlign` marks a gloss.
        // DirectWrite receives the whole wrap box and an alignment flag.
        // `DrawRun` has no alignment, so pass the ink box that the scene aligned.
        //
        // A one-line box paints the same pixels for every aligned box in the corpus:
        // a corner, an attribution, or a centered cell.
        // A *wrapped* aligned paragraph places shorter lines at the ink box's left edge,
        // not at a separate center position for each line.
        // `DrawRun` would need an alignment field for that behavior.
        // That change belongs to the Linux text seam, not the box model.
        //
        // Pass the run and nothing else.
        // A reading and a marker use run-relative positions, so both draw from the pen below.
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
        // Draw readings over the bases that the scene placed them against.
        // A reading is not a span of the main run.
        // It takes no horizontal space from its line.
        // Draw it at the scene's position with the element's measured width.
        //
        // Use `painted.pen`, not `origin`, for the same reason as markers.
        // [`RubyBox::x`] is run-relative and already includes the line's alignment slack.
        // If the aligned element's ink box supplied the origin, that slack would apply twice.
        // The furigana would then move away from the panel.
        for run in &elem.ruby {
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
        // Draw list markers in the gutters that their lists open.
        // Use `painted.pen` for markers too.
        // A marker box hangs from the item's *content* edge, which is the pen,
        // regardless of text alignment.
        // Every item line and each continuation therefore starts at its own indent.
        // `textAlign` moves line boxes, but a marker box is not a line box.
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

    // 5. Draw the "See also" column.
    // Its rule divides the whole panel, not only the column.
    // It reaches the bottom padding of the body view.
    // Windows uses the window as the body view, but this surface stops above the Anki strip.
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

    // 6. Draw the thumb, not a track.
    // Core does this arithmetic in whole pixels.
    // The scene rounds f32 heights on input.
    // Core derives the track and content height from the scene itself.
    // The Linux painter and Windows painter therefore agree about the padding.
    let pad = theme.padding;
    let view = scene.view_h.round() as i32;
    let at = p.scroll.round() as i32;
    if let Some((top, thumb_h)) = scene.scrollbar_thumb(pad, view, at) {
        // The painter scales only this length.
        // `SCROLLBAR_W` is a logical constant, unlike every length in `theme`.
        let bar_w = (SCROLLBAR_W as f32 * p.scale).round().max(1.0);
        let x = w - (pad / 2) as f32 - bar_w;
        fill(target, x, (pad + top) as f32, bar_w, thumb_h as f32, theme.dimmed_text);
    }

    // 7. Draw the Anki strip: its top rule, then its centered label.
    // The label needs a measurement to find its center, so `PanelText` extends `TextMeasure`.
    // If the measurer refuses, the painter loses the center position but keeps the frame.
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

    // 8. Apply Windows' constant window alpha to every pixel.
    fade(target, PANEL_ALPHA);
}

/// Creates the span for the collapsed role that the side column and Anki label use.
///
/// Neither span comes from a `SceneElem`, so neither has a span list.
/// Core provides their text and color, and the role provides the other fields.
/// This matches the shape that `layout::side_span` measures.
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

/// Creates the hide frame as a fully transparent buffer.
///
/// The surface never unmaps, so Hyprland's layer animation never fires and show/hide
/// stays instant.
/// Zeroed bytes are transparent premultiplied pixels in every channel order.
/// A zero fill also avoids `pixels_mut`'s cast of a buffer that this module did not size.
pub fn transparent(target: &mut PixmapMut<'_>) {
    let n = pixel_bytes(target);
    target.data_mut()[..n].fill(0);
}

/// Returns the total surface height for a scene: the body view plus the Anki strip.
///
/// One surface holds both.
/// Windows uses a second window for the affordance, so its popup height is only the view.
/// Placement needs this total, not `view_h`.
pub fn surface_height(scene: &PopupScene) -> f32 {
    scene.view_h + scene.anki.as_ref().map_or(0.0, |a| a.rect.h)
}

/// Returns the physical equivalent of a Windows 1.0 DIP stroke.
/// The result is never thinner than one pixel because antialiasing turns a thin border into a smudge.
fn hairline(scale: f32) -> f32 {
    scale.max(1.0)
}

/// Returns the number of bytes that `target` uses for pixels.
///
/// `PixmapMut::from_bytes` accepts an oversized slice.
/// An shm pool provides a slice sized by its own stride.
/// Whole-buffer loops therefore use the dimensions, not the slice length.
fn pixel_bytes(target: &PixmapMut<'_>) -> usize {
    target.width() as usize * target.height() as usize * 4
}

/// Scales every channel of every pixel by `alpha/255`.
///
/// This function edits bytes instead of `pixels_mut`.
/// The premultiplied constructor that skips the `rgb <= a` check is crate-private
/// in tiny-skia.
/// The check is unnecessary because one monotonic factor applied to all four channels
/// preserves the invariant and does not depend on channel order.
fn fade(target: &mut PixmapMut<'_>, alpha: u8) {
    let n = pixel_bytes(target);
    let a = u32::from(alpha);
    for b in &mut target.data_mut()[..n] {
        // Skia rounds `c * a / 255` without a divide.
        let prod = u32::from(*b) * a + 128;
        *b = ((prod + (prod >> 8)) >> 8) as u8;
    }
}

/// Creates opaque paint.
/// [`fade`] applies translucency once at the end.
/// A translucent fill here would blend against the frame below it and stack alpha.
fn solid((r, g, b): Rgb) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(Color::from_rgba8(r, g, b, 255)),
        ..Paint::default()
    }
}

/// Fills one box, or returns when its dimensions are degenerate.
/// `Rect` rejects non-finite edges, so the function handles every input.
fn fill(target: &mut PixmapMut<'_>, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(rect) = Rect::from_xywh(x, y, w, h) else { return };
    target.fill_rect(rect, &solid(color), Transform::identity(), None);
}

/// Create paint with one source alpha.
fn translucent((r, g, b): Rgb, alpha: f32) -> Paint<'static> {
    let color = Color::from_rgba(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        alpha.clamp(0.0, 1.0),
    )
    .expect("the highlight alpha is finite");
    Paint { shader: Shader::SolidColor(color), ..Paint::default() }
}

/// Fill one translucent box. Return when its dimensions are not positive.
fn fill_alpha(
    target: &mut PixmapMut<'_>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Rgb,
    alpha: f32,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(rect) = Rect::from_xywh(x, y, w, h) else { return };
    target.fill_rect(rect, &translucent(color, alpha), Transform::identity(), None);
}

/// This constant stores the cubic handle length for a circular-arc approximation with radius 1.
const ARC: f32 = 0.552_284_8;

/// Creates a rounded rectangle path that matches D2D's elliptical corners.
///
/// Returns `None` for a box with no area because tiny-skia cannot fill it.
/// Clamps the radius to the box.
/// A popup shorter than its corner radius therefore becomes a stadium, not an inside-out path.
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

/// Draws one scene box: fill first, then border.
///
/// `dy` is the scroll applied when the element is drawn.
/// The box rect remains in unscrolled panel space, like every other scene rect.
///
/// This function insets a stroke by half its width, so the border stays inside the box.
/// This matches the CSS border box and the panel edge.
/// `BoxStyle::even_border` decides between one stroke and four fills.
/// A rounded corner with two widths has no single path.
/// The census corpus has only one asymmetric border, a one-sided rule that a rect
/// draws exactly.
///
/// Every drawn style uses a solid stroke.
/// The scene retains `dashed`, `dotted`, and `double` for a later painter.
/// The corpus currently has only `solid` and `none`.
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
    // Core chooses one stroke or four fills, not the painter.
    // Both bins therefore handle it the same way (`BoxStyle::even_border`).
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

/// Composites one asset into its resolved box.
/// Returns `false` when the ladder must continue.
///
/// The function draws a backing fill first only when the node requests it.
/// Yomitan draws that fill behind a transparent asset, but every image node in the
/// census samples disables it, so the fill is almost always skipped.
/// The panel background is the correct backing here.
/// A light-gray fill behind gaiji would be the only opaque patch on a dark theme.
///
/// The function scales with a pattern instead of `draw_pixmap` because the resolved
/// box can be fractional.
/// `draw_pixmap` translates by whole pixels.
/// A gaiji half a pixel from its baseline would then appear beside its replacement text.
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
    // Core chooses the tint, so both bins take the expensive path for exactly
    // the same assets (`SceneImage::tint`).
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

/// Composites one decoded pixmap into one box and scales it to fill the box.
fn composite(target: &mut PixmapMut<'_>, pixmap: &Pixmap, rect: SceneRect) {
    let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(box_) = Rect::from_xywh(rect.x, rect.y, rect.w, rect.h) else { return };
    let place = Transform::from_scale(rect.w / w, rect.h / h).post_translate(rect.x, rect.y);
    // Use `Pad`, not `Repeat`.
    // A bilinear sample at the box edge reaches half a texel past it.
    // `Pad` keeps that sample at the asset edge. `Repeat` would fetch the asset's far side
    // and create a bright seam along every gaiji.
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

/// Draws the last ladder rung as an outlined box where the character belongs.
///
/// It draws an outline instead of a fill.
/// A solid block resembles a censored glyph, but an empty frame shows an absent glyph.
/// The asset usually represents a character in this same color.
fn placeholder(target: &mut PixmapMut<'_>, rect: SceneRect, color: Rgb, scale: f32) {
    let edge = hairline(scale);
    if rect.w <= 2.0 * edge || rect.h <= 2.0 * edge {
        // If the box is too small for an outline, fill a speck so the ink remains visible.
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
    use chibipop::ui::layout::Appearance;
    use chibipop::ui::layout::{ElemSpan, GlyphBox, LineBox};
    use chibipop::ui::layout::{MeasureError, Metrics, SpanBox, TextMeasure};
    use tiny_skia::Pixmap;

    /// Records one run that the painter requested.
    ///
    /// It stores the complete span list, not only the first span.
    /// An element with mixed styles must reach the engine intact.
    /// A test that records only the first span could miss emphasis, colors, and raised marks.
    #[derive(Debug, Clone, PartialEq)]
    struct Recorded {
        spans: Vec<RecordedSpan>,
        max_w: f32,
        origin: (f32, f32),
    }

    /// Records one span from one recorded run.
    #[derive(Debug, Clone, PartialEq)]
    struct RecordedSpan {
        text: String,
        size: f32,
        weight: u16,
        italic: bool,
        color: Rgb,
        shift: f32,
    }

    /// Supplies fixed metrics and records drawn runs.
    /// Core layout tests use the same method, so the painter needs no font stack.
    #[derive(Default)]
    struct Fake {
        runs: Vec<Recorded>,
        /// Makes every measurement fail to exercise the no-JP-font degrade path.
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
            // Assign half an em to each character in sequence.
            // Break a line when the whole run does not fit.
            // This provides enough geometry for painter assertions without a font stack.
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

        fn hit_offset(
            &mut self,
            run: MeasureRun<'_>,
            x: f32,
            y: f32,
        ) -> Result<u32, MeasureError> {
            if self.broken {
                return Err(MeasureError::new("no fonts"));
            }
            let total = run.spans.iter().map(|s| s.text.encode_utf16().count()).sum::<usize>();
            let adv = run.spans.first().map_or(0.0, |s| s.size * 0.5).max(1.0);
            let line_h = run.spans.iter().fold(0.0f32, |h, s| h.max(s.size * 1.4)).max(1.4);
            let per_line = (run.max_w.max(1.0) / adv).floor().max(1.0) as usize;
            let line = (y / line_h).floor().max(0.0) as usize;
            let local = ((x / adv) - 0.5).round().max(0.0) as usize;
            Ok(line.saturating_mul(per_line).saturating_add(local).min(total) as u32)
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
            // Write one opaque pixel at the pen so a test can detect a run in the buffer.
            let (x, y) = run.origin;
            if x < 0.0 || y < 0.0 || x >= target.width() as f32 || y >= target.height() as f32 {
                return;
            }
            let i = (y as usize * target.width() as usize + x as usize) * 4;
            target.data_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }

    /// Returns the premultiplied value that a solid channel leaves after the fade.
    fn faded(channel: u8) -> u8 {
        let prod = u32::from(channel) * u32::from(PANEL_ALPHA) + 128;
        ((prod + (prod >> 8)) >> 8) as u8
    }

    /// Returns the premultiplied red channel that a solid `color` leaves after the fade.
    fn faded_red(color: Rgb) -> u8 {
        faded(color.0)
    }

    fn text_elem(text: &str, pen: (f32, f32), align: Align) -> SceneElem {
        styled_elem(pen, align, &[(text, 15.0, 400, false, 0.0)])
    }

    /// Creates an element with one span for each `(text, size, weight, italic, shift)` tuple.
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
            sources: Vec::new(),
        }
    }

    /// Creates a 200x100 body with one run and no other elements.
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
            highlights: Vec::new(),
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

    // ---- inline images ----

    /// Creates an image element with `key` and an optional `alt` run.
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

    /// Creates one asset entry with the path and format that the scene names.
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

    /// Creates a scene with exactly one image element.
    fn image_scene(elem: SceneElem) -> PopupScene {
        PopupScene { elems: vec![elem], ..plain_scene() }
    }

    /// Builds a database from the committed media fixture archive.
    /// The painter's composite uses pixels that the project builder wrote.
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

    /// The first rung composites a stored, decodable asset into the box that the scene
    /// resolved.
    /// The pixmap supplies the asset's pixels, not the panel background.
    ///
    /// The fixture is half-alpha pure blue, so this test also checks channel order.
    /// A bin that swaps red and blue paints every gaiji in the wrong hue but could pass a
    /// test that only checks for any output.
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

        // Inside the box, the asset blends over the panel and the frame then fades it once.
        // Half-alpha blue over the dark background leaves its blue channel above the background.
        let inside = pix.pixel(50, 50).unwrap();
        let outside = pix.pixel(100, 50).unwrap();
        assert!(
            inside.blue() > outside.blue() + 20,
            "the asset's own blue must reach the buffer: {inside:?} against {outside:?}"
        );
        assert!(inside.red() <= outside.red(), "and not as red: {inside:?}");
    }
}

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
use chibipop::ui::layout::{PopupScene, Rgb, SceneElem, SceneImage, SceneRect};
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

    // 4. Draw elements in scene order with scroll applied.
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
    use chibipop::ui::layout::{AnkiSlot, Appearance, BorderStyle, BoxStyle, Edges};
    use chibipop::ui::layout::{ElemSpan, GlyphBox, LineBox, MarkerBox, RubyBox};
    use chibipop::ui::layout::{MeasureError, Metrics, SidePanel, SideRow, SpanBox, TextMeasure};
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

    impl Recorded {
        /// Returns the recorded text.
        fn text(&self) -> String {
            self.spans.iter().map(|s| s.text.as_str()).collect()
        }

        /// Returns the only span in a run that contains one span.
        fn one(&self) -> &RecordedSpan {
            assert_eq!(1, self.spans.len(), "expected a single-span run");
            &self.spans[0]
        }
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

    /// Creates a box with only a style and no ink.
    fn elem_box(rect: SceneRect, style: BoxStyle) -> ElemBox {
        ElemBox { rect, style }
    }

    /// A pill's fill and border reach the buffer.
    /// The fill appears *under* the text because the box pass runs first.
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

        // Sample inside the border, away from every corner.
        let inside = pix.pixel(60, 50).unwrap();
        assert_eq!(faded_red((255, 0, 0)), inside.red(), "the fill covers the box");
        assert_eq!(0, inside.green(), "and it is the fill's colour, not the border's");
        // Sample the border two pixels from the left edge.
        let edge = pix.pixel(32, 50).unwrap();
        assert_eq!(faded(255), edge.green(), "the border strokes inside the box");
        assert_eq!(0, edge.red(), "over the fill, not blended with it");
        // Sample outside the box, where the panel background remains.
        let outside = pix.pixel(20, 50).unwrap();
        assert_eq!(faded_red(theme.background), outside.red());
        assert_eq!(1, text.runs.len(), "and the text is still drawn, once");
    }

    /// A rounded box leaves its corners unfilled.
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

    /// `border-style: none none none solid` creates only a left rule.
    /// The asymmetric path therefore fills one edge, not four.
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

    /// Scene rects remain in unscrolled panel space.
    /// The box pass applies the same scroll as text, so the border stays with its paragraph.
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

    /// A block box belongs to a container element that **leads** the paragraphs it frames.
    /// Its fill must appear under a *later* element's text, not only under its own.
    /// This test builds the scene through `layout::scene` instead of by hand.
    /// It checks that both passes agree about draw order for a bordered, filled `div`.
    /// This shape previously drew no box.
    /// Its first child carries a `data.content` marker.
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
                pitch: Vec::new(),
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

        // The fill sits under both paragraph pens and uses the box color, not panel color.
        for para in &after {
            let (x, y) = (para.pen.0 as u32 + 1, para.pen.1 as u32 + 1);
            let px = pix.pixel(x, y).unwrap();
            assert_eq!(
                faded_red((255, 0, 0)),
                px.red(),
                "the fill covers the paragraph at ({x}, {y})"
            );
        }
        // The fill stops at the border box.
        let below = (frame.rect.y + frame.rect.h + 2.0) as u32;
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(frame.rect.x as u32 + 4, below).unwrap().red(),
            "and nothing under it"
        );
        // Each boxed paragraph draws once.
        // The textless container does not draw it twice.
        for para in &after {
            let drawn = text.runs.iter().filter(|r| r.text() == para.text).count();
            assert_eq!(1, drawn, "{:?} drawn once", para.text);
        }
    }

    /// The scene determines the weight.
    ///
    /// A run must use the weight that Core measured.
    /// Otherwise, the line wrap and drawn output would differ, and CSS emphasis would disappear.
    /// The weight belongs to the *span*, not the element.
    /// One element can therefore contain two weights.
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
        // The side column uses the collapsed role format.
        let side = text.runs.last().expect("a side row must be drawn");
        assert_eq!(theme.collapsed_weight, side.one().weight);
        assert!(side.one().italic);
    }

    /// Passes the complete span list to the engine as one run.
    ///
    /// A bin that passes only the first span can still draw text while it loses the
    /// other emphasis, colors, and raised marks.
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

    /// Draws the reading where the scene placed it.
    ///
    /// A bin that paints only `spans` draws the base but loses the furigana.
    /// The reading uses a second run, not a span of the first.
    /// It takes no horizontal space from the line that contains it.
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

    /// Draws a reading over an *aligned* line from the element's pen.
    /// This matches the marker position and the Windows painter.
    ///
    /// The main run uses the ink box that the scene aligned because a `DrawRun`
    /// has no alignment flag.
    /// A reading is separate because `RubyBox::x` is run-relative and already
    /// includes the line's alignment slack.
    /// An ink-box origin would apply that slack twice and move the furigana away.
    ///
    /// These values come from 小学館例解学習国語 第十二版's 百人一首 appendix
    /// and one poet line.
    /// The name is right-aligned to the card's content edge at 403.75.
    /// The last reading sits over 呂 at 398.59 inside the 424 px panel.
    /// An ink-box origin would place that reading at 740.84.
    #[test]
    fn a_reading_over_an_aligned_line_is_drawn_from_the_pen_and_not_the_ink_box() {
        let theme = Theme::dark();
        let mut elem = styled_elem(
            (20.25, 12.0),
            Align::Trailing,
            &[("\u{67ff}\u{672c}\u{4eba}\u{9ebb}\u{5442}", 16.5, 400, false, 0.0)],
        );
        elem.wrap_w = 383.5;
        elem.rect = SceneRect { x: 362.5, y: 12.0, w: 41.25, h: 49.5 };
        elem.ruby = vec![RubyBox {
            text: "\u{308d}".into(),
            x: 378.34375,
            y: 0.0,
            w: 2.0625,
            h: 8.25,
            size: 4.125,
            color: (9, 8, 7),
            weight: 400,
            italic: false,
        }];
        let mut scene = plain_scene();
        scene.elems = vec![elem];

        let mut pix = Pixmap::new(424, 100).unwrap();
        let mut text = Fake::default();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, None, &mut pix.as_mut());

        assert_eq!(2, text.runs.len(), "the name, then its reading");
        let name = &text.runs[0];
        assert_eq!(362.5, name.origin.0, "the scene already right-aligned the line");
        assert_eq!(41.25, name.max_w, "and its width is the ink box");

        let read = &text.runs[1];
        assert_eq!("\u{308d}", read.text());
        assert_eq!((398.59375, 12.0), read.origin, "the pen plus the run-relative offset");
        assert_eq!(383.5, read.max_w, "at the width the scene measured it at");
        let ink = scene.elems[0].ruby[0].w;
        assert!(
            read.origin.0 + ink <= 424.0 - scene.origin,
            "so the furigana stays on the panel: {} against {}",
            read.origin.0 + ink,
            424.0 - scene.origin,
        );
    }

    /// Draws the list marker where the scene placed it.
    ///
    /// A bin that paints only `spans` draws the item but loses its bullet.
    /// The marker uses a second run because the layout places its box beside
    /// the item's principal box.
    /// Every item line and each continuation therefore starts at the item's indent.
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

    /// Each span keeps its own color.
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

        // The track height is 76. Content height 40 fits in view 100, but 300 does not.
        let mut short = plain_scene();
        short.used_h = 40.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&short, &theme, 0.0, 1.0), &mut Fake::default(), None, &mut pix.as_mut());
        assert_eq!(bg, pix.pixel(192, 20).unwrap().red(), "content that fits gets no thumb");

        let mut tall = plain_scene();
        tall.used_h = 300.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 0.0, 1.0), &mut Fake::default(), None, &mut pix.as_mut());
        // At 1x, the bar is 4px wide and its right edge is at 200 - padding/2.
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
        // The fake measures 11 characters at 13px as 71.5 pixels.
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

    /// This rung handles a corrupt asset even though every census format can decode.
    /// `gaiji/torn.png` has a valid 12x7 IHDR, but its IDAT is not a zlib stream.
    /// The media row is real and the line uses its dimensions.
    /// The *painter* receives no pixels and draws the element's own `alt` run.
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

    /// The last rung draws an outlined box when nothing is stored and no `alt` exists.
    /// It prevents a gap from becoming a hole in a word.
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

    /// A session without a readable Dictionary database still paints.
    /// Every image takes the `alt` rung because `None` is a state, not a failure.
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

    /// The painter sends a monochrome asset to the buffer in the element's own color.
    /// That color is the theme's body text color, so black-ink gaiji remains visible
    /// on a dark panel.
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
        // Use a mask color that differs from the asset's own blue.
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

    /// Scene rects remain unscrolled, so an image moves with its paragraph.
    /// Every other scene rect follows the same rule.
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

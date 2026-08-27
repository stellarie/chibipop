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
use chibipop::ui::layout::{self, Align, ElemKind, MeasureRun, PopupScene, Rgb};
use chibipop::ui::theme::{Theme, SCROLLBAR_W};
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut};
use tiny_skia::{Rect, Shader, Stroke, Transform};

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
pub fn panel(p: &Panel<'_>, text: &mut dyn PanelText, target: &mut PixmapMut<'_>) {
    let (w, h) = (target.width() as f32, target.height() as f32);
    let scene = p.scene;
    let theme = p.theme;

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
    // runs already culled by core.
    for painted in scene.visible(p.scroll, scene.view_h) {
        let elem = painted.elem;
        if elem.kind == ElemKind::Separator {
            fill(target, elem.rect.x, painted.pen.1, elem.rect.w, elem.rect.h, elem.color);
            continue;
        }
        // `Trailing` is the frequency corner. DirectWrite gets the
        // whole wrap box plus an alignment flag; a `DrawRun` carries
        // no alignment, so we hand over the ink box the scene already
        // right-aligned instead. Same pixels, no second concept.
        let (origin, max_w) = match elem.align {
            Align::Trailing => ((elem.rect.x, painted.pen.1), elem.rect.w.max(1.0)),
            Align::Leading => (painted.pen, elem.wrap_w),
        };
        let run = DrawRun {
            text: &elem.text,
            size: elem.font_size,
            weight: elem.weight,
            italic: elem.italic,
            max_w,
            color: elem.color,
            origin,
        };
        text.draw_run(run, target);
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
            let run = DrawRun {
                text: &painted.row.text,
                size: theme.collapsed_size,
                weight: theme.collapsed_weight,
                italic: theme.collapsed_italic,
                max_w: side.col_w,
                color: painted.row.color,
                origin: (side.col_x, painted.y),
            };
            text.draw_run(run, target);
        }
    }

    // 6. The thumb, and never a track. Core does this arithmetic in
    // whole pixels, so the scene's f32 heights round on the way in.
    let pad = theme.padding;
    let view = scene.view_h.round() as i32;
    let track_h = view - 2 * pad;
    let total = scene.used_h.ceil() as i32 + 2 * pad;
    let at = p.scroll.round() as i32;
    if let Some((top, thumb_h)) = layout::scrollbar_thumb(track_h, total, view, at) {
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
        let measured = text.measure(MeasureRun {
            text: &anki.label,
            font: &theme.font_name,
            size: theme.collapsed_size,
            weight: theme.collapsed_weight,
            italic: theme.collapsed_italic,
            max_w: rect.w,
        });
        let x = match measured {
            Ok(m) => rect.x + ((rect.w - m.w) / 2.0).max(0.0),
            Err(_) => rect.x,
        };
        let run = DrawRun {
            text: &anki.label,
            size: theme.collapsed_size,
            weight: theme.collapsed_weight,
            italic: theme.collapsed_italic,
            max_w: rect.w.max(1.0),
            color: anki.color,
            origin: (x, rect.y),
        };
        text.draw_run(run, target);
    }

    // 8. Windows' constant window alpha, per pixel.
    fade(target, PANEL_ALPHA);
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

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::ui::layout::{AnkiSlot, GlyphBox, Metrics, SceneElem, SceneRect};
    use chibipop::ui::layout::{MeasureError, SidePanel, SideRow, TextMeasure};
    use tiny_skia::Pixmap;

    /// One run the painter asked for.
    #[derive(Debug, Clone, PartialEq)]
    struct Recorded {
        text: String,
        size: f32,
        max_w: f32,
        weight: u16,
        italic: bool,
        color: Rgb,
        origin: (f32, f32),
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
        fn measure(&mut self, run: MeasureRun<'_>) -> Result<Metrics, MeasureError> {
            if self.broken {
                return Err(MeasureError::new("no fonts"));
            }
            let w = run.text.chars().count() as f32 * run.size * 0.5;
            let lines = if run.max_w > 0.0 { (w / run.max_w).ceil().max(1.0) } else { 1.0 };
            Ok(Metrics { w, h: lines * run.size * 1.4, lines: lines as u32 })
        }

        fn caret_boxes(
            &mut self,
            run: MeasureRun<'_>,
            at: &[u32],
            out: &mut Vec<GlyphBox>,
        ) -> Result<(), MeasureError> {
            let adv = run.size * 0.5;
            out.extend(at.iter().map(|i| GlyphBox {
                x: *i as f32 * adv,
                y: 0.0,
                w: adv,
                h: run.size * 1.4,
            }));
            Ok(())
        }
    }

    impl PanelText for Fake {
        fn draw_run(&mut self, run: DrawRun<'_>, target: &mut PixmapMut<'_>) {
            self.runs.push(Recorded {
                text: run.text.to_string(),
                size: run.size,
                max_w: run.max_w,
                weight: run.weight,
                italic: run.italic,
                color: run.color,
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

    /// The premultiplied red channel a solid `color` leaves behind
    /// after the fade.
    fn faded_red(color: Rgb) -> u8 {
        let prod = u32::from(color.0) * u32::from(PANEL_ALPHA) + 128;
        ((prod + (prod >> 8)) >> 8) as u8
    }

    fn text_elem(text: &str, pen: (f32, f32), align: Align) -> SceneElem {
        SceneElem {
            kind: ElemKind::Text,
            text: text.to_string(),
            color: (210, 212, 218),
            font_size: 15.0,
            weight: 400,
            italic: false,
            top_gap: 0.0,
            wrap_w: 176.0,
            align,
            pen,
            rect: SceneRect { x: pen.0, y: pen.1, w: 100.0, h: 20.0 },
            lines: 1,
            advance: 20.0,
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
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, &mut pix.as_mut());

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
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, &mut pix.as_mut());

        let run = text.runs.first().expect("the corner run must be drawn");
        assert_eq!(150.0, run.origin.0, "the scene already right-aligned this box");
        assert_eq!(38.0, run.max_w, "and its width is the box, not the wrap width");
    }

    /// The scene decides the weight.
    ///
    /// A run painted in a weight core
    /// did not measure it in would be
    /// wrapped one way and drawn
    /// another, and CSS emphasis would
    /// reach the panel not at all.
    #[test]
    fn a_run_is_drawn_in_the_weight_and_style_the_scene_measured_it_in() {
        let theme = Theme { collapsed_weight: 200, collapsed_italic: true, ..Theme::dark() };
        let mut elem = text_elem("gloss", (12.0, 12.0), Align::Leading);
        elem.weight = 700;
        elem.italic = true;
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
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, &mut pix.as_mut());

        let gloss = text.runs.first().expect("the gloss must be drawn");
        assert_eq!(700, gloss.weight);
        assert!(gloss.italic);
        // The side column is one
        // format, the collapsed role's.
        let side = text.runs.last().expect("a side row must be drawn");
        assert_eq!(theme.collapsed_weight, side.weight);
        assert!(side.italic);
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
        panel(&painter(&short, &theme, 0.0, 1.0), &mut Fake::default(), &mut pix.as_mut());
        assert_eq!(bg, pix.pixel(192, 20).unwrap().red(), "content that fits gets no thumb");

        let mut tall = plain_scene();
        tall.used_h = 300.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 0.0, 1.0), &mut Fake::default(), &mut pix.as_mut());
        // 4px wide, right edge at 200 - padding/2.
        assert_eq!(thumb, pix.pixel(192, 20).unwrap().red());
        assert_eq!(bg, pix.pixel(187, 20).unwrap().red(), "a 1x bar is four pixels wide");

        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 0.0, 2.0), &mut Fake::default(), &mut pix.as_mut());
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
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, &mut pix.as_mut());

        let label = text.runs.last().expect("the label must be drawn");
        assert_eq!("Add to Anki", label.text);
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
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, &mut pix.as_mut());

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
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut text, &mut pix.as_mut());

        let sep = faded_red(theme.separator);
        assert_eq!(sep, pix.pixel(140, 80).unwrap().red(), "the rule runs the panel's height");
        assert_ne!(sep, pix.pixel(140, 112).unwrap().red(), "but never into the Anki strip");
        assert!(
            text.runs.iter().any(|r| r.origin == (152.0, 12.0) && r.text == "See also"),
            "the column's rows are drawn at the column"
        );
    }

    #[test]
    fn a_one_pixel_target_and_a_scene_scrolled_past_its_end_still_paint() {
        let theme = Theme::dark();
        let scene = plain_scene();
        let mut pix = Pixmap::new(1, 1).unwrap();
        panel(&painter(&scene, &theme, 0.0, 1.0), &mut Fake::default(), &mut pix.as_mut());

        let mut tall = plain_scene();
        tall.used_h = 900.0;
        let mut pix = Pixmap::new(200, 100).unwrap();
        panel(&painter(&tall, &theme, 9_000.0, 1.5), &mut Fake::default(), &mut pix.as_mut());
        assert_eq!(
            faded_red(theme.background),
            pix.pixel(100, 50).unwrap().red(),
            "an overscrolled scene is an empty panel, not a missing one"
        );
    }
}

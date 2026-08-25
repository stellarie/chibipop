//! Direct2D/DirectWrite paint (ADR-0004).
//!
//! Hwnd target, not layered.
//! Caller paints; wndproc won't.
//!
//! Paint-only. Layout is core's:
//! `Text` is the `TextMeasure` core
//! wraps its runs through, and this
//! module draws the `PopupScene` that
//! comes back. Every gap, width and y
//! in the panel is decided there, so
//! the Linux surface lays the popup
//! out the same way.
//!
//! DIPs, not device pixels: the D2D
//! target carries the DPI, so the
//! scene is built at `client / scale`
//! and the conversion never leaves
//! this module.

use crate::controller::HitAction;
use crate::present::Presentation;
use crate::ui::layout::{
    self, Align, ElemKind, GlyphBox, HitTarget, MeasureError, MeasureRun, Metrics,
    PopupScene, SceneElem, SceneRequest, TextMeasure,
};
use crate::ui::theme::{Theme, SCROLLBAR_W};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows_numerics::Vector2;

/// Measure-only geometry capture.
///
/// The layout goldens' capture side.
pub mod geometry;

/// One font's formats, by size.
#[derive(Default)]
struct FormatCache {
    font: String,
    by_size: HashMap<u32, IDWriteTextFormat>,
}

/// DirectWrite, as a text engine.
///
/// Shapes runs for core's layout and
/// re-shapes them for paint. It owns
/// the format cache, so both walks
/// hit the same `IDWriteTextFormat`s.
pub struct Text {
    dwrite: IDWriteFactory,
    /// Text formats, reused.
    formats: RefCell<FormatCache>,
}

impl Text {
    pub fn new() -> Result<Text> {
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .context("DWriteCreateFactory")?;
        Ok(Text { dwrite, formats: RefCell::default() })
    }

    /// One format per font and size.
    fn format(&self, font: &str, size: f32) -> windows::core::Result<IDWriteTextFormat> {
        let key = size.to_bits();
        {
            let cache = self.formats.borrow();
            if cache.font == font {
                if let Some(f) = cache.by_size.get(&key) {
                    return Ok(f.clone());
                }
            }
        }

        let created = unsafe {
            self.dwrite.CreateTextFormat(
                &HSTRING::from(font),
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("ja-JP"),
            )
        }?;

        let mut cache = self.formats.borrow_mut();
        if cache.font != font {
            cache.font = font.to_string();
            cache.by_size.clear();
        }
        cache.by_size.insert(key, created.clone());
        Ok(created)
    }

    /// One wrapped layout.
    ///
    /// The one shaping call in the
    /// bin: measure and paint both
    /// come through here, so a run
    /// is never wrapped two ways.
    fn layout(
        &self,
        text: &str,
        font: &str,
        size: f32,
        max_w: f32,
    ) -> windows::core::Result<IDWriteTextLayout> {
        let format = self.format(font, size)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&wide, &format, max_w.max(1.0), f32::MAX) }
    }

    /// Borrows it as a `TextMeasure`.
    fn measurer(&self) -> Measurer<'_> {
        Measurer(self)
    }
}

/// `Text`, seen through the seam.
///
/// The trait wants `&mut self`; the
/// cache behind it is shared, so the
/// borrow is what moves, not `Text`.
struct Measurer<'a>(&'a Text);

/// Layout cannot read an HRESULT.
fn refused(e: windows::core::Error) -> MeasureError {
    MeasureError::new(e.message())
}

impl TextMeasure for Measurer<'_> {
    fn measure(&mut self, run: MeasureRun<'_>) -> std::result::Result<Metrics, MeasureError> {
        let layout = self
            .0
            .layout(run.text, run.font, run.size, run.max_w)
            .map_err(refused)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut m) }.map_err(refused)?;
        Ok(Metrics { w: m.width, h: m.height, lines: m.lineCount })
    }

    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> std::result::Result<(), MeasureError> {
        let layout = self
            .0
            .layout(run.text, run.font, run.size, run.max_w)
            .map_err(refused)?;
        for &offset in at {
            let mut px = 0.0f32;
            let mut py = 0.0f32;
            let mut hm = DWRITE_HIT_TEST_METRICS::default();
            // SAFETY: offset is a valid
            // index into the layout's
            // UTF-16 text - core walks
            // the same string.
            unsafe { layout.HitTestTextPosition(offset, false, &mut px, &mut py, &mut hm) }
                .map_err(refused)?;
            out.push(GlyphBox { x: hm.left, y: hm.top, w: hm.width, h: hm.height });
        }
        Ok(())
    }
}

/// Lays one popup out, in DIPs.
fn scene_of(
    text: &Text,
    p: &Presentation,
    theme: &Theme,
    max_w: f32,
    max_h: f32,
    show_back: bool,
    side_panel: bool,
) -> Result<PopupScene> {
    // The Anki affordance is its own
    // window here, sized by its own
    // font: Windows takes the label
    // and leaves core's slot alone
    // (ADR-0004).
    layout::scene(
        &SceneRequest {
            presentation: p,
            theme,
            max_w,
            max_h,
            show_back,
            side_panel,
            anki: None,
        },
        &mut text.measurer(),
    )
    .map_err(anyhow::Error::new)
    .context("laying out the popup")
}

/// `(width, view_h, content_h)`.
///
/// All three in physical pixels: the
/// scene is in DIPs, and this is the
/// only place the scale goes back on.
fn popup_size(scene: &PopupScene, scale: f32, max_w: i32) -> (i32, i32, i32) {
    (
        // No side column: keep the
        // width the caller offered,
        // exactly, without a round trip
        // through the scale.
        scene.panel_w.map_or(max_w, |w| (w * scale).ceil() as i32),
        (scene.view_h * scale).ceil() as i32,
        (scene.content_h * scale).ceil() as i32,
    )
}

/// D2D state for one window.
pub struct Renderer {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    text: Text,
    /// Lazy; the window starts 0x0.
    target: Option<ID2D1HwndRenderTarget>,
    /// From the last paint pass, in
    /// DIPs, as the scene reported it.
    hits: RefCell<Vec<HitTarget>>,
}

impl Renderer {
    /// Factories only, no target.
    pub fn new(hwnd: HWND) -> Result<Renderer> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .context("D2D1CreateFactory")?;
        Ok(Renderer {
            hwnd,
            d2d_factory,
            text: Text::new()?,
            target: None,
            hits: RefCell::new(Vec::new()),
        })
    }

    fn dpi_scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }

    /// Finds the action at (x, y).
    pub fn hit_test(&self, x_phys: i32, y_phys: i32, scroll_phys: i32) -> Option<HitAction> {
        let scale = self.dpi_scale();
        let x = x_phys as f32 / scale;
        let y = y_phys as f32 / scale + scroll_phys as f32 / scale;
        self.hits
            .borrow()
            .iter()
            .find(|r| {
                let y_hit = y >= r.y && y < r.y + r.h;
                let x_hit = match (r.x, r.w) {
                    (Some(rx), Some(rw)) => x >= rx && x < rx + rw,
                    _ => true,
                };
                x_hit && y_hit
            })
            .map(|r| r.action.clone())
    }

    /// `(width, view_h, content_h)`.
    ///
    /// All three in physical pixels.
    pub fn measure(
        &mut self,
        p: &Presentation,
        theme: &Theme,
        max_w: i32,
        max_h: i32,
        show_back: bool,
        side_panel: bool,
    ) -> Result<(i32, i32, i32)> {
        let scale = self.dpi_scale();
        let scene = scene_of(
            &self.text,
            p,
            theme,
            max_w as f32 / scale,
            max_h as f32 / scale,
            show_back,
            side_panel,
        )?;
        Ok(popup_size(&scene, scale, max_w))
    }

    /// Paints into the client rect.
    ///
    /// Device loss retries once. The
    /// scene is built before the retry
    /// loop: measuring cannot lose a
    /// device, only painting can.
    pub fn paint(
        &mut self,
        p: &Presentation,
        theme: &Theme,
        scroll: i32,
        show_back: bool,
        side_panel: bool,
    ) -> Result<()> {
        let (cw, ch) = self.client_size().context("querying the popup's client size")?;
        self.ensure_target(cw, ch).context("preparing the D2D render target")?;

        let scale = self.dpi_scale();
        let w = (cw as f32 / scale) as i32;
        let h = (ch as f32 / scale) as i32;
        let scroll = (scroll as f32 / scale) as i32;

        let scene = scene_of(&self.text, p, theme, w as f32, h as f32, show_back, side_panel)?;
        *self.hits.borrow_mut() = scene.hit_targets();

        if let Err(e) = self.paint_once(&scene, theme, w, h, scroll) {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.ensure_target(cw, ch)
                    .context("recreating the D2D render target after device loss")?;
                self.paint_once(&scene, theme, w, h, scroll)
                    .context("repainting after device-lost recovery")?;
            } else {
                return Err(e).context("D2D paint failed");
            }
        }
        Ok(())
    }

    fn client_size(&self) -> Result<(i32, i32)> {
        let mut rc = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut rc) }.context("GetClientRect")?;
        Ok(((rc.right - rc.left).max(1), (rc.bottom - rc.top).max(1)))
    }

    /// Creates or resizes it.
    fn ensure_target(&mut self, w: i32, h: i32) -> Result<()> {
        let size = D2D_SIZE_U { width: w.max(1) as u32, height: h.max(1) as u32 };

        if let Some(target) = &self.target {
            unsafe { target.Resize(&size) }.context("ID2D1HwndRenderTarget::Resize")?;
            return Ok(());
        }

        let dpi = unsafe { GetDpiForWindow(self.hwnd) } as f32;
        let dpi = if dpi == 0.0 { 96.0 } else { dpi };
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: size,
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let target = unsafe { self.d2d_factory.CreateHwndRenderTarget(&rt_props, &hwnd_props) }
            .context("ID2D1Factory::CreateHwndRenderTarget")?;
        self.target = Some(target);
        Ok(())
    }

    /// One BeginDraw/EndDraw cycle.
    ///
    /// Raw error; paint matches it.
    fn paint_once(
        &self,
        scene: &PopupScene,
        theme: &Theme,
        w: i32,
        h: i32,
        scroll: i32,
    ) -> windows::core::Result<()> {
        let target = self
            .target
            .as_ref()
            .expect("ensure_target must run before paint_once");

        unsafe { target.BeginDraw() };
        let scope = DrawScope { target: Some(target) };

        let draw_result: windows::core::Result<()> = (|| {
            unsafe {
                target.Clear(Some(&color_f(theme.background)));

                let bg_brush = target.CreateSolidColorBrush(&color_f(theme.background), None)?;
                let panel = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 },
                    radiusX: theme.corner_radius as f32,
                    radiusY: theme.corner_radius as f32,
                };
                target.FillRoundedRectangle(&panel, &bg_brush);

                let border_brush = target.CreateSolidColorBrush(&color_f(theme.border), None)?;
                target.DrawRoundedRectangle(&panel, &border_brush, 1.0, None);
            }

            for painted in scene.visible(scroll as f32, h as f32) {
                self.draw_elem(target, theme, painted.elem, painted.pen)?;
            }

            if let Some(side) = &scene.side {
                let sep_brush =
                    unsafe { target.CreateSolidColorBrush(&color_f(theme.separator), None) }?;
                // Full-height rule: it
                // divides the panel, not
                // just the column.
                let sep = D2D_RECT_F {
                    left: side.rule_x,
                    top: side.origin_y,
                    right: side.rule_x + side.rule_w,
                    bottom: (h - theme.padding) as f32,
                };
                unsafe { target.FillRectangle(&sep, &sep_brush) };

                for painted in side.visible(scroll as f32, h as f32) {
                    let brush = unsafe {
                        target.CreateSolidColorBrush(&color_f(painted.row.color), None)
                    }?;
                    let layout = self.text.layout(
                        &painted.row.text,
                        &theme.font_name,
                        theme.collapsed_size,
                        side.col_w,
                    )?;
                    unsafe {
                        target.DrawTextLayout(
                            Vector2 { X: side.col_x, Y: painted.y },
                            &layout,
                            &brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }

            let track_h = h - 2 * theme.padding;
            let total = scene.used_h.ceil() as i32 + 2 * theme.padding;
            if let Some((top, thumb_h)) = layout::scrollbar_thumb(track_h, total, h, scroll) {
                let brush =
                    unsafe { target.CreateSolidColorBrush(&color_f(theme.dimmed_text), None) }?;
                let x = (w - theme.padding / 2 - SCROLLBAR_W) as f32;
                let rect = D2D_RECT_F {
                    left: x,
                    top: (theme.padding + top) as f32,
                    right: x + SCROLLBAR_W as f32,
                    bottom: (theme.padding + top + thumb_h) as f32,
                };
                unsafe { target.FillRectangle(&rect, &brush) };
            }

            Ok(())
        })();

        let end_result = scope.end();

        // Draw error first: it's finer.
        // EndDraw signals device loss.
        draw_result.and(end_result)
    }

    /// Draws one scene element.
    ///
    /// Re-shapes the run at the width
    /// the scene measured it at, so
    /// the ink lands where the hit
    /// rects say it does.
    fn draw_elem(
        &self,
        target: &ID2D1HwndRenderTarget,
        theme: &Theme,
        elem: &SceneElem,
        pen: (f32, f32),
    ) -> windows::core::Result<()> {
        let brush = unsafe { target.CreateSolidColorBrush(&color_f(elem.color), None) }?;

        if elem.kind == ElemKind::Separator {
            let rect = D2D_RECT_F {
                left: elem.rect.x,
                top: pen.1,
                right: elem.rect.x + elem.rect.w,
                bottom: pen.1 + elem.rect.h,
            };
            unsafe { target.FillRectangle(&rect, &brush) };
            return Ok(());
        }

        let layout =
            self.text
                .layout(&elem.text, &theme.font_name, elem.font_size, elem.wrap_w)?;
        if elem.align == Align::Trailing {
            unsafe { layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING) }?;
        }
        unsafe {
            target.DrawTextLayout(
                Vector2 { X: pen.0, Y: pen.1 },
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(())
    }
}

/// Ends the draw on drop.
struct DrawScope<'a> {
    target: Option<&'a ID2D1HwndRenderTarget>,
}

impl DrawScope<'_> {
    /// Ends it, returns its result.
    fn end(mut self) -> windows::core::Result<()> {
        match self.target.take() {
            Some(t) => unsafe { t.EndDraw(None, None) },
            None => Ok(()),
        }
    }
}

impl Drop for DrawScope<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.target {
            unsafe {
                let _ = t.EndDraw(None, None);
            }
        }
    }
}

fn color_f((r, g, b): (u8, u8, u8)) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
}

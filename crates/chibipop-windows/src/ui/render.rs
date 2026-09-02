//! Paints with Direct2D and DirectWrite.
//!
//! This module paints to an HWND target.
//! The target is not a layered window.
//! The caller calls the paint function. `wndproc` does not paint.
//!
//! This module only paints. Core owns layout.
//! `Text` implements core's `TextMeasure` trait.
//! Core uses `Text` to wrap each run, and this module draws the returned `PopupScene`.
//!
//! Core decides every panel gap, width, and y position.
//! The Linux surface uses the same arrangement.
//!
//! This module uses DIPs instead of device pixels.
//! The Direct2D target carries the DPI value.
//! This module builds the scene at `client / scale`.
//! The scale conversion stays in this module.

use crate::controller::HitAction;
use crate::present::Presentation;
use crate::ui::layout::{
    self, Align, ElemKind, GlyphBox, HitTarget, LineBox, MeasureError, MeasureRun, Measured,
    Metrics, PopupScene, RenderSettings, SceneElem, SceneImage, SceneRect, SceneRequest, SpanBox,
    StyledSpan, TextMeasure,
};
use crate::ui::media::MediaSurfaces;
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

/// Captures measured geometry only.
///
/// Geometry-snapshot goldens use this module to capture the scene.
pub mod geometry;

/// Caches formats for one font family. The key stores the size, weight, and style.
#[derive(Default)]
struct FormatCache {
    font: String,
    by_key: HashMap<u64, IDWriteTextFormat>,
}

/// Packs the size, weight, and style into one cache key.
///
/// DirectWrite cannot change a format after it creates the format.
/// An earlier key stored only the size. A run can also vary in weight and style, so the key must
/// store all three values.
fn format_key(size: f32, weight: u16, italic: bool) -> u64 {
    let s = size.to_bits() as u64;
    let w = (weight as u64) << 32;
    let i = if italic { 1u64 << 48 } else { 0 };
    s | w | i
}

/// Uses DirectWrite as the text engine.
///
/// `Text` shapes each run for core's layout pass and then shapes it again for paint.
/// `Text` owns the format cache, so both passes use the same `IDWriteTextFormat` objects.
pub struct Text {
    dwrite: IDWriteFactory,
    /// This field stores the formats for reuse.
    formats: RefCell<FormatCache>,
}

impl Text {
    pub fn new() -> Result<Text> {
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .context("DWriteCreateFactory")?;
        Ok(Text { dwrite, formats: RefCell::default() })
    }

    /// Gets or creates the text format for a font, size, weight, and style.
    fn format(
        &self,
        font: &str,
        size: f32,
        weight: u16,
        italic: bool,
    ) -> windows::core::Result<IDWriteTextFormat> {
        let key = format_key(size, weight, italic);
        {
            let cache = self.formats.borrow();
            if cache.font == font {
                if let Some(f) = cache.by_key.get(&key) {
                    return Ok(f.clone());
                }
            }
        }

        let style = style_of(italic);
        let created = unsafe {
            self.dwrite.CreateTextFormat(
                &HSTRING::from(font),
                None,
                DWRITE_FONT_WEIGHT(weight as i32),
                style,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("ja-JP"),
            )
        }?;

        let mut cache = self.formats.borrow_mut();
        if cache.font != font {
            cache.font = font.to_string();
            cache.by_key.clear();
        }
        cache.by_key.insert(key, created.clone());
        Ok(created)
    }

    /// Builds one wrapped `IDWriteTextLayout` for a run.
    ///
    /// This is the only call that shapes text in the bin.
    /// The measure pass and paint pass both call this function with the same `MeasureRun`.
    /// A run therefore wraps in one way and never paints with a different weight from its measure
    /// pass.
    ///
    /// The whole run forms one layout, so its spans wrap as one paragraph.
    /// A bold span can share a line with a normal span (ARCHITECTURE.md#popup-and-measurement).
    /// The cached `IDWriteTextFormat` of the first span sets the base format.
    /// Each other span overrides its own range.
    /// A one-span run therefore uses the same calls as before.
    ///
    /// This layout ignores color because color does not change geometry.
    /// The paint side owns the brush that applies color.
    fn layout(&self, run: MeasureRun<'_>) -> windows::core::Result<IDWriteTextLayout> {
        let base = run.spans.first().copied().unwrap_or(NO_SPAN);
        let format = self.format(base.font, base.size, base.weight, base.italic)?;
        let mut wide: Vec<u16> = Vec::new();
        for span in run.spans {
            wide.extend(span.text.encode_utf16());
        }
        let layout = unsafe {
            self.dwrite
                .CreateTextLayout(&wide, &format, run.max_w.max(1.0), f32::MAX)
        }?;
        for (span, range) in run.spans.iter().zip(ranges(run.spans)).skip(1) {
            // SAFETY: Each range names UTF-16 positions inside the string that the layout just
            // received from this function.
            unsafe {
                layout.SetFontFamilyName(&HSTRING::from(span.font), range)?;
                layout.SetFontWeight(DWRITE_FONT_WEIGHT(span.weight as i32), range)?;
                layout.SetFontStyle(style_of(span.italic), range)?;
                layout.SetFontSize(span.size, range)?;
            }
        }
        Ok(layout)
    }

    /// Borrows `Text` as a `TextMeasure`.
    fn measurer(&self) -> Measurer<'_> {
        Measurer(self)
    }
}

/// Defines the style for a run with no spans.
///
/// Core never creates an empty run because it measures every element with a styled span.
/// A layout still needs a font family and size for text.
/// DirectWrite must reject an unnamed family, so this module must not choose one.
const NO_SPAN: StyledSpan<'static> = StyledSpan {
    text: "",
    font: "",
    size: 1.0,
    weight: 400,
    italic: false,
    color: (0, 0, 0),
};

fn style_of(italic: bool) -> DWRITE_FONT_STYLE {
    if italic {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    }
}

/// Gives each span's UTF-16 range inside the run.
///
/// The layout holds one string from start to end, so each span maps to a range inside that string.
/// A range is the unit that per-range formatting and `HitTestTextRange` accept.
fn ranges<'a>(
    spans: &'a [StyledSpan<'a>],
) -> impl Iterator<Item = DWRITE_TEXT_RANGE> + 'a {
    let mut start = 0u32;
    spans.iter().map(move |span| {
        let length = span.text.encode_utf16().count() as u32;
        let range = DWRITE_TEXT_RANGE { startPosition: start, length };
        start += length;
        range
    })
}

/// Finds the line that contains UTF-16 position `at`.
fn line_of(lines: &[DWRITE_LINE_METRICS], at: u32) -> usize {
    let mut end = 0u32;
    for (i, line) in lines.iter().enumerate() {
        end += line.length;
        if at < end {
            return i;
        }
    }
    lines.len().saturating_sub(1)
}

/// Gives the rects that the range `[at, at + len)` fills.
///
/// The range returns one rect for each line that it touches.
/// It returns one more rect for each bidirectional run on a line.
/// `HitTestTextRange` reports the count that the buffer needs when it is too small.
/// The `hint` parameter handles the ordinary case in one call.
/// A retry handles every other case.
fn hit_range(
    layout: &IDWriteTextLayout,
    at: u32,
    len: u32,
    hint: usize,
    out: &mut Vec<DWRITE_HIT_TEST_METRICS>,
) -> windows::core::Result<()> {
    out.clear();
    out.resize(hint.max(1), DWRITE_HIT_TEST_METRICS::default());
    let mut got = 0u32;
    // SAFETY: `at` and `len` name UTF-16 positions inside the layout text.
    // The code sizes the buffer before each call.
    let mut hr = unsafe { layout.HitTestTextRange(at, len, 0.0, 0.0, Some(out), &mut got) };
    if hr.is_err() && got as usize > out.len() {
        out.resize(got as usize, DWRITE_HIT_TEST_METRICS::default());
        hr = unsafe { layout.HitTestTextRange(at, len, 0.0, 0.0, Some(out), &mut got) };
    }
    hr?;
    out.truncate(got as usize);
    Ok(())
}

/// Views `Text` through the `TextMeasure` seam.
///
/// The `TextMeasure` trait needs `&mut self`.
/// The cache behind `Text` is shared, so only the borrow changes.
/// `Text` itself does not move.
struct Measurer<'a>(&'a Text);

/// Converts a Windows error to a `MeasureError` because the `TextMeasure` trait cannot return a raw
/// HRESULT.
fn refused(e: windows::core::Error) -> MeasureError {
    MeasureError::new(e.message())
}

impl TextMeasure for Measurer<'_> {
    fn measure(
        &mut self,
        run: MeasureRun<'_>,
        out: &mut Measured,
    ) -> std::result::Result<(), MeasureError> {
        out.clear();
        let layout = self.0.layout(run).map_err(refused)?;
        // `GetMetrics` still supplies the aggregate values.
        // A one-span run therefore measures exactly as it did before the seam widened.
        // The code below adds detail beside that call.
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut m) }.map_err(refused)?;
        out.metrics = Metrics { w: m.width, h: m.height, lines: m.lineCount };

        let mut lines = vec![DWRITE_LINE_METRICS::default(); m.lineCount.max(1) as usize];
        let mut got = 0u32;
        unsafe { layout.GetLineMetrics(Some(&mut lines), &mut got) }.map_err(refused)?;
        lines.truncate(got as usize);

        let mut rects = Vec::new();
        let (mut y, mut start) = (0.0f32, 0u32);
        for line in &lines {
            // The width excludes whitespace at the end, so a line's width has the same meaning as
            // the width from `GetMetrics`.
            let visible = line.length.saturating_sub(line.trailingWhitespaceLength);
            let w = if visible == 0 {
                0.0
            } else {
                hit_range(&layout, start, visible, 1, &mut rects).map_err(refused)?;
                rects.iter().fold(0.0f32, |a, r| a.max(r.left + r.width))
            };
            out.lines.push(LineBox { y, w, h: line.height, baseline: line.baseline });
            y += line.height;
            start += line.length;
        }
        // Each counted line gets one line box, no matter how many lines DirectWrite fills.
        // Core still adds the gap after an empty run in either case.
        if out.lines.is_empty() {
            out.lines.push(LineBox { y: 0.0, w: m.width, h: m.height, baseline: m.height });
        }

        for (i, range) in ranges(run.spans).enumerate() {
            if range.length == 0 {
                continue;
            }
            hit_range(&layout, range.startPosition, range.length, lines.len(), &mut rects)
                .map_err(refused)?;
            for rect in &rects {
                let line = line_of(&lines, rect.textPosition) as u32;
                match out.spans.iter_mut().rev().find(|b| b.span == i as u32 && b.line == line) {
                    // Each span gets one box on each line.
                    // A bidirectional split reports the same line twice, so the code unions both
                    // reports.
                    Some(b) => {
                        let right = (b.x + b.w).max(rect.left + rect.width);
                        b.x = b.x.min(rect.left);
                        b.w = right - b.x;
                    }
                    None => out.spans.push(SpanBox {
                        span: i as u32,
                        line,
                        x: rect.left,
                        w: rect.width,
                        // The code fills this value below.
                        // A span's own advance needs the advance of its line.
                        h: 0.0,
                    }),
                }
            }
        }
        span_heights(run.spans, out);
        Ok(())
    }

    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> std::result::Result<(), MeasureError> {
        let layout = self.0.layout(run).map_err(refused)?;
        for &offset in at {
            let mut px = 0.0f32;
            let mut py = 0.0f32;
            let mut hm = DWRITE_HIT_TEST_METRICS::default();
            // SAFETY: `offset` is a valid index in the layout's UTF-16 text.
            // Core walks the same string.
            unsafe { layout.HitTestTextPosition(offset, false, &mut px, &mut py, &mut hm) }
                .map_err(refused)?;
            out.push(GlyphBox { x: hm.left, y: hm.top, w: hm.width, h: hm.height });
        }
        Ok(())
    }
}

/// Returns the height that each span gets from its line.
///
/// DirectWrite reports the advance of a whole line, not each range's share.
/// `HitTestTextRange` returns the line's box for every fragment on that line.
/// That call cannot report the share.
///
/// This function recovers each share from the line.
/// DirectWrite sizes a line at the largest value of `size × the advance-per-em ratio of the face`
/// across all ranges on that line.
/// The tallest span fixes that ratio.
/// Each other span gets its own size times the same ratio.
///
/// This method is exact for one font family at mixed sizes.
/// That case matches the 18 `fontSize` dictionaries in the census corpus.
/// A fallback face on the same line makes every span use the tallest face's ratio instead of its
/// own face.
/// That result is never taller than the line.
fn span_heights(spans: &[StyledSpan<'_>], out: &mut Measured) {
    for (i, geom) in out.lines.iter().enumerate() {
        let i = i as u32;
        let tallest = out
            .spans
            .iter()
            .filter(|b| b.line == i)
            .filter_map(|b| spans.get(b.span as usize))
            .fold(0.0f32, |a, s| a.max(s.size));
        if tallest <= 0.0 {
            continue;
        }
        let per_em = geom.h / tallest;
        for b in out.spans.iter_mut().filter(|b| b.line == i) {
            b.h = spans.get(b.span as usize).map_or(geom.h, |s| s.size * per_em);
        }
    }
}

/// Builds one popup scene in DIPs.
///
/// The `box_dip` parameter packs both halves of the box into one argument.
/// The function always resolves and passes both halves together.
/// This packing leaves room for the `render` parameter without a seventh loose parameter.
fn scene_of(
    text: &Text,
    p: &Presentation,
    theme: &Theme,
    box_dip: (f32, f32),
    show_back: bool,
    side_panel: bool,
    render: RenderSettings,
) -> Result<PopupScene> {
    // On Windows, the Anki control is a child window with its own font and size.
    // Windows therefore draws the label itself, and this call leaves core's `anki` slot empty.
    layout::scene(
        &SceneRequest {
            presentation: p,
            theme,
            max_w: box_dip.0,
            max_h: box_dip.1,
            show_back,
            side_panel,
            render,
            anki: None,
        },
        &mut text.measurer(),
    )
    .map_err(anyhow::Error::new)
    .context("laying out the popup")
}

/// Returns `(width, view_h, content_h)`.
///
/// This function returns all three values in physical pixels.
/// The scene uses DIPs, and this function alone applies the scale again.
fn popup_size(scene: &PopupScene, scale: f32, max_w: i32) -> (i32, i32, i32) {
    (
        // Without a side column, this code keeps the caller's width exactly.
        // It does not convert the width through the scale.
        scene.panel_w.map_or(max_w, |w| (w * scale).ceil() as i32),
        (scene.view_h * scale).ceil() as i32,
        (scene.content_h * scale).ceil() as i32,
    )
}

/// `Renderer` holds the Direct2D state for one window.
pub struct Renderer {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    text: Text,
    /// The field stays empty until the window has a real size. A new window starts at zero by zero.
    target: Option<ID2D1HwndRenderTarget>,
    /// The hit targets from the last paint pass, in DIPs, exactly as the scene reported them.
    hits: RefCell<Vec<HitTarget>>,
    /// The painter's decoded-asset cache, or `None` when this build cannot open the dictionary
    /// database.
    ///
    /// This field uses `RefCell` for the same reason as `hits` and the format cache.
    /// `paint_once` takes `&self`, and this window uses one thread by construction.
    /// `None` is a real state, not a failure.
    /// The popup still paints, and every image uses its `alt`-text rung.
    media: RefCell<Option<MediaSurfaces>>,
}

impl Renderer {
    /// Creates the factories only. This function does not create a target.
    ///
    /// The `db` parameter names the dictionary database.
    /// The painter opens its own read-only connection to `db` because the worker owns the
    /// dictionary on another thread.
    /// A `rusqlite::Connection` is not `Sync`.
    pub fn new(hwnd: HWND, db: &std::path::Path) -> Result<Renderer> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .context("D2D1CreateFactory")?;
        // If this build cannot open the database, the panel loses its images but keeps its frame.
        // An image node then renders its `alt` text, the character that the image represents.
        let media = match MediaSurfaces::open(db) {
            Ok(cache) => Some(cache),
            Err(e) => {
                eprintln!(
                    "chibipop: no dictionary media - {e:#}; image nodes will render \
                     their alt text"
                );
                None
            }
        };
        Ok(Renderer {
            hwnd,
            d2d_factory,
            text: Text::new()?,
            target: None,
            hits: RefCell::new(Vec::new()),
            media: RefCell::new(media),
        })
    }

    fn dpi_scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
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

    /// Returns `(width, view_h, content_h)`.
    ///
    /// This function returns all three values in physical pixels.
    /// The `box_phys` parameter also uses physical pixels and packs both halves into one argument,
    /// as [`scene_of`] does.
    /// The function always resolves and passes both halves together.
    /// Separate parameters would exceed the clippy argument cap without improving clarity.
    pub fn measure(
        &mut self,
        p: &Presentation,
        theme: &Theme,
        box_phys: (i32, i32),
        show_back: bool,
        side_panel: bool,
        render: RenderSettings,
    ) -> Result<(i32, i32, i32)> {
        let (max_w, max_h) = box_phys;
        let scale = self.dpi_scale();
        let scene = scene_of(
            &self.text,
            p,
            theme,
            (max_w as f32 / scale, max_h as f32 / scale),
            show_back,
            side_panel,
            render,
        )?;
        Ok(popup_size(&scene, scale, max_w))
    }

    /// Paints into the client rect.
    ///
    /// A device loss triggers one retry.
    /// The function builds the scene before the retry loop because only paint can lose a device.
    /// Measure cannot lose a device.
    pub fn paint(
        &mut self,
        p: &Presentation,
        theme: &Theme,
        scroll: i32,
        show_back: bool,
        side_panel: bool,
        render: RenderSettings,
    ) -> Result<()> {
        let (cw, ch) = self.client_size().context("querying the popup's client size")?;
        self.ensure_target(cw, ch).context("preparing the D2D render target")?;

        let scale = self.dpi_scale();
        let w = (cw as f32 / scale) as i32;
        let h = (ch as f32 / scale) as i32;
        let scroll = (scroll as f32 / scale) as i32;

        let scene =
            scene_of(&self.text, p, theme, (w as f32, h as f32), show_back, side_panel, render)?;
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

    /// Creates or resizes the Direct2D target.
    fn ensure_target(&mut self, w: i32, h: i32) -> Result<()> {
        let size = D2D_SIZE_U {
            width: w.max(1) as u32,
            height: h.max(1) as u32,
        };

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
        let target = unsafe {
            self.d2d_factory
                .CreateHwndRenderTarget(&rt_props, &hwnd_props)
        }
        .context("ID2D1Factory::CreateHwndRenderTarget")?;
        self.target = Some(target);
        Ok(())
    }

    /// Runs one `BeginDraw` and `EndDraw` cycle.
    ///
    /// This function returns the raw Direct2D error.
    /// The caller, `paint`, matches that error.
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
        let scope = DrawScope {
            target: Some(target),
        };

        let draw_result: windows::core::Result<()> = (|| {
            unsafe {
                target.Clear(Some(&color_f(theme.background)));

                let bg_brush = target.CreateSolidColorBrush(&color_f(theme.background), None)?;
                let panel = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: w as f32,
                        bottom: h as f32,
                    },
                    radiusX: theme.corner_radius as f32,
                    radiusY: theme.corner_radius as f32,
                };
                target.FillRoundedRectangle(&panel, &bg_brush);

                let border_brush = target.CreateSolidColorBrush(&color_f(theme.border), None)?;
                target.DrawRoundedRectangle(&panel, &border_brush, theme.border_width, None);
            }

            // One buffer holds the spans for every element.
            // A rich entry therefore needs no allocation for each element on each frame.
            let mut spans: Vec<StyledSpan<'_>> = Vec::new();
            for painted in scene.visible(scroll as f32, h as f32) {
                self.draw_elem(target, theme, painted.elem, painted.pen, &mut spans)?;
            }

            if let Some(side) = &scene.side {
                let sep_brush =
                    unsafe { target.CreateSolidColorBrush(&color_f(theme.separator), None) }?;
                // This rule spans the full height.
                // It divides the whole panel, not only the column.
                let sep = D2D_RECT_F {
                    left: side.rule_x,
                    top: side.origin_y,
                    right: side.rule_x + side.rule_w,
                    bottom: (h - theme.padding) as f32,
                };
                unsafe { target.FillRectangle(&sep, &sep_brush) };

                for painted in side.visible(scroll as f32, h as f32) {
                    // The whole column uses one role, the collapsed role.
                    let span = StyledSpan {
                        text: &painted.row.text,
                        font: &theme.font_name,
                        size: theme.collapsed_size,
                        weight: theme.collapsed_weight,
                        italic: theme.collapsed_italic,
                        color: painted.row.color,
                    };
                    let at = Vector2 { X: side.col_x, Y: painted.y };
                    self.draw_spans(
                        target,
                        &[span],
                        &[0.0],
                        side.col_w,
                        at,
                        Align::Leading,
                    )?;
                }
            }

            // Core derives the track and content height from the scene.
            // This renderer, the Linux painter, and the golden capture therefore agree on how
            // padding affects both values.
            if let Some((top, thumb_h)) = scene.scrollbar_thumb(theme.padding, h, scroll) {
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

        // The draw error comes first because it is more specific.
        // `EndDraw` only signals a device loss.
        draw_result.and(end_result)
    }

    /// Draws one scene element.
    fn draw_elem<'a>(
        &self,
        target: &ID2D1HwndRenderTarget,
        theme: &'a Theme,
        elem: &'a SceneElem,
        pen: (f32, f32),
        spans: &mut Vec<StyledSpan<'a>>,
    ) -> windows::core::Result<()> {
        if elem.kind == ElemKind::Separator {
            let brush = unsafe { target.CreateSolidColorBrush(&color_f(elem.color), None) }?;
            let rect = D2D_RECT_F {
                left: elem.rect.x,
                top: pen.1,
                right: elem.rect.x + elem.rect.w,
                bottom: pen.1 + elem.rect.h,
            };
            unsafe { target.FillRectangle(&rect, &brush) };
            return Ok(());
        }

        // This code draws the boxes first, below the text.
        // A pill's fill stays behind the label that it tints.
        // A block's border frames the paragraph inside it.
        // The scene already resolves every rect, so this walk makes no decisions.
        // This walk only draws.
        let dy = pen.1 - elem.pen.1;
        for b in elem.boxes() {
            self.draw_box(target, b, dy)?;
        }
        // An image uses a ladder.
        // It tries the asset when this build can decode it, then the `alt` run that the element
        // already carries, and then an outlined box.
        // The image always renders something because an empty result would leave a hole in a word.
        if let Some(img) = &elem.image {
            if self.draw_asset(target, elem, img, pen.1, theme)? {
                return Ok(());
            }
            if elem.spans.is_empty() {
                self.draw_placeholder(target, elem.rect_at(pen.1), elem.color)?;
                return Ok(());
            }
        }
        // An empty table cell has nothing to shape.
        // It draws its border and no text, and the `TextMeasure` seam receives no empty run.
        if elem.spans.is_empty() {
            return Ok(());
        }

        // One layout covers the whole element, regardless of its styles.
        // Its spans wrap as one paragraph.
        // If the code paints each span alone, it breaks the lines that the scene already measured
        // (ARCHITECTURE.md#popup-and-measurement).
        spans.clear();
        spans.extend(elem.styled_spans(&theme.font_name));
        let shifts: Vec<f32> = elem.spans.iter().map(|s| s.shift).collect();
        let at = Vector2 { X: pen.0, Y: pen.1 };
        self.draw_spans(
            target,
            spans,
            &shifts,
            elem.wrap_w,
            at,
            elem.align,
        )?;

        // This loop draws each reading over the base characters that it annotates.
        // A reading is not a span in the layout above.
        // It takes no horizontal room from the line that holds it.
        // The code draws each reading at the position that the scene already placed.
        // It also uses the element's wrap width, which the scene already measured.
        for run in &elem.ruby {
            let at = Vector2 { X: pen.0 + run.x, Y: pen.1 + run.y };
            let span = [run.styled_span(&theme.font_name)];
            self.draw_spans(target, &span, &[0.0], elem.wrap_w, at, Align::Leading)?;
        }

        // This loop draws list markers in the gutters that their lists open.
        // Each marker always uses `Align::Leading` from the pen position.
        // A marker box sits at the item's *content* edge, regardless of text alignment.
        // `textAlign` moves line boxes only. A marker box is not a line box.
        for run in &elem.marker {
            let at = Vector2 { X: pen.0 + run.x, Y: pen.1 + run.y };
            let span = [run.styled_span(&theme.font_name)];
            self.draw_spans(target, &span, &[0.0], elem.wrap_w, at, Align::Leading)?;
        }
        Ok(())
    }

    /// Draws one scene box with its fill first and its border second.
    ///
    /// The `dy` parameter gives the scroll shift for the element.
    /// The box rect uses unscrolled panel space, like every other rect that the scene reports.
    /// `dy` moves it to the current scroll position.
    ///
    /// `DrawRoundedRectangle` centers its stroke on the path.
    /// This function therefore insets the rect by half the border width, so the border stays inside
    /// the box.
    /// This placement follows the CSS border box model.
    /// `BoxStyle::even_border` decides whether one stroke is enough, not this painter.
    /// A rounded corner between two border widths has no single path.
    /// The census corpus has only one asymmetric border: a one-sided rule that a rect draws
    /// exactly.
    ///
    /// Every drawn style uses a solid stroke.
    /// The scene still carries `dashed`, `dotted`, and `double` for a later painter.
    /// The census corpus shows only `solid` and `none`.
    fn draw_box(
        &self,
        target: &ID2D1HwndRenderTarget,
        b: &layout::ElemBox,
        dy: f32,
    ) -> windows::core::Result<()> {
        let (x, y, w, h) = (b.rect.x, b.rect.y + dy, b.rect.w, b.rect.h);
        if w <= 0.0 || h <= 0.0 {
            return Ok(());
        }
        let radius = b.style.radius;
        let round = |x: f32, y: f32, w: f32, h: f32, r: f32| D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h },
            radiusX: r.max(0.0),
            radiusY: r.max(0.0),
        };
        if let Some(bg) = b.style.background {
            let brush = unsafe { target.CreateSolidColorBrush(&color_f(bg), None) }?;
            unsafe { target.FillRoundedRectangle(&round(x, y, w, h, radius), &brush) };
        }

        let e = b.style.border_used();
        if e == layout::Edges::default() {
            return Ok(());
        }
        let brush = unsafe { target.CreateSolidColorBrush(&color_f(b.style.border_color), None) }?;
        if let Some(width) = b.style.even_border() {
            let inset = width / 2.0;
            let edge = round(x + inset, y + inset, w - width, h - width, radius - inset);
            unsafe { target.DrawRoundedRectangle(&edge, &brush, width, None) };
            return Ok(());
        }
        let side = |x: f32, y: f32, w: f32, h: f32| {
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let rect = D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h };
            unsafe { target.FillRectangle(&rect, &brush) };
        };
        side(x, y, w, e.top);
        side(x, y + h - e.bottom, w, e.bottom);
        side(x, y, e.left, h);
        side(x + w - e.right, y, e.right, h);
        Ok(())
    }

    /// Composites one asset into its resolved box.
    /// `false` means that the caller must continue to the next rung of the ladder.
    ///
    /// This function draws the background fill first, but only when the node asks for one.
    /// Yomitan draws a background fill behind a transparent asset, and every image node in the
    /// census corpus turns that fill off.
    /// The panel background supplies this fill.
    /// A light gray fill behind a gaiji would create the only opaque patch on a dark theme.
    ///
    /// This function creates an `ID2D1Bitmap` for each paint. It does not cache the bitmap.
    /// `ui::media`'s own header gives the reason: a device bitmap belongs to a render target and
    /// dies with the target after a device loss.
    /// The bytes behind the bitmap outlive that loss, so the paint pass uploads them.
    /// A hover shows only a few gaiji at a time, so each damage event needs only a few bitmap
    /// uploads and no image re-decode.
    fn draw_asset(
        &self,
        target: &ID2D1HwndRenderTarget,
        elem: &SceneElem,
        img: &SceneImage,
        pen_y: f32,
        theme: &Theme,
    ) -> windows::core::Result<bool> {
        let rect = elem.rect_at(pen_y);
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return Ok(false);
        }
        let Some(key) = img.key.as_ref() else { return Ok(false) };
        let mut held = self.media.borrow_mut();
        let Some(cache) = held.as_mut() else { return Ok(false) };
        // Core chooses the tint, so both bins take the expensive tint path for the same assets
        // (`SceneImage::tint`).
        let tint = img.tint(elem.rect, elem.font_size, self.dpi_scale());
        let Ok(bitmap) = cache.bitmap(key, tint, elem.color) else {
            return Ok(false);
        };
        let size = D2D_SIZE_U { width: bitmap.w, height: bitmap.h };
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        // SAFETY: `bitmap.bgra` holds `w * h * 4` bytes with no row padding.
        // `stride` reports that layout, and `size` describes it as well.
        let uploaded = unsafe {
            target.CreateBitmap(size, Some(bitmap.bgra.as_ptr().cast()), bitmap.stride(), &props)
        }?;
        if img.background {
            let brush = unsafe { target.CreateSolidColorBrush(&color_f(theme.background), None) }?;
            let box_ = D2D_RECT_F {
                left: rect.x,
                top: rect.y,
                right: rect.x + rect.w,
                bottom: rect.y + rect.h,
            };
            unsafe { target.FillRectangle(&box_, &brush) };
        }
        let box_ = D2D_RECT_F {
            left: rect.x,
            top: rect.y,
            right: rect.x + rect.w,
            bottom: rect.y + rect.h,
        };
        unsafe {
            target.DrawBitmap(
                &uploaded,
                Some(&box_),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            )
        };
        Ok(true)
    }

    /// Draws the last rung: an outlined box where the character belongs.
    ///
    /// This function outlines the box. It does not fill the box.
    /// A solid block would look like a censored glyph, but an empty frame shows an absent glyph.
    /// The asset that the box replaces is usually a character in this same color.
    fn draw_placeholder(
        &self,
        target: &ID2D1HwndRenderTarget,
        rect: SceneRect,
        color: layout::Rgb,
    ) -> windows::core::Result<()> {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return Ok(());
        }
        let brush = unsafe { target.CreateSolidColorBrush(&color_f(color), None) }?;
        let edge = D2D_RECT_F {
            left: rect.x + PLACEHOLDER_EDGE / 2.0,
            top: rect.y + PLACEHOLDER_EDGE / 2.0,
            right: rect.x + rect.w - PLACEHOLDER_EDGE / 2.0,
            bottom: rect.y + rect.h - PLACEHOLDER_EDGE / 2.0,
        };
        if edge.right <= edge.left || edge.bottom <= edge.top {
            // The box is too small for an outline. A filled speck still reads as ink.
            let box_ = D2D_RECT_F {
                left: rect.x,
                top: rect.y,
                right: rect.x + rect.w,
                bottom: rect.y + rect.h,
            };
            unsafe { target.FillRectangle(&box_, &brush) };
            return Ok(());
        }
        unsafe { target.DrawRectangle(&edge, &brush, PLACEHOLDER_EDGE, None) };
        Ok(())
    }

    /// Draws the spans of one run with each span's color and baseline.
    ///
    /// This function builds one layout with the same path that measures the scene.
    /// It then sets one effect for each span range.
    /// The first span's brush also becomes the layout's default brush, so a one-span run draws as
    /// it did before the seam widened.
    /// Measurement ignores color, and this function applies that styled-span field
    /// (ARCHITECTURE.md#popup-and-measurement).
    ///
    /// A `verticalAlign` value needs one draw call for each *distinct* shift because
    /// `DrawTextLayout` has no per-range baseline.
    /// `draw_spans` builds the layout once, so every pass wraps the text in the same way.
    /// Each pass paints only spans with its shift and gives every other span a fully transparent
    /// brush.
    /// A run without a `verticalAlign` value needs one pass and one `DrawTextLayout` call, as
    /// before.
    /// Every run that the panel's chrome builds has no `verticalAlign` value.
    fn draw_spans(
        &self,
        target: &ID2D1HwndRenderTarget,
        spans: &[StyledSpan<'_>],
        shifts: &[f32],
        max_w: f32,
        at: Vector2,
        align: Align,
    ) -> windows::core::Result<()> {
        if spans.is_empty() {
            return Ok(());
        }
        let layout = self.text.layout(MeasureRun { spans, max_w })?;
        // DirectWrite aligns the whole wrap box, the same box that the scene measured.
        // A centered paragraph that wraps therefore centers each line as the scene's per-line
        // offset needs.
        match align {
            Align::Leading => {}
            Align::Center => unsafe { layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER) }?,
            Align::Trailing => {
                unsafe { layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING) }?
            }
        }
        for i in 0..spans.len() {
            let shift = shift_at(shifts, i);
            // This code handles each distinct shift once.
            // The pass for the first span that asks for a shift draws that shift, so no span paints
            // twice.
            if (0..i).any(|j| shift_at(shifts, j) == shift) {
                continue;
            }
            let mut default = None;
            for (j, range) in ranges(spans).enumerate() {
                let color = match shift_at(shifts, j) == shift {
                    true => color_f(spans[j].color),
                    false => TRANSPARENT,
                };
                let brush = unsafe { target.CreateSolidColorBrush(&color, None) }?;
                // SAFETY: The range names UTF-16 positions inside the layout text.
                unsafe { layout.SetDrawingEffect(&brush, range) }?;
                default.get_or_insert(brush);
            }
            let Some(default) = default else { return Ok(()) };
            let origin = Vector2 { X: at.X, Y: at.Y - shift };
            unsafe {
                target.DrawTextLayout(origin, &layout, &default, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
        Ok(())
    }
}

/// Returns the baseline shift that span `i` requests.
///
/// A run with no shifts passes an empty slice.
/// Every span in that run therefore sits on its own line baseline.
fn shift_at(shifts: &[f32], i: usize) -> f32 {
    shifts.get(i).copied().unwrap_or(0.0)
}

/// Gives the outline width, in DIPs, for a placeholder box.
///
/// One DIP is the hairline width that this target uses elsewhere.
/// Examples include the panel edge, a table cell rule, and the spec's `1em / 14` cell border at
/// base size.
const PLACEHOLDER_EDGE: f32 = 1.0;

/// A brush that paints nothing for a span that this pass skips.
const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

/// Ends the draw on drop.
struct DrawScope<'a> {
    target: Option<&'a ID2D1HwndRenderTarget>,
}

impl DrawScope<'_> {
    /// Ends the draw and returns its result.
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
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}


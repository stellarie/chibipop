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
    self, Align, ElemKind, GlyphBox, HitTarget, LineBox, MeasureError, MeasureRun, Measured,
    Metrics, PopupScene, SceneElem, SceneRequest, SpanBox, StyledSpan, TextMeasure,
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

/// Cached by font+size+weight+style.
#[derive(Default)]
struct FormatCache {
    font: String,
    by_key: HashMap<u64, IDWriteTextFormat>,
}

/// Packs size+weight+style into a key.
///
/// A format is immutable once created,
/// so the cache has to key on every
/// axis a run can vary on, not just
/// the size it used to.
fn format_key(size: f32, weight: u16, italic: bool) -> u64 {
    let s = size.to_bits() as u64;
    let w = (weight as u64) << 32;
    let i = if italic { 1u64 << 48 } else { 0 };
    s | w | i
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

    /// One format per font, size,
    /// weight and style.
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

    /// One wrapped layout.
    ///
    /// The one shaping call in the
    /// bin: measure and paint both come
    /// through here, and through the
    /// same `MeasureRun`, so a run is
    /// never wrapped two ways nor
    /// painted in a weight it was not
    /// measured in.
    ///
    /// The whole run is one layout, so
    /// its spans wrap as one paragraph
    /// and a bold span can share a line
    /// with a normal one (ADR-0013).
    /// The first span's cached
    /// `IDWriteTextFormat` is the base
    /// and the rest override their own
    /// ranges, so a one-span run makes
    /// exactly the calls it always did.
    /// Colour is not here: it changes
    /// no geometry and needs a brush,
    /// which only the paint side has.
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
            // SAFETY: each range names
            // UTF-16 positions inside the
            // string the layout was just
            // built from.
            unsafe {
                layout.SetFontFamilyName(&HSTRING::from(span.font), range)?;
                layout.SetFontWeight(DWRITE_FONT_WEIGHT(span.weight as i32), range)?;
                layout.SetFontStyle(style_of(span.italic), range)?;
                layout.SetFontSize(span.size, range)?;
            }
        }
        Ok(layout)
    }

    /// Borrows it as a `TextMeasure`.
    fn measurer(&self) -> Measurer<'_> {
        Measurer(self)
    }
}

/// What a run with no spans formats as.
///
/// Core never builds one - every
/// element it measures carries a styled
/// span - but a layout needs a family
/// and a size whatever its text is, and
/// an unnamed family is DirectWrite's
/// to refuse rather than this module's
/// to guess at.
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

/// Each span's UTF-16 range in the run.
///
/// The layout is one string end to end,
/// so a span is a range in it - which
/// is the unit both the per-range
/// formatting and `HitTestTextRange`
/// take.
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

/// The line UTF-16 position `at` is on.
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

/// The rects `[at, at + len)` fills.
///
/// One per line the range touches, and
/// one more per bidi run on a line.
/// `HitTestTextRange` reports how many
/// it needed when the buffer was too
/// small, so `hint` covers the ordinary
/// case in one call and the retry
/// covers the rest.
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
    // SAFETY: `at` and `len` name UTF-16
    // positions inside the layout's own
    // text, and the buffer is sized
    // before each call.
    let mut hr = unsafe { layout.HitTestTextRange(at, len, 0.0, 0.0, Some(out), &mut got) };
    if hr.is_err() && got as usize > out.len() {
        out.resize(got as usize, DWRITE_HIT_TEST_METRICS::default());
        hr = unsafe { layout.HitTestTextRange(at, len, 0.0, 0.0, Some(out), &mut got) };
    }
    hr?;
    out.truncate(got as usize);
    Ok(())
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
    fn measure(
        &mut self,
        run: MeasureRun<'_>,
        out: &mut Measured,
    ) -> std::result::Result<(), MeasureError> {
        out.clear();
        let layout = self.0.layout(run).map_err(refused)?;
        // The aggregate is still
        // `GetMetrics`, untouched, so a
        // one-span run measures exactly
        // what it did before the seam
        // widened. Everything below is
        // detail beside it.
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
            // Trailing whitespace out, so
            // a line's width means what
            // `GetMetrics` means by width.
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
        // A line box per counted line,
        // whatever DirectWrite filled:
        // core stacks the gap after an
        // empty run either way.
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
                    // One box per span per
                    // line: a bidi split
                    // reports the same line
                    // twice and the box is
                    // their union.
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
                        // Filled below: a
                        // span's own advance
                        // needs its line's.
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

/// What each span asked its line for.
///
/// DirectWrite reports a line's advance
/// but never a range's share of it:
/// `HitTestTextRange` answers with the
/// line's own box for every fragment on
/// it. The share is recoverable from
/// the line, though, because
/// DirectWrite sizes a line at the
/// largest of `size × the face's
/// advance per em` over its ranges - so
/// the tallest span on a line fixes
/// that ratio, and every other span on
/// it asked for its own size times the
/// same number. Exact for one family at
/// mixed sizes, which is what the
/// census's 18 `fontSize` dictionaries
/// produce; a fallback face on the same
/// line makes it the tallest face's
/// ratio for all of them, which is
/// never more than the line.
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
                    // One role for the
                    // whole column, the
                    // collapsed one.
                    let span = StyledSpan {
                        text: &painted.row.text,
                        font: &theme.font_name,
                        size: theme.collapsed_size,
                        weight: theme.collapsed_weight,
                        italic: theme.collapsed_italic,
                        color: painted.row.color,
                    };
                    let at = Vector2 { X: side.col_x, Y: painted.y };
                    self.draw_spans(target, &[span], side.col_w, at, false)?;
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

        let span = StyledSpan {
            text: &elem.text,
            font: &theme.font_name,
            size: elem.font_size,
            weight: elem.weight,
            italic: elem.italic,
            color: elem.color,
        };
        let at = Vector2 { X: pen.0, Y: pen.1 };
        self.draw_spans(target, &[span], elem.wrap_w, at, elem.align == Align::Trailing)
    }

    /// Draws one run's spans, each in
    /// its own colour.
    ///
    /// One layout - the same shaping
    /// path the scene was measured
    /// against - with a drawing effect
    /// per span range. The first span's
    /// brush is also the layout's
    /// default, so a one-span run draws
    /// exactly what it drew before the
    /// seam widened. Colour is the one
    /// styled-span field measurement
    /// ignores (ADR-0013), and this is
    /// where it is answered.
    fn draw_spans(
        &self,
        target: &ID2D1HwndRenderTarget,
        spans: &[StyledSpan<'_>],
        max_w: f32,
        at: Vector2,
        trailing: bool,
    ) -> windows::core::Result<()> {
        let layout = self.text.layout(MeasureRun { spans, max_w })?;
        if trailing {
            unsafe { layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING) }?;
        }
        let mut first = None;
        for (span, range) in spans.iter().zip(ranges(spans)) {
            let brush = unsafe { target.CreateSolidColorBrush(&color_f(span.color), None) }?;
            // SAFETY: the range names
            // UTF-16 positions inside the
            // layout's own text.
            unsafe { layout.SetDrawingEffect(&brush, range) }?;
            first.get_or_insert(brush);
        }
        let Some(default) = first else { return Ok(()) };
        unsafe {
            target.DrawTextLayout(at, &layout, &default, D2D1_DRAW_TEXT_OPTIONS_NONE);
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
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}


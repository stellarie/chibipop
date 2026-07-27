//! Direct2D + DirectWrite rendering of a [`Presentation`] into the popup's
//! `HWND`.
//!
//! **Render-target choice, and why it is not the spike's.**
//! `docs/superpowers/findings/2026-07-27-m3-win32-d2d-spike.md` built its D2D
//! demo on `ID2D1DCRenderTarget` + `UpdateLayeredWindow`, and that code is
//! reused here for the *mechanics* (factory creation order, the
//! `D2D1_PIXEL_FORMAT` construction, `IDWriteTextFormat`/`DrawText`) - but not
//! for the render-target type. That spike itself measured `UpdateLayeredWindow`
//! (true per-pixel alpha) to be **incompatible** with `WDA_EXCLUDEFROMCAPTURE`:
//! the affinity call fails with a misleading "not enough memory" HRESULT and
//! silently no-ops, leaving the popup fully capturable - see `ui::window`'s
//! module docs. Task 1 built the window around the fix that spike flagged as
//! unverified: `WS_EX_LAYERED` + `SetLayeredWindowAttributes(LWA_ALPHA)`,
//! painted by ordinary `WM_PAINT`. This module paints into that window with
//! `ID2D1HwndRenderTarget`, which is the D2D target type designed to draw into
//! an HWND's own surface rather than an offscreen DIB - no `UpdateLayeredWindow`,
//! no DIB section, no memory DC.
//!
//! **How `paint` reaches the screen.** `Renderer::paint` is called directly by
//! application code (the example here, and eventually `app.rs`'s dispatch)
//! once per hover, immediately after `Popup::show_at` has sized and shown the
//! window - mirroring Task 7's stated sequence "measure, place_popup, show_at,
//! paint". It performs one full `BeginDraw`/.../`EndDraw` cycle itself; it does
//! not wait to be invoked reactively from `wndproc`'s `WM_PAINT` arm. `WM_PAINT`
//! still has to be handled (Windows delivers it regardless, e.g. via the
//! `UpdateWindow` call already inside `show_at`), so `ui::window::wndproc`'s
//! handler now just validates the update region - see that module for why the
//! placeholder magenta fill had to go. Reactively repainting *cached* content
//! from an unrelated `WM_PAINT` (e.g. after the popup is briefly occluded) is
//! not implemented: it would need somewhere to keep "the last thing shown"
//! reachable from a bare `extern "system" fn` (`GWLP_USERDATA` or a global),
//! which is state-ownership plumbing that belongs to `app.rs` (not yet built),
//! not to this task's three-function interface. Flagged here rather than
//! silently built around.
//!
//! **Measuring and painting share one layout walk** (`layout_pass`) so they
//! can never disagree about where content gets cut off: `measure` calls it
//! with `target: None` (DirectWrite layout only, no device); `paint` calls it
//! with `target: Some(&_)` against the *same* per-line heights, so the height
//! `measure` reported is exactly the height `paint` fills.

use crate::present::Presentation;
use crate::ui::theme::Theme;
use anyhow::{Context, Result};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows_numerics::Vector2;

/// Gap above a line that continues the same semantic block (e.g. the
/// reading line right under the headword, or a gloss paragraph right under
/// its dictionary label).
const LINE_GAP: f32 = 4.0;
/// Gap above a line that starts a new block within the card (a new
/// `GlossBlock`'s dictionary label).
const SECTION_GAP: f32 = 10.0;
/// Gap above and below the rule between the top card and the collapsed
/// rows.
const SEPARATOR_MARGIN: f32 = 10.0;
const SEPARATOR_THICKNESS: f32 = 1.0;
/// The truncation marker `paint` draws when content was clamped to the
/// popup's height cap (M3-D4) - drawn in `theme.dimmed_text` so a
/// deliberately-ended entry never looks identical to one that was cut off.
const CLAMP_MARKER: &str = "…";

/// One line of text the layout walk lays out and, in paint mode, draws.
/// Built fresh from a `Presentation`/`Theme` on every `measure`/`paint`
/// call - never cached - so the two never see different content.
struct Line {
    text: String,
    color: (u8, u8, u8),
    size: f32,
    /// Space to leave above this line, on top of whatever space the
    /// previous element already consumed.
    top_gap: f32,
}

enum Elem {
    Text(Line),
    Separator { top_gap: f32 },
}

/// D2D device resources and the DirectWrite factory for one popup `HWND`.
/// One `Renderer` per `Popup`, living as long as the window does - the
/// factories are cheap process-wide singletons in all but name, but the
/// `ID2D1HwndRenderTarget` is sized to this specific window and gets
/// recreated (not just resized) after a device-lost error.
pub struct Renderer {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    /// Lazily created on the first `paint` (not in `new`): the window is
    /// 0x0 until `Popup::show_at` sizes it, and a render target needs a
    /// real pixel size. `measure` never touches this - it is pure
    /// DirectWrite layout, no device involved.
    target: Option<ID2D1HwndRenderTarget>,
}

impl Renderer {
    /// Creates the D2D and DirectWrite factories for `hwnd`. Does not yet
    /// create the render target - see the `target` field's doc comment.
    pub fn new(hwnd: HWND) -> Result<Renderer> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .context("D2D1CreateFactory")?;
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                .context("DWriteCreateFactory")?;
        Ok(Renderer { hwnd, d2d_factory, dwrite_factory, target: None })
    }

    /// Computes the popup's `(width, height)` for `p`/`theme` before the
    /// window is shown, so `geom::place_popup` can be given a correct size.
    /// `max_w` is returned unchanged - the popup does not shrink its width
    /// to fit short content, only its height grows with content, up to
    /// `max_h`.
    ///
    /// **The returned height is always `<= max_h`, unconditionally** - the
    /// final `.min(max_h)` below is a deliberate, redundant-by-construction
    /// backstop, not just the loop's own accounting. `geom::place_popup`
    /// proves the popup never covers the character being read *only* when
    /// handed a height within the 45%-of-monitor-height cap; it has no way
    /// to enforce that itself, since it is never told what produced the
    /// size (see `geom.rs`'s `place_popup` docs). An unclamped height here
    /// would silently break that guarantee for exactly the long entries
    /// where truncation was supposed to kick in.
    ///
    /// The third element of the tuple is `true` when content did not fit
    /// and was cut off - `paint` independently re-derives the same fact
    /// from the window's actual (by-then-clamped) client size, so the
    /// truncation marker it draws always agrees with what `measure` decided
    /// here; see the module docs.
    ///
    /// **Deliberate deviation from the task brief's literal
    /// `Result<(i32, i32)>` signature.** The brief also requires `measure`
    /// to "report that it clamped, so the caller can draw the truncation
    /// marker" - a 2-tuple has nowhere to put that. Extended to a 3-tuple
    /// rather than dropping either requirement; flagged here and in the
    /// task report rather than silently picking one.
    pub fn measure(
        &self,
        p: &Presentation,
        theme: &Theme,
        max_w: i32,
        max_h: i32,
    ) -> Result<(i32, i32, bool)> {
        let elems = build_elements(p, theme);
        let content_w = (max_w - 2 * theme.padding).max(0) as f32;
        let content_h = (max_h - 2 * theme.padding).max(0) as f32;
        let (used_h, clamped) =
            layout_pass(&self.dwrite_factory, None, &elems, theme, 0.0, 0.0, content_w, content_h)
                .context("measuring popup content")?;
        let h = (used_h.ceil() as i32 + 2 * theme.padding).min(max_h);
        Ok((max_w, h, clamped))
    }

    /// Paints `p`/`theme` into the window's current client rect (already
    /// sized by `Popup::show_at` to what `measure` returned). Draws, in
    /// order: the panel background, the top card (headword, reading, POS,
    /// frequency, then each `GlossBlock`), a separator, each collapsed row,
    /// and - only when content did not fit - a dimmed truncation marker.
    ///
    /// Device-lost (`D2DERR_RECREATE_TARGET`, e.g. a display mode change, a
    /// driver reset, or an RDP session change) is handled here: the stale
    /// target is dropped, a fresh one is created against the current size,
    /// and painting is retried once, immediately, before this call returns.
    /// The failed `BeginDraw`/`EndDraw` pair is the one dropped frame - D2D
    /// gives no guarantee its content reached the screen - but the retry
    /// means the caller still gets a correctly painted popup back, not an
    /// error and not a crash.
    pub fn paint(&mut self, p: &Presentation, theme: &Theme) -> Result<()> {
        let (w, h) = self.client_size().context("querying the popup's client size")?;
        self.ensure_target(w, h).context("preparing the D2D render target")?;

        if let Err(e) = self.paint_once(p, theme, w, h) {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.ensure_target(w, h)
                    .context("recreating the D2D render target after device loss")?;
                self.paint_once(p, theme, w, h)
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

    /// Creates the render target on first use, or resizes the existing one
    /// (cheap - keeps the same device resources) when the window's size has
    /// changed since the last paint.
    fn ensure_target(&mut self, w: i32, h: i32) -> Result<()> {
        let size = D2D_SIZE_U { width: w.max(1) as u32, height: h.max(1) as u32 };

        if let Some(target) = &self.target {
            unsafe { target.Resize(&size) }.context("ID2D1HwndRenderTarget::Resize")?;
            return Ok(());
        }

        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                // The window's own translucency comes entirely from
                // `SetLayeredWindowAttributes(LWA_ALPHA)` at the OS
                // compositing level (M3-D7) - an HWND render target is
                // always opaque within its own client rect, so there is no
                // per-pixel alpha channel for D2D to manage here.
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 0.0,
            dpiY: 0.0,
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

    /// One full `BeginDraw`/.../`EndDraw` cycle. Returns the raw
    /// `windows::core::Error` (not yet wrapped in `anyhow` context) so
    /// `paint` can distinguish `D2DERR_RECREATE_TARGET` from every other
    /// failure before deciding whether to retry.
    fn paint_once(
        &self,
        p: &Presentation,
        theme: &Theme,
        w: i32,
        h: i32,
    ) -> windows::core::Result<()> {
        let target = self
            .target
            .as_ref()
            .expect("ensure_target must run before paint_once");

        unsafe { target.BeginDraw() };

        // `BeginDraw`/`EndDraw` must always be paired, even when something
        // in between fails - an early `?` return from this span (e.g.
        // `CreateSolidColorBrush` failing) would otherwise skip `EndDraw`
        // and leave the render target mid-draw for the *next* `paint`
        // call. Everything fallible between `BeginDraw` and `EndDraw` runs
        // in this closure instead, so the `EndDraw` below always runs.
        let draw_result: windows::core::Result<()> = (|| {
            let elems = build_elements(p, theme);

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

            let content_w = (w - 2 * theme.padding).max(0) as f32;
            let content_h = (h - 2 * theme.padding).max(0) as f32;
            let origin = theme.padding as f32;
            layout_pass(&self.dwrite_factory, Some(target), &elems, theme, origin, origin, content_w, content_h)?;
            Ok(())
        })();

        let end_result = unsafe { target.EndDraw(None, None) };

        // Prioritise the draw-phase error when both fail - it is usually
        // more specific (e.g. a font/format failure) than whatever EndDraw
        // reports for an already-broken span. A draw phase that *succeeded*
        // must still surface whatever EndDraw returns, since that is where
        // device-lost is authoritatively signalled.
        draw_result.and(end_result)
    }
}

fn color_f((r, g, b): (u8, u8, u8)) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
}

/// Turns `p`'s content into an ordered list of drawable elements, exactly
/// as `present.rs` ordered them: headword, reading, POS/frequency, then
/// each `GlossBlock` (dictionary label + its glosses joined into one
/// paragraph), a separator, then each `CollapsedRow`. Pure with respect to
/// D2D/DirectWrite - no factory or render target touched here, only plain
/// data - so `measure` and `paint` build identical input to `layout_pass`.
fn build_elements(p: &Presentation, theme: &Theme) -> Vec<Elem> {
    let mut out = Vec::new();

    if let Some(card) = &p.top {
        let headword = card.written.clone().or_else(|| card.reading.clone()).unwrap_or_default();
        if !headword.is_empty() {
            out.push(Elem::Text(Line {
                text: headword,
                color: theme.headword_text,
                size: theme.headword_size,
                top_gap: 0.0,
            }));
        }

        // The reading gets its own line only when the headword line above
        // showed `written`, not the reading itself - otherwise it would
        // just repeat what is already on screen.
        if card.written.is_some() {
            if let Some(reading) = card.reading.as_deref().filter(|r| !r.is_empty()) {
                out.push(Elem::Text(Line {
                    text: reading.to_string(),
                    color: theme.reading_text,
                    size: theme.body_size,
                    top_gap: LINE_GAP,
                }));
            }
        }

        if let Some(text) = pos_freq_line(&card.pos, card.freq) {
            out.push(Elem::Text(Line {
                text,
                color: theme.body_text,
                size: theme.body_size,
                top_gap: LINE_GAP,
            }));
        }

        for block in &card.blocks {
            out.push(Elem::Text(Line {
                text: block.dict_name.clone(),
                color: theme.dict_label_text,
                size: theme.collapsed_size,
                top_gap: SECTION_GAP,
            }));
            if !block.glosses.is_empty() {
                out.push(Elem::Text(Line {
                    text: block.glosses.join("; "),
                    color: theme.body_text,
                    size: theme.body_size,
                    top_gap: LINE_GAP,
                }));
            }
        }
    }

    if !p.collapsed.is_empty() {
        out.push(Elem::Separator { top_gap: SEPARATOR_MARGIN });
        for (i, row) in p.collapsed.iter().enumerate() {
            let head = row.written.clone().or_else(|| row.reading.clone()).unwrap_or_default();
            let text =
                if head.is_empty() { row.summary.clone() } else { format!("{head} — {}", row.summary) };
            out.push(Elem::Text(Line {
                text,
                color: theme.collapsed_text,
                size: theme.collapsed_size,
                top_gap: if i == 0 { SEPARATOR_MARGIN } else { LINE_GAP },
            }));
        }
    }

    out
}

fn pos_freq_line(pos: &[String], freq: Option<i64>) -> Option<String> {
    let pos_part = if pos.is_empty() { None } else { Some(pos.join(", ")) };
    let freq_part = freq.map(|f| format!("freq {f}"));
    match (pos_part, freq_part) {
        (Some(p), Some(f)) => Some(format!("{p} · {f}")),
        (Some(p), None) => Some(p),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    }
}

/// The layout walk shared by `measure` (`target: None`) and `paint_once`
/// (`target: Some(_)`): both build `elems` from `build_elements` and call
/// this with the same content box, so a line either measures and paints
/// identically, or - past the point where it stops fitting - is clamped
/// identically in both. `origin_x`/`origin_y` are where `(0, 0)` in this
/// walk's own coordinate space lands in the render target (the padded
/// panel's top-left corner in paint mode; irrelevant, so `0.0, 0.0`, in
/// measure mode, since nothing is actually drawn).
///
/// Returns the total content height used (always `<= content_h`) and
/// whether anything was cut off.
fn layout_pass(
    dwrite: &IDWriteFactory,
    target: Option<&ID2D1HwndRenderTarget>,
    elems: &[Elem],
    theme: &Theme,
    origin_x: f32,
    origin_y: f32,
    content_w: f32,
    content_h: f32,
) -> windows::core::Result<(f32, bool)> {
    let font_name = HSTRING::from(theme.font_name.as_str());
    let mut y = 0.0f32;
    let mut clamped = false;

    for elem in elems {
        let drawn_height = match elem {
            Elem::Separator { top_gap } => {
                let h = SEPARATOR_THICKNESS;
                if y + top_gap + h > content_h {
                    clamped = true;
                    break;
                }
                y += top_gap;
                if let Some(t) = target {
                    let brush = unsafe { t.CreateSolidColorBrush(&color_f(theme.separator), None) }?;
                    let rect = D2D_RECT_F {
                        left: origin_x,
                        top: origin_y + y,
                        right: origin_x + content_w,
                        bottom: origin_y + y + h,
                    };
                    unsafe { t.FillRectangle(&rect, &brush) };
                }
                h
            }
            Elem::Text(line) => {
                let (layout, h) = build_text_layout(dwrite, &font_name, line, content_w)?;
                if y + line.top_gap + h > content_h {
                    clamped = true;
                    break;
                }
                y += line.top_gap;
                if let Some(t) = target {
                    let brush = unsafe { t.CreateSolidColorBrush(&color_f(line.color), None) }?;
                    unsafe {
                        t.DrawTextLayout(
                            Vector2 { X: origin_x, Y: origin_y + y },
                            &layout,
                            &brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        )
                    };
                }
                h
            }
        };
        y += drawn_height;
    }

    if clamped {
        let marker = Line {
            text: CLAMP_MARKER.to_string(),
            color: theme.dimmed_text,
            size: theme.body_size,
            top_gap: SECTION_GAP,
        };
        let (layout, h) = build_text_layout(dwrite, &font_name, &marker, content_w)?;
        if y + marker.top_gap + h <= content_h {
            y += marker.top_gap;
            if let Some(t) = target {
                let brush = unsafe { t.CreateSolidColorBrush(&color_f(marker.color), None) }?;
                unsafe {
                    t.DrawTextLayout(
                        Vector2 { X: origin_x, Y: origin_y + y },
                        &layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    )
                };
            }
            y += h;
        }
        // If even the marker does not fit, there is no narrower fallback -
        // the walk simply stops with `clamped: true` and whatever `y` it
        // had reached.
    }

    Ok((y, clamped))
}

/// Builds one `IDWriteTextLayout` for `line`, word-wrapped to `max_w`, and
/// returns it alongside its measured height. Used identically by both
/// measuring and drawing - `paint_once` draws exactly this layout object at
/// a computed origin, so the two can never disagree about how a given
/// string wraps.
fn build_text_layout(
    dwrite: &IDWriteFactory,
    font_name: &HSTRING,
    line: &Line,
    max_w: f32,
) -> windows::core::Result<(IDWriteTextLayout, f32)> {
    let format = unsafe {
        dwrite.CreateTextFormat(
            font_name,
            None,
            DWRITE_FONT_WEIGHT_REGULAR,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            line.size,
            w!("ja-JP"),
        )
    }?;
    let wide: Vec<u16> = line.text.encode_utf16().collect();
    let layout = unsafe { dwrite.CreateTextLayout(&wide, &format, max_w.max(1.0), f32::MAX) }?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics) }?;
    Ok((layout, metrics.height))
}

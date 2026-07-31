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
//! **Measuring and painting share one layout walk** (`layout_pass`) so they can
//! never disagree about where a line lands: `measure` calls it with
//! `target: None` (DirectWrite layout only, no device); `paint` calls it with
//! `target: Some(&_)` against the *same* per-line heights.
//!
//! **The walk no longer cuts content off.** It used to stop at the first
//! element that would not fit and draw a `…` marker. Now it visits every
//! element and `paint` offsets the whole walk by a scroll amount, letting D2D
//! clip to the window — so a long entry is scrolled rather than truncated, and
//! `measure` reports the natural height the scroll range is computed from. The
//! window's own height is still capped by the caller, because
//! `geom::place_popup`'s proof that the popup never covers the character being
//! read holds only within that cap.

use crate::present::Presentation;
use crate::ui::theme::{Theme, SCROLLBAR_MIN_THUMB, SCROLLBAR_W};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::HashMap;
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
/// Horizontal gap kept clear between an `Elem::Corner` and the line it
/// shares, so the frequency never touches a wide headword.
const CORNER_GAP: f32 = 8.0;
/// Gap above and below the rule between the top card and the collapsed
/// rows.
const SEPARATOR_MARGIN: f32 = 10.0;
const SEPARATOR_THICKNESS: f32 = 1.0;

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
    /// Drawn right-aligned at whatever `y` the walk has reached, without
    /// advancing it, so it shares a line with the `Elem::Text` that follows
    /// (the frequency, sitting in the card's top-right corner beside the
    /// headword). The width it occupies is reserved off that next line, so
    /// a long headword wraps instead of running underneath it.
    Corner(Line),
    Separator { top_gap: f32 },
}

/// How far the content can scroll: the overflow past the visible height, or
/// zero when it fits.
pub fn max_scroll(content_h: i32, view_h: i32) -> i32 {
    (content_h - view_h).max(0)
}

/// The scrollbar thumb as `(top, height)` within a `track_h`-tall track, or
/// `None` when the content fits and no scrollbar should be drawn.
///
/// The thumb's height is the visible fraction of the content, floored at
/// [`SCROLLBAR_MIN_THUMB`] — but never taller than the track itself, so a track
/// shorter than the floor still gets a thumb that fits. The floor is applied
/// *before* positioning, so a floored thumb ends flush with the track at full
/// scroll rather than overhanging it.
pub fn scrollbar_thumb(
    track_h: i32,
    content_h: i32,
    view_h: i32,
    scroll: i32,
) -> Option<(i32, i32)> {
    let span = max_scroll(content_h, view_h);
    if span == 0 || track_h <= 0 || content_h <= 0 {
        return None;
    }
    let ideal = (i64::from(track_h) * i64::from(view_h) / i64::from(content_h)) as i32;
    let thumb_h = ideal.clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
    let travel = track_h - thumb_h;
    let at = scroll.clamp(0, span);
    let top = (i64::from(travel) * i64::from(at) / i64::from(span)) as i32;
    Some((top, thumb_h))
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
    /// Text formats, reused across paints.
    formats: RefCell<FormatCache>,
}

/// One font's formats, by size.
#[derive(Default)]
struct FormatCache {
    font: String,
    by_size: HashMap<u32, IDWriteTextFormat>,
}

/// Creates one format per font and size.
///
/// `CreateTextFormat` ran once per line, per paint, per hover. Alignment is
/// set on the *layout*, never the format, so sharing one is safe.
fn text_format(
    dwrite: &IDWriteFactory,
    formats: &RefCell<FormatCache>,
    font: &str,
    size: f32,
) -> windows::core::Result<IDWriteTextFormat> {
    let key = size.to_bits();
    {
        let cache = formats.borrow();
        if cache.font == font {
            if let Some(f) = cache.by_size.get(&key) {
                return Ok(f.clone());
            }
        }
    }

    let created = unsafe {
        dwrite.CreateTextFormat(
            &HSTRING::from(font),
            None,
            DWRITE_FONT_WEIGHT_REGULAR,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("ja-JP"),
        )
    }?;

    let mut cache = formats.borrow_mut();
    if cache.font != font {
        cache.font = font.to_string();
        cache.by_size.clear();
    }
    cache.by_size.insert(key, created.clone());
    Ok(created)
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
        Ok(Renderer {
            hwnd,
            d2d_factory,
            dwrite_factory,
            target: None,
            formats: RefCell::default(),
        })
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
    /// Returns `(width, view_h, content_h)`: the width, the **visible** height
    /// after the cap, and the content's **natural** height before it.
    ///
    /// `view_h` is what the window is sized to and what `place_popup` must be
    /// handed. `content_h` is what `scroll` ranges against — the caller gets
    /// `max_scroll` from the difference, and a `content_h` greater than
    /// `view_h` is exactly the condition for drawing a scrollbar. This
    /// replaced an earlier `clamped: bool`, which was a lossy summary of the
    /// same fact.
    ///
    /// The measure walk runs with an effectively infinite content box, since
    /// nothing is drawn in measure mode and the point is to reach every
    /// element rather than stop at the first that would not fit.
    pub fn measure(
        &self,
        p: &Presentation,
        theme: &Theme,
        max_w: i32,
        max_h: i32,
    ) -> Result<(i32, i32, i32)> {
        let elems = build_elements(p, theme);
        let content_w = (max_w - 2 * theme.padding).max(0) as f32;
        let used_h = layout_pass(
            &self.dwrite_factory,
            &self.formats,
            None,
            &elems,
            theme,
            0.0,
            0.0,
            content_w,
            0.0,
        )
        .context("measuring popup content")?;
        let content_h = used_h.ceil() as i32 + 2 * theme.padding;
        Ok((max_w, content_h.min(max_h), content_h))
    }

    /// Paints `p`/`theme` into the window's current client rect (already
    /// sized by `Popup::show_at` to what `measure` returned). Draws, in
    /// order: the panel background, the top card (frequency in the
    /// top-right corner, headword, reading, POS, then each `GlossBlock`),
    /// a separator, each collapsed row, and - only when the content overflows
    /// the window - a scrollbar on the right edge.
    ///
    /// `scroll` shifts the content up by that many physical pixels. D2D clips
    /// to the render target, so a partially-visible line at the top edge falls
    /// out for free; nothing needs to cull off-screen elements.
    ///
    /// Device-lost (`D2DERR_RECREATE_TARGET`, e.g. a display mode change, a
    /// driver reset, or an RDP session change) is handled here: the stale
    /// target is dropped, a fresh one is created against the current size,
    /// and painting is retried once, immediately, before this call returns.
    /// The failed `BeginDraw`/`EndDraw` pair is the one dropped frame - D2D
    /// gives no guarantee its content reached the screen - but the retry
    /// means the caller still gets a correctly painted popup back, not an
    /// error and not a crash.
    pub fn paint(&mut self, p: &Presentation, theme: &Theme, scroll: i32) -> Result<()> {
        let (w, h) = self.client_size().context("querying the popup's client size")?;
        self.ensure_target(w, h).context("preparing the D2D render target")?;

        if let Err(e) = self.paint_once(p, theme, w, h, scroll) {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.ensure_target(w, h)
                    .context("recreating the D2D render target after device loss")?;
                self.paint_once(p, theme, w, h, scroll)
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
        scroll: i32,
    ) -> windows::core::Result<()> {
        let target = self
            .target
            .as_ref()
            .expect("ensure_target must run before paint_once");

        unsafe { target.BeginDraw() };
        let scope = DrawScope { target: Some(target) };

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
            let origin = theme.padding as f32;
            let used = layout_pass(
                &self.dwrite_factory,
                &self.formats,
                Some(target),
                &elems,
                theme,
                origin,
                origin,
                content_w,
                scroll as f32,
            )?;

            let track_h = h - 2 * theme.padding;
            let total = used.ceil() as i32 + 2 * theme.padding;
            if let Some((top, thumb_h)) = scrollbar_thumb(track_h, total, h, scroll) {
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

        // Prioritise the draw-phase error when both fail - it is usually
        // more specific (e.g. a font/format failure) than whatever EndDraw
        // reports for an already-broken span. A draw phase that *succeeded*
        // must still surface whatever EndDraw returns, since that is where
        // device-lost is authoritatively signalled.
        draw_result.and(end_result)
    }
}

/// Ends the draw on drop.
struct DrawScope<'a> {
    target: Option<&'a ID2D1HwndRenderTarget>,
}

impl DrawScope<'_> {
    /// Ends it, returning its result.
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

/// Turns `p`'s content into an ordered list of drawable elements, exactly
/// as `present.rs` ordered them: the frequency corner, headword, reading,
/// POS, then each `GlossBlock` (dictionary label + its glosses joined into
/// one paragraph), a separator, then each `CollapsedRow`. Frequency and POS
/// are both drawn in `theme.dimmed_text` at `collapsed_size` - they are
/// metadata about the entry, not part of reading it, and at body weight
/// they competed with the definition itself. Pure with respect to
/// D2D/DirectWrite - no factory or render target touched here, only plain
/// data - so `measure` and `paint` build identical input to `layout_pass`.
fn build_elements(p: &Presentation, theme: &Theme) -> Vec<Elem> {
    let mut out = Vec::new();

    if let Some(card) = &p.top {
        // Emitted before the headword so the walk draws it at the same y.
        if let Some(freq) = card.freq {
            out.push(Elem::Corner(Line {
                text: format!("freq {freq}"),
                color: theme.dimmed_text,
                size: theme.collapsed_size,
                top_gap: 0.0,
            }));
        }

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

        if !card.pos.is_empty() {
            out.push(Elem::Text(Line {
                text: card.pos.join(" · "),
                color: theme.dimmed_text,
                size: theme.collapsed_size,
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

/// The layout walk shared by `measure` (`target: None`) and `paint_once`
/// (`target: Some(_)`): both build `elems` from `build_elements` and call
/// this with the same content box, so a line either measures and paints
/// identically, or - past the point where it stops fitting - is clamped
/// identically in both. `origin_x`/`origin_y` are where `(0, 0)` in this
/// walk's own coordinate space lands in the render target (the padded
/// panel's top-left corner in paint mode; irrelevant, so `0.0, 0.0`, in
/// measure mode, since nothing is actually drawn).
///
/// The content height used.
fn layout_pass(
    dwrite: &IDWriteFactory,
    formats: &RefCell<FormatCache>,
    target: Option<&ID2D1HwndRenderTarget>,
    elems: &[Elem],
    theme: &Theme,
    origin_x: f32,
    origin_y: f32,
    content_w: f32,
    scroll: f32,
) -> windows::core::Result<f32> {
    let mut y = 0.0f32;
    let mut reserved_w = 0.0f32;

    for elem in elems {
        let drawn_height = match elem {
            Elem::Separator { top_gap } => {
                let h = SEPARATOR_THICKNESS;
                y += top_gap;
                if let Some(t) = target {
                    let brush = unsafe { t.CreateSolidColorBrush(&color_f(theme.separator), None) }?;
                    let rect = D2D_RECT_F {
                        left: origin_x,
                        top: origin_y + y - scroll,
                        right: origin_x + content_w,
                        bottom: origin_y + y + h - scroll,
                    };
                    unsafe { t.FillRectangle(&rect, &brush) };
                }
                h
            }
            Elem::Corner(line) => {
                let (layout, m) =
                    build_text_layout(dwrite, formats, &theme.font_name, line, content_w)?;
                unsafe { layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING) }?;
                if let Some(t) = target {
                    let brush = unsafe { t.CreateSolidColorBrush(&color_f(line.color), None) }?;
                    unsafe {
                        t.DrawTextLayout(
                            Vector2 { X: origin_x, Y: origin_y + y - scroll },
                            &layout,
                            &brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        )
                    };
                }
                reserved_w = m.width + CORNER_GAP;
                0.0
            }
            Elem::Text(line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let (layout, m) =
                    build_text_layout(dwrite, formats, &theme.font_name, line, avail_w)?;
                let h = m.height;
                y += line.top_gap;
                if let Some(t) = target {
                    let brush = unsafe { t.CreateSolidColorBrush(&color_f(line.color), None) }?;
                    unsafe {
                        t.DrawTextLayout(
                            Vector2 { X: origin_x, Y: origin_y + y - scroll },
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

    Ok(y)
}

/// Builds one `IDWriteTextLayout` for `line`, word-wrapped to `max_w`, and
/// returns it alongside its measured height. Used identically by both
/// measuring and drawing - `paint_once` draws exactly this layout object at
/// a computed origin, so the two can never disagree about how a given
/// string wraps.
fn build_text_layout(
    dwrite: &IDWriteFactory,
    formats: &RefCell<FormatCache>,
    font: &str,
    line: &Line,
    max_w: f32,
) -> windows::core::Result<(IDWriteTextLayout, DWRITE_TEXT_METRICS)> {
    let format = text_format(dwrite, formats, font, line.size)?;
    let wide: Vec<u16> = line.text.encode_utf16().collect();
    let layout = unsafe { dwrite.CreateTextLayout(&wide, &format, max_w.max(1.0), f32::MAX) }?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics) }?;
    Ok((layout, metrics))
}

/// Covers `build_elements` only - the pure half of this module. The layout
/// walk needs a real DirectWrite factory and a device, so it stays verified
/// by eye rather than here.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::{Card, GlossBlock};

    fn one_card(pos: &[&str], freq: Option<i64>) -> Presentation {
        Presentation {
            top: Some(Card {
                written: Some("雑談".into()),
                reading: Some("ざつだん".into()),
                pos: pos.iter().map(|s| s.to_string()).collect(),
                freq,
                blocks: vec![GlossBlock {
                    dict_name: "Jitendex".into(),
                    glosses: vec!["chatting".into()],
                }],
                match_len: 2,
            }),
            collapsed: vec![],
        }
    }

    /// It must come first: the walk draws a corner at whatever `y` it has
    /// reached, so anything before it would push the frequency down out of
    /// the top-right corner it is supposed to sit in.
    #[test]
    fn frequency_leads_as_a_corner_so_it_shares_the_headword_line() {
        let theme = Theme::dark();
        let elems = build_elements(&one_card(&[], Some(7671)), &theme);
        match &elems[0] {
            Elem::Corner(line) => {
                assert_eq!("freq 7671", line.text);
                assert_eq!(theme.dimmed_text, line.color);
            }
            _ => panic!("the frequency corner must be the first element"),
        }
    }

    #[test]
    fn an_unranked_entry_draws_no_corner() {
        let elems = build_elements(&one_card(&[], None), &Theme::dark());
        assert!(!elems.iter().any(|e| matches!(e, Elem::Corner(_))));
    }

    #[test]
    fn part_of_speech_is_dimmed_metadata_not_body_text() {
        let theme = Theme::dark();
        let elems = build_elements(&one_card(&["noun", "suru"], Some(1)), &theme);
        let pos = elems
            .iter()
            .find_map(|e| match e {
                Elem::Text(line) if line.text.contains("noun") => Some(line),
                _ => None,
            })
            .expect("a POS line must be drawn");
        assert_eq!("noun · suru", pos.text);
        assert_eq!(theme.dimmed_text, pos.color);
        assert_ne!(theme.body_text, pos.color, "POS must not read as body text");
        assert_eq!(theme.collapsed_size, pos.size);
    }

    /// 大辞林 carries no POS markup, so its cards have none - and an empty
    /// `pos` must draw nothing rather than a bare separator line.
    #[test]
    fn an_entry_without_part_of_speech_draws_no_pos_line() {
        let elems = build_elements(&one_card(&[], Some(1)), &Theme::dark());
        assert!(!elems
            .iter()
            .any(|e| matches!(e, Elem::Text(line) if line.text.contains('·'))));
    }

    #[test]
    fn content_that_fits_cannot_scroll() {
        assert_eq!(0, max_scroll(200, 300));
        assert_eq!(0, max_scroll(300, 300));
    }

    #[test]
    fn max_scroll_is_the_overflow() {
        assert_eq!(200, max_scroll(500, 300));
    }

    #[test]
    fn content_that_fits_has_no_thumb() {
        assert_eq!(None, scrollbar_thumb(300, 200, 300, 0));
        assert_eq!(None, scrollbar_thumb(300, 300, 300, 0));
    }

    #[test]
    fn the_thumb_is_proportional_and_starts_at_the_top() {
        let (top, h) = scrollbar_thumb(300, 600, 300, 0).unwrap();
        assert_eq!(0, top);
        assert_eq!(150, h, "half the content is visible, so half the track");
    }

    /// At the bottom the thumb must be flush with the track's end, or the popup
    /// looks unscrolled when it is fully scrolled.
    #[test]
    fn the_thumb_ends_flush_with_the_track_at_max_scroll() {
        let (top, h) = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
        assert_eq!(300, top + h);
    }

    /// A very long entry would otherwise produce a 1px sliver.
    #[test]
    fn the_thumb_has_a_floor() {
        let (_, h) = scrollbar_thumb(300, 100_000, 300, 0).unwrap();
        assert_eq!(SCROLLBAR_MIN_THUMB, h);
    }

    /// The floor must not push the thumb past the track's end.
    #[test]
    fn a_floored_thumb_still_ends_inside_the_track() {
        let m = max_scroll(100_000, 300);
        let (top, h) = scrollbar_thumb(300, 100_000, 300, m).unwrap();
        assert!(top + h <= 300, "thumb {top}+{h} escaped a 300px track");
        assert_eq!(300, top + h);
    }

    #[test]
    fn a_scroll_beyond_the_end_is_treated_as_the_end() {
        let a = scrollbar_thumb(300, 600, 300, 999_999).unwrap();
        let b = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
        assert_eq!(b, a);
    }

    /// A track shorter than the floor must still produce a thumb that fits,
    /// not one taller than the track it lives in.
    #[test]
    fn a_track_shorter_than_the_floor_still_fits() {
        let (top, h) = scrollbar_thumb(10, 600, 300, 0).unwrap();
        assert!(h <= 10, "thumb {h} in a 10px track");
        assert!(top + h <= 10);
    }
}

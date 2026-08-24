//! Direct2D/DirectWrite paint.
//!
//! Hwnd target, not layered.
//! One walk: measure and paint.
//! Caller paints; wndproc won't.

use crate::present::Presentation;
use crate::ui::theme::{Theme, SCROLLBAR_MIN_THUMB, SCROLLBAR_W};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows_numerics::Vector2;

/// Gap within a block.
const LINE_GAP: f32 = 4.0;
/// Gap before a new block.
const SECTION_GAP: f32 = 10.0;
/// Gap beside the corner elem.
const CORNER_GAP: f32 = 8.0;
/// Gap around the rule.
const SEPARATOR_MARGIN: f32 = 10.0;
const SEPARATOR_THICKNESS: f32 = 1.0;
/// Fixed "See also" column width.
const SIDE_PANEL_W: f32 = 110.0;
/// Gap before the side panel.
const SIDE_GAP: f32 = 12.0;

/// Clickable region in the popup.
#[derive(Debug, Clone)]
pub struct HitRect {
    pub x: Option<f32>,
    pub y: f32,
    pub w: Option<f32>,
    pub h: f32,
    pub action: HitAction,
}

/// What a click on a region does.
#[derive(Debug, Clone)]
pub enum HitAction {
    /// Expand collapsed row `i`.
    ExpandEntry(usize),
    /// Look up a single character.
    DrillDown(String),
    /// Navigate back in history.
    Back,
}

/// CJK ideograph check.
fn is_kanji(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
    )
}

/// Anki state for this popup.
#[derive(Clone)]
pub struct AnkiPopupState {
    pub dupes: HashSet<String>,
    pub added: HashSet<String>,
    pub enabled: bool,
    pub adding: bool,
    /// Dupe check in flight.
    pub checking: bool,
    /// AnkiConnect reachable.
    pub connected: bool,
    /// Last add-note failed.
    pub failed: bool,
}

impl AnkiPopupState {
    /// Disabled, no markers.
    pub fn disabled() -> Self {
        Self {
            dupes: HashSet::new(),
            added: HashSet::new(),
            enabled: false,
            adding: false,
            checking: false,
            connected: false,
            failed: false,
        }
    }
}

/// One line to lay out or draw.
///
/// Rebuilt, never cached.
struct Line {
    text: String,
    color: (u8, u8, u8),
    size: f32,
    /// Extra space above this line.
    top_gap: f32,
}

enum Elem {
    Text(Line),
    /// Right-aligned, advances no y.
    ///
    /// Steals width from the next.
    Corner(Line),
    Separator { top_gap: f32 },
    /// A clickable collapsed row.
    Collapsed(usize, Line),
    /// Per-char click targets.
    Headword {
        headword: String,
        prefix_u16: usize,
        line: Line,
    },
    /// Navigate back in history.
    BackButton(Line),
}

/// One "See also" headword.
struct SideEntry {
    idx: usize,
    text: String,
    color: (u8, u8, u8),
}

/// Overflow past the view, or 0.
pub fn max_scroll(content_h: i32, view_h: i32) -> i32 {
    (content_h - view_h).max(0)
}

/// The thumb as `(top, height)`.
///
/// Floored, and kept in track.
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

/// D2D state for one window.
pub struct Renderer {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    /// Lazy; the window starts 0x0.
    target: Option<ID2D1HwndRenderTarget>,
    /// Text formats, reused.
    formats: RefCell<FormatCache>,
    /// From the last paint pass.
    hit_rects: RefCell<Vec<HitRect>>,
}

/// One font's formats, by size.
#[derive(Default)]
struct FormatCache {
    font: String,
    by_size: HashMap<u32, IDWriteTextFormat>,
}

/// One format per font and size.
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
    /// Factories only, no target.
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
            hit_rects: RefCell::new(Vec::new()),
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
        self.hit_rects
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
        let dip_w = max_w as f32 / scale;
        let dip_h = max_h as f32 / scale;
        let (elems, side) = build_elements(p, theme, show_back, side_panel);
        let has_side = !side.is_empty();
        let side_extra = if has_side {
            SIDE_GAP + SEPARATOR_THICKNESS + SIDE_GAP + SIDE_PANEL_W
        } else {
            0.0
        };
        let content_w = (dip_w - 2.0 * theme.padding as f32 - side_extra).max(0.0);
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
            None,
        )
        .context("measuring popup content")?;
        let side_h = if has_side {
            side_measure(
                &self.dwrite_factory,
                &self.formats,
                &theme.font_name,
                theme.collapsed_size,
                &side,
                SIDE_PANEL_W,
            )?
        } else {
            0.0
        };
        let body_h = used_h.max(side_h);
        let content_h = body_h.ceil() + 2.0 * theme.padding as f32;
        let view_h = content_h.min(dip_h);
        let total_w = if has_side {
            ((content_w + side_extra + 2.0 * theme.padding as f32) * scale).ceil() as i32
        } else {
            max_w
        };
        Ok((
            total_w,
            (view_h * scale).ceil() as i32,
            (content_h * scale).ceil() as i32,
        ))
    }

    /// Paints into the client rect.
    ///
    /// Device loss retries once.
    pub fn paint(
        &mut self,
        p: &Presentation,
        theme: &Theme,
        scroll: i32,
        show_back: bool,
        side_panel: bool,
    ) -> Result<()> {
        let (w, h) = self.client_size().context("querying the popup's client size")?;
        self.ensure_target(w, h).context("preparing the D2D render target")?;

        if let Err(e) = self.paint_once(p, theme, w, h, scroll, show_back, side_panel) {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.ensure_target(w, h)
                    .context("recreating the D2D render target after device loss")?;
                self.paint_once(p, theme, w, h, scroll, show_back, side_panel)
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
    #[allow(clippy::too_many_arguments)]
    fn paint_once(
        &self,
        p: &Presentation,
        theme: &Theme,
        w: i32,
        h: i32,
        scroll: i32,
        show_back: bool,
        side_panel: bool,
    ) -> windows::core::Result<()> {
        let target = self
            .target
            .as_ref()
            .expect("ensure_target must run before paint_once");

        let scale = self.dpi_scale();
        let w = (w as f32 / scale) as i32;
        let h = (h as f32 / scale) as i32;
        let scroll = (scroll as f32 / scale) as i32;

        unsafe { target.BeginDraw() };
        let scope = DrawScope { target: Some(target) };

        let draw_result: windows::core::Result<()> = (|| {
            let (elems, side) = build_elements(p, theme, show_back, side_panel);
            let has_side = !side.is_empty();
            let side_extra = if has_side {
                SIDE_GAP + SEPARATOR_THICKNESS + SIDE_GAP + SIDE_PANEL_W
            } else {
                0.0
            };

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

            let main_w = (w as f32 - 2.0 * theme.padding as f32 - side_extra).max(0.0);
            let origin = theme.padding as f32;

            self.hit_rects.borrow_mut().clear();

            let used = layout_pass(
                &self.dwrite_factory,
                &self.formats,
                Some(target),
                &elems,
                theme,
                origin,
                origin,
                main_w,
                scroll as f32,
                Some(&self.hit_rects),
            )?;

            if has_side {
                let sep_x = origin + main_w + SIDE_GAP;
                let sep_brush =
                    unsafe { target.CreateSolidColorBrush(&color_f(theme.separator), None) }?;
                let sep = D2D_RECT_F {
                    left: sep_x,
                    top: origin,
                    right: sep_x + SEPARATOR_THICKNESS,
                    bottom: (h - theme.padding) as f32,
                };
                unsafe { target.FillRectangle(&sep, &sep_brush) };

                let col_x = sep_x + SEPARATOR_THICKNESS + SIDE_GAP;
                side_paint(
                    &self.dwrite_factory,
                    &self.formats,
                    target,
                    &theme.font_name,
                    theme,
                    &side,
                    col_x,
                    origin,
                    SIDE_PANEL_W,
                    scroll as f32,
                    &self.hit_rects,
                )?;
            }

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

        // Draw error first: it's finer.
        // EndDraw signals device loss.
        draw_result.and(end_result)
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

/// `p`'s content, in draw order.
///
/// Pure: no factory, no target.
fn build_elements(
    p: &Presentation,
    theme: &Theme,
    show_back: bool,
    side_panel: bool,
) -> (Vec<Elem>, Vec<SideEntry>) {
    let mut out = Vec::new();

    if show_back {
        out.push(Elem::BackButton(Line {
            text: "\u{2190} Back".to_string(),
            color: theme.dict_label_text,
            size: theme.collapsed_size,
            top_gap: 0.0,
        }));
    }

    if let Some(card) = &p.top {
        // Before the headword: same y.
        if let Some(freq) = card.freq {
            out.push(Elem::Corner(Line {
                text: format!("freq {freq}"),
                color: theme.dimmed_text,
                size: theme.dimmed_size,
                top_gap: 0.0,
            }));
        }

        let headword = card.written.clone()
            .or_else(|| card.reading.clone())
            .unwrap_or_default();
        if !headword.is_empty() {
            out.push(Elem::Headword {
                headword: headword.clone(),
                prefix_u16: 0,
                line: Line {
                    text: headword.clone(),
                    color: theme.headword_text,
                    size: theme.headword_size,
                    top_gap: if show_back { LINE_GAP } else { 0.0 },
                },
            });
        }

        // Only if the headword differs.
        if card.written.is_some() {
            if let Some(reading) = card.reading.as_deref().filter(|r| !r.is_empty()) {
                out.push(Elem::Text(Line {
                    text: reading.to_string(),
                    color: theme.reading_text,
                    size: theme.reading_size,
                    top_gap: LINE_GAP,
                }));
            }
        }

        if !card.pos.is_empty() {
            out.push(Elem::Text(Line {
                text: card.pos.join(" · "),
                color: theme.dimmed_text,
                size: theme.dimmed_size,
                top_gap: LINE_GAP,
            }));
        }

        for block in &card.blocks {
            out.push(Elem::Text(Line {
                text: block.dict_name.clone(),
                color: theme.dict_label_text,
                size: theme.dict_label_size,
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

    let mut side = Vec::new();

    if !p.collapsed.is_empty() {
        if side_panel {
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = row.written.clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                if head.is_empty() { continue; }
                side.push(SideEntry {
                    idx: i,
                    text: head,
                    color: theme.collapsed_text,
                });
            }
        } else {
            out.push(Elem::Separator { top_gap: SEPARATOR_MARGIN });
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = row.written.clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                let text = if head.is_empty() {
                    row.summary.clone()
                } else {
                    format!("{head} \u{2014} {}", row.summary)
                };
                out.push(Elem::Collapsed(i, Line {
                    text,
                    color: theme.collapsed_text,
                    size: theme.collapsed_size,
                    top_gap: if i == 0 { SEPARATOR_MARGIN } else { LINE_GAP },
                }));
            }
        }
    }

    (out, side)
}

/// The Anki button's label.
///
/// `None` means: show no button.
pub(crate) fn anki_button_label(
    p: &Presentation,
    theme: &Theme,
    anki: &AnkiPopupState,
) -> Option<(String, (u8, u8, u8))> {
    if !anki.enabled { return None; }
    if !anki.connected { return None; }
    let expr = p.top.as_ref()
        .and_then(|c| c.written.as_deref().or(c.reading.as_deref()))
        .unwrap_or("");
    let (text, color) = if anki.checking {
        ("Checking\u{2026}", theme.dimmed_text)
    } else if anki.adding {
        ("Adding\u{2026}", theme.dimmed_text)
    } else if anki.failed {
        ("\u{2717} Failed to add", theme.dimmed_text)
    } else if anki.added.contains(expr) {
        ("\u{2713} Added", theme.dimmed_text)
    } else if anki.dupes.contains(expr) {
        ("\u{ff0b} Add to Anki (duplicate)", theme.dict_label_text)
    } else {
        ("\u{ff0b} Add to Anki", theme.dict_label_text)
    };
    Some((text.to_string(), color))
}

/// The shared layout walk.
///
/// Returns the content height.
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
    hit_out: Option<&RefCell<Vec<HitRect>>>,
) -> windows::core::Result<f32> {
    let mut y = 0.0f32;
    let mut reserved_w = 0.0f32;

    for elem in elems {
        let drawn_height = match elem {
            Elem::Separator { top_gap } => {
                let h = theme.separator_height;
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
            Elem::Collapsed(idx, line) => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let (layout, m) =
                    build_text_layout(dwrite, formats, &theme.font_name, line, avail_w)?;
                let h = m.height;
                y += line.top_gap;
                if let Some(hits) = hit_out {
                    hits.borrow_mut().push(HitRect {
                        x: None,
                        y: origin_y + y,
                        w: None,
                        h,
                        action: HitAction::ExpandEntry(*idx),
                    });
                }
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
            Elem::Headword { ref headword, prefix_u16, ref line } => {
                let avail_w = (content_w - reserved_w).max(1.0);
                reserved_w = 0.0;
                let (layout, m) =
                    build_text_layout(dwrite, formats, &theme.font_name, line, avail_w)?;
                let h = m.height;
                y += line.top_gap;
                if let Some(hits) = hit_out {
                    let mut u16_pos = *prefix_u16 as u32;
                    for ch in headword.chars() {
                        if is_kanji(ch) {
                            let mut px = 0.0f32;
                            let mut py = 0.0f32;
                            let mut hm = DWRITE_HIT_TEST_METRICS::default();
                            // SAFETY: u16_pos is a
                            // valid index into the
                            // layout's UTF-16 text.
                            unsafe {
                                layout.HitTestTextPosition(
                                    u16_pos,
                                    false,
                                    &mut px,
                                    &mut py,
                                    &mut hm,
                                )?;
                            }
                            hits.borrow_mut().push(HitRect {
                                x: Some(origin_x + hm.left),
                                y: origin_y + y + hm.top,
                                w: Some(hm.width),
                                h: hm.height,
                                action: HitAction::DrillDown(
                                    ch.to_string(),
                                ),
                            });
                        }
                        u16_pos += ch.len_utf16() as u32;
                    }
                }
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
            Elem::BackButton(line) => {
                let (layout, m) =
                    build_text_layout(dwrite, formats, &theme.font_name, line, content_w)?;
                let h = m.height;
                y += line.top_gap;
                if let Some(hits) = hit_out {
                    hits.borrow_mut().push(HitRect {
                        x: None,
                        y: origin_y + y,
                        w: None,
                        h,
                        action: HitAction::Back,
                    });
                }
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

/// One wrapped layout + metrics.
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

/// Height of the side column.
fn side_measure(
    dwrite: &IDWriteFactory,
    formats: &RefCell<FormatCache>,
    font: &str,
    size: f32,
    entries: &[SideEntry],
    col_w: f32,
) -> windows::core::Result<f32> {
    let format = text_format(dwrite, formats, font, size)?;
    let heading: Vec<u16> = "See also".encode_utf16().collect();
    let layout = unsafe {
        dwrite.CreateTextLayout(&heading, &format, col_w.max(1.0), f32::MAX)
    }?;
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut m) }?;
    let mut y = m.height + LINE_GAP;

    for entry in entries {
        let wide: Vec<u16> = entry.text.encode_utf16().collect();
        let layout = unsafe {
            dwrite.CreateTextLayout(&wide, &format, col_w.max(1.0), f32::MAX)
        }?;
        let mut em = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut em) }?;
        y += LINE_GAP + em.height;
    }
    Ok(y)
}

/// Draws the "See also" column.
#[allow(clippy::too_many_arguments)]
fn side_paint(
    dwrite: &IDWriteFactory,
    formats: &RefCell<FormatCache>,
    target: &ID2D1HwndRenderTarget,
    font: &str,
    theme: &Theme,
    entries: &[SideEntry],
    x: f32,
    origin_y: f32,
    col_w: f32,
    scroll: f32,
    hit_out: &RefCell<Vec<HitRect>>,
) -> windows::core::Result<()> {
    let format = text_format(dwrite, formats, font, theme.collapsed_size)?;
    let dim_brush = unsafe {
        target.CreateSolidColorBrush(&color_f(theme.dimmed_text), None)
    }?;

    let heading: Vec<u16> = "See also".encode_utf16().collect();
    let layout = unsafe {
        dwrite.CreateTextLayout(&heading, &format, col_w.max(1.0), f32::MAX)
    }?;
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut m) }?;
    let mut y = 0.0f32;
    unsafe {
        target.DrawTextLayout(
            Vector2 { X: x, Y: origin_y + y - scroll },
            &layout, &dim_brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }
    y += m.height + LINE_GAP;

    for entry in entries {
        let brush = unsafe {
            target.CreateSolidColorBrush(&color_f(entry.color), None)
        }?;
        let wide: Vec<u16> = entry.text.encode_utf16().collect();
        let layout = unsafe {
            dwrite.CreateTextLayout(&wide, &format, col_w.max(1.0), f32::MAX)
        }?;
        let mut em = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut em) }?;
        hit_out.borrow_mut().push(HitRect {
            x: Some(x),
            y: origin_y + y,
            w: Some(col_w),
            h: em.height,
            action: HitAction::ExpandEntry(entry.idx),
        });
        unsafe {
            target.DrawTextLayout(
                Vector2 { X: x, Y: origin_y + y - scroll },
                &layout, &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        y += LINE_GAP + em.height;
    }
    Ok(())
}

/// Covers `build_elements` only.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::{Card, GlossBlock};

    fn one_card(pos: &[&str], freq: Option<i64>) -> Presentation {
        let card = Card {
            written: Some("雑談".into()),
            reading: Some("ざつだん".into()),
            pos: pos.iter().map(|s| s.to_string()).collect(),
            freq,
            blocks: vec![GlossBlock {
                dict_name: "Jitendex".into(),
                glosses: vec!["chatting".into()],
                glosses_html: vec![],
            }],
            match_len: 2,
        };
        Presentation {
            top: Some(card.clone()),
            collapsed: vec![],
            all_cards: vec![card],
            sentence: None,
        }
    }

    /// It must lead the list.
    #[test]
    fn frequency_leads_as_a_corner_so_it_shares_the_headword_line() {
        let theme = Theme::dark();
        let (elems, _) = build_elements(&one_card(&[], Some(7671)), &theme, false, false);
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
        let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
        assert!(!elems.iter().any(|e| matches!(e, Elem::Corner(_))));
    }

    #[test]
    fn part_of_speech_is_dimmed_metadata_not_body_text() {
        let theme = Theme::dark();
        let (elems, _) = build_elements(&one_card(&["noun", "suru"], Some(1)), &theme, false, false);
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

    /// 大辞林 has no POS markup.
    #[test]
    fn an_entry_without_part_of_speech_draws_no_pos_line() {
        let (elems, _) = build_elements(&one_card(&[], Some(1)), &Theme::dark(), false, false);
        assert!(!elems
            .iter()
            .any(|e| matches!(e, Elem::Text(line) if line.text.contains('·'))));
    }

    // -- headword / drill-down --

    #[test]
    fn the_headword_is_a_headword_element_not_text() {
        let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
        assert!(
            elems.iter().any(|e| matches!(e, Elem::Headword { .. })),
            "expected a Headword element for the headword"
        );
    }

    #[test]
    fn headword_prefix_u16_is_zero_without_anki_marks() {
        let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
        let hw = elems.iter().find_map(|e| match e {
            Elem::Headword { prefix_u16, .. } => Some(*prefix_u16),
            _ => None,
        });
        assert_eq!(Some(0), hw);
    }

    /// Dupes no longer mark headwords.
    #[test]
    fn headword_has_no_prefix_even_for_dupes() {
        let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
        let hw = elems.iter().find_map(|e| match e {
            Elem::Headword { prefix_u16, .. } => Some(*prefix_u16),
            _ => None,
        });
        assert_eq!(Some(0), hw);
    }

    #[test]
    fn show_back_adds_a_back_button_element() {
        let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), true, false);
        assert!(matches!(&elems[0], Elem::BackButton(_)));
    }

    #[test]
    fn no_back_button_without_show_back() {
        let (elems, _) = build_elements(&one_card(&[], None), &Theme::dark(), false, false);
        assert!(!elems.iter().any(|e| matches!(e, Elem::BackButton(_))));
    }

    #[test]
    fn is_kanji_covers_cjk_unified() {
        assert!(is_kanji('\u{98DF}'));
        assert!(is_kanji('\u{4E00}'));
        assert!(is_kanji('\u{9FFF}'));
        assert!(!is_kanji('\u{3042}'));
        assert!(!is_kanji('a'));
    }

    // -- anki_button_label --

    #[test]
    fn anki_button_label_is_none_when_disabled() {
        let p = one_card(&[], None);
        assert!(anki_button_label(&p, &Theme::dark(), &AnkiPopupState::disabled()).is_none());
    }

    #[test]
    fn anki_button_label_shows_add_by_default() {
        let theme = Theme::dark();
        let anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
        let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("\u{ff0b} Add to Anki", text);
        assert_eq!(theme.dict_label_text, color);
    }

    #[test]
    fn anki_button_label_shows_adding_while_in_flight() {
        let theme = Theme::dark();
        let anki = AnkiPopupState { enabled: true, connected: true, adding: true, ..AnkiPopupState::disabled() };
        let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("Adding\u{2026}", text);
        assert_eq!(theme.dimmed_text, color);
    }

    #[test]
    fn anki_button_label_flags_a_known_dupe() {
        let theme = Theme::dark();
        let mut anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
        anki.dupes.insert("\u{96D1}\u{8AC7}".to_string());
        let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("\u{ff0b} Add to Anki (duplicate)", text);
        assert_eq!(theme.dict_label_text, color);
    }

    #[test]
    fn anki_button_label_shows_added_after_success() {
        let theme = Theme::dark();
        let mut anki = AnkiPopupState { enabled: true, connected: true, ..AnkiPopupState::disabled() };
        anki.added.insert("\u{96D1}\u{8AC7}".to_string());
        let (text, _) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("\u{2713} Added", text);
    }

    /// Adding outranks both markers.
    #[test]
    fn anki_button_label_prefers_adding_over_dupe_or_added() {
        let theme = Theme::dark();
        let mut anki = AnkiPopupState {
            enabled: true,
            connected: true,
            adding: true,
            ..AnkiPopupState::disabled()
        };
        anki.dupes.insert("\u{96D1}\u{8AC7}".to_string());
        anki.added.insert("\u{96D1}\u{8AC7}".to_string());
        let (text, _) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("Adding\u{2026}", text);
    }

    /// Checking outranks the add label.
    #[test]
    fn anki_button_label_shows_checking() {
        let theme = Theme::dark();
        let anki = AnkiPopupState {
            enabled: true, connected: true, checking: true,
            ..AnkiPopupState::disabled()
        };
        let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("Checking\u{2026}", text);
        assert_eq!(theme.dimmed_text, color);
    }

    #[test]
    fn anki_button_label_shows_failed() {
        let theme = Theme::dark();
        let anki = AnkiPopupState {
            enabled: true, connected: true, failed: true,
            ..AnkiPopupState::disabled()
        };
        let (text, color) = anki_button_label(&one_card(&[], None), &theme, &anki).unwrap();
        assert_eq!("\u{2717} Failed to add", text);
        assert_eq!(theme.dimmed_text, color);
    }

    /// Disconnected hides the button.
    #[test]
    fn anki_button_label_is_none_when_disconnected() {
        let anki = AnkiPopupState {
            enabled: true, connected: false,
            ..AnkiPopupState::disabled()
        };
        assert!(anki_button_label(&one_card(&[], None), &Theme::dark(), &anki).is_none());
    }

    /// No marks on collapsed rows.
    #[test]
    fn collapsed_rows_carry_no_dupe_marks() {
        let (elems, _) = build_elements(
            &with_collapsed(), &Theme::dark(), false, false,
        );
        for e in &elems {
            if let Elem::Collapsed(_, line) = e {
                assert!(!line.text.starts_with('\u{2713}'), "no check marks on collapsed rows");
            }
        }
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

    /// Else it looks unscrolled.
    #[test]
    fn the_thumb_ends_flush_with_the_track_at_max_scroll() {
        let (top, h) = scrollbar_thumb(300, 600, 300, max_scroll(600, 300)).unwrap();
        assert_eq!(300, top + h);
    }

    /// Else a 1px sliver.
    #[test]
    fn the_thumb_has_a_floor() {
        let (_, h) = scrollbar_thumb(300, 100_000, 300, 0).unwrap();
        assert_eq!(SCROLLBAR_MIN_THUMB, h);
    }

    /// The floor must not overhang.
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

    /// A short track still fits.
    #[test]
    fn a_track_shorter_than_the_floor_still_fits() {
        let (top, h) = scrollbar_thumb(10, 600, 300, 0).unwrap();
        assert!(h <= 10, "thumb {h} in a 10px track");
        assert!(top + h <= 10);
    }

    fn with_collapsed() -> Presentation {
        use crate::present::CollapsedRow;
        let card = Card {
            written: Some("\u{96D1}\u{8AC7}".into()),
            reading: Some("\u{3056}\u{3064}\u{3060}\u{3093}".into()),
            pos: vec![],
            freq: None,
            blocks: vec![GlossBlock {
                dict_name: "Jitendex".into(),
                glosses: vec!["chatting".into()],
                glosses_html: vec![],
            }],
            match_len: 2,
        };
        Presentation {
            top: Some(card.clone()),
            collapsed: vec![
                CollapsedRow {
                    written: Some("\u{96D1}\u{97F3}".into()),
                    reading: Some("\u{3056}\u{3064}\u{304A}\u{3093}".into()),
                    summary: "noise".into(),
                },
                CollapsedRow {
                    written: Some("\u{96D1}\u{8A8C}".into()),
                    reading: Some("\u{3056}\u{3063}\u{3057}".into()),
                    summary: "magazine".into(),
                },
            ],
            all_cards: vec![card],
            sentence: None,
        }
    }

    #[test]
    fn side_panel_false_keeps_collapsed_rows_inline() {
        let (elems, side) = build_elements(
            &with_collapsed(), &Theme::dark(), false, false,
        );
        assert!(side.is_empty());
        assert!(elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
    }

    #[test]
    fn side_panel_true_moves_collapsed_rows_to_side() {
        let (elems, side) = build_elements(
            &with_collapsed(), &Theme::dark(), false, true,
        );
        assert!(!elems.iter().any(|e| matches!(e, Elem::Collapsed(..))));
        assert_eq!(2, side.len());
        assert!(side[0].text.contains('\u{96D1}'));
    }

    #[test]
    fn side_entries_carry_expand_indices() {
        let (_, side) = build_elements(
            &with_collapsed(), &Theme::dark(), false, true,
        );
        assert_eq!(0, side[0].idx);
        assert_eq!(1, side[1].idx);
    }

    #[test]
    fn side_entries_show_headword_only() {
        let (_, side) = build_elements(
            &with_collapsed(), &Theme::dark(), false, true,
        );
        assert!(!side[0].text.contains("noise"));
        assert!(!side[1].text.contains("magazine"));
    }
}

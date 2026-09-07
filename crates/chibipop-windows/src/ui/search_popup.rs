//! Search owns this shell so native input works without daemon hooks.

use super::render::{Renderer, SceneInputs};
use super::theme::Theme;
use super::window::Popup;
use anyhow::{Context, Result};
use chibipop::config::Config;
use chibipop::controller::HitAction;
use chibipop::geom::{PhysPoint, PhysRect};
use chibipop::present::Presentation;
use std::cell::Cell;
use std::path::Path;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Default)]
struct Events {
    click: Cell<Option<PhysPoint>>,
    pointer: Cell<Option<PhysPoint>>,
    wheel: Cell<i32>,
    repaint: Cell<bool>,
    close: Cell<bool>,
}

pub(super) struct SearchPopup {
    renderer: Renderer,
    window: Popup,
    events: Box<Events>,
    presentation: Presentation,
    theme: Theme,
    config: Config,
    scroll: i32,
    max_scroll: i32,
    has_parent: bool,
}

pub(super) enum Action { Lookup(String), Back }

pub(super) fn theme(config: &Config) -> Theme {
    let mut theme = if config.popup.theme == "light" { Theme::light() } else { Theme::dark() };
    theme.font_name.clone_from(&config.popup.font);
    if let Ok(css) = std::fs::read_to_string(crate::paths::beside_exe("popup.css")) {
        for error in super::css::parse(&css, &mut theme) {
            eprintln!("chibipop: popup.css:{}: {}", error.line, error.message);
        }
    }
    theme
}

impl SearchPopup {
    pub(super) fn open(database: &Path, config: &Config, owner: HWND,
        presentation: Presentation, anchor: PhysPoint, has_parent: bool) -> Result<Self> {
        let events = Box::<Events>::default();
        let window = Popup::create(config.popup.exclude_from_capture)?;
        // SAFETY: This thread owns the HWND and stable event allocation until window destruction.
        unsafe {
            let style = GetWindowLongPtrW(window.hwnd(), GWL_EXSTYLE);
            SetWindowLongPtrW(window.hwnd(), GWL_EXSTYLE, style & !(WS_EX_TRANSPARENT.0 as isize));
            SetWindowLongPtrW(window.hwnd(), GWLP_HWNDPARENT, owner.0 as isize);
            SetWindowPos(window.hwnd(), None, anchor.x, anchor.y, 1, 1,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED)?;
            if !SetWindowSubclass(window.hwnd(), Some(popup_proc), 1,
                events.as_ref() as *const Events as usize).as_bool() {
                anyhow::bail!("installing search popup input handler");
            }
        }
        let renderer = Renderer::new(window.hwnd(), database)?;
        let theme = theme(config);
        // SAFETY: The window belongs to this UI thread; opacity uses the theme's bounded alpha.
        unsafe { SetLayeredWindowAttributes(window.hwnd(), COLORREF(0),
            (theme.opacity.clamp(0.0, 1.0) * 255.0).round() as u8, LWA_ALPHA)?; }
        let mut popup = Self { renderer, window, events, presentation, theme,
            config: config.clone(), scroll: 0, max_scroll: 0, has_parent };
        popup.place(anchor)?;
        Ok(popup)
    }

    fn place(&mut self, anchor: PhysPoint) -> Result<()> {
        let mut monitor = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default() };
        // SAFETY: The monitor query writes only to this initialized structure.
        unsafe { GetMonitorInfoW(MonitorFromPoint(POINT { x: anchor.x, y: anchor.y },
            MONITOR_DEFAULTTONEAREST), &mut monitor).ok()?; }
        let work = monitor.rcWork;
        let width = ((work.right - work.left) * i32::from(self.config.popup.max_width_percent) / 100).max(180);
        let height = ((work.bottom - work.top) * i32::from(self.config.popup.max_height_percent) / 100).max(100);
        let (w, h, content) = self.renderer.measure(SceneInputs {
            presentation: &self.presentation, theme: &self.theme, show_back: self.has_parent,
            side_panel: self.config.popup.side_panel, render: self.config.popup.render_settings(), selection: None,
        }, (width, height))?;
        self.max_scroll = (content - h).max(0);
        self.window.show_at(PhysRect { x: anchor.x.min(work.right - w).max(work.left),
            y: anchor.y.min(work.bottom - h).max(work.top), w, h })?;
        self.paint()
    }

    fn paint(&mut self) -> Result<()> {
        self.renderer.paint(SceneInputs { presentation: &self.presentation, theme: &self.theme,
            show_back: self.has_parent, side_panel: self.config.popup.side_panel,
            render: self.config.popup.render_settings(), selection: None }, self.scroll)
    }

    pub(super) fn poll(&mut self) -> Result<Option<Action>> {
        if self.events.close.replace(false) { return Ok(Some(Action::Back)); }
        let wheel = self.events.wheel.replace(0);
        if wheel != 0 && self.config.popup.scroll_popup {
            self.scroll = self.scroll.saturating_sub(wheel / 120 * 48).clamp(0, self.max_scroll);
            self.events.repaint.set(true);
        }
        let action = self.events.click.take().and_then(|point|
            self.renderer.hit_test(point.x, point.y, self.scroll));
        match action {
            Some(HitAction::Back) => return Ok(Some(Action::Back)),
            Some(HitAction::DrillDown(query)) => return Ok(Some(Action::Lookup(query))),
            Some(HitAction::ExpandEntry(index)) => {
                chibipop::present::swap_top(&mut self.presentation, index, self.config.popup.summary_chars);
                self.scroll = 0;
                let anchor = self.anchor()?;
                self.place(anchor)?;
            }
            Some(HitAction::OpenUrl(url)) if url.starts_with("https://") || url.starts_with("http://") => {
                    let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
                    // SAFETY: ShellExecuteW reads this terminated buffer during the call.
                    unsafe { windows::Win32::UI::Shell::ShellExecuteW(None, windows::core::w!("open"),
                        windows::core::PCWSTR(url.as_ptr()), None, None, SW_SHOWNORMAL); }
            }
            _ => {}
        }
        if self.events.repaint.replace(false) { self.paint()?; }
        Ok(None)
    }

    pub(super) fn hover(&mut self) -> Option<String> {
        if !self.config.popup.sub_popups { return None; }
        let point = self.events.pointer.get()?;
        self.renderer.hover_query(point, self.scroll)
    }

    pub(super) fn contains_pointer(&self) -> bool {
        // SAFETY: Queries do not retain either local buffer.
        unsafe {
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_err() { return false; }
            WindowFromPoint(point) == self.window.hwnd()
        }
    }

    pub(super) fn anchor(&self) -> Result<PhysPoint> {
        let mut rect = RECT::default();
        // SAFETY: This live window belongs to this thread; rect is valid output storage.
        unsafe { GetWindowRect(self.window.hwnd(), &mut rect).context("locating search definition")?; }
        Ok(PhysPoint { x: rect.left, y: rect.top })
    }

    pub(super) fn child_anchor(&self) -> Result<PhysPoint> {
        let point = self.events.pointer.get().unwrap_or(PhysPoint { x: 24, y: 24 });
        let anchor = self.anchor()?;
        Ok(PhysPoint { x: anchor.x + point.x + 24, y: anchor.y + point.y + 24 })
    }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM,
    _id: usize, data: usize) -> LRESULT {
    // SAFETY: Popup destruction precedes Events destruction. This callback only writes Cells.
    unsafe {
        let events = &*(data as *const Events);
        let point = PhysPoint { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 };
        match msg {
            WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONUP => { events.click.set(Some(point)); return LRESULT(0); }
            WM_MOUSEMOVE => { events.pointer.set(Some(point)); return LRESULT(0); }
            WM_MOUSEWHEEL => {
                events.wheel.set(events.wheel.get().saturating_add((wp.0 >> 16) as i16 as i32));
                return LRESULT(0);
            }
            WM_PAINT => events.repaint.set(true),
            WM_CLOSE => { events.close.set(true); return LRESULT(0); }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

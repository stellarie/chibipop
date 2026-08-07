//! The popup window shell.
//!
//! Const alpha, not per-pixel.
//! Per-pixel breaks WDA exclude.

use crate::geom::PhysRect;
use crate::ui::theme::Theme;
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Corner radius, in pixels.
const CORNER_RADIUS: i32 = 12;

/// Constant alpha, 0-255.
const LAYERED_ALPHA: u8 = 230;

fn class_name() -> PCWSTR {
    w!("ChibipopPopupClass")
}

/// The WM_PAINT work itself.
unsafe fn validate_paint_region(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let _ = BeginPaint(hwnd, &mut ps);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Validates; draws nothing.
///
/// Unwinding here would be UB.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let _ = catch_unwind(|| unsafe { validate_paint_region(hwnd) });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Registers the class once.
unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name(),
            hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW(IDC_ARROW)")?,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread()).context("RegisterClassExW");
        }
    }

    // Latch only after it succeeds.
    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Outcome of the affinity call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureExclusion {
    /// The OS accepted exclusion.
    Excluded,
    /// Never attempted, by request.
    DeliberatelyNotExcluded,
    /// Attempted; the OS refused.
    AttemptFailed,
}

impl CaptureExclusion {
    /// True unless truly excluded.
    pub fn needs_capture_guard(self) -> bool {
        !matches!(self, CaptureExclusion::Excluded)
    }
}

/// The popup window.
pub struct Popup {
    hwnd: HWND,
    capture_exclusion: CaptureExclusion,
}

impl Popup {
    /// Creates the window, hidden.
    pub fn create(exclude: bool) -> Result<Popup> {
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();

            register_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TRANSPARENT,
                class_name(),
                w!("chibipop"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW for the popup")?;

            SetLayeredWindowAttributes(hwnd, COLORREF(0), LAYERED_ALPHA, LWA_ALPHA)
                .context("SetLayeredWindowAttributes(LWA_ALPHA)")?;

            let capture_exclusion = if exclude {
                // Not `?`: the outcome is data.
                if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok() {
                    CaptureExclusion::Excluded
                } else {
                    CaptureExclusion::AttemptFailed
                }
            } else {
                CaptureExclusion::DeliberatelyNotExcluded
            };

            Ok(Popup { hwnd, capture_exclusion })
        }
    }

    /// Moves, shapes, and shows.
    pub fn show_at(&self, r: PhysRect) -> Result<()> {
        unsafe {
            let region = CreateRoundRectRgn(0, 0, r.w, r.h, CORNER_RADIUS, CORNER_RADIUS);
            if region.is_invalid() {
                anyhow::bail!("CreateRoundRectRgn({}, {}) returned a null region", r.w, r.h);
            }
            // Ours only if it failed.
            if SetWindowRgn(self.hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(region.into());
                anyhow::bail!("SetWindowRgn failed to apply the rounded silhouette");
            }

            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                r.x,
                r.y,
                r.w,
                r.h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .context("SetWindowPos to show the popup")?;

            // WM_PAINT is posted; force it.
            let _ = UpdateWindow(self.hwnd);
        }
        Ok(())
    }

    /// Hides without destroying.
    pub fn hide(&self) -> Result<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        Ok(())
    }

    /// Re-shows after a guard hide.
    pub fn show_without_activating(&self) -> Result<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    /// Asked fresh, never cached.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether it is excluded.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion
    }
}

/// Fixed height, 96-DPI px.
const BTN_HEIGHT: i32 = 36;

/// One button click, banked.
static BTN_CLICKED: AtomicBool = AtomicBool::new(false);

fn btn_class_name() -> PCWSTR {
    w!("ChibipopBtnClass")
}

/// What WM_PAINT redraws from.
struct BtnPaint {
    hwnd: HWND,
    text: String,
    text_color: (u8, u8, u8),
    bg: (u8, u8, u8),
    font_name: String,
    font_size: f32,
}

thread_local! {
    static BTN_PAINT: RefCell<Option<BtnPaint>> = const { RefCell::new(None) };
}

fn colorref((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Scale a 96-DPI value.
///
/// We are PER_MONITOR_AWARE_V2.
fn dpi_scale(hwnd: HWND, v: i32) -> i32 {
    // SAFETY: FFI call on a live window handle; returns 96 for an
    // invalid one, which degrades to no scaling rather than a wrong
    // size.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (v as i64 * dpi as i64 / 96) as i32
}

/// Ends the paint on drop.
struct BtnPaintScope {
    hwnd: HWND,
    ps: PAINTSTRUCT,
}

impl Drop for BtnPaintScope {
    fn drop(&mut self) {
        unsafe {
            let _ = EndPaint(self.hwnd, &self.ps);
        }
    }
}

/// Fill plus centered text.
unsafe fn paint_button(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `btn_wndproc` calls this only for its own `hwnd` on
    // `WM_PAINT`, the OS contract `BeginPaint`/`EndPaint` requires.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let _scope = BtnPaintScope { hwnd, ps };
    if hdc.is_invalid() {
        return;
    }

    BTN_PAINT.with(|cell| {
        let state = cell.borrow();
        let Some(state) = state.as_ref() else { return };
        if state.hwnd != hwnd {
            return;
        }

        let mut rc = RECT::default();
        // SAFETY: `hwnd` is the same live window as above.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };

        let px = dpi_scale(hwnd, state.font_size as i32);
        let face: Vec<u16> =
            state.font_name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut text: Vec<u16> = state.text.encode_utf16().collect();

        // SAFETY: `hdc` was checked non-invalid above; `face` and
        // `text` outlive every call below that reads their pointers;
        // every GDI handle created here (`bg_brush`, `font`) is
        // deleted before this closure returns.
        unsafe {
            let bg_brush = CreateSolidBrush(colorref(state.bg));
            FillRect(hdc, &rc, bg_brush);
            let _ = DeleteObject(bg_brush.into());

            let font = CreateFontW(
                -px, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                PCWSTR::from_raw(face.as_ptr()),
            );
            let old_font = SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, colorref(state.text_color));
            DrawTextW(hdc, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(hdc, old_font);
            let _ = DeleteObject(font.into());
        }
    });
}

/// Paints; also tracks clicks.
///
/// Unwinding here would be UB.
unsafe extern "system" fn btn_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: `hwnd` is the live handle the OS just supplied
            // to this `wndproc` for its own `WM_PAINT`, exactly the
            // precondition `paint_button`'s own `BeginPaint` note
            // relies on.
            let _ = catch_unwind(|| unsafe { paint_button(hwnd) });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            BTN_CLICKED.store(true, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // SAFETY: system cursor, always valid.
            if let Ok(cur) = unsafe { LoadCursorW(None, IDC_HAND) } {
                unsafe { SetCursor(Some(cur)) };
            }
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Registers the class once.
unsafe fn register_btn_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `wc` is a fully-initialised `WNDCLASSEXW` (the
    // `..Default` spread zeroes every field this module does not
    // set); `lpfnWndProc` points to `btn_wndproc`, a `'static
    // extern "system" fn` valid for the process lifetime, exactly
    // what the OS requires it to be.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(btn_wndproc),
            hInstance: hinstance,
            lpszClassName: btn_class_name(),
            hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW(IDC_ARROW)")?,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread()).context("RegisterClassExW for the button");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// The "Add to Anki" button.
///
/// Real and opaque: it catches
/// its own clicks, no overlay
/// trick needed.
pub struct AnkiButton {
    hwnd: HWND,
    capture_exclusion: CaptureExclusion,
}

impl AnkiButton {
    /// Creates the window, hidden.
    pub fn create(exclude: bool) -> Result<AnkiButton> {
        // SAFETY: mirrors `Popup::create` - `hinstance` comes from
        // `GetModuleHandleW(None)`, always valid for this process;
        // `register_btn_class` only registers one well-formed class;
        // `CreateWindowExW`'s and `SetLayeredWindowAttributes`'s
        // results are checked via `?`; `SetWindowDisplayAffinity`'s
        // failure is an accepted, non-fatal outcome, exactly as
        // `Popup::create` treats it.
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();

            register_btn_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                btn_class_name(),
                w!("chibipop-anki-button"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW for the Anki button")?;

            SetLayeredWindowAttributes(hwnd, COLORREF(0), LAYERED_ALPHA, LWA_ALPHA)
                .context("SetLayeredWindowAttributes for the Anki button")?;

            let capture_exclusion = if exclude {
                // Not `?`: the outcome is data.
                if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok() {
                    CaptureExclusion::Excluded
                } else {
                    CaptureExclusion::AttemptFailed
                }
            } else {
                CaptureExclusion::DeliberatelyNotExcluded
            };

            Ok(AnkiButton { hwnd, capture_exclusion })
        }
    }

    /// Moves, shapes, and shows.
    pub fn show_at(&self, r: PhysRect) -> Result<()> {
        // SAFETY: mirrors `Popup::show_at` — `self.hwnd` was created
        // by `create` and lives for `&self`'s lifetime; the GDI region
        // is consumed by a successful `SetWindowRgn` (which then owns
        // it — deleted only on failure); `SetWindowPos`'s result is
        // checked via `?`.
        unsafe {
            let rgn = CreateRoundRectRgn(
                0, 0, r.w, r.h,
                CORNER_RADIUS, CORNER_RADIUS,
            );
            if rgn.is_invalid() {
                anyhow::bail!(
                    "CreateRoundRectRgn({}, {}) returned a null region",
                    r.w, r.h,
                );
            }
            if SetWindowRgn(self.hwnd, Some(rgn), true) == 0 {
                let _ = DeleteObject(rgn.into());
                anyhow::bail!("SetWindowRgn failed for the Anki button");
            }

            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                r.x,
                r.y,
                r.w,
                r.h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .context("SetWindowPos for the Anki button")?;
        }
        Ok(())
    }

    /// Sets the label, repaints now.
    pub fn render(&self, text: &str, text_color: (u8, u8, u8), theme: &Theme) {
        BTN_PAINT.with(|cell| {
            *cell.borrow_mut() = Some(BtnPaint {
                hwnd: self.hwnd,
                text: text.to_string(),
                text_color,
                bg: theme.background,
                font_name: theme.font_name.clone(),
                font_size: theme.collapsed_size,
            });
        });
        // SAFETY: `self.hwnd` is valid for `&self`'s lifetime; the
        // paint state above was just stored under this same hwnd, so
        // the WM_PAINT this triggers finds it. Both calls' failures
        // are ignored - worst case is a stale repaint.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    /// Hides without destroying.
    pub fn hide(&self) {
        // SAFETY: `self.hwnd` is valid for `&self`'s lifetime (see
        // `create`); `ShowWindow`'s failure is intentionally ignored,
        // mirroring `Popup::hide`.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Re-shows after a guard hide.
    pub fn show_without_activating(&self) {
        // SAFETY: same guarantee as `hide`, above.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Asked fresh, never cached.
    pub fn is_visible(&self) -> bool {
        // SAFETY: same guarantee as `hide`, above.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether it is excluded.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion
    }

    /// Fixed height, DPI-scaled.
    pub fn height_phys(&self) -> i32 {
        dpi_scale(self.hwnd, BTN_HEIGHT)
    }

    /// Takes the click, once.
    pub fn take_click(&self) -> bool {
        BTN_CLICKED.swap(false, Ordering::SeqCst)
    }
}

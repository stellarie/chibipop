//! The popup window shell.
//!
//! Use constant alpha. Per-pixel alpha breaks WDA exclusion.

use crate::geom::PhysRect;
use crate::ui::theme::Theme;
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Popup corner radius in pixels.
const CORNER_RADIUS: i32 = 12;

/// Constant alpha value from 0 through 255.
const LAYERED_ALPHA: u8 = 230;

fn class_name() -> PCWSTR {
    w!("ChibipopPopupClass")
}

/// Complete the WM_PAINT cycle without paint output.
unsafe fn validate_paint_region(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let _ = BeginPaint(hwnd, &mut ps);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Validate WM_PAINT without paint output.
///
/// Do not let a panic cross the system callback.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let _ = catch_unwind(|| unsafe { validate_paint_region(hwnd) });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Register the popup class once for this process.
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

    // Mark the class registered only after registration succeeds.
    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Result of a capture affinity request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureExclusion {
    /// The OS accepted the exclusion request.
    Excluded,
    /// The caller made no capture exclusion request. This is the `DeliberatelyNotExcluded` state.
    DeliberatelyNotExcluded,
    /// The caller requested exclusion, but the OS refused it.
    AttemptFailed,
}

impl CaptureExclusion {
    /// Report whether the capture guard must protect the popup.
    pub fn needs_capture_guard(self) -> bool {
        !matches!(self, CaptureExclusion::Excluded)
    }

    /// Convert the request and OS result into a capture state.
    pub fn from_attempt(on: bool, ok: bool) -> CaptureExclusion {
        match (on, ok) {
            (false, _) => CaptureExclusion::DeliberatelyNotExcluded,
            (true, true) => CaptureExclusion::Excluded,
            (true, false) => CaptureExclusion::AttemptFailed,
        }
    }
}

/// The popup window and its capture state.
pub struct Popup {
    hwnd: HWND,
    capture_exclusion: Cell<CaptureExclusion>,
}

impl Popup {
    /// Create the hidden popup window.
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
                // Treat the result as state data, not as an error.
                if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok() {
                    CaptureExclusion::Excluded
                } else {
                    CaptureExclusion::AttemptFailed
                }
            } else {
                CaptureExclusion::DeliberatelyNotExcluded
            };

            Ok(Popup {
                hwnd,
                capture_exclusion: Cell::new(capture_exclusion),
            })
        }
    }

    /// Show the popup at `r` with a rounded shape.
    pub fn show_at(&self, r: PhysRect) -> Result<()> {
        unsafe {
            let region = CreateRoundRectRgn(0, 0, r.w, r.h, CORNER_RADIUS, CORNER_RADIUS);
            if region.is_invalid() {
                anyhow::bail!(
                    "CreateRoundRectRgn({}, {}) returned a null region",
                    r.w,
                    r.h
                );
            }
            // SetWindowRgn owns the region after success.
            // Delete the region only after failure.
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

            // WM_PAINT is posted first.
            // UpdateWindow forces the paint now.
            let _ = UpdateWindow(self.hwnd);
        }
        Ok(())
    }

    /// Hide the popup while its window stays alive.
    pub fn hide(&self) -> Result<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        Ok(())
    }

    /// Show the popup after the capture guard hides it.
    pub fn show_without_activating(&self) -> Result<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    /// Query visibility from the window instead of a cached value.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Return the current capture exclusion state.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion.get()
    }

    /// Set the popup alpha.
    pub fn set_alpha(&self, alpha: u8) {
        // SAFETY: `create` keeps `self.hwnd` live for the lifetime of `self`.
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
    }

    /// Apply the capture exclusion state again. The OS can refuse the request.
    pub fn set_capture_exclusion(&self, on: bool) {
        let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        // SAFETY: `create` made `self.hwnd`, and `Popup` owns it for
        // `&self`'s lifetime. The OS can refuse this non-fatal request.
        // Record the result as the new state.
        let ok = unsafe { SetWindowDisplayAffinity(self.hwnd, affinity) }.is_ok();
        self.capture_exclusion
            .set(CaptureExclusion::from_attempt(on, ok));
    }
}

/// Fixed button height at 96 DPI in pixels.
const BTN_HEIGHT: i32 = 36;

/// Store one button click until `take_click` reads and clears it.
static BTN_CLICKED: AtomicBool = AtomicBool::new(false);

fn btn_class_name() -> PCWSTR {
    w!("ChibipopBtnClass")
}

/// Keep the state that WM_PAINT needs to draw the button.
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

/// Scale a 96-DPI value for this window.
///
/// The process uses PER_MONITOR_AWARE_V2.
fn dpi_scale(hwnd: HWND, v: i32) -> i32 {
    // SAFETY: The owner supplies a live `hwnd`.
    // If GetDpiForWindow returns 0, this function does not scale the value.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (v as i64 * dpi as i64 / 96) as i32
}

/// End the button paint cycle when the scope drops.
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

/// Fill the button and center its label.
unsafe fn paint_button(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `btn_wndproc` calls this function for its own `hwnd`
    // on `WM_PAINT`. The OS requires the BeginPaint and EndPaint pair.
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
        // SAFETY: `hwnd` is the same live window handle.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };

        let px = dpi_scale(hwnd, state.font_size as i32);
        let face: Vec<u16> = state
            .font_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut text: Vec<u16> = state.text.encode_utf16().collect();

        // SAFETY: The code checked `hdc` above. `face` and `text` stay alive
        // while the calls read their pointers. The code deletes `bg_brush` and
        // `font` before this closure returns.
        unsafe {
            let bg_brush = CreateSolidBrush(colorref(state.bg));
            FillRect(hdc, &rc, bg_brush);
            let _ = DeleteObject(bg_brush.into());

            let font = CreateFontW(
                -px,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                DEFAULT_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                PCWSTR::from_raw(face.as_ptr()),
            );
            let old_font = SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, colorref(state.text_color));
            DrawTextW(
                hdc,
                &mut text,
                &mut rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            SelectObject(hdc, old_font);
            let _ = DeleteObject(font.into());
        }
    });
}

/// Handle button paint and clicks without panic propagation.
///
/// Do not let a panic cross the system callback.
unsafe extern "system" fn btn_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: The OS supplies this live `hwnd` for `WM_PAINT`.
            // `paint_button` uses the same precondition for BeginPaint.
            let _ = catch_unwind(|| unsafe { paint_button(hwnd) });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            BTN_CLICKED.store(true, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // SAFETY: `IDC_HAND` names a system cursor.
            if let Ok(cur) = unsafe { LoadCursorW(None, IDC_HAND) } {
                unsafe { SetCursor(Some(cur)) };
            }
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Register the button class once for this process.
unsafe fn register_btn_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `wc` has all required fields. `..Default` sets each other
    // field to zero. `lpfnWndProc` points to `btn_wndproc`, a `'static`
    // `extern "system" fn` that stays valid for the process lifetime.
    // The OS requires this callback lifetime.
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

/// The "Add to Anki" button uses alpha 230.
///
/// The button receives its own clicks, so the outline overlay does not need to.
pub struct AnkiButton {
    hwnd: HWND,
    capture_exclusion: Cell<CaptureExclusion>,
}

impl AnkiButton {
    /// Create the hidden button window.
    pub fn create(exclude: bool) -> Result<AnkiButton> {
        // SAFETY: `GetModuleHandleW(None)` returns a process-valid instance handle.
        // `register_btn_class` creates one valid class.
        // The code checks `CreateWindowExW` and `SetLayeredWindowAttributes`.
        // `SetWindowDisplayAffinity` can fail and does not stop creation, as in
        // `Popup::create`.
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
                // Treat the result as state data, not as an error.
                if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok() {
                    CaptureExclusion::Excluded
                } else {
                    CaptureExclusion::AttemptFailed
                }
            } else {
                CaptureExclusion::DeliberatelyNotExcluded
            };

            Ok(AnkiButton {
                hwnd,
                capture_exclusion: Cell::new(capture_exclusion),
            })
        }
    }

    /// Show the button at `r` with a rounded shape.
    pub fn show_at(&self, r: PhysRect) -> Result<()> {
        // SAFETY: `create` made `self.hwnd`, and `AnkiButton` owns it for
        // `&self`'s lifetime. A successful `SetWindowRgn` takes ownership of
        // the GDI region. The code deletes it only on failure.
        // The code checks `SetWindowPos`.
        unsafe {
            let rgn = CreateRoundRectRgn(0, 0, r.w, r.h, CORNER_RADIUS, CORNER_RADIUS);
            if rgn.is_invalid() {
                anyhow::bail!(
                    "CreateRoundRectRgn({}, {}) returned a null region",
                    r.w,
                    r.h,
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

    /// Set the label and repaint the button immediately.
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
        // SAFETY: `self.hwnd` stays valid for `&self`'s lifetime. The code stores
        // the same paint state first, so WM_PAINT can read it.
        // Both calls can fail and leave stale paint output.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = UpdateWindow(self.hwnd);
        }
    }

    /// Hide the button while its window stays alive.
    pub fn hide(&self) {
        // SAFETY: `create` keeps `self.hwnd` valid for `&self`'s lifetime.
        // The code ignores ShowWindow failure, as `Popup::hide` does.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Show the button after the capture guard hides it.
    pub fn show_without_activating(&self) {
        // SAFETY: `create` keeps `self.hwnd` valid for `&self`'s lifetime.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Query visibility from the window instead of a cached value.
    pub fn is_visible(&self) -> bool {
        // SAFETY: `create` keeps `self.hwnd` valid for `&self`'s lifetime.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Return the current capture exclusion state.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion.get()
    }

    /// Apply the capture exclusion state again. The OS can refuse the request.
    pub fn set_capture_exclusion(&self, on: bool) {
        let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        // SAFETY: `create` made `self.hwnd`, and `AnkiButton` owns it for
        // `&self`'s lifetime. The OS can refuse this non-fatal request.
        // Record the result as the new state.
        let ok = unsafe { SetWindowDisplayAffinity(self.hwnd, affinity) }.is_ok();
        self.capture_exclusion
            .set(CaptureExclusion::from_attempt(on, ok));
    }

    /// Return the button height after DPI conversion, in physical pixels.
    pub fn height_phys(&self) -> i32 {
        dpi_scale(self.hwnd, BTN_HEIGHT)
    }

    /// Take the stored click and clear it.
    pub fn take_click(&self) -> bool {
        BTN_CLICKED.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accepted_exclusion_attempt_is_recorded_as_excluded() {
        assert_eq!(
            CaptureExclusion::Excluded,
            CaptureExclusion::from_attempt(true, true)
        );
    }

    #[test]
    fn a_refused_exclusion_attempt_is_never_recorded_as_excluded() {
        assert_eq!(
            CaptureExclusion::AttemptFailed,
            CaptureExclusion::from_attempt(true, false)
        );
    }

    #[test]
    fn turning_exclusion_off_is_deliberate_whatever_the_os_answered() {
        assert_eq!(
            CaptureExclusion::DeliberatelyNotExcluded,
            CaptureExclusion::from_attempt(false, true)
        );
        assert_eq!(
            CaptureExclusion::DeliberatelyNotExcluded,
            CaptureExclusion::from_attempt(false, false)
        );
    }

    #[test]
    fn only_a_real_exclusion_leaves_the_capture_guard_disarmed() {
        assert!(!CaptureExclusion::Excluded.needs_capture_guard());
        assert!(CaptureExclusion::DeliberatelyNotExcluded.needs_capture_guard());
        assert!(CaptureExclusion::AttemptFailed.needs_capture_guard());
    }

    #[test]
    fn a_refused_attempt_still_arms_the_guard() {
        assert!(CaptureExclusion::from_attempt(true, false).needs_capture_guard());
        assert!(!CaptureExclusion::from_attempt(true, true).needs_capture_guard());
        assert!(CaptureExclusion::from_attempt(false, true).needs_capture_guard());
    }
}

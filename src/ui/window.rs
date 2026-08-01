//! The popup window shell.
//!
//! Const alpha, not per-pixel.
//! Per-pixel breaks WDA exclude.

use crate::geom::PhysRect;
use anyhow::{Context, Result};
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
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

/// Click interception overlay.
///
/// Not `WS_EX_TRANSPARENT`: eats
/// clicks so they never reach the
/// app behind the popup's button.
pub struct ClickCatcher {
    hwnd: HWND,
}

impl ClickCatcher {
    /// Invisible, click-eating.
    pub fn create() -> Result<ClickCatcher> {
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();
            register_class(hinstance)?;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW,
                class_name(),
                w!("chibipop-btn"),
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
            .context("CreateWindowExW for click catcher")?;
            // Alpha 1: invisible to the
            // eye, opaque to hit-testing.
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA)
                .context("SetLayeredWindowAttributes for click catcher")?;
            Ok(ClickCatcher { hwnd })
        }
    }

    /// Moves and shows at `r`.
    pub fn show_at(&self, r: PhysRect) -> Result<()> {
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                r.x,
                r.y,
                r.w,
                r.h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .context("SetWindowPos for click catcher")?;
        }
        Ok(())
    }

    /// Hides without destroying.
    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Re-shows after a guard hide.
    pub fn show_without_activating(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Asked fresh, never cached.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

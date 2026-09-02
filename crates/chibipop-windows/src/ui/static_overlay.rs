//! Static region outline.
//!
//! A click-through, topmost window draws the border.

use crate::geom::PhysRect;
use crate::ui::window::CaptureExclusion;
use anyhow::{Context, Result};
use std::cell::Cell;
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Border width in pixels.
const BORDER_PX: i32 = 2;

/// Teal border color.
const BORDER_COLOR: COLORREF = COLORREF(
    0xC0u32 | (0xE0u32 << 8) | (0xE0u32 << 16),
);

/// Constant alpha value from 0 through 255.
const OVERLAY_ALPHA: u8 = 180;

fn class_name() -> PCWSTR {
    w!("ChibipopStaticRegion")
}

/// Keep the window handle and rectangle that the paint callback needs.
struct PaintState {
    hwnd: HWND,
    rect: PhysRect,
}

thread_local! {
    static PAINT: std::cell::RefCell<Option<PaintState>> =
        const { std::cell::RefCell::new(None) };
}

/// Split a rectangle into border strips so the center stays transparent.
fn edge_strips(r: PhysRect, t: i32) -> Vec<PhysRect> {
    if r.w < 2 * t || r.h < 2 * t {
        return vec![r];
    }
    vec![
        PhysRect { x: 0, y: 0, w: r.w, h: t },
        PhysRect { x: 0, y: r.h - t, w: r.w, h: t },
        PhysRect { x: 0, y: t, w: t, h: r.h - 2 * t },
        PhysRect { x: r.w - t, y: t, w: t, h: r.h - 2 * t },
    ]
}

/// Paint only the border strips so the center remains click-through.
unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: The caller passes the live window handle that WM_PAINT supplies.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    struct Scope(HWND, PAINTSTRUCT);
    impl Drop for Scope {
        fn drop(&mut self) {
            unsafe { let _ = EndPaint(self.0, &self.1); }
        }
    }
    let _s = Scope(hwnd, ps);

    if hdc.is_invalid() {
        return;
    }
    PAINT.with(|cell| {
        let st = cell.borrow();
        let Some(st) = st.as_ref() else { return };
        if st.hwnd != hwnd {
            return;
        }
        // SAFETY: BeginPaint returned a valid device context.
        unsafe {
            let brush = CreateSolidBrush(BORDER_COLOR);
            if !brush.is_invalid() {
                for strip in edge_strips(st.rect, BORDER_PX) {
                    let rc = RECT {
                        left: strip.x,
                        top: strip.y,
                        right: strip.x + strip.w,
                        bottom: strip.y + strip.h,
                    };
                    FillRect(hdc, &rc, brush);
                }
                let _ = DeleteObject(brush.into());
            }
        }
    });
}

/// Handle WM_PAINT and stop panic propagation across the system callback.
unsafe extern "system" fn wndproc(
    hwnd: HWND, msg: u32,
    wp: WPARAM, lp: LPARAM,
) -> LRESULT {
    if msg == WM_PAINT {
        let _ = catch_unwind(|| unsafe { paint(hwnd) });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Register the window class once for this process.
unsafe fn register_class(hi: HINSTANCE) -> Result<()> {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::SeqCst) {
        return Ok(());
    }
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hi,
            lpszClassName: class_name(),
            hCursor: LoadCursorW(None, IDC_ARROW)
                .context("LoadCursorW")?,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread())
                .context("RegisterClassExW");
        }
    }
    DONE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Build a region that contains only the frame strips.
unsafe fn build_region(r: PhysRect) -> Result<HRGN> {
    unsafe {
        let outer = CreateRectRgn(0, 0, r.w, r.h);
        if outer.is_invalid() {
            anyhow::bail!("outer CreateRectRgn");
        }
        let t = BORDER_PX;
        if r.w >= 2 * t && r.h >= 2 * t {
            let inner = CreateRectRgn(t, t, r.w - t, r.h - t);
            if inner.is_invalid() {
                let _ = DeleteObject(outer.into());
                anyhow::bail!("inner CreateRectRgn");
            }
            if CombineRgn(
                Some(outer), Some(outer), Some(inner),
                RGN_DIFF,
            ) == RGN_ERROR {
                let _ = DeleteObject(inner.into());
                let _ = DeleteObject(outer.into());
                anyhow::bail!("CombineRgn");
            }
            let _ = DeleteObject(inner.into());
        }
        Ok(outer)
    }
}

/// A topmost window that outlines the saved static region.
pub struct StaticRegionOverlay {
    hwnd: HWND,
    exclusion: Cell<CaptureExclusion>,
}

/// Allow only one static region overlay at a time.
static LIVE: AtomicBool = AtomicBool::new(false);

impl StaticRegionOverlay {
    /// Create the hidden overlay that outlines the static region.
    pub fn create(exclude: bool) -> Result<Self> {
        if LIVE.load(Ordering::SeqCst) {
            anyhow::bail!("already exists");
        }
        unsafe {
            let hi: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW")?
                .into();
            register_class(hi)?;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TRANSPARENT,
                class_name(),
                w!("chibipop-static-region"),
                WS_POPUP,
                0, 0, 0, 0,
                None, None, Some(hi), None,
            ).context("CreateWindowExW")?;
            SetLayeredWindowAttributes(
                hwnd, COLORREF(0),
                OVERLAY_ALPHA, LWA_ALPHA,
            ).context("SetLayeredWindowAttributes")?;
            let exclusion = if exclude {
                if SetWindowDisplayAffinity(
                    hwnd, WDA_EXCLUDEFROMCAPTURE,
                ).is_ok() {
                    CaptureExclusion::Excluded
                } else {
                    CaptureExclusion::AttemptFailed
                }
            } else {
                CaptureExclusion::DeliberatelyNotExcluded
            };
            LIVE.store(true, Ordering::SeqCst);
            Ok(Self {
                hwnd,
                exclusion: Cell::new(exclusion),
            })
        }
    }

    /// Show the border at `region` in the click-through window.
    pub fn show(&self, region: PhysRect) -> Result<()> {
        unsafe {
            let rgn = build_region(region)?;
            if SetWindowRgn(self.hwnd, Some(rgn), true) == 0 {
                let _ = DeleteObject(rgn.into());
                anyhow::bail!("SetWindowRgn failed");
            }
            PAINT.with(|cell| {
                *cell.borrow_mut() = Some(PaintState {
                    hwnd: self.hwnd,
                    rect: region,
                });
            });
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                region.x, region.y,
                region.w, region.h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ).context("SetWindowPos")?;
            let _ = UpdateWindow(self.hwnd);
        }
        Ok(())
    }

    /// Hide the overlay while its window stays alive.
    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Return the overlay window handle.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Return the current capture exclusion state.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.exclusion.get()
    }

    /// Apply the capture exclusion state again. The OS can refuse the request.
    pub fn set_capture_exclusion(&self, on: bool) {
        let a = if on {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        let ok = unsafe {
            SetWindowDisplayAffinity(self.hwnd, a)
        }.is_ok();
        self.exclusion.set(
            CaptureExclusion::from_attempt(on, ok),
        );
    }
}

impl Drop for StaticRegionOverlay {
    fn drop(&mut self) {
        PAINT.with(|cell| {
            let mut st = cell.borrow_mut();
            if st.as_ref().is_some_and(|s| s.hwnd == self.hwnd) {
                *st = None;
            }
        });
        LIVE.store(false, Ordering::SeqCst);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

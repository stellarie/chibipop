//! The popup window shell.
//!
//! This establishes the window itself - flags, transparency, shape, and
//! capture exclusion - with nothing yet drawn beyond a placeholder fill.
//! Real content rendering belongs to later M3 tasks (Presentation, Theme);
//! they paint into the same `HWND` this module hands out via `Popup::hwnd()`.
//!
//! The flags below are exactly what
//! `docs/superpowers/findings/2026-07-27-m3-win32-d2d-spike.md` measured, not
//! guessed. In particular: `WDA_EXCLUDEFROMCAPTURE` (keeping the popup out of
//! M2's own OCR captures - without it, every hover would photograph the
//! popup and feed its own text back into the next lookup) was measured to be
//! **incompatible** with per-pixel-alpha rendering via `UpdateLayeredWindow`:
//! that combination fails the affinity call with a misleading "not enough
//! memory" HRESULT and then silently no-ops, leaving the window fully
//! capturable. The fix the spike proved, used here, is constant alpha via
//! `SetLayeredWindowAttributes` with ordinary `WM_PAINT`/GDI painting instead
//! of `UpdateLayeredWindow`.

use crate::geom::PhysRect;
use anyhow::{Context, Result};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Distinctive placeholder fill painted by this shell's `WM_PAINT` handler.
/// Chosen to be unlikely to occur on a real desktop, so it can double as an
/// unambiguous marker colour for the capture-exclusion measurement (see
/// `examples/exclusion_check.rs`, run once for Task 1's verification and then
/// deleted - not part of the committed tree). Tasks 2/3 (Presentation,
/// Theme) replace this fill with real D2D-rendered content.
pub const PLACEHOLDER_FILL_RGB: (u8, u8, u8) = (255, 0, 255); // magenta

/// Corner radius (both x and y) used by `SetWindowRgn`'s rounded silhouette,
/// in pixels. Matches the value the spike measured to still pass
/// `WDA_EXCLUDEFROMCAPTURE`.
const CORNER_RADIUS: i32 = 12;

/// Constant alpha applied via `SetLayeredWindowAttributes` (0-255 scale).
/// 230 matches the spike's verified-working value.
const LAYERED_ALPHA: u8 = 230;

fn class_name() -> PCWSTR {
    w!("ChibipopPopupClass")
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// Paints the placeholder fill. Everything else falls through to
/// `DefWindowProcW` - there is no message loop or window lifecycle beyond
/// paint in this task.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (r, g, b) = PLACEHOLDER_FILL_RGB;
        let brush = CreateSolidBrush(rgb(r, g, b));
        FillRect(hdc, &rc, brush);
        let _ = DeleteObject(brush.into());
        let _ = EndPaint(hwnd, &ps);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Registers the window class exactly once per process. Safe to call from
/// every `Popup::create()`; later calls are no-ops rather than the
/// `ERROR_CLASS_ALREADY_EXISTS` failure `RegisterClassExW` would otherwise
/// give on a second registration.
unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: class_name(),
        hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW(IDC_ARROW)")?,
        ..Default::default()
    };
    let atom = RegisterClassExW(&wc);
    if atom == 0 {
        return Err(Error::from_thread()).context("RegisterClassExW");
    }

    // Only latch success *after* `RegisterClassExW` has actually returned
    // one - not before, the way a `swap(true, ..)` up front would. Claiming
    // "registered" ahead of the result would leave a failed first attempt
    // stuck true for the rest of the process: every later `Popup::create()`
    // would then skip registration entirely and fail confusingly at
    // `CreateWindowExW` against a class that was never created, instead of
    // retrying registration and surfacing the real `RegisterClassExW` error.
    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// A layered, click-through, always-on-top popup window, excluded (when the
/// OS accepts it - see `capture_excluded`) from the app's own screen
/// captures.
pub struct Popup {
    hwnd: HWND,
    capture_excluded: bool,
}

impl Popup {
    /// Registers the window class if needed and creates the popup window,
    /// hidden, with every flag the spike proved necessary. Applies
    /// `WDA_EXCLUDEFROMCAPTURE` immediately - before the window is ever
    /// shown - so there is no window of time in which it could be
    /// visible but unexcluded.
    pub fn create() -> Result<Popup> {
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

            // Deliberately not `?`: whether this succeeds is itself a
            // result the caller needs, not grounds to fail window creation.
            // The spike measured this call to *silently no-op* rather than
            // error, on a window painted via UpdateLayeredWindow - stored
            // here so `capture_excluded()` can report the truth instead of
            // assuming success. Discarding it is exactly the failure mode
            // that turns the OCR tier into a feedback loop.
            let capture_excluded =
                SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok();

            Ok(Popup { hwnd, capture_excluded })
        }
    }

    /// Moves, resizes, reshapes, and shows the popup at `r` (physical
    /// virtual-desktop pixels), without taking focus or disturbing z-order
    /// beyond staying topmost. The rounded-rect region is reapplied on every
    /// call so the silhouette always matches the current size.
    pub fn show_at(&self, r: PhysRect) -> Result<()> {
        unsafe {
            let region = CreateRoundRectRgn(0, 0, r.w, r.h, CORNER_RADIUS, CORNER_RADIUS);
            if region.is_invalid() {
                anyhow::bail!("CreateRoundRectRgn({}, {}) returned a null region", r.w, r.h);
            }
            // Ownership of `region`: `SetWindowRgn` transfers it to the OS,
            // but only on success - the window owns it from then on, so it
            // must NOT be deleted below on that path (that would be a
            // double-free of a handle the OS also thinks it owns). On
            // failure, ownership never transferred: nobody else will ever
            // free this handle, so it must be deleted here or it leaks.
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

            // WM_PAINT is a posted (queued) message and nothing here runs a
            // message loop, so force it to run synchronously now via the
            // standard bypass-the-queue mechanism - otherwise the window
            // shows whatever it last painted, which on first show is
            // nothing.
            let _ = UpdateWindow(self.hwnd);
        }
        Ok(())
    }

    /// Hides the popup without destroying it.
    pub fn hide(&self) -> Result<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        Ok(())
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` was
    /// accepted by the OS at creation time. `false` here must be treated as
    /// loud, not silent: it means this popup WILL appear in the app's own
    /// screen captures - see the module docs for why that matters to M2's
    /// OCR tier.
    pub fn capture_excluded(&self) -> bool {
        self.capture_excluded
    }
}

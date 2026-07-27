//! The popup window shell.
//!
//! This establishes the window itself - flags, transparency, shape, and
//! capture exclusion. Real content is painted by `ui::render::Renderer`,
//! called directly by application code against the same `HWND` this module
//! hands out via `Popup::hwnd()` - see `render.rs`'s module docs for why
//! that call happens from application code rather than from `wndproc`
//! below. `wndproc`'s `WM_PAINT` handler here only validates the update
//! region; it used to fill a placeholder magenta rectangle (Task 1), which
//! had to go once real content existed - a stray `WM_PAINT` between hovers
//! would otherwise paint magenta over whatever `Renderer::paint` had
//! already drawn.
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
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

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

/// The actual `WM_PAINT` work, split out so it can be run inside
/// `catch_unwind` from `wndproc` - same shape as `input::hooks`'s
/// `record_mouse_move`/`mouse_hook_proc` split (W).
unsafe fn validate_paint_region(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let _ = BeginPaint(hwnd, &mut ps);
    let _ = EndPaint(hwnd, &ps);
}

/// Validates the update region on `WM_PAINT` and draws nothing itself -
/// see the module docs for why real content isn't painted here.
///
/// A panic must never unwind across this `extern "system"` boundary (M3
/// spec §6) - it would unwind into whichever window procedure the OS calls
/// next, not just this one. `catch_unwind` wraps the working half, same
/// discipline `input::hooks`'s callbacks already apply.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let _ = catch_unwind(|| unsafe { validate_paint_region(hwnd) });
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

/// The outcome of `Popup::create`'s decision about
/// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` - or the deliberate
/// choice not to attempt it at all (spec §5.1).
///
/// A bare `bool` cannot distinguish "the OS refused" from "the caller asked
/// for this", and collapsing them back into one boolean at the one place
/// that reports this at startup is exactly the confusion this type exists
/// to prevent: the first is a serious defect (the OCR tier becomes a
/// feedback loop, and nothing about the running process looks wrong until a
/// lookup is silently contaminated); the second is an intentional opt-out
/// that trades a per-capture flicker for making the popup recordable. Both
/// leave the popup **actually visible to capture APIs**, though, so both
/// need the same hide/reshow guard around every capture (`app.rs`) - see
/// `needs_capture_guard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureExclusion {
    /// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` was attempted and
    /// the OS accepted it. The popup is invisible to every capture API;
    /// M3-D7's whole point is that nothing else needs to guard a capture.
    Excluded,
    /// The caller passed `exclude: false` - the call was never attempted.
    /// Spec §5.1's opt-in-to-recording path.
    DeliberatelyNotExcluded,
    /// The caller passed `exclude: true` but the OS rejected the call (the
    /// spike's documented `0x80070008` failure mode, or any other refusal).
    /// Serious: without the capture guard, every hover photographs the
    /// popup and feeds its own rendered text back into the next OCR pass.
    AttemptFailed,
}

impl CaptureExclusion {
    /// Whether captures need the hide/reshow guard around them (`app.rs`) -
    /// true whenever the popup is not actually excluded, regardless of
    /// *why*: the on-screen risk (the popup's own rendered text landing in
    /// the next capture) is identical whether that is by request or by OS
    /// refusal.
    pub fn needs_capture_guard(self) -> bool {
        !matches!(self, CaptureExclusion::Excluded)
    }
}

/// A layered, click-through, always-on-top popup window, excluded from the
/// app's own screen captures when `exclude` asked for it and the OS
/// accepted (see `CaptureExclusion`).
pub struct Popup {
    hwnd: HWND,
    capture_exclusion: CaptureExclusion,
}

impl Popup {
    /// Registers the window class if needed and creates the popup window,
    /// hidden, with every flag the spike proved necessary.
    ///
    /// When `exclude` is true, applies `WDA_EXCLUDEFROMCAPTURE` immediately -
    /// before the window is ever shown - so there is no window of time in
    /// which it could be visible but unexcluded. When `exclude` is false,
    /// the call is skipped entirely (spec §5.1's opt-out): not attempted and
    /// not ignored, because a stored "not excluded" must be traceable to
    /// which of those two happened - see `CaptureExclusion`.
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
                // Deliberately not `?`: whether this succeeds is itself a
                // result the caller needs, not grounds to fail window
                // creation. The spike measured this call to *silently
                // no-op* rather than error, on a window painted via
                // UpdateLayeredWindow - stored here so `capture_exclusion()`
                // can report the truth instead of assuming success.
                // Discarding it is exactly the failure mode that turns the
                // OCR tier into a feedback loop.
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

    /// Re-shows the popup without activating it or touching its position,
    /// size, or painted content - the mirror half of `hide()`. Used only to
    /// undo a capture-guard hide (`app.rs`'s `CaptureGuard`), where nothing
    /// about the popup changed while it was briefly hidden, so a full
    /// `show_at` (which reapplies the window region and forces a repaint)
    /// would be redundant work for a state that is already correct.
    pub fn show_without_activating(&self) -> Result<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    /// Whether the popup is currently showing, queried fresh from the OS
    /// every call rather than cached, so it can never drift from what
    /// Windows itself believes. `app.rs`'s capture guard uses this to
    /// remember whether a temporary hide-for-capture needs to be undone
    /// afterwards, or whether the popup was already hidden and should stay
    /// that way.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether (and why not) this popup is excluded from the app's own
    /// screen captures - see `CaptureExclusion`.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion
    }
}

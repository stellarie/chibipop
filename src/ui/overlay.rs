//! The scan overlay window.
//!
//! Shaped: the middle is clear.

use crate::geom::{inset, overlay_layout, PhysRect, ScanKind, ScanRect};
use crate::ui::theme::Theme;
use crate::ui::window::CaptureExclusion;
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Constant alpha, 0-255.
const OVERLAY_ALPHA: u8 = 90;

/// Outline thickness, in pixels.
const FRAME_THICKNESS: i32 = 2;

/// Not the popup's wndproc.
fn class_name() -> PCWSTR {
    w!("ChibipopOverlayClass")
}

/// What WM_PAINT redraws from.
///
/// Keyed by hwnd; one at a time.
struct PaintState {
    hwnd: HWND,
    /// Window-local, not screen.
    rects: Vec<ScanRect>,
    pass1: (u8, u8, u8),
    tile: (u8, u8, u8),
    anchor: (u8, u8, u8),
    matched: (u8, u8, u8),
}

thread_local! {
    static PAINT_STATE: RefCell<Option<PaintState>> = const { RefCell::new(None) };
}

fn set_paint_state(hwnd: HWND, rects: Vec<ScanRect>, theme: &Theme) {
    PAINT_STATE.with(|cell| {
        *cell.borrow_mut() = Some(PaintState {
            hwnd,
            rects,
            pass1: theme.scan_pass1,
            tile: theme.scan_tile,
            anchor: theme.scan_anchor,
            matched: theme.scan_match,
        });
    });
}

fn colorref((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// A rect's four border strips.
///
/// Not bounds: rects overlap.
fn edge_strips(rect: PhysRect, thickness: i32) -> Vec<PhysRect> {
    if inset(rect, thickness).is_none() {
        return vec![rect];
    }
    let t = thickness;
    vec![
        PhysRect { x: rect.x, y: rect.y, w: rect.w, h: t },
        PhysRect { x: rect.x, y: rect.y + rect.h - t, w: rect.w, h: t },
        PhysRect { x: rect.x, y: rect.y + t, w: t, h: rect.h - 2 * t },
        PhysRect { x: rect.x + rect.w - t, y: rect.y + t, w: t, h: rect.h - 2 * t },
    ]
}

/// Ends the paint on drop.
struct PaintScope {
    hwnd: HWND,
    ps: PAINTSTRUCT,
}

impl Drop for PaintScope {
    fn drop(&mut self) {
        unsafe {
            let _ = EndPaint(self.hwnd, &self.ps);
        }
    }
}

unsafe fn paint_overlay(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `wndproc` calls this only for its own `hwnd` on `WM_PAINT`,
    // which the OS delivers with a live window handle - the same
    // precondition `ui::window::validate_paint_region` relies on.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let _scope = PaintScope { hwnd, ps };

    if !hdc.is_invalid() {
        PAINT_STATE.with(|cell| {
            let state = cell.borrow();
            let Some(state) = state.as_ref() else { return };
            if state.hwnd != hwnd {
                return;
            }
            for r in &state.rects {
                let color = match r.kind {
                    ScanKind::Pass1 => state.pass1,
                    ScanKind::Tile => state.tile,
                    ScanKind::Anchor => state.anchor,
                    ScanKind::Match => state.matched,
                };
                // SAFETY: `hdc` was validated non-invalid above; each
                // `strip` is plain stack data borrowed only for this call;
                // the brush is created and deleted within this same
                // iteration, so no handle here outlives the scope that
                // owns it.
                unsafe {
                    let brush = CreateSolidBrush(colorref(color));
                    if !brush.is_invalid() {
                        for strip in edge_strips(r.rect, FRAME_THICKNESS) {
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
            }
        });
    }
}

/// Paints on WM_PAINT.
///
/// Unwinding here would be UB.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        // SAFETY: `hwnd` is the live handle the OS just supplied to
        // this `wndproc` for its own `WM_PAINT`, exactly the
        // precondition `paint_overlay`'s own `BeginPaint` SAFETY note
        // relies on.
        let _ = catch_unwind(|| unsafe { paint_overlay(hwnd) });
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

    // SAFETY: `wc` is a fully-initialised `WNDCLASSEXW` (the `..Default`
    // spread zeroes every field this module does not set); `lpfnWndProc`
    // points to `wndproc`, a `'static extern "system" fn` valid for the
    // process lifetime, which is exactly what the OS requires it to be.
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
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            return Err(Error::from_thread()).context("RegisterClassExW");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Union of every rect's frame.
unsafe fn build_region(rects: &[ScanRect]) -> Result<HRGN> {
    // SAFETY: every `HRGN` created below is either deleted before this
    // function returns or is `accum`, the single handle handed back to the
    // caller. `CombineRgn` copies its sources' geometry into the
    // destination - it does not take ownership of `hrgnsrc1`/`hrgnsrc2` -
    // so `outer` and `inner` are each deleted right after being combined
    // in, on every path, including the early `bail!` returns. `accum`
    // itself is never deleted here: ownership of it passes to the caller
    // (`show_rects`), which is responsible for it exactly the way
    // `ui::window::Popup::show_at` is responsible for its own region.
    unsafe {
        let accum = CreateRectRgn(0, 0, 0, 0);
        if accum.is_invalid() {
            anyhow::bail!("CreateRectRgn(0, 0, 0, 0) returned a null region");
        }

        for r in rects {
            let b = r.rect;
            let outer = CreateRectRgn(b.x, b.y, b.x + b.w, b.y + b.h);
            if outer.is_invalid() {
                let _ = DeleteObject(accum.into());
                anyhow::bail!("CreateRectRgn for an outer rect returned a null region");
            }

            if let Some(interior) = inset(b, FRAME_THICKNESS) {
                let inner = CreateRectRgn(
                    interior.x,
                    interior.y,
                    interior.x + interior.w,
                    interior.y + interior.h,
                );
                if inner.is_invalid() {
                    let _ = DeleteObject(outer.into());
                    let _ = DeleteObject(accum.into());
                    anyhow::bail!("CreateRectRgn for an inner rect returned a null region");
                }
                if CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF) == RGN_ERROR {
                    let _ = DeleteObject(inner.into());
                    let _ = DeleteObject(outer.into());
                    let _ = DeleteObject(accum.into());
                    anyhow::bail!("CombineRgn(RGN_DIFF) failed while cutting a frame interior");
                }
                let _ = DeleteObject(inner.into());
            }

            if CombineRgn(Some(accum), Some(accum), Some(outer), RGN_OR) == RGN_ERROR {
                let _ = DeleteObject(outer.into());
                let _ = DeleteObject(accum.into());
                anyhow::bail!("CombineRgn(RGN_OR) failed while accumulating a frame region");
            }
            let _ = DeleteObject(outer.into());
        }

        Ok(accum)
    }
}

/// The shaped outline window.
pub struct Overlay {
    hwnd: HWND,
    capture_exclusion: CaptureExclusion,
}

/// Guards a second live Overlay.
static OVERLAY_LIVE: AtomicBool = AtomicBool::new(false);

impl Overlay {
    /// Creates the window, hidden.
    ///
    /// Errs if one is already alive.
    pub fn create(exclude_from_capture: bool) -> Result<Overlay> {
        if OVERLAY_LIVE.load(Ordering::SeqCst) {
            anyhow::bail!("an Overlay already exists; only one may be alive at a time");
        }

        // SAFETY: mirrors `ui::window::Popup::create` exactly - `hinstance`
        // comes from `GetModuleHandleW(None)`, always valid for this
        // process; `register_class` only ever registers one well-formed
        // class; `CreateWindowExW`'s and `SetLayeredWindowAttributes`'s
        // results are checked via `?`; `SetWindowDisplayAffinity`'s
        // failure is an accepted, non-fatal outcome, exactly as
        // `Popup::create` treats it.
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();

            register_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                class_name(),
                w!("chibipop-overlay"),
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
            .context("CreateWindowExW for the overlay")?;

            SetLayeredWindowAttributes(hwnd, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA)
                .context("SetLayeredWindowAttributes(LWA_ALPHA)")?;

            let capture_exclusion = if exclude_from_capture {
                if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok() {
                    CaptureExclusion::Excluded
                } else {
                    CaptureExclusion::AttemptFailed
                }
            } else {
                CaptureExclusion::DeliberatelyNotExcluded
            };

            OVERLAY_LIVE.store(true, Ordering::SeqCst);
            Ok(Overlay { hwnd, capture_exclusion })
        }
    }

    /// Reshapes and shows the rects.
    ///
    /// Empty hides it instead.
    pub fn show_rects(&self, rects: &[ScanRect], theme: &Theme) -> Result<()> {
        let Some((bounds, local)) = overlay_layout(rects) else {
            self.hide();
            return Ok(());
        };

        // SAFETY: `self.hwnd` was created by `create` and is destroyed
        // only by this `Overlay`'s own `Drop`, so it is valid for the
        // lifetime of `&self`. `build_region`'s returned `HRGN` is either
        // consumed by a successful `SetWindowRgn` (which then owns it -
        // never deleted on that path) or explicitly deleted on the
        // failure path right below, exactly like
        // `ui::window::Popup::show_at`.
        unsafe {
            let rgn = build_region(&local)?;

            if SetWindowRgn(self.hwnd, Some(rgn), true) == 0 {
                let _ = DeleteObject(rgn.into());
                anyhow::bail!("SetWindowRgn failed to apply the scan outline shape");
            }

            set_paint_state(self.hwnd, local, theme);

            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                bounds.x,
                bounds.y,
                bounds.w,
                bounds.h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .context("SetWindowPos to show the overlay")?;

            let _ = UpdateWindow(self.hwnd);
        }
        Ok(())
    }

    /// Hides without destroying.
    pub fn hide(&self) {
        // SAFETY: `self.hwnd` is valid for the lifetime of `&self` (see
        // `show_rects`); `ShowWindow`'s failure is intentionally ignored,
        // mirroring `Popup::hide`.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether it is excluded.
    ///
    /// May differ from the popup's.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion
    }

    /// Re-applies live; may refuse.
    pub fn set_capture_exclusion(&self, on: bool) {
        let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        // SAFETY: `self.hwnd` was created by `create` and is destroyed
        // only by this `Overlay`'s own `Drop`, so it is valid for
        // `&self`'s lifetime. Refusal is accepted, as in `create`.
        unsafe {
            let _ = SetWindowDisplayAffinity(self.hwnd, affinity);
        }
    }
}

impl Drop for Overlay {
    /// This one is torn down.
    fn drop(&mut self) {
        PAINT_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            if state.as_ref().is_some_and(|s| s.hwnd == self.hwnd) {
                *state = None;
            }
        });
        OVERLAY_LIVE.store(false, Ordering::SeqCst);
        // SAFETY: `self.hwnd` was created by `create` and `drop` runs at
        // most once (ordinary `Drop` semantics), so this always targets a
        // window this process owns and has not already destroyed.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PhysPoint;

    /// Frame yes, interior never.
    #[test]
    fn edge_strips_cover_the_frame_but_never_the_interior() {
        let rect = PhysRect { x: 10, y: 10, w: 100, h: 40 };
        let t = FRAME_THICKNESS;
        let strips = edge_strips(rect, t);

        assert_eq!(
            vec![
                PhysRect { x: 10, y: 10, w: 100, h: t },
                PhysRect { x: 10, y: 10 + 40 - t, w: 100, h: t },
                PhysRect { x: 10, y: 10 + t, w: t, h: 40 - 2 * t },
                PhysRect { x: 10 + 100 - t, y: 10 + t, w: t, h: 40 - 2 * t },
            ],
            strips,
            "top, bottom, left, right, in that order"
        );

        let frame_points = [
            PhysPoint { x: rect.x, y: rect.y },
            PhysPoint { x: rect.x + rect.w - 1, y: rect.y },
            PhysPoint { x: rect.x, y: rect.y + rect.h - 1 },
            PhysPoint { x: rect.x + rect.w - 1, y: rect.y + rect.h - 1 },
            PhysPoint { x: rect.x + rect.w / 2, y: rect.y },
            PhysPoint { x: rect.x + rect.w / 2, y: rect.y + rect.h - 1 },
            PhysPoint { x: rect.x, y: rect.y + rect.h / 2 },
            PhysPoint { x: rect.x + rect.w - 1, y: rect.y + rect.h / 2 },
        ];
        for p in frame_points {
            assert!(strips.iter().any(|s| s.contains(p)), "no strip covers frame point {p:?}");
        }

        let center = rect.center();
        for s in &strips {
            assert!(!s.contains(center), "strip {s:?} covers the interior centre {center:?}");
        }
    }

    /// Too thin: no interior.
    #[test]
    fn edge_strips_of_a_too_thin_rect_is_the_whole_rect() {
        let rect = PhysRect { x: 0, y: 0, w: 17, h: 3 };
        assert_eq!(vec![rect], edge_strips(rect, FRAME_THICKNESS));
    }

    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn create_after_drop_succeeds_while_a_second_live_one_is_rejected() {
        let first = Overlay::create(false).expect("first create must succeed");

        let blocked = Overlay::create(false);
        assert!(blocked.is_err(), "a second live Overlay must be rejected");

        drop(first);

        let second = Overlay::create(false).expect("create after drop must succeed");
        drop(second);
    }
}

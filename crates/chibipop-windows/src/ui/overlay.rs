//! The scan overlay window.
//!
//! A window region shapes the overlay.
//! Only the outline frames stay opaque. The area inside each rect stays clear.

use crate::geom::{inset, overlay_layout, PhysRect, ScanKind, ScanRect};
use crate::ui::theme::Theme;
use crate::ui::window::CaptureExclusion;
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The window's constant alpha, from 0 to 255.
const OVERLAY_ALPHA: u8 = 90;

/// The outline width in pixels.
const FRAME_THICKNESS: i32 = 2;

/// The overlay window class. The popup uses a different wndproc.
fn class_name() -> PCWSTR {
    w!("ChibipopOverlayClass")
}

/// Stores the data that WM_PAINT uses for a redraw.
///
/// The `hwnd` field identifies the state.
/// Only one overlay exists at a time, so one slot is enough.
struct PaintState {
    hwnd: HWND,
    /// The rects in window coordinates, not in screen coordinates.
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

/// Returns the ring just outside each rect.
///
/// The overlay draws its outlines **outset**.
/// An inside stroke would touch the same pixels that the next grab reads.
/// A capture must never include pixels that chibipop drew
/// (ARCHITECTURE.md#capture-and-masking).
/// This function inflates each rect by the stroke thickness, so the full frame stays in the band
/// around the rect.
/// The result keeps `inset(outset(r), FRAME_THICKNESS) == r`, and the capture rect stays clear.
fn outset(rects: &[ScanRect]) -> Vec<ScanRect> {
    rects
        .iter()
        .map(|r| ScanRect {
            rect: r.rect.inflated(FRAME_THICKNESS, FRAME_THICKNESS),
            kind: r.kind,
        })
        .collect()
}

/// Returns the four border strips of a rect.
///
/// The strips do not define the rect bounds.
/// Rects can overlap, so each rect needs its own four strips.
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

/// Calls `EndPaint` on drop. This closes the paint even after an early return.
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
    // SAFETY: `wndproc` calls this function only for its own `hwnd` and only for `WM_PAINT`.
    // The system sends that message with a live window handle.
    // `ui::window::validate_paint_region` has the same precondition.
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
                // SAFETY: The check above proves that `hdc` is valid.
                // Each `strip` is stack data, and this call borrows it only for the call.
                // This loop creates and deletes the brush in the same pass.
                // No handle here outlives the scope that owns it.
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

/// Paints the outlines when the window receives WM_PAINT.
///
/// A panic that crosses this `extern "system"` boundary causes undefined behavior.
/// `catch_unwind` stops that panic.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        // SAFETY: `hwnd` is the live handle that the operating system gave this
        // `wndproc` for its `WM_PAINT` message.
        // `paint_overlay` relies on the same fact for its `BeginPaint` call.
        let _ = catch_unwind(|| unsafe { paint_overlay(hwnd) });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Registers the window class once.
unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `wc` is a fully initialized `WNDCLASSEXW`. The `..Default`
    // spread zeroes every field that this module does not set.
    // `lpfnWndProc` points to a `'static extern "system" fn` that
    // stays valid for the process lifetime. The operating system needs this form.
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

/// Builds the union of every rect's frame.
unsafe fn build_region(rects: &[ScanRect]) -> Result<HRGN> {
    // SAFETY: This code deletes every valid `HRGN` that it creates below.
    // It deletes `outer` and `inner` after each combine and on every
    // `bail!` return. It also deletes `accum` on those error paths.
    // `CombineRgn` copies source geometry into the destination.
    // It does not take ownership of `hrgnsrc1` or `hrgnsrc2`.
    // On success, `accum` is the only handle left.
    // `show_rects` receives and owns `accum`, like `ui::window::Popup::show_at` owns its region.
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

/// The window with the shaped outline.
pub struct Overlay {
    hwnd: HWND,
    capture_exclusion: Cell<CaptureExclusion>,
}

/// True while an `Overlay` exists. It prevents a second instance.
static OVERLAY_LIVE: AtomicBool = AtomicBool::new(false);

impl Overlay {
    /// Creates the window and leaves it hidden.
    ///
    /// Returns an error if an `Overlay` already exists.
    pub fn create(exclude_from_capture: bool) -> Result<Overlay> {
        if OVERLAY_LIVE.load(Ordering::SeqCst) {
            anyhow::bail!("an Overlay already exists; only one may be alive at a time");
        }

        // SAFETY: This block follows `ui::window::Popup::create`.
        // `hinstance` comes from `GetModuleHandleW(None)`, which is valid for this process.
        // `register_class` registers one well-formed class.
        // The `?` operator checks `CreateWindowExW` and `SetLayeredWindowAttributes`.
        // A `SetWindowDisplayAffinity` failure is allowed and is not fatal, as in `Popup::create`.
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
            Ok(Overlay { hwnd, capture_exclusion: Cell::new(capture_exclusion) })
        }
    }

    /// Reshapes the window and shows the rects.
    ///
    /// An empty list hides the window.
    /// The outlines stay outside the caller's rects, so the overlay never paints on a scan region
    /// that it shows (ARCHITECTURE.md#capture-and-masking).
    pub fn show_rects(&self, rects: &[ScanRect], theme: &Theme) -> Result<()> {
        let Some((bounds, local)) = overlay_layout(&outset(rects)) else {
            self.hide();
            return Ok(());
        };

        // SAFETY: `create` made `self.hwnd`, and only `Drop` for this `Overlay` destroys it.
        // The handle stays valid for the life of `&self`.
        // When `SetWindowRgn` succeeds, it takes the `HRGN` from `build_region` and owns it.
        // This path does not delete the region.
        // The failure path deletes it, like `ui::window::Popup::show_at`.
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

    /// Hides the window but keeps it alive.
    pub fn hide(&self) {
        // SAFETY: `self.hwnd` stays valid for the life of `&self`. See `show_rects`.
        // This code ignores a `ShowWindow` failure on purpose, like `Popup::hide`.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Reports the overlay's capture exclusion state.
    ///
    /// This state can differ from the popup's state.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion.get()
    }

    /// Applies capture exclusion to the live window. The system can refuse the request.
    pub fn set_capture_exclusion(&self, on: bool) {
        let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        // SAFETY: `create` made `self.hwnd`, and only `Drop` for this `Overlay` destroys it.
        // The handle stays valid for the life of `&self`.
        // This code accepts a refusal, as `create` does.
        // It records that refusal because it is the new state.
        let ok = unsafe { SetWindowDisplayAffinity(self.hwnd, affinity) }.is_ok();
        self.capture_exclusion.set(CaptureExclusion::from_attempt(on, ok));
    }
}

impl Drop for Overlay {
    /// Clears the paint state and latch, then destroys the window.
    fn drop(&mut self) {
        PAINT_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            if state.as_ref().is_some_and(|s| s.hwnd == self.hwnd) {
                *state = None;
            }
        });
        OVERLAY_LIVE.store(false, Ordering::SeqCst);
        // SAFETY: `create` made `self.hwnd`, and `drop` runs at most once.
        // `Drop` guarantees this behavior.
        // The call therefore targets a window that this process owns and has not destroyed.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PhysPoint;

    /// The strips must cover the frame but never the interior.
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

    /// A rect that is too thin for an interior gives one strip for the whole rect.
    #[test]
    fn edge_strips_of_a_too_thin_rect_is_the_whole_rect() {
        let rect = PhysRect { x: 0, y: 0, w: 17, h: 3 };
        assert_eq!(vec![rect], edge_strips(rect, FRAME_THICKNESS));
    }

    /// The overlay must never paint inside a capture rect.
    #[test]
    fn outset_strokes_never_touch_the_rect_they_outline() {
        let scan = PhysRect { x: 100, y: 200, w: 60, h: 30 };
        let out = outset(&[ScanRect { rect: scan, kind: ScanKind::Pass1 }]);
        assert_eq!(1, out.len());
        assert_eq!(ScanKind::Pass1, out[0].kind, "the kind, and so the colour, is kept");
        assert_eq!(
            Some(scan),
            inset(out[0].rect, FRAME_THICKNESS),
            "the ring's interior is exactly the scan rect"
        );

        for strip in edge_strips(out[0].rect, FRAME_THICKNESS) {
            assert_eq!(
                None,
                strip.intersection(scan),
                "strip {strip:?} would paint inside the capture rect"
            );
        }
    }

    /// The outset strokes touch the rect. They do not leave a gap.
    #[test]
    fn outset_strokes_sit_flush_against_the_rect() {
        let scan = PhysRect { x: 0, y: 0, w: 20, h: 20 };
        let ring = outset(&[ScanRect { rect: scan, kind: ScanKind::Tile }])[0].rect;
        let t = FRAME_THICKNESS;
        assert_eq!(PhysRect { x: -t, y: -t, w: 20 + 2 * t, h: 20 + 2 * t }, ring);
        let left = edge_strips(ring, FRAME_THICKNESS)[2];
        assert_eq!(scan.x, left.x + left.w, "the left stroke ends where the rect begins");
    }

    #[test]
    fn outset_of_no_rects_is_no_rects() {
        assert!(outset(&[]).is_empty());
    }

    /// This test needs a real desktop session.
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

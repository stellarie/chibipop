//! The scan overlay window - faint outlines drawn over the small screen
//! regions M2's OCR pipeline actually captures (pass-1 box, forward tiles,
//! the resolved anchor), so that geometry stops being invisible arithmetic.
//!
//! **Shaped, not painted transparent, and why.** `ui::window`'s module docs
//! record the measurement this window relies on too: `WDA_EXCLUDEFROMCAPTURE`
//! is incompatible with per-pixel alpha via `UpdateLayeredWindow` - the
//! affinity call fails with a misleading "not enough memory" HRESULT and
//! silently no-ops, leaving the window fully capturable. An outline needs an
//! *interior*-transparent shape, and constant alpha
//! (`SetLayeredWindowAttributes`) cannot single out an interior - it applies
//! uniformly to the whole window. So this window is shaped instead:
//! `SetWindowRgn` is set to the union of every rect's *frame* region (its
//! outer rectangle minus its inset interior), so the interior is genuinely
//! transparent rather than merely unpainted, and `WM_PAINT` separately fills
//! each rect's own four border strips - not its full bounds - in its kind's
//! colour, so one rect's fill can never repaint a neighbour's frame where
//! the two overlap (`edge_strips`).
//!
//! Structure otherwise mirrors `ui::window` throughout: the one-shot class
//! registration guard, the same extended-style set, constant alpha via
//! `SetLayeredWindowAttributes`, the same opt-in `SetWindowDisplayAffinity`,
//! and the same `SetWindowRgn` ownership/leak discipline on every path.

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

/// Constant alpha applied via `SetLayeredWindowAttributes` (0-255 scale).
/// 90 - the spec's own word for the look this produces is "faint".
const OVERLAY_ALPHA: u8 = 90;

/// Frame thickness, in physical pixels, that each rect is inset by to find
/// its interior (`geom::inset`'s `thickness` argument) - what remains after
/// removing that interior is the outline `WM_PAINT` actually shows.
const FRAME_THICKNESS: i32 = 2;

/// Distinct from `ui::window`'s popup class: a shared class would mean a
/// shared `wndproc`, and this window's `WM_PAINT` handling shares nothing
/// with the popup's.
fn class_name() -> PCWSTR {
    w!("ChibipopOverlayClass")
}

/// What `WM_PAINT` needs to redraw the last `Overlay::show_rects` call's
/// content. Unlike `ui::window::Popup`, which paints imperatively right
/// after showing and keeps no state its `wndproc` can reach, this window's
/// `wndproc` really does need to reproduce the outlines on every repaint
/// (e.g. after the overlay is occluded and re-exposed) - and `wndproc` is a
/// bare `extern "system" fn` with no `self` to hold them on.
///
/// A thread-local, not `GWLP_USERDATA`: every `Overlay` is created and
/// driven from the main thread only, the same assumption `ui::window`'s
/// process-wide class-registration guard already makes, so there is no
/// concurrent access for a `RefCell` to guard against, and it avoids the
/// raw-pointer lifetime bookkeeping `GWLP_USERDATA` would need instead
/// (stashing, and later reclaiming, a boxed pointer across `create`/`Drop`).
/// `HWND`'s raw pointer field also makes `Overlay` itself `!Send`, so the
/// compiler already rules out the one thing that would make this unsound.
///
/// Keyed by `hwnd` rather than holding one bare value, so a `WM_PAINT` for
/// any window other than the current overlay's paints nothing instead of
/// showing stale content under the wrong `HWND`.
struct PaintState {
    hwnd: HWND,
    /// Window-local rectangles, exactly as `overlay_layout` returns them.
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

/// The four border strips `paint_overlay` fills for one rect, `thickness`
/// px wide on each side - not its full bounds, so that when two rects
/// overlap (routine - see `geom::overlay_layout`'s own doc comment),
/// filling the later one can never repaint pixels of the earlier one's
/// frame that fall outside its own strips (IMPORTANT-2).
///
/// Mirrors `build_region`'s use of `geom::inset` exactly: when `inset`
/// returns `None` the rect has no interior at all (too thin - see its doc
/// comment), so the four strips would just be the rect sliced apart for no
/// benefit, and the whole rect is returned instead, unchanged from filling
/// its bounds directly.
///
/// Pure geometry, no window needed - see the tests below.
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

/// The actual `WM_PAINT` work, split out so it can run inside
/// `catch_unwind` from `wndproc` - same shape as `ui::window`'s
/// `validate_paint_region`/`wndproc` split.
///
/// `BeginPaint`/`EndPaint` are unconditionally paired (mirroring
/// `render.rs`'s `BeginDraw`/`EndDraw` discipline): a bad `hdc` skips only
/// the fill loop below, never the matching `EndPaint`, so the OS's
/// paint/update-region bookkeeping for `hwnd` always gets closed out.
unsafe fn paint_overlay(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `wndproc` calls this only for its own `hwnd` on `WM_PAINT`,
    // which the OS delivers with a live window handle - the same
    // precondition `ui::window::validate_paint_region` relies on.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };

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

    // SAFETY: pairs the `BeginPaint` above unconditionally - see the doc
    // comment above this function for why that pairing must not branch.
    let _ = unsafe { EndPaint(hwnd, &ps) };
}

/// Validates/paints on `WM_PAINT`; forwards everything else. A panic must
/// never unwind across this `extern "system"` boundary (M3 spec §6) - it
/// would unwind into whichever window procedure the OS calls next, not just
/// this one - so `catch_unwind` wraps the working half, matching
/// `ui::window::wndproc`'s own discipline.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        // SAFETY: `hwnd` is the live handle the OS just supplied to
        // this `wndproc` for its own `WM_PAINT`, exactly the
        // precondition `paint_overlay`'s own `BeginPaint` SAFETY note
        // relies on.
        let _ = catch_unwind(|| unsafe { paint_overlay(hwnd) });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Registers the window class exactly once per process - see
/// `ui::window::register_class`, whose ordering rationale (latch `true`
/// only *after* `RegisterClassExW` actually succeeds) applies identically
/// here.
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

/// Builds the union of every local rect's frame region: each rect's outer
/// rectangle, minus its inset interior when `geom::inset` returns one, or
/// the solid outer rectangle when it does not - a rectangle no thicker than
/// two frame borders is all border, so there is nothing left to cut out,
/// and `geom::inset` returns `None` for exactly that case (it happens in
/// practice - see its own doc comment).
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

/// A layered, click-through, always-on-top window that outlines the OCR
/// pipeline's capture regions instead of painting content - see the module
/// docs for why it is shaped rather than made transparent.
pub struct Overlay {
    hwnd: HWND,
    capture_exclusion: CaptureExclusion,
}

/// Guards against a second live `Overlay` - see `create`'s docs for why
/// only one may exist. Released by `Drop`, so create -> drop -> create
/// succeeds. Only ever stored `true` after `create` has fully
/// succeeded, mirroring `register_class`'s own latch-after-success
/// ordering: setting it any earlier would leave it stuck `true` after
/// a failed `create`, permanently refusing every later attempt.
static OVERLAY_LIVE: AtomicBool = AtomicBool::new(false);

impl Overlay {
    /// Registers the window class if needed and creates the overlay
    /// window, hidden, with the same flags `ui::window::Popup::create`
    /// uses (see the module docs).
    ///
    /// Only one `Overlay` may be alive at a time: `PAINT_STATE` is a
    /// single process-global slot keyed by `hwnd` (see its doc
    /// comment), so a second live instance would let each
    /// `show_rects` clobber the other's entry - the loser would find
    /// `state.hwnd != hwnd` on its next repaint and paint nothing.
    /// Calling this while an `Overlay` already exists returns `Err`
    /// instead of that silently-broken window; dropping the existing
    /// `Overlay` first (its `Drop` releases the guard) lets a later
    /// call succeed.
    ///
    /// When `exclude_from_capture` is true, `SetWindowDisplayAffinity` is
    /// attempted the same way `Popup::create` attempts it for the popup -
    /// deliberately not propagated via `?`, since whether the OS accepts it
    /// is not grounds to fail window creation. The outcome is captured into
    /// a `CaptureExclusion`, exactly like `Popup::create` does, and readable
    /// back via `capture_exclusion()` - `app.rs`'s `run` uses it so the
    /// overlay's own affinity result (which can differ from the popup's) is
    /// never silently dropped: it is reported on `AttemptFailed` and folded
    /// into whether the capture guard needs to run, same as `Popup`'s.
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

    /// Lays out `rects` (`geom::overlay_layout`), moves/sizes/reshapes the
    /// window to their bounds, stores `rects`/`theme` where `WM_PAINT` can
    /// reach them, and shows the window without activating it. Empty
    /// `rects` hides the overlay instead of showing an empty window.
    ///
    /// Finishes with an explicit `UpdateWindow`: `WM_PAINT` is only a
    /// posted message, and nothing here runs a message loop to
    /// dispatch it, so without this call the freshly-applied region
    /// would show stale or empty content until something else happens
    /// to pump the queue - the same reason `Popup::show_at` does it.
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

    /// Hides the overlay without destroying it.
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

    /// Whether (and why not) this overlay is excluded from the app's own
    /// screen captures - see `CaptureExclusion`. Independent of
    /// `Popup::capture_exclusion()`: the two windows get the same
    /// `exclude_from_capture` input but the OS accepts or refuses each
    /// window's affinity call on its own, so one can diverge from the
    /// other.
    pub fn capture_exclusion(&self) -> CaptureExclusion {
        self.capture_exclusion
    }
}

impl Drop for Overlay {
    /// `ui::window::Popup` has no `Drop` - one popup is created and lives
    /// for the process's lifetime, so the OS reclaims it on exit. This
    /// window is created and torn down per this task's spec instead, so it
    /// needs an explicit one; this is a deliberate difference from
    /// `Popup`'s pattern, not an oversight.
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

    /// IMPORTANT-2: the four strips must together cover the frame and must
    /// never reach the interior. Same rect `geom.rs`'s own
    /// `inset_leaves_an_interior_for_an_ordinary_rect` uses, so it is
    /// pinned to have an interior at `FRAME_THICKNESS`.
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

    /// A rect no thicker than two borders has no interior (`geom::inset`),
    /// so there is nothing to slice into strips - same 17x3 word box
    /// `geom.rs`'s own `inset_of_a_rect_thinner_than_its_border_has_no_interior`
    /// pins.
    #[test]
    fn edge_strips_of_a_too_thin_rect_is_the_whole_rect() {
        let rect = PhysRect { x: 0, y: 0, w: 17, h: 3 };
        assert_eq!(vec![rect], edge_strips(rect, FRAME_THICKNESS));
    }

    /// Needs a real desktop session - `CreateWindowExW` fails headless
    /// (no interactive window station), so this only runs explicitly
    /// via `cargo test -- --ignored`, never as part of the normal suite.
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

//! The Region selector lets the user draw a rectangle on the virtual desktop.

use crate::geom::{PhysPoint, PhysRect};
use crate::input::hooks::Hooks;
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Minimum width or height for a valid drag, in physical pixels.
const MIN_DRAG_PX: i32 = 5;
/// Alpha value for the dim fill, from 0 through 255.
const DIM_ALPHA: u8 = 102;
/// Border width for the selection frame, in physical pixels.
const BORDER_PX: i32 = 2;
/// Virtual-key code for `VK_ESCAPE`.
const VK_ESCAPE: usize = 0x1B;

fn class_name() -> PCWSTR {
    w!("ChibipopRegionSelect")
}

thread_local! {
    static ANCHOR: Cell<Option<PhysPoint>> = const { Cell::new(None) };
    static RESULT: Cell<Option<PhysRect>> = const { Cell::new(None) };
    static DONE: Cell<bool> = const { Cell::new(false) };
    static PAINT_CTX: RefCell<Option<PaintCtx>> = const { RefCell::new(None) };
}

/// Return the smallest `PhysRect` that contains both drag points.
fn normalized_rect(a: PhysPoint, b: PhysPoint) -> PhysRect {
    PhysRect {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        w: (a.x - b.x).abs(),
        h: (a.y - b.y).abs(),
    }
}

/// Return true when either drag dimension reaches `MIN_DRAG_PX`.
fn meets_drag_threshold(r: PhysRect) -> bool {
    r.w >= MIN_DRAG_PX || r.h >= MIN_DRAG_PX
}

/// Return the virtual desktop as `(x, y, w, h)` coordinates and dimensions.
fn virtual_screen() -> (i32, i32, i32, i32) {
    // SAFETY: `GetSystemMetrics` accepts these metric indexes without other preconditions.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Clear alpha inside the selected rectangle after the function clips it to the virtual desktop.
fn punch_through(pixels: &mut [u32], vw: i32, vh: i32, sel: PhysRect) {
    let top = sel.y.max(0);
    let bottom = (sel.y + sel.h).min(vh);
    let left = sel.x.max(0);
    let right = (sel.x + sel.w).min(vw);
    if left >= right || top >= bottom {
        return;
    }
    for row in top..bottom {
        let base = (row * vw) as usize;
        pixels[base + left as usize..base + right as usize].fill(0);
    }
}

/// Draw the selection frame with a width of `BORDER_PX` pixels.
fn paint_border(pixels: &mut [u32], vw: i32, vh: i32, sel: PhysRect) {
    let white = (0xFFu32 << 24) | 0x00FF_FFFF;
    let outer = sel.inflated(BORDER_PX, BORDER_PX);
    let top = outer.y.max(0);
    let bottom = (outer.y + outer.h).min(vh);
    let left = outer.x.max(0);
    let right = (outer.x + outer.w).min(vw);
    if left >= right || top >= bottom {
        return;
    }
    for row in top..bottom {
        let base = (row * vw) as usize;
        let top_band = row < outer.y + BORDER_PX;
        let bottom_band = row >= outer.y + outer.h - BORDER_PX;
        if top_band || bottom_band {
            pixels[base + left as usize..base + right as usize].fill(white);
            continue;
        }
        let inner_left = sel.x.clamp(left, right);
        let inner_right = (sel.x + sel.w).clamp(left, right);
        if inner_left > left {
            pixels[base + left as usize..base + inner_left as usize].fill(white);
        }
        if right > inner_right {
            pixels[base + inner_right as usize..base + right as usize].fill(white);
        }
    }
}

/// A screen device context that releases its handle when it drops.
struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `GetDC(None)` in `build_paint_ctx`.
        // This `Drop` implementation releases that handle exactly once.
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

/// A memory device context that deletes its handle when it drops.
struct MemDc(HDC);

impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateCompatibleDC` in `build_paint_ctx`.
        // This `Drop` implementation deletes it once after `PaintCtx::drop`
        // deselects its bitmap.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// A DIB section that deletes its handle when it drops.
struct Dib(HBITMAP);

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateDIBSection` in `build_paint_ctx`.
        // This `Drop` implementation deletes it once after `PaintCtx::drop`
        // deselects it.
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

/// A paint context that reuses one DIB for each overlay update.
struct PaintCtx {
    screen: ScreenDc,
    mem: MemDc,
    // Keep this value so `Dib::drop` releases the DIB.
    _dib: Dib,
    old: HGDIOBJ,
    bits: *mut u32,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
}

impl PaintCtx {
    fn fill(&self, selection: Option<(PhysPoint, PhysPoint)>) {
        // SAFETY: `CreateDIBSection` gave `self.bits` space for `vw * vh` pixels.
        // `PaintCtx` owns the DIB, so `Dib::drop` frees that space after the last `fill` call.
        // The paint loop uses one thread and one call at a time, so no other code reads or
        // writes this buffer.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(self.bits, self.vw as usize * self.vh as usize)
        };
        pixels.fill((DIM_ALPHA as u32) << 24);

        let Some((a, b)) = selection else { return };
        let sel = normalized_rect(a, b).translated(-self.vx, -self.vy);
        punch_through(pixels, self.vw, self.vh, sel);
        paint_border(pixels, self.vw, self.vh, sel);
    }

    fn present(&self, hwnd: HWND) {
        let pt_src = POINT { x: 0, y: 0 };
        let pt_dst = POINT {
            x: self.vx,
            y: self.vy,
        };
        let sz = SIZE {
            cx: self.vw,
            cy: self.vh,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: `PaintCtx` owns `self.screen.0` and `self.mem.0` until `drop`.
        // `self.mem.0` keeps the pixel-filled DIB selected for this call.
        // `UpdateLayeredWindow` requires that DIB in its source DC.
        unsafe {
            let _ = UpdateLayeredWindow(
                hwnd,
                Some(self.screen.0),
                Some(&pt_dst),
                Some(&sz),
                Some(self.mem.0),
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

impl Drop for PaintCtx {
    fn drop(&mut self) {
        // SAFETY: `self.mem.0` remains valid here. Its `Drop` runs after this method returns.
        // Restore the old object before `Dib` and `MemDc` drop. GDI requires this order before
        // it deletes a bitmap selected into the DC.
        unsafe {
            SelectObject(self.mem.0, self.old);
        }
    }
}

/// Build the DIB once for all overlay updates.
fn build_paint_ctx() -> Result<PaintCtx> {
    let (vx, vy, vw, vh) = virtual_screen();

    // SAFETY: `ScreenDc`, `MemDc`, and `Dib` release their handles in `Drop`.
    // An early `?` or `bail!` therefore releases every handle that this block creates.
    // The code checks `CreateDIBSection`'s `bits` pointer before `PaintCtx` stores it.
    // `PaintCtx` owns `Dib`, so the pointer stays valid until the context drops.
    unsafe {
        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            anyhow::bail!("GetDC(None) returned an invalid screen DC");
        }
        let mem = MemDc(CreateCompatibleDC(Some(screen.0)));
        if mem.0.is_invalid() {
            anyhow::bail!("CreateCompatibleDC returned an invalid DC");
        }

        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: vw,
            biHeight: -vh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = Dib(
            CreateDIBSection(Some(mem.0), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .context("CreateDIBSection for the selection overlay")?,
        );
        if bits.is_null() {
            anyhow::bail!("CreateDIBSection returned a null pixel buffer");
        }

        let old = SelectObject(mem.0, dib.0.into());

        Ok(PaintCtx {
            screen,
            mem,
            _dib: dib,
            old,
            bits: bits as *mut u32,
            vx,
            vy,
            vw,
            vh,
        })
    }
}

fn paint_overlay(hwnd: HWND, selection: Option<(PhysPoint, PhysPoint)>) {
    PAINT_CTX.with(|cell| {
        let ctx = cell.borrow();
        let Some(ctx) = ctx.as_ref() else { return };
        ctx.fill(selection);
        ctx.present(hwnd);
    });
}

fn cursor_point() -> PhysPoint {
    let mut pt = POINT::default();
    // SAFETY: `pt` is writable stack storage that remains valid for this call.
    // `GetCursorPos` has no other preconditions.
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    PhysPoint { x: pt.x, y: pt.y }
}

fn on_lbuttondown(hwnd: HWND) {
    ANCHOR.set(Some(cursor_point()));
    // SAFETY: `wndproc` receives `hwnd` from the OS for each message and passes it here.
    // `hwnd` therefore identifies a live window.
    unsafe {
        SetCapture(hwnd);
    }
}

fn on_mousemove(hwnd: HWND) {
    let Some(anchor) = ANCHOR.get() else { return };
    paint_overlay(hwnd, Some((anchor, cursor_point())));
}

/// Commit the drag when one dimension reaches the threshold.
fn on_lbuttonup() {
    // SAFETY: `ReleaseCapture` has no preconditions.
    let _ = unsafe { ReleaseCapture() };
    if let Some(anchor) = ANCHOR.get() {
        let r = normalized_rect(anchor, cursor_point());
        if meets_drag_threshold(r) {
            RESULT.set(Some(r));
        }
    }
    ANCHOR.set(None);
    DONE.set(true);
}

fn on_cancel() {
    ANCHOR.set(None);
    DONE.set(true);
}

/// Dispatch each window message to its handler.
///
/// A panic that crosses this system callback causes undefined behavior.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let _ = catch_unwind(|| on_lbuttondown(hwnd));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let _ = catch_unwind(|| on_mousemove(hwnd));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = catch_unwind(on_lbuttonup);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let _ = catch_unwind(on_cancel);
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE => {
            let _ = catch_unwind(on_cancel);
            LRESULT(0)
        }
        // SAFETY: The OS supplies `hwnd`, `msg`, `wp`, and `lp` for this callback.
        // These values stay valid for the callback duration.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Register the window class once per process.
unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `..Default` initializes every `WNDCLASSEXW` field that this code does not set.
    // `lpfnWndProc` points to the `'static extern "system" fn` `wndproc`.
    // The callback remains valid for the process lifetime, as the OS requires.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name(),
            hCursor: LoadCursorW(None, IDC_CROSS).context("LoadCursorW(IDC_CROSS)")?,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread()).context("RegisterClassExW");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// A modal window that lets the user select a screen region.
pub struct RegionSelection {
    hwnd: HWND,
}

impl RegionSelection {
    /// Create the selector window and leave it hidden.
    pub fn new() -> Result<Self> {
        // SAFETY: This follows `ui::window::Popup::create`.
        // `GetModuleHandleW(None)` supplies `hinstance`, which remains valid for this process.
        // `register_class` registers one valid class.
        // The `?` operator checks the `CreateWindowExW` result.
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();

            register_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class_name(),
                w!("chibipop-region-select"),
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
            .context("CreateWindowExW for the region selection overlay")?;

            Ok(RegionSelection { hwnd })
        }
    }

    /// Show the selector and block until the user selects or cancels a region.
    pub fn run(&mut self) -> Option<PhysRect> {
        ANCHOR.set(None);
        RESULT.set(None);
        DONE.set(false);

        let ctx = match build_paint_ctx() {
            Ok(ctx) => ctx,
            Err(e) => {
                eprintln!("chibipop: region selection overlay failed: {e:#}");
                return None;
            }
        };
        PAINT_CTX.with(|cell| *cell.borrow_mut() = Some(ctx));

        Hooks::set_selection_active(true);
        paint_overlay(self.hwnd, None);
        // SAFETY: `new` created `self.hwnd`, and `Drop` destroys it only after `run` returns.
        // The handle is valid for these calls.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = SetForegroundWindow(self.hwnd);
        }

        let mut msg = MSG::default();
        while !DONE.get() {
            // SAFETY: `msg` is writable stack storage owned by this loop.
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if got.0 <= 0 {
                break; // `0` means `WM_QUIT`. `-1` means an error.
            }
            // SAFETY: `GetMessageW` just filled `msg` before this block.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // SAFETY: `Drop` destroys `self.hwnd` after this method returns, so it remains valid here.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        Hooks::set_selection_active(false);
        PAINT_CTX.with(|cell| *cell.borrow_mut() = None);
        RESULT.get()
    }
}

impl RegionSelection {
    /// Return a test value without a window.
    #[cfg(test)]
    pub(crate) fn dummy() -> Self {
        RegionSelection {
            hwnd: HWND::default(),
        }
    }
}

impl Drop for RegionSelection {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: `new` created `self.hwnd`, and `Drop` runs at most once.
            // This call therefore destroys a window that this process still owns.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> PhysPoint {
        PhysPoint { x, y }
    }

    fn idx(row: i32, col: i32, vw: i32) -> usize {
        (row * vw + col) as usize
    }

    #[test]
    fn normalized_rect_is_order_independent() {
        let forward = normalized_rect(p(10, 10), p(30, 40));
        let backward = normalized_rect(p(30, 40), p(10, 10));
        assert_eq!(forward, backward);
        assert_eq!(
            PhysRect {
                x: 10,
                y: 10,
                w: 20,
                h: 30
            },
            forward
        );
    }

    #[test]
    fn meets_drag_threshold_requires_one_axis_over_the_floor() {
        assert!(!meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 4,
            h: 4
        }));
        assert!(meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 5,
            h: 0
        }));
        assert!(meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 0,
            h: 5
        }));
    }

    #[test]
    fn punch_through_clears_exactly_the_selection() {
        let (vw, vh) = (5, 5);
        let mut px = vec![7u32; (vw * vh) as usize];
        punch_through(
            &mut px,
            vw,
            vh,
            PhysRect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
        );

        assert_eq!(0, px[idx(1, 1, vw)]);
        assert_eq!(0, px[idx(2, 2, vw)]);
        assert_eq!(7, px[idx(0, 0, vw)]);
        assert_eq!(7, px[idx(1, 3, vw)], "one column past the selection");
        assert_eq!(7, px[idx(3, 1, vw)], "one row past the selection");
    }

    #[test]
    fn punch_through_clamps_without_panicking() {
        let (vw, vh) = (4, 4);
        let mut px = vec![9u32; (vw * vh) as usize];
        punch_through(
            &mut px,
            vw,
            vh,
            PhysRect {
                x: -1,
                y: -1,
                w: 3,
                h: 3,
            },
        );

        assert_eq!(0, px[idx(0, 0, vw)]);
        assert_eq!(0, px[idx(1, 1, vw)]);
        assert_eq!(9, px[idx(2, 0, vw)], "clamped out of the selection");
        assert_eq!(9, px[idx(3, 3, vw)]);
    }

    #[test]
    fn paint_border_draws_a_ring_and_leaves_the_interior() {
        let (vw, vh) = (8, 8);
        let mut px = vec![5u32; (vw * vh) as usize];
        paint_border(
            &mut px,
            vw,
            vh,
            PhysRect {
                x: 3,
                y: 3,
                w: 2,
                h: 2,
            },
        );
        let white = (0xFFu32 << 24) | 0x00FF_FFFF;

        assert_eq!(white, px[idx(1, 1, vw)], "top band");
        assert_eq!(white, px[idx(3, 1, vw)], "left band");
        assert_eq!(white, px[idx(3, 5, vw)], "right band");
        assert_eq!(white, px[idx(5, 4, vw)], "bottom band");
        assert_eq!(5, px[idx(3, 3, vw)], "the punched hole, untouched");
        assert_eq!(5, px[idx(0, 4, vw)], "outside the ring entirely");
        assert_eq!(5, px[idx(1, 0, vw)], "outside the ring's left edge");
    }

    #[test]
    fn paint_border_clamps_without_panicking() {
        let (vw, vh) = (5, 5);
        let mut px = vec![9u32; (vw * vh) as usize];
        paint_border(
            &mut px,
            vw,
            vh,
            PhysRect {
                x: 0,
                y: 2,
                w: 1,
                h: 1,
            },
        );
        let white = (0xFFu32 << 24) | 0x00FF_FFFF;

        assert_eq!(white, px[idx(0, 0, vw)], "top band, clipped left");
        assert_eq!(9, px[idx(2, 0, vw)], "middle row's empty left band");
        assert_eq!(white, px[idx(2, 1, vw)], "middle row's right band");
        assert_eq!(white, px[idx(4, 2, vw)], "bottom band, clipped left");
        assert_eq!(9, px[idx(2, 4, vw)], "past the clamped right edge");
    }
}

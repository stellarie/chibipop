//! The target selector lets the user choose a region or window on the virtual desktop.

use crate::config::{ScreenshotMode, ScreenshotWindow};
use crate::geom::{PhysPoint, PhysRect};
use crate::input::hooks::Hooks;
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Minimum width or height for a valid drag, in physical pixels.
const MIN_DRAG_PX: i32 = 5;
/// Alpha value for the dim fill, from 0 through 255.
const DIM_ALPHA: u8 = 102;
/// Border width for the selection frame, in physical pixels.
const BORDER_PX: i32 = 2;
/// Virtual-key code for `VK_ESCAPE`.
const VK_ESCAPE: usize = 0x1B;
/// Virtual-key code for `VK_MENU` (either Alt key).
const VK_MENU: i32 = 0x12;

fn class_name() -> PCWSTR {
    w!("ChibipopRegionSelect")
}

/// The result of one screenshot selection.
///
/// A window target keeps its identity and its current bounds. Fixed-window
/// captures resolve the identity again before every capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionTarget {
    Region(PhysRect),
    Window {
        rect: PhysRect,
        target: ScreenshotWindow,
    },
}

impl SelectionTarget {
    /// Return the physical rectangle to capture.
    pub fn rect(&self) -> PhysRect {
        match self {
            SelectionTarget::Region(rect) | SelectionTarget::Window { rect, .. } => *rect,
        }
    }
}

thread_local! {
    static ANCHOR: Cell<Option<PhysPoint>> = const { Cell::new(None) };
    static DONE: Cell<bool> = const { Cell::new(false) };
    static PAINT_CTX: RefCell<Option<PaintCtx>> = const { RefCell::new(None) };
    static TARGET: RefCell<Option<SelectionTarget>> = const { RefCell::new(None) };
    static MODE: Cell<ScreenshotMode> = const { Cell::new(ScreenshotMode::Region) };
    static ALLOW_TARGET_SWITCH: Cell<bool> = const { Cell::new(false) };
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

/// Return true when both dimensions are nonzero and one reaches `MIN_DRAG_PX`.
fn meets_drag_threshold(r: PhysRect) -> bool {
    r.w > 0 && r.h > 0 && (r.w >= MIN_DRAG_PX || r.h >= MIN_DRAG_PX)
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

fn window_text(hwnd: HWND) -> String {
    // SAFETY: `GetWindowTextLengthW` and `GetWindowTextW` use the same live
    // top-level handle. The vector has one extra slot for the terminating NUL.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn window_class(hwnd: HWND) -> String {
    // SAFETY: The fixed buffer prevents `GetClassNameW` from writing past its
    // end. The function returns the character count without the NUL.
    unsafe {
        let mut buf = [0u16; 256];
        let got = GetClassNameW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn window_rect(hwnd: HWND) -> Option<PhysRect> {
    // SAFETY: `rect` is local writable storage and `hwnd` comes from EnumWindows.
    unsafe {
        let mut rect = RECT::default();
        // DWM bounds omit invisible resize borders. Some windows do not expose
        // them, so use the normal bounds when DWM cannot provide the frame.
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            GetWindowRect(hwnd, &mut rect).ok()?;
        }
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        (w > 0 && h > 0).then_some(PhysRect {
            x: rect.left,
            y: rect.top,
            w,
            h,
        })
    }
}

fn window_is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is writable storage of the size requested by DWM.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

enum WindowSearchKind {
    At(PhysPoint),
    Exact(ScreenshotWindow),
}

struct WindowSearch {
    kind: WindowSearchKind,
    process_id: u32,
    result: Option<SelectionTarget>,
    matches: usize,
}

/// Search visible top-level windows in z-order.
///
/// `EnumWindows` starts with the topmost window, so an at-point search can stop
/// at its first match. An exact fixed-target search stops after two matches.
unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` points to the `WindowSearch` supplied for this call.
    let search = unsafe { &mut *(lparam.0 as *mut WindowSearch) };
    unsafe {
        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id as *mut u32));
        if process_id == search.process_id
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
            || window_is_cloaked(hwnd)
            || (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) != 0
        {
            return BOOL(1);
        }
    }
    let Some(rect) = window_rect(hwnd) else {
        return BOOL(1);
    };
    let class = window_class(hwnd);
    let title = window_text(hwnd);
    match &search.kind {
        WindowSearchKind::At(point) if rect.contains(*point) => {
            search.result = Some(SelectionTarget::Window {
                rect,
                target: ScreenshotWindow { app_id: class, title },
            });
            BOOL(0)
        }
        WindowSearchKind::Exact(target) if target.app_id == class && target.title == title => {
            search.matches += 1;
            if search.matches == 1 {
                search.result = Some(SelectionTarget::Window {
                    rect,
                    target: target.clone(),
                });
            }
            (search.matches < 2).then_some(BOOL(1)).unwrap_or(BOOL(0))
        }
        _ => BOOL(1),
    }
}

fn find_window_at(point: PhysPoint) -> Option<SelectionTarget> {
    let mut search = WindowSearch {
        kind: WindowSearchKind::At(point),
        // Exclude every chibipop surface, not only the selector itself.
        process_id: unsafe { GetCurrentProcessId() },
        result: None,
        matches: 0,
    };
    // SAFETY: The callback only runs during this call, and `search` stays live
    // until `EnumWindows` returns.
    unsafe {
        let _ = EnumWindows(
            Some(enum_window_proc),
            LPARAM(&mut search as *mut WindowSearch as isize),
        );
    }
    search.result
}

/// Resolve a saved window identity to its current physical bounds.
///
/// The class and title must identify one visible top-level window. Missing and
/// ambiguous identities return errors instead of selecting a different window.
pub fn resolve_window(target: &ScreenshotWindow) -> Result<PhysRect> {
    let mut search = WindowSearch {
        kind: WindowSearchKind::Exact(target.clone()),
        process_id: unsafe { GetCurrentProcessId() },
        result: None,
        matches: 0,
    };
    // SAFETY: The callback only runs during this call, and `search` stays live
    // until `EnumWindows` returns.
    let enumerated = unsafe {
        EnumWindows(
            Some(enum_window_proc),
            LPARAM(&mut search as *mut WindowSearch as isize),
        )
    };
    if search.matches < 2 {
        enumerated.context("enumerating screenshot windows")?;
    }
    match search.matches {
        0 => anyhow::bail!(
            "saved screenshot window is not visible: class {:?}, title {:?}",
            target.app_id,
            target.title
        ),
        1 => search
            .result
            .map(|selection| selection.rect())
            .context("saved screenshot window has no bounds"),
        _ => anyhow::bail!(
            "saved screenshot window is ambiguous: class {:?}, title {:?}",
            target.app_id,
            target.title
        ),
    }
}

fn alt_down() -> bool {
    // SAFETY: `GetAsyncKeyState` reads the current state of the virtual key.
    unsafe { (GetAsyncKeyState(VK_MENU) as u16 & 0x8000) != 0 }
}

fn window_mode(mode: ScreenshotMode) -> bool {
    matches!(mode, ScreenshotMode::Window | ScreenshotMode::FixedWindow)
}
fn use_window_for_selection() -> bool {
    ALLOW_TARGET_SWITCH.with(|allow| {
        if !allow.get() {
            return false;
        }
        let mode = MODE.get();
        window_mode(mode) ^ alt_down()
    })
}

fn on_lbuttondown(hwnd: HWND) {
    if use_window_for_selection() {
        // Capture the button-up message too. This prevents the click that
        // chooses a window from reaching the selected application.
        // SAFETY: `hwnd` identifies the live selector window.
        unsafe {
            SetCapture(hwnd);
        }
        TARGET.with(|cell| *cell.borrow_mut() = find_window_at(cursor_point()));
        // Keep the selector captured until button-up commits the target.
        return;
    }
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

/// Commit the drag when both dimensions are nonzero and one reaches the threshold.
fn on_lbuttonup() {
    if let Some(anchor) = ANCHOR.get() {
        let r = normalized_rect(anchor, cursor_point());
        if meets_drag_threshold(r) {
            TARGET.with(|cell| *cell.borrow_mut() = Some(SelectionTarget::Region(r)));
        }
    }
    ANCHOR.set(None);
    // Finish while the selector still owns the button-up message.
    DONE.set(true);
    // SAFETY: `ReleaseCapture` has no preconditions.
    let _ = unsafe { ReleaseCapture() };
}

fn on_cancel() {
    ANCHOR.set(None);
    TARGET.with(|cell| *cell.borrow_mut() = None);
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
    ///
    /// This legacy entry point remains region-only. OCR and static-region
    /// selectors must not change behavior when Alt is held.
    pub fn run(&mut self) -> Option<PhysRect> {
        self.run_target_inner(ScreenshotMode::Region, false)
            .and_then(|target| match target {
                SelectionTarget::Region(rect) => Some(rect),
                SelectionTarget::Window { .. } => None,
            })
    }

    /// Show the selector and return a region or a clicked top-level window.
    ///
    /// Region mode starts a drag. Window mode selects the top-level window
    /// under the first click. Alt switches these two interactions.
    pub fn run_target(&mut self, mode: ScreenshotMode) -> Option<SelectionTarget> {
        self.run_target_inner(mode, true)
    }

    fn run_target_inner(
        &mut self,
        mode: ScreenshotMode,
        allow_target_switch: bool,
    ) -> Option<SelectionTarget> {
        ANCHOR.set(None);
        TARGET.with(|cell| *cell.borrow_mut() = None);
        MODE.set(mode);
        ALLOW_TARGET_SWITCH.with(|cell| cell.set(allow_target_switch));
        DONE.set(false);

        let ctx = match build_paint_ctx() {
            Ok(ctx) => ctx,
            Err(e) => {
                eprintln!("chibipop: region selection overlay failed: {e:#}");
                ALLOW_TARGET_SWITCH.with(|cell| cell.set(false));
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

        // SAFETY: `ReleaseCapture` has no preconditions. This also releases
        // the capture used to swallow a window-selection button-up message.
        unsafe {
            let _ = ReleaseCapture();
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        Hooks::set_selection_active(false);
        PAINT_CTX.with(|cell| *cell.borrow_mut() = None);
        ALLOW_TARGET_SWITCH.with(|cell| cell.set(false));
        TARGET.with(|cell| cell.borrow_mut().take())
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
    struct NativeWindowGuard(HWND);

    impl Drop for NativeWindowGuard {
        fn drop(&mut self) {
            // SAFETY: The guard owns the window created by the test.
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }


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
    fn meets_drag_threshold_rejects_zero_dimension_drags() {
        assert!(!meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 4,
            h: 4
        }));
        assert!(!meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 5,
            h: 0
        }));
        assert!(!meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 0,
            h: 5
        }));
        assert!(meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 5,
            h: 1
        }));
        assert!(meets_drag_threshold(PhysRect {
            x: 0,
            y: 0,
            w: 1,
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
    #[test]
    fn resolve_window_rejects_a_window_owned_by_this_process() {
        let title = format!(
            "chibipop-pid-regression-{}",
            // SAFETY: This call has no preconditions.
            unsafe { GetCurrentProcessId() }
        );
        let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = unsafe {
            // SAFETY: The built-in STATIC class accepts these window arguments.
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR(title_w.as_ptr()),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                16,
                16,
                None,
                None,
                None,
                None,
            )
        }
        .expect("create the native regression window");
        let _guard = NativeWindowGuard(hwnd);
        assert!(unsafe { IsWindowVisible(hwnd).as_bool() });
        assert_ne!(
            unsafe { GetWindowThreadProcessId(hwnd, None) },
            unsafe { GetCurrentProcessId() },
            "the test must distinguish the window thread ID from the process ID"
        );

        let target = ScreenshotWindow {
            app_id: window_class(hwnd),
            title,
        };
        assert!(
            resolve_window(&target).is_err(),
            "the selector must reject windows owned by this process"
        );
    }

}

//! Screen capture. Win32.

use crate::geom::{PhysPoint, PhysRect};
use crate::text::{Frame, RegionCapture};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use windows::core::{Interface, HRESULT};
use windows::Win32::Foundation::{HMODULE, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_IDENTITY,
    DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};


/// First; else DPI-scaled.
pub fn init_dpi_awareness() -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .context("setting per-monitor DPI awareness")?;
    }
    Ok(())
}

/// Wake the pump. +2 is tray's.
pub const WM_APP_CAPTURE_GUARD: u32 = WM_APP + 3;

/// Hide-ack wait, then capture.
const ACK_TIMEOUT: Duration = Duration::from_millis(250);

/// Popup out of one capture.
pub enum CaptureGuardMsg {
    /// Hide now; ack when done.
    Hide { ack: mpsc::Sender<()> },
    /// Undo a Hide. Fire-and-forget.
    Restore,
}

/// The worker's guard handle.
pub struct CaptureGuard {
    /// Recomputed by `apply_live`.
    pub active: Arc<AtomicBool>,
    pub main_tid: u32,
    pub request_tx: mpsc::Sender<CaptureGuardMsg>,
}

impl CaptureGuard {
    /// Blocks until hidden.
    fn hide_for_capture(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.request_tx.send(CaptureGuardMsg::Hide { ack: ack_tx }).is_err() {
            return; // main thread gone - nothing left to hide.
        }
        self.wake_main_thread();
        if ack_rx.recv_timeout(ACK_TIMEOUT).is_err() {
            eprintln!(
                "chibipop: capture guard: hide was not acknowledged within {ACK_TIMEOUT:?}; \
                 capturing anyway - this capture may include the popup itself"
            );
        }
    }

    /// Undoes `hide_for_capture`.
    fn restore_after_capture(&self) {
        let _ = self.request_tx.send(CaptureGuardMsg::Restore);
        self.wake_main_thread();
    }

    /// Thread message, not window.
    fn wake_main_thread(&self) {
        unsafe {
            let _ = PostThreadMessageW(self.main_tid, WM_APP_CAPTURE_GUARD, WPARAM(0), LPARAM(0));
        }
    }
}

/// The Windows capture backend: DXGI first, BitBlt fallback.
pub struct WinCapture {
    guard: Option<CaptureGuard>,
    /// `begin_read` hid; `end_read` reshows.
    hiding: bool,
}

impl WinCapture {
    /// DPI awareness before any GDI.
    pub fn new(guard: Option<CaptureGuard>) -> Result<Self> {
        init_dpi_awareness()?;
        Ok(WinCapture { guard, hiding: false })
    }
}

impl RegionCapture for WinCapture {
    fn grab(&mut self, region: PhysRect) -> Result<Frame> {
        capture_region(region)
    }

    fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
        monitor_bounds_containing(p)
    }

    // Fresh check per read, so no ordering rule.
    fn begin_read(&mut self) {
        let Some(guard) = &self.guard else { return };
        if guard.active.load(Ordering::SeqCst) {
            guard.hide_for_capture();
            self.hiding = true;
        }
    }

    fn end_read(&mut self) {
        if self.hiding {
            if let Some(guard) = &self.guard {
                guard.restore_after_capture();
            }
            self.hiding = false;
        }
    }
}

/// The monitor holding `p`.
fn monitor_bounds_containing(p: PhysPoint) -> PhysRect {
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: p.x, y: p.y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let rc = mi.rcMonitor;
            PhysRect { x: rc.left, y: rc.top, w: rc.right - rc.left, h: rc.bottom - rc.top }
        } else {
            PhysRect { x: p.x - 960, y: p.y - 540, w: 1920, h: 1080 }
        }
    }
}

// -- DXGI Desktop Duplication -----------------------------------------

/// DXGI_ERROR_ACCESS_LOST.
const ACCESS_LOST: HRESULT = HRESULT(0x887A0026u32 as i32);

/// DXGI_ERROR_WAIT_TIMEOUT.
const WAIT_TIMEOUT: HRESULT = HRESULT(0x887A0027u32 as i32);

/// Cold wait for a first frame.
///
/// A presenting source yields one inside a
/// frame time; a still desktop never will,
/// so this is a ceiling, not a target.
const COLD_BUDGET: Duration = Duration::from_millis(50);

/// Warm: the kept frame is current.
const WARM_ACQUIRE_MS: u32 = 4;

/// Warn about DXGI once only.
static DXGI_WARNED: AtomicBool = AtomicBool::new(false);

/// Cached DXGI resources.
struct DxgiState {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    mon: RECT,
    /// Newest real desktop frame.
    last: Option<ID3D11Texture2D>,
    tex_w: u32,
    tex_h: u32,
}

/// Is `r` wholly inside `mon`?
fn rect_inside(mon: &RECT, r: &PhysRect) -> bool {
    r.x >= mon.left && r.y >= mon.top && r.x + r.w <= mon.right && r.y + r.h <= mon.bottom
}

/// Rotated outputs need remapping.
fn is_rotated(rot: DXGI_MODE_ROTATION) -> bool {
    rot != DXGI_MODE_ROTATION_IDENTITY && rot != DXGI_MODE_ROTATION_UNSPECIFIED
}

/// Every pixel bit-identical?
fn is_uniform(buf: &[u8]) -> bool {
    let (px, _) = buf.as_chunks::<4>();
    let Some(first) = px.first() else { return true };
    px.iter().all(|p| p == first)
}

thread_local! {
    /// Per-thread DXGI cache.
    static DXGI: RefCell<Option<DxgiState>> = const { RefCell::new(None) };
}

/// DXGI first, BitBlt fallback.
fn capture_region(region: PhysRect) -> Result<Frame> {
    match capture_dxgi(region) {
        // A flat frame is never real text.
        Ok(buf) if !is_uniform(&buf) => Ok(Frame {
            buf,
            w: region.w,
            h: region.h,
            source: "dxgi",
            fallback: None,
            // DXGI's AcquireNextFrame does report "no new frame", but
            // this backend never holds a previous one to compare with.
            unchanged: false,
        }),
        Ok(_) => bitblt_after(region, "DXGI frame was one flat colour".to_string()),
        Err(e) => bitblt_after(region, format!("{e:#}")),
    }
}

/// BitBlt, remembering DXGI's reason.
fn bitblt_after(region: PhysRect, why: String) -> Result<Frame> {
    if !DXGI_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("chibipop: DXGI capture unavailable ({why}); using BitBlt");
    }
    let buf = capture_bitblt(region)
        .with_context(|| format!("DXGI failed ({why}); BitBlt also failed"))?;
    Ok(Frame {
        buf,
        w: region.w,
        h: region.h,
        source: "bitblt",
        fallback: Some(why),
        unchanged: false,
    })
}

/// Acquire + copy subregion.
fn capture_dxgi(region: PhysRect) -> Result<Vec<u8>> {
    let (w, h) = (region.w, region.h);
    if w <= 0 || h <= 0 {
        anyhow::bail!("non-positive extent: {w}x{h}");
    }
    DXGI.with(|cell| {
        let mut slot = cell.borrow_mut();

        let need = !slot.as_ref().is_some_and(|s| rect_inside(&s.mon, &region));
        if need {
            *slot = Some(init_dxgi(&region)?);
        }

        let st = slot.as_mut().unwrap();
        match refresh_and_copy(st, &region) {
            Ok(buf) => Ok(buf),
            Err(e) => {
                let lost = e
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|we| we.code() == ACCESS_LOST);
                if lost {
                    *slot = Some(init_dxgi(&region)?);
                    refresh_and_copy(slot.as_mut().unwrap(), &region)
                } else {
                    Err(e)
                }
            }
        }
    })
}

/// Wait for a live frame, then crop.
fn refresh_and_copy(st: &mut DxgiState, region: &PhysRect) -> Result<Vec<u8>> {
    refresh_desktop(st)?;
    copy_out(st, region)
}

/// Loop until the desktop image is real.
///
/// A pointer-only update leaves the surface
/// undefined; on a fresh duplication it is
/// blank. `LastPresentTime` is the tell.
///
/// Once a frame is kept, "nothing new" means
/// the kept one is still accurate, so warm
/// calls must not wait out the cold budget.
fn refresh_desktop(st: &mut DxgiState) -> Result<()> {
    let warm = st.last.is_some();
    let deadline = Instant::now() + COLD_BUDGET;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let wait = if warm {
            WARM_ACQUIRE_MS
        } else {
            left.as_millis() as u32
        };
        let mut fi = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: `fi` and `res` are live locals; the
        // frame is released on every path below before
        // the next acquire, as the API requires.
        let got = unsafe { st.dup.AcquireNextFrame(wait, &mut fi, &mut res) };
        match got {
            Ok(()) => {
                let mut fresh = false;
                let mut failed = None;
                if fi.LastPresentTime != 0 {
                    match res.as_ref().context("null resource") {
                        Ok(r) => match store_desktop(st, r) {
                            Ok(()) => fresh = true,
                            Err(e) => failed = Some(e),
                        },
                        Err(e) => failed = Some(e),
                    }
                }
                // SAFETY: pairs with the acquire above.
                unsafe {
                    let _ = st.dup.ReleaseFrame();
                }
                if let Some(e) = failed {
                    return Err(e);
                }
                if fresh {
                    return Ok(());
                }
            }
            Err(e) if e.code() == WAIT_TIMEOUT => {}
            Err(e) => return Err(e.into()),
        }
        if warm || Instant::now() >= deadline {
            break;
        }
    }
    if st.last.is_some() {
        Ok(())
    } else {
        anyhow::bail!("no live desktop frame within {COLD_BUDGET:?}")
    }
}

/// Keep this frame for later crops.
fn store_desktop(st: &mut DxgiState, res: &IDXGIResource) -> Result<()> {
    let tex: ID3D11Texture2D = res.cast()?;
    let mut d = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `tex` is a live texture from DXGI.
    unsafe { tex.GetDesc(&mut d) };
    if d.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        anyhow::bail!("desktop format {} is not BGRA8", d.Format.0);
    }

    if st.last.is_none() || (st.tex_w, st.tex_h) != (d.Width, d.Height) {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: d.Width,
            Height: d.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: d.Format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            ..Default::default()
        };
        let mut keep = None;
        // SAFETY: `desc` is fully initialised above.
        unsafe { st.dev.CreateTexture2D(&desc, None, Some(&mut keep))? };
        st.last = Some(keep.context("no retained texture")?);
        st.tex_w = d.Width;
        st.tex_h = d.Height;
    }

    let keep = st.last.as_ref().context("no retained texture")?;
    // SAFETY: both textures share `dev`, have the
    // same desc, and neither is mapped here.
    unsafe { st.ctx.CopyResource(keep, &tex) };
    Ok(())
}

/// Crop the retained frame to `region`.
fn copy_out(st: &DxgiState, region: &PhysRect) -> Result<Vec<u8>> {
    let (w, h) = (region.w, region.h);
    let len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .context("region too large")?;
    let src = st.last.as_ref().context("no retained desktop frame")?;

    if region.x < st.mon.left || region.y < st.mon.top {
        anyhow::bail!("region starts outside its monitor");
    }
    let sx = (region.x - st.mon.left) as u32;
    let sy = (region.y - st.mon.top) as u32;
    let (right, bottom) = (sx + w as u32, sy + h as u32);
    if right > st.tex_w || bottom > st.tex_h {
        anyhow::bail!(
            "crop {sx},{sy}+{w}x{h} exceeds {}x{}",
            st.tex_w,
            st.tex_h
        );
    }

    unsafe {
        let stg_desc = D3D11_TEXTURE2D_DESC {
            Width: w as u32,
            Height: h as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging = None;
        // SAFETY: `stg_desc` is fully initialised.
        st.dev.CreateTexture2D(&stg_desc, None, Some(&mut staging))?;
        let staging: ID3D11Texture2D = staging.context("no staging texture")?;

        let src_box = D3D11_BOX {
            left: sx,
            top: sy,
            front: 0,
            right,
            bottom,
            back: 1,
        };
        // SAFETY: the box is bounds-checked above.
        st.ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, src, 0, Some(&src_box));

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        st.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

        let mut buf = vec![0u8; len];
        let src_pitch = mapped.RowPitch as usize;
        let dst_pitch = w as usize * 4;
        for row in 0..h as usize {
            // SAFETY: mapped memory is valid for
            // Height rows of RowPitch bytes each;
            // we read dst_pitch <= RowPitch per row.
            let src = std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(row * src_pitch),
                dst_pitch,
            );
            let off = row * dst_pitch;
            buf[off..off + dst_pitch].copy_from_slice(src);
        }

        st.ctx.Unmap(&staging, 0);
        Ok(buf)
    }
}

/// Find output, create device.
fn init_dxgi(region: &PhysRect) -> Result<DxgiState> {
    unsafe {
        let fac: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut ai = 0u32;
        loop {
            let adp = match fac.EnumAdapters1(ai) {
                Ok(a) => a,
                Err(_) => break,
            };
            let mut oi = 0u32;
            loop {
                let out = match adp.EnumOutputs(oi) {
                    Ok(o) => o,
                    Err(_) => break,
                };
                let desc = out.GetDesc()?;
                let r = desc.DesktopCoordinates;
                if region.x >= r.left
                    && region.x < r.right
                    && region.y >= r.top
                    && region.y < r.bottom
                {
                    // Bail before making a device.
                    if !rect_inside(&r, region) {
                        anyhow::bail!("region spans monitors");
                    }
                    if is_rotated(desc.Rotation) {
                        anyhow::bail!(
                            "output is rotated ({}); DXGI cannot map it",
                            desc.Rotation.0
                        );
                    }
                    let o1: IDXGIOutput1 = out.cast()?;
                    let mut dev = None;
                    let mut ctx = None;
                    D3D11CreateDevice(
                        &adp,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        HMODULE::default(),
                        Default::default(),
                        Some(&[D3D_FEATURE_LEVEL_11_0]),
                        D3D11_SDK_VERSION,
                        Some(&mut dev),
                        None,
                        Some(&mut ctx),
                    )?;
                    let dev = dev.context("no D3D11 device")?;
                    let ctx = ctx.context("no D3D11 context")?;
                    let dup = o1.DuplicateOutput(&dev)?;
                    return Ok(DxgiState {
                        dev,
                        ctx,
                        dup,
                        mon: r,
                        last: None,
                        tex_w: 0,
                        tex_h: 0,
                    });
                }
                oi += 1;
            }
            ai += 1;
        }
        anyhow::bail!("no DXGI output for region");
    }
}

// -- BitBlt fallback --------------------------------------------------

/// Releases the screen DC.
struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe { ReleaseDC(None, self.0) };
    }
}

/// Deletes the memory DC.
struct MemDc(HDC);

impl Drop for MemDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// Deletes the bitmap.
struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

/// Reselects the old object.
struct Selection {
    dc: HDC,
    prev: HGDIOBJ,
}

impl Drop for Selection {
    fn drop(&mut self) {
        unsafe { SelectObject(self.dc, self.prev) };
    }
}

/// GDI BitBlt capture path.
fn capture_bitblt(region: PhysRect) -> Result<Vec<u8>> {
    let (w, h) = (region.w, region.h);
    if w <= 0 || h <= 0 {
        anyhow::bail!("capture region has a non-positive extent: {w}x{h}");
    }
    let len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .context("capture region is too large to allocate")?;

    unsafe {
        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            anyhow::bail!("GetDC(None) returned a null device context");
        }
        let mem = MemDc(CreateCompatibleDC(Some(screen.0)));
        if mem.0.is_invalid() {
            anyhow::bail!("CreateCompatibleDC returned a null device context");
        }
        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, w, h));
        if bitmap.0.is_invalid() {
            anyhow::bail!("CreateCompatibleBitmap({w}, {h}) returned a null bitmap");
        }
        let sel = Selection { dc: mem.0, prev: SelectObject(mem.0, bitmap.0.into()) };

        BitBlt(mem.0, 0, 0, w, h, Some(screen.0), region.x, region.y, SRCCOPY)
            .context("BitBlt of the screen region")?;

        // GetDIBits wants it deselected.
        drop(sel);

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let mut buf = vec![0u8; len];
        let scanlines = GetDIBits(
            mem.0, bitmap.0, 0, h as u32,
            Some(buf.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS,
        );
        if scanlines < h {
            anyhow::bail!("GetDIBits copied {scanlines} of {h} scanlines");
        }
        Ok(buf)
    }
}


/// The cursor's position.
pub fn cursor_position() -> Result<PhysPoint> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).context("GetCursorPos")? };
    Ok(PhysPoint { x: p.x, y: p.y })
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn an_all_black_frame_is_uniform() {
        assert!(is_uniform(&[0u8; 16]));
    }

    #[test]
    fn an_all_white_frame_is_uniform() {
        assert!(is_uniform(&[0xFFu8; 16]));
    }

    /// A dark scene is not a dead frame.
    #[test]
    fn one_differing_pixel_is_not_uniform() {
        let mut buf = [0u8; 16];
        buf[9] = 1;
        assert!(!is_uniform(&buf));
    }

    #[test]
    fn an_empty_buffer_counts_as_uniform() {
        assert!(is_uniform(&[]));
    }

    #[test]
    fn a_single_pixel_is_uniform() {
        assert!(is_uniform(&[3, 4, 5, 255]));
    }

    #[test]
    fn identity_and_unspecified_are_not_rotated() {
        assert!(!is_rotated(DXGI_MODE_ROTATION_IDENTITY));
        assert!(!is_rotated(DXGI_MODE_ROTATION_UNSPECIFIED));
    }

    #[test]
    fn ninety_degrees_is_rotated() {
        use windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION_ROTATE90;
        assert!(is_rotated(DXGI_MODE_ROTATION_ROTATE90));
    }

    fn mon() -> RECT {
        RECT { left: 2560, top: 0, right: 3640, bottom: 1920 }
    }

    #[test]
    fn a_contained_region_is_inside() {
        let r = PhysRect { x: 2600, y: 155, w: 500, h: 100 };
        assert!(rect_inside(&mon(), &r));
    }

    /// The seam case that hid this bug.
    #[test]
    fn a_region_crossing_the_seam_is_not_inside() {
        let r = PhysRect { x: 2445, y: 155, w: 500, h: 100 };
        assert!(!rect_inside(&mon(), &r));
    }

    #[test]
    fn a_region_past_the_right_edge_is_not_inside() {
        let r = PhysRect { x: 3400, y: 155, w: 500, h: 100 };
        assert!(!rect_inside(&mon(), &r));
    }

    #[test]
    fn a_region_flush_with_the_edges_is_inside() {
        let r = PhysRect { x: 2560, y: 0, w: 1080, h: 1920 };
        assert!(rect_inside(&mon(), &r));
    }
}

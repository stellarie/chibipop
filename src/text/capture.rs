//! Screen capture. Win32.

use crate::geom::{PhysPoint, PhysRect};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use windows::core::{Interface, HRESULT};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// Small text else misreads.
pub const UPSCALE: i32 = 2;

/// First; else DPI-scaled.
pub fn init_dpi_awareness() -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .context("setting per-monitor DPI awareness")?;
    }
    Ok(())
}

/// Capture + upscale; BGRA.
pub fn capture_upscaled(region: PhysRect) -> Result<(Vec<u8>, i32, i32)> {
    let raw = capture_region(region)?;
    Ok(upscale(&raw, region.w, region.h))
}

// -- DXGI Desktop Duplication -----------------------------------------

/// DXGI_ERROR_ACCESS_LOST.
const ACCESS_LOST: HRESULT = HRESULT(0x887A0026u32 as i32);

/// Cached DXGI resources.
struct DxgiState {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    mon: RECT,
}

thread_local! {
    /// Per-thread DXGI cache.
    static DXGI: RefCell<Option<DxgiState>> = const { RefCell::new(None) };
}

/// Releases frame on drop.
struct FrameGuard<'a>(&'a IDXGIOutputDuplication);

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.ReleaseFrame();
        }
    }
}

/// DXGI first, BitBlt fallback.
fn capture_region(region: PhysRect) -> Result<Vec<u8>> {
    capture_dxgi(region).or_else(|_| capture_bitblt(region))
}

/// Acquire + copy subregion.
fn capture_dxgi(region: PhysRect) -> Result<Vec<u8>> {
    let (w, h) = (region.w, region.h);
    if w <= 0 || h <= 0 {
        anyhow::bail!("non-positive extent: {w}x{h}");
    }
    DXGI.with(|cell| {
        let mut slot = cell.borrow_mut();

        let need = match &*slot {
            None => true,
            Some(s) => {
                region.x < s.mon.left
                    || region.x >= s.mon.right
                    || region.y < s.mon.top
                    || region.y >= s.mon.bottom
            }
        };
        if need {
            *slot = Some(init_dxgi(&region)?);
        }

        let st = slot.as_ref().unwrap();
        if region.x + w > st.mon.right || region.y + h > st.mon.bottom {
            anyhow::bail!("region spans monitors");
        }

        match acquire_copy(st, &region) {
            Ok(buf) => Ok(buf),
            Err(e) => {
                let lost = e
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|we| we.code() == ACCESS_LOST);
                if lost {
                    *slot = Some(init_dxgi(&region)?);
                    let st = slot.as_ref().unwrap();
                    acquire_copy(st, &region)
                } else {
                    Err(e)
                }
            }
        }
    })
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
                    return Ok(DxgiState { dev, ctx, dup, mon: r });
                }
                oi += 1;
            }
            ai += 1;
        }
        anyhow::bail!("no DXGI output for region");
    }
}

/// Acquire frame, copy pixels.
fn acquire_copy(st: &DxgiState, region: &PhysRect) -> Result<Vec<u8>> {
    let (w, h) = (region.w, region.h);
    let len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .context("region too large")?;

    unsafe {
        let mut fi = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        st.dup.AcquireNextFrame(100, &mut fi, &mut res)?;
        let _guard = FrameGuard(&st.dup);
        let resource = res.context("null resource")?;

        let tex: ID3D11Texture2D = resource.cast()?;
        let mut src_desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut src_desc);

        let stg_desc = D3D11_TEXTURE2D_DESC {
            Width: w as u32,
            Height: h as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: src_desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging = None;
        st.dev.CreateTexture2D(&stg_desc, None, Some(&mut staging))?;
        let staging: ID3D11Texture2D = staging.context("no staging texture")?;

        let sx = (region.x - st.mon.left) as u32;
        let sy = (region.y - st.mon.top) as u32;
        let src_box = D3D11_BOX {
            left: sx,
            top: sy,
            front: 0,
            right: sx + w as u32,
            bottom: sy + h as u32,
            back: 1,
        };
        st.ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, &tex, 0, Some(&src_box));

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
        let _sel = Selection { dc: mem.0, prev: SelectObject(mem.0, bitmap.0.into()) };

        BitBlt(mem.0, 0, 0, w, h, Some(screen.0), region.x, region.y, SRCCOPY)
            .context("BitBlt of the screen region")?;

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
        if scanlines == 0 {
            anyhow::bail!("GetDIBits copied no scanlines - capture was empty");
        }
        Ok(buf)
    }
}

/// Nearest-neighbour upscale.
fn upscale(src: &[u8], w: i32, h: i32) -> (Vec<u8>, i32, i32) {
    let (w2, h2) = (w * UPSCALE, h * UPSCALE);
    let mut dst = vec![0u8; (w2 as usize) * (h2 as usize) * 4];
    for y in 0..h2 as usize {
        let sy = y / UPSCALE as usize;
        for x in 0..w2 as usize {
            let sx = x / UPSCALE as usize;
            let si = (sy * w as usize + sx) * 4;
            let di = (y * w2 as usize + x) * 4;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
            dst[di + 3] = 0xFF;
        }
    }
    (dst, w2, h2)
}

/// The cursor's position.
pub fn cursor_position() -> Result<PhysPoint> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).context("GetCursorPos")? };
    Ok(PhysPoint { x: p.x, y: p.y })
}

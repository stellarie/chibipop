//! Screen capture. Win32.

use crate::geom::{PhysPoint, PhysRect};
use anyhow::{Context, Result};
use std::ffi::c_void;
use std::mem::size_of;
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

/// Guards free every handle.
fn capture_region(region: PhysRect) -> Result<Vec<u8>> {
    let (w, h) = (region.w, region.h);
    if w <= 0 || h <= 0 {
        anyhow::bail!("capture region has a non-positive extent: {w}x{h}");
    }
    let len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .context("capture region is too large to allocate")?;

    unsafe {
        let screen = ScreenDc(GetDC(None)); // NULL hwnd => the whole virtual screen
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
        bmi.bmiHeader.biHeight = -h; // negative => top-down rows
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

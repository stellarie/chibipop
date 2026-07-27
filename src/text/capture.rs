//! Screen capture. Windows-only; the rest of `text/` stays platform-free.

use crate::geom::{PhysPoint, PhysRect};
use anyhow::{Context, Result};
use std::ffi::c_void;
use std::mem::size_of;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// How much the captured region is enlarged before OCR. Small on-screen text
/// otherwise degrades into plausible-but-wrong characters — the worst failure
/// mode for a dictionary, because it silently looks up a different word.
pub const UPSCALE: i32 = 2;

/// Must be called before any other GDI call.
///
/// Without it, BitBlt silently returns DPI-virtualized pixels on any display
/// above 100% scale: no error, just wrong pixels at wrong coordinates.
pub fn init_dpi_awareness() -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .context("setting per-monitor DPI awareness")?;
    }
    Ok(())
}

/// BitBlt `region` off the virtual desktop, upscale it, and return the buffer
/// with its upscaled dimensions. The buffer is tightly packed BGRA with alpha
/// forced to 0xFF — GDI leaves that byte as garbage.
pub fn capture_upscaled(region: PhysRect) -> Result<(Vec<u8>, i32, i32)> {
    let raw = capture_region(region)?;
    Ok(upscale(&raw, region.w, region.h))
}

fn capture_region(region: PhysRect) -> Result<Vec<u8>> {
    unsafe {
        let screen_dc = GetDC(None); // NULL hwnd => the whole virtual screen
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bitmap = CreateCompatibleBitmap(screen_dc, region.w, region.h);
        let old = SelectObject(mem_dc, bitmap.into());

        let blt = BitBlt(
            mem_dc, 0, 0, region.w, region.h,
            Some(screen_dc), region.x, region.y, SRCCOPY,
        );

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = region.w;
        bmi.bmiHeader.biHeight = -region.h; // negative => top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let mut buf = vec![0u8; (region.w as usize) * (region.h as usize) * 4];
        let scanlines = GetDIBits(
            mem_dc, bitmap, 0, region.h as u32,
            Some(buf.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        blt.context("BitBlt of the screen region")?;
        if scanlines == 0 {
            anyhow::bail!("GetDIBits copied no scanlines - capture was empty");
        }
        Ok(buf)
    }
}

/// Nearest-neighbour upscale. Forces alpha to 0xFF: GDI does not populate it,
/// and a zero alpha byte read as meaningful would make the whole capture
/// transparent.
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

/// The cursor's current position, in virtual-desktop physical pixels.
pub fn cursor_position() -> Result<PhysPoint> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).context("GetCursorPos")? };
    Ok(PhysPoint { x: p.x, y: p.y })
}

//! Windows clipboard writes.

use anyhow::{Context, Result};
use std::mem::size_of;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Owns an open clipboard.
struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

struct GlobalMemory(Option<HGLOBAL>);

impl Drop for GlobalMemory {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            unsafe {
                let _ = GlobalFree(Some(handle));
            }
        }
    }
}

/// Copies UTF-16 text to the Windows clipboard.
pub fn set_text(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide
        .len()
        .checked_mul(size_of::<u16>())
        .context("clipboard text is too large")?;
    let handle =
        unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }.context("allocating clipboard text")?;
    let mut memory = GlobalMemory(Some(handle));
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        anyhow::bail!("locking clipboard text");
    }
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), ptr.cast::<u8>(), bytes);
    }
    let _ = unsafe { GlobalUnlock(handle) };

    if let Err(error) = unsafe { OpenClipboard(None) } {
        return Err(error).context("opening clipboard");
    }
    let _clipboard = ClipboardGuard;
    unsafe { EmptyClipboard() }.context("emptying clipboard")?;
    unsafe { SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(handle.0))) }
        .context("setting clipboard text")?;
    memory.0 = None;
    Ok(())
}

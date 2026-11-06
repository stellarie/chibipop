//! Installed font families that can show Japanese text.

use crate::ipc::{ErrorKind, IpcError};
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Graphics::Gdi::{
    EnumFontFamiliesExW, GetDC, ReleaseDC, ENUMLOGFONTEXW, LOGFONTW, SHIFTJIS_CHARSET,
    TEXTMETRICW,
};

/// Lists fonts that can display Japanese kana glyphs.
///
/// Glyph coverage is not guaranteed.
/// A font name that starts with `@` represents a vertical layout variant.
pub fn japanese_font_families() -> Result<Vec<String>, IpcError> {
    let mut out: Vec<String> = Vec::new();
    // SAFETY: `lf` and `out` are local storage that outlives the call.
    // `EnumFontFamiliesExW` calls the callback synchronously on this thread,
    // so its `&mut Vec` stays valid. `ReleaseDC` runs on every path after
    // `GetDC` succeeds.
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return Err(IpcError::new(
                ErrorKind::Internal,
                "GetDC returned no device context",
            ));
        }
        let lf = LOGFONTW {
            lfCharSet: SHIFTJIS_CHARSET,
            ..Default::default()
        };
        EnumFontFamiliesExW(
            hdc,
            &lf,
            Some(enum_font_cb),
            LPARAM(&mut out as *mut Vec<String> as isize),
            0,
        );
        ReleaseDC(None, hdc);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

unsafe extern "system" fn enum_font_cb(
    lf: *const LOGFONTW,
    _tm: *const TEXTMETRICW,
    _kind: u32,
    lparam: LPARAM,
) -> i32 {
    // SAFETY: The operating system passes a valid `ENUMLOGFONTEXW` pointer
    // and the `lparam` value. `japanese_font_families` supplies an
    // `&mut Vec<String>` pointer that outlives font enumeration.
    unsafe {
        let elf = &*(lf as *const ENUMLOGFONTEXW);
        let name = String::from_utf16_lossy(
            &elf.elfLogFont.lfFaceName[..elf
                .elfLogFont
                .lfFaceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(0)],
        );
        if !name.is_empty() && !name.starts_with('@') {
            (*(lparam.0 as *mut Vec<String>)).push(name);
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_sorted_unique() {
        let families = japanese_font_families().expect("font enumeration should succeed");
        assert!(
            families.windows(2).all(|pair| pair[0] < pair[1]),
            "families must be sorted with no duplicate: {families:?}"
        );
    }
}

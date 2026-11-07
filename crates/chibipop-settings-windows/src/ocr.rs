//! Installed Windows OCR recognisers.
//!
//! This is a copy of `installed_recognisers` in the daemon crate.
//! The daemon crate is a bin and the settings crate cannot link it.

use crate::ipc::{ErrorKind, IpcError};
use windows::Media::Ocr::OcrEngine;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Returns each installed recogniser as a name and tag pair.
///
/// A broken WinRT call is an error, not an empty list.
pub fn installed_recognisers() -> Result<Vec<(String, String)>, IpcError> {
    let io = |e: windows::core::Error| IpcError::new(ErrorKind::Io, format!("Windows OCR: {e}"));
    // SAFETY: `RoInitialize` takes an enum value and no pointer. A repeat
    // call on a thread that already holds an apartment only counts a
    // reference.
    let started = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    match started {
        Err(e) if e.code() != RPC_E_CHANGED_MODE => return Err(io(e)),
        _ => {}
    }
    let langs = OcrEngine::AvailableRecognizerLanguages().map_err(io)?;
    let mut out = Vec::new();
    for l in langs {
        let Ok(tag) = l.LanguageTag() else {
            continue;
        };
        let tag = tag.to_string();
        let name = l.DisplayName().map(|n| n.to_string()).unwrap_or_else(|_| tag.clone());
        out.push((name, tag));
    }
    Ok(out)
}

fn extends_at_boundary(long: &str, short: &str) -> bool {
    let (long, short) = (long.as_bytes(), short.as_bytes());
    long.len() >= short.len()
        && long[..short.len()].eq_ignore_ascii_case(short)
        && (long.len() == short.len()
            || (long.len() > short.len() + 1 && long[short.len()] == b'-'))
}

/// Matches tags only at a subtag boundary, so `ja` matches `ja-JP`.
pub fn tag_matches(reported: &str, wanted: &str) -> bool {
    extends_at_boundary(reported, wanted) || extends_at_boundary(wanted, reported)
}

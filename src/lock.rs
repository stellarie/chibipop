//! Cross-process Apply lock.

use anyhow::{bail, Context, Result};
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

/// Held for one whole Apply.
pub struct LibraryLock(HANDLE);

impl LibraryLock {
    /// Claim `dir`, or refuse.
    pub fn acquire(dir: &Path) -> Result<LibraryLock> {
        let name: Vec<u16> = name_for(dir).encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the
        // call, which is all `CreateMutexW` reads through the pointer. A null
        // security descriptor asks for the default, and the returned handle is
        // owned by this struct and closed exactly once, in `Drop`.
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }
            .context("CreateMutexW for the library lock")?;
        // SAFETY: FFI call with no preconditions, reading this thread's own
        // last-error value, which the call above just set.
        let taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if taken {
            // SAFETY: `handle` was just returned by `CreateMutexW` and is not
            // stored anywhere else, so closing it here cannot be a double free.
            unsafe {
                let _ = CloseHandle(handle);
            }
            bail!("another chibipop is changing your dictionaries - close it and try again");
        }
        Ok(LibraryLock(handle))
    }
}

impl Drop for LibraryLock {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateMutexW` in `acquire`, is owned by
        // this struct alone, and `Drop` runs exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// One name per library folder.
fn name_for(dir: &Path) -> String {
    format!("chibipop-library-{:016x}", fnv1a(&dir.to_string_lossy().to_lowercase()))
}

/// FNV-1a, 64-bit.
///
/// Stable across builds.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_the_published_fnv1a_vector() {
        assert_eq!(0xcbf2_9ce4_8422_2325, fnv1a(""));
        assert_eq!(0xaf63_dc4c_8601_ec8c, fnv1a("a"));
        assert_eq!(0x85944171f73967e8, fnv1a("foobar"));
    }

    #[test]
    fn two_library_folders_do_not_share_a_lock() {
        assert_ne!(name_for(Path::new(r"C:\a\library")), name_for(Path::new(r"C:\b\library")));
    }

    /// Paths are case-insensitive.
    #[test]
    fn the_same_folder_named_two_ways_is_one_lock() {
        assert_eq!(name_for(Path::new(r"C:\A\Library")), name_for(Path::new(r"c:\a\library")));
    }

    #[test]
    fn a_name_is_short_enough_and_free_of_backslashes() {
        let name = name_for(Path::new(r"C:\Users\Someone\a\very\long\install\path\library"));
        assert!(name.len() < 260, "{name}");
        assert!(!name.contains('\\'), "{name}");
    }

    /// Refused, never queued.
    #[test]
    fn a_second_lock_on_one_folder_is_refused_and_freed_by_dropping_the_first() {
        let dir = std::env::temp_dir().join(format!("chibipop_lock_{}", std::process::id()));
        let first = LibraryLock::acquire(&dir).unwrap();
        assert!(LibraryLock::acquire(&dir).is_err());
        drop(first);
        assert!(LibraryLock::acquire(&dir).is_ok());
    }
}

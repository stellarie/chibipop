//! Provides a cross-process lock for a library Apply.

use anyhow::{bail, Context, Result};
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

/// Holds the library lock for one complete Apply.
pub struct LibraryLock(HANDLE);

impl LibraryLock {
    /// Claims the lock for `dir`, or returns an error.
    pub fn acquire(dir: &Path) -> Result<LibraryLock> {
        let name: Vec<u16> = name_for(dir).encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer. It stays alive for
        // this call, and `CreateMutexW` reads only through this pointer. The
        // null security descriptor selects the default. This struct owns the
        // returned handle and closes it exactly once in `Drop`.
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }
            .context("CreateMutexW for the library lock")?;
        // SAFETY: This FFI call has no preconditions. It reads this thread's
        // own last-error value that the call before it set.
        let taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if taken {
            // SAFETY: `CreateMutexW` returned `handle` immediately before this
            // block. This struct has not stored the handle. This block closes
            // it once, so no double free occurs.
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
        // SAFETY: `acquire` received `self.0` from `CreateMutexW`. This struct
        // owns the handle alone. `Drop` runs once and closes the handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Makes one lock name for each library folder.
fn name_for(dir: &Path) -> String {
    format!("chibipop-library-{:016x}", fnv1a(&dir.to_string_lossy().to_lowercase()))
}

/// Computes a 64-bit FNV-1a value.
///
/// The result stays stable across builds.
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

    /// Windows treats these paths as case-insensitive.
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

    /// Refuses a second lock and does not queue it.
    #[test]
    fn a_second_lock_on_one_folder_is_refused_and_freed_by_dropping_the_first() {
        let dir = std::env::temp_dir().join(format!("chibipop_lock_{}", std::process::id()));
        let first = LibraryLock::acquire(&dir).unwrap();
        assert!(LibraryLock::acquire(&dir).is_err());
        drop(first);
        assert!(LibraryLock::acquire(&dir).is_ok());
    }
}

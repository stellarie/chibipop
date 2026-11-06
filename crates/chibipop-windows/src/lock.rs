//! Provides cross-process locks.

use anyhow::{bail, Context, Result};
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

/// The wait for the config lock, in milliseconds.
const CONFIG_LOCK_WAIT_MS: u32 = 5000;

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

/// Holds the config lock for one load-merge-save.
pub struct ConfigLock(HANDLE);

impl ConfigLock {
    /// Waits for the lock on `config_path`.
    pub fn acquire(config_path: &Path) -> Result<ConfigLock> {
        let name: Vec<u16> = format!("chibipop-config-{}", config_key(config_path))
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer. It stays alive for
        // this call, and `CreateMutexW` reads only through this pointer. The
        // null security descriptor selects the default. This struct owns the
        // returned handle and closes it exactly once in `Drop`.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .context("CreateMutexW for the config lock")?;
        // SAFETY: `handle` came from `CreateMutexW` above and stays open. This
        // call only waits on it. It does not close or move the handle.
        let waited = unsafe { WaitForSingleObject(handle, CONFIG_LOCK_WAIT_MS) };
        if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
            return Ok(ConfigLock(handle));
        }
        // SAFETY: `CreateMutexW` returned `handle` above. This struct has not
        // stored it. This block closes it once, so no double free occurs.
        unsafe {
            let _ = CloseHandle(handle);
        }
        if waited == WAIT_TIMEOUT {
            bail!("another settings window is saving");
        }
        bail!("waiting for the config lock failed");
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        // SAFETY: `acquire` waited on `self.0`, so the acquiring thread owns
        // the mutex. `HANDLE` is not `Send`, so `Drop` runs on that thread.
        // This struct owns the handle alone, and `Drop` closes it once.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// Makes the hex key of a config file.
/// The pipe name and the lock name use it.
pub(crate) fn config_key(config_path: &Path) -> String {
    let text = std::fs::canonicalize(config_path)
        .unwrap_or_else(|_| config_path.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    format!("{:016x}", fnv1a(&text))
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
    fn two_library_folders_do_not_share_a_lock() {
        assert_ne!(name_for(Path::new(r"C:\a\library")), name_for(Path::new(r"C:\b\library")));
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

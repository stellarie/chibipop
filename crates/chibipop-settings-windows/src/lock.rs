//! The cross-process lock for configuration writes.

use crate::ipc::{ErrorKind, IpcError};
use std::marker::PhantomData;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

const WAIT_MS: u32 = 5000;

/// Holds the lock for one load-merge-save sequence.
pub struct ConfigLock {
    handle: HANDLE,
    // Keeps this value on the thread that took the mutex.
    _same_thread: PhantomData<*const ()>,
}

impl ConfigLock {
    /// Waits up to five seconds for the lock on `config_path`.
    pub fn acquire(config_path: &Path) -> Result<Self, IpcError> {
        let name: Vec<u16> = lock_name(config_path)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer. It stays alive for
        // this call, and `CreateMutexW` reads only through this pointer. The
        // null security descriptor selects the default. This struct owns the
        // returned handle and closes it exactly once in `Drop`.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .map_err(|e| IpcError::new(ErrorKind::Io, format!("creating the config lock: {e}")))?;
        // SAFETY: `handle` is the valid mutex handle that `CreateMutexW` just
        // returned. The wait only reads the handle and has no other input.
        let wait = unsafe { WaitForSingleObject(handle, WAIT_MS) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            return Ok(Self {
                handle,
                _same_thread: PhantomData,
            });
        }
        // SAFETY: The wait did not acquire the mutex. This block closes the
        // handle once, because no `ConfigLock` stores it.
        unsafe {
            let _ = CloseHandle(handle);
        }
        let message = if wait == WAIT_TIMEOUT {
            "another settings window is saving".to_string()
        } else {
            format!("waiting for the config lock failed ({})", wait.0)
        };
        Err(IpcError::new(ErrorKind::Io, message))
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        // SAFETY: `acquire` returned this struct only after the wait took the
        // mutex on this thread. `_same_thread` keeps the struct on that thread.
        // This struct owns the handle alone, and `Drop` runs once.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Makes the key text for one configuration file.
pub fn key_text(config_path: &Path) -> String {
    let path = std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    path.to_string_lossy().to_lowercase()
}

/// Computes a 64-bit FNV-1a value.
pub fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn lock_name(config_path: &Path) -> String {
    format!("chibipop-config-{:016x}", fnv1a(&key_text(config_path)))
}

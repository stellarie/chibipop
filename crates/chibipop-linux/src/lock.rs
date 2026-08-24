//! Single-instance lock (ADR-0006): `flock(LOCK_EX | LOCK_NB)` on
//! `$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.lock`.
//!
//! Kernel-released on any death — no stale-lock handling, and no unlink
//! on exit either: removing the file would let a third launch lock a
//! fresh inode while a second one still holds the old, and two daemons
//! is exactly what this exists to prevent. The runtime dir is a per-boot
//! tmpfs; an idle 16-byte file is not litter.

use std::fs::{File, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Held for the daemon's whole life.
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub enum LockError {
    /// Another daemon owns this display.
    AlreadyRunning { path: PathBuf, pid: Option<u32> },
    Io(std::io::Error),
}

impl From<std::io::Error> for LockError {
    fn from(e: std::io::Error) -> LockError {
        LockError::Io(e)
    }
}

/// One lock per compositor instance.
pub fn file_name(display: &str) -> String {
    format!("run-{}.lock", sanitize(display))
}

/// `$WAYLAND_DISPLAY` may be an absolute path; keep it one filename.
pub fn sanitize(display: &str) -> String {
    display
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect()
}

/// Claim `display`, or refuse naming the holder.
pub fn acquire(runtime_dir: &Path, display: &str) -> Result<InstanceLock, LockError> {
    std::fs::create_dir_all(runtime_dir)?;
    let path = runtime_dir.join(file_name(display));
    // truncate(false) is load-bearing: truncating on open would erase the
    // live holder's pid before we know whether we ARE the holder.
    let mut file =
        File::options().read(true).write(true).create(true).truncate(false).open(&path)?;
    match file.try_lock() {
        Ok(()) => {
            // Best-effort breadcrumb for the refusal message.
            let _ = file.set_len(0);
            let _ = file.rewind();
            let _ = write!(file, "{}", std::process::id());
            let _ = file.flush();
            Ok(InstanceLock { _file: file, path })
        }
        Err(TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = file.read_to_string(&mut pid);
            Err(LockError::AlreadyRunning {
                path,
                pid: pid.trim().parse().ok(),
            })
        }
        Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
    }
}

/// The one message a second launch prints.
pub fn refusal(display: &str, path: &Path, pid: Option<u32>) -> String {
    let holder = match pid {
        Some(pid) => format!("pid {pid}"),
        None => "an unknown pid".to_string(),
    };
    format!(
        "already running for WAYLAND_DISPLAY={display} ({holder} holds {}); \
         use `chibipop ctl` to talk to it",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chibipop_lock_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_display_that_is_a_path_stays_one_filename() {
        assert_eq!("run-_run_user_1000_wayland-1.lock", file_name("/run/user/1000/wayland-1"));
        assert_eq!("run-wayland-1.lock", file_name("wayland-1"));
    }

    /// Refused, never queued; freed by dropping the first.
    #[test]
    fn a_second_acquire_is_refused_and_names_the_holder() {
        let dir = tmp("contend");
        let first = acquire(&dir, "wayland-9").expect("first acquire");
        match acquire(&dir, "wayland-9") {
            Err(LockError::AlreadyRunning { path, pid }) => {
                assert_eq!(first.path(), path);
                assert_eq!(Some(std::process::id()), pid);
            }
            Err(LockError::Io(e)) => panic!("io error instead of refusal: {e}"),
            Ok(_) => panic!("a second lock on one display must be refused"),
        }
        drop(first);
        acquire(&dir, "wayland-9").expect("freed by dropping the first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_displays_do_not_share_a_lock() {
        let dir = tmp("displays");
        let _a = acquire(&dir, "wayland-0").expect("first display");
        let _b = acquire(&dir, "wayland-1").expect("second display");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_refusal_names_display_pid_and_path() {
        let msg = refusal("wayland-1", Path::new("/run/user/1000/chibipop/run-wayland-1.lock"), Some(4242));
        assert!(msg.contains("wayland-1"), "{msg}");
        assert!(msg.contains("4242"), "{msg}");
        assert!(msg.contains("run-wayland-1.lock"), "{msg}");
    }
}

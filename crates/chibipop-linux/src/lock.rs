//! Single-instance lock (ARCHITECTURE.md#platform-integration):
//! `flock(LOCK_EX | LOCK_NB)` on
//! `$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.lock`.
//!
//! Kernel-released on any death — no stale-lock handling, and no unlink
//! on exit either: removing the file would let a third launch lock a
//! fresh inode while a second one still holds the old, and two daemons
//! is exactly what this exists to prevent. The runtime dir is a per-boot
//! tmpfs; an idle 16-byte file is not litter.

use std::fs::{File, TryLockError};
use std::io::{Read, Seek, Write};
use std::os::unix::ffi::OsStrExt;
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

/// The settings window's own lock, beside the daemon's: one settings
/// process per compositor instance, without ever contending with the
/// daemon (ARCHITECTURE.md#settings-and-config).
pub fn settings_file_name(display: &str) -> String {
    format!("settings-{}.lock", sanitize(display))
}

/// The library's own lock, keyed by the library path rather than by
/// display: a rebuild rewrites files on disk, so what must never overlap
/// is two writers of the *same library*, whichever session they belong
/// to. Windows guards this with a named mutex; on Linux it is one more
/// flock in the same per-user runtime dir
/// (ARCHITECTURE.md#settings-and-config).
pub fn library_file_name(library: &Path) -> String {
    format!("library-{:016x}.lock", fnv1a(library))
}

/// FNV-1a over the library path's bytes.
///
/// Hand-rolled on purpose: `DefaultHasher` is explicitly not stable
/// across releases, and a lock name that moves with the toolchain guards
/// nothing. Trailing separators are dropped so `…/library` and
/// `…/library/` name one lock.
fn fnv1a(library: &Path) -> u64 {
    let bytes = library.as_os_str().as_bytes();
    let end = bytes.iter().rposition(|b| *b != b'/').map_or(0, |i| i + 1);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes[..end] {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
    acquire_at(runtime_dir, &file_name(display))
}

/// The same claim on any lock file name (the settings lock differs only
/// in name; flock semantics are shared).
pub fn acquire_at(runtime_dir: &Path, file_name: &str) -> Result<InstanceLock, LockError> {
    std::fs::create_dir_all(runtime_dir)?;
    let path = runtime_dir.join(file_name);
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

/// The one line a refused rebuild shows in the status area.
pub fn rebuild_refusal(library: &Path, path: &Path, pid: Option<u32>) -> String {
    let holder = match pid {
        Some(pid) => format!("pid {pid}"),
        None => "another chibipop".to_string(),
    };
    format!(
        "Another rebuild of {} is already running ({holder} holds {}); \
         wait for it to finish.",
        library.display(),
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

    /// The settings window never contends with the daemon: two locks,
    /// two names, one display.
    #[test]
    fn settings_lock_is_scoped_apart_from_the_daemon_lock() {
        let dir = tmp("settings_scope");
        let _daemon = acquire(&dir, "wayland-5").expect("daemon lock");
        let _settings = acquire_at(&dir, &settings_file_name("wayland-5"))
            .expect("the settings lock must not contend with the daemon's");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second `chibipop settings` on the same display is refused.
    #[test]
    fn a_second_settings_lock_is_refused() {
        let dir = tmp("settings_contend");
        let name = settings_file_name("wayland-5");
        let first = acquire_at(&dir, &name).expect("first settings lock");
        match acquire_at(&dir, &name) {
            Err(LockError::AlreadyRunning { path, pid }) => {
                assert_eq!(first.path(), path);
                assert_eq!(Some(std::process::id()), pid);
            }
            Err(LockError::Io(e)) => panic!("io error instead of refusal: {e}"),
            Ok(_) => panic!("a second settings lock on one display must be refused"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_refusal_names_display_pid_and_path() {
        let msg = refusal("wayland-1", Path::new("/run/user/1000/chibipop/run-wayland-1.lock"), Some(4242));
        assert!(msg.contains("wayland-1"), "{msg}");
        assert!(msg.contains("4242"), "{msg}");
        assert!(msg.contains("run-wayland-1.lock"), "{msg}");
    }

    /// Two libraries never share a lock, and one library always names the
    /// same lock file — a rebuild guard keyed by a hash that drifts would
    /// guard nothing.
    #[test]
    fn the_library_lock_is_keyed_by_path_and_stable() {
        let a = library_file_name(Path::new("/home/x/.local/share/chibipop/library"));
        assert_eq!(a, library_file_name(Path::new("/home/x/.local/share/chibipop/library")));
        assert_eq!(a, library_file_name(Path::new("/home/x/.local/share/chibipop/library/")));
        assert_ne!(a, library_file_name(Path::new("/home/y/.local/share/chibipop/library")));
        assert_eq!("library-7938d952a0e0914d.lock", a);
    }

    /// One library, two settings processes: the second rebuild is
    /// refused, not queued.
    #[test]
    fn a_second_rebuild_of_one_library_is_refused() {
        let dir = tmp("library_contend");
        let library = Path::new("/home/x/.local/share/chibipop/library");
        let name = library_file_name(library);
        let first = acquire_at(&dir, &name).expect("first library lock");
        match acquire_at(&dir, &name) {
            Err(LockError::AlreadyRunning { path, pid }) => {
                assert_eq!(first.path(), path);
                assert_eq!(Some(std::process::id()), pid);
                let msg = rebuild_refusal(library, &path, pid);
                assert!(msg.contains("library"), "{msg}");
                assert!(msg.contains(&std::process::id().to_string()), "{msg}");
            }
            Err(LockError::Io(e)) => panic!("io error instead of refusal: {e}"),
            Ok(_) => panic!("two rebuilds of one library must not overlap"),
        }
        drop(first);
        acquire_at(&dir, &name).expect("freed by the first rebuild finishing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebuild never blocks the daemon or the settings window itself.
    #[test]
    fn the_library_lock_is_scoped_apart_from_the_instance_locks() {
        let dir = tmp("library_scope");
        let _daemon = acquire(&dir, "wayland-5").expect("daemon lock");
        let _settings = acquire_at(&dir, &settings_file_name("wayland-5")).expect("settings lock");
        let _library = acquire_at(&dir, &library_file_name(Path::new("/tmp/lib")))
            .expect("the library lock must not contend with the instance locks");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

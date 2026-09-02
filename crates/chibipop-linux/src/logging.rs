//! Diagnostics and the lookup log
//! (ARCHITECTURE.md#platform-integration).
//!
//! Diagnostics go to stderr always (terminal; journal under a systemd
//! unit) plus a logfile truncated on start — bounded without rotation
//! machinery. Lookup lines — actual words read off the user's screen —
//! are written only when `debug.show_lookup_log` is on; screen content
//! never touches disk un-opted-in.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct Log {
    start: Instant,
    /// `None` when the state dir was unwritable — stderr still works.
    file: Option<File>,
    file_path: Option<PathBuf>,
    show_lookup: bool,
}

impl Log {
    /// Truncates `<state_dir>/chibipop.log` and starts logging.
    ///
    /// A logfile failure is itself only a diagnostic: a read-only state
    /// dir must not keep the daemon from running.
    pub fn open(log_file: &Path, show_lookup: bool) -> Log {
        let file = log_file
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| File::create(log_file).map(Some))
            .unwrap_or(None);
        let mut log = Log {
            start: Instant::now(),
            file_path: file.is_some().then(|| log_file.to_path_buf()),
            file,
            show_lookup,
        };
        if log.file.is_none() {
            log.diag(&format!("log: cannot write {} - stderr only", log_file.display()));
        }
        log
    }

    /// Where diagnostics persist, when they do.
    pub fn path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// A diagnostic: stderr and the logfile, always.
    pub fn diag(&mut self, msg: &str) {
        let line = format!("[{:9.3}] {msg}", self.start.elapsed().as_secs_f64());
        eprintln!("{line}");
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Screen content. Dropped entirely unless the user opted in —
    /// never written anywhere, not even redacted.
    pub fn lookup(&mut self, msg: &str) {
        if self.show_lookup {
            self.diag(&format!("lookup: {msg}"));
        }
    }

    /// `reload` re-reads the config; the gate follows it live.
    pub fn set_show_lookup(&mut self, on: bool) {
        self.show_lookup = on;
    }

    pub fn show_lookup(&self) -> bool {
        self.show_lookup
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chibipop_log_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("chibipop.log")
    }

    #[test]
    fn diagnostics_land_in_the_file() {
        let path = tmp("diag");
        let mut log = Log::open(&path, false);
        log.diag("hello");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hello"), "{text}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The gate for actual screen content: off means not on disk at all.
    #[test]
    fn lookup_content_stays_off_disk_until_opted_in() {
        let path = tmp("gate");
        let mut log = Log::open(&path, false);
        log.lookup("秘密の言葉");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("秘密"), "opt-out lookup content reached disk: {text}");

        log.set_show_lookup(true);
        log.lookup("公開の言葉");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("lookup: 公開の言葉"), "{text}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Bounded without rotation machinery.
    #[test]
    fn a_restart_truncates_the_previous_log() {
        let path = tmp("trunc");
        Log::open(&path, false).diag("first run line");
        let mut second = Log::open(&path, false);
        second.diag("second run line");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("first run line"), "{text}");
        assert!(text.contains("second run line"), "{text}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A read-only state dir must not kill the daemon.
    #[test]
    fn an_unwritable_logfile_degrades_to_stderr() {
        let mut log = Log::open(Path::new("/proc/definitely/not/writable/x.log"), false);
        assert_eq!(None, log.path());
        log.diag("still alive");
    }
}

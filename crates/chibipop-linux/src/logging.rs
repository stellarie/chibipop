//! Diagnostics and the lookup log.
//! See ARCHITECTURE.md#platform-integration.
//!
//! Diagnostics go to stderr always.
//! Examples are a terminal or a journal under a systemd unit.
//! Diagnostics also go to a log file that truncates at startup.
//! Therefore, the file is bounded without rotation mechanisms.
//! Lookup lines record words read from the screen of the user.
//! The system writes lookup lines only when `debug.show_lookup_log` is active.
//! Screen content never touches disk without an opt-in.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct Log {
    start: Instant,
    /// `None` when the state directory cannot be written. Stderr still works.
    file: Option<File>,
    file_path: Option<PathBuf>,
    show_lookup: bool,
}

impl Log {
    /// Truncate `<state_dir>/chibipop.log` and start the log.
    ///
    /// A log file failure is only a diagnostic.
    /// A read-only state directory must not stop the daemon.
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

    /// Get the path where diagnostics persist, when a path exists.
    pub fn path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// Write a diagnostic line to stderr and to the log file.
    pub fn diag(&mut self, msg: &str) {
        let line = format!("[{:9.3}] {msg}", self.start.elapsed().as_secs_f64());
        eprintln!("{line}");
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Write screen content. Drop the message if the user did not opt in.
    /// Never write screen content anywhere without an opt-in.
    pub fn lookup(&mut self, msg: &str) {
        if self.show_lookup {
            self.diag(&format!("lookup: {msg}"));
        }
    }

    /// `reload` reads the configuration again. The gate changes immediately.
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

    /// The gate for screen content. Off means not on disk.
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

    /// Bounded without file rotation mechanisms.
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

    /// A read-only state directory must not stop the daemon.
    #[test]
    fn an_unwritable_logfile_degrades_to_stderr() {
        let mut log = Log::open(Path::new("/proc/definitely/not/writable/x.log"), false);
        assert_eq!(None, log.path());
        log.diag("still alive");
    }
}

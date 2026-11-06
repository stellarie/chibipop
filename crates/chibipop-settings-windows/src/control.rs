//! The client for the daemon's control pipe.
//!
//! The wire format matches `crates/chibipop-windows/src/control.rs`.

use crate::lock::{fnv1a, key_text};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BUSY_RETRY: Duration = Duration::from_millis(50);
const BUSY_LIMIT: Duration = Duration::from_secs(2);
const REPLY_LIMIT: Duration = Duration::from_secs(10);
const ERROR_PIPE_BUSY: i32 = 231;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    Live { reply: String },
    ConfigOnly,
    NoReply,
}

/// Names the control pipe for one configuration file.
pub fn pipe_name(config_path: &Path) -> String {
    format!(
        r"\\.\pipe\chibipop-control-{:016x}",
        fnv1a(&key_text(config_path))
    )
}

/// Sends `reload` to the daemon that serves `config_path`.
pub fn send_reload(config_path: &Path) -> ReloadOutcome {
    send_line(&pipe_name(config_path), "reload\n", REPLY_LIMIT)
}

fn send_line(pipe: &str, line: &str, limit: Duration) -> ReloadOutcome {
    let started = Instant::now();
    let mut file = loop {
        match OpenOptions::new().read(true).write(true).open(pipe) {
            Ok(file) => break file,
            Err(e) if e.kind() == ErrorKind::NotFound => return ReloadOutcome::ConfigOnly,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                if started.elapsed() >= BUSY_LIMIT {
                    return ReloadOutcome::NoReply;
                }
                std::thread::sleep(BUSY_RETRY);
            }
            Err(_) => return ReloadOutcome::NoReply,
        }
    };
    if file.write_all(line.as_bytes()).is_err() {
        return ReloadOutcome::NoReply;
    }
    match read_reply(file, limit) {
        Some(reply) => ReloadOutcome::Live { reply },
        None => ReloadOutcome::NoReply,
    }
}

// Reads one reply line, and gives up after `limit`.
fn read_reply(mut file: std::fs::File, limit: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while buf.len() < 512 {
            match file.read(&mut byte) {
                Ok(1) if byte[0] != b'\n' => buf.push(byte[0]),
                _ => break,
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).trim().to_string());
    });
    rx.recv_timeout(limit).ok().filter(|reply| !reply.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_golden() {
        assert_eq!(
            pipe_name(Path::new(r"C:\Users\X\chibipop.toml")),
            r"\\.\pipe\chibipop-control-b3e76833bdc645c8"
        );
    }

    #[test]
    fn absent_daemon_is_config_only() {
        let path = std::env::temp_dir().join(format!("chibipop-absent-{}.toml", std::process::id()));
        assert_eq!(send_reload(&path), ReloadOutcome::ConfigOnly);
    }
}

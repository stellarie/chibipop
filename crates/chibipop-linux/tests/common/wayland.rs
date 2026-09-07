#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Create a private XDG tree and link its runtime socket to the compositor.
pub fn scratch(prefix: &str, subdirs: &[&str]) -> PathBuf {
    let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in subdirs {
        std::fs::create_dir_all(dir.join(sub)).expect("creating the scratch tree");
    }
    if !display.starts_with('/') {
        let runtime = std::env::var("XDG_RUNTIME_DIR").expect("a session runtime dir");
        std::os::unix::fs::symlink(
            PathBuf::from(runtime).join(&display),
            dir.join("run").join(&display),
        )
        .expect("linking the compositor socket into the scratch tree");
    }
    dir
}

/// Set the private XDG paths and the compositor display on a child command.
pub fn xdg(cmd: &mut Command, dir: &Path) {
    let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
    cmd.env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("WAYLAND_DISPLAY", display);
}

/// Return the first required global missing from the compositor probe.
pub fn missing_global(bin: &str, needed: &[&str]) -> Option<String> {
    let probe = Command::new(bin).arg("probe").output().expect("spawning chibipop probe");
    let report = String::from_utf8_lossy(&probe.stdout);
    needed
        .iter()
        .find(|global| !report.contains(&format!("{global} v")))
        .map(|global| (*global).to_string())
}

/// Wait for a log line that contains `needle`, then return it.
pub fn wait_for(log: &Path, needle: &str, seconds: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if let Some(line) = text.lines().rev().find(|line| line.contains(needle)) {
            return line.to_string();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let text = std::fs::read_to_string(log).unwrap_or_default();
    panic!("waited {seconds}s for {needle:?}; the log was:\n{text}");
}

/// Return one `hyprctl` subcommand's output, or `None` outside Hyprland.
pub fn hyprctl(sub: &str) -> Option<String> {
    let out = Command::new("hyprctl").args(["-i", "0", sub]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// Send SIGTERM, reap the child, and remove its private tree.
pub fn teardown(child: &mut Child, dir: &Path) {
    let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(dir);
}

/// Send SIGTERM to a child without waiting for it.
pub fn terminate(child: &Child) {
    let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
}

/// Reap a child for up to two seconds. This reap is the honest way to test
/// whether the daemon exited. An unwaited child stays a zombie, and `kill -0`
/// cannot tell a zombie from a process that runs.
pub fn wait_exit(child: &mut Child) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return false,
        }
    }
    false
}

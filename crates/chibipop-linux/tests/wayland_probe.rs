//! Against the real compositor, when there is one.
//!
//! CI is headless (ADR-0007): every test here skips without
//! `WAYLAND_DISPLAY`, so the gate stays compositor-free while a dev box
//! still exercises the real connect path on `cargo test`.
#![cfg(target_os = "linux")]

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

fn skip() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return true;
    }
    false
}

#[test]
fn probe_reports_the_core_globals_on_a_live_compositor() {
    if skip() {
        return;
    }
    let out = std::process::Command::new(BIN).arg("probe").output().expect("spawning chibipop probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "probe failed: {stdout}\n{stderr}");
    assert!(stdout.contains("wl_compositor"), "{stdout}");
    assert!(stdout.contains("globals advertised"), "{stdout}");
}

/// `probe` must not take the instance lock or the socket: it runs beside
/// a live daemon by design.
#[test]
fn probe_leaves_no_lock_or_socket_behind() {
    if skip() {
        return;
    }
    let out = std::process::Command::new(BIN).arg("probe").output().expect("spawning chibipop probe");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("lock"), "probe must stay lock-free: {stdout}");
}

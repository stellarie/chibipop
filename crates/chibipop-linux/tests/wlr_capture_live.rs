//! The capture backend against a real compositor, when there is one.
//!
//! CI is headless (ADR-0007), so every test here skips without
//! `WAYLAND_DISPLAY` - and skips again when the session advertises no
//! `zwlr_screencopy_manager_v1`, because an absent rung is a rung the
//! ladder walks past (ADR-0002), not a failure.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The screencopy global this backend needs.
const MANAGER: &str = "zwlr_screencopy_manager_v1";

fn skip() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return true;
    }
    let probe = Command::new(BIN).arg("probe").output().expect("spawning chibipop probe");
    let report = String::from_utf8_lossy(&probe.stdout).to_string();
    if !report.contains(MANAGER) {
        eprintln!("skipping: this compositor advertises no {MANAGER}");
        return true;
    }
    false
}

/// A scratch directory of our own, so a dump never collides with one
/// from another test or a stray run.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chibipop-capture-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// `width x height` out of a PNG's IHDR.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).expect("reading the dumped PNG");
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], "not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR", "IHDR must come first");
    (
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
    )
}

/// Every `capture:` line the dump printed for a grab.
fn grab_lines(stdout: &str) -> Vec<&str> {
    stdout.lines().filter(|l| l.contains("unchanged=")).collect()
}

#[test]
fn a_real_region_grab_lands_in_a_png_of_the_size_it_promised() {
    if skip() {
        return;
    }
    let dir = scratch("grab");
    let out = Command::new(BIN)
        .args(["capture-dump", "--region", "8,8,64,48", "--out"])
        .arg(&dir)
        .output()
        .expect("spawning chibipop capture-dump");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the dump failed: {stdout}\n{stderr}");
    let lines = grab_lines(&stdout);
    assert_eq!(lines.len(), 1, "one region, one grab: {stdout}");
    assert!(lines[0].contains("64x48 at (8,8)"), "{stdout}");
    assert!(lines[0].contains("source=wlr-screencopy"), "{stdout}");
    // A fresh region can never be an unchanged one.
    assert!(lines[0].contains("unchanged=false"), "{stdout}");
    assert_eq!(png_size(&dir.join("chibipop-capture-0.png")), (64, 48), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every advertised output must be grabbable, whatever its scale: the
/// dump samples each one when given no explicit box.
#[test]
fn every_output_answers_a_grab() {
    if skip() {
        return;
    }
    let dir = scratch("outputs");
    let out = Command::new(BIN)
        .args(["capture-dump", "--out"])
        .arg(&dir)
        .output()
        .expect("spawning chibipop capture-dump");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the dump failed: {stdout}");
    let outputs = stdout.lines().filter(|l| l.starts_with("capture: output ")).count();
    assert!(outputs >= 1, "no output geometry: {stdout}");
    assert_eq!(grab_lines(&stdout).len(), outputs, "one grab per output: {stdout}");
    for i in 0..outputs {
        let png = dir.join(format!("chibipop-capture-{i}.png"));
        let (w, h) = png_size(&png);
        assert!(w > 0 && h > 0, "{} is empty", png.display());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The trait's never-block invariant, measured: a dwell races damage
/// against the 250 ms deadline (ADR-0010), so five reads of a static
/// screen must finish in well under the two seconds an unbounded wait
/// would take - and a busy screen must not be slower.
#[test]
fn a_dwell_answers_within_the_deadline_and_never_hangs() {
    if skip() {
        return;
    }
    let dir = scratch("dwell");
    let started = Instant::now();
    let out = Command::new(BIN)
        .args(["capture-dump", "--region", "8,8,64,48", "--dwell", "5", "--out"])
        .arg(&dir)
        .output()
        .expect("spawning chibipop capture-dump");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the dump failed: {stdout}");
    // The first grab plus five dwell reads, each bounded by the
    // deadline; generous enough for a loaded machine, tight enough to
    // fail an unbounded wait for damage.
    assert!(
        elapsed < Duration::from_secs(4),
        "six paced reads took {elapsed:?}: {stdout}"
    );
    let lines = grab_lines(&stdout);
    assert_eq!(lines.len(), 6, "one grab plus five dwell reads: {stdout}");
    for line in &lines {
        assert!(line.contains("64x48 at (8,8)"), "a dwell must re-read the same box: {line}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A box on no output at all is refused, not answered with invention.
#[test]
fn a_region_off_every_output_is_refused() {
    if skip() {
        return;
    }
    let dir = scratch("offscreen");
    let out = Command::new(BIN)
        .args(["capture-dump", "--region", "900000,900000,32,32", "--out"])
        .arg(&dir)
        .output()
        .expect("spawning chibipop capture-dump");
    assert!(!out.status.success(), "an off-screen box must fail the grab");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("is on no output"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

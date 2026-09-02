//! Tests the capture backend against a real compositor, when a
//! compositor exists.
//!
//! CI runs headless. Each test here skips when `WAYLAND_DISPLAY` is
//! unset. Each test skips again when the session advertises no
//! `zwlr_screencopy_manager_v1`. The ladder steps past an absent rung,
//! and an absent rung is not a failure.
//!
//! The three tests that read pixels skip once more when the compositor
//! does not repaint the outputs of this session. A locked desktop with
//! a powered-off panel is the normal state of an unattended development
//! machine. A copy that the compositor never answers measures the power
//! state of the display, not this rung. The fourth test still runs,
//! because a dark screen refuses an off-screen box like a lit screen.
//! That gate is narrow on purpose. See [`UNANSWERED`].
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The screencopy global that this backend needs.
const MANAGER: &str = "zwlr_screencopy_manager_v1";

/// The refusal for a copy that the compositor took and then never
/// answered. The compositor sent neither `ready` nor `failed`. The
/// `copy` request of `wlr-screencopy-unstable-v1` names those two
/// events as its only two answers.
///
/// One cause is measured on this machine. The compositor does not
/// repaint the output. With the display DPMS-off, a grab that answers
/// in 2 ms awake is still unanswered at 10 s (3 of 3, Hyprland 0.55.4).
/// `grim` hangs in the same way on the same session, so no client
/// reaches a powered-off panel.
const UNANSWERED: &str = "the copy went unanswered";

/// Reports whether this session offers the rung.
///
/// This code probes once for the whole file. What a compositor
/// advertises is a property of the session, not of a test.
fn skip() -> bool {
    static WHY: LazyLock<Option<String>> = LazyLock::new(no_rung);
    skipping(&WHY)
}

/// [`skip`], plus a check that the compositor answers a copy.
///
/// Every test that reads pixels needs this check. The test that asserts
/// only a geometry refusal does not need it, and it must not use it. A
/// dark screen refuses an off-screen box like a lit screen. That
/// assertion is therefore the one check here that still gives value on
/// an unattended machine.
fn skip_unless_painting() -> bool {
    static WHY: LazyLock<Option<String>> = LazyLock::new(unanswered_copy);
    skip() || skipping(&WHY)
}

/// Print the reason and skip, or print nothing and run.
fn skipping(why: &Option<String>) -> bool {
    match why {
        Some(why) => {
            eprintln!("skipping: {why}");
            true
        }
        None => false,
    }
}

/// Why this session has no screencopy rung, or `None` when it has one.
fn no_rung() -> Option<String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Some("WAYLAND_DISPLAY is unset (headless)".to_string());
    }
    let probe = Command::new(BIN).arg("probe").output().expect("spawning chibipop probe");
    if !String::from_utf8_lossy(&probe.stdout).contains(MANAGER) {
        return Some(format!("this compositor advertises no {MANAGER}"));
    }
    None
}

/// The refusal of the compositor to answer a copy, or `None` when the
/// compositor answers one.
///
/// The diagnostic is the probe, as in `clipboard_live.rs`. It runs one
/// real grab along the same ladder as the daemon. Only the
/// [`UNANSWERED`] refusal counts as a skip. Every other failure answers
/// `None`, so the assertions below still run. A regression in this
/// crate can then never hide here.
fn unanswered_copy() -> Option<String> {
    let dir = scratch("probe");
    let grab = Command::new(BIN)
        .args(["capture-dump", "--region", "8,8,64,48", "--out"])
        .arg(&dir)
        .output()
        .expect("spawning chibipop capture-dump");
    let refused = String::from_utf8_lossy(&grab.stderr).trim().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    (!grab.status.success() && refused.contains(UNANSWERED)).then_some(refused)
}

/// A private scratch directory. A dump then never collides with a dump
/// from another test or from a stray run.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chibipop-capture-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// The `width x height` values from the IHDR chunk of a PNG.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).expect("reading the dumped PNG");
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], "not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR", "IHDR must come first");
    (
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
    )
}

/// Every `capture:` line that the dump printed for a grab.
fn grab_lines(stdout: &str) -> Vec<&str> {
    stdout.lines().filter(|l| l.contains("unchanged=")).collect()
}

#[test]
fn a_real_region_grab_lands_in_a_png_of_the_size_it_promised() {
    if skip_unless_painting() {
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
    // A fresh region can never be an unchanged region.
    assert!(lines[0].contains("unchanged=false"), "{stdout}");
    assert_eq!(png_size(&dir.join("chibipop-capture-0.png")), (64, 48), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A grab must answer for every advertised output, at any scale. With
/// no explicit box, the dump samples each output.
#[test]
fn every_output_answers_a_grab() {
    if skip_unless_painting() {
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

/// Measures the never-block invariant of the trait. A dwell races
/// damage against the 250 ms deadline. Five reads of a static screen
/// must therefore finish far inside the two seconds that an unbounded
/// wait costs. A busy screen must not be slower.
#[test]
fn a_dwell_answers_within_the_deadline_and_never_hangs() {
    if skip_unless_painting() {
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
    // The first grab plus five dwell reads, and the deadline bounds
    // each read. The limit is large enough for a loaded machine. The
    // limit is also small enough to fail an unbounded wait for damage.
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

/// The code refuses a box that lies on no output. It never invents a
/// frame.
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

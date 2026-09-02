//! This module tests the portal ScreenCast capture rung against a real Wayland session.
//!
//! CI has no display. The tests skip when `WAYLAND_DISPLAY` is not set.
//! The tests also skip when the session bus does not provide
//! `org.freedesktop.portal.ScreenCast`. A missing rung is a ladder state, not a failure.
//!
//! Frame assertions skip when the compositor does not repaint an output.
//! This condition occurs when an unattended computer loses panel power.
//! An unanswered copy tests display power, not this capture rung.
//! The tests still check the selected rung. The daemon writes that line before it requests
//! a frame, so the line does not prove that capture works. See [`skip_unless_painting`].
//!
//! **The default tests never open a consent dialog.** Frequent dialogs can prevent developer
//! test runs. The portal rung reserves consent for a launch, not for `cargo test`.
//! The default tests check ladder selection, the override, and refusal. Set
//! `CHIBIPOP_PORTAL_CONSENT_TEST=1` to enable the test that opens a dialog:
//!
//! ```text
//! CHIBIPOP_PORTAL_CONSENT_TEST=1 \
//!   cargo test -p chibipop-linux --test portal_capture_live -- --nocapture
//! ```
//!
//! This command runs the only test that receives frames through PipeWire.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The chibipop binary under test.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// Both the daemon and `capture-dump` read this Capture backend override.
const BACKEND: &str = "CHIBIPOP_CAPTURE_BACKEND";

/// This opt-in lets the test open a portal consent dialog.
const CONSENT_OPT_IN: &str = "CHIBIPOP_PORTAL_CONSENT_TEST";

/// The compositor emits this error when a `copy` request receives neither `ready` nor
/// `failed`. The `wlr-screencopy-unstable-v1` protocol defines only these two replies.
/// Tests saw this result when no process repainted an output whose power was off.
/// Rung 1 in this file uses that protocol.
/// `wlr_capture_live.rs` records the same result with `UNANSWERED`.
const UNANSWERED: &str = "the copy went unanswered";

/// The test needs a Wayland session and a ScreenCast portal.
/// Skip when either resource does not exist.
fn skip() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return true;
    }
    if !portal_on_the_bus() {
        eprintln!("skipping: no org.freedesktop.portal.ScreenCast on the session bus");
        return true;
    }
    false
}

/// Query the session bus directly. An internal probe failure must not hide a test failure.
fn portal_on_the_bus() -> bool {
    Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.ScreenCast",
            "version",
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Skip when the compositor does not answer the grab in `out`.
/// Without a frame, the grab tests display power instead of this capture rung.
///
/// This rule matches `skip_unless_painting` in `wlr_capture_live.rs`.
/// Only [`UNANSWERED`] causes a skip. A wrong size, a `failed` frame, or a bad format
/// still fails the assertion.
/// Read the completed dump instead of a new probe. A new probe can select the portal rung
/// when the session lacks screencopy. That probe can open the dialog forbidden by the
/// module default.
/// The `tests/` directory contains data fixtures only. It has no shared module, so this
/// file defines the function here.
fn skip_unless_painting(out: &std::process::Output) -> bool {
    let refused = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && refused.contains(UNANSWERED) {
        eprintln!("skipping: {}", refused.trim());
        return true;
    }
    false
}

/// Give each test a scratch state directory.
/// The test does not read or rotate the developer's restore token.
fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("chibipop-portal-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}
/// Give `capture-dump` separate XDG directories for configuration, data, state, and cache.
/// This keeps the developer's installation separate.
fn dump(state: &Path, backend: Option<&str>, region: &str) -> std::process::Output {
    let out = state.join("dump");
    std::fs::create_dir_all(&out).expect("creating the dump directory");
    let mut command = Command::new(BIN);
    command
        .arg("capture-dump")
        .args(["--region", region, "--out"])
        .arg(&out)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("XDG_DATA_HOME", state.join("data"))
        .env("XDG_STATE_HOME", state.join("state"))
        .env("XDG_CACHE_HOME", state.join("cache"))
        .env_remove(BACKEND);
    if let Some(b) = backend {
        command.env(BACKEND, b);
    }
    command.output().expect("spawning chibipop capture-dump")
}

/// Read `width x height` from the `IHDR` chunk of a PNG file.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).expect("reading the dumped PNG");
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], "not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR", "IHDR must come first");
    (
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
    )
}

/// Hyprland advertises screencopy and runs a ScreenCast portal.
/// Make sure that the promptless screencopy rung has priority.
/// Otherwise, wlr users can receive a consent dialog that they do not need.
///
/// The selected rung must also provide a frame. Selection and frame delivery are separate
/// claims. The daemon writes the `capture:` line before it requests a copy, so an
/// unanswered grab can still contain that line.
#[test]
fn a_session_with_both_rungs_still_takes_the_promptless_one() {
    if skip() {
        return;
    }
    let state = scratch("auto");
    let out = dump(&state, None, "0,0,8,8");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stdout.contains("zwlr_screencopy_manager_v1")
        && !stdout.contains("capture: wlr-screencopy region capture")
    {
        eprintln!("skipping: this session advertises no screencopy rung to prefer");
        let _ = std::fs::remove_dir_all(&state);
        return;
    }
    assert!(
        stdout.contains("capture: wlr-screencopy region capture (promptless - ladder rung 1)"),
        "auto must pick rung 1 while screencopy is advertised: {stdout}"
    );
    assert!(
        !stdout.contains("portal ScreenCast + PipeWire"),
        "auto must not reach the portal here: {stdout}"
    );
    // No restore token exists because the daemon never asked the portal.
    assert!(!state.join("state/chibipop/portal-restore-token").exists());
    // The earlier assertions check the selected rung. This assertion checks frame delivery.
    // A compositor can refuse frame delivery when it does not repaint the display.
    if skip_unless_painting(&out) {
        let _ = std::fs::remove_dir_all(&state);
        return;
    }
    assert!(out.status.success(), "the preferred rung grabbed nothing: {stdout}\n{stderr}");
    assert_eq!(png_size(&state.join("dump/chibipop-capture-0.png")), (8, 8), "{stdout}");
    let _ = std::fs::remove_dir_all(&state);
}

/// An empty ladder is a status, not a crash. The diagnostic must name both rungs.
#[test]
fn an_empty_ladder_names_both_rungs_and_fails_cleanly() {
    if skip() {
        return;
    }
    let state = scratch("none");
    let out = dump(&state, Some("none"), "0,0,8,8");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "an empty ladder cannot dump: {stdout}");
    assert!(stdout.contains("capture: unsupported - missing"), "{stdout}");
    assert!(stdout.contains("zwlr_screencopy_manager_v1"), "{stdout}");
    assert!(stdout.contains("org.freedesktop.portal.ScreenCast"), "{stdout}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    // A clean failure creates no partial frame.
    // A partial frame would falsely report capture success.
    assert!(!state.join("dump/chibipop-capture-0.png").exists(), "{stdout}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The test hook reports and ignores an invalid override.
/// The override is not a configuration field, so it must not stop the process.
#[test]
fn an_unknown_backend_override_is_reported_and_ignored() {
    if skip() {
        return;
    }
    let state = scratch("badenv");
    let out = dump(&state, Some("evdev"), "0,0,8,8");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("ignoring CHIBIPOP_CAPTURE_BACKEND=\"evdev\""), "{stdout}");
    assert!(stdout.contains("expected auto|screencopy|portal|none"), "{stdout}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    // The diagnostic alone cannot prove that the daemon ignored the value.
    // The daemon must complete the same grab as it does without the hook.
    if skip_unless_painting(&out) {
        let _ = std::fs::remove_dir_all(&state);
        return;
    }
    assert!(out.status.success(), "a bad override must not be fatal: {stdout}\n{stderr}");
    assert_eq!(png_size(&state.join("dump/chibipop-capture-0.png")), (8, 8), "{stdout}");
    let _ = std::fs::remove_dir_all(&state);
}

/// This opt-in test covers the complete portal rung and its consent dialog.
/// See the module documentation.
///
/// The test runs at most two times. The second run reuses the restore token from the first
/// run. It checks that later launches do not open a dialog.
/// `persist_mode` is a ScreenCast v4 key. Only ScreenCast v4 or later can store a restore
/// token. A ScreenCast v3 portal, such as xdg-desktop-portal-hyprland, cannot remember a
/// grant. A second run on version 3 would open another dialog and prove nothing. The test
/// checks the version 3 diagnostic and then stops.
#[test]
fn the_portal_rung_streams_frames_and_makes_the_next_launch_silent() {
    if skip() {
        return;
    }
    if std::env::var_os(CONSENT_OPT_IN).is_none() {
        eprintln!("skipping: set {CONSENT_OPT_IN}=1 to allow a real consent dialog");
        return;
    }
    let state = scratch("consent");
    let token = state.join("state/chibipop/portal-restore-token");

    eprintln!("PORTAL CONSENT DIALOG OPEN - please approve/deny (waiting up to 120s)");
    let started = std::time::Instant::now();
    let first = dump(&state, Some("portal"), "16,16,320,240");
    let stdout = String::from_utf8_lossy(&first.stdout);
    let stderr = String::from_utf8_lossy(&first.stderr);
    eprintln!("--- first run ({:?}) ---\n{stdout}{stderr}", started.elapsed());

    if !first.status.success() {
        // Portal denial or timeout is a Capture channel state, not a test failure.
        // The daemon must report a retry action and continue to run.
        // These assertions check both rules.
        assert!(
            stderr.contains("retry") || stderr.contains("settings"),
            "a refusal must name the way back: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert!(!token.exists(), "a refused portal must leave no restore token");
        eprintln!("portal consent was refused or unanswered; the refusal path is what was proved");
        let _ = std::fs::remove_dir_all(&state);
        return;
    }

    assert!(stdout.contains("source=portal-screencast"), "{stdout}");
    assert!(stdout.contains("320x240 at (16,16)"), "{stdout}");
    assert_eq!(png_size(&state.join("dump/chibipop-capture-0.png")), (320, 240), "{stdout}");

    if !stdout.contains("cannot remember a grant") {
        assert!(token.exists(), "persist_mode=2 must leave a restore token: {stdout}");
        let stored = std::fs::read_to_string(&token).expect("reading the restore token");
        assert!(!stored.trim().is_empty());

        // The second run uses the same state directory, so it supplies the stored token.
        // The time limit makes any consent dialog fail.
        let started = std::time::Instant::now();
        let second = dump(&state, Some("portal"), "16,16,320,240");
        let elapsed = started.elapsed();
        let stdout = String::from_utf8_lossy(&second.stdout);
        eprintln!("--- second run ({elapsed:?}) ---\n{stdout}");
        assert!(second.status.success(), "the silent relaunch failed: {stdout}");
        assert!(stdout.contains("token restored"), "{stdout}");
        assert!(elapsed < Duration::from_secs(20), "a silent relaunch took {elapsed:?}");
        let rotated = std::fs::read_to_string(&token).expect("reading the rotated token");
        assert!(!rotated.trim().is_empty());
    } else {
        // A portal that cannot persist a grant must report this limit.
        // It must not create a restore token.
        // Such a token would falsely report persistence.
        assert!(stdout.contains("persist_mode needs v4"), "{stdout}");
        assert!(!token.exists(), "a v3 portal cannot have issued a token: {stdout}");
        eprintln!(
            "this portal is ScreenCast v3: the silent-relaunch half is unreachable here, and a \
             second run would only open a second dialog"
        );
    }

    let _ = std::fs::remove_dir_all(&state);
}

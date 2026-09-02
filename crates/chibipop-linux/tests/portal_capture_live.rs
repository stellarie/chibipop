//! The portal capture rung against a real session, when there is one.
//!
//! CI is headless, so everything here skips without
//! `WAYLAND_DISPLAY` - and skips again when no
//! `org.freedesktop.portal.ScreenCast` answers on the session bus,
//! because an absent rung is a rung the ladder walks past, not a
//! failure.
//!
//! The assertions that read a *frame* skip once more when this session's
//! outputs are not being repainted - an unattended dev box whose panel
//! has powered off is the ordinary case - because a copy the compositor
//! will never answer measures the display's power state and not this
//! rung. Which rung was *chosen* is asserted either way: that line is
//! printed at selection, before any frame exists, so it neither needs a
//! lit screen nor is ever evidence that capture works. See
//! [`skip_unless_painting`].
//!
//! **Nothing here opens a consent dialog by default.** A test suite
//! that puts a permission prompt on a developer's screen every time it
//! runs is a test suite people stop running, and the one dialog the
//! portal rung budgets for belongs in a launch, not in `cargo test`. So
//! the tests below exercise everything reachable without consent - the
//! ladder's choice, the override, the refusal path - and the single
//! test that really does prompt is behind
//! `CHIBIPOP_PORTAL_CONSENT_TEST=1`:
//!
//! ```text
//! CHIBIPOP_PORTAL_CONSENT_TEST=1 \
//!   cargo test -p chibipop-linux --test portal_capture_live -- --nocapture
//! ```
//!
//! That is the smoke this ticket's Comments record, and it is the only
//! way to see frames actually flow over PipeWire.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The env hook the daemon and the dump both read.
const BACKEND: &str = "CHIBIPOP_CAPTURE_BACKEND";

/// The opt-in that allows a real consent dialog.
const CONSENT_OPT_IN: &str = "CHIBIPOP_PORTAL_CONSENT_TEST";

/// The refusal a copy earns when the compositor took it and then said
/// nothing at all: neither `ready` nor `failed`, which
/// `wlr-screencopy-unstable-v1`'s `copy` request names as its only two
/// answers. The measured cause on this box is an output nothing is
/// repainting, and rung 1 here *is* that protocol -
/// `wlr_capture_live.rs`'s `UNANSWERED` pins the measurement.
const UNANSWERED: &str = "the copy went unanswered";

/// Only skip on the two honest reasons: no session, or no portal.
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

/// Ask the bus directly rather than through our own code, so a broken
/// probe cannot make these tests silently vacuous.
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

/// Whether the grab `out` records went unanswered, which makes the frame
/// it never produced a measurement of the display's power state and not
/// of this file's rung.
///
/// Mirrors `wlr_capture_live.rs`'s `skip_unless_painting`, and is narrow
/// for the same reason: only [`UNANSWERED`] skips, so a wrong size, a
/// `failed` frame or a bad format still falls through and fails the
/// assertion it guards. It reads the dump the caller already ran instead
/// of probing with a grab of its own, because down *this* file's ladder
/// a probe is not free - a session advertising no screencopy would
/// answer it from the portal rung, and the module doc budgets no dialog
/// for a default run. Duplicated rather than shared because these two
/// test binaries have no module to share: `tests/` here holds data
/// fixtures and nothing else.
fn skip_unless_painting(out: &std::process::Output) -> bool {
    let refused = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && refused.contains(UNANSWERED) {
        eprintln!("skipping: {}", refused.trim());
        return true;
    }
    false
}

/// A scratch state dir, so a test never reads or rotates the real
/// restore token.
fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("chibipop-portal-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}
/// `capture-dump` in its own XDG world: its own config, data, state and
/// cache, so nothing here touches the developer's install.
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

/// The ticket's headline selection rule, on the machine that can
/// actually contradict it: Hyprland advertises screencopy *and* runs a
/// ScreenCast portal, and the promptless rung must still win. If this
/// ever regresses, a wlr user gets a permission dialog they never had.
///
/// And the rung it prefers has to *deliver*, which is a separate claim
/// from having been chosen: the `capture:` line below is printed at
/// selection, before the copy is even requested, so a grab that then
/// produced nothing prints it just the same.
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
    // No dialog can have appeared, because the portal was never asked.
    assert!(!state.join("state/chibipop/portal-restore-token").exists());
    // Everything above reads the choice. This reads the frame, so it is
    // the one part a display nothing is repainting can honestly refuse.
    if skip_unless_painting(&out) {
        let _ = std::fs::remove_dir_all(&state);
        return;
    }
    assert!(out.status.success(), "the preferred rung grabbed nothing: {stdout}\n{stderr}");
    assert_eq!(png_size(&state.join("dump/chibipop-capture-0.png")), (8, 8), "{stdout}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The empty ladder is a status, not a crash: the daemon stays up and
/// the diagnostic names both rungs.
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
    // "Cleanly" cuts both ways: a ladder with no rung must not leave a
    // half-written frame behind pretending it had one.
    assert!(!state.join("dump/chibipop-capture-0.png").exists(), "{stdout}");
    let _ = std::fs::remove_dir_all(&state);
}

/// A bad override value is ignored with a diagnostic, never obeyed and
/// never fatal - the hook is a test hook, not a config surface.
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
    // "Ignored, never fatal" is a claim about what the ladder did next,
    // and the diagnostic above would read exactly the same if the dump
    // had died on the value: the run has to have gone on to finish the
    // grab an unset hook would have taken.
    if skip_unless_painting(&out) {
        let _ = std::fs::remove_dir_all(&state);
        return;
    }
    assert!(out.status.success(), "a bad override must not be fatal: {stdout}\n{stderr}");
    assert_eq!(png_size(&state.join("dump/chibipop-capture-0.png")), (8, 8), "{stdout}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The whole rung, end to end, dialog and all. Opt-in: see the module
/// doc.
///
/// Up to two runs. The second one proves the portal rung's "silent
/// launches after" by reusing the restore token the first rotated in -
/// but only where a restore token can exist at all. `persist_mode` is a
/// ScreenCast v4 key, and a v3 portal (xdg-desktop-portal-hyprland
/// today) cannot remember a grant, so a second run there would open a
/// second dialog and prove nothing. That case asserts the honest
/// diagnostic instead and stops.
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
        // The denial/timeout path is a first-class outcome, not a test
        // failure: a refusal is an actionable state and the app keeps
        // running. Assert *that*, and say so loudly.
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

        // Second run: same state dir, so the stored token is offered.
        // It must not prompt, which shows up as speed - a dialog cannot
        // be answered in a second.
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
        // A portal that cannot persist must say so and must not leave a
        // token behind pretending otherwise.
        assert!(stdout.contains("persist_mode needs v4"), "{stdout}");
        assert!(!token.exists(), "a v3 portal cannot have issued a token: {stdout}");
        eprintln!(
            "this portal is ScreenCast v3: the silent-relaunch half is unreachable here, and a \
             second run would only open a second dialog"
        );
    }

    let _ = std::fs::remove_dir_all(&state);
}

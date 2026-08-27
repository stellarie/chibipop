//! The data-control clipboard against a real compositor, when there is
//! one.
//!
//! CI is headless (ADR-0007), so this skips without `WAYLAND_DISPLAY` -
//! and skips again when the session advertises no data-control protocol,
//! because an absent rung is a rung the ladder walks past (spec D2), not
//! a failure. The degradation is asserted from the other side instead:
//! `chibipop clipboard-check` exits non-zero naming both globals, which
//! is what `skip` reads, and `src/clipboard.rs`'s unit tests pin the same
//! refusal with no compositor at all.
//!
//! **These tests replace the session clipboard** with a marker string.
//! There is no way to own the selection without owning it, and a test
//! that only *claimed* to would be the thing this file exists to rule
//! out.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// What another Wayland client must read back. Non-ASCII on purpose: the
/// payload a real `ocr-clipboard` copies is Japanese, so a MIME type or
/// an encoding that mangled it would still pass an ASCII assertion.
const MARKER: &str = "chibipop 日本語 clipboard 確認";

/// The line `clipboard-check` prints once the selection is ours.
const TAKEN: &str = "clipboard: selection taken";

fn skip() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return true;
    }
    // The diagnostic is the probe: it walks the same ladder the daemon
    // does and refuses with both global names when there is no rung,
    // which is a truer answer than a registry listing (a compositor may
    // advertise the global and still refuse the bind).
    let check = Command::new(BIN)
        .args(["clipboard-check", "--hold", "0", "--text", "chibipop probe"])
        .output()
        .expect("spawning chibipop clipboard-check");
    if !check.status.success() {
        eprintln!("skipping: {}", String::from_utf8_lossy(&check.stderr).trim());
        return true;
    }
    if !Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .is_ok_and(|out| out.status.success() || !out.stderr.is_empty())
    {
        eprintln!("skipping: wl-paste is not installed to read the selection back with");
        return true;
    }
    false
}

/// Take the selection and hold it, returning once the compositor has it.
///
/// The offer only answers `send` while that process lives, so the read
/// has to happen inside its window - and the window opens when the child
/// says so, never after a sleep.
fn holding() -> (Child, String) {
    let mut held = Command::new(BIN)
        .args(["clipboard-check", "--hold", "10", "--text", MARKER])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning chibipop clipboard-check");
    let lines = BufReader::new(held.stdout.take().expect("piped stdout")).lines();
    let mut said = Vec::new();
    let mut rung = None;
    for line in lines {
        let line = line.expect("reading clipboard-check's stdout");
        if let Some(tail) = line.strip_prefix("clipboard: rung ") {
            rung = tail.split_whitespace().next().map(str::to_string);
        }
        let taken = line.starts_with(TAKEN);
        said.push(line);
        if taken {
            break;
        }
    }
    assert!(
        said.iter().any(|l| l.starts_with(TAKEN)),
        "clipboard-check never took the selection: {said:#?}"
    );
    let rung = rung.expect("the rung is named before the selection is taken");
    assert!(rung.ends_with("data_control_manager_v1"), "unexpected rung {rung}");
    (held, rung)
}

/// The offer is real: a second Wayland client, in another process, reads
/// the bytes back out of it.
///
/// `wl-paste` is the reader because it is the one every user has and
/// because it is written against the *other* side of this protocol - it
/// asks for a MIME type, gets an fd and drains it. A reader of our own on
/// a second connection would be this crate agreeing with itself.
#[test]
fn another_wayland_client_reads_the_selection_the_daemon_took() {
    if skip() {
        return;
    }
    let (mut held, rung) = holding();

    let read = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain;charset=utf-8"])
        .output()
        .expect("spawning wl-paste");
    let pasted = String::from_utf8_lossy(&read.stdout).to_string();

    let _ = held.kill();
    let _ = held.wait();

    assert!(read.status.success(), "wl-paste failed: {}", String::from_utf8_lossy(&read.stderr));
    assert_eq!(
        MARKER, pasted,
        "another client must read exactly what the daemon offered (rung {rung})"
    );
}

/// The legacy X11 selection targets are offered too, so a paste into an
/// XWayland application asks by a name the offer answers.
#[test]
fn the_offer_answers_the_legacy_x11_selection_targets_as_well() {
    if skip() {
        return;
    }
    let (mut held, _rung) = holding();

    let offered = Command::new("wl-paste").arg("--list-types").output().expect("spawning wl-paste");
    let types = String::from_utf8_lossy(&offered.stdout).to_string();
    let utf8 = Command::new("wl-paste")
        .args(["--no-newline", "--type", "UTF8_STRING"])
        .output()
        .expect("spawning wl-paste");
    let pasted = String::from_utf8_lossy(&utf8.stdout).to_string();

    let _ = held.kill();
    let _ = held.wait();

    for target in ["text/plain;charset=utf-8", "text/plain", "TEXT", "STRING", "UTF8_STRING"] {
        assert!(types.lines().any(|l| l.trim() == target), "{target} was not offered: {types}");
    }
    assert_eq!(MARKER, pasted, "the X11 target must carry the same bytes");
}

//! Tests the data-control clipboard against a real compositor, when a
//! compositor exists.
//!
//! CI runs headless (ARCHITECTURE.md#packaging-and-ci). This file skips
//! when `WAYLAND_DISPLAY` is unset. It skips again when the session
//! advertises no data-control protocol. The ladder steps past an absent
//! rung, and an absent rung is not a failure. Other code asserts the
//! degraded path instead. `chibipop clipboard-check` exits non-zero and
//! names both globals, and `skip` reads that exit. The unit tests in
//! `src/clipboard.rs` pin the same refusal with no compositor at all.
//!
//! **These tests replace the session clipboard** with a marker string.
//! A test can own the selection only when it takes the selection. A
//! test that makes the claim without the take is the error this file
//! must prevent.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The bytes that another Wayland client must read back. The marker holds
/// non-ASCII text on purpose. A real `ocr-clipboard` copies Japanese
/// text. A broken MIME type or a broken encoding mangles that text and
/// still passes an ASCII assertion.
const MARKER: &str = "chibipop 日本語 clipboard 確認";

/// The line that `clipboard-check` prints after it takes the selection.
const TAKEN: &str = "clipboard: selection taken";

/// The single session selection. Both tests below must own it.
///
/// libtest runs the two tests on two threads of this process, and the
/// session holds only one selection. With an overlap, the
/// `set_selection` call of the later holder cancels the offer of the
/// earlier holder. The earlier test then reads the bytes of its sibling,
/// not its own bytes. After that sibling reaps its holder, the earlier
/// test reads an empty selection. Each test therefore owns the selection
/// for one complete take, read, and release, and then releases it to the
/// other test.
static SELECTION: Mutex<()> = Mutex::new(());

/// Own the session selection until the caller drops the returned guard.
///
/// A poisoned mutex means a sibling failed in the middle of a take. That
/// failure says nothing about the selection now. This code honors the
/// guard in both cases, so it reports a real failure once, not twice.
fn selection() -> MutexGuard<'static, ()> {
    SELECTION.lock().unwrap_or_else(PoisonError::into_inner)
}

fn skip() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return true;
    }
    // The diagnostic is the probe. It steps along the same ladder as the
    // daemon, and it refuses with both global names when it finds no
    // rung. That answer is more accurate than a registry listing,
    // because a compositor can advertise the global and still refuse the
    // bind.
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

/// Take the selection, hold it, and return after the compositor holds
/// it.
///
/// The offer answers `send` only while the holder process lives. The
/// read must occur inside that window. The child process announces the
/// start of the window. Do not use a sleep.
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

/// The offer is real. A second Wayland client in another process reads
/// the bytes out of the offer.
///
/// `wl-paste` is the reader for two reasons. Every user has it, and its
/// author wrote it against the *other* side of this protocol. It asks
/// for a MIME type, receives a file descriptor, and drains it. A reader
/// of our own on a second connection makes this crate agree with itself.
#[test]
fn another_wayland_client_reads_the_selection_the_daemon_took() {
    let _selection = selection();
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

/// The offer also carries the legacy X11 selection targets. A paste into
/// an XWayland application then asks by a name that the offer answers.
#[test]
fn the_offer_answers_the_legacy_x11_selection_targets_as_well() {
    let _selection = selection();
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

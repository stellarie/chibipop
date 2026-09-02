//! This test checks the plugin host lifecycle with a real child process.
//!
//! Windows-only: it starts the real chibipop.exe and watches process IDs with
//! Win32 process APIs. Elsewhere, this file compiles to zero tests.
#![cfg(windows)]

use chibipop_windows::plugin::{host, manifest};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Tracks one process ID across a process kill.
struct Probe(HANDLE);

impl Probe {
    fn open(pid: u32) -> Probe {
        // SAFETY: `OpenProcess` reads no pointer data.
        // It takes the pid by value, so it has no memory precondition.
        // This struct owns the returned handle and closes it exactly once in `Drop`.
        // Keep the process handle open during the identity check.
        // Windows then cannot recycle the pid.
        // Therefore, `alive` refers to this process, not a later process with the same pid.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .expect("OpenProcess on the grandchild");
        Probe(handle)
    }

    fn alive(&self) -> bool {
        let mut code = 0u32;
        // SAFETY: `self.0` stays open for the whole life of `self`.
        // It carries PROCESS_QUERY_LIMITED_INFORMATION, the access that this call needs.
        // `code` is an initialized `u32` that outlives the call.
        // The call writes exactly one `u32` through that pointer.
        unsafe { GetExitCodeProcess(self.0, &mut code) }.expect("GetExitCodeProcess");
        code == STILL_ACTIVE.0 as u32
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `OpenProcess` in `open`.
        // This struct alone owns the handle, and `Drop` runs exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn fixture(mode: &str) -> manifest::Manifest {
    let toml = format!(
        r#"
name = "echo"
version = "0.1.0"
protocol = 1
command = "{}"
args = ["plugin-echo", "{mode}"]
roles = ["text-provider"]

[text_provider]
provides_geometry = true
languages = ["ja"]
timeout_ms = 2000
"#,
        env!("CARGO_BIN_EXE_chibipop").replace('\\', "\\\\")
    );
    manifest::parse(&toml).expect("fixture manifest")
}

#[test]
fn a_clean_exchange_returns_words() {
    let m = fixture("ok");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    assert_eq!(h.ready().name, "echo");
    let v = h
        .call("text/recognise", serde_json::json!({}), Duration::from_secs(2))
        .unwrap();
    assert_eq!(v["lines"][0]["text"], "宿舎");
    h.shutdown();
}

#[test]
fn a_hang_times_out_without_killing_the_test() {
    let m = fixture("hang");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    let e = h
        .call("text/recognise", serde_json::json!({}), Duration::from_millis(150))
        .unwrap_err()
        .to_string();
    assert!(e.contains("deadline"), "{e}");
    h.shutdown();
}

#[test]
fn a_deaf_plugin_times_out_instead_of_blocking_the_writer() {
    let m = fixture("deaf");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    let big = "x".repeat(256 * 1024);
    let e = h
        .call(
            "text/recognise",
            serde_json::json!({ "image_png": big }),
            Duration::from_millis(250),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("deadline"), "{e}");
    h.shutdown();
}

#[test]
fn dropping_the_host_kills_the_grandchild_too() {
    let m = fixture("tree");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    let v = h
        .call("echo/grandchild", serde_json::json!({}), Duration::from_secs(2))
        .unwrap();
    let pid = u32::try_from(v["pid"].as_u64().expect("a grandchild pid")).unwrap();
    let probe = Probe::open(pid);
    assert!(probe.alive(), "the grandchild {pid} never started");
    drop(h);
    let until = Instant::now() + Duration::from_secs(5);
    while probe.alive() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!probe.alive(), "the grandchild {pid} outlived its host");
}

/// This check needs the host to move to a worker thread.
#[test]
fn a_host_can_still_move_between_threads() {
    fn needs_send<T: Send>() {}
    needs_send::<host::Host>();
}

#[test]
fn a_crash_is_reported_as_an_error() {
    let m = fixture("crash");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    assert!(h
        .call("text/recognise", serde_json::json!({}), Duration::from_secs(2))
        .is_err());
    h.shutdown();
}

#[test]
fn garbage_output_is_reported_as_an_error() {
    let m = fixture("garbage");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    assert!(h
        .call("text/recognise", serde_json::json!({}), Duration::from_secs(2))
        .is_err());
    h.shutdown();
}

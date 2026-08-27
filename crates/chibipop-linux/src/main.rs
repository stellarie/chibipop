// Allow-by-default lints.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

//! The Linux/Wayland platform adapter: this package *is* the adapter
//! (ADR-0001). Per-platform bins compile everywhere, run on their own OS
//! (ADR-0007): on a foreign target this bin is a two-line refusal, so
//! `cargo test --workspace` stays one command on both CI runners with no
//! cross-compilation. All Wayland/POSIX code sits behind
//! `cfg(target_os = "linux")` — the stub pulls in none of it.

#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod clipboard;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod cursor;
#[cfg(target_os = "linux")]
mod daemon;
#[cfg(target_os = "linux")]
mod entry;
#[cfg(target_os = "linux")]
mod lock;
#[cfg(target_os = "linux")]
mod logging;
#[cfg(target_os = "linux")]
mod overlay;
#[cfg(target_os = "linux")]
mod paths;
#[cfg(target_os = "linux")]
mod popup;
#[cfg(target_os = "linux")]
mod select;
#[cfg(target_os = "linux")]
mod settings;
#[cfg(target_os = "linux")]
mod shortcuts;
#[cfg(target_os = "linux")]
mod tray;
#[cfg(target_os = "linux")]
mod trigger;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
mod worker;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    entry::run()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("chibipop-linux: this binary only runs on Linux; on Windows build and run the `chibipop-windows` package");
    std::process::ExitCode::FAILURE
}

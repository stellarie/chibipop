// Allow-by-default lints.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

//! The Linux platform adapter. This package is the platform adapter.
//! See ARCHITECTURE.md#workspace-and-seams. Platform binaries compile on
//! all targets, but run on their own operating system.
//! See ARCHITECTURE.md#packaging-and-ci. On a foreign target, this
//! binary prints an error and exits. Therefore, `cargo test --workspace`
//! stays one command on both CI runners without cross-compilation.
//! All Wayland and POSIX code sits behind `cfg(target_os = "linux")`.
//! The stub includes none of that code.

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
mod catcher;
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
mod signals;
#[cfg(target_os = "linux")]
mod tray;
#[cfg(target_os = "linux")]
mod trigger;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
mod screenshot;
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

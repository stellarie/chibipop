// Allow-by-default lints.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

//! Per-platform bins compile everywhere, run on their own OS
//! (ARCHITECTURE.md#packaging-and-ci):
//! on a foreign target this bin is a two-line refusal instead of a compile
//! error, so `cargo test --workspace` / `cargo clippy --workspace` stay one
//! command on both CI runners with no cross-compilation and no member
//! juggling. All Win32 code sits behind `cfg(windows)` — the stub pulls in
//! none of it.

#[cfg(windows)]
mod entry;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    entry::run()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("chibipop-windows: this binary only runs on Windows; on Linux build and run the `chibipop-linux` package");
    std::process::ExitCode::FAILURE
}

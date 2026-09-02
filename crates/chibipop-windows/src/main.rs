// Keep these lints allowed by default.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

//! Each platform bin compiles on every target, but it runs only on its own
//! OS (ARCHITECTURE.md#packaging-and-ci). On another target, this bin
//! returns a two-line refusal instead of a compile error. This design keeps
//! `cargo test --workspace` and `cargo clippy --workspace` as one command on
//! both CI runners. CI does not cross-compile and does not exclude a crate
//! for each platform. All Win32 code sits behind `cfg(windows)`. On another
//! target, the compiler does not build that code.

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

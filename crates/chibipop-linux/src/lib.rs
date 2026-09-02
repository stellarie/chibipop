// Allow-by-default lints, to match the binary (src/main.rs).
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

//! The library face of the Linux platform adapter.
//!
//! The platform adapter is the binary. See ARCHITECTURE.md#workspace-and-seams.
//! `src/main.rs` can reach all actions of the adapter.
//! This library exists for one reason.
//! The OCR engine has a standing 152-crop fixture gate.
//! See ARCHITECTURE.md#ocr-engine.
//! Cargo cannot link an integration test against a binary target.
//! Therefore, the modules that have their own `tests/` gate stay here.
//! The binary uses these modules, and nothing else moves.
//!
//! Foreign targets get an empty crate, as the binary gets an exit message.
//! Therefore, `cargo clippy --workspace` lints every member on both CI
//! runners without pulling in the Linux stack.

#[cfg(target_os = "linux")]
pub mod ocr;

/// The decoded-surface cache of the popup.
///
/// This module lives here rather than in the `popup` tree of the binary
/// for the reason above. The module has its own tests against a database
/// that the builder wrote. Cargo cannot link a test target against a binary.
/// `#[path]` keeps the file beside the other popup files.
/// The binary reaches it as `chibipop_linux::media`.
#[cfg(target_os = "linux")]
#[path = "popup/media.rs"]
pub mod media;

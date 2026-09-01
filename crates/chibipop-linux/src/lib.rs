// Allow-by-default lints, matching the bin (src/main.rs).
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

//! The library face of the Linux adapter.
//!
//! The adapter *is* the bin (ADR-0001) and everything it does is reachable
//! from `src/main.rs`. This lib exists for one reason: the OCR engine
//! carries a standing 152-crop fixture gate (ADR-0009) and cargo cannot
//! link an integration test against a bin target. So the modules with
//! their own `tests/` gate live here and the bin uses them; nothing else
//! moves.
//!
//! Foreign targets get an empty crate, exactly as the bin gets a two-line
//! refusal: `cargo clippy --workspace` still lints every member on both CI
//! runners without pulling in the Linux stack.

#[cfg(target_os = "linux")]
pub mod ocr;

/// The popup's decoded-surface cache (ticket 03).
///
/// Here rather than in the bin's own `popup` tree for the reason above:
/// the module carries its own tests against a database the real builder
/// wrote, and cargo cannot link a test target against a bin. `#[path]`
/// keeps the file beside the rest of the popup, which is where a reader
/// looks for it. The bin reaches it as `chibipop_linux::media`.
#[cfg(target_os = "linux")]
#[path = "popup/media.rs"]
pub mod media;

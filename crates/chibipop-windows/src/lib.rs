//! The Windows platform adapter: this package *is* the adapter (ADR-0001).
//!
//! A lib target beside the bin, not a separate platform-lib crate: it exists
//! only so this package's own integration tests (`tests/rebuild.rs`,
//! `tests/geometry_goldens.rs`) can link the platform modules. Nothing else
//! links it, and nothing should.

// Allow-by-default lints.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

// Everything is Win32: on a foreign target this lib is intentionally empty
// (see main.rs — the whole package cfg-stubs so --workspace builds anywhere).
#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod lock;
#[cfg(windows)]
pub mod rebuild;
#[cfg(windows)]
pub mod text;
#[cfg(windows)]
pub mod ui;

// Core modules, imported at the root so the platform modules above keep
// addressing them as `crate::…`, unchanged by the workspace split.
#[cfg(windows)]
use chibipop::{
    anki, config, controller, dict, geom, library, lookup, paths, present, settings, update,
};

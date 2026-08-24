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

pub mod app;
pub mod input;
pub mod lock;
pub mod rebuild;
pub mod text;
pub mod ui;

// Core modules, imported at the root so the platform modules above keep
// addressing them as `crate::…`, unchanged by the workspace split.
use chibipop::{anki, config, dict, geom, library, lookup, paths, present, settings, update};

//! This package is the Windows platform adapter
//! (ARCHITECTURE.md#workspace-and-seams).
//!
//! This lib target sits beside the bin. It is not a separate
//! platform-lib crate. It exists only so this package's integration
//! tests (`tests/rebuild.rs`, `tests/geometry_goldens.rs`) can link
//! the platform modules. Other code must never link it.

// Keep these lints allowed by default.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

// This file contains only Win32 code. On another target, this lib is
// empty by design. See main.rs for the reason. The `cfg(windows)`
// attribute makes the whole package empty. This design lets
// `--workspace` commands build on every host.
#[cfg(windows)]
pub mod action;
#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod clipboard;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod lock;
#[cfg(windows)]
pub mod plugin;
#[cfg(windows)]
pub mod rebuild;
#[cfg(windows)]
pub mod text;
#[cfg(windows)]
pub mod ui;

// This file imports core modules at the crate root. The platform modules
// above can address them as `crate::…`, as they did before the workspace split.
#[cfg(windows)]
use chibipop::{
    anki, config, controller, dict, geom, image, library, lookup, paths, present, settings, shot,
    update, worker,
};

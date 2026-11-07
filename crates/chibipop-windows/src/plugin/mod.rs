//! Group the plugin host components that discover plugins, validate manifests,
//! exchange protocol messages, and track failures.
pub mod cli;
pub mod echo;
pub mod host;
pub mod proto;
pub mod strikes;
pub mod text;
pub mod version;
pub use chibipop::plugin::{discover, manifest};

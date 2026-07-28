// Allow-by-default, so silent until asked for.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod config;
pub mod geom;
pub mod input;
pub mod lookup;
pub mod paths;
pub mod present;
pub mod settings;
pub mod text;
pub mod ui;

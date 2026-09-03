// These lints use the `warn` level.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

pub mod analysis;
pub mod anki;
pub mod config;
pub mod controller;
pub mod dict;
pub mod geom;
pub mod image;
pub mod library;
pub mod lookup;
pub mod paths;
pub mod present;
pub mod select;
pub mod settings;
pub mod shot;
pub mod text;
pub mod ui;
pub mod update;
pub mod worker;

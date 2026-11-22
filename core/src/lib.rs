//! Hardware-independent logic shared by M-Pulse nodes.
//!
//! Nothing in this crate touches a peripheral or a radio, so it builds and
//! tests on an ordinary computer with `cargo test`.

pub mod actuator;
pub mod camera;
pub mod bluetti;
pub mod clock;
pub mod eco_guard;
pub mod filter;
pub mod fridge;
pub mod fuel;
pub mod heater_cutoff;
pub mod history;
pub mod protocol;
pub mod rules;
pub mod sht3x;
pub mod thermostat;
pub mod wire;

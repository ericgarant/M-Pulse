//! Hardware-independent logic shared by M-Pulse nodes.
//!
//! Nothing in this crate touches a peripheral or a radio, so it builds and
//! tests on an ordinary computer with `cargo test`.

pub mod actuator;
pub mod clock;
pub mod filter;
pub mod protocol;
pub mod thermostat;
pub mod wire;

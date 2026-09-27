//! RfiDex hardware integration: reader configuration and the bounded helper
//! protocol.
//!
//! Nothing in this crate is reachable from the app's normal command surface
//! except through the runtime's device enums.

pub mod config;
pub mod wire;

//! RfiDex hardware integration: reader configuration, the bounded helper
//! protocol and the real transports.
//!
//! Nothing in this crate is reachable from the app's normal command surface
//! except through the runtime's device enums.

pub mod config;
pub mod ec;
pub mod process;
pub mod tcp;
pub mod wire;

//! RfiDex hardware integration: reader configuration, the bounded helper
//! protocol, the real transports and the isolated vendor SDK host.
//!
//! Nothing in this crate is reachable from the app's normal command surface
//! except through the runtime's device enums. The vendor SDK is loaded only in
//! a separate child process ([`host`]); the plain TCP profile never loads a
//! DLL at all.

pub mod config;
pub mod ec;
pub mod host;
pub mod process;
pub mod sdk;
pub mod tcp;
pub mod wire;

pub use host::run_host_from_args;

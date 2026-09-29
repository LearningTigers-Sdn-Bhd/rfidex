//! RfiDex hardware integration: reader configuration, transports and adapters.
//!
//! Nothing in this crate is reachable from the app's normal command surface
//! except through the runtime's device enums. The vendor SDK is loaded only in
//! a separate child process ([`host`]); the plain TCP profile never loads a DLL
//! at all.

pub mod adapters;
pub mod config;
pub mod ec;
pub mod host;
pub mod library_gate;
pub mod process;
pub mod sdk;
pub mod tcp;
pub mod wire;

pub use adapters::{EcrfidDesk, EcrfidGate};
pub use config::{HardwareConfig, SdkConnection};
pub use host::{run_app_host_from_args, run_host_from_args};
pub use process::{HardwareClient, HostLauncher, StopControl};
pub use sdk::EnumerationKind;
pub use wire::{Operation, Reply, Request, Response, WireError, WireTag};

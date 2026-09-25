//! RfiDex runtime: all application logic behind a small, serializable API.
//!
//! Nothing here depends on Tauri. The desktop shell owns one `Runtime` and
//! renders the views and messages this crate produces.

pub mod config;

pub use config::{
    validate_connection, AppConfig, AppPaths, ConfigError, DeviceChoice, SetupInput, SetupView,
    StationConfig,
};

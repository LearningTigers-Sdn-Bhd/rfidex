//! RfiDex runtime: all application logic behind a small, serializable API.
//!
//! Nothing here depends on Tauri. The desktop shell owns one `Runtime` and
//! renders the views and messages this crate produces.

use std::fmt;

pub mod config;
pub mod desk;
pub mod devices;
pub mod runtime;

pub use config::{
    validate_connection, AppConfig, AppPaths, ConfigError, DeviceChoice, SetupInput, SetupView,
    StationConfig,
};
pub use runtime::{Runtime, RuntimeOptions};

/// An error the operator can read. Never carries a raw database or HTTP error,
/// a URL, or a server error body: the code says what happened, the message says
/// what to do.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RuntimeError {
    pub code: String,
    pub message: String,
}

impl RuntimeError {
    pub fn new(code: &str, message: &str) -> Self {
        RuntimeError {
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RuntimeError {}

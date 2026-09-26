//! RfiDex runtime: all application logic behind a small, serializable API.
//!
//! Nothing here depends on Tauri. The desktop shell owns one `Runtime` and
//! renders the views and messages this crate produces.

use std::fmt;

pub mod config;
pub mod desk;
pub mod devices;
pub mod diagnostics;
pub mod gate;
pub mod printer;
pub mod problems;
pub mod runtime;
pub mod search;

pub use config::{
    default_printer_url, is_loopback_host, validate_connection, validate_printer_url, AppConfig,
    AppPaths, ConfigError, DeviceChoice, SetupInput, SetupView, StationConfig,
};
pub use desk::{DeskStep, DeskView};
pub use diagnostics::{test_connection, ConnectionView};
pub use gate::{GateStatus, GateView};
pub use printer::{test_printer, PrinterClient};
pub use problems::ProblemView;
pub use runtime::{AppStatus, Runtime, RuntimeOptions, StationStatus};
pub use search::{SearchRow, SearchView};

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

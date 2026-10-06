//! The device a station drives: a desk session or a gate.

use std::sync::Arc;

use rfidex_core::station::gate::GateStation;

use crate::desk::DeskSession;
use crate::devices::GateDevice;

/// Behind an `Arc` so the enum stays small whatever a device holds, and so a
/// blocking worker can own its station's device without borrowing the runtime.
/// One allocation per station at startup.
pub(super) enum StationDevice {
    Desk(Arc<tokio::sync::Mutex<DeskSession>>),
    Gate(Arc<tokio::sync::Mutex<GateStation<GateDevice>>>),
}

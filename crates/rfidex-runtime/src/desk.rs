//! The desk session: which ticket is selected, what the operator sees, and the
//! sticker awaiting confirmation.
//!
//! Every message here is written in Rust so the UI only ever renders text. The
//! session is deliberately not persisted: a half-finished scan is not work, and
//! the durable part of a desk action is the outbox row the core already queues.

use rfidex_core::contract::{RfidMode, TicketSummary};
use rfidex_core::station::desk::DeskStation;
use uuid::Uuid;

use crate::devices::DeskDevice;

pub const READY_MESSAGE: &str = "Scan a ticket to begin.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeskStep {
    Ready,
    Scanned,
    Linked,
    NeedsConfirm,
    Error,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeskView {
    pub step: DeskStep,
    pub code: Option<String>,
    pub message: String,
    pub ticket: Option<TicketSummary>,
    pub offline: bool,
    pub mode: RfidMode,
}

impl DeskView {
    pub fn ready(mode: RfidMode) -> DeskView {
        DeskView {
            step: DeskStep::Ready,
            code: None,
            message: READY_MESSAGE.to_string(),
            ticket: None,
            offline: false,
            mode,
        }
    }

    pub fn error(mode: RfidMode, code: &str, message: &str) -> DeskView {
        DeskView {
            step: DeskStep::Error,
            code: Some(code.to_string()),
            message: message.to_string(),
            ticket: None,
            offline: false,
            mode,
        }
    }
}

/// A warning the operator has been shown and has not yet answered. Confirming
/// authorises one specific sticker for one specific ticket; if either changes,
/// the authorisation is dropped and the new sticker is checked from scratch.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingConfirm {
    pub uid_raw_hex: String,
    pub ticket: Uuid,
    pub warning: rfidex_core::station::desk::Warning,
    pub message: String,
}

pub struct DeskSession {
    pub station: DeskStation<DeskDevice>,
    pub view: DeskView,
    pub pending: Option<PendingConfirm>,
}

impl DeskSession {
    pub fn new(station: DeskStation<DeskDevice>, mode: RfidMode) -> DeskSession {
        DeskSession {
            station,
            view: DeskView::ready(mode),
            pending: None,
        }
    }
}

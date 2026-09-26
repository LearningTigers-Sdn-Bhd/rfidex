//! The desk session: which ticket is selected, what the operator sees, and the
//! sticker awaiting confirmation.
//!
//! Every message here is written in Rust so the UI only ever renders text. The
//! session is deliberately not persisted: a half-finished scan is not work, and
//! the durable part of a desk action is the outbox row the core already queues.
//!
//! Failures that are the operator's business come back as `Ok(DeskView)` with
//! `DeskError`-free wording. Only a wrong station or a broken device selection
//! is a `RuntimeError`, because that is not something the desk screen can fix.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use chrono::Local;
use rfidex_core::contract::{CheckInResult, RfidMode, TicketSummary};
use rfidex_core::device::DeviceError;
use rfidex_core::station::desk::{Confirm, DeskError, DeskStation, Scanned, Warning};
use rfidex_core::store::Store;
use rfidex_core::tag::hex_upper;
use uuid::Uuid;

use crate::devices::DeskDevice;

pub const READY_MESSAGE: &str = "Scan a ticket to begin.";
pub const TAP_MESSAGE: &str = "Place one sticker on the reader.";
pub const LINKED_MESSAGE: &str = "Sticker linked.";
pub const SCAN_FIRST_MESSAGE: &str = "Scan a ticket first.";
pub const CONNECT_FIRST_MESSAGE: &str = "Connect to the server once before using this station.";
pub const PRINTING_MESSAGE: &str = "Printing badge…";
pub const PRINTED_MESSAGE: &str = "Badge sent to printer";
pub const PRINT_FAILED_MESSAGE: &str = "Badge not printed — press Reprint";
pub const OFFLINE_PRINT_MESSAGE: &str = "Offline — press Reprint when back online";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeskStep {
    Ready,
    Scanned,
    Linked,
    NeedsConfirm,
    Error,
}

/// What the desk knows about the guest's badge. The printing itself is a
/// separate command, so the sticker step never waits for a printer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BadgeView {
    /// Rust decided this scan should print now: the UI calls `desk_print` once.
    pub print_now: bool,
    pub can_reprint: bool,
    /// Keep the guest on screen (no 3 s auto-reset) until staff act.
    pub hold: bool,
    pub message: Option<String>,
}

impl BadgeView {
    /// Nothing to do with a badge yet: used before any scan.
    fn idle() -> BadgeView {
        BadgeView {
            print_now: false,
            can_reprint: false,
            hold: false,
            message: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeskView {
    pub step: DeskStep,
    pub code: Option<String>,
    pub message: String,
    pub ticket: Option<TicketSummary>,
    pub offline: bool,
    pub mode: RfidMode,
    /// Identifies the guest on screen. A print result is only allowed to land
    /// on the session that asked for it.
    pub session_id: Uuid,
    pub badge: BadgeView,
}

impl DeskView {
    pub fn ready(mode: RfidMode) -> DeskView {
        // The session fills in its own id and badge in `DeskSession::show`.
        DeskView {
            step: DeskStep::Ready,
            code: None,
            message: READY_MESSAGE.to_string(),
            ticket: None,
            offline: false,
            mode,
            session_id: Uuid::nil(),
            badge: BadgeView::idle(),
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
            session_id: Uuid::nil(),
            badge: BadgeView::idle(),
        }
    }
}

/// The badge state for a scan that just happened, straight from the server's
/// own answer. Printing is offered only for a first check-in: a guest who was
/// already in needs a sticker, not necessarily a second badge.
fn badge_for_scan(scanned: &Scanned) -> BadgeView {
    match &scanned.check_in {
        Some(check_in) if check_in.result == CheckInResult::CheckedIn => BadgeView {
            print_now: true,
            can_reprint: false,
            hold: true,
            message: Some(PRINTING_MESSAGE.to_string()),
        },
        Some(check_in) => BadgeView {
            print_now: false,
            can_reprint: true,
            hold: true,
            message: Some(format!(
                "Already checked in at {}",
                crate::search::clock(check_in.checked_in_at, &Local)
            )),
        },
        None => BadgeView {
            print_now: false,
            can_reprint: true,
            hold: true,
            message: Some(OFFLINE_PRINT_MESSAGE.to_string()),
        },
    }
}

/// A warning the operator has been shown and has not yet answered. Confirming
/// authorises one specific sticker for one specific ticket; if either changes,
/// the authorisation is dropped and the new sticker is checked from scratch.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingConfirm {
    pub uid_raw_hex: String,
    pub ticket: Uuid,
    pub warning: Warning,
    pub message: String,
}

pub struct DeskSession {
    pub station: DeskStation<DeskDevice>,
    pub view: DeskView,
    /// The attendee this session is working on, kept until reset.
    pub ticket: Option<TicketSummary>,
    pub pending: Option<PendingConfirm>,
    /// The UID rule already applied to this station, so a heartbeat that
    /// changes nothing does not throw away a half-answered warning.
    pub uid_rule: rfidex_core::tag::UidRule,
    /// New on every scan and every reset, so a late print answer can never be
    /// shown against a different guest.
    pub session_id: Uuid,
    pub badge: BadgeView,
    /// One print in flight per desk: a second press joins the one running
    /// instead of sending a second job.
    pub printing: Arc<AtomicBool>,
}

impl DeskSession {
    pub fn new(
        station: DeskStation<DeskDevice>,
        mode: RfidMode,
        uid_rule: rfidex_core::tag::UidRule,
    ) -> DeskSession {
        let session_id = Uuid::new_v4();
        let badge = BadgeView::idle();
        DeskSession {
            station,
            view: DeskView::ready(mode),
            ticket: None,
            pending: None,
            uid_rule,
            session_id,
            badge,
            printing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Apply settings from a heartbeat. A confirmation checked against the old
    /// mode or UID rule must not carry over to a new one, but an unchanged
    /// heartbeat leaves the session exactly as it was.
    pub fn apply_settings(&mut self, mode: RfidMode, uid_rule: rfidex_core::tag::UidRule) {
        let changed = self.station.mode() != mode || self.uid_rule != uid_rule;
        self.station.configure(mode, uid_rule);
        self.view.mode = mode;
        self.uid_rule = uid_rule;
        if changed {
            self.pending = None;
        }
    }

    /// A new guest on screen: a new session id, so nothing in flight for the
    /// old one can land on this one.
    pub fn new_session(&mut self) {
        self.session_id = Uuid::new_v4();
        // A fresh flag, not a reset one: a print still running for the old
        // guest must not clear the new guest's in-flight flag when it ends.
        self.printing = Arc::new(AtomicBool::new(false));
    }

    pub fn set_badge(&mut self, badge: BadgeView) {
        self.badge = badge;
        self.view.badge = self.badge.clone();
    }

    fn show(&mut self, mut view: DeskView) -> DeskView {
        view.session_id = self.session_id;
        view.badge = self.badge.clone();
        self.view = view;
        self.view.clone()
    }

    fn error(&mut self, code: &str, message: &str) -> DeskView {
        let mode = self.station.mode();
        let view = DeskView {
            step: DeskStep::Error,
            code: Some(code.to_string()),
            message: message.to_string(),
            ticket: self.ticket.clone(),
            offline: false,
            mode,
            session_id: self.session_id,
            badge: self.badge.clone(),
        };
        self.show(view)
    }

    /// Refuse to work until the station has heard from the server once. Without
    /// settings it does not know the event's mode or UID rule, and guessing
    /// would write the wrong thing.
    pub fn connect_first(&mut self) -> DeskView {
        let mode = self.station.mode();
        self.ticket = None;
        self.pending = None;
        self.new_session();
        self.show(DeskView::error(
            mode,
            "connect_first",
            CONNECT_FIRST_MESSAGE,
        ))
    }
}

/// Forget the attendee and the half-answered warning. Nothing was written.
pub fn reset(session: &mut DeskSession) -> DeskView {
    session.ticket = None;
    session.pending = None;
    session.new_session();
    session.set_badge(BadgeView::idle());
    let mode = session.station.mode();
    session.show(DeskView::ready(mode))
}

/// Resolve the scanned code to a ticket and forget whoever was here before, so a
/// failed scan can never be linked to the previous attendee.
pub async fn scan(session: &mut DeskSession, store: &Mutex<Store>, code: &str) -> DeskView {
    session.ticket = None;
    session.pending = None;
    session.new_session();
    let mode = session.station.mode();
    match session.station.scan_ticket(code).await {
        Ok(scanned) => {
            session.ticket = Some(scanned.ticket.clone());
            session.set_badge(badge_for_scan(&scanned));
            session.show(DeskView {
                step: DeskStep::Scanned,
                code: None,
                message: TAP_MESSAGE.to_string(),
                ticket: Some(scanned.ticket),
                offline: scanned.offline,
                mode,
                session_id: session.session_id,
                badge: session.badge.clone(),
            })
        }
        Err(e) => {
            session.set_badge(BadgeView::idle());
            let (code, message) = failure(&e, store);
            session.error(&code, &message)
        }
    }
}

/// Link the selected ticket to the sticker on the reader, or say why not.
pub async fn link(
    session: &mut DeskSession,
    store: &Mutex<Store>,
    reason: Option<String>,
) -> DeskView {
    if session.view.step == DeskStep::Linked {
        // A repeat press of Link must not create a second binding or write again.
        return session.view.clone();
    }
    let mode = session.station.mode();
    let Some(ticket) = session.ticket.clone() else {
        return session.error("scan_first", SCAN_FIRST_MESSAGE);
    };

    let tag = match session.station.detect_tag() {
        Ok(tag) => tag,
        // Keep the ticket: the operator only has to place a sticker.
        Err(e) => {
            let (code, message) = failure(&e, store);
            return session.error(&code, &message);
        }
    };
    let uid_hex = hex_upper(&tag.uid_raw);

    let confirm = match &session.pending {
        Some(pending) if pending.uid_raw_hex == uid_hex && pending.ticket == ticket.public_id => {
            let Some(reason) = reason.filter(|r| !r.trim().is_empty()) else {
                return session.show(DeskView {
                    step: DeskStep::NeedsConfirm,
                    code: Some("reason_required".to_string()),
                    message: pending.message.clone(),
                    ticket: Some(ticket),
                    offline: false,
                    mode,
                    session_id: session.session_id,
                    badge: session.badge.clone(),
                });
            };
            Some(Confirm { reason })
        }
        // A different sticker, or no warning at all: an unsolicited reason must
        // never stand in for a confirmation the operator has not seen.
        Some(_) => {
            session.pending = None;
            None
        }
        None => None,
    };

    match session.station.link(&ticket, &tag, confirm).await {
        Ok(linked) => {
            session.pending = None;
            session.show(DeskView {
                step: DeskStep::Linked,
                code: None,
                message: LINKED_MESSAGE.to_string(),
                ticket: Some(ticket),
                offline: linked.offline,
                mode,
                session_id: session.session_id,
                badge: session.badge.clone(),
            })
        }
        Err(DeskError::NeedsConfirm(warning)) => {
            let (code, message) = warning_text(&warning, store);
            session.pending = Some(PendingConfirm {
                uid_raw_hex: uid_hex,
                ticket: ticket.public_id,
                warning,
                message: message.clone(),
            });
            session.show(DeskView {
                step: DeskStep::NeedsConfirm,
                code: Some(code),
                message,
                ticket: Some(ticket),
                offline: false,
                mode,
                session_id: session.session_id,
                badge: session.badge.clone(),
            })
        }
        Err(e) => {
            session.pending = None;
            let (code, message) = failure(&e, store);
            session.error(&code, &message)
        }
    }
}

/// What the operator sees for a sticker that is already in use. The wording of
/// a warning is the plan's operator contract, not a prettified error.
fn warning_text(warning: &Warning, store: &Mutex<Store>) -> (String, String) {
    match warning {
        Warning::StickerBoundElsewhere { holder } => {
            let who = holder
                .as_ref()
                .map(|h| h.name.clone())
                .unwrap_or_else(|| "another attendee".to_string());
            (
                "sticker_in_use".to_string(),
                format!("This sticker belongs to {who}. Replace its link?"),
            )
        }
        Warning::TicketHasSticker => (
            "ticket_has_sticker".to_string(),
            "This ticket already has a sticker. Replace it?".to_string(),
        ),
        Warning::StickerHasOtherPayload { public_id } => {
            let holder = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .ticket(*public_id)
                .ok()
                .flatten();
            let message = match holder {
                Some(t) => format!(
                    "This sticker holds another ticket ({}). Replace it?",
                    t.name
                ),
                None => "This sticker holds another ticket. Replace it?".to_string(),
            };
            ("sticker_has_ticket".to_string(), message)
        }
        Warning::StickerHasUnknownData => (
            "sticker_has_data".to_string(),
            "This sticker already holds data. Replace it?".to_string(),
        ),
    }
}

/// The operator message for every desk failure. `DeskError`'s own text carries
/// raw API errors and device detail, so it is never shown.
fn failure(e: &DeskError, store: &Mutex<Store>) -> (String, String) {
    let (code, message) = match e {
        DeskError::NoTag | DeskError::Device(DeviceError::TagNotFound) => {
            ("no_tag", TAP_MESSAGE)
        }
        DeskError::MultipleTags(_) => ("multiple_tags", "Keep only one sticker near the reader."),
        DeskError::TicketNotFound => (
            "ticket_not_found",
            "Ticket not found. Scan the ticket code again.",
        ),
        DeskError::TicketUnpaid => ("ticket_unpaid", "This ticket has not been paid."),
        DeskError::TicketCancelled => ("ticket_cancelled", "This ticket has been cancelled."),
        DeskError::TicketInvalid => ("ticket_invalid", "This ticket cannot be used."),
        DeskError::PayloadTooLarge { .. } => (
            "sticker_too_small",
            "This sticker cannot hold the ticket. Use another sticker or ask the event administrator to use Bind mode.",
        ),
        DeskError::WriteUnsupported | DeskError::Device(DeviceError::WriteUnsupported) => {
            ("write_unsupported", "This reader cannot write stickers.")
        }
        DeskError::WriteVerifyFailed => ("write_failed", "Writing failed. Use another sticker."),
        DeskError::ReasonRequired => (
            "reason_required",
            "Enter a reason before replacing the sticker.",
        ),
        DeskError::Device(DeviceError::Disconnected) => (
            "reader_disconnected",
            "Reader disconnected. Reconnect it and try again.",
        ),
        DeskError::Device(DeviceError::OutOfRange) => (
            "sticker_memory",
            "This sticker cannot be written at the selected location.",
        ),
        DeskError::Device(DeviceError::Other(_)) => (
            "reader_error",
            "The reader could not finish. Check the sticker and try again.",
        ),
        DeskError::Api(rfidex_core::client::ApiError::Unauthorized) => (
            "unauthorized",
            "The server did not accept the API key. Open Setup to check it.",
        ),
        DeskError::Api(rfidex_core::client::ApiError::BadResponse(_)) => (
            "server_reply",
            "The server sent a reply this app cannot read. Ask for help.",
        ),
        DeskError::Api(rfidex_core::client::ApiError::Retryable(_)) => (
            "offline",
            "Cannot reach the server. Try again when the connection returns.",
        ),
        DeskError::Api(rfidex_core::client::ApiError::Rejected { .. }) => (
            "server_rejected",
            "The server could not accept this action. Check the ticket and try again.",
        ),
        DeskError::Store(_) => (
            "save_failed",
            "Could not save this action on this computer. Stop and ask for help.",
        ),
        DeskError::NeedsConfirm(warning) => return warning_text(warning, store),
    };
    (code.to_string(), message.to_string())
}

//! Gate results for the operator: the newest passage first, with a status, a
//! name and a message that are all decided here rather than in the UI.

use chrono::{DateTime, Utc};
use rfidex_core::codec;
use rfidex_core::contract::{
    ErrorBody, ErrorCode, ObservationItem, ObservationResult, Outcome, Role,
};
use rfidex_core::store::{OutboxRow, OutboxState, Store};
use rfidex_core::tag::{parse_hex, tag_key, UidRule};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    /// Saved on this computer, not yet accepted by the server.
    Recorded,
    Accepted,
    Denied,
    Problem,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateView {
    pub id: i64,
    pub captured_at: DateTime<Utc>,
    pub status: GateStatus,
    pub role: Role,
    pub name: Option<String>,
    pub message: String,
    pub anomalies: Vec<String>,
}

pub const UNREADABLE_ROW: &str = "This saved action cannot be read. Ask for help.";
pub const NEEDS_HELP: &str = "This saved action needs help before it can be sent.";

/// Turn one durable outbox row into what the gate screen shows. The saved
/// request's own role is used, so an old passage stays truthful after the
/// station direction is edited.
pub fn view(row: &OutboxRow, store: &Store, uid_rule: UidRule) -> GateView {
    let item: Option<ObservationItem> = serde_json::from_value(row.item.payload.clone()).ok();
    let result: Option<ObservationResult> = row
        .result
        .clone()
        .and_then(|value| serde_json::from_value(value).ok());
    let mut view = GateView {
        id: row.item.id,
        captured_at: row.item.captured_at,
        status: GateStatus::Problem,
        role: item.as_ref().map(|i| i.role).unwrap_or(Role::Entry),
        name: None,
        message: String::new(),
        anomalies: Vec::new(),
    };
    if item.is_none() {
        // A row written by another version is still shown, just without detail.
        view.message = UNREADABLE_ROW.to_string();
        return view;
    }
    let display_name = result
        .as_ref()
        .and_then(|r| r.display.name.as_deref())
        .map(str::to_string);
    view.name =
        crate::problems::resolve_name(&row.item.payload, display_name.as_deref(), store, uid_rule);

    match row.state {
        OutboxState::Pending => {
            view.status = GateStatus::Recorded;
            view.message = "Recorded — waiting for the server.".to_string();
        }
        OutboxState::Sent => match &result {
            Some(res) => {
                view.anomalies = anomaly_text(&res.anomalies);
                match res.outcome {
                    Outcome::Accepted => {
                        view.status = GateStatus::Accepted;
                        view.message = match view.role {
                            Role::Entry => "Welcome",
                            Role::Exit => "Goodbye",
                        }
                        .to_string();
                    }
                    denied => {
                        view.status = GateStatus::Denied;
                        view.message = denial(denied);
                    }
                }
            }
            None => {
                view.status = GateStatus::Problem;
                view.message = UNREADABLE_ROW.to_string();
            }
        },
        OutboxState::Conflict => {
            view.status = GateStatus::Problem;
            view.message = conflict_message(row);
        }
        OutboxState::Parked => {
            view.status = GateStatus::Problem;
            view.message = parked_message(row);
        }
        OutboxState::Dismissed => {
            view.status = GateStatus::Problem;
            view.message = "Dismissed — this saved action still has a problem.".to_string();
        }
    }
    view
}

/// Server warnings stay warnings: the passage is still accepted, but the
/// operator is told in words rather than by an enum name.
fn anomaly_text(anomalies: &[String]) -> Vec<String> {
    anomalies
        .iter()
        .map(|a| match a.as_str() {
            "role_mismatch" => {
                "The reader reported a different direction. The station direction was used."
                    .to_string()
            }
            "entered_without_check_in" => {
                "This attendee has not checked in at registration.".to_string()
            }
            "payload_binding_mismatch" => {
                "The ticket written on this sticker does not match its link.".to_string()
            }
            _ => "The server reported a warning. Ask for help.".to_string(),
        })
        .collect()
}

fn denial(outcome: Outcome) -> String {
    match outcome {
        Outcome::UnknownTag => "Sticker not recognised.",
        Outcome::RevokedTag => "This sticker has been replaced.",
        Outcome::WrongEvent => "This sticker belongs to another event.",
        Outcome::TicketInvalid => "This ticket cannot be used.",
        Outcome::NotCheckedIn => "Check in at registration first.",
        Outcome::PossibleDuplicate => "This reader record was already received.",
        // Accepted is handled before this is reached, and must never read as a
        // welcome.
        Outcome::Accepted => NEEDS_HELP,
    }
    .to_string()
}

pub fn conflict_message(row: &OutboxRow) -> String {
    row.result
        .clone()
        .and_then(|value| serde_json::from_value::<ErrorBody>(value).ok())
        .map(|body| code_message(body.error))
        .unwrap_or_else(|| NEEDS_HELP.to_string())
}

pub fn parked_message(row: &OutboxRow) -> String {
    let text = row.last_error.as_deref().unwrap_or_default();
    if text.starts_with("undecodable payload") {
        UNREADABLE_ROW.to_string()
    } else if text.starts_with("unreadable server reply") {
        "The server sent a reply this app cannot read. Ask for help.".to_string()
    } else {
        // A parked row keeps the server's own free text, which is not fit to
        // show: only the known failure classes above are recognisable.
        NEEDS_HELP.to_string()
    }
}

pub fn code_message(code: ErrorCode) -> String {
    match code {
        ErrorCode::UidBoundElsewhere => "This sticker is already linked to another attendee.",
        ErrorCode::TicketHasSticker => "This ticket already has another sticker.",
        ErrorCode::ReasonRequired => "A reason is needed before replacing a sticker.",
        ErrorCode::TicketNotFound => "The server does not have this ticket.",
        ErrorCode::TicketUnpaid => "This ticket has not been paid.",
        ErrorCode::TicketCancelled => "This ticket has been cancelled.",
        ErrorCode::Unauthorized => "The server did not accept the API key.",
        ErrorCode::BatchTooLarge | ErrorCode::Malformed => NEEDS_HELP,
    }
    .to_string()
}

/// Best name for a row: the server's own display name first, then the station's
/// binding cache, and only then the ticket id written on the sticker. A written
/// payload never overrides a server name.
pub fn resolve_name(
    payload: &serde_json::Value,
    display_name: Option<&str>,
    store: &Store,
    uid_rule: UidRule,
) -> Option<String> {
    if let Some(name) = display_name {
        return Some(name.to_string());
    }
    if let Some(pid) = payload
        .get("public_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        if let Some(ticket) = store.ticket(pid).ok().flatten() {
            return Some(ticket.name);
        }
    }
    if let Some(uid_hex) = payload.get("uid_raw_hex").and_then(|v| v.as_str()) {
        if let Ok(raw) = parse_hex(uid_hex) {
            let key = tag_key(&raw, uid_rule);
            if let Some(pid) = store.binding_holder(&key).ok().flatten() {
                if let Some(ticket) = store.ticket(pid).ok().flatten() {
                    return Some(ticket.name);
                }
            }
        }
    }
    let payload_id = payload
        .get("payload_hex")
        .and_then(|v| v.as_str())
        .and_then(|hex| parse_hex(hex).ok())
        .and_then(|bytes| codec::decode(&bytes).ok());
    payload_id
        .and_then(|pid| store.ticket(pid).ok().flatten())
        .map(|ticket| ticket.name)
}

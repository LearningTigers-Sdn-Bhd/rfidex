//! Privacy-safe diagnostics: an allowlisted CSV of the outbox, and a
//! side-effect-free connection check for Setup.
//!
//! The export is built from named columns rather than by redacting a dump, so
//! attendee names, ticket ids, raw UIDs and server replies cannot leak by
//! accident: they are never read in the first place.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rfidex_core::client::{ApiClient, ApiError};
use rfidex_core::contract::{ErrorBody, ErrorCode, ObservationResult, Outcome};
use rfidex_core::store::{OutboxRow, OutboxState};
use rfidex_core::tag::{hex_upper, parse_hex};
use serde_json::Value;
use uuid::Uuid;

use crate::config::{validate_connection, ConfigError};
use crate::runtime::StationRuntime;
use crate::RuntimeError;

const HEADER: &str =
    "station_id,row_id,kind,state,captured_at,attempts,uid_last4,outcome,problem_code";

const ALL_STATES: [OutboxState; 5] = [
    OutboxState::Pending,
    OutboxState::Sent,
    OutboxState::Conflict,
    OutboxState::Parked,
    OutboxState::Dismissed,
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConnectionView {
    pub ok: bool,
    pub code: String,
    pub message: String,
}

fn view(ok: bool, code: &str, message: &str) -> ConnectionView {
    ConnectionView {
        ok,
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// Check an address and key by reading one binding lookup, which changes
/// nothing on the server: no heartbeat, no scan, no binding, no observation.
pub async fn test_connection(url: &str, key: &str) -> ConnectionView {
    if let Err(e) = validate_connection(url, key) {
        let message = match e {
            ConfigError::Invalid(message) => message,
            other => other.to_string(),
        };
        return view(false, "invalid_setup", &message);
    }
    let client = ApiClient::new(
        url.trim().trim_end_matches('/'),
        key,
        &Uuid::new_v4().to_string(),
        Duration::from_secs(5),
    );
    match client.lookup("00").await {
        Ok(_) => view(true, "connected", "Connection works."),
        Err(ApiError::Unauthorized) => view(
            false,
            "unauthorized",
            "The server did not accept the API key.",
        ),
        Err(ApiError::Retryable(_)) => view(
            false,
            "offline",
            "Cannot reach the server. Check the address and connection.",
        ),
        Err(ApiError::BadResponse(_)) => view(
            false,
            "server_reply",
            "The server reply does not match this app.",
        ),
        Err(ApiError::Rejected { .. }) => view(
            false,
            "server_rejected",
            "The server could not check the connection.",
        ),
    }
}

impl crate::Runtime {
    /// Write an allowlisted CSV of the outbox and return its path. The file is
    /// created fresh, so an export can never overwrite an earlier one.
    pub async fn export_diagnostics(&self) -> Result<PathBuf, RuntimeError> {
        // Collect everything before touching the file, so no station lock is
        // held while writing.
        let mut rows: Vec<(Uuid, OutboxRow)> = Vec::new();
        for station in self.stations() {
            let collected = collect(station)?;
            for row in collected {
                rows.push((station.id(), row));
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.item.id.cmp(&b.1.item.id)));

        let dir = self.paths().exports();
        std::fs::create_dir_all(&dir).map_err(|_| export_failure())?;
        let path = dir.join(format!(
            "diagnostics-{}-{}.csv",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            Uuid::new_v4()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| export_failure())?;

        let write = (|| -> std::io::Result<()> {
            write!(file, "{HEADER}\r\n")?;
            for (station_id, row) in &rows {
                file.write_all(
                    format!(
                        "{},{},{},{},{},{},{},{},{}\r\n",
                        csv_cell(&station_id.to_string()),
                        csv_cell(&row.item.id.to_string()),
                        csv_cell(row.item.kind.as_str()),
                        csv_cell(row.state.as_str()),
                        csv_cell(&row.item.captured_at.to_rfc3339()),
                        csv_cell(&row.item.attempts.to_string()),
                        csv_cell(&uid_last4(&row.item.payload)),
                        csv_cell(&outcome_of(row)),
                        csv_cell(&problem_code_of(row)),
                    )
                    .as_bytes(),
                )?;
            }
            file.flush()?;
            Ok(())
        })();

        match write {
            Ok(()) => Ok(path),
            Err(_) => {
                drop(file);
                let _ = std::fs::remove_file(&path);
                Err(export_failure())
            }
        }
    }
}

fn collect(station: &Arc<StationRuntime>) -> Result<Vec<OutboxRow>, RuntimeError> {
    station
        .outbox_rows(&ALL_STATES, None, usize::MAX)
        .map_err(|_| export_failure())
}

fn export_failure() -> RuntimeError {
    RuntimeError::new(
        "export_failed",
        "Could not write the diagnostics file. Check the disk and try again.",
    )
}

/// Only the last four characters of a sticker UID, and only when the stored
/// value is really hex. Anything else is blank: an unvalidated value is never
/// echoed back into a file that leaves the machine.
fn uid_last4(payload: &Value) -> String {
    let Some(hex) = payload.get("uid_raw_hex").and_then(|v| v.as_str()) else {
        return String::new();
    };
    let Ok(raw) = parse_hex(hex) else {
        return String::new();
    };
    let canonical = hex_upper(&raw);
    let start = canonical.len().saturating_sub(4);
    canonical[start..].to_string()
}

/// A server outcome, from the contract's own allowlist. A reply this app cannot
/// read is reported as such rather than quoted.
fn outcome_of(row: &OutboxRow) -> String {
    row.result
        .clone()
        .and_then(|value| serde_json::from_value::<ObservationResult>(value).ok())
        .map(|result| outcome_name(result.outcome).to_string())
        .unwrap_or_else(|| "unreadable".to_string())
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Accepted => "accepted",
        Outcome::UnknownTag => "unknown_tag",
        Outcome::RevokedTag => "revoked_tag",
        Outcome::WrongEvent => "wrong_event",
        Outcome::TicketInvalid => "ticket_invalid",
        Outcome::NotCheckedIn => "not_checked_in",
        Outcome::PossibleDuplicate => "possible_duplicate",
    }
}

fn problem_code_of(row: &OutboxRow) -> String {
    if let Some(code) = row
        .result
        .clone()
        .and_then(|value| serde_json::from_value::<ErrorBody>(value).ok())
        .map(|body| code_name(body.error))
    {
        return code.to_string();
    }
    match row.state {
        OutboxState::Conflict | OutboxState::Parked | OutboxState::Dismissed => {
            "problem".to_string()
        }
        _ => String::new(),
    }
}

fn code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::TicketNotFound => "ticket_not_found",
        ErrorCode::TicketUnpaid => "ticket_unpaid",
        ErrorCode::TicketCancelled => "ticket_cancelled",
        ErrorCode::UidBoundElsewhere => "uid_bound_elsewhere",
        ErrorCode::TicketHasSticker => "ticket_has_sticker",
        ErrorCode::ReasonRequired => "reason_required",
        ErrorCode::BatchTooLarge => "batch_too_large",
        ErrorCode::Malformed => "malformed",
    }
}

/// Quote every field, double internal quotes, and stop a spreadsheet treating
/// a leading `=`, `+`, `-` or `@` as a formula. Quoting alone is not a formula
/// defence, so a leading apostrophe is added after any leading whitespace too.
fn csv_cell(value: &str) -> String {
    let first = value
        .trim_start_matches(|c: char| c.is_whitespace())
        .chars()
        .next();
    let formula = matches!(first, Some('=' | '+' | '-' | '@'));
    let escaped = value.replace('"', "\"\"");
    format!("\"{}{}\"", if formula { "'" } else { "" }, escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cell_quotes_and_defuses_formulas() {
        assert_eq!(csv_cell("plain"), "\"plain\"");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("one\r\ntwo"), "\"one\r\ntwo\"");
        assert_eq!(csv_cell("one\ntwo"), "\"one\ntwo\"");
        for starter in ['=', '+', '-', '@'] {
            assert_eq!(
                csv_cell(&format!("{starter}cmd|' /C calc'!A0")),
                format!("\"'{starter}cmd|' /C calc'!A0\"")
            );
        }
        assert_eq!(csv_cell("  =cmd"), "\"'  =cmd\"");
        assert_eq!(csv_cell("\t+cmd"), "\"'\t+cmd\"");
        assert_eq!(csv_cell(""), "\"\"");
    }

    #[test]
    fn uid_last4_needs_real_hex() {
        assert_eq!(
            uid_last4(&serde_json::json!({"uid_raw_hex": "3412CDAB500104E0"})),
            "04E0"
        );
        assert_eq!(uid_last4(&serde_json::json!({"uid_raw_hex": "9A"})), "9A");
        assert_eq!(
            uid_last4(&serde_json::json!({"uid_raw_hex": "3412cdab500104e0"})),
            "04E0"
        );
        assert_eq!(
            uid_last4(&serde_json::json!({"uid_raw_hex": "not hex"})),
            ""
        );
        assert_eq!(uid_last4(&serde_json::json!({"uid_raw_hex": "E00"})), "");
        assert_eq!(uid_last4(&serde_json::json!({"uid_raw_hex": ""})), "");
        assert_eq!(uid_last4(&serde_json::json!({"uid_raw_hex": 7})), "");
        assert_eq!(
            uid_last4(&serde_json::json!({"tag_key": "3412CDAB500104E0"})),
            ""
        );
        assert_eq!(uid_last4(&serde_json::json!(null)), "");
    }
}

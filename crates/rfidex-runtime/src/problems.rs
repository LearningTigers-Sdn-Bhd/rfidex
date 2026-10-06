//! Cross-station problems: the rows that need a person, gathered from every
//! configured station.
//!
//! A row id is local to one station's database, so an action always carries the
//! station UUID as well. Dismissing hides a row from this list; it does not
//! delete it, retry it, or claim the server agreed.

use chrono::{DateTime, Utc};
use rfidex_core::contract::{ErrorBody, ObservationResult};
use rfidex_core::store::{OutboxRow, OutboxState, Store};
use uuid::Uuid;

pub use crate::gate::{conflict_message, parked_message, resolve_name};

use crate::diagnostics::csv_cell;
use crate::RuntimeError;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProblemView {
    pub station_id: Uuid,
    pub station_name: String,
    pub id: i64,
    pub captured_at: DateTime<Utc>,
    pub name: Option<String>,
    pub message: String,
}

/// The rows a person still has to deal with: a conflict the server refused, or
/// a parked row the app could not send. Dismissed rows are no longer listed.
pub fn for_station(
    station_id: Uuid,
    station_name: &str,
    store: &Store,
    uid_rule: rfidex_core::tag::UidRule,
) -> Result<Vec<ProblemView>, RuntimeError> {
    let rows = store
        .rows(
            &[OutboxState::Conflict, OutboxState::Parked],
            None,
            usize::MAX,
        )
        .map_err(|_| {
            crate::RuntimeError::new(
                "save_failed",
                "Could not read this station's saved work. Stop and ask for help.",
            )
        })?;
    Ok(rows
        .iter()
        .map(|row| view(station_id, station_name, row, store, uid_rule))
        .collect())
}

fn view(
    station_id: Uuid,
    station_name: &str,
    row: &OutboxRow,
    store: &Store,
    uid_rule: rfidex_core::tag::UidRule,
) -> ProblemView {
    let holder_name = row
        .result
        .clone()
        .and_then(|value| serde_json::from_value::<rfidex_core::contract::ErrorBody>(value).ok())
        .and_then(|body| body.holder.map(|h| h.name));
    ProblemView {
        station_id,
        station_name: station_name.to_string(),
        id: row.item.id,
        captured_at: row.item.captured_at,
        name: resolve_name(&row.item.payload, holder_name.as_deref(), store, uid_rule),
        message: match row.state {
            OutboxState::Conflict => conflict_message(row),
            _ => parked_message(row),
        },
    }
}

pub const EXPORT_HEADER: &str = "station,station_id,row_id,kind,state,attempts,captured_at,role,\
uid_raw_hex,name,idem_key,last_error,server_reply,payload";

/// Everything that never reached the server as an accepted send, so a missed
/// scan can be rebuilt by hand. Unlike the diagnostics CSV this keeps the raw
/// UID, the guest name, the real error text and the full saved payload; the
/// file stays on this computer and is for the operator to hand to support.
/// Each line is keyed by `(captured_at, station, row)` so the caller can sort.
pub struct ExportLine {
    pub sort_key: (DateTime<Utc>, Uuid, i64),
    pub line: String,
}

pub fn export_lines(
    station_id: Uuid,
    station_name: &str,
    store: &Store,
    uid_rule: rfidex_core::tag::UidRule,
) -> Result<Vec<ExportLine>, RuntimeError> {
    let rows = store
        .rows(
            &[
                OutboxState::Pending,
                OutboxState::Conflict,
                OutboxState::Parked,
                OutboxState::Dismissed,
            ],
            None,
            usize::MAX,
        )
        .map_err(|_| {
            RuntimeError::new(
                "save_failed",
                "Could not read this station's saved work. Stop and ask for help.",
            )
        })?;
    Ok(rows
        .iter()
        .map(|row| {
            let holder = row.result.clone().and_then(|value| {
                serde_json::from_value::<ObservationResult>(value.clone())
                    .ok()
                    .and_then(|r| r.display.name)
                    .or_else(|| {
                        serde_json::from_value::<ErrorBody>(value)
                            .ok()
                            .and_then(|b| b.holder.map(|h| h.name))
                    })
            });
            let payload = &row.item.payload;
            let text = |key: &str| {
                payload
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
            };
            let name = resolve_name(payload, holder.as_deref(), store, uid_rule);
            let line = [
                station_name.to_string(),
                station_id.to_string(),
                row.item.id.to_string(),
                row.item.kind.as_str().to_string(),
                row.state.as_str().to_string(),
                row.item.attempts.to_string(),
                row.item.captured_at.to_rfc3339(),
                text("role").to_string(),
                text("uid_raw_hex").to_string(),
                name.unwrap_or_default(),
                row.item.idem_key.clone(),
                row.last_error.clone().unwrap_or_default(),
                row.result
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                payload.to_string(),
            ]
            .iter()
            .map(|cell| csv_cell(cell))
            .collect::<Vec<_>>()
            .join(",");
            ExportLine {
                sort_key: (row.item.captured_at, station_id, row.item.id),
                line,
            }
        })
        .collect())
}

/// Newest first, then station UUID and row id so two rows captured in the same
/// millisecond always come out in the same order.
pub fn sort(problems: &mut [ProblemView]) {
    problems.sort_by(|a, b| {
        b.captured_at
            .cmp(&a.captured_at)
            .then(a.station_id.cmp(&b.station_id))
            .then(a.id.cmp(&b.id))
    });
}

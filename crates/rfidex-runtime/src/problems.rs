//! Cross-station problems: the rows that need a person, gathered from every
//! configured station.
//!
//! A row id is local to one station's database, so an action always carries the
//! station UUID as well. Dismissing hides a row from this list; it does not
//! delete it, retry it, or claim the server agreed.

use chrono::{DateTime, Utc};
use rfidex_core::store::{OutboxRow, OutboxState, Store};
use uuid::Uuid;

pub use crate::gate::{conflict_message, parked_message, resolve_name};

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

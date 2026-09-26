//! Desk search: one query in, one answer out.
//!
//! The minima, the rows, the masks and every sentence the operator reads are
//! decided here, in Rust. The module never checks in, prints or enqueues: a
//! search is a question, and choosing a row afterwards is an ordinary desk
//! scan. The desk and store locks are never held across the HTTP call.
//!
//! Online the server does the matching and the masking. Offline only the local
//! ticket cache can answer, and it holds names alone: email and phone search
//! therefore say they need the internet rather than pretend to have searched.

use std::sync::Mutex;

use chrono::{DateTime, Local, TimeZone, Utc};
use rfidex_core::client::{ApiClient, ApiError};
use rfidex_core::contract::{SearchBy, TicketSummary};
use rfidex_core::search::normalized_query;
use rfidex_core::store::Store;
use uuid::Uuid;

use crate::runtime::store_failure;
use crate::RuntimeError;

pub const NAME_MINIMUM: &str = "Enter at least 2 characters";
pub const EMAIL_MINIMUM: &str = "Enter an email address";
pub const PHONE_MINIMUM: &str = "Enter at least 4 phone digits";
pub const NO_MATCHES: &str = "No matching tickets";
pub const OFFLINE_CONTACT: &str = "Needs internet — search by name or scan QR";
/// Offline the cache holds no check-in time, so none is shown.
const CHECKED_IN: &str = "Checked in";

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchRow {
    pub public_id: Uuid,
    pub name: String,
    pub ticket_type: String,
    /// Masked by the server; always `None` offline, where the cache holds no
    /// contact at all.
    pub email_hint: Option<String>,
    pub phone_hint: Option<String>,
    /// "Checked in 09:14" online, "Checked in" offline, `None` if not in.
    pub checked_in_message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchView {
    pub offline: bool,
    /// The minimum-input hint, "No matching tickets", or the offline
    /// contact message. `None` when rows are shown.
    pub message: Option<String>,
    pub rows: Vec<SearchRow>,
}

/// The check-in time as this PC's clock reads it, 24-hour. The zone is a
/// parameter so the wording can be tested against a fixed offset.
pub fn checked_in_message<Tz: TimeZone>(at: DateTime<Utc>, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!("{CHECKED_IN} {}", at.with_timezone(zone).format("%H:%M"))
}

fn minimum(by: SearchBy) -> &'static str {
    match by {
        SearchBy::Name => NAME_MINIMUM,
        SearchBy::Email => EMAIL_MINIMUM,
        SearchBy::Phone => PHONE_MINIMUM,
    }
}

fn rows_view(offline: bool, rows: Vec<SearchRow>) -> SearchView {
    SearchView {
        offline,
        message: rows.is_empty().then(|| NO_MATCHES.to_string()),
        rows,
    }
}

/// Answer one desk search. The caller has already refused a station that is
/// not a desk, and a station that belongs to another event.
pub async fn desk_search(
    client: &ApiClient,
    store: &Mutex<Store>,
    by: SearchBy,
    query: &str,
) -> Result<SearchView, RuntimeError> {
    let Some(query) = normalized_query(by, query) else {
        return Ok(SearchView {
            offline: false,
            message: Some(minimum(by).to_string()),
            rows: Vec::new(),
        });
    };
    match client.search_tickets(by, &query).await {
        Ok(resp) => Ok(rows_view(
            false,
            resp.tickets
                .into_iter()
                .map(|t| SearchRow {
                    checked_in_message: t
                        .checked_in_at
                        .map(|at| checked_in_message(at, &Local))
                        .or_else(|| t.checked_in.then(|| CHECKED_IN.to_string())),
                    public_id: t.public_id,
                    name: t.name,
                    ticket_type: t.ticket_type,
                    email_hint: t.email_hint,
                    phone_hint: t.phone_hint,
                })
                .collect(),
        )),
        Err(ApiError::Retryable(_)) => offline(store, by, &query),
        Err(e) => Err(search_failure(&e)),
    }
}

/// The local cache can only answer a name: email and phone are not stored on
/// this computer, and inventing a "no result" for them would be a lie.
fn offline(store: &Mutex<Store>, by: SearchBy, query: &str) -> Result<SearchView, RuntimeError> {
    if by != SearchBy::Name {
        return Ok(SearchView {
            offline: true,
            message: Some(OFFLINE_CONTACT.to_string()),
            rows: Vec::new(),
        });
    }
    let rows = store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .search_tickets_by_name(query)
        .map_err(|_| store_failure())?;
    Ok(rows_view(
        true,
        rows.into_iter()
            .map(|t: TicketSummary| SearchRow {
                public_id: t.public_id,
                name: t.name,
                ticket_type: t.ticket_type,
                email_hint: None,
                phone_hint: None,
                checked_in_message: t.checked_in.then(|| CHECKED_IN.to_string()),
            })
            .collect(),
    ))
}

/// Never a raw status, a URL or a server body: the operator gets a sentence.
fn search_failure(e: &ApiError) -> RuntimeError {
    match e {
        ApiError::Unauthorized => RuntimeError::new(
            "unauthorized",
            "The server did not accept the API key. Open Setup to check it.",
        ),
        ApiError::BadResponse(_) => RuntimeError::new(
            "server_reply",
            "The server sent a reply this app cannot read. Ask for help.",
        ),
        ApiError::Rejected { .. } => RuntimeError::new(
            "server_rejected",
            "The server could not run this search. Try again.",
        ),
        // Retried above; kept total so a future variant cannot be missed here.
        ApiError::Retryable(_) => RuntimeError::new(
            "offline",
            "Cannot reach the server. Try again when the connection returns.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_in_time_reads_on_the_pc_clock_in_twenty_four_hours() {
        let at: DateTime<Utc> = "2026-09-26T09:14:03Z".parse().unwrap();
        let east = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let west = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(checked_in_message(at, &east), "Checked in 17:14");
        assert_eq!(checked_in_message(at, &west), "Checked in 04:14");
        assert_eq!(checked_in_message(at, &Utc), "Checked in 09:14");
    }

    #[test]
    fn each_field_has_its_own_minimum_sentence() {
        assert_eq!(minimum(SearchBy::Name), NAME_MINIMUM);
        assert_eq!(minimum(SearchBy::Email), EMAIL_MINIMUM);
        assert_eq!(minimum(SearchBy::Phone), PHONE_MINIMUM);
    }
}

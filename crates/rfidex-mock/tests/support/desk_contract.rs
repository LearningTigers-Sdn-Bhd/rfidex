#![allow(dead_code)]

//! Backend-neutral assertions for the desk check-in and search contract.
//!
//! The mock runs these here; P5 is expected to provision the same fixtures and
//! run the same functions against its own base URL and key, unchanged. Nothing
//! in this module knows about `MockState`, its faults or its counters: the
//! caller supplies the base URL, the key, the station id, its own fixture ids,
//! the known first check-in time and the order it expects.
//!
//! Fixture setup stays in the calling test. Only these assertions are shared.

use chrono::{DateTime, Utc};
use reqwest::header::AUTHORIZATION;
use rfidex_core::client::ApiClient;
use rfidex_core::contract::*;
use uuid::Uuid;

/// One caller's fixture: who is in the event, and what the answers must be.
pub struct Case {
    pub base: String,
    pub key: String,
    pub station: String,

    /// The guest whose first check-in this case proves, and the operations the
    /// station used for it.
    pub first: Uuid,
    pub first_operation: Uuid,
    pub first_scan_at: DateTime<Utc>,
    pub repeat_operation: Uuid,
    /// A name that matches more guests than the cap allows.
    pub guest_name: String,

    /// Name queries and the exact order they must come back in.
    pub name_spaced_query: String,
    pub name_plain_query: String,
    pub name_expected: Vec<Uuid>,
    pub cap_query: String,
    pub cap_expected: Vec<Uuid>,
    pub literal_query: String,
    pub literal_expected: Vec<Uuid>,
    pub no_match_query: String,

    /// The contact the masked search must find, and its mask.
    pub email_query: String,
    pub email_full: String,
    pub email_hint: String,
    pub email_expected: Uuid,
    pub phone_queries: Vec<String>,
    pub phone_full: String,
    pub phone_hint: String,
    pub phone_expected: Vec<Uuid>,
    pub short_phone_queries: Vec<String>,

    /// Rows that must never be listed, and a guest with no contact at all.
    pub unpaid_name: String,
    pub cancelled_name: String,
    pub paid_cancelled: Uuid,
    pub no_contact: Uuid,
    pub no_contact_name: String,
    /// A guest the fixture already had checked in before any scan.
    pub checked_in: Uuid,
    pub checked_in_name: String,
    pub checked_in_at: DateTime<Utc>,

    /// A guest only this event has, for the isolation assertion.
    pub unique_name: String,
    pub unique: Uuid,
}

pub fn client(case: &Case) -> ApiClient {
    ApiClient::new(
        &case.base,
        &case.key,
        &case.station,
        std::time::Duration::from_secs(2),
    )
}

pub struct Raw {
    pub status: u16,
    pub body: String,
}

/// A request the typed client cannot make: a wrong key or no station header.
pub async fn raw(case: &Case, by: &str, q: &str, key: Option<&str>, station: Option<&str>) -> Raw {
    let mut req = reqwest::Client::new()
        .get(format!("{}{}", case.base, paths::TICKET_SEARCH))
        .query(&[("by", by), ("q", q)]);
    if let Some(key) = key {
        req = req.header(AUTHORIZATION, key);
    }
    if let Some(station) = station {
        req = req.header(HEADER_STATION, station);
    }
    let resp = req.send().await.expect("the mock answers");
    Raw {
        status: resp.status().as_u16(),
        body: resp.text().await.expect("a body"),
    }
}

pub async fn raw_ok(case: &Case, by: &str, q: &str) -> Raw {
    raw(case, by, q, Some(&case.key), Some(&case.station)).await
}

pub async fn raw_get(case: &Case, path: &str) -> String {
    let resp = reqwest::Client::new()
        .get(format!("{}{path}", case.base))
        .header(AUTHORIZATION, &case.key)
        .header(HEADER_STATION, &case.station)
        .send()
        .await
        .expect("the mock answers");
    assert!(resp.status().is_success(), "{path}: {}", resp.status());
    resp.text().await.expect("a body")
}

/// No full contact value may appear anywhere in a reply, in any spelling.
pub fn assert_no_full_contact(case: &Case, body: &str) {
    let lower = body.to_lowercase();
    assert!(
        !lower.contains(&case.email_full.to_lowercase()),
        "the full email reached the client: {body}"
    );
    assert!(
        !body.contains(&case.phone_full),
        "the full phone reached the client: {body}"
    );
    let digits: String = case
        .phone_full
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    assert!(
        !body.contains(&digits),
        "the phone digits reached the client: {body}"
    );
}

pub async fn search(case: &Case, by: SearchBy, q: &str) -> TicketSearchResp {
    client(case)
        .search_tickets(by, q)
        .await
        .expect("a valid search is answered")
}

pub fn ids(resp: &TicketSearchResp) -> Vec<Uuid> {
    resp.tickets.iter().map(|t| t.public_id).collect()
}

/// The first scan is a first check-in, timed by the capture the station sent.
pub async fn first_check_in(case: &Case) -> DeskScanResp {
    let resp = client(case)
        .desk_scan(&DeskScanReq {
            public_id: case.first,
            operation_id: case.first_operation,
            captured_at: case.first_scan_at,
        })
        .await
        .expect("the first scan is accepted");
    assert_eq!(resp.check_in.result, CheckInResult::CheckedIn);
    assert_eq!(resp.check_in.checked_in_at, case.first_scan_at);
    assert!(resp.ticket.checked_in);
    resp
}

/// Any later scan is `already_checked_in` and keeps the original time, whatever
/// capture time it carried.
pub async fn a_later_scan_is_already_checked_in(case: &Case, first: &DeskScanResp) {
    let resp = client(case)
        .desk_scan(&DeskScanReq {
            public_id: case.first,
            operation_id: case.repeat_operation,
            captured_at: case.first_scan_at + chrono::Duration::minutes(5),
        })
        .await
        .expect("a rescan is not an error");
    assert_eq!(resp.check_in.result, CheckInResult::AlreadyCheckedIn);
    assert_eq!(resp.check_in.checked_in_at, first.check_in.checked_in_at);
}

/// A replayed operation id returns the answer it already gave, even after the
/// guest has been scanned again.
pub async fn replaying_the_first_scan_keeps_its_own_answer(case: &Case, first: &DeskScanResp) {
    let again = client(case)
        .desk_scan(&DeskScanReq {
            public_id: case.first,
            operation_id: case.first_operation,
            captured_at: case.first_scan_at,
        })
        .await
        .expect("a replay is answered");
    assert_eq!(
        &again, first,
        "a replayed operation keeps its original check-in outcome"
    );
}

pub async fn name_search_collapses_case_and_spaces(case: &Case) {
    let spaced = search(case, SearchBy::Name, &case.name_spaced_query).await;
    let plain = search(case, SearchBy::Name, &case.name_plain_query).await;
    assert_eq!(
        ids(&spaced),
        ids(&plain),
        "spacing must not change the answer"
    );
    assert_eq!(ids(&plain), case.name_expected);
}

pub async fn a_name_query_below_two_characters_returns_nothing(case: &Case) {
    for q in ["a", "", "   "] {
        let found = search(case, SearchBy::Name, q).await;
        assert!(found.tickets.is_empty(), "{q:?} must return nothing");
    }
}

pub async fn a_name_query_over_ten_matches_returns_the_ten_newest(case: &Case) {
    let found = search(case, SearchBy::Name, &case.cap_query).await;
    assert_eq!(
        ids(&found),
        case.cap_expected,
        "ten newest, stable tie order"
    );
}

pub async fn wildcards_in_a_name_are_literal(case: &Case) {
    let found = search(case, SearchBy::Name, &case.literal_query).await;
    assert_eq!(
        ids(&found),
        case.literal_expected,
        "% and _ must not expand to every ticket"
    );
    assert!(found
        .tickets
        .iter()
        .all(|t| t.name.contains(&case.literal_query)));
}

pub async fn a_query_without_matches_is_an_empty_list(case: &Case) {
    let raw = raw_ok(case, "name", &case.no_match_query).await;
    assert_eq!(raw.status, 200);
    let typed: TicketSearchResp = serde_json::from_str(&raw.body).expect("the contract shape");
    assert!(typed.tickets.is_empty());
}

pub async fn email_search_is_exact_and_masked(case: &Case) {
    let found = search(case, SearchBy::Email, &case.email_query).await;
    assert_eq!(ids(&found), vec![case.email_expected]);
    assert_eq!(
        found.tickets[0].email_hint.as_deref(),
        Some(case.email_hint.as_str())
    );
    let json = serde_json::to_string(&found).unwrap();
    assert_no_full_contact(case, &json);
    let raw = raw_ok(case, "email", &case.email_query).await;
    assert_eq!(raw.status, 200);
    assert_no_full_contact(case, &raw.body);
    let leaked: serde_json::Value = serde_json::from_str(&raw.body).unwrap();
    assert!(
        leaked["tickets"][0].get("email").is_none() && leaked["tickets"][0].get("phone").is_none(),
        "the raw reply carries no unmasked contact fields: {}",
        raw.body
    );
}

pub async fn email_without_an_at_sign_is_not_a_substring_match(case: &Case) {
    for q in ["ahmad", "example.com"] {
        let found = search(case, SearchBy::Email, q).await;
        assert!(found.tickets.is_empty(), "{q:?} must not match");
    }
}

pub async fn every_phone_spelling_finds_the_same_guest(case: &Case) {
    for q in &case.phone_queries {
        let found = search(case, SearchBy::Phone, q).await;
        assert_eq!(ids(&found), case.phone_expected, "{q:?}");
    }
    let found = search(case, SearchBy::Phone, &case.phone_queries[0]).await;
    assert_eq!(
        found.tickets[0].phone_hint.as_deref(),
        Some(case.phone_hint.as_str())
    );
    let raw = raw_ok(case, "phone", &case.phone_queries[0]).await;
    assert_eq!(raw.status, 200);
    assert_no_full_contact(case, &raw.body);
}

pub async fn a_phone_query_below_four_digits_returns_nothing(case: &Case) {
    for q in &case.short_phone_queries {
        let found = search(case, SearchBy::Phone, q).await;
        assert!(found.tickets.is_empty(), "{q:?} must return nothing");
    }
}

pub async fn an_unpaid_guest_is_never_listed(case: &Case) {
    let found = search(case, SearchBy::Name, &case.unpaid_name).await;
    assert!(found.tickets.is_empty(), "search is for paid tickets");
}

/// A paid but cancelled guest may be listed — the contract filters on payment,
/// not on desk validity — but is never offered as usable.
pub async fn a_cancelled_ticket_is_never_listed_as_valid(case: &Case) {
    let found = search(case, SearchBy::Name, &case.cancelled_name).await;
    assert!(
        found.tickets.iter().all(|t| !t.valid),
        "a cancelled ticket is never valid"
    );
    if let Some(row) = found
        .tickets
        .iter()
        .find(|t| t.public_id == case.paid_cancelled)
    {
        assert!(!row.valid);
    }
}

pub async fn a_missing_contact_has_no_hint(case: &Case) {
    let found = search(case, SearchBy::Name, &case.no_contact_name).await;
    assert_eq!(ids(&found), vec![case.no_contact]);
    assert_eq!(found.tickets[0].email_hint, None);
    assert_eq!(found.tickets[0].phone_hint, None);
}

pub async fn a_checked_in_guest_carries_the_first_time(case: &Case) {
    let found = search(case, SearchBy::Name, &case.checked_in_name).await;
    assert_eq!(ids(&found), vec![case.checked_in]);
    assert!(found.tickets[0].checked_in);
    assert_eq!(found.tickets[0].checked_in_at, Some(case.checked_in_at));
}

/// One event at a time: another event's guest is invisible here.
pub async fn a_guest_is_invisible_to_another_event(guest: &Case, other: &Case) {
    let here = search(guest, SearchBy::Name, &guest.unique_name).await;
    assert_eq!(ids(&here), vec![guest.unique]);
    let there = search(other, SearchBy::Name, &guest.unique_name).await;
    assert!(
        there.tickets.is_empty(),
        "a guest of one event must not appear in another"
    );
}

pub async fn a_missing_or_wrong_key_reveals_nothing(case: &Case) {
    let missing = raw(
        case,
        "name",
        &case.name_plain_query,
        None,
        Some(&case.station),
    )
    .await;
    assert_eq!(missing.status, 401);
    let wrong = raw(
        case,
        "name",
        &case.name_plain_query,
        Some("wrong_key_wrong_key_wrong_key_xx"),
        Some(&case.station),
    )
    .await;
    assert_eq!(wrong.status, 401);
    for body in [&missing.body, &wrong.body] {
        assert_no_full_contact(case, body);
        assert!(
            !body.contains(&case.guest_name),
            "a rejected key never returns guest data: {body}"
        );
    }
}

pub async fn a_missing_station_header_is_rejected(case: &Case) {
    let raw = raw(case, "name", &case.name_plain_query, Some(&case.key), None).await;
    assert_eq!(raw.status, 400);
    let body: ErrorBody = serde_json::from_str(&raw.body).expect("a typed error body");
    assert_eq!(body.error, ErrorCode::Malformed);
}

pub async fn an_unknown_search_field_is_a_typed_error(case: &Case) {
    let raw = raw_ok(case, "staff", &case.name_plain_query).await;
    assert_eq!(raw.status, 400, "{}", raw.body);
    let body: ErrorBody = serde_json::from_str(&raw.body).expect("a typed error body");
    assert_eq!(body.error, ErrorCode::Malformed);
}

pub async fn the_ticket_cache_never_carries_contacts(case: &Case) {
    let body = raw_get(case, paths::CACHE).await;
    assert_no_full_contact(case, &body);
    for field in ["email", "phone", "hint"] {
        assert!(
            !body.contains(field),
            "the offline cache must not grow a {field} field: {body}"
        );
    }
}

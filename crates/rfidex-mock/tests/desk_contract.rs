//! The desk contract, over real loopback HTTP, against the mock EventzFlow API.
//!
//! The assertions themselves live in `support/desk_contract.rs` and know nothing
//! about the mock, so P5 can run the same ones against a real server. This file
//! only builds the fixtures, starts the server and calls them.

mod support {
    pub mod desk_contract;
}

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rfidex_core::contract::*;
use rfidex_mock::http::{serve, AppState};
use rfidex_mock::state::{MockState, SeedTicket};
use support::desk_contract as contract;
use uuid::Uuid;

const KEY: &str = "rfidex_test_key_0123456789abcdefghij";
const OTHER_KEY: &str = "rfidex_other_key_0123456789abcdefghij";
const STATION: &str = "desk-contract";

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn minutes(n: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-20T06:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::minutes(n)
}

fn stamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn ticket(n: u128, name: &str, created: DateTime<Utc>) -> SeedTicket {
    SeedTicket {
        public_id: id(n),
        name: name.into(),
        ticket_type: "Delegate".into(),
        paid: true,
        cancelled: false,
        email: None,
        phone: None,
        created_at: Some(created),
        checked_in_at: None,
    }
}

/// Fourteen paid guests whose name matches `ahmad`, one unpaid, one paid but
/// cancelled, one with no contact at all, one already checked in, and a name
/// holding the two SQL wildcard characters.
fn event_a_seeds() -> Vec<SeedTicket> {
    let surnames = [
        "Rahman", "Zaki", "Faizal", "Hakim", "Nizam", "Rizal", "Sani", "Taufik", "Wafi", "Yusuf",
        "Zainal", "Amran",
    ];
    let mut seeds = vec![
        SeedTicket {
            email: Some("Ahmad@Example.com".into()),
            phone: Some("012-345 6789".into()),
            ..ticket(1, "Ahmad Bin Ali", minutes(1))
        },
        SeedTicket {
            email: Some("second.ahmad@example.com".into()),
            phone: Some("+60 12 345 6790".into()),
            ..ticket(2, "Ahmad Bin Ali", minutes(2))
        },
    ];
    for (i, surname) in surnames.iter().enumerate() {
        let n = i as u128 + 3;
        // Two of them share a creation time, so the tie order is exercised
        // inside the ten-row cap.
        let created = if n == 12 {
            minutes(13)
        } else {
            minutes(n as i64)
        };
        seeds.push(ticket(n, &format!("Ahmad {surname}"), created));
    }
    seeds.push(SeedTicket {
        paid: false,
        ..ticket(15, "Ahmad Unpaid", minutes(15))
    });
    seeds.push(ticket(16, "Siti %_ Nurhaliza", minutes(16)));
    seeds.push(SeedTicket {
        cancelled: true,
        ..ticket(17, "Zulkifli Cancelled", minutes(17))
    });
    seeds.push(SeedTicket {
        checked_in_at: Some(stamp("2026-09-25T10:00:00Z")),
        ..ticket(18, "Nur Prechecked", minutes(18))
    });
    seeds.push(ticket(19, "Tan No Contact", minutes(19)));
    seeds
}

fn event_b_seeds() -> Vec<SeedTicket> {
    // No creation time anywhere: the seed order decides, later rows newer.
    let seed = |n: u128, name: &str| SeedTicket {
        public_id: id(n),
        name: name.into(),
        ticket_type: "Delegate".into(),
        paid: true,
        cancelled: false,
        email: None,
        phone: None,
        created_at: None,
        checked_in_at: None,
    };
    vec![
        seed(901, "Zara Isolated"),
        seed(902, "Umar First"),
        seed(903, "Umar Second"),
        seed(904, "Umar Third"),
    ]
}

async fn spawn(key: &str, event_id: i64, seeds: Vec<SeedTicket>) -> (String, Arc<AppState>) {
    let event = EventSettings {
        event_id,
        name: format!("Event {event_id}"),
        rfid_mode: RfidMode::Bind,
        require_check_in: false,
    };
    let state = Arc::new(AppState::new(MockState::new(key.into(), event, seeds)));
    let (addr, _handle) = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (format!("http://{addr}"), state)
}

fn case_for(base: String, key: &str) -> contract::Case {
    contract::Case {
        base,
        key: key.into(),
        station: STATION.into(),
        first: id(1),
        first_operation: id(9001),
        first_scan_at: stamp("2026-09-26T09:14:03Z"),
        repeat_operation: id(9002),
        guest_name: "Ahmad Bin Ali".into(),
        name_spaced_query: "  AHMAD   bin ".into(),
        name_plain_query: "ahmad bin".into(),
        name_expected: vec![id(2), id(1)],
        cap_query: "ahmad".into(),
        cap_expected: vec![
            id(14),
            id(12),
            id(13),
            id(11),
            id(10),
            id(9),
            id(8),
            id(7),
            id(6),
            id(5),
        ],
        literal_query: "%_".into(),
        literal_expected: vec![id(16)],
        no_match_query: "nobody here at all".into(),
        email_query: " AHMAD@EXAMPLE.COM ".into(),
        email_full: "Ahmad@Example.com".into(),
        email_hint: "ah***@example.com".into(),
        email_expected: id(1),
        phone_queries: vec![
            "012-345 6789".into(),
            "0123456789".into(),
            "+60 12 345 6789".into(),
        ],
        phone_full: "012-345 6789".into(),
        phone_hint: "•••• 6789".into(),
        phone_expected: vec![id(1)],
        short_phone_queries: vec!["012".into(), "600".into(), "abc".into()],
        unpaid_name: "Ahmad Unpaid".into(),
        cancelled_name: "Zulkifli".into(),
        paid_cancelled: id(17),
        no_contact: id(19),
        no_contact_name: "Tan No Contact".into(),
        checked_in: id(18),
        checked_in_name: "Nur Prechecked".into(),
        checked_in_at: stamp("2026-09-25T10:00:00Z"),
        unique_name: "Zara Isolated".into(),
        unique: id(901),
    }
}

#[tokio::test]
async fn the_desk_contract_holds_over_http() {
    let (base, _state) = spawn(KEY, 7, event_a_seeds()).await;
    let case = case_for(base, KEY);

    // First check-in, the rescan after it, and the replay of the first.
    let first = contract::first_check_in(&case).await;
    contract::a_later_scan_is_already_checked_in(&case, &first).await;
    contract::replaying_the_first_scan_keeps_its_own_answer(&case, &first).await;

    // Name search: normalization, minima, the cap and literal wildcards.
    contract::name_search_collapses_case_and_spaces(&case).await;
    contract::a_name_query_below_two_characters_returns_nothing(&case).await;
    contract::a_name_query_over_ten_matches_returns_the_ten_newest(&case).await;
    contract::wildcards_in_a_name_are_literal(&case).await;
    contract::a_query_without_matches_is_an_empty_list(&case).await;

    // Contact search: exact email, every phone spelling, masks in the raw body.
    contract::email_search_is_exact_and_masked(&case).await;
    contract::email_without_an_at_sign_is_not_a_substring_match(&case).await;
    contract::every_phone_spelling_finds_the_same_guest(&case).await;
    contract::a_phone_query_below_four_digits_returns_nothing(&case).await;

    // Who may be listed, and what the row says about them.
    contract::an_unpaid_guest_is_never_listed(&case).await;
    contract::a_cancelled_ticket_is_never_listed_as_valid(&case).await;
    contract::a_missing_contact_has_no_hint(&case).await;
    contract::a_checked_in_guest_carries_the_first_time(&case).await;

    // Auth, malformed input, and a cache that stays free of contacts.
    contract::a_missing_or_wrong_key_reveals_nothing(&case).await;
    contract::a_missing_station_header_is_rejected(&case).await;
    contract::an_unknown_search_field_is_a_typed_error(&case).await;
    contract::the_ticket_cache_never_carries_contacts(&case).await;
}

#[tokio::test]
async fn the_mock_lists_a_paid_cancelled_ticket_as_invalid() {
    let (base, _state) = spawn(KEY, 7, event_a_seeds()).await;
    let case = case_for(base, KEY);
    let found = contract::search(&case, SearchBy::Name, &case.cancelled_name).await;
    assert_eq!(
        contract::ids(&found),
        vec![case.paid_cancelled],
        "search filters on payment, not on desk validity"
    );
    assert!(!found.tickets[0].valid, "and says the ticket is not usable");
}

#[tokio::test]
async fn guests_without_a_creation_time_keep_their_seed_order() {
    let (base, _state) = spawn(OTHER_KEY, 8, event_b_seeds()).await;
    let case = case_for(base, OTHER_KEY);
    let found = contract::search(&case, SearchBy::Name, "umar").await;
    assert_eq!(
        contract::ids(&found),
        vec![id(904), id(903), id(902)],
        "later seed rows are newer, and no hash order can change that"
    );
}

#[tokio::test]
async fn one_event_never_sees_another_events_guest() {
    let (a, _state_a) = spawn(KEY, 7, event_a_seeds()).await;
    let (b, _state_b) = spawn(OTHER_KEY, 8, event_b_seeds()).await;
    let event_a = case_for(a, KEY);
    let event_b = case_for(b, OTHER_KEY);
    contract::a_guest_is_invisible_to_another_event(&event_b, &event_a).await;
}

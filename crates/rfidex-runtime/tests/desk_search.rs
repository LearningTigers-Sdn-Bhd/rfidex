//! Desk search: the online query, the offline name-only cache, and the
//! guard rails that stop a wrong answer reaching the screen.

mod common;

use common::*;
use rfidex_core::contract::{EventSettings, RfidMode, SearchBy};
use rfidex_core::store::OutboxState;
use rfidex_mock::http::Faults;
use rfidex_mock::state::SeedTicket;
use rfidex_runtime::config::StationConfig;
use rfidex_runtime::search::{
    SearchView, EMAIL_MINIMUM, NAME_MINIMUM, NO_MATCHES, OFFLINE_CONTACT, PHONE_MINIMUM,
};
use uuid::Uuid;

const CHECK_IN: &str = "Checked in ";

fn stamp(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn minutes(n: i64) -> chrono::DateTime<chrono::Utc> {
    stamp("2026-09-20T06:00:00Z") + chrono::Duration::minutes(n)
}

/// Fourteen paid guests named Ahmad — two of them sharing a name and a
/// creation time — one unpaid, one wildcard name, and one already in.
fn desk_seeds() -> Vec<SeedTicket> {
    let surnames = [
        "Rahman", "Zaki", "Faizal", "Hakim", "Nizam", "Rizal", "Sani", "Taufik", "Wafi", "Yusuf",
        "Zainal", "Amran",
    ];
    let seed = |n: u128, name: String, created: chrono::DateTime<chrono::Utc>| SeedTicket {
        public_id: ticket(n),
        name,
        ticket_type: "Delegate".into(),
        paid: true,
        cancelled: false,
        email: None,
        phone: None,
        created_at: Some(created),
        checked_in_at: None,
    };
    let mut seeds = vec![
        SeedTicket {
            email: Some("Ahmad@Example.com".into()),
            phone: Some("012-345 6789".into()),
            ..seed(1, "Ahmad Bin Ali".into(), minutes(1))
        },
        SeedTicket {
            email: Some("second.ahmad@example.com".into()),
            ..seed(2, "Ahmad Bin Ali".into(), minutes(2))
        },
    ];
    for (i, surname) in surnames.iter().enumerate() {
        let n = i as u128 + 3;
        let created = if n == 12 {
            minutes(13)
        } else {
            minutes(n as i64)
        };
        seeds.push(seed(n, format!("Ahmad {surname}"), created));
    }
    seeds.push(SeedTicket {
        paid: false,
        ..seed(15, "Ahmad Unpaid".into(), minutes(15))
    });
    seeds.push(seed(16, "Siti %_ Nurhaliza".into(), minutes(16)));
    seeds
}

fn event() -> EventSettings {
    EventSettings {
        event_id: 21,
        name: "Desk Search Expo".into(),
        rfid_mode: RfidMode::Bind,
        require_check_in: false,
    }
}

async fn seeded() -> Harness {
    Harness::start_seeded(event(), desk_seeds(), fast_options(), vec![desk_station()]).await
}

/// The ten newest of the fourteen paid Ahmad guests, ties by id.
fn expected_cap() -> Vec<Uuid> {
    [14, 12, 13, 11, 10, 9, 8, 7, 6, 5]
        .into_iter()
        .map(ticket)
        .collect()
}

#[tokio::test]
async fn below_the_minimum_nothing_leaves_the_app() {
    let h = Harness::start(RfidMode::Bind).await;
    // Any request at all would fail here and read as an offline answer.
    h.set_down(true);
    for (by, query, message) in [
        (SearchBy::Name, "a", NAME_MINIMUM),
        (SearchBy::Name, "   ", NAME_MINIMUM),
        (SearchBy::Email, "ahmad", EMAIL_MINIMUM),
        (SearchBy::Email, "", EMAIL_MINIMUM),
        (SearchBy::Phone, "012", PHONE_MINIMUM),
        (SearchBy::Phone, "abc", PHONE_MINIMUM),
    ] {
        let view = h.runtime.desk_search(desk_id(), by, query).await.unwrap();
        assert_eq!(view.message.as_deref(), Some(message), "{by:?} {query:?}");
        assert!(view.rows.is_empty(), "{by:?} {query:?}");
        assert!(!view.offline, "{by:?} {query:?} must not query the cache");
    }
}

#[tokio::test]
async fn online_search_answers_by_name_email_and_phone() {
    let h = seeded().await;

    let names = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "  AHMAD   bin ")
        .await
        .unwrap();
    assert!(!names.offline);
    assert_eq!(names.message, None, "rows are their own answer");
    assert_eq!(
        names.rows.iter().map(|r| r.public_id).collect::<Vec<_>>(),
        vec![ticket(2), ticket(1)],
        "newest first"
    );
    assert_eq!(names.rows[0].ticket_type, "Delegate");

    let capped = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "ahmad")
        .await
        .unwrap();
    assert_eq!(capped.rows.len(), 10, "at most ten rows reach the screen");
    assert_eq!(
        capped.rows.iter().map(|r| r.public_id).collect::<Vec<_>>(),
        expected_cap()
    );

    let email = h
        .runtime
        .desk_search(desk_id(), SearchBy::Email, " AHMAD@EXAMPLE.COM ")
        .await
        .unwrap();
    assert_eq!(email.rows.len(), 1);
    assert_eq!(email.rows[0].public_id, ticket(1));
    assert_eq!(
        email.rows[0].email_hint.as_deref(),
        Some("ah***@example.com")
    );

    let phone = h
        .runtime
        .desk_search(desk_id(), SearchBy::Phone, "+60 12 345 6789")
        .await
        .unwrap();
    assert_eq!(
        phone.rows.iter().map(|r| r.public_id).collect::<Vec<_>>(),
        vec![ticket(1)]
    );
    assert_eq!(phone.rows[0].phone_hint.as_deref(), Some("•••• 6789"));

    let empty = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "nobody here at all")
        .await
        .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(empty.message.as_deref(), Some(NO_MATCHES));
    assert!(!empty.offline);
}

#[tokio::test]
async fn a_checked_in_guest_shows_the_time_on_this_pc() {
    let h = seeded().await;
    h.runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();

    let view = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "ahmad bin ali")
        .await
        .unwrap();
    let row = view
        .rows
        .iter()
        .find(|r| r.public_id == ticket(1))
        .expect("the scanned guest");
    let message = row.checked_in_message.as_deref().expect("a check-in time");
    assert!(message.starts_with(CHECK_IN), "{message}");
    let clock = &message[CHECK_IN.len()..];
    assert_eq!(clock.len(), 5, "{message}");
    assert_eq!(clock.chars().filter(|c| *c == ':').count(), 1, "{message}");

    let other = view
        .rows
        .iter()
        .find(|r| r.public_id == ticket(2))
        .expect("the guest who was not scanned");
    assert_eq!(other.checked_in_message, None);
}

#[tokio::test]
async fn a_gate_cannot_search() {
    let h = Harness::start(RfidMode::Bind).await;
    let error = h
        .runtime
        .desk_search(entry_id(), SearchBy::Name, "ahmad")
        .await
        .unwrap_err();
    assert_eq!(error.code, "wrong_station");
}

#[tokio::test]
async fn searching_creates_no_work_on_either_desk() {
    let mut stations = three_stations();
    stations.push(StationConfig {
        id: Uuid::from_u128(201),
        name: "Second desk".into(),
        ..desk_station()
    });
    let h = Harness::start_with(RfidMode::Bind, fast_options(), stations).await;
    let second = Uuid::from_u128(201);

    let first = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "aina")
        .await
        .unwrap();
    let other = h
        .runtime
        .desk_search(second, SearchBy::Name, "aina")
        .await
        .unwrap();
    assert_eq!(
        first.rows.iter().map(|r| r.public_id).collect::<Vec<_>>(),
        other.rows.iter().map(|r| r.public_id).collect::<Vec<_>>(),
        "each desk asks the same event for itself"
    );
    assert!(!first.rows.is_empty());

    for id in [desk_id(), second] {
        assert_eq!(
            h.store_of(id).count(OutboxState::Pending).unwrap(),
            0,
            "a search is never queued"
        );
    }
    let mock = h.server.mock.lock().unwrap();
    assert_eq!(mock.scan_operation_count(), 0, "a search is not a check-in");
    assert_eq!(mock.binding_operation_count(), 0);
}

#[tokio::test]
async fn offline_name_search_answers_from_the_cache_and_points_contacts_at_the_internet() {
    let h = Harness::start(RfidMode::Bind).await;
    h.runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sync_now().await.unwrap();

    h.set_down(true);
    let names = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "  AINA  ")
        .await
        .unwrap();
    assert!(names.offline, "the answer came from this computer");
    assert_eq!(names.message, None);
    assert_eq!(
        names.rows.iter().map(|r| r.public_id).collect::<Vec<_>>(),
        vec![ticket(1)]
    );
    assert_eq!(
        names.rows[0].checked_in_message.as_deref(),
        Some("Checked in"),
        "the cache holds no check-in time, so none is invented"
    );
    assert_eq!(names.rows[0].email_hint, None);
    assert_eq!(names.rows[0].phone_hint, None);

    for (by, query) in [
        (SearchBy::Email, "ahmad@example.com"),
        (SearchBy::Phone, "0123456789"),
    ] {
        let view = h.runtime.desk_search(desk_id(), by, query).await.unwrap();
        assert!(view.offline, "{by:?}");
        assert!(view.rows.is_empty(), "{by:?}");
        assert_eq!(view.message.as_deref(), Some(OFFLINE_CONTACT), "{by:?}");
    }

    // Choosing a row afterwards is an ordinary scan, and offline it queues as
    // it always did.
    let scanned = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    assert!(scanned.offline);
    assert_eq!(
        h.store_of(desk_id()).count(OutboxState::Pending).unwrap(),
        1
    );
}

#[tokio::test]
async fn a_broken_reply_is_an_error_and_not_a_cache_answer() {
    let h = Harness::start(RfidMode::Bind).await;
    // The cache would answer this query, so a fallback would be visible.
    h.set_faults(Faults {
        bad_body: 20,
        ..Faults::default()
    });
    let error = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "aina")
        .await
        .unwrap_err();
    assert_eq!(error.code, "server_reply");
}

#[tokio::test]
async fn a_rejected_key_is_an_error_and_not_a_cache_answer() {
    let h = Harness::start_with_bad_key(RfidMode::Bind).await;
    let error = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "aina")
        .await
        .unwrap_err();
    assert_eq!(error.code, "unauthorized");
}

#[tokio::test]
async fn a_different_event_is_refused_rather_than_answered() {
    let h = Harness::start(RfidMode::Bind).await;
    // Work for this event, so the guard cannot simply adopt the new one.
    h.set_down(true);
    h.runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    h.set_event(2);
    h.set_down(false);
    eventually("the station to report the different event", || async {
        h.station(desk_id()).event_mismatch()
    })
    .await;

    let error = h
        .runtime
        .desk_search(desk_id(), SearchBy::Name, "aina")
        .await
        .unwrap_err();
    assert_eq!(error.code, "different_event");
    assert!(
        error.message.contains("different event"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn the_search_view_never_carries_a_full_contact() {
    let h = seeded().await;
    let view: SearchView = h
        .runtime
        .desk_search(desk_id(), SearchBy::Email, " AHMAD@EXAMPLE.COM ")
        .await
        .unwrap();
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains("ahmad@example.com"), "{json}");
    assert!(!json.contains("0123456789"), "{json}");
}

//! P3 rehearsal: the real runtime against the real mock over loopback HTTP.
//!
//! Two isolated event runs exercise one Bind desk and one Write desk, each with
//! its own entry and exit gate. Bind is event 101 with desk 10001, Write is
//! event 102 with desk 10002; the gates are 20001 and 20002 in both, which is
//! safe because the two events never share a server or a data root. Tickets are
//! numbered 1..=total, first half Bind, second half Write, and the UID for
//! ticket `n` is `format!("{n:016X}")`, so every sticker has its own eight bytes.
//!
//! The oracle is always the mock's own counters, ID sets and stored rows, never
//! the harness's expectation of what it sent.

mod common;

use common::{eventually, gate_station, Harness};
use rfidex_core::contract::{EventSettings, RfidMode, Role};
use rfidex_mock::state::SeedTicket;
use rfidex_runtime::config::{DeviceChoice, StationConfig};
use rfidex_runtime::desk::DeskStep;
use rfidex_runtime::RuntimeOptions;
use uuid::Uuid;

pub const BIND_EVENT: i64 = 101;
pub const WRITE_EVENT: i64 = 102;
pub const BIND_DESK: u128 = 10001;
pub const WRITE_DESK: u128 = 10002;
pub const ENTRY_GATE: u128 = 20001;
pub const EXIT_GATE: u128 = 20002;

/// The ticket `n` of the rehearsal, in the one numbering every stage uses.
fn ticket(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A sticker UID that is distinct from every other ticket's, and wide enough
/// that the raw bytes are not all zero.
fn uid(n: u128) -> String {
    format!("{n:016X}")
}

fn rehearsal_event(event_id: i64, name: &str, mode: RfidMode) -> EventSettings {
    EventSettings {
        event_id,
        name: name.into(),
        rfid_mode: mode,
        require_check_in: false,
    }
}

/// `count` paid, non-cancelled fictional tickets, numbered from `first`.
fn rehearsal_tickets(first: u128, count: u128) -> Vec<SeedTicket> {
    (first..first + count)
        .map(|n| SeedTicket {
            public_id: ticket(n),
            name: format!("Rehearsal {n:04}"),
            ticket_type: "General".into(),
            paid: true,
            cancelled: false,
        })
        .collect()
}

fn rehearsal_stations(desk: u128) -> Vec<StationConfig> {
    vec![
        StationConfig {
            id: Uuid::from_u128(desk),
            name: "Rehearsal desk".into(),
            kind: rfidex_core::contract::StationKind::Desk,
            role: None,
            device: DeviceChoice::SimDesk,
            debounce_secs: 5,
            write_start_block: 0,
        },
        gate_station(Uuid::from_u128(ENTRY_GATE), "Entry gate", Role::Entry),
        gate_station(Uuid::from_u128(EXIT_GATE), "Exit gate", Role::Exit),
    ]
}

fn rehearsal_options() -> RuntimeOptions {
    RuntimeOptions {
        heartbeat: std::time::Duration::from_millis(100),
        gate_poll: std::time::Duration::from_millis(10),
        client_timeout: std::time::Duration::from_secs(2),
    }
}

/// One ticket through the desk: scan it, put a sticker on the reader, link it,
/// take the sticker off. Every stage of the rehearsal does exactly this, so the
/// path proved here is the path the 500-ticket run uses.
async fn register(h: &Harness, desk: Uuid, n: u128) {
    let view = h.runtime.desk_reset(desk).await.expect("reset the desk");
    assert_eq!(view.step, DeskStep::Ready);
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(n).to_string())
        .await
        .expect("scan the ticket");
    assert_eq!(
        scanned.step,
        DeskStep::Scanned,
        "ticket {n} should scan: {:?}",
        scanned.message
    );
    assert_eq!(
        scanned.message, "Place one sticker on the reader.",
        "ticket {n} must be recognised online, not answered from a stale cache"
    );
    let scanned_ticket = scanned.ticket.expect("a scanned ticket");
    assert_eq!(scanned_ticket.public_id, ticket(n));
    assert!(scanned_ticket.checked_in, "ticket {n} is checked in");
    assert_eq!(scanned_ticket.name, format!("Rehearsal {n:04}"));

    h.runtime
        .sim_place(desk, &uid(n))
        .await
        .expect("place the sticker");
    let linked = h
        .runtime
        .desk_link(desk, None)
        .await
        .expect("link the sticker");
    assert_eq!(
        linked.step,
        DeskStep::Linked,
        "ticket {n} should link: {:?}",
        linked.message
    );
    h.runtime.sim_clear(desk).await.expect("remove the sticker");
}

#[tokio::test]
async fn a_custom_seeded_event_reaches_the_mock() {
    let desk = Uuid::from_u128(BIND_DESK);
    let mut h = Harness::start_seeded(
        rehearsal_event(BIND_EVENT, "Rehearsal Bind", RfidMode::Bind),
        rehearsal_tickets(1, 6),
        rehearsal_options(),
        rehearsal_stations(BIND_DESK),
    )
    .await;

    assert_eq!(
        h.station(desk).mode(),
        Some(RfidMode::Bind),
        "the desk takes the mode from the event it was seeded with"
    );
    assert_eq!(
        h.saved_settings(desk).unwrap().event.event_id,
        BIND_EVENT,
        "not the fixture's event 1"
    );

    for n in 1..=6 {
        register(&h, desk, n).await;
    }

    eventually("six scans and six bindings to reach the server", || async {
        let mock = h.server.mock.lock().unwrap();
        mock.scan_log_count == 6 && mock.binding_count() == 6
    })
    .await;

    {
        let mock = h.server.mock.lock().unwrap();
        assert_eq!(mock.scan_operation_count(), 6);
        assert_eq!(mock.binding_operation_count(), 6);
        assert_eq!(mock.event.event_id, BIND_EVENT);
        assert_eq!(
            mock.observation_count(),
            0,
            "the desk stages send no passage"
        );
        assert_eq!(mock.received_order.len(), 0);
        let bindings = mock.active_bindings();
        assert_eq!(bindings.len(), 6);
        for b in &bindings {
            assert_eq!(b.mode, rfidex_core::contract::BindMode::Bind);
            assert!(
                b.uid_raw_hex == uid(b.public_id.as_u128()),
                "sticker {} belongs to a different ticket",
                b.uid_raw_hex
            );
        }
    }

    for n in 1..=6 {
        let cached = h.store_of(desk).ticket(ticket(n)).unwrap();
        assert!(
            cached.is_some_and(|t| t.checked_in),
            "ticket {n} should be cached as checked in"
        );
    }
    h.stop().await;
}

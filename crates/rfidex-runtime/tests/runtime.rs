//! The runtime against a real mock server over HTTP: station identity, the
//! lifecycle, restart behaviour, and the event guard.

mod common;

use std::time::Duration;

use common::{desk_id, entry_id, eventually, exit_id, ticket, Harness, TAG_A, TAG_B, TAG_C};
use rfidex_core::contract::{RfidMode, Role, StationKind};
use rfidex_core::store::OutboxState;
use rfidex_runtime::desk::DeskStep;
use rfidex_runtime::GateStatus;
use rfidex_runtime::RuntimeOptions;

#[tokio::test]
async fn heartbeat_registers_three_stations() {
    let mut h = Harness::start(RfidMode::Bind).await;
    {
        let mock = h.server.mock.lock().unwrap();
        let mut registered: Vec<_> = mock.stations.keys().cloned().collect();
        registered.sort();
        assert_eq!(
            registered,
            vec![
                desk_id().to_string(),
                entry_id().to_string(),
                exit_id().to_string()
            ],
            "each station registers under its own UUID, never one shared gate name"
        );
        let desk = mock.stations.get(&desk_id().to_string()).unwrap();
        assert_eq!(desk.kind, StationKind::Desk);
        assert_eq!(desk.role, None);
        assert_eq!(desk.name, "Desk");
        let entry = mock.stations.get(&entry_id().to_string()).unwrap();
        assert_eq!(entry.role, Some(Role::Entry));
        let exit = mock.stations.get(&exit_id().to_string()).unwrap();
        assert_eq!(exit.role, Some(Role::Exit));
    }
    h.stop().await;
}

#[tokio::test]
async fn desk_follows_the_event_mode() {
    let mut h = Harness::start(RfidMode::Bind).await;
    assert_eq!(
        h.saved_settings(desk_id()).unwrap().event.rfid_mode,
        RfidMode::Bind
    );

    h.set_mode(RfidMode::Write);
    eventually("the desk to follow the new event mode", || async {
        h.saved_settings(desk_id())
            .is_some_and(|s| s.event.rfid_mode == RfidMode::Write)
            && h.station(desk_id()).mode() == Some(RfidMode::Write)
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn shutdown_finishes_even_when_the_server_stalls() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.server.faults.lock().unwrap().delay_ms = 5_000;

    let joined = tokio::time::timeout(Duration::from_secs(1), h.runtime.shutdown()).await;
    assert!(
        joined.is_ok(),
        "shutdown must not wait through an HTTP timeout"
    );
    assert!(joined.unwrap().is_ok());
    assert!(h.runtime.shutdown().await.is_ok(), "shutdown is safe twice");
    h.stop().await;
}

#[tokio::test]
async fn restart_keeps_the_queue_and_moves_the_sequence_on() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.set_down(true);
    h.runtime.sim_pass(entry_id(), TAG_A).await.unwrap();
    eventually("the gate row to be saved", || async {
        h.store_of(entry_id()).count(OutboxState::Pending).unwrap() == 1
    })
    .await;

    let first_sequence = h.saved_sequence(entry_id());
    assert!(first_sequence > 0);

    h.restart_runtime_offline().await;
    assert_eq!(
        h.store_of(entry_id()).count(OutboxState::Pending).unwrap(),
        1,
        "a queued row survives a restart"
    );
    assert_eq!(
        h.station(entry_id()).mode(),
        Some(RfidMode::Bind),
        "saved settings make the station usable again without the server"
    );

    h.runtime.sim_pass(entry_id(), TAG_B).await.unwrap();
    let second = h.saved_sequence(entry_id());
    assert!(
        second > first_sequence,
        "a new record must not reuse a sequence the mock has already seen"
    );
    h.set_down(false);
    eventually("both rows to drain after the restart", || async {
        h.observations() == 2
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn event_guard_parks_rows_for_another_event() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.set_down(true);
    h.runtime.sim_pass(entry_id(), TAG_A).await.unwrap();
    eventually("the gate row to be saved", || async {
        h.store_of(entry_id()).count(OutboxState::Pending).unwrap() == 1
    })
    .await;

    // The key now belongs to a different event while work is still queued.
    h.set_event(2);
    h.set_down(false);
    eventually("the station to notice the different event", || async {
        h.runtime.stations().iter().any(|s| s.event_mismatch())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(h.observations(), 0, "nothing may reach the wrong event");
    let status = h.runtime.status().await.unwrap();
    let entry = status.stations.iter().find(|s| s.id == entry_id()).unwrap();
    assert_eq!(
        entry.last_error.as_deref(),
        Some(
            "This API key belongs to a different event. Open Setup and enter the key for this event."
        )
    );
    assert_eq!(
        h.store_of(entry_id()).count(OutboxState::Pending).unwrap(),
        1,
        "the row stays queued"
    );
    assert_eq!(
        h.saved_settings(entry_id()).unwrap().event.event_id,
        1,
        "the saved settings are not replaced while rows are waiting"
    );

    // Back on the original event, the same row is accepted.
    h.set_event(1);
    eventually("the queued row to be accepted", || async {
        h.observations() == 1
    })
    .await;

    // With nothing queued, a different event is adopted instead of blocking.
    h.set_event(2);
    eventually("the empty station to adopt event 2", || async {
        h.saved_settings(desk_id())
            .is_some_and(|s| s.event.event_id == 2)
            && h.runtime
                .stations()
                .iter()
                .all(|s| s.online() && s.event_ok())
    })
    .await;
    assert!(
        h.runtime.stations().iter().all(|s| !s.event_mismatch()),
        "an empty queue adopts the new event and stays online"
    );
    h.stop().await;
}

#[tokio::test]
async fn runtime_rejects_unusable_options() {
    let temp = tempfile::tempdir().unwrap();
    let paths = rfidex_runtime::AppPaths::new(temp.path().to_path_buf());
    let config = rfidex_runtime::AppConfig {
        server_url: "https://example.test".into(),
        api_key: common::KEY.into(),
        stations: vec![],
    };
    let zero = RuntimeOptions {
        heartbeat: Duration::ZERO,
        ..common::fast_options()
    };
    let err = match rfidex_runtime::Runtime::start(paths, config, zero).await {
        Ok(_) => panic!("zero-length intervals must be refused"),
        Err(e) => e,
    };
    assert_eq!(err.code, "bad_options");
}

#[tokio::test]
async fn simulator_commands_respect_station_kind_and_ownership() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    let entry = entry_id();

    assert_eq!(
        h.runtime.sim_pass(desk, TAG_A).await.unwrap_err().code,
        "wrong_station"
    );
    assert_eq!(
        h.runtime.sim_place(entry, TAG_A).await.unwrap_err().code,
        "wrong_station"
    );
    assert_eq!(
        h.runtime.sim_place(desk, "not hex").await.unwrap_err().code,
        "bad_sticker"
    );
    assert!(h.runtime.sim_place(desk, "3412cdab500104e0").await.is_ok());

    let err = h.runtime.sim_pass(entry, TAG_A).await.unwrap_err();
    assert_eq!(err.code, "sticker_in_use");
    assert!(err.message.contains("other reader") || err.message.contains("reader first"));

    h.runtime.sim_clear(desk).await.unwrap();
    h.runtime.sim_pass(entry, TAG_A).await.unwrap();
    eventually("the walked sticker to reach the server", || async {
        h.observations() == 1
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn a_disconnected_reader_is_not_hidden_by_info() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.sim_set_connected(desk_id(), false).await.unwrap();
    eventually("the desk to report its reader as gone", || async {
        !h.station(desk_id()).connected()
    })
    .await;

    h.runtime
        .sim_set_connected(entry_id(), false)
        .await
        .unwrap();
    eventually("the gate poll to report it as gone", || async {
        !h.station(entry_id()).connected()
    })
    .await;
    assert!(
        h.station(exit_id()).connected(),
        "one gate's problem is not another's"
    );

    h.runtime.sim_set_connected(entry_id(), true).await.unwrap();
    eventually("the gate to come back", || async {
        h.station(entry_id()).connected()
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn unknown_stickers_still_reach_the_server() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.sim_pass(entry_id(), TAG_C).await.unwrap();
    eventually("the unknown sticker to be sent", || async {
        h.observations() == 1
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn bind_flow_waits_for_scan_and_one_sticker() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();

    let before = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(before.step, DeskStep::Error);
    assert_eq!(before.code.as_deref(), Some("scan_first"));

    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    assert_eq!(scanned.step, DeskStep::Scanned);
    assert_eq!(scanned.ticket.as_ref().unwrap().name, "Aina");
    assert_eq!(scanned.message, "Place one sticker on the reader.");

    let waiting = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(waiting.step, DeskStep::Error);
    assert_eq!(waiting.code.as_deref(), Some("no_tag"));
    assert_eq!(
        waiting.ticket.as_ref().unwrap().name,
        "Aina",
        "a no-tag retry keeps the selected attendee"
    );

    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    let linked = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(linked.step, DeskStep::Linked);
    assert_eq!(linked.message, "Sticker linked.");
    assert_eq!(linked.ticket.as_ref().unwrap().name, "Aina");
    assert_eq!(
        h.server.mock.lock().unwrap().active_bindings()[0].public_id,
        ticket(1)
    );

    // Pressing Link again must not bind or write a second time.
    let again = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(again.step, DeskStep::Linked);
    assert_eq!(h.server.mock.lock().unwrap().active_bindings().len(), 1);

    let reset = h.runtime.desk_reset(desk).await.unwrap();
    assert_eq!(reset.step, DeskStep::Ready);
    assert_eq!(reset.message, "Scan a ticket to begin.");
    assert!(reset.ticket.is_none());
    h.stop().await;
}

#[tokio::test]
async fn a_failed_scan_cannot_link_the_previous_attendee() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();

    for (code, expected) in [
        (ticket(3).to_string(), "ticket_unpaid"),
        (ticket(4).to_string(), "ticket_cancelled"),
        (ticket(99).to_string(), "ticket_not_found"),
        ("not-a-uuid".to_string(), "ticket_not_found"),
    ] {
        let view = h.runtime.desk_scan(desk, &code).await.unwrap();
        assert_eq!(view.step, DeskStep::Error, "{code} should be rejected");
        assert_eq!(view.code.as_deref(), Some(expected));

        // Even with a sticker waiting, there is nobody to link.
        h.runtime.sim_place(desk, TAG_A).await.unwrap();
        let link = h.runtime.desk_link(desk, None).await.unwrap();
        assert_eq!(link.code.as_deref(), Some("scan_first"));
        h.runtime.sim_clear(desk).await.unwrap();
    }
    assert!(h.server.mock.lock().unwrap().active_bindings().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn multiple_stickers_never_write() {
    let mut h = Harness::start(RfidMode::Write).await;
    let desk = desk_id();
    eventually("the desk to switch to write mode", || async {
        h.station(desk).mode() == Some(RfidMode::Write)
    })
    .await;
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();

    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    h.runtime.sim_place(desk, TAG_B).await.unwrap();
    let view = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(view.step, DeskStep::Error);
    assert_eq!(view.code.as_deref(), Some("multiple_tags"));
    assert!(view.ticket.is_some(), "the retry needs no new scan");
    assert!(h.server.mock.lock().unwrap().active_bindings().is_empty());

    h.runtime.sim_clear(desk).await.unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    let linked = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(linked.step, DeskStep::Linked);
    h.stop().await;
}

#[tokio::test]
async fn confirm_is_required_and_binds_only_the_sticker_that_was_warned_about() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();

    // Aina takes TAG_A.
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    h.runtime.desk_link(desk, None).await.unwrap();

    // Ben scans, and the same sticker is offered.
    h.runtime.desk_reset(desk).await.unwrap();
    h.runtime
        .desk_scan(desk, &ticket(2).to_string())
        .await
        .unwrap();
    let warned = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(warned.step, DeskStep::NeedsConfirm);
    assert_eq!(warned.code.as_deref(), Some("sticker_in_use"));
    assert!(warned.message.contains("Aina"));
    assert_eq!(warned.ticket.as_ref().unwrap().name, "Ben");

    // A blank reason changes nothing.
    let blank = h
        .runtime
        .desk_link(desk, Some("   ".to_string()))
        .await
        .unwrap();
    assert_eq!(blank.step, DeskStep::NeedsConfirm);
    assert_eq!(
        h.server.mock.lock().unwrap().active_bindings()[0].public_id,
        ticket(1),
        "nothing was rebound"
    );

    // Swap the sticker on the reader and confirm: the authorisation for TAG_A
    // must not carry over, and TAG_B is unbound so it links cleanly.
    h.runtime.sim_clear(desk).await.unwrap();
    h.runtime.sim_place(desk, TAG_B).await.unwrap();
    let swapped = h
        .runtime
        .desk_link(desk, Some("badge swapped".to_string()))
        .await
        .unwrap();
    assert_eq!(swapped.step, DeskStep::Linked);
    assert_eq!(swapped.message, "Sticker linked.");
    let active = h.server.mock.lock().unwrap().active_bindings();
    assert_eq!(active.len(), 2, "Aina keeps TAG_A, Ben takes TAG_B");
    assert!(active
        .iter()
        .any(|b| b.tag_key == TAG_B && b.public_id == ticket(2)));

    // And now a real confirmation: Aina offers the sticker Ben is holding.
    h.runtime.desk_reset(desk).await.unwrap();
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_clear(desk).await.unwrap();
    h.runtime.sim_place(desk, TAG_B).await.unwrap();
    let warned_again = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(warned_again.step, DeskStep::NeedsConfirm);
    assert_eq!(warned_again.code.as_deref(), Some("sticker_in_use"));
    assert!(warned_again.message.contains("Ben"));
    let confirmed = h
        .runtime
        .desk_link(desk, Some("badge replaced".to_string()))
        .await
        .unwrap();
    assert_eq!(confirmed.step, DeskStep::Linked);
    let active = h.server.mock.lock().unwrap().active_bindings();
    assert_eq!(active.len(), 1, "the replace revoked Ben's link");
    assert_eq!(active[0].tag_key, TAG_B);
    assert_eq!(active[0].public_id, ticket(1));
    h.stop().await;
}

#[tokio::test]
async fn a_reason_without_a_warning_links_but_never_replaces() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();

    let view = h
        .runtime
        .desk_link(desk, Some("just because".to_string()))
        .await
        .unwrap();
    assert_eq!(view.step, DeskStep::Linked);
    assert_eq!(h.server.mock.lock().unwrap().active_bindings().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn an_offline_desk_never_prints_by_itself_and_offers_a_reprint() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.set_down(true);
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    assert_eq!(scanned.step, DeskStep::Scanned);
    assert!(scanned.offline);
    assert!(
        !scanned.badge.print_now,
        "an offline scan cannot know it was a first check-in"
    );
    assert!(scanned.badge.can_reprint && scanned.badge.hold);
    assert_eq!(
        scanned.badge.message.as_deref(),
        Some("Offline — press Reprint when back online")
    );

    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    let linked = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(linked.step, DeskStep::Linked);
    assert!(linked.offline);

    h.set_down(false);
    eventually("the queued desk work to drain", || async {
        let mock = h.server.mock.lock().unwrap();
        mock.scan_log_count == 1 && mock.active_bindings().len() == 1
    })
    .await;
    assert_eq!(
        h.printer(desk).count(),
        0,
        "reconnecting must not print on its own"
    );
    h.stop().await;
}

#[tokio::test]
async fn the_desk_refuses_to_work_before_the_first_heartbeat() {
    let mut h = Harness::start_with_server_down(RfidMode::Bind).await;
    let desk = desk_id();
    let view = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    assert_eq!(view.step, DeskStep::Error);
    assert_eq!(view.code.as_deref(), Some("connect_first"));
    assert!(view.message.contains("Connect to the server once"));

    // A different station kind is an IPC-level failure, not a desk message.
    let err = h
        .runtime
        .desk_scan(entry_id(), &ticket(1).to_string())
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrong_station");
    h.stop().await;
}

#[tokio::test]
async fn written_sticker_yields_welcome_and_goodbye() {
    let mut h = Harness::start(RfidMode::Write).await;
    let desk = desk_id();
    eventually("the desk to switch to write mode", || async {
        h.station(desk).mode() == Some(RfidMode::Write)
    })
    .await;

    // Desk: check in Aina and write her ticket into TAG_A.
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    let linked = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(linked.step, DeskStep::Linked);
    // The sticker leaves the reader physically, carrying what was written.
    h.runtime.sim_clear(desk).await.unwrap();

    h.runtime.sim_pass(entry_id(), TAG_A).await.unwrap();
    h.runtime.sim_pass(exit_id(), TAG_A).await.unwrap();
    // The server answering is not the same moment as the row being marked sent,
    // so wait for the screen rather than the mock's counter.
    eventually("both passages to be accepted", || async {
        let entry = h
            .runtime
            .gate_recent(entry_id(), 30)
            .await
            .unwrap_or_default();
        let exit = h
            .runtime
            .gate_recent(exit_id(), 30)
            .await
            .unwrap_or_default();
        entry
            .first()
            .is_some_and(|r| r.status == GateStatus::Accepted)
            && exit
                .first()
                .is_some_and(|r| r.status == GateStatus::Accepted)
    })
    .await;
    assert_eq!(h.observations(), 2);

    let entry = h.runtime.gate_recent(entry_id(), 30).await.unwrap();
    assert_eq!(entry.len(), 1);
    assert_eq!(entry[0].status, GateStatus::Accepted);
    assert_eq!(entry[0].role, Role::Entry);
    assert_eq!(entry[0].name.as_deref(), Some("Aina"));
    assert_eq!(entry[0].message, "Welcome");

    let exit = h.runtime.gate_recent(exit_id(), 30).await.unwrap();
    assert_eq!(exit.len(), 1);
    assert_eq!(exit[0].status, GateStatus::Accepted);
    assert_eq!(exit[0].role, Role::Exit);
    assert_eq!(exit[0].name.as_deref(), Some("Aina"));
    assert_eq!(exit[0].message, "Goodbye");

    // Acceptance alone could come from the UID binding, so prove the gate saw
    // the ticket that was actually written into the sticker.
    let store = h.store_of(entry_id());
    let rows = store
        .rows(
            &[OutboxState::Sent],
            Some(rfidex_core::store::OutboxKind::Observation),
            10,
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let payload_hex = rows[0].item.payload["payload_hex"]
        .as_str()
        .expect("the written payload reached the gate");
    let decoded =
        rfidex_core::codec::decode(&rfidex_core::tag::parse_hex(payload_hex).unwrap()).unwrap();
    assert_eq!(decoded, ticket(1));

    assert_eq!(h.runtime.gate_recent(entry_id(), 0).await.unwrap().len(), 0);
    let wrong = h.runtime.gate_recent(desk_id(), 10).await;
    assert_eq!(wrong.unwrap_err().code, "wrong_station");
    h.stop().await;
}

#[tokio::test]
async fn an_offline_passage_is_recorded_then_accepted_on_the_same_row() {
    let mut h = Harness::start(RfidMode::Write).await;
    let desk = desk_id();
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    h.runtime.desk_link(desk, None).await.unwrap();
    h.runtime.sim_clear(desk).await.unwrap();
    // The gates only learn a binding from their cache, so refresh it before
    // expecting them to know who is walking through.
    h.runtime.sync_now().await.unwrap();
    eventually("the gates to know the binding", || async {
        h.store_of(entry_id())
            .binding_holder(TAG_A)
            .unwrap()
            .is_some()
    })
    .await;

    h.set_down(true);
    h.runtime.sim_pass(entry_id(), TAG_A).await.unwrap();
    eventually("the passage to be recorded offline", || async {
        h.runtime
            .gate_recent(entry_id(), 5)
            .await
            .map(|rows| {
                rows.first()
                    .is_some_and(|r| r.status == GateStatus::Recorded)
            })
            .unwrap_or(false)
    })
    .await;
    let recorded = h.runtime.gate_recent(entry_id(), 5).await.unwrap();
    assert_eq!(recorded[0].name.as_deref(), Some("Aina"), "from the cache");
    assert_eq!(recorded[0].message, "Recorded — waiting for the server.");
    assert_eq!(h.observations(), 0, "nothing was sent while offline");
    let row_id = recorded[0].id;

    h.set_down(false);
    eventually("the same row to be accepted", || async {
        let _ = h.runtime.sync_now().await;
        h.runtime
            .gate_recent(entry_id(), 5)
            .await
            .map(|rows| {
                rows.first()
                    .is_some_and(|r| r.status == GateStatus::Accepted)
            })
            .unwrap_or(false)
    })
    .await;

    let accepted = h.runtime.gate_recent(entry_id(), 5).await.unwrap();
    assert_eq!(accepted[0].id, row_id, "the same local row changed state");
    assert_eq!(accepted[0].message, "Welcome");
    assert_eq!(accepted[0].name.as_deref(), Some("Aina"));
    assert_eq!(h.observations(), 1);
    h.stop().await;
}

#[tokio::test]
async fn an_offline_conflict_names_the_holder_and_can_be_dismissed() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();

    // Aina, offline, takes TAG_B.
    h.set_down(true);
    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_B).await.unwrap();
    let linked = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(linked.step, DeskStep::Linked);
    assert!(linked.offline);

    // Meanwhile the server gives TAG_B to Ben.
    h.server
        .mock
        .lock()
        .unwrap()
        .bind(rfidex_core::contract::BindingReq {
            public_id: ticket(2),
            protocol: rfidex_core::tag::Protocol::Iso15693,
            uid_raw_hex: TAG_B.into(),
            mode: rfidex_core::contract::BindMode::Bind,
            payload_version: None,
            operation_id: ticket(9001),
            captured_at: chrono::Utc::now(),
            replace: false,
            reason: None,
        })
        .unwrap();

    h.set_down(false);
    eventually("the queued link to conflict", || async {
        let _ = h.runtime.sync_now().await;
        h.store_of(desk).count(OutboxState::Conflict).unwrap() == 1
    })
    .await;

    let problems = h.runtime.problems().await.unwrap();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].station_id, desk);
    assert_eq!(problems[0].station_name, "Desk");
    assert_eq!(problems[0].name.as_deref(), Some("Ben"));
    assert!(problems[0].message.contains("already linked"));

    let status = h.runtime.status().await.unwrap();
    assert_eq!(status.problems, 1);
    assert!(status
        .alarm
        .as_deref()
        .unwrap_or_default()
        .contains("Some sticker links need attention."));

    assert!(h.runtime.dismiss(desk, problems[0].id).await.unwrap());
    assert!(
        !h.runtime.dismiss(desk, problems[0].id).await.unwrap(),
        "dismissing twice does nothing the second time"
    );
    assert!(h.runtime.problems().await.unwrap().is_empty());
    assert_eq!(h.runtime.status().await.unwrap().problems, 0);

    // The row is hidden, not gone: the server reply and the payload survive.
    let store = h.store_of(desk);
    let dismissed = store.rows(&[OutboxState::Dismissed], None, 10).unwrap();
    assert_eq!(dismissed.len(), 1);
    assert_eq!(
        dismissed[0].result.as_ref().unwrap()["holder"]["name"],
        "Ben"
    );
    assert_eq!(dismissed[0].item.payload["uid_raw_hex"], TAG_B);

    // A pending row is not something to dismiss; there is nothing to hide yet.
    h.set_down(true);
    h.runtime.sim_pass(entry_id(), TAG_C).await.unwrap();
    eventually("the observation to be saved", || async {
        h.store_of(entry_id()).count(OutboxState::Pending).unwrap() == 1
    })
    .await;
    let pending_row = h
        .store_of(entry_id())
        .rows(&[OutboxState::Pending], None, 1)
        .unwrap()[0]
        .item
        .id;
    assert!(!h.runtime.dismiss(entry_id(), pending_row).await.unwrap());
    h.stop().await;
}

#[tokio::test]
async fn a_rejected_key_is_unauthorized_rather_than_offline() {
    let mut h = Harness::start_with_bad_key(RfidMode::Bind).await;
    eventually("the station to report the key as rejected", || async {
        h.station(desk_id()).unauthorized()
    })
    .await;

    let status = h.runtime.status().await.unwrap();
    let desk = status.stations.iter().find(|s| s.id == desk_id()).unwrap();
    assert!(desk.unauthorized);
    assert!(!desk.online);
    assert!(desk.connected, "the reader is fine; the key is not");
    assert!(!desk.settings_ready);
    assert!(desk.mode.is_none());
    assert_eq!(
        desk.last_error.as_deref(),
        Some("The server did not accept the API key. Open Setup to check it.")
    );
    assert!(status.stations.iter().all(|s| s.simulated));
    h.stop().await;
}

#[tokio::test]
async fn an_empty_gate_denies_with_a_reason_and_never_a_welcome() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.sim_pass(entry_id(), TAG_C).await.unwrap();
    eventually("the unknown passage to be answered", || async {
        h.runtime
            .gate_recent(entry_id(), 5)
            .await
            .map(|rows| rows.first().is_some_and(|r| r.status == GateStatus::Denied))
            .unwrap_or(false)
    })
    .await;
    assert_eq!(h.observations(), 1);

    let rows = h.runtime.gate_recent(entry_id(), 5).await.unwrap();
    assert_eq!(rows[0].status, GateStatus::Denied);
    assert_eq!(rows[0].message, "Sticker not recognised.");
    assert!(rows[0].name.is_none(), "no fabricated name");
    h.stop().await;
}

/// Everything the connection check must not touch.
fn server_fingerprint(h: &Harness) -> (Vec<String>, usize, usize, usize) {
    let mock = h.server.mock.lock().unwrap();
    let mut stations: Vec<String> = mock.stations.keys().cloned().collect();
    stations.sort();
    (
        stations,
        mock.scan_log_count,
        mock.active_bindings().len(),
        mock.observation_count(),
    )
}

#[tokio::test]
async fn connection_test_reports_and_changes_nothing() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let before = server_fingerprint(&h);

    let good = rfidex_runtime::test_connection(&h.base, common::KEY).await;
    assert!(good.ok);
    assert_eq!(good.code, "connected");
    assert_eq!(good.message, "Connection works.");

    let bad = rfidex_runtime::test_connection(&h.base, "wrong_key_wrong_key_wrong_key_xx").await;
    assert!(!bad.ok);
    assert_eq!(bad.code, "unauthorized");

    for (url, key) in [
        ("not a url", common::KEY),
        ("http://example.test", common::KEY),
        (&h.base.clone(), "too_short"),
        (&h.base.clone(), "has spaces in it which is not allowed"),
    ] {
        let invalid = rfidex_runtime::test_connection(url, key).await;
        assert!(
            !invalid.ok,
            "{url} / {key} must be refused before any request"
        );
        assert_eq!(invalid.code, "invalid_setup");
        assert!(!invalid.message.is_empty());
    }

    let dead = rfidex_runtime::test_connection("http://127.0.0.1:9", common::KEY).await;
    assert!(!dead.ok);
    assert_eq!(dead.code, "offline");

    assert_eq!(
        before,
        server_fingerprint(&h),
        "a connection check must not register a station or change attendee state"
    );
    h.stop().await;
}

#[tokio::test]
async fn diagnostics_export_keeps_people_and_raw_uid_out() {
    let mut h = Harness::start(RfidMode::Write).await;
    let desk = desk_id();

    // A real link, a real accepted passage, and a real conflict to export.
    h.runtime
        .desk_scan(desk, &ticket(2).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    h.runtime.desk_link(desk, None).await.unwrap();
    h.runtime.sim_clear(desk).await.unwrap();
    h.runtime.sim_pass(exit_id(), TAG_A).await.unwrap();
    eventually("the passage to be stored as sent", || async {
        h.store_of(exit_id()).count(OutboxState::Sent).unwrap() == 1
    })
    .await;

    // Offline, Ely takes TAG_B; meanwhile the server gives TAG_B to Aina.
    h.set_down(true);
    h.runtime
        .desk_scan(desk, &ticket(5).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_B).await.unwrap();
    assert!(h.runtime.desk_link(desk, None).await.unwrap().offline);
    let bound = h
        .server
        .mock
        .lock()
        .unwrap()
        .bind(rfidex_core::contract::BindingReq {
            public_id: ticket(1),
            protocol: rfidex_core::tag::Protocol::Iso15693,
            uid_raw_hex: TAG_B.into(),
            mode: rfidex_core::contract::BindMode::Bind,
            payload_version: None,
            operation_id: ticket(9002),
            captured_at: chrono::Utc::now(),
            replace: false,
            reason: None,
        });
    assert!(bound.is_ok(), "the competing binding must be created");
    h.set_down(false);
    eventually("the conflict to land", || async {
        let _ = h.runtime.sync_now().await;
        h.store_of(desk).count(OutboxState::Conflict).unwrap() == 1
    })
    .await;

    let path = h.runtime.export_diagnostics().await.unwrap();
    assert!(path.starts_with(h.paths.exports()));
    let text = std::fs::read_to_string(&path).unwrap();

    for secret in [
        "Aina",
        "Ben",
        "Chong",
        "Devi",
        "Ely",
        common::KEY,
        TAG_A,
        TAG_B,
        &ticket(1).to_string(),
        &ticket(2).to_string(),
        &ticket(5).to_string(),
        "uid_raw_hex",
        "payload_hex",
        "holder",
        "api_key",
    ] {
        assert!(
            !text.contains(secret),
            "diagnostics must not contain {secret:?}"
        );
    }

    assert!(text.starts_with(
        "station_id,row_id,kind,state,captured_at,attempts,uid_last4,outcome,problem_code\r\n"
    ));
    assert!(
        text.contains("04E0"),
        "a short UID suffix is kept for support"
    );
    assert!(text.contains("observation"));
    assert!(text.contains("binding"));
    assert!(text.contains("accepted"));
    assert!(text.contains("uid_bound_elsewhere"));
    assert!(text.contains(&desk.to_string()));

    // A second export is a second file, never an overwrite.
    let second = h.runtime.export_diagnostics().await.unwrap();
    assert_ne!(path, second);
    assert!(path.exists() && second.exists());
    h.stop().await;
}

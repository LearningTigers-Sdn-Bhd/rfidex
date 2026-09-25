//! The runtime against a real mock server over HTTP: station identity, the
//! lifecycle, restart behaviour, and the event guard.

mod common;

use std::time::Duration;

use common::{desk_id, entry_id, eventually, exit_id, Harness, TAG_A, TAG_B, TAG_C};
use rfidex_core::contract::{RfidMode, Role, StationKind};
use rfidex_core::store::OutboxState;
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

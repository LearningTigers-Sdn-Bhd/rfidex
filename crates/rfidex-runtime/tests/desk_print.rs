//! Badge printing at the desk: one print per live check-in, never in the way of
//! the sticker, and never anything the staff did not ask for.

mod common;

use std::time::Duration;

use common::*;
use rfidex_core::contract::RfidMode;
use rfidex_mock::printer::Mode;
use rfidex_runtime::config::StationConfig;
use rfidex_runtime::DeskStep;
use uuid::Uuid;

const PRINTING: &str = "Printing badge…";
const SENT: &str = "Badge sent to printer";
const FAILED: &str = "Badge not printed — press Reprint";
const OFFLINE: &str = "Offline — press Reprint when back online";

fn path_for(ticket: Uuid) -> String {
    format!("/scan/{ticket}/reprint")
}

#[tokio::test]
async fn a_first_check_in_asks_for_one_print_and_nothing_prints_by_itself() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();

    assert!(
        scanned.badge.print_now,
        "a new online check-in is what printing is for"
    );
    assert_eq!(scanned.badge.message.as_deref(), Some(PRINTING));
    assert_eq!(
        h.printer(desk).count(),
        0,
        "scanning alone must not print: the UI calls desk_print"
    );

    let badge = h
        .runtime
        .desk_print(desk, scanned.session_id)
        .await
        .unwrap();
    assert_eq!(badge.message.as_deref(), Some(SENT));
    assert!(!badge.hold, "the guest may leave the screen now");
    assert!(!badge.can_reprint);
    assert!(!badge.print_now);

    let requests = h.printer(desk).requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, path_for(ticket(1)));
}

#[tokio::test]
async fn a_held_print_never_holds_up_the_sticker() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.printer(desk).set_mode(Mode::Held);
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();

    let printing = h.runtime.desk_print(desk, scanned.session_id);
    tokio::pin!(printing);
    tokio::select! {
        done = &mut printing => panic!("the print cannot have finished yet: {done:?}"),
        seen = h.printer(desk).wait_for_request() => assert_eq!(seen, 1),
    }

    // The desk lock is free while the print is in flight: the sticker links.
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    let linked = tokio::time::timeout(Duration::from_secs(2), h.runtime.desk_link(desk, None))
        .await
        .expect("linking must not wait for the printer")
        .unwrap();
    assert_eq!(linked.step, DeskStep::Linked);
    assert!(!linked.offline);
    assert_eq!(
        h.printer(desk).count(),
        1,
        "the sticker step sent no print request"
    );

    h.printer(desk).release();
    let badge = printing.await.unwrap();
    assert_eq!(badge.message.as_deref(), Some(SENT));
    assert_eq!(h.printer(desk).count(), 1);
}

#[tokio::test]
async fn a_rescan_never_prints_by_itself_but_a_reprint_does() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    let first = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.desk_print(desk, first.session_id).await.unwrap();
    assert_eq!(h.printer(desk).count(), 1);

    let second = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    assert_ne!(
        second.session_id, first.session_id,
        "the guest on screen is a new session"
    );
    assert!(
        !second.badge.print_now,
        "an already-in guest never auto-prints"
    );
    assert!(second.badge.can_reprint && second.badge.hold);
    let message = second.badge.message.as_deref().unwrap();
    assert!(message.starts_with("Already checked in at "), "{message}");
    assert_eq!(
        h.printer(desk).count(),
        1,
        "the rescan itself sent no print"
    );

    let reprinted = h.runtime.desk_print(desk, second.session_id).await.unwrap();
    assert_eq!(reprinted.message.as_deref(), Some(SENT));
    assert_eq!(h.printer(desk).count(), 2);
}

#[tokio::test]
async fn printer_trouble_keeps_the_guest_and_offers_a_reprint() {
    for mode in [Mode::ServerError, Mode::BadJson, Mode::NotOk] {
        let h = Harness::start(RfidMode::Bind).await;
        let desk = desk_id();
        h.printer(desk).set_mode(mode.clone());
        let scanned = h
            .runtime
            .desk_scan(desk, &ticket(1).to_string())
            .await
            .unwrap();

        let badge = h
            .runtime
            .desk_print(desk, scanned.session_id)
            .await
            .unwrap();
        assert_eq!(badge.message.as_deref(), Some(FAILED), "{mode:?}");
        assert!(badge.hold, "{mode:?} must keep the guest on screen");
        assert!(badge.can_reprint, "{mode:?}");
        assert!(!badge.print_now, "{mode:?}");

        // Nothing about the check-in or the sticker changed.
        h.runtime.sim_place(desk, TAG_A).await.unwrap();
        let linked = h.runtime.desk_link(desk, None).await.unwrap();
        assert_eq!(linked.step, DeskStep::Linked, "{mode:?}");
        assert_eq!(
            h.server.mock.lock().unwrap().active_bindings()[0].public_id,
            ticket(1)
        );

        let again = h
            .runtime
            .desk_print(desk, scanned.session_id)
            .await
            .unwrap();
        assert!(again.hold && again.can_reprint);
        assert_eq!(
            h.printer(desk).count(),
            2,
            "{mode:?}: one request per press"
        );
    }
}

#[tokio::test]
async fn two_presses_while_a_print_is_in_flight_send_one_request() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.printer(desk).set_mode(Mode::Held);
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();

    let printing = h.runtime.desk_print(desk, scanned.session_id);
    tokio::pin!(printing);
    tokio::select! {
        done = &mut printing => panic!("the print cannot have finished yet: {done:?}"),
        _ = h.printer(desk).wait_for_request() => {}
    }

    let second = h
        .runtime
        .desk_print(desk, scanned.session_id)
        .await
        .unwrap();
    assert_eq!(second.message.as_deref(), Some(PRINTING));
    assert_eq!(
        h.printer(desk).count(),
        1,
        "a second press while one is running is not a second job"
    );

    h.printer(desk).release();
    assert_eq!(printing.await.unwrap().message.as_deref(), Some(SENT));
    assert_eq!(h.printer(desk).count(), 1);
}

#[tokio::test]
async fn a_stale_session_prints_nothing_at_all() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    let first = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime
        .desk_scan(desk, &ticket(2).to_string())
        .await
        .unwrap();

    let error = h
        .runtime
        .desk_print(desk, first.session_id)
        .await
        .unwrap_err();
    assert_eq!(error.code, "stale_session");
    assert_eq!(
        h.printer(desk).count(),
        0,
        "a late answer must never print for another guest"
    );
}

#[tokio::test]
async fn a_queued_offline_check_in_prints_only_when_staff_ask_after_reconnecting() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.set_down(true);
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    assert!(scanned.offline);
    assert!(!scanned.badge.print_now);
    assert!(scanned.badge.can_reprint && scanned.badge.hold);
    assert_eq!(scanned.badge.message.as_deref(), Some(OFFLINE));
    eventually("the station to notice the outage", || async {
        !h.station(desk).online()
    })
    .await;

    let refused = h
        .runtime
        .desk_print(desk, scanned.session_id)
        .await
        .unwrap();
    assert_eq!(refused.message.as_deref(), Some(OFFLINE));
    assert_eq!(h.printer(desk).count(), 0, "no print while offline");

    h.set_down(false);
    eventually("the queued check-in to reach the server", || async {
        h.server.mock.lock().unwrap().scan_log_count == 1
    })
    .await;
    h.runtime.sync_now().await.unwrap();
    assert_eq!(
        h.printer(desk).count(),
        0,
        "draining, refreshing or reconnecting never prints by itself"
    );

    let badge = h
        .runtime
        .desk_print(desk, scanned.session_id)
        .await
        .unwrap();
    assert_eq!(badge.message.as_deref(), Some(SENT));
    assert_eq!(h.printer(desk).count(), 1);

    h.restart_runtime().await;
    assert_eq!(
        h.printer(desk).count(),
        1,
        "a restart never prints, and never prints twice"
    );
}

#[tokio::test]
async fn each_desk_prints_only_on_its_own_printer() {
    let second = Uuid::from_u128(201);
    let mut stations = three_stations();
    stations.push(StationConfig {
        id: second,
        name: "Second desk".into(),
        ..desk_station()
    });
    let h = Harness::start_with(RfidMode::Bind, fast_options(), stations).await;

    let first_scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    let second_scan = h
        .runtime
        .desk_scan(second, &ticket(2).to_string())
        .await
        .unwrap();
    h.runtime
        .desk_print(desk_id(), first_scan.session_id)
        .await
        .unwrap();
    h.runtime
        .desk_print(second, second_scan.session_id)
        .await
        .unwrap();

    assert_eq!(h.printer(desk_id()).count(), 1);
    assert_eq!(h.printer(second).count(), 1);
    assert_eq!(
        h.printer(desk_id()).requests()[0].public_id,
        Some(ticket(1)),
        "a desk may only ask its own printer for its own guest"
    );
    assert_eq!(h.printer(second).requests()[0].public_id, Some(ticket(2)));

    // A stale session on one desk cannot print on either.
    let error = h
        .runtime
        .desk_print(second, first_scan.session_id)
        .await
        .unwrap_err();
    assert_eq!(error.code, "stale_session");
    assert_eq!(h.printer(second).count(), 1);
}

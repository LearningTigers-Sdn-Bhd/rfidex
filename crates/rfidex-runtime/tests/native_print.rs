mod common;
use common::*;
use rfidex_core::contract::RfidMode;
use rfidex_runtime::{BadgeSettings, RuntimeError};

#[tokio::test]
async fn the_events_ticket_types_come_from_the_local_cache() {
    let mut h = Harness::start(RfidMode::Bind).await;
    assert_eq!(h.runtime.ticket_types(), vec!["VIP".to_string()]);
    h.stop().await;
}

#[tokio::test]
async fn default_off_keeps_legacy_and_on_selects_native_without_fallback() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime
        .desk_print(desk_id(), scan.session_id)
        .await
        .unwrap();
    assert_eq!(h.printer(desk_id()).count(), 1);
    assert_eq!(h.printing.jobs().len(), 0);
    assert!(h.server.public_requests.lock().unwrap().is_empty());
    h.runtime
        .badge()
        .save(BadgeSettings {
            printer: "Zebra".into(),
            ..Default::default()
        })
        .unwrap();
    h.runtime.badge().set_enabled(true).unwrap();
    h.runtime
        .desk_print(desk_id(), scan.session_id)
        .await
        .unwrap();
    assert_eq!(h.printing.jobs()[0].document, ticket(1).to_string());
    assert_eq!(h.printing.jobs()[0].printer, "Zebra");
    assert_eq!(h.printer(desk_id()).count(), 1);
    let rescan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    assert!(!rescan.badge.print_now);
    h.printing.set_mode(rfidex_mock::badge::Mode::Fail(
        rfidex_badge::print::PrintError::PrinterUnavailable,
    ));
    let failed = h
        .runtime
        .desk_print(desk_id(), rescan.session_id)
        .await
        .unwrap();
    assert!(failed.message.unwrap().contains("cannot be reached"));
    assert_eq!(h.printer(desk_id()).count(), 1);
    h.stop().await;
}

#[tokio::test]
async fn off_during_public_fetch_cancels_even_if_turned_on_again() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.badge().set_enabled(true).unwrap();
    let scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    h.server
        .public_delay_ms
        .store(250, std::sync::atomic::Ordering::SeqCst);
    {
        let print = h.runtime.desk_print(desk_id(), scan.session_id);
        tokio::pin!(print);
        tokio::select! {
            _ = &mut print => panic!("fetch should be held"),
            _ = eventually("public fetch",||async{!h.server.public_requests.lock().unwrap().is_empty()}) => {}
        }
        h.runtime.badge().set_enabled(false).unwrap();
        h.runtime.badge().set_enabled(true).unwrap();
        assert!(print.await.unwrap().message.unwrap().contains("cancelled"));
        assert_eq!(h.printing.jobs().len(), 0);
        assert_eq!(h.printer(desk_id()).count(), 0);
    }
    h.stop().await;
}

#[tokio::test]
async fn held_native_job_keeps_one_guard_and_sticker_linking_works_after_off() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.badge().set_enabled(true).unwrap();
    h.printing
        .set_mode(rfidex_mock::badge::Mode::HeldAfterSubmit);
    let scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    {
        let print = h.runtime.desk_print(desk_id(), scan.session_id);
        tokio::pin!(print);
        tokio::select! {
            _ = &mut print => panic!("job should be held"),
            _ = h.printing.wait_for_request() => {}
        }
        h.runtime.badge().set_enabled(false).unwrap();
        let duplicate = h
            .runtime
            .desk_print(desk_id(), scan.session_id)
            .await
            .unwrap();
        assert!(!duplicate.can_reprint);
        assert_eq!(h.printer(desk_id()).count(), 0);
        h.runtime.sim_place(desk_id(), TAG_A).await.unwrap();
        let linked = h.runtime.desk_link(desk_id(), None).await.unwrap();
        assert_eq!(linked.step, rfidex_runtime::DeskStep::Linked);
        h.printing.release();
        print.await.unwrap();
        assert_eq!(h.printing.jobs().len(), 1);
    }
    h.stop().await;
}

#[tokio::test]
async fn off_during_render_cancels_submission_and_stale_session_never_prints() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.badge().set_enabled(true).unwrap();
    h.printing
        .set_mode(rfidex_mock::badge::Mode::HeldBeforeSubmit);
    let scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    {
        let print = h.runtime.desk_print(desk_id(), scan.session_id);
        tokio::pin!(print);
        tokio::select! {
            _ = &mut print => panic!("render should be held"),
            _ = h.printing.wait_for_request() => {}
        }
        h.runtime.badge().set_enabled(false).unwrap();
        h.printing.release();
        assert!(print.await.unwrap().message.unwrap().contains("cancelled"));
        assert!(h.printing.jobs().is_empty());
        h.runtime
            .desk_scan(desk_id(), &ticket(2).to_string())
            .await
            .unwrap();
        let RuntimeError { code, .. } = h
            .runtime
            .desk_print(desk_id(), scan.session_id)
            .await
            .unwrap_err();
        assert_eq!(code, "stale_session");
    }
    h.stop().await;
}

#[tokio::test]
async fn two_desks_share_native_printer_and_custom_fields_reach_rendering() {
    let second = uuid::Uuid::from_u128(201);
    let mut stations = three_stations();
    stations.push(rfidex_runtime::StationConfig {
        id: second,
        name: "Second desk".into(),
        ..desk_station()
    });
    let mut h = Harness::start_with(RfidMode::Bind, fast_options(), stations).await;
    let badge = h.runtime.badge();
    let mut settings = BadgeSettings {
        printer: "Zebra".into(),
        thermal: true,
        ..Default::default()
    };
    settings.layout.elements = vec!["company".into()];
    badge.save(settings).unwrap();
    badge.set_enabled(true).unwrap();
    h.server
        .mock
        .lock()
        .unwrap()
        .set_badge_fields(ticket(1), serde_json::json!({"company":"Borneo Expo"}));
    let first = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    let next = h
        .runtime
        .desk_scan(second, &ticket(2).to_string())
        .await
        .unwrap();
    h.runtime
        .desk_print(desk_id(), first.session_id)
        .await
        .unwrap();
    h.runtime.desk_print(second, next.session_id).await.unwrap();
    let jobs = h.printing.jobs();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].document, ticket(1).to_string());
    assert_eq!(jobs[1].document, ticket(2).to_string());
    assert!(
        jobs[0].dark_pixels > jobs[1].dark_pixels,
        "company fields must reach the renderer"
    );
    assert!(jobs
        .iter()
        .all(|job| job.thermal && job.printer == "Zebra" && job.paper_tenths_mm == (1000, 800)));
    assert_eq!(h.printer(desk_id()).count(), 0);
    assert_eq!(h.printer(second).count(), 0);
    h.stop().await;
}

#[tokio::test]
async fn offline_reconnect_and_restart_never_replay_native_prints() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.badge().set_enabled(true).unwrap();
    h.set_down(true);
    eventually("offline", || async { !h.station(desk_id()).online() }).await;
    let scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    assert!(!scan.badge.print_now);
    let badge = h
        .runtime
        .desk_print(desk_id(), scan.session_id)
        .await
        .unwrap();
    assert!(badge.message.unwrap().contains("Offline"));
    assert!(h.printing.jobs().is_empty());
    h.set_down(false);
    h.await_online().await;
    h.runtime.sync_now().await.unwrap();
    assert!(h.printing.jobs().is_empty());
    h.runtime
        .desk_print(desk_id(), scan.session_id)
        .await
        .unwrap();
    assert_eq!(h.printing.jobs().len(), 1);
    h.restart_runtime().await;
    assert!(h.runtime.badge().selection().is_some());
    h.runtime.sync_now().await.unwrap();
    assert_eq!(h.printing.jobs().len(), 1);
    h.runtime.badge().set_enabled(false).unwrap();
    h.restart_runtime().await;
    assert!(h.runtime.badge().selection().is_none());
    let rescan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    assert!(!rescan.badge.print_now);
    h.runtime
        .desk_print(desk_id(), rescan.session_id)
        .await
        .unwrap();
    assert_eq!(
        h.printer(desk_id()).count(),
        1,
        "Off restores retained legacy settings"
    );
    assert_eq!(h.printing.jobs().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn late_fetch_never_prints_for_replaced_guest() {
    let mut h = Harness::start(RfidMode::Bind).await;
    h.runtime.badge().set_enabled(true).unwrap();
    let first = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    h.server
        .public_delay_ms
        .store(250, std::sync::atomic::Ordering::SeqCst);
    {
        let print = h.runtime.desk_print(desk_id(), first.session_id);
        tokio::pin!(print);
        tokio::select! {
            _ = &mut print => panic!("fetch should be held"),
            _ = eventually("public fetch",||async{!h.server.public_requests.lock().unwrap().is_empty()}) => {}
        }
        let next = h
            .runtime
            .desk_scan(desk_id(), &ticket(2).to_string())
            .await
            .unwrap();
        let result = print.await.unwrap();
        assert_eq!(result, next.badge);
        assert!(h.printing.jobs().is_empty());
    }
    h.stop().await;
}

#[tokio::test]
async fn sideways_roll_is_saved_and_used_by_desk_manual_and_test_prints() {
    let mut h = Harness::start(RfidMode::Bind).await;
    let mut json = serde_json::to_value(BadgeSettings::default()).unwrap();
    json["rotate_90"] = true.into();
    h.runtime
        .badge()
        .save(serde_json::from_value(json).unwrap())
        .unwrap();
    h.restart_runtime().await;
    assert_eq!(
        serde_json::to_value(h.runtime.badge().view().settings).unwrap()["rotate_90"],
        true
    );
    h.runtime.badge().set_enabled(true).unwrap();
    let scan = h
        .runtime
        .desk_scan(desk_id(), &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime
        .desk_print(desk_id(), scan.session_id)
        .await
        .unwrap();
    h.runtime
        .badge()
        .print_ticket(&rfidex_badge::layout::Ticket::default(), "manual")
        .unwrap();
    h.runtime.badge().test_print().unwrap();
    let jobs = h.printing.jobs();
    assert_eq!(jobs.len(), 3);
    assert!(jobs.iter().all(|job| job.paper_tenths_mm == (800, 1000)));
    h.stop().await;
}

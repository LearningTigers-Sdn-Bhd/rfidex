use chrono::{Duration as ChronoDuration, Utc};
use rfidex_core::contract::*;
use rfidex_core::health::{health, Alarm};
use rfidex_core::store::{OutboxKind, OutboxState, Store};
use rfidex_core::sync::SyncWorker;
use rfidex_core::tag::Protocol;
use serde_json::to_value;

mod common;
use common::{id, TAG_A, TAG_B};

fn obs(n: u128, uid: &str) -> ObservationItem {
    ObservationItem {
        delivery_id: id(3000 + n),
        role: Role::Entry,
        protocol: Protocol::Iso15693,
        uid_raw_hex: uid.into(),
        payload_hex: None,
        device_direction_raw: None,
        device_time_raw_hex: None,
        device_record_seq: None,
        flags_raw: serde_json::Value::Null,
        captured_at: Utc::now(),
    }
}

fn enqueue_obs(s: &Store, o: &ObservationItem) {
    s.enqueue(
        OutboxKind::Observation,
        &o.delivery_id.to_string(),
        &to_value(o).unwrap(),
        o.captured_at,
    )
    .unwrap();
}

fn bind_req(ticket: u128, uid: &str, op: u128) -> BindingReq {
    BindingReq {
        public_id: id(ticket),
        protocol: Protocol::Iso15693,
        uid_raw_hex: uid.into(),
        mode: BindMode::Bind,
        payload_version: None,
        operation_id: id(2000 + op),
        captured_at: Utc::now(),
        replace: false,
        reason: None,
    }
}

#[tokio::test]
async fn outage_then_drain_in_order() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    let store = common::store();
    for n in 0..120 {
        enqueue_obs(&store.lock().unwrap(), &obs(n, TAG_A));
    }
    let worker = SyncWorker::new(store.clone(), common::client(&base, "gate-in"));

    state.faults.lock().unwrap().down = true;
    let r = worker.run_once(Utc::now()).await.unwrap();
    assert_eq!(r.sent, 0);
    assert_eq!(
        r.retried, 50,
        "only the first batch is attempted during an outage"
    );

    state.faults.lock().unwrap().down = false;
    let r = worker
        .run_once(Utc::now() + ChronoDuration::seconds(120))
        .await
        .unwrap();
    assert_eq!(r.sent, 120);
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        0
    );
    let order = state.mock.lock().unwrap().received_order.clone();
    assert_eq!(order, (0..120).map(|n| id(3000 + n)).collect::<Vec<_>>());
}

#[tokio::test]
async fn timeout_after_commit_creates_one_server_row() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    let store = common::store();
    enqueue_obs(&store.lock().unwrap(), &obs(1, TAG_A));
    let worker = SyncWorker::new(store.clone(), common::client(&base, "gate-in"));

    state.faults.lock().unwrap().hang_after_commit = 1;
    assert_eq!(worker.run_once(Utc::now()).await.unwrap().retried, 1);
    let r = worker
        .run_once(Utc::now() + ChronoDuration::seconds(120))
        .await
        .unwrap();
    assert_eq!(r.sent, 1);
    assert_eq!(state.mock.lock().unwrap().observation_count(), 1);
}

#[tokio::test]
async fn poison_is_parked_conflict_is_kept_and_queue_continues() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    state
        .mock
        .lock()
        .unwrap()
        .bind(bind_req(1, TAG_A, 1))
        .unwrap();
    let store = common::store();
    {
        let s = store.lock().unwrap();
        for req in [
            bind_req(99, TAG_B, 2),
            bind_req(2, TAG_A, 3),
            bind_req(2, TAG_B, 4),
        ] {
            s.enqueue(
                OutboxKind::Binding,
                &req.operation_id.to_string(),
                &to_value(&req).unwrap(),
                req.captured_at,
            )
            .unwrap();
        }
    }
    let worker = SyncWorker::new(store.clone(), common::client(&base, "desk-1"));
    let r = worker.run_once(Utc::now()).await.unwrap();
    assert_eq!((r.parked, r.conflicts, r.sent), (1, 1, 1));
    let s = store.lock().unwrap();
    let conflicts = s.items(OutboxState::Conflict, 10).unwrap();
    assert_eq!(
        conflicts[0].1.as_ref().unwrap()["error"],
        "uid_bound_elsewhere"
    );
    assert_eq!(
        s.binding_holder(TAG_B).unwrap(),
        Some(id(2)),
        "sent binding applied to local cache"
    );
}

#[tokio::test]
async fn unauthorized_stops_the_pass() {
    let (base, _state) = common::spawn(RfidMode::Bind).await;
    let store = common::store();
    enqueue_obs(&store.lock().unwrap(), &obs(1, TAG_A));
    let bad = rfidex_core::client::ApiClient::new(
        &base,
        "revoked_key_revoked_key_revoked_",
        "gate-in",
        std::time::Duration::from_millis(500),
    );
    let r = SyncWorker::new(store.clone(), bad)
        .run_once(Utc::now())
        .await
        .unwrap();
    assert!(r.unauthorized);
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        1
    );
}

#[tokio::test]
async fn refresh_cache_and_health_alarms() {
    let (base, _state) = common::spawn(RfidMode::Bind).await;
    let store = common::store();
    let worker = SyncWorker::new(store.clone(), common::client(&base, "desk-1"));
    worker.refresh_cache().await.unwrap();
    assert_eq!(
        store.lock().unwrap().ticket(id(1)).unwrap().unwrap().name,
        "Aina"
    );

    let s = store.lock().unwrap();
    for n in 0..3 {
        enqueue_obs(&s, &obs(n, TAG_A));
    }
    let h = health(&s, Some(100 * 1024 * 1024), 3).unwrap();
    assert_eq!(h.pending, 3);
    assert!(h.alarms.contains(&Alarm::DeepQueue(3)));
    assert!(h.alarms.contains(&Alarm::LowDisk(100 * 1024 * 1024)));
    assert!(health(&s, None, 10).unwrap().alarms.is_empty());
}

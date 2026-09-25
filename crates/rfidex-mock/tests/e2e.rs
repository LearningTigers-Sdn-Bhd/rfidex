//! One ticket through the whole system on simulators: desk (write mode) →
//! entry gate while server is down → exit gate → sync → mock outcomes.

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use rfidex_core::contract::*;
use rfidex_core::device::sim_desk::{SimDesk, SimTag};
use rfidex_core::device::sim_gate::SimGate;
use rfidex_core::device::{GateKind, GateRead};
use rfidex_core::station::desk::DeskStation;
use rfidex_core::station::gate::{GateStation, LocalGuess};
use rfidex_core::store::OutboxState;
use rfidex_core::sync::SyncWorker;
use rfidex_core::tag::{parse_hex, UidRule};

mod common;
use common::{id, TAG_A};

#[tokio::test]
async fn desk_to_gates_to_server() {
    let (base, state) = common::spawn(RfidMode::Write).await;
    let store = common::store();

    // Desk: check in + write + bind, online.
    let mut desk = DeskStation::new(
        SimDesk::new(),
        store.clone(),
        common::client(&base, "desk-1"),
        RfidMode::Write,
        UidRule::AsIs,
        0,
    );
    let aina = desk.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    desk.reader
        .place(SimTag::blank(parse_hex(TAG_A).unwrap(), 4, 28));
    let tag = desk.detect_tag().unwrap();
    desk.link(&aina, &tag, None).await.unwrap();
    let sticker = desk.reader.clear().remove(0);
    let pass = |seq: u64| {
        let mut r = GateRead::sighting(sticker.uid_raw.clone());
        r.device_record_seq = Some(seq);
        r.payload = Some(sticker.memory[..20].to_vec());
        r
    };

    let mut entry = GateStation::new(
        SimGate::new(GateKind::Records, false),
        store.clone(),
        "gate-in",
        Role::Entry,
        UidRule::AsIs,
        Duration::from_secs(5),
    );
    let mut exit = GateStation::new(
        SimGate::new(GateKind::Records, false),
        store.clone(),
        "gate-out",
        Role::Exit,
        UidRule::AsIs,
        Duration::from_secs(5),
    );
    let worker = SyncWorker::new(store.clone(), common::client(&base, "gate-pc"));

    // Entry while the server is down: shown from cache, queued.
    state.faults.lock().unwrap().down = true;
    entry.gate.push(pass(1));
    let seen = entry.tick(Utc::now()).unwrap();
    assert_eq!(
        seen[0].local,
        LocalGuess::Known {
            name: "Aina".into(),
            ticket_type: "VIP".into()
        }
    );
    assert_eq!(worker.run_once(Utc::now()).await.unwrap().sent, 0);

    // Server back; exit; drain.
    state.faults.lock().unwrap().down = false;
    exit.gate.push(pass(2));
    exit.tick(Utc::now()).unwrap();
    let r = worker
        .run_once(Utc::now() + ChronoDuration::seconds(120))
        .await
        .unwrap();
    assert_eq!(r.sent, 2);

    let s = store.lock().unwrap();
    assert_eq!(s.count(OutboxState::Pending).unwrap(), 0);
    let outcomes: Vec<_> = s
        .items(OutboxState::Sent, 10)
        .unwrap()
        .into_iter()
        .filter_map(|(item, res)| {
            (item.kind == rfidex_core::store::OutboxKind::Observation)
                .then(|| res.unwrap()["outcome"].clone())
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![serde_json::json!("accepted"), serde_json::json!("accepted")]
    );
    let m = state.mock.lock().unwrap();
    assert_eq!(m.observation_count(), 2);
    assert_eq!(m.scan_log_count, 1);
}

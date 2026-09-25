use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use rfidex_core::contract::*;
use rfidex_core::device::sim_gate::SimGate;
use rfidex_core::device::{GateKind, GateRead};
use rfidex_core::station::gate::{GateStation, LocalGuess};
use rfidex_core::store::{OutboxState, Store};
use rfidex_core::tag::{parse_hex, UidRule};

mod common;
use common::{id, TAG_A};

fn read(seq: Option<u64>) -> GateRead {
    let mut r = GateRead::sighting(parse_hex(TAG_A).unwrap());
    r.device_record_seq = seq;
    r
}

fn station(gate: SimGate, store: Arc<Mutex<Store>>) -> GateStation<SimGate> {
    GateStation::new(
        gate,
        store,
        "gate-in",
        Role::Entry,
        UidRule::AsIs,
        Duration::from_secs(5),
    )
}

#[test]
fn release_happens_only_after_commit() {
    let store = common::store();
    let mut gate = SimGate::new(GateKind::Records, true);
    let probe = store.clone();
    gate.on_release = Some(Box::new(move |_| {
        let pending = probe.lock().unwrap().count(OutboxState::Pending).unwrap();
        assert!(pending >= 1, "release before local commit");
    }));
    gate.push(read(Some(1)));
    gate.push(read(Some(2)));
    let mut g = station(gate, store.clone());
    assert_eq!(g.tick(Utc::now()).unwrap().len(), 2);
    assert_eq!(g.gate.released.len(), 2);
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        2
    );
}

#[test]
fn unverified_gate_is_never_released() {
    let store = common::store();
    let mut gate = SimGate::new(GateKind::Records, false);
    gate.push(read(Some(1)));
    let mut g = station(gate, store);
    assert_eq!(
        g.tick(Utc::now()).unwrap().len(),
        1,
        "SimGate errors if release is called"
    );
    assert!(g.gate.released.is_empty());
}

#[test]
fn redelivered_records_are_stored_once() {
    let store = common::store();
    let mut gate = SimGate::new(GateKind::Records, false);
    gate.redeliver_unreleased = true;
    gate.push(read(Some(42)));
    let mut g = station(gate, store.clone());
    assert_eq!(g.tick(Utc::now()).unwrap().len(), 1);
    assert_eq!(g.tick(Utc::now()).unwrap().len(), 0);
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        1
    );
}

#[test]
fn live_inventory_debounce() {
    let store = common::store();
    let mut g = station(SimGate::new(GateKind::LiveInventory, false), store.clone());
    let t0 = Utc::now();
    for secs in [0, 1, 4, 10] {
        g.gate.push(read(None));
        g.tick(t0 + ChronoDuration::seconds(secs)).unwrap();
    }
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        2
    );
}

#[test]
fn captured_rows_survive_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rfidex.db");
    {
        let store = Arc::new(Mutex::new(Store::open(&path).unwrap()));
        let mut gate = SimGate::new(GateKind::Records, false);
        gate.push(read(Some(7)));
        station(gate, store.clone()).tick(Utc::now()).unwrap();
        std::mem::forget(store); // crash: no clean shutdown, nothing synced
    }
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .count(OutboxState::Pending)
            .unwrap(),
        1
    );
}

#[test]
fn local_guess_from_cache() {
    let store = common::store();
    {
        let s = store.lock().unwrap();
        s.upsert_ticket(&TicketSummary {
            public_id: id(1),
            name: "Aina".into(),
            ticket_type: "VIP".into(),
            valid: true,
            checked_in: true,
        })
        .unwrap();
        s.upsert_binding(TAG_A, id(1)).unwrap();
    }
    let mut gate = SimGate::new(GateKind::Records, false);
    gate.push(read(Some(1)));
    gate.push(GateRead::sighting(vec![0xAA]));
    let got = station(gate, store).tick(Utc::now()).unwrap();
    assert_eq!(
        got[0].local,
        LocalGuess::Known {
            name: "Aina".into(),
            ticket_type: "VIP".into()
        }
    );
    assert_eq!(got[1].local, LocalGuess::Unknown);
}

#[test]
fn release_failure_does_not_lose_later_reads() {
    let store = common::store();
    let mut gate = SimGate::new(GateKind::Records, true);
    gate.fail_next_releases = 1;
    for seq in 1..=3 {
        gate.push(read(Some(seq)));
    }
    let mut g = station(gate, store.clone());
    assert!(g.tick(Utc::now()).is_err(), "release failure is reported");
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        3,
        "every read is saved even though one release failed"
    );
    assert_eq!(
        g.gate.released.len(),
        2,
        "the other reads are still released"
    );
}

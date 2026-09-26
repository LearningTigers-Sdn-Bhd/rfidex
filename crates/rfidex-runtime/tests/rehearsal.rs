//! P3 rehearsal: the real runtime against the real mock over loopback HTTP.
//!
//! Two isolated event runs exercise one Bind desk and one Write desk, each with
//! its own entry and exit gate. Bind is event 101 with desk 10001, Write is
//! event 102 with desk 10002; the gates are 20001 and 20002 in both, which is
//! safe because the two events never share a server or a data root. Tickets are
//! numbered 1..=total, first half Bind, second half Write, and the UID for
//! ticket `n` is `format!("{n:016X}")`, so every sticker has its own eight bytes.
//!
//! Every run: register all but the last ticket online, burst the morning entry,
//! go down for a logical lunch, register the last ticket from cache and capture
//! every remaining passage offline, restart on the same databases, bring the
//! offline desk back **before** the gate queues, then drain and compare against
//! the mock's own counters and ID sets — never against what the harness thought
//! it sent.
//!
//! `rehearsal_smoke` and `rehearsal_500` are `#[ignore]`d: they wait on real
//! retry backoff, so they run locally and in the windows-package workflow only.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use common::{gate_station, Harness};
use rfidex_core::codec;
use rfidex_core::contract::{EventSettings, ObservationItem, Outcome, RfidMode, Role, StationKind};
use rfidex_core::store::{OutboxKind, OutboxRow, OutboxState};
use rfidex_core::tag::parse_hex;
use rfidex_mock::state::SeedTicket;
use rfidex_runtime::config::{AppConfig, DeviceChoice, StationConfig};
use rfidex_runtime::desk::DeskStep;
use rfidex_runtime::{GateStatus, Runtime, RuntimeOptions};
use uuid::Uuid;

pub const BIND_EVENT: i64 = 101;
pub const WRITE_EVENT: i64 = 102;
pub const BIND_DESK: u128 = 10001;
pub const WRITE_DESK: u128 = 10002;
pub const ENTRY_GATE: u128 = 20001;
pub const EXIT_GATE: u128 = 20002;

/// The whole event, every stage included, must finish inside this. Treated as a
/// failure, not a benchmark.
const EVENT_BOUND: Duration = Duration::from_secs(180);
/// A single ordinary state predicate, e.g. one gate's rows being saved.
const STAGE_BOUND: Duration = Duration::from_secs(10);
/// Bulk persistence after a whole afternoon of offline captures. Still bounded,
/// still a failure if it is hit; the extra room is for a slow Windows runner
/// committing every row durably, not for a retry loop.
const PERSIST_BOUND: Duration = Duration::from_secs(30);
/// Draining the queues after the outage: the outbox backs off up to a minute, so
/// recovery is allowed a longer bound than an ordinary state change.
const RECOVERY_BOUND: Duration = Duration::from_secs(90);

const ALL_STATES: [OutboxState; 5] = [
    OutboxState::Pending,
    OutboxState::Sent,
    OutboxState::Conflict,
    OutboxState::Parked,
    OutboxState::Dismissed,
];

/// The ticket `n` of the rehearsal, in the one numbering every stage uses.
fn ticket(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A sticker UID that is distinct from every other ticket's, and wide enough
/// that the raw bytes are not all zero.
fn uid(n: u128) -> String {
    format!("{n:016X}")
}

/// The ticket a sticker belongs to, read back from the UID alone.
fn ticket_of_uid(uid_hex: &str) -> Uuid {
    ticket(u128::from_str_radix(uid_hex, 16).expect("a rehearsal UID is hex"))
}

fn desk_for(event_id: i64) -> Uuid {
    match event_id {
        BIND_EVENT => Uuid::from_u128(BIND_DESK),
        WRITE_EVENT => Uuid::from_u128(WRITE_DESK),
        other => panic!("no desk is defined for event {other}"),
    }
}

fn rehearsal_event(event_id: i64, mode: RfidMode) -> EventSettings {
    EventSettings {
        event_id,
        name: format!("Rehearsal {mode:?}"),
        rfid_mode: mode,
        require_check_in: false,
    }
}

/// `count` paid, non-cancelled fictional tickets, numbered from `first`.
fn rehearsal_tickets(first: u128, count: usize) -> Vec<SeedTicket> {
    (first..first + count as u128)
        .map(|n| SeedTicket {
            public_id: ticket(n),
            name: format!("Rehearsal {n:04}"),
            ticket_type: "General".into(),
            paid: true,
            cancelled: false,
        })
        .collect()
}

fn rehearsal_stations(desk: Uuid) -> Vec<StationConfig> {
    vec![
        StationConfig {
            id: desk,
            name: "Rehearsal desk".into(),
            kind: StationKind::Desk,
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
        heartbeat: Duration::from_millis(100),
        gate_poll: Duration::from_millis(10),
        client_timeout: Duration::from_secs(2),
    }
}

/// Wait for a state with a named bound. Panics with the label when it never
/// arrives, so a failure says which stage stalled rather than only timing out.
async fn wait_for<F, Fut>(label: &str, limit: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(limit, async {
        loop {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {limit:?} waiting for {label}"));
}

/// Everything the mock will be asked about, read under one short lock so no
/// guard is ever held across an await.
#[derive(Debug, Clone, PartialEq)]
struct MockSnapshot {
    scan_logs: usize,
    scan_operations: usize,
    bindings: usize,
    binding_operations: usize,
    observations: usize,
    received: Vec<Uuid>,
    results: Vec<(String, rfidex_core::contract::ObservationResult)>,
    active: Vec<rfidex_core::contract::BindingInfo>,
}

fn mock_snapshot(h: &Harness) -> MockSnapshot {
    let mock = h.server.mock.lock().unwrap();
    MockSnapshot {
        scan_logs: mock.scan_log_count,
        scan_operations: mock.scan_operation_count(),
        bindings: mock.binding_count(),
        binding_operations: mock.binding_operation_count(),
        observations: mock.observation_count(),
        received: mock.received_order.clone(),
        results: mock.observation_results(),
        active: mock.active_bindings(),
    }
}

/// Every durable row of a station, whatever its state.
fn rows_of(h: &Harness, station: Uuid) -> Vec<OutboxRow> {
    h.store_of(station)
        .rows(&ALL_STATES, None, usize::MAX)
        .expect("read the station's rows")
}

fn observation_rows(h: &Harness, station: Uuid) -> Vec<OutboxRow> {
    h.store_of(station)
        .rows(&ALL_STATES, Some(OutboxKind::Observation), usize::MAX)
        .expect("read the station's observations")
}

fn observations_in_state(h: &Harness, station: Uuid, state: OutboxState) -> usize {
    h.store_of(station)
        .rows(&[state], Some(OutboxKind::Observation), usize::MAX)
        .expect("read the station's observations")
        .len()
}

/// `(kind, id, idem_key)` for every row: the identity of the work, which a
/// restart and a recovery must not change.
fn row_identity(h: &Harness, station: Uuid) -> BTreeSet<(String, i64, String)> {
    rows_of(h, station)
        .into_iter()
        .map(|r| (r.item.kind.as_str().to_string(), r.item.id, r.item.idem_key))
        .collect()
}

/// One saved passage, read from the durable row rather than from the scenario.
#[derive(Debug, Clone, PartialEq)]
struct SavedPassage {
    station: Uuid,
    delivery_id: Uuid,
    role: Role,
    uid_raw_hex: String,
    payload_hex: Option<String>,
}

fn saved_passages(h: &Harness, stations: &[Uuid]) -> Vec<SavedPassage> {
    let mut out = Vec::new();
    for station in stations {
        for row in observation_rows(h, *station) {
            let item: ObservationItem =
                serde_json::from_value(row.item.payload.clone()).expect("a saved observation");
            out.push(SavedPassage {
                station: *station,
                delivery_id: item.delivery_id,
                role: item.role,
                uid_raw_hex: item.uid_raw_hex,
                payload_hex: item.payload_hex,
            });
        }
    }
    out
}

/// One ticket through the desk: scan it, put a sticker on the reader, link it,
/// take the sticker off. The whole rehearsal drives the desk through this, so
/// the 500-ticket run proves the same path the smoke run does.
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
        "ticket {n} should scan: {}",
        scanned.message
    );
    assert_eq!(
        scanned.message, "Place one sticker on the reader.",
        "ticket {n} must be answered by the server, not from a stale cache"
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
        "ticket {n} should link: {}",
        linked.message
    );
    h.runtime.sim_clear(desk).await.expect("remove the sticker");
}

/// Register the reserved last ticket from the cache while the server is down.
async fn register_offline(h: &Harness, desk: Uuid, n: u128) {
    let offline_note = "Offline — badge will print when connection returns.";
    h.runtime.desk_reset(desk).await.expect("reset the desk");
    let scanned = h
        .runtime
        .desk_scan(desk, &ticket(n).to_string())
        .await
        .expect("scan the reserved ticket offline");
    assert_eq!(scanned.step, DeskStep::Scanned);
    assert!(
        scanned.offline,
        "the reserved scan must be queued, not sent"
    );
    assert!(
        scanned.message.contains(offline_note),
        "the operator is told the badge prints later: {}",
        scanned.message
    );
    assert_eq!(
        scanned.ticket.as_ref().map(|t| t.public_id),
        Some(ticket(n)),
        "the reserved ticket comes from the cache"
    );

    h.runtime
        .sim_place(desk, &uid(n))
        .await
        .expect("place the reserved sticker");
    let linked = h
        .runtime
        .desk_link(desk, None)
        .await
        .expect("link the reserved sticker offline");
    assert_eq!(linked.step, DeskStep::Linked);
    assert!(linked.offline, "the reserved link must be queued, not sent");
    assert!(
        linked.message.contains(offline_note),
        "the operator is told the badge prints later: {}",
        linked.message
    );
    h.runtime.sim_clear(desk).await.expect("remove the sticker");
}

/// One isolated event, end to end. Returns what the mock committed for it.
async fn run_event(
    mode: RfidMode,
    event_id: i64,
    first_ticket: u128,
    count: usize,
) -> MockSnapshot {
    assert!(count >= 6, "an event needs at least six tickets");
    let started = Instant::now();
    let desk = desk_for(event_id);
    let entry = Uuid::from_u128(ENTRY_GATE);
    let exit = Uuid::from_u128(EXIT_GATE);
    let last = first_ticket + count as u128 - 1;
    let earlier = first_ticket..last;
    let all: Vec<u128> = (first_ticket..=last).collect();
    let expected_bind_mode = match mode {
        RfidMode::Bind => rfidex_core::contract::BindMode::Bind,
        RfidMode::Write => rfidex_core::contract::BindMode::Written,
    };

    let mut h = Harness::start_seeded(
        rehearsal_event(event_id, mode),
        rehearsal_tickets(first_ticket, count),
        rehearsal_options(),
        rehearsal_stations(desk),
    )
    .await;
    assert_eq!(h.station(desk).mode(), Some(mode));
    assert_eq!(h.saved_settings(desk).unwrap().event.event_id, event_id);

    // ---- Registration, all but the reserved ticket, online ----
    for n in earlier.clone() {
        register(&h, desk, n).await;
    }
    let after_registration = mock_snapshot(&h);
    assert_eq!(
        (
            after_registration.scan_logs,
            after_registration.scan_operations,
            after_registration.bindings,
            after_registration.binding_operations,
            after_registration.observations,
        ),
        (count - 1, count - 1, count - 1, count - 1, 0),
        "each online registration is one scan and one binding, and no passage"
    );
    assert_eq!(after_registration.active.len(), count - 1);
    for binding in &after_registration.active {
        assert_eq!(binding.mode, expected_bind_mode, "{binding:?}");
        assert_eq!(binding.uid_raw_hex, uid(binding.public_id.as_u128()));
    }
    for n in &all {
        assert!(
            h.store_of(desk)
                .ticket(ticket(*n))
                .expect("read the cache")
                .is_some(),
            "ticket {n} must be cached before the outage, reserved one included"
        );
    }

    // ---- Morning entry burst ----
    for n in earlier.clone() {
        h.runtime
            .sim_pass(entry, &uid(n))
            .await
            .expect("walk a sticker past the entry gate");
    }
    wait_for(
        "the morning entries to be committed and answered",
        STAGE_BOUND,
        || async {
            mock_snapshot(&h).observations == count - 1
                && observations_in_state(&h, entry, OutboxState::Sent) == count - 1
        },
    )
    .await;

    // ---- Logical lunch outage ----
    // Warm every station's cache while the server is still reachable, so the
    // outage is the only reason anything is queued.
    h.runtime
        .sync_now()
        .await
        .expect("a clean sync before the outage");
    h.set_down(true);
    let during_outage = mock_snapshot(&h);

    register_offline(&h, desk, last).await;
    for n in earlier.clone() {
        // Lunch exit, afternoon entry, final exit.
        h.runtime.sim_pass(exit, &uid(n)).await.unwrap();
        h.runtime.sim_pass(entry, &uid(n)).await.unwrap();
        h.runtime.sim_pass(exit, &uid(n)).await.unwrap();
    }
    // The reserved attendee's four passages are all captured during the outage.
    for _ in 0..2 {
        h.runtime.sim_pass(entry, &uid(last)).await.unwrap();
        h.runtime.sim_pass(exit, &uid(last)).await.unwrap();
    }

    wait_for(
        "every planned passage to be saved on this computer",
        PERSIST_BOUND,
        || async {
            observation_rows(&h, entry).len() == 2 * count
                && observation_rows(&h, exit).len() == 2 * count
        },
    )
    .await;

    let captured = saved_passages(&h, &[entry, exit]);
    assert_eq!(captured.len(), 4 * count);
    check_payloads(mode, &captured, "before the restart");

    // Nothing reached the server while it was down, and the offline desk work
    // is queued, not lost.
    assert_eq!(
        mock_snapshot(&h),
        during_outage,
        "no counter may move while the fault is active"
    );
    let desk_pending = rows_of(&h, desk)
        .into_iter()
        .filter(|r| r.state == OutboxState::Pending)
        .collect::<Vec<_>>();
    assert_eq!(
        desk_pending.len(),
        2,
        "the offline registration is a queued scan and a queued binding"
    );
    assert!(desk_pending
        .iter()
        .any(|r| r.item.kind == OutboxKind::DeskScan));
    assert!(desk_pending
        .iter()
        .any(|r| r.item.kind == OutboxKind::Binding));
    assert_eq!(
        observations_in_state(&h, entry, OutboxState::Pending)
            + observations_in_state(&h, exit, OutboxState::Pending),
        3 * (count - 1) + 4,
        "every outage passage is still waiting to be sent"
    );
    let recent = h.runtime.gate_recent(entry, 5).await.unwrap();
    assert!(
        recent.iter().any(|r| r.status == GateStatus::Recorded),
        "a saved, unsent passage reads as Recorded, not Accepted: {recent:?}"
    );

    let identity_before: Vec<BTreeSet<(String, i64, String)>> = [desk, entry, exit]
        .iter()
        .map(|s| row_identity(&h, *s))
        .collect();
    let delivery_before: Vec<Uuid> = captured.iter().map(|p| p.delivery_id).collect();

    // ---- Restart on the same databases, still offline ----
    h.restart_runtime_offline().await;
    let identity_after: Vec<BTreeSet<(String, i64, String)>> = [desk, entry, exit]
        .iter()
        .map(|s| row_identity(&h, *s))
        .collect();
    assert_eq!(
        identity_after, identity_before,
        "a restart keeps every row, its id and its idempotency key"
    );
    assert_eq!(
        saved_passages(&h, &[entry, exit])
            .iter()
            .map(|p| p.delivery_id)
            .collect::<Vec<_>>(),
        delivery_before
    );
    assert_eq!(
        h.saved_settings(desk).unwrap().event.rfid_mode,
        mode,
        "the station still knows the event mode with the server down"
    );
    assert_eq!(h.saved_settings(desk).unwrap().event.event_id, event_id);

    // ---- Recover the offline desk first, then the gate queues ----
    // Station workers are independent: a gate that reaches the mock before the
    // offline binding would be answered UnknownTag, and the mock never
    // re-evaluates a stored result. This is the honest ordered recovery, not a
    // scheduler the product has; the P5 late-binding requirement is the fix.
    h.runtime
        .shutdown()
        .await
        .expect("shut the offline runtime");
    let full_config = h.runtime.config().clone();
    let desk_only = AppConfig {
        server_url: full_config.server_url.clone(),
        api_key: full_config.api_key.clone(),
        stations: vec![full_config.stations[0].clone()],
    };
    h.runtime = Runtime::start(h.paths.clone(), desk_only, h.opts.clone())
        .await
        .expect("start a desk-only runtime on the same data root");
    h.clear_faults();
    wait_for(
        "the desk-only runtime to accept its event",
        STAGE_BOUND,
        || async {
            h.runtime
                .stations()
                .iter()
                .all(|s| s.online() && s.event_ok())
        },
    )
    .await;
    wait_for(
        "the offline desk's scan and binding to reach the mock",
        RECOVERY_BOUND,
        || async {
            let _ = h.runtime.sync_now().await;
            let mock = mock_snapshot(&h);
            mock.scan_logs == count && mock.bindings == count
        },
    )
    .await;
    h.runtime
        .shutdown()
        .await
        .expect("shut the desk-only runtime");

    // ---- Back to the full station set, on the same databases ----
    h.runtime = Runtime::start(h.paths.clone(), full_config, h.opts.clone())
        .await
        .expect("restart the full configuration on the same data root");
    wait_for(
        "the gate queues to drain with nothing left to fix",
        RECOVERY_BOUND,
        || async {
            let _ = h.runtime.sync_now().await;
            h.runtime
                .status()
                .await
                .is_ok_and(|s| s.pending == 0 && s.problems == 0)
        },
    )
    .await;

    // ---- The exact effects, from the mock's own state ----
    let final_state = mock_snapshot(&h);
    assert_eq!(
        (
            final_state.scan_logs,
            final_state.scan_operations,
            final_state.bindings,
            final_state.binding_operations,
            final_state.observations,
        ),
        (count, count, count, count, 4 * count),
        "one scan, one binding and four stored passages per ticket"
    );
    assert_eq!(
        final_state.active.len(),
        count,
        "one active binding per ticket"
    );
    for binding in &final_state.active {
        assert_eq!(binding.mode, expected_bind_mode, "{binding:?}");
    }
    for n in &all {
        assert_eq!(
            h.server
                .mock
                .lock()
                .unwrap()
                .ticket(ticket(*n))
                .map(|t| t.checked_in),
            Some(true),
            "ticket {n} is checked in on the server"
        );
        let bound = final_state
            .active
            .iter()
            .find(|b| b.public_id == ticket(*n))
            .unwrap_or_else(|| panic!("ticket {n} has no active binding"));
        assert_eq!(bound.uid_raw_hex, uid(*n));
    }

    // Every result is an ordinary accepted passage: no denial, no anomaly. An
    // outage passage that had raced ahead of its binding would have been
    // refused permanently, and an entry before check-in would be flagged.
    for (station, result) in &final_state.results {
        assert_eq!(
            result.outcome,
            Outcome::Accepted,
            "{station} refused {}: {:?}",
            result.delivery_id,
            result.display
        );
        assert!(
            result.anomalies.is_empty(),
            "{station} flagged {}: {:?}",
            result.delivery_id,
            result.anomalies
        );
    }

    // The saved passages and the server's stored results are the same set, and
    // the delivery order carries exactly one entry per passage.
    let saved = saved_passages(&h, &[entry, exit]);
    assert_eq!(saved.len(), 4 * count);
    let saved_ids: BTreeSet<(Uuid, Uuid)> =
        saved.iter().map(|p| (p.station, p.delivery_id)).collect();
    let server_ids: BTreeSet<(Uuid, Uuid)> = final_state
        .results
        .iter()
        .map(|(station, result)| {
            (
                Uuid::parse_str(station).expect("a station UUID"),
                result.delivery_id,
            )
        })
        .collect();
    assert_eq!(saved_ids, server_ids, "the same passages, exactly once");
    assert_eq!(server_ids.len(), 4 * count, "no duplicates either side");
    let mut received = final_state.received.clone();
    received.sort();
    let mut expected_received: Vec<Uuid> = server_ids.iter().map(|(_, id)| *id).collect();
    expected_received.sort();
    assert_eq!(received, expected_received);

    // The offline work kept its identity all the way through recovery.
    let identity_final: Vec<BTreeSet<(String, i64, String)>> = [desk, entry, exit]
        .iter()
        .map(|s| row_identity(&h, *s))
        .collect();
    assert_eq!(
        identity_final, identity_before,
        "recovery marks rows sent; it never re-creates one"
    );

    // Each station carried its own half, each sticker has two entries and two
    // exits, and nothing was invented for the sticker that was never written.
    for station in [entry, exit] {
        assert_eq!(
            saved.iter().filter(|p| p.station == station).count(),
            2 * count,
            "station {station} carries its own passages"
        );
    }
    let mut per_ticket: BTreeMap<Uuid, (usize, usize)> = BTreeMap::new();
    for passage in &saved {
        let entry = per_ticket
            .entry(ticket_of_uid(&passage.uid_raw_hex))
            .or_default();
        match passage.role {
            Role::Entry => entry.0 += 1,
            Role::Exit => entry.1 += 1,
        }
    }
    assert_eq!(per_ticket.len(), count);
    for n in &all {
        assert_eq!(
            per_ticket.get(&ticket(*n)),
            Some(&(2, 2)),
            "ticket {n} has two entries and two exits"
        );
    }
    check_payloads(mode, &saved, "after the restart");

    // A quiet station must stay quiet: two more passes change nothing.
    h.runtime.sync_now().await.expect("a quiet sync pass");
    h.runtime
        .sync_now()
        .await
        .expect("a second quiet sync pass");
    assert_eq!(
        mock_snapshot(&h),
        final_state,
        "a sync with nothing to send changes nothing"
    );

    let elapsed = started.elapsed();
    println!(
        "{mode:?} event {event_id}: {count} tickets, {} scans, {} bindings, {} passages in {:.1?}",
        final_state.scan_logs, final_state.bindings, final_state.observations, elapsed
    );
    h.stop().await;
    final_state
}

/// Every captured passage must carry the right payload for its mode: a written
/// ticket in Write mode, and nothing invented in Bind mode.
fn check_payloads(mode: RfidMode, passages: &[SavedPassage], when: &str) {
    for passage in passages {
        let ticket = ticket_of_uid(&passage.uid_raw_hex);
        match (mode, passage.payload_hex.as_deref()) {
            (RfidMode::Write, Some(hex)) => {
                let bytes = parse_hex(hex).expect("payload hex");
                assert_eq!(
                    codec::decode(&bytes),
                    Ok(ticket),
                    "a written sticker must carry its own ticket {when}"
                );
            }
            (RfidMode::Write, None) => {
                panic!("a Write-mode passage has no payload {when}")
            }
            (RfidMode::Bind, payload) => {
                if let Some(hex) = payload {
                    assert!(
                        codec::decode(&parse_hex(hex).expect("payload hex")).is_err(),
                        "Bind mode writes nothing, so no sticker may carry a ticket {when}"
                    );
                }
            }
        }
    }
}

/// Both sizes of the same routine: Bind event first, then Write, each isolated.
async fn run_rehearsal(total: usize) {
    assert!(
        total >= 12 && total.is_multiple_of(2),
        "the rehearsal runs an even total of at least 12 tickets"
    );
    let per_event = total / 2;
    let started = Instant::now();

    let bind = tokio::time::timeout(
        EVENT_BOUND,
        run_event(RfidMode::Bind, BIND_EVENT, 1, per_event),
    )
    .await
    .unwrap_or_else(|_| panic!("the Bind event must finish within {EVENT_BOUND:?}"));
    let write = tokio::time::timeout(
        EVENT_BOUND,
        run_event(
            RfidMode::Write,
            WRITE_EVENT,
            per_event as u128 + 1,
            per_event,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("the Write event must finish within {EVENT_BOUND:?}"));

    let totals = (
        bind.scan_logs + write.scan_logs,
        bind.scan_operations + write.scan_operations,
        bind.bindings + write.bindings,
        bind.binding_operations + write.binding_operations,
        bind.observations + write.observations,
    );
    println!(
        "rehearsal {total}: {} scan logs, {} scan operations, {} bindings, \
         {} binding operations, {} passages in {:.1?}",
        totals.0,
        totals.1,
        totals.2,
        totals.3,
        totals.4,
        started.elapsed()
    );
    assert_eq!(
        totals,
        (total, total, total, total, 4 * total),
        "the two events together must add up to the whole rehearsal"
    );
}

#[tokio::test]
#[ignore = "waits on real retry backoff; run locally or in windows-package"]
async fn rehearsal_smoke() {
    run_rehearsal(12).await;
}

#[tokio::test]
#[ignore = "500-ticket rehearsal; run explicitly before Windows packaging"]
async fn rehearsal_500() {
    run_rehearsal(500).await;
}

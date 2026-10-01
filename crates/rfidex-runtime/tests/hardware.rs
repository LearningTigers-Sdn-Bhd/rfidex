//! Real reader stations through the runtime: saved setup, validation, and the
//! simulator's own commands being refused.
//!
//! Nothing here needs a reader. A station whose reader is absent or silent must
//! still save, start and report honestly, and the fake-only commands must fail
//! without touching a real adapter.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Harness, KEY};
use rfidex_core::contract::{RfidMode, Role, StationKind};
use rfidex_core::device::GateKind;
use rfidex_core::store::OutboxState;
use rfidex_hardware::config::{HardwareConfig, SdkConnection};
use rfidex_runtime::config::{
    default_printer_url, AppConfig, AppPaths, ConfigError, DeviceChoice, SetupInput, StationConfig,
};
use uuid::Uuid;

/// A reader that is present and silent: a fresh bound port with nothing behind
/// it, so a station that probes it waits and fails rather than inventing a
/// success. Each call gets its own endpoint, which is what lets two real
/// stations coexist.
fn idle_reader(timeout_ms: u32) -> HardwareConfig {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let SocketAddr::V4(address) = listener.local_addr().expect("a bound address") else {
        panic!("the fixture binds IPv4 loopback");
    };
    // Held open for the process lifetime on purpose: a dropped listener would
    // let the port be reused and turn "silent" into "refused".
    Box::leak(Box::new(listener));
    HardwareConfig::EcV19PlainTcp {
        address,
        bus_address: 0xFF,
        antenna_byte: false,
        timeout_ms,
    }
}

fn sdk_reader() -> HardwareConfig {
    HardwareConfig::EcrfidSdk {
        dll_path: if cfg!(windows) {
            PathBuf::from(r"C:\nowhere\ECRFID.dll")
        } else {
            PathBuf::from("/nonexistent/nowhere/ECRFID.dll")
        },
        connection: SdkConnection::Hid {
            model: "EC1101".into(),
            path: "\\\\?\\hid#vid_0483&pid_5750#1".into(),
            address_mode: 1,
            exclusive: 1,
        },
        inventory_mode: 4,
        timeout_ms: 2_000,
        write_verified: false,
    }
}

/// A helper program that does not exist: a real SDK station in these tests must
/// never be able to start one, so a test that spawns something fails loudly
/// instead of running the test binary again.
fn no_helper() -> PathBuf {
    std::env::temp_dir().join("rfidex-nonexistent-helper")
}

fn real_desk(id: u128, name: &str, hardware: HardwareConfig) -> StationConfig {
    StationConfig {
        id: Uuid::from_u128(id),
        name: name.into(),
        kind: StationKind::Desk,
        role: None,
        device: DeviceChoice::EcrfidDesk { hardware },
        debounce_secs: 5,
        write_start_block: 0,
        printer_url: default_printer_url(),
        alarm_all_panels: true,
        alarm_wait_ms: 1500,
    }
}

fn real_gate(id: u128, name: &str, role: Role, hardware: HardwareConfig) -> StationConfig {
    StationConfig {
        id: Uuid::from_u128(id),
        name: name.into(),
        kind: StationKind::Gate,
        role: Some(role),
        device: DeviceChoice::EcrfidGate { hardware },
        debounce_secs: 5,
        write_start_block: 0,
        printer_url: default_printer_url(),
        alarm_all_panels: true,
        alarm_wait_ms: 1500,
    }
}

fn config(stations: Vec<StationConfig>) -> AppConfig {
    AppConfig {
        server_url: "https://example.test".into(),
        api_key: KEY.into(),
        stations,
    }
}

fn refused(config: &AppConfig) -> String {
    match config.validate() {
        Err(ConfigError::Invalid(message)) => message,
        other => panic!("expected a readable rejection, got {other:?}"),
    }
}

#[test]
fn a_saved_simulator_config_still_loads_unchanged() {
    // The exact shape plan 7 wrote, which must keep working byte for byte.
    let saved = serde_json::json!({
        "server_url": "https://example.test",
        "api_key": KEY,
        "stations": [
            {
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "Desk",
                "kind": "desk",
                "role": null,
                "device": { "type": "sim_desk" }
            },
            {
                "id": "00000000-0000-0000-0000-000000000002",
                "name": "Entry gate",
                "kind": "gate",
                "role": "entry",
                "device": { "type": "sim_gate", "gate_kind": "live_inventory",
                            "release_verified": true }
            }
        ]
    });
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(dir.path().to_path_buf());
    std::fs::write(&paths.config_file, saved.to_string()).unwrap();

    let loaded = paths.load().unwrap().unwrap();
    assert_eq!(loaded.stations.len(), 2);
    assert_eq!(loaded.stations[0].device, DeviceChoice::SimDesk);
    assert_eq!(
        loaded.stations[1].device,
        DeviceChoice::SimGate {
            gate_kind: GateKind::LiveInventory,
            release_verified: true,
        }
    );
    let written = serde_json::to_string(&loaded.setup_view()).unwrap();
    assert!(written.contains(r#""type":"sim_desk""#));
    assert!(
        written.contains(r#""release_verified":true"#),
        "the simulator's own settings survive a round trip"
    );
}

#[test]
fn a_real_reader_config_round_trips_through_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(dir.path().to_path_buf());
    let original = config(vec![
        real_desk(1, "Desk", idle_reader(500)),
        real_gate(2, "Entry gate", Role::Entry, sdk_reader()),
    ]);
    paths.save(&original).unwrap();
    let loaded = paths.load().unwrap().unwrap();
    assert_eq!(loaded.stations[0].device, original.stations[0].device);
    assert_eq!(loaded.stations[1].device, original.stations[1].device);

    let json = serde_json::to_string(&loaded.setup_view()).unwrap();
    assert!(json.contains(r#""type":"ecrfid_desk""#));
    assert!(json.contains(r#""type":"ecrfid_gate""#));
    assert!(
        !json.contains(KEY),
        "a real reader in the setup never brings the key with it"
    );
    assert!(!json.contains("\"api_key\""));
}

#[test]
fn the_station_kind_and_the_reader_have_to_agree() {
    let mut desk_with_a_direction = real_desk(1, "Desk", idle_reader(500));
    desk_with_a_direction.role = Some(Role::Entry);
    assert!(refused(&config(vec![desk_with_a_direction])).contains("direction"));

    let mut gate_without_a_direction = real_gate(2, "Gate", Role::Entry, idle_reader(500));
    gate_without_a_direction.role = None;
    assert!(refused(&config(vec![gate_without_a_direction])).contains("entry or exit"));

    // A simulator gate is still not a desk, and a real gate is not a desk
    // either: the pairing is checked, not just the reader type.
    let mismatched = StationConfig {
        device: DeviceChoice::EcrfidGate {
            hardware: idle_reader(500),
        },
        ..real_desk(1, "Desk", idle_reader(500))
    };
    assert!(refused(&config(vec![mismatched])).contains("device"));
}

#[test]
fn a_real_gate_cannot_be_given_the_simulator_records_settings() {
    let json = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000002",
        "name": "Entry gate",
        "kind": "gate",
        "role": "entry",
        "device": {
            "type": "ecrfid_gate",
            "hardware": {
                "transport": "ec_v19_plain_tcp",
                "address": "127.0.0.1:6688",
                "bus_address": 255,
                "antenna_byte": false,
                "timeout_ms": 500
            },
            "gate_kind": "records",
            "release_verified": true
        }
    });
    let outcome = serde_json::from_value::<StationConfig>(json);
    assert!(
        outcome.is_err(),
        "a real gate reads live inventory; records and releases are not settings it has"
    );
}

#[test]
fn two_stations_cannot_claim_one_reader() {
    let shared = idle_reader(500);
    let duplicate = config(vec![
        real_desk(1, "Desk", shared.clone()),
        real_gate(2, "Gate", Role::Entry, shared),
    ]);
    let message = refused(&duplicate);
    assert!(message.contains("same reader"), "{message}");

    // A plain TCP profile and an SDK network profile that name the same address
    // are the same physical device, so they cannot both be configured either.
    let address: SocketAddrV4 = "192.168.1.20:6688".parse().unwrap();
    let sdk_net = HardwareConfig::EcrfidSdk {
        dll_path: if cfg!(windows) {
            PathBuf::from(r"C:\rfidex\ECRFID.dll")
        } else {
            PathBuf::from("/opt/rfidex/ECRFID.dll")
        },
        connection: SdkConnection::Net {
            model: "EC1101".into(),
            interface: "192.168.1.10".into(),
            address,
        },
        inventory_mode: 4,
        timeout_ms: 2_000,
        write_verified: false,
    };
    let same_endpoint = config(vec![
        real_desk(1, "Desk", idle_reader(500)),
        real_gate(
            2,
            "Gate",
            Role::Entry,
            HardwareConfig::EcV19PlainTcp {
                address,
                bus_address: 0xFF,
                antenna_byte: false,
                timeout_ms: 500,
            },
        ),
    ]);
    assert!(
        same_endpoint.validate().is_ok(),
        "different endpoints are fine"
    );
    let cross_transport = config(vec![
        real_gate(2, "Gate", Role::Entry, sdk_net.clone()),
        real_desk(
            1,
            "Desk",
            HardwareConfig::EcV19PlainTcp {
                address,
                bus_address: 0xFF,
                antenna_byte: false,
                timeout_ms: 500,
            },
        ),
    ]);
    assert!(
        refused(&cross_transport).contains("same reader"),
        "one address is one device whatever transport reaches it"
    );
}

#[test]
fn a_missing_reader_does_not_block_saving_the_setup() {
    // The DLL is not installed and the address has nothing behind it, and the
    // save is still accepted: a disconnected reader is a status, not a refusal.
    let saved = AppConfig::from_input(
        None,
        SetupInput {
            server_url: "https://example.test".into(),
            api_key: KEY.into(),
            stations: vec![
                real_desk(1, "Desk", sdk_reader()),
                real_gate(2, "Entry gate", Role::Entry, idle_reader(500)),
            ],
        },
    )
    .unwrap();
    let json = serde_json::to_string(&saved.setup_view()).unwrap();
    assert!(!json.contains(KEY));
    assert!(json.contains("/nonexistent/nowhere/ECRFID.dll") || json.contains("nowhere"));
}

#[tokio::test]
async fn a_real_station_refuses_every_simulator_command() {
    let desk = Uuid::from_u128(1);
    let gate = Uuid::from_u128(2);
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![
            real_desk(1, "Desk", idle_reader(300)),
            real_gate(2, "Entry gate", Role::Entry, idle_reader(300)),
        ],
        no_helper(),
    )
    .await;

    let before = harness.store_of(gate).count(OutboxState::Pending).unwrap();
    let observations = harness.observations();

    for (label, outcome) in [
        (
            "sim_place",
            harness.runtime.sim_place(desk, "3412CDAB500104E0").await,
        ),
        ("sim_clear", harness.runtime.sim_clear(desk).await),
        (
            "sim_pass",
            harness.runtime.sim_pass(gate, "3412CDAB500104E0").await,
        ),
        (
            "sim_set_connected",
            harness.runtime.sim_set_connected(desk, true).await,
        ),
        (
            "sim_set_connected (gate)",
            harness.runtime.sim_set_connected(gate, false).await,
        ),
    ] {
        let error = outcome.expect_err(label);
        assert_eq!(error.code, "not_simulated", "{label}: {error:?}");
        assert_eq!(error.message, "This station uses a real reader.");
    }

    assert_eq!(
        harness.store_of(gate).count(OutboxState::Pending).unwrap(),
        before,
        "a refused simulator command writes nothing"
    );
    assert_eq!(
        harness.observations(),
        observations,
        "and it sends nothing to the server"
    );
    assert!(
        !harness.station(desk).connected(),
        "a real reader is not declared connected by a simulator command"
    );
    harness.stop().await;
}

#[tokio::test]
async fn a_real_reader_station_reports_disconnected_and_keeps_working() {
    let desk = Uuid::from_u128(11);
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![real_desk(11, "Desk", sdk_reader())],
        no_helper(),
    )
    .await;

    let status = harness.runtime.status().await.unwrap();
    let station = status
        .stations
        .iter()
        .find(|s| s.id == desk)
        .expect("the station is reported");
    assert!(!station.simulated, "a real station is not a simulator");
    assert!(
        station.online,
        "the station still talks to the event server"
    );
    assert!(
        !station.connected,
        "and it says its reader is not connected"
    );

    // The ticket workflow is untouched by a missing reader. The cache arrives
    // with the first sync pass, so the test waits for it rather than assuming
    // it landed before the heartbeat that made the station online.
    harness.await_cached(desk, common::ticket(1)).await;
    assert!(harness
        .store_of(desk)
        .ticket(common::ticket(1))
        .unwrap()
        .is_some());
    harness.stop().await;
}

/// A reader that answers inventory the way the candidate protocol describes, so
/// a real gate produces real passages without any hardware. The bytes are the
/// pinned synthetic candidates, not a vendor capture.
struct AnsweringReader {
    address: SocketAddrV4,
    enabled: Arc<AtomicBool>,
    /// Whether the runtime asked with the candidate request this profile
    /// defines. Read back by the test: the fixture cannot panic usefully.
    saw_request: Arc<AtomicBool>,
    _listener: Arc<TcpListener>,
}

const REQUEST: [u8; 8] = [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x00, 0x38, 0x8B];
const TAG_A: [u8; 17] = [
    0xEC, 0x10, 0x00, 0xFE, 0x01, 0x00, 0x00, 0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34, 0x0F,
    0x02,
];
const TERMINAL: [u8; 8] = [0xEC, 0x07, 0x00, 0xFE, 0x01, 0x00, 0x08, 0x9F];

impl AnsweringReader {
    /// Bind, and start answering only when the test says so. A reader that
    /// answers from the first poll would let a passage be sent before the test
    /// has taken the server away.
    fn start() -> AnsweringReader {
        let listener = Arc::new(TcpListener::bind("127.0.0.1:0").expect("a loopback port"));
        let SocketAddr::V4(address) = listener.local_addr().expect("a bound address") else {
            panic!("the fixture binds IPv4 loopback");
        };
        let enabled = Arc::new(AtomicBool::new(false));
        let saw_request = Arc::new(AtomicBool::new(false));
        let answer = {
            let enabled = enabled.clone();
            let saw_request = saw_request.clone();
            let accepting = listener.clone();
            move || {
                for incoming in accepting.incoming() {
                    let Ok(mut stream) = incoming else { break };
                    let enabled = enabled.clone();
                    let saw_request = saw_request.clone();
                    std::thread::spawn(move || {
                        let mut request = [0u8; 8];
                        loop {
                            if read_full(&mut stream, &mut request).is_err() {
                                return;
                            }
                            saw_request.store(request == REQUEST, Ordering::SeqCst);
                            if !enabled.load(Ordering::SeqCst) {
                                // Present but silent, exactly like a reader that
                                // has been switched off.
                                continue;
                            }
                            if stream.write_all(&TAG_A).is_err() {
                                return;
                            }
                            if stream.write_all(&TERMINAL).is_err() {
                                return;
                            }
                            let _ = stream.flush();
                        }
                    });
                }
            }
        };
        std::thread::spawn(answer);
        AnsweringReader {
            address,
            enabled,
            saw_request,
            _listener: listener,
        }
    }

    fn config(&self, timeout_ms: u32) -> HardwareConfig {
        HardwareConfig::EcV19PlainTcp {
            address: self.address,
            bus_address: 0xFF,
            antenna_byte: false,
            timeout_ms,
        }
    }

    fn answer_now(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }
}

fn read_full(stream: &mut std::net::TcpStream, buffer: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..])? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => filled += n,
        }
    }
    Ok(())
}

#[tokio::test]
async fn real_gate_never_claims_connected_before_a_reader_answers() {
    let gate = Uuid::from_u128(52);
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![real_gate(52, "Entry gate", Role::Entry, idle_reader(4_000))],
        no_helper(),
    )
    .await;
    let status = harness.runtime.status().await.unwrap();
    assert!(
        !status
            .stations
            .iter()
            .find(|s| s.id == gate)
            .unwrap()
            .connected
    );
    harness.stop().await;
}

#[tokio::test]
async fn hardware_stall_does_not_block_status_or_other_station() {
    let desk = common::desk_id();
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![
            common::desk_station(),
            real_gate(42, "Entry gate", Role::Entry, idle_reader(4_000)),
        ],
        no_helper(),
    )
    .await;

    // The gate is stuck inside a poll for four seconds. The status bar and the
    // other station must not wait for it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let started = Instant::now();
    let status = harness.runtime.status().await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a stalled reader must not hold the status bar"
    );
    assert_eq!(status.stations.len(), 2);

    harness
        .runtime
        .desk_scan(desk, &common::ticket(1).to_string())
        .await
        .unwrap();
    harness
        .runtime
        .sim_place(desk, "3412CDAB500104E0")
        .await
        .unwrap();
    let view = harness
        .runtime
        .desk_link(desk, None)
        .await
        .expect("the desk links while the gate is stuck");
    assert_eq!(
        view.step,
        rfidex_runtime::DeskStep::Linked,
        "the desk flow runs on a blocking worker and still completes"
    );
    harness.stop().await;
}

#[tokio::test]
async fn shutdown_cancels_hardware_before_join() {
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![real_gate(51, "Entry gate", Role::Entry, idle_reader(9_000))],
        no_helper(),
    )
    .await;
    // Let the gate get stuck inside a poll, so shutdown has something to cancel.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    harness.runtime.shutdown().await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown cancels the stuck reader instead of waiting out its deadline"
    );
    harness.stop().await;
}

#[tokio::test]
async fn reconfigure_discards_old_hardware_results() {
    let gate = Uuid::from_u128(61);
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![real_gate(61, "Entry gate", Role::Entry, idle_reader(500))],
        no_helper(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let before = harness.store_of(gate).count(OutboxState::Pending).unwrap();

    // A reconfigure stops the old runtime. The poll it started must come back
    // and must publish nothing to the station that replaces it.
    let started = Instant::now();
    harness.runtime.shutdown().await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the stalled poll is cancelled, not waited out"
    );
    assert_eq!(
        harness.store_of(gate).count(OutboxState::Pending).unwrap(),
        before,
        "a cancelled poll writes nothing"
    );

    harness.restart_runtime_with_launcher(no_helper()).await;
    let status = harness.runtime.status().await.unwrap();
    let station = status
        .stations
        .iter()
        .find(|s| s.id == gate)
        .expect("the replacement station is reported");
    assert_eq!(
        station.pending, before,
        "the replacement starts from the same durable state and nothing more"
    );
    // And it reports its own reader honestly instead of inheriting a result.
    common::eventually("the replacement to report its own reader", || async {
        harness
            .runtime
            .status()
            .await
            .ok()
            .and_then(|status| {
                status
                    .stations
                    .iter()
                    .find(|s| s.id == gate)
                    .and_then(|s| s.last_error.clone())
            })
            .is_some()
    })
    .await;
    harness.stop().await;
}

#[tokio::test]
async fn offline_real_gate_uses_existing_outbox() {
    let gate = Uuid::from_u128(71);
    let reader = AnsweringReader::start();
    let mut harness = Harness::start_hardware(
        RfidMode::Bind,
        common::fast_options(),
        vec![real_gate(71, "Entry gate", Role::Entry, reader.config(500))],
        no_helper(),
    )
    .await;

    // The event server goes away, then the sticker arrives in the field.
    harness.set_down(true);
    reader.answer_now();
    common::eventually("the passage to be saved on this computer", || async {
        harness.store_of(gate).count(OutboxState::Pending).unwrap() == 1
    })
    .await;

    // The sticker is still in the field and the gate keeps polling; the
    // existing debounce means one passage, not one per poll.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        harness.store_of(gate).count(OutboxState::Pending).unwrap(),
        1,
        "one sticker in the field is one passage"
    );

    harness.set_down(false);
    harness.runtime.sync_now().await.unwrap();
    common::eventually("the passage to reach the server", || async {
        harness.store_of(gate).count(OutboxState::Pending).unwrap() == 0
    })
    .await;
    assert_eq!(
        harness.observations(),
        1,
        "the server saw the passage exactly once"
    );
    assert!(
        reader.saw_request.load(Ordering::SeqCst),
        "the runtime asked with the pinned candidate request"
    );
    harness.stop().await;
}

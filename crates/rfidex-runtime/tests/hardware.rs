//! Real reader stations through the runtime: saved setup, validation, and the
//! simulator's own commands being refused.
//!
//! Nothing here needs a reader. A station whose reader is absent or silent must
//! still save, start and report honestly, and the fake-only commands must fail
//! without touching a real adapter.

mod common;

use std::net::{SocketAddr, SocketAddrV4, TcpListener};
use std::path::PathBuf;

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

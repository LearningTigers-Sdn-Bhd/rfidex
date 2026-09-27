//! The desk and gate adapters, over the real TCP transport and — with the
//! `test-host` feature — over a real helper process.
//!
//! The parts that need no helper run in the plan's plain command. The parts
//! that drive a real child are behind the feature so this file never reports
//! zero tests as coverage: run them with
//! `cargo test -p rfidex-hardware --features test-host --test adapters`.

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rfidex_core::device::{DeviceError, GateKind, GateSource, TagReaderWriter};
use rfidex_core::store::{OutboxKind, OutboxState, Store};
use rfidex_core::tag::UidRule;
use rfidex_hardware::adapters::{EcrfidDesk, EcrfidGate};
use rfidex_hardware::config::{HardwareConfig, SdkConnection};
use rfidex_hardware::process::HostLauncher;
use support::frames::{tag_frame, terminal_frame};
use support::{FakeReader, Script, UID_A, UID_B};

/// A launcher that never starts anything. Any test that expects a refusal
/// before the transport is touched uses it: if the adapter tried to open a
/// reader, it would fail with `Disconnected` instead of `WriteUnsupported`.
fn no_launcher() -> HostLauncher {
    HostLauncher::new(std::env::temp_dir().join("rfidex-nonexistent-helper"))
}

fn sdk_config(write_verified: bool) -> HardwareConfig {
    HardwareConfig::EcrfidSdk {
        dll_path: if cfg!(windows) {
            PathBuf::from(r"C:\rfidex\ECRFID.dll")
        } else {
            PathBuf::from("/opt/rfidex/ECRFID.dll")
        },
        connection: SdkConnection::Com {
            model: "EC1101".into(),
            port: "COM3".into(),
            baud: 38_400,
            frame: "8E1".into(),
            bus_address: 255,
        },
        inventory_mode: 4,
        timeout_ms: 2_000,
        write_verified,
    }
}

fn response(uid: &[u8; 8]) -> Vec<u8> {
    let mut out = tag_frame(0x00, 0x00, uid, None);
    out.extend_from_slice(&terminal_frame(0x00, 0x00));
    out
}

const REQUEST: [u8; 8] = [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x00, 0x38, 0x8B];

#[test]
fn a_desk_keeps_the_tag_bytes_exactly_as_the_reader_reported_them() {
    let fake = FakeReader::bind();
    let handler = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), response(&UID_A))]);
    let mut desk = EcrfidDesk::new(fake.config(500, false), no_launcher()).unwrap();
    let tags = desk.inventory().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].uid_raw, UID_A.to_vec());
    assert_eq!(tags[0].dsfid, Some(0x00));
    assert_eq!(tags[0].protocol, rfidex_core::tag::Protocol::Iso15693);
    assert!(desk.connected());
    handler.join().unwrap();
}

#[test]
fn a_desk_reports_two_stickers_as_two() {
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, None);
    body.extend_from_slice(&tag_frame(0x00, 0x00, &UID_B, None));
    body.extend_from_slice(&terminal_frame(0x00, 0x00));
    let handler = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut desk = EcrfidDesk::new(fake.config(500, false), no_launcher()).unwrap();
    let tags = desk.inventory().unwrap();
    assert_eq!(tags.len(), 2, "two stickers are not collapsed into one");
    assert_eq!(tags[0].uid_raw, UID_A.to_vec());
    assert_eq!(tags[1].uid_raw, UID_B.to_vec());
    handler.join().unwrap();
}

#[test]
fn a_timed_out_exchange_is_a_failure_and_never_an_empty_field() {
    let fake = FakeReader::bind();
    let handler = fake.expect_connections(vec![Script::scripted(
        REQUEST.to_vec(),
        vec![support::Chunk::now(tag_frame(0x00, 0x00, &UID_A, None))],
        true,
    )]);
    let mut desk = EcrfidDesk::new(fake.config(300, false), no_launcher()).unwrap();
    let outcome = desk.inventory();
    assert!(
        outcome.is_err(),
        "a reader that never finished is not an empty reader"
    );
    assert!(
        !desk.connected(),
        "a failed exchange clears the cached state"
    );
    handler.join().unwrap();
}

#[test]
fn the_tcp_profile_reads_and_never_writes() {
    let fake = FakeReader::bind();
    let mut desk = EcrfidDesk::new(fake.config(500, false), no_launcher()).unwrap();
    assert!(
        !desk.capabilities().write_supported,
        "the candidate TCP profile is not a writer"
    );
    assert_eq!(
        desk.write_blocks(&UID_A, 0, &[0; 4]),
        Err(DeviceError::WriteUnsupported)
    );
    assert_eq!(desk.tag_memory(&UID_A), Err(DeviceError::WriteUnsupported));
    assert_eq!(
        desk.read_blocks(&UID_A, 0, 1),
        Err(DeviceError::WriteUnsupported)
    );
}

#[test]
fn a_long_uid_is_refused_before_the_transport_is_touched() {
    let fake = FakeReader::bind();
    let mut desk = EcrfidDesk::new(fake.config(500, false), no_launcher()).unwrap();
    assert_eq!(
        desk.read_blocks(&[1, 2, 3], 0, 1),
        Err(DeviceError::WriteUnsupported)
    );
    // The SDK profile is the one that reaches the transport, and it refuses the
    // uid before opening anything.
    let mut sdk = EcrfidDesk::new(sdk_config(true), no_launcher()).unwrap();
    assert_eq!(
        sdk.write_blocks(&[1, 2, 3], 0, &[0; 4]),
        Err(DeviceError::OutOfRange),
        "a UID that is not eight bytes never becomes a vendor call"
    );
}

#[test]
fn an_unverified_sdk_profile_refuses_to_write_before_any_transport() {
    let mut desk = EcrfidDesk::new(sdk_config(false), no_launcher()).unwrap();
    assert!(!desk.capabilities().write_supported);
    assert_eq!(
        desk.write_blocks(&UID_A, 0, &[1, 2, 3, 4]),
        Err(DeviceError::WriteUnsupported),
        "an unverified profile is refused before a helper is started"
    );
    assert_eq!(
        desk.inventory(),
        Err(DeviceError::Disconnected),
        "and the reader was never started at all"
    );
}

#[test]
fn the_cached_info_never_opens_the_reader() {
    let fake = FakeReader::bind();
    let mut desk = EcrfidDesk::new(fake.config(500, false), no_launcher()).unwrap();
    assert_eq!(
        desk.info(),
        Err(DeviceError::Disconnected),
        "nothing has been read yet, and the heartbeat must not start a reader"
    );
    assert!(!desk.connected());

    let handler = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), response(&UID_A))]);
    let info = desk.probe().unwrap();
    assert_eq!(info.adapter, "ec-v19-plain-tcp");
    assert!(info.model.is_none(), "the profile does not invent a model");
    assert!(info.firmware.is_none());
    assert_eq!(desk.info().unwrap(), info, "the cache is now populated");
    handler.join().unwrap();
}

#[test]
fn a_gate_reports_a_sighting_without_inventing_anything() {
    let fake = FakeReader::bind();
    let handler = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), response(&UID_A))]);
    let mut gate = EcrfidGate::new(fake.config(500, false), no_launcher()).unwrap();
    let caps = gate.capabilities();
    assert_eq!(caps.kind, GateKind::LiveInventory);
    assert!(!caps.release_verified);

    let polled = gate.poll().unwrap();
    assert_eq!(polled.len(), 1);
    let (read, handle) = &polled[0];
    assert_eq!(read.tag.uid_raw, UID_A.to_vec());
    assert_eq!(read.payload, None, "live inventory reads no payload");
    assert_eq!(read.device_direction_raw, None);
    assert_eq!(read.device_time_raw, None);
    assert_eq!(
        read.device_record_seq, None,
        "no sequence was allocated, so a restart cannot duplicate one"
    );
    assert_eq!(
        read.flags_raw["source"], "ecrfid_live_inventory",
        "the raw flags name the profile"
    );
    assert_eq!(gate.release(*handle), Ok(()), "a no-op, and it says so");
    handler.join().unwrap();
}

#[test]
fn a_gate_sighting_becomes_one_durable_row_and_repeats_are_debounced() {
    let fake = FakeReader::bind();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(&dir.path().join("gate.db")).unwrap(),
    ));
    let handler = fake.expect_connections(vec![Script::answering(
        REQUEST.to_vec(),
        response(&UID_A),
        2,
    )]);
    let gate = EcrfidGate::new(fake.config(500, false), no_launcher()).unwrap();
    let mut station = rfidex_core::station::gate::GateStation::new(
        gate,
        store.clone(),
        "station-1",
        rfidex_core::contract::Role::Exit,
        UidRule::AsIs,
        Duration::from_secs(5),
    );

    let now = chrono::Utc::now();
    let captured = station.tick(now).unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        1,
        "one sighting is one durable row"
    );

    // The same sticker again inside the debounce window is the same passage.
    let again = station
        .tick(now + chrono::Duration::milliseconds(500))
        .unwrap();
    assert!(
        again.is_empty(),
        "a repeat inside the window is not a passage"
    );
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        1,
        "and it added no row"
    );

    let row = store
        .lock()
        .unwrap()
        .rows(&[OutboxState::Pending], Some(OutboxKind::Observation), 1)
        .unwrap();
    let item: rfidex_core::contract::ObservationItem =
        serde_json::from_value(row[0].item.payload.clone()).unwrap();
    assert_eq!(
        item.role,
        rfidex_core::contract::Role::Exit,
        "the configured station role is authoritative, not the reader"
    );
    handler.join().unwrap();
}

#[test]
fn a_failed_gate_poll_saves_nothing() {
    let fake = FakeReader::bind();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(&dir.path().join("gate.db")).unwrap(),
    ));
    // Damaged checksum: the reader answered, and the answer cannot be trusted.
    let mut damaged = response(&UID_A);
    damaged[15] ^= 0xFF;
    let handler = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), damaged)]);
    let gate = EcrfidGate::new(fake.config(500, false), no_launcher()).unwrap();
    let mut station = rfidex_core::station::gate::GateStation::new(
        gate,
        store.clone(),
        "station-1",
        rfidex_core::contract::Role::Entry,
        UidRule::AsIs,
        Duration::from_secs(5),
    );
    assert!(station.tick(chrono::Utc::now()).is_err());
    assert_eq!(
        store.lock().unwrap().count(OutboxState::Pending).unwrap(),
        0,
        "an unreadable answer becomes no row at all"
    );
    handler.join().unwrap();
}

#[test]
fn stopping_a_gate_ends_a_reader_that_is_waiting() {
    let fake = FakeReader::bind();
    let handler = fake.expect_connections(vec![Script::scripted(
        REQUEST.to_vec(),
        vec![support::Chunk::now(vec![0xEC])],
        true,
    )]);
    let gate = EcrfidGate::new(fake.config(9_000, false), no_launcher()).unwrap();
    let control = gate.stop_control();
    let started = std::time::Instant::now();
    let runner = std::thread::spawn(move || {
        let mut gate = gate;
        gate.poll()
    });
    std::thread::sleep(Duration::from_millis(150));
    control.stop();
    assert!(runner.join().unwrap().is_err());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the stop ends the wait instead of letting the deadline run out"
    );
    handler.join().unwrap();
}

#[cfg(feature = "test-host")]
mod with_helper {
    use super::*;
    use rfidex_core::device::DeviceInfo;

    /// The fake helper is chosen by environment variables, so the tests that
    /// use it must not overlap.
    static HELPER: Mutex<()> = Mutex::new(());

    struct Helper {
        _exclusive: std::sync::MutexGuard<'static, ()>,
        log: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Helper {
        fn start(scenario: &str) -> Helper {
            let exclusive = HELPER.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("a temporary folder");
            let log = dir.path().join("calls.log");
            std::env::set_var("RFIDEX_FAKE_HOST", scenario);
            std::env::set_var("RFIDEX_FAKE_HOST_LOG", &log);
            Helper {
                _exclusive: exclusive,
                log,
                _dir: dir,
            }
        }

        fn count_of(&self, operation: &str) -> usize {
            let wanted = format!("op={operation}");
            std::fs::read_to_string(&self.log)
                .map(|text| text.lines().filter(|line| *line == wanted).count())
                .unwrap_or_default()
        }
    }

    fn launcher() -> HostLauncher {
        HostLauncher::new(PathBuf::from(env!("CARGO_BIN_EXE_rfidex-fake-host")))
    }

    #[test]
    fn a_verified_sdk_profile_writes_and_reads_back_the_exact_blocks() {
        let helper = Helper::start("echo");
        let mut desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        assert!(desk.capabilities().write_supported);

        let info: DeviceInfo = desk.probe().unwrap();
        assert_eq!(info.adapter, "ecrfid-sdk");
        assert_eq!(info.model.as_deref(), Some("EC1101"));
        assert!(
            info.firmware.is_none(),
            "no version is invented from raw bytes"
        );

        let memory = desk.tag_memory(&UID_A).unwrap();
        assert_eq!((memory.block_size, memory.block_count), (4, 28));

        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        desk.write_blocks(&UID_A, 2, &data).unwrap();
        assert_eq!(
            desk.read_blocks(&UID_A, 2, 2).unwrap(),
            data.to_vec(),
            "the bytes that were written are the bytes that come back"
        );
        assert_eq!(helper.count_of("write"), 1, "one write, not a write loop");
        drop(desk);
    }

    #[test]
    fn a_corrupted_readback_is_not_a_successful_write() {
        let _helper = Helper::start("read_corrupt");
        let mut desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        let data = [9u8, 9, 9, 9];
        desk.write_blocks(&UID_A, 2, &data).unwrap();
        let readback = desk.read_blocks(&UID_A, 2, 1).unwrap();
        assert_ne!(
            readback,
            data.to_vec(),
            "the desk's own readback check is what catches this, and it must"
        );
        drop(desk);
    }

    #[test]
    fn a_torn_write_is_visible_in_the_readback() {
        let _helper = Helper::start("write_tear");
        let mut desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        desk.write_blocks(&UID_A, 2, &data).unwrap();
        assert_ne!(
            desk.read_blocks(&UID_A, 2, 2).unwrap(),
            data.to_vec(),
            "half a write is not a write"
        );
        drop(desk);
    }

    #[test]
    fn a_write_that_fails_is_not_repeated_by_the_adapter() {
        let helper = Helper::start("write_error");
        let mut desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        assert!(desk.write_blocks(&UID_A, 2, &[1, 2, 3, 4]).is_err());
        assert_eq!(
            helper.count_of("write"),
            1,
            "the adapter sends one write and reports the failure"
        );
        drop(desk);
    }

    #[test]
    fn failed_memory_invalidates_session_before_explicit_read_reconnects() {
        let helper = Helper::start("memory_error");
        let mut desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        assert!(desk.tag_memory(&UID_A).is_err());
        assert!(!desk.connected());
        assert_eq!(helper.count_of("open"), 1);
        assert_eq!(desk.inventory().unwrap().len(), 1);
        assert_eq!(
            helper.count_of("open"),
            2,
            "old child was reaped before second handle"
        );
    }

    #[test]
    fn failed_write_never_reconnects_until_explicit_read() {
        let helper = Helper::start("write_error");
        let mut desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        assert!(desk.write_blocks(&UID_A, 2, &[1, 2, 3, 4]).is_err());
        assert!(!desk.connected());
        assert_eq!(helper.count_of("open"), 1);
        assert_eq!(helper.count_of("write"), 1);
        assert!(desk.write_blocks(&UID_A, 2, &[5, 6, 7, 8]).is_err());
        assert_eq!(
            helper.count_of("open"),
            1,
            "another write cannot open a new SDK session"
        );
        assert_eq!(helper.count_of("write"), 1);
        assert_eq!(desk.inventory().unwrap().len(), 1);
        assert_eq!(helper.count_of("open"), 2);
        assert_eq!(
            helper.count_of("write"),
            1,
            "uncertain write was never replayed"
        );
    }

    #[test]
    fn stop_while_open_is_stalled_interrupts_start_and_reaps_child() {
        let helper = Helper::start("stall_open_reply");
        let desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        let stop = desk.stop_control();
        let runner = std::thread::spawn(move || {
            let mut desk = desk;
            desk.inventory()
        });
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while helper.count_of("open") == 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(helper.count_of("open"), 1);
        let started = std::time::Instant::now();
        stop.stop();
        assert!(runner.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(helper.count_of("open"), 1);
    }

    #[test]
    fn the_write_guard_holds_even_with_a_working_helper() {
        let helper = Helper::start("echo");
        let mut desk = EcrfidDesk::new(sdk_config(false), launcher()).unwrap();
        assert_eq!(
            desk.write_blocks(&UID_A, 0, &[1, 2, 3, 4]),
            Err(DeviceError::WriteUnsupported)
        );
        assert_eq!(
            helper.count_of("write"),
            0,
            "an unverified profile never reaches the helper with a write"
        );
        // Reading still works, so the guard is about writing and not about the
        // reader being unusable.
        assert_eq!(desk.inventory().unwrap().len(), 1);
        drop(desk);
    }

    #[test]
    fn a_desk_write_reaches_the_sticker_through_core() {
        let helper = Helper::start("echo");
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            Store::open(&dir.path().join("desk.db")).unwrap(),
        ));
        // Nothing is listening, so the server calls fail fast and the existing
        // offline path is used. The reader half is the part under test.
        let client = rfidex_core::client::ApiClient::new(
            "http://127.0.0.1:1",
            "unused-for-this-test",
            "station-1",
            Duration::from_millis(300),
        );
        let desk = EcrfidDesk::new(sdk_config(true), launcher()).unwrap();
        let mut station = rfidex_core::station::desk::DeskStation::new(
            desk,
            store.clone(),
            client,
            rfidex_core::contract::RfidMode::Write,
            UidRule::AsIs,
            0,
        );
        let ticket = rfidex_core::contract::TicketSummary {
            public_id: uuid::Uuid::from_u128(0x1234_5678),
            name: "Test Guest".into(),
            ticket_type: "Delegate".into(),
            valid: true,
            checked_in: false,
        };
        let tag = rfidex_core::tag::TagRead {
            protocol: rfidex_core::tag::Protocol::Iso15693,
            uid_raw: UID_A.to_vec(),
            vendor_display: None,
            dsfid: Some(0),
            antenna: None,
        };

        // `link` is async, so the test needs a reactor for one future.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let linked = runtime.block_on(station.link(&ticket, &tag, None)).unwrap();
        assert!(
            linked.offline,
            "the server is unreachable, so the binding is queued, not lost"
        );
        assert_eq!(
            helper.count_of("write"),
            1,
            "the desk core wrote the payload once through the real adapter"
        );
        assert!(
            helper.count_of("read") >= 1,
            "and the core read the sticker back to verify it"
        );
        assert_eq!(
            store.lock().unwrap().count(OutboxState::Pending).unwrap(),
            1,
            "the binding is durable on this computer"
        );
        drop(station);
    }
}

//! Process control around the device helper, driven by a fixture that stalls,
//! crashes or lies on purpose.
//!
//! The whole file is behind the `test-host` feature so the default test run
//! reports zero tests here rather than a green result nobody asked for. Run it
//! with the explicit command in the plan.
//!
//! Every test has an outer bound: a fixture that never finishes is killed by
//! the client's own stop path, and the test fails on the clock rather than
//! hanging the suite.

#![cfg(feature = "test-host")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rfidex_hardware::config::{HardwareConfig, SdkConnection};
use rfidex_hardware::process::{HardwareClient, HostLauncher};
use rfidex_hardware::wire::{Operation, Response, WireError};

/// The fixture is chosen by environment variables, so tests that share it must
/// not overlap.
static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rfidex-fake-host"))
}

struct Fixture {
    _exclusive: std::sync::MutexGuard<'static, ()>,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn start(scenario: &str) -> Fixture {
        let exclusive = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("a temporary folder");
        let log = dir.path().join("calls.log");
        std::env::set_var("RFIDEX_FAKE_HOST", scenario);
        std::env::set_var("RFIDEX_FAKE_HOST_LOG", &log);
        Fixture {
            _exclusive: exclusive,
            log,
            _dir: dir,
        }
    }

    fn operations(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn count_of(&self, operation: &str) -> usize {
        let wanted = format!("op={operation}");
        self.operations()
            .iter()
            .filter(|line| line.as_str() == wanted)
            .count()
    }
}

fn config(timeout_ms: u32) -> HardwareConfig {
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
        timeout_ms,
        write_verified: true,
    }
}

fn launcher() -> HostLauncher {
    HostLauncher::new(executable())
}

/// The helper was expected not to start. `HardwareClient` holds a process and a
/// socket and deliberately has no `Debug`, so the failure is unwrapped here.
fn start_error(outcome: Result<HardwareClient, WireError>) -> WireError {
    match outcome {
        Ok(client) => {
            drop(client);
            panic!("the helper was expected not to start");
        }
        Err(e) => e,
    }
}

#[test]
fn a_helper_is_started_authenticated_and_opened_once() {
    let fixture = Fixture::start("echo");
    let mut client = HardwareClient::start(&launcher(), &config(2_000)).expect("the helper starts");
    assert!(client.is_alive());
    assert_eq!(fixture.count_of("open"), 1, "one context per helper");
    assert!(
        matches!(
            client.call(Operation::Inventory).unwrap(),
            Response::Tags { .. }
        ),
        "the helper answers on the same connection"
    );
    client.stop();
    assert!(!client.is_alive());
    assert_eq!(
        fixture.count_of("open"),
        1,
        "a session is not re-opened behind the caller's back"
    );
}

#[test]
fn stalled_operation_can_be_cancelled_without_request_lock() {
    let fixture = Fixture::start("stall_inventory");
    let mut client = HardwareClient::start(&launcher(), &config(9_000)).expect("the helper starts");
    let control = client.stop_control();

    // The call runs on another thread and stalls inside the helper. The test
    // waits until the fixture has the request in hand before cancelling, so it
    // cannot pass by cancelling before any work started.
    let runner = std::thread::spawn(move || client.call(Operation::Inventory));
    let wait_until = Instant::now() + Duration::from_secs(5);
    while fixture.count_of("inventory") == 0 && Instant::now() < wait_until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fixture.count_of("inventory"),
        1,
        "the fixture stalled on the request"
    );

    let cancelled = Instant::now();
    control.stop();
    let outcome = runner.join().expect("the caller finishes");
    assert!(outcome.is_err(), "a cancelled call does not report success");
    assert!(
        cancelled.elapsed() < Duration::from_secs(2),
        "stopping must not wait for the request the helper is stuck on"
    );
    assert!(control.is_stopped());
    assert_eq!(
        fixture.count_of("inventory"),
        1,
        "the cancelled request was not sent again"
    );
}

#[test]
fn stop_during_stalled_open_reaps_child_without_request_lock() {
    let fixture = Fixture::start("stall_open_reply");
    let (tx, rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let outcome = HardwareClient::start_with_stop(&launcher(), &config(9_000), |control| {
            tx.send(control).unwrap();
            Ok(())
        });
        start_error(outcome)
    });
    let control = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("startup control published");
    let wait_until = Instant::now() + Duration::from_secs(5);
    while fixture.count_of("open") == 0 && Instant::now() < wait_until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fixture.count_of("open"), 1, "child stalled during Open");
    let cancelled = Instant::now();
    control.stop();
    assert_eq!(runner.join().unwrap(), WireError::Disconnected);
    assert!(cancelled.elapsed() < Duration::from_secs(2));
    assert_eq!(fixture.count_of("open"), 1);
}

#[test]
fn a_late_partial_reply_cannot_finish_after_deadline() {
    let fixture = Fixture::start("late_partial_reply");
    let mut client = HardwareClient::start(&launcher(), &config(300)).unwrap();
    assert_eq!(client.call(Operation::Inventory), Err(WireError::Timeout));
    assert!(!client.is_alive());
    assert_eq!(fixture.count_of("inventory"), 1);
}

#[test]
fn write_timeout_is_not_retried() {
    let fixture = Fixture::start("stall_write");
    let mut client = HardwareClient::start(&launcher(), &config(300)).expect("the helper starts");

    let outcome = client.call(Operation::Write {
        uid: [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34],
        start: 2,
        data: vec![1, 2, 3, 4],
    });
    assert!(matches!(outcome, Err(WireError::Timeout)), "{outcome:?}");
    assert_eq!(
        fixture.count_of("write"),
        1,
        "a write whose outcome is unknown is reported, never repeated"
    );

    // A later call on the same client is refused: the session is over and the
    // reader is not asked to replay anything.
    assert!(client.call(Operation::Info).is_err());
    assert_eq!(fixture.count_of("write"), 1);

    // A fresh session may read, and that does not resurrect the write either.
    let mut fresh = HardwareClient::start(&launcher(), &config(2_000)).expect("a new helper");
    assert!(fresh.call(Operation::Inventory).is_ok());
    assert_eq!(
        fixture.count_of("write"),
        1,
        "starting again must not resend an unacknowledged write"
    );
    fresh.stop();
}

#[test]
fn a_late_reply_for_another_request_is_refused() {
    let fixture = Fixture::start("wrong_id");
    // The helper answers the opening request with the wrong id, so the session
    // never starts and nothing is left running.
    assert_eq!(
        start_error(HardwareClient::start(&launcher(), &config(2_000))),
        WireError::BadResponse,
        "a mismatched id is a broken channel, not a guess"
    );
    assert_eq!(
        fixture.count_of("open"),
        1,
        "the request was sent exactly once"
    );
}

#[test]
fn an_oversized_frame_from_the_helper_is_refused_before_allocation() {
    let fixture = Fixture::start("oversized");
    let started = Instant::now();
    assert_eq!(
        start_error(HardwareClient::start(&launcher(), &config(2_000))),
        WireError::BadResponse,
        "a length prefix the protocol cannot honour must fail"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the frame cap ends the read instead of waiting for a body that never comes"
    );
    assert_eq!(fixture.count_of("open"), 1);
}

#[test]
fn a_helper_that_crashes_mid_session_is_reported_and_dropped() {
    let fixture = Fixture::start("die_after_open");
    let mut client = HardwareClient::start(&launcher(), &config(2_000)).expect("the helper starts");
    let outcome = client.call(Operation::Info);
    assert!(
        outcome.is_err(),
        "a helper that has gone cannot answer: {outcome:?}"
    );
    assert!(!client.is_alive());
    assert_eq!(
        fixture.count_of("info"),
        1,
        "the operation was attempted once and not retried"
    );
}

#[test]
fn a_helper_that_exits_before_its_first_reply_ends_the_start_quickly() {
    let _fixture = Fixture::start("die_before_open_reply");
    let started = Instant::now();
    let _ = start_error(HardwareClient::start(&launcher(), &config(9_000)));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a helper that has already exited must not hold the whole handshake window"
    );
}

#[test]
fn a_helper_that_never_connects_ends_the_start_quickly() {
    let _fixture = Fixture::start("exit_before_connect");
    let started = Instant::now();
    assert_eq!(
        start_error(HardwareClient::start(&launcher(), &config(9_000))),
        WireError::Disconnected
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a child that never appears is noticed, not waited out"
    );
}

#[test]
fn a_child_that_cannot_be_started_is_an_error_not_a_panic() {
    let _fixture = Fixture::start("echo");
    let missing =
        HostLauncher::new(std::env::temp_dir().join("rfidex-nonexistent-helper-for-this-test"));
    assert_eq!(
        start_error(HardwareClient::start(&missing, &config(1_000))),
        WireError::Disconnected
    );
}

#[test]
fn stopping_twice_is_harmless_and_stops_the_child() {
    let fixture = Fixture::start("echo");
    let client = HardwareClient::start(&launcher(), &config(2_000)).expect("the helper starts");
    let control = client.stop_control();
    control.stop();
    control.stop();
    assert!(control.is_stopped());
    drop(client);
    assert_eq!(fixture.count_of("open"), 1);
}

#[test]
fn stop_ends_a_helper_that_is_stuck_closing() {
    let fixture = Fixture::start("stall_close");
    let mut client = HardwareClient::start(&launcher(), &config(9_000)).expect("the helper starts");
    let control = client.stop_control();
    let runner = std::thread::spawn(move || client.call(Operation::Close));
    let wait_until = Instant::now() + Duration::from_secs(5);
    while fixture.count_of("close") == 0 && Instant::now() < wait_until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fixture.count_of("close"), 1, "the close reached the helper");

    let cancelled = Instant::now();
    control.stop();
    assert!(runner.join().expect("the caller finishes").is_err());
    assert!(
        cancelled.elapsed() < Duration::from_secs(2),
        "a helper stuck closing must not hold shutdown"
    );
}

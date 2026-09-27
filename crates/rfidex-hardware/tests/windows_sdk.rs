//! The vendor SDK boundary, exercised for real on Windows x64 against a fake
//! library built from `tests/support/fake_sdk.rs.txt`.
//!
//! Nothing here runs on another host: the ABI, the calling convention, the
//! ownership rules and the freeing paths are Windows-only by construction. A
//! macOS run of this file is zero tests, which is not a pass.
//!
//! The fake is built by the test with `rustc` into a temporary folder, so no
//! vendor binary is needed and none is committed.

#![cfg(all(windows, target_arch = "x86_64"))]

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rfidex_hardware::config::{HardwareConfig, SdkConnection};
use rfidex_hardware::sdk::{EnumerationKind, SdkReader};
use rfidex_hardware::wire::{Response, WireError, WireTag};

/// The fake chooses its behaviour from the process environment, so the tests
/// that share it must not overlap.
static FAKE: Mutex<()> = Mutex::new(());

const UID: [u8; 8] = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34];

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
}

fn find_export(dll_path: &Path, symbol: &str) -> *mut c_void {
    let path = CString::new(dll_path.to_str().expect("a text path")).expect("no NUL in a path");
    let module = unsafe { LoadLibraryA(path.as_ptr()) };
    assert!(!module.is_null(), "the fake library must load");
    let name = CString::new(symbol).expect("no NUL in a symbol");
    let address = unsafe { GetProcAddress(module, name.as_ptr()) };
    assert!(!address.is_null(), "{symbol} must be exported");
    address
}

fn counter(dll_path: &Path, symbol: &str) -> i32 {
    let address = find_export(dll_path, symbol);
    let function: unsafe extern "system" fn() -> i32 =
        unsafe { std::mem::transmute_copy(&address) };
    unsafe { function() }
}

fn pair(dll_path: &Path, symbol: &str, first: i32, second: i32) -> i32 {
    let address = find_export(dll_path, symbol);
    let function: unsafe extern "system" fn(i32, i32) -> i32 =
        unsafe { std::mem::transmute_copy(&address) };
    unsafe { function(first, second) }
}

struct FakeDll {
    /// The lock is held for the whole test, and released last.
    _exclusive: MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    path: PathBuf,
    log: PathBuf,
}

impl FakeDll {
    fn build() -> FakeDll {
        let exclusive = FAKE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("a temporary folder");
        let source = dir.path().join("fake_sdk.rs");
        std::fs::write(&source, include_str!("support/fake_sdk.rs.txt")).expect("the fake source");
        let path = dir.path().join("ECRFID.dll");
        let status = std::process::Command::new("rustc")
            .arg("--edition=2021")
            .arg("--crate-type=cdylib")
            .arg("--crate-name=ecrfid_fake")
            .arg("-o")
            .arg(&path)
            .arg(&source)
            .status()
            .expect("rustc is on PATH");
        assert!(status.success(), "the fake vendor library must build");
        FakeDll {
            _exclusive: exclusive,
            log: dir.path().join("calls.log"),
            _dir: dir,
            path,
        }
    }

    fn scenario(&self, name: &str) {
        std::env::set_var("RFIDEX_FAKE_SCENARIO", name);
        std::env::set_var("RFIDEX_FAKE_LOG", &self.log);
        let _ = std::fs::remove_file(&self.log);
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn count_of(&self, needle: &str) -> usize {
        self.calls()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    fn counter(&self, symbol: &str) -> i32 {
        counter(&self.path, symbol)
    }

    /// Nothing the vendor handed out may be outstanding, nothing may be freed
    /// twice, and no forbidden operation may have been attempted.
    fn assert_clean(&self) {
        assert_eq!(self.counter("FakeBadFrees"), 0, "a pointer was freed twice");
        assert_eq!(
            self.counter("FakeForbiddenCalls"),
            0,
            "a write that must never happen was attempted"
        );
        assert_eq!(
            self.counter("FakeLiveAllocations"),
            0,
            "something the vendor handed out was never released"
        );
    }

    fn config(&self, inventory_mode: u8) -> HardwareConfig {
        HardwareConfig::EcrfidSdk {
            dll_path: self.path.clone(),
            connection: SdkConnection::Hid {
                model: "EC1101".into(),
                path: "\\\\?\\hid#vid_0483&pid_5750#1".into(),
                address_mode: 1,
                exclusive: 1,
            },
            inventory_mode,
            timeout_ms: 2_000,
            write_verified: false,
        }
    }
}

/// `FakeReadRequest` and `FakeWriteRequest` pack the vendor arguments into one
/// integer so one call reads them back: start in the high byte, count next.
fn start_of(packed: i32) -> i32 {
    (packed >> 16) & 0xFF
}

fn count_of_packed(packed: i32) -> i32 {
    (packed >> 8) & 0xFF
}

fn tags(response: Response) -> Vec<WireTag> {
    match response {
        Response::Tags { tags } => tags,
        other => panic!("expected tags, got {other:?}"),
    }
}

fn bytes(response: Response) -> Vec<u8> {
    match response {
        Response::Bytes { data } => data,
        other => panic!("expected bytes, got {other:?}"),
    }
}

fn records(response: Response) -> Vec<Vec<u8>> {
    match response {
        Response::Records { raw } => raw,
        other => panic!("expected records, got {other:?}"),
    }
}

#[test]
fn sdk_enumerates_without_opening_a_device() {
    let fake = FakeDll::build();
    fake.scenario("none");

    let hid = SdkReader::enumerate(&fake.path, EnumerationKind::Hid).unwrap();
    assert_eq!(
        hid,
        vec![
            "\\\\?\\hid#vid_0483&pid_5750#1".to_string(),
            "\\\\?\\hid#vid_0483&pid_5750#2".to_string()
        ]
    );
    assert_eq!(
        SdkReader::enumerate(&fake.path, EnumerationKind::Com).unwrap(),
        vec!["COM3".to_string()]
    );
    assert!(
        SdkReader::enumerate(&fake.path, EnumerationKind::Net)
            .unwrap()
            .is_empty(),
        "a list of nothing is an empty list, not a failure"
    );

    assert_eq!(
        fake.count_of("open "),
        0,
        "enumeration must not open a device, and must not pick the first one"
    );
    fake.assert_clean();
}

#[test]
fn sdk_inventory_owns_and_decodes_buffers() {
    let fake = FakeDll::build();

    for (scenario, expected) in [
        ("none", 0usize),
        ("one", 1),
        ("two_tags", 2),
        ("five_tags", 5),
    ] {
        fake.scenario(scenario);
        let reader = SdkReader::open(&fake.config(4)).expect("the reader opens");
        let found = tags(reader.inventory().unwrap());
        assert_eq!(found.len(), expected, "scenario {scenario}");
        if expected > 0 {
            assert_eq!(found[0].uid, UID);
            assert_eq!(found[0].dsfid, 0x00);
            assert_eq!(found[0].antenna, None);
        }
        drop(reader);
        fake.assert_clean();
    }

    assert_eq!(
        fake.counter("FakeInventoryAip"),
        0,
        "this profile is ISO15693 and sends the vendor's 0x00 address byte"
    );
    assert_eq!(fake.counter("FakeInventoryMode"), 4, "the configured mode");

    // A seven-byte end marker comes back inside a batch and is not a tag.
    fake.scenario("terminal");
    let reader = SdkReader::open(&fake.config(4)).unwrap();
    assert_eq!(tags(reader.inventory().unwrap()).len(), 1);
    drop(reader);
    fake.assert_clean();

    // Shapes that cannot be trusted end the call, not the process, and the
    // batch is still released exactly once.
    for (scenario, why) in [
        ("status_bad", "a buffer whose status is not success"),
        ("bad_length", "a buffer whose length field is impossible"),
        ("zero_length", "a zero length buffer"),
        ("null_member", "a null pointer inside a positive batch"),
        (
            "over_capacity",
            "more buffers than the profile can describe",
        ),
        ("error", "a negative vendor return code"),
    ] {
        fake.scenario(scenario);
        let reader = SdkReader::open(&fake.config(4)).unwrap();
        assert!(
            reader.inventory().is_err(),
            "{why} must be an error, never an empty field"
        );
        drop(reader);
        fake.assert_clean();
    }

    fake.scenario("none");
    let reader = SdkReader::open(&fake.config(7)).unwrap();
    let _ = reader.inventory().unwrap();
    assert_eq!(
        fake.counter("FakeInventoryMode"),
        7,
        "the mode is not hardcoded"
    );
    drop(reader);
    assert_eq!(fake.counter("FakeCloses"), 1, "one close per context");
    fake.assert_clean();
}

#[test]
fn sdk_reads_and_writes_exact_selected_blocks() {
    let fake = FakeDll::build();
    fake.scenario("read_ok");
    let reader = SdkReader::open(&fake.config(4)).unwrap();

    match reader.memory(&UID).unwrap() {
        Response::Memory {
            block_size,
            block_count,
            raw,
        } => {
            assert_eq!(raw.len(), 32, "the raw answer is preserved");
            assert_eq!(
                (block_size, block_count),
                (4, 28),
                "the demo's four-byte, twenty-eight-block map"
            );
        }
        other => panic!("expected memory, got {other:?}"),
    }

    let data = bytes(reader.read(&UID, 2, 2).unwrap());
    assert_eq!(
        data.len(),
        8,
        "two blocks, four bytes each, and no security bytes"
    );
    let requested = fake.counter("FakeReadRequest");
    assert_eq!(
        (start_of(requested), count_of_packed(requested)),
        (2, 2),
        "the exact block range reached the vendor"
    );
    assert_eq!(
        fake.counter("FakeReadSecurity"),
        0,
        "the demo's readSecSta branch"
    );

    // A read that describes a different sticker, or the wrong number of blocks,
    // is refused rather than sliced.
    for scenario in ["read_corrupt", "read_short"] {
        fake.scenario(scenario);
        let reader = SdkReader::open(&fake.config(4)).unwrap();
        assert!(reader.read(&UID, 2, 2).is_err(), "{scenario}");
        drop(reader);
        fake.assert_clean();
    }

    // Writes: exact block-aligned data only. Nothing reaches the vendor for an
    // unaligned or out-of-range request, which the call log proves.
    fake.scenario("write_ok");
    let reader = SdkReader::open(&fake.config(4)).unwrap();
    for (data, why) in [
        (vec![0u8; 3], "unaligned data"),
        (vec![0u8; 36], "more than eight blocks"),
        (Vec::new(), "no blocks at all"),
    ] {
        let before = fake.count_of("write_multiple_blocks");
        assert!(
            reader.write(&UID, 0, &data).is_err(),
            "{why} must be refused before the vendor is called"
        );
        assert_eq!(
            fake.count_of("write_multiple_blocks"),
            before,
            "{why} reached the vendor library"
        );
    }

    reader.write(&UID, 2, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    let written = fake.counter("FakeWriteRequest");
    assert_eq!(
        (start_of(written), count_of_packed(written)),
        (2, 2),
        "the exact block range and count reached the vendor"
    );
    assert_eq!(
        pair(&fake.path, "FakeMemoryAt", 2, 0),
        1,
        "the first byte written is the first byte stored"
    );
    assert_eq!(pair(&fake.path, "FakeMemoryAt", 2, 7), 8);
    assert_eq!(
        pair(&fake.path, "FakeMemoryAt", 1, 0),
        4,
        "the block before the write keeps its baseline value"
    );
    assert_eq!(
        pair(&fake.path, "FakeMemoryAt", 4, 0),
        16,
        "the block after the write keeps its baseline value"
    );

    // A vendor status that is not success is a failed write, not a silent one.
    fake.scenario("write_error");
    let reader = SdkReader::open(&fake.config(4)).unwrap();
    assert_eq!(
        reader.write(&UID, 2, &[1, 2, 3, 4]),
        Err(WireError::BadResponse)
    );
    drop(reader);
    fake.assert_clean();
}

#[test]
fn sdk_raw_records_never_delete() {
    let fake = FakeDll::build();
    fake.scenario("records_one");
    let reader = SdkReader::open(&fake.config(4)).unwrap();

    let raw = records(reader.raw_records().unwrap());
    assert_eq!(raw.len(), 1);
    assert_eq!(
        raw[0].len(),
        11,
        "the bytes are kept exactly as they arrived"
    );
    assert_eq!(
        fake.counter("FakeRecordsRequest"),
        0x0001,
        "flag 0 and maxcrp 1, the only combination this profile sends"
    );
    assert_eq!(
        fake.count_of("FORBIDDEN"),
        0,
        "no clear, delete, initialise or paged read was attempted"
    );
    drop(reader);
    fake.assert_clean();

    for scenario in ["records_empty", "records_error", "records_many"] {
        fake.scenario(scenario);
        let reader = SdkReader::open(&fake.config(4)).unwrap();
        let outcome = reader.raw_records();
        match scenario {
            "records_empty" => assert!(records(outcome.unwrap()).is_empty()),
            _ => assert!(outcome.is_err(), "{scenario}"),
        }
        drop(reader);
        fake.assert_clean();
    }
}

#[test]
fn sdk_context_is_opened_once_and_closed_once() {
    let fake = FakeDll::build();
    fake.scenario("one");
    let reader = SdkReader::open(&fake.config(4)).unwrap();
    assert_eq!(fake.counter("FakeOpens"), 1);
    drop(reader);
    assert_eq!(fake.counter("FakeCloses"), 1);
    assert_eq!(
        fake.count_of("free_h_global"),
        1,
        "the connection string the vendor built is released exactly once"
    );
    fake.assert_clean();

    // A reader that cannot be opened is not a reader, and nothing is leaked.
    fake.scenario("noopen");
    assert!(matches!(
        SdkReader::open(&fake.config(4)),
        Err(WireError::Disconnected)
    ));
    fake.assert_clean();
}

#[test]
fn sdk_connection_parameters_reach_the_vendor_unchanged() {
    let fake = FakeDll::build();
    fake.scenario("one");
    let reader = SdkReader::open(&fake.config(4)).unwrap();
    let _ = reader.inventory().unwrap();

    let line = fake
        .calls()
        .into_iter()
        .find(|line| line.starts_with("get_hid_connect_string"))
        .expect("the HID builder was called");
    assert!(
        line.contains("model=EC1101")
            && line.contains("addr_mode=1")
            && line.contains("exclusive=1"),
        "the configured connection reached the vendor: {line}"
    );
    assert!(
        line.contains("path=\\\\?\\hid#vid_0483&pid_5750#1"),
        "the selected device path is used, not the first one found: {line}"
    );
    drop(reader);
    fake.assert_clean();

    fake.scenario("one");
    let mut com = fake.config(4);
    if let HardwareConfig::EcrfidSdk { connection, .. } = &mut com {
        *connection = SdkConnection::Com {
            model: "EC1101".into(),
            port: "COM3".into(),
            baud: 38_400,
            frame: "8E1".into(),
            bus_address: 255,
        };
    }
    let reader = SdkReader::open(&com).unwrap();
    let _ = reader.info().unwrap();
    let line = fake
        .calls()
        .into_iter()
        .find(|line| line.starts_with("get_com_connect_string"))
        .expect("the COM builder was called");
    assert!(
        line.contains("port=COM3")
            && line.contains("baud=38400")
            && line.contains("bus_address=255"),
        "{line}"
    );
    drop(reader);
    fake.assert_clean();
}

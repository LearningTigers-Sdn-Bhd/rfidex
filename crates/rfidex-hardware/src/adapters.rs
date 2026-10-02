//! Desk and gate adapters over the two transports.
//!
//! These implement the existing `TagReaderWriter` and `GateSource` traits and
//! nothing else: the desk's confirmation rules, the payload codec, the binding
//! workflow, the gate's debounce and the durable outbox all stay in
//! `rfidex-core` where they already live.
//!
//! Both adapters connect lazily. A reader that is switched off must not stop
//! the app from starting, and must not be opened from the heartbeat: `info()`
//! answers from the cached state, and only `probe()` — an explicit operator
//! action — touches the reader.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rfidex_core::device::{
    DeskCaps, DeviceError, DeviceInfo, DeviceResult, GateCaps, GateKind, GateRead, GateSource,
    ReleaseHandle, TagMemory, TagReaderWriter,
};
use rfidex_core::tag::{Protocol, TagRead};

use crate::config::HardwareConfig;
use crate::process::{HardwareClient, HostLauncher, StopControl};
use crate::tcp::TcpReader;
use crate::wire::{Operation, Response, WireError, WireTag};

/// One control that stops the transport that exists now and every transport
/// that would be built later.
///
/// An adapter is constructed before it is connected, so the control an operator
/// or the shutdown path holds cannot be the socket's own handle: it has to be
/// able to end a connection that has not been made yet.
struct TransportStop {
    stopped: Arc<AtomicBool>,
    current: Arc<Mutex<Option<Arc<StopControl>>>>,
}

impl TransportStop {
    fn new() -> TransportStop {
        TransportStop {
            stopped: Arc::new(AtomicBool::new(false)),
            current: Arc::new(Mutex::new(None)),
        }
    }

    fn control(&self) -> Arc<StopControl> {
        let stopped = self.stopped.clone();
        let current = self.current.clone();
        StopControl::new(move || {
            let live = {
                let mut current = current.lock().unwrap_or_else(|e| e.into_inner());
                stopped.store(true, Ordering::SeqCst);
                current.take()
            };
            if let Some(control) = live {
                control.stop();
            }
        })
    }

    /// Register a freshly built transport. A transport built after the stop
    /// flag was set is stopped at once and never used.
    fn adopt(&self, control: Arc<StopControl>) -> DeviceResult<()> {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped.load(Ordering::SeqCst) {
            drop(current);
            control.stop();
            return Err(DeviceError::Disconnected);
        }
        *current = Some(control);
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

enum Connection {
    Tcp(TcpReader),
    Sdk(HardwareClient),
}

/// A reader for one station, over one transport.
struct Reader {
    config: HardwareConfig,
    launcher: Option<HostLauncher>,
    stop: TransportStop,
    control: Arc<StopControl>,
    connection: Option<Connection>,
    /// Bumped for every transport that is built. A result carrying an older
    /// generation belongs to a reader that no longer exists.
    generation: u64,
    info: Option<DeviceInfo>,
    connected: bool,
    /// Sessions lost since the last good exchange. A helper is a process, and
    /// Windows shows the busy cursor for every one that starts, so a reader
    /// that stays down must not get a new helper on every poll.
    failures: u32,
    retry_at: Option<Instant>,
}

/// The first retry is immediate; after that the wait doubles up to the cap.
const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(10);

impl Reader {
    fn new(config: HardwareConfig, launcher: HostLauncher) -> DeviceResult<Reader> {
        config.validate().map_err(DeviceError::Other)?;
        let stop = TransportStop::new();
        let control = stop.control();
        Ok(Reader {
            launcher: config.is_sdk().then_some(launcher),
            config,
            stop,
            control,
            connection: None,
            generation: 0,
            info: None,
            connected: false,
            failures: 0,
            retry_at: None,
        })
    }

    fn stop_control(&self) -> Arc<StopControl> {
        self.control.clone()
    }

    fn connected(&self) -> bool {
        self.connected
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    /// What the reader last reported, without touching it. Safe on the
    /// heartbeat path.
    fn cached_info(&self) -> DeviceResult<DeviceInfo> {
        self.info.clone().ok_or(DeviceError::Disconnected)
    }

    fn write_supported(&self) -> bool {
        match &self.config {
            HardwareConfig::EcrfidSdk { .. } => self.config.write_verified(),
            // The candidate TCP profile reads. It does not write, and this
            // profile never claims otherwise.
            HardwareConfig::EcV19PlainTcp { .. } => false,
        }
    }

    /// Connect and ask the reader who it is. Returns the cached connection
    /// state to disconnected after a failure rather than leaving a stale
    /// "connected" on screen.
    fn probe(&mut self) -> DeviceResult<DeviceInfo> {
        if self.stop.is_stopped() {
            self.forget();
            return Err(DeviceError::Disconnected);
        }
        // The operator asked, so this one is never made to wait.
        self.retry_at = None;
        let outcome = self.ensure_connection().and_then(|()| self.read_info());
        match outcome {
            Ok(info) => {
                self.info = Some(info.clone());
                self.healthy();
                Ok(info)
            }
            Err(e) => {
                self.forget();
                Err(e)
            }
        }
    }

    fn forget(&mut self) {
        self.connection = None;
        self.connected = false;
        self.note_failure();
    }

    fn note_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let wait = match self.failures {
            0 | 1 => Duration::ZERO,
            n => RETRY_BASE
                .saturating_mul(1 << (n - 2).min(4))
                .min(RETRY_MAX),
        };
        self.retry_at = Some(Instant::now() + wait);
    }

    /// A deliberate hand-back, not a fault: no waiting before the next open.
    fn release(&mut self) {
        self.connection = None;
        self.connected = false;
        self.failures = 0;
        self.retry_at = None;
    }

    fn healthy(&mut self) {
        self.connected = true;
        self.failures = 0;
        self.retry_at = None;
    }

    fn ensure_connection(&mut self) -> DeviceResult<()> {
        if self.stop.is_stopped() {
            return Err(DeviceError::Disconnected);
        }
        if self.connection.is_some() {
            return Ok(());
        }
        self.generation = self.generation.wrapping_add(1);
        match &self.config {
            HardwareConfig::EcV19PlainTcp { .. } => {
                let reader = TcpReader::new(self.config.clone())?;
                self.stop.adopt(reader.stop_control())?;
                self.connection = Some(Connection::Tcp(reader));
                Ok(())
            }
            HardwareConfig::EcrfidSdk { .. } => {
                let launcher = self.launcher.as_ref().ok_or(DeviceError::Disconnected)?;
                if self.retry_at.is_some_and(|at| Instant::now() < at) {
                    return Err(DeviceError::Disconnected);
                }
                let started = HardwareClient::start_with_stop(launcher, &self.config, |control| {
                    self.stop
                        .adopt(control)
                        .map_err(|_| WireError::Disconnected)
                });
                let client = match started {
                    Ok(client) => client,
                    Err(e) => {
                        self.note_failure();
                        return Err(device_error(e));
                    }
                };
                if self.stop.is_stopped() {
                    return Err(DeviceError::Disconnected);
                }
                self.connection = Some(Connection::Sdk(client));
                Ok(())
            }
        }
    }

    fn read_info(&mut self) -> DeviceResult<DeviceInfo> {
        match self.connection.as_mut() {
            Some(Connection::Tcp(reader)) => {
                // The TCP profile has no information call. A real exchange is
                // the only honest connection check, and it is one inventory.
                reader.inventory()?;
                Ok(reader.info())
            }
            Some(Connection::Sdk(client)) => match client.call(Operation::Info) {
                Ok(Response::Info {
                    model, firmware, ..
                }) => Ok(DeviceInfo {
                    adapter: "ecrfid-sdk".to_string(),
                    model,
                    firmware,
                }),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            None => Err(DeviceError::Disconnected),
        }
    }

    fn inventory(&mut self) -> DeviceResult<Vec<TagRead>> {
        self.ensure_connection()?;
        let tags = match self.connection.as_mut() {
            Some(Connection::Tcp(reader)) => reader.inventory(),
            Some(Connection::Sdk(client)) => match client.call(Operation::Inventory) {
                Ok(Response::Tags { tags }) => Ok(tags),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            None => Err(DeviceError::Disconnected),
        };
        match tags {
            Ok(tags) => {
                self.healthy();
                Ok(tags.into_iter().map(tag).collect())
            }
            Err(e) => {
                self.forget();
                Err(e)
            }
        }
    }

    fn tag_memory(&mut self, uid_raw: &[u8]) -> DeviceResult<TagMemory> {
        // The profile decides this, not the connection: a read-only transport
        // is read-only whether or not a reader is attached yet.
        if !self.write_supported() {
            return Err(DeviceError::WriteUnsupported);
        }
        let uid = uid8(uid_raw)?;
        self.ensure_connection()?;
        let result = match self.connection.as_mut() {
            Some(Connection::Tcp(_)) => Err(DeviceError::WriteUnsupported),
            Some(Connection::Sdk(client)) => match client.call(Operation::Memory { uid }) {
                Ok(Response::Memory {
                    block_size,
                    block_count,
                    ..
                }) => Ok(TagMemory {
                    block_size,
                    block_count,
                }),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            None => Err(DeviceError::Disconnected),
        };
        if result.is_err() {
            self.forget();
        }
        result
    }

    fn read_blocks(&mut self, uid_raw: &[u8], start: u8, count: u8) -> DeviceResult<Vec<u8>> {
        if !self.write_supported() {
            return Err(DeviceError::WriteUnsupported);
        }
        let uid = uid8(uid_raw)?;
        self.ensure_connection()?;
        let result = match self.connection.as_mut() {
            Some(Connection::Tcp(_)) => Err(DeviceError::WriteUnsupported),
            Some(Connection::Sdk(client)) => {
                match client.call(Operation::Read { uid, start, count }) {
                    Ok(Response::Bytes { data }) => Ok(data),
                    Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                    Err(e) => Err(device_error(e)),
                }
            }
            None => Err(DeviceError::Disconnected),
        };
        if result.is_err() {
            self.forget();
        }
        result
    }

    fn write_blocks(&mut self, uid_raw: &[u8], start: u8, data: &[u8]) -> DeviceResult<()> {
        if !self.write_supported() {
            // Refused here, before a byte reaches a reader or a helper.
            return Err(DeviceError::WriteUnsupported);
        }
        let uid = uid8(uid_raw)?;
        // Once a session is uncertain, only an explicit read/probe may open a
        // replacement. A second write must never trigger a fresh SDK handle.
        if self.connection.is_none() && self.generation != 0 {
            return Err(DeviceError::Disconnected);
        }
        self.ensure_connection()?;
        let result = match self.connection.as_mut() {
            Some(Connection::Tcp(_)) => Err(DeviceError::WriteUnsupported),
            Some(Connection::Sdk(client)) => match client.call(Operation::Write {
                uid,
                start,
                data: data.to_vec(),
            }) {
                Ok(Response::Unit) => Ok(()),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            None => Err(DeviceError::Disconnected),
        };
        if result.is_err() {
            self.forget();
        }
        result
    }

    fn raw_records(&mut self) -> DeviceResult<Vec<Vec<u8>>> {
        self.records(Operation::RawRecords)
    }

    fn library_alarm(&mut self, mode: u8) -> DeviceResult<()> {
        self.ensure_connection()?;
        let result = match self.connection.as_mut() {
            Some(Connection::Sdk(client)) => match client.call(Operation::LibraryAlarm { mode }) {
                Ok(Response::Unit) => Ok(()),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            _ => Err(DeviceError::WriteUnsupported),
        };
        if result.is_err() {
            self.forget();
        }
        result
    }

    fn clear_gate_records(&mut self) -> DeviceResult<()> {
        self.ensure_connection()?;
        let result = match self.connection.as_mut() {
            Some(Connection::Sdk(client)) => match client.call(Operation::GateClearRecords) {
                Ok(Response::Unit) => Ok(()),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            _ => Err(DeviceError::WriteUnsupported),
        };
        if result.is_err() {
            self.forget();
        }
        result
    }

    fn records(&mut self, operation: Operation) -> DeviceResult<Vec<Vec<u8>>> {
        self.ensure_connection()?;
        let result = match self.connection.as_mut() {
            Some(Connection::Sdk(client)) => match client.call(operation) {
                Ok(Response::Records { raw }) => Ok(raw),
                Ok(_) => Err(DeviceError::Other(UNREADABLE.to_string())),
                Err(e) => Err(device_error(e)),
            },
            _ => Err(DeviceError::WriteUnsupported),
        };
        if result.is_err() {
            self.forget();
        } else {
            self.healthy();
        }
        result
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.control.stop();
    }
}

fn uid8(uid_raw: &[u8]) -> DeviceResult<[u8; 8]> {
    uid_raw.try_into().map_err(|_| DeviceError::OutOfRange)
}

/// The one place a vendor tag becomes a core tag. The eight raw bytes are kept
/// exactly as reported: a vendor screen that shows them reversed is a display
/// choice, and the core's UID rule is the single authority on order.
fn tag(value: WireTag) -> TagRead {
    TagRead {
        protocol: Protocol::Iso15693,
        uid_raw: value.uid.to_vec(),
        vendor_display: None,
        dsfid: Some(value.dsfid),
        antenna: value.antenna,
    }
}

const UNREADABLE: &str = "the reader sent an answer this app cannot read";

fn device_error(e: WireError) -> DeviceError {
    match e {
        WireError::Disconnected => DeviceError::Disconnected,
        WireError::NoTag => DeviceError::TagNotFound,
        WireError::OutOfRange => DeviceError::OutOfRange,
        WireError::WriteUnsupported => DeviceError::WriteUnsupported,
        WireError::Unsupported => {
            DeviceError::Other("this reader needs the Windows build of this app".to_string())
        }
        WireError::BadResponse => DeviceError::Other(UNREADABLE.to_string()),
        WireError::Timeout => DeviceError::Other("the reader did not answer in time".to_string()),
        WireError::Sdk(code) => DeviceError::Other(format!("the reader reported failure {code}")),
    }
}

/// A desk reader/writer. One per station, owned by that station's session.
pub struct EcrfidDesk {
    reader: Reader,
}

impl EcrfidDesk {
    pub fn new(config: HardwareConfig, launcher: HostLauncher) -> DeviceResult<EcrfidDesk> {
        Ok(EcrfidDesk {
            reader: Reader::new(config, launcher)?,
        })
    }

    pub fn stop_control(&self) -> Arc<StopControl> {
        self.reader.stop_control()
    }

    pub fn connected(&self) -> bool {
        self.reader.connected()
    }

    pub fn generation(&self) -> u64 {
        self.reader.generation()
    }

    /// An explicit connection and identity check, for the operator's own test.
    pub fn probe(&mut self) -> DeviceResult<DeviceInfo> {
        self.reader.probe()
    }

    /// Close the helper so another one can open the reader. The next ordinary
    /// call reconnects; a write still refuses until a read reopens it.
    pub fn release(&mut self) {
        self.reader.release();
    }

    /// Raw stored records. Only the commissioning host reaches this: the app's
    /// own commands never ask for gate records, and nothing here deletes one.
    pub fn raw_records(&mut self) -> DeviceResult<Vec<Vec<u8>>> {
        self.reader.raw_records()
    }
}

impl TagReaderWriter for EcrfidDesk {
    fn info(&mut self) -> DeviceResult<DeviceInfo> {
        self.reader.cached_info()
    }

    fn inventory(&mut self) -> DeviceResult<Vec<TagRead>> {
        self.reader.inventory()
    }

    fn tag_memory(&mut self, uid_raw: &[u8]) -> DeviceResult<TagMemory> {
        self.reader.tag_memory(uid_raw)
    }

    fn read_blocks(&mut self, uid_raw: &[u8], start: u8, count: u8) -> DeviceResult<Vec<u8>> {
        self.reader.read_blocks(uid_raw, start, count)
    }

    fn write_blocks(&mut self, uid_raw: &[u8], start: u8, data: &[u8]) -> DeviceResult<()> {
        self.reader.write_blocks(uid_raw, start, data)
    }

    fn capabilities(&self) -> DeskCaps {
        DeskCaps {
            write_supported: self.reader.write_supported(),
        }
    }
}

/// A gate reader. A library (security) gate hands over its own pass records;
/// any other reader falls back to live inventory.
pub struct EcrfidGate {
    reader: Reader,
    /// None until the first records call; Some(false) once a reader has
    /// refused it without ever answering, so it stays on live inventory.
    library: Option<bool>,
    /// False until a fetch was started with flag 0x02.
    started: bool,
    /// Alarm every panel (0x00) instead of only the one that read (0x01).
    alarm_all_panels: bool,
}

impl EcrfidGate {
    pub fn new(config: HardwareConfig, launcher: HostLauncher) -> DeviceResult<EcrfidGate> {
        Ok(EcrfidGate {
            reader: Reader::new(config, launcher)?,
            library: None,
            started: false,
            alarm_all_panels: true,
        })
    }

    pub fn set_alarm_all_panels(&mut self, all: bool) {
        self.alarm_all_panels = all;
    }

    fn library_poll(&mut self) -> DeviceResult<Vec<(GateRead, ReleaseHandle)>> {
        // 0x02 starts a fetch; each 0x01 acknowledges the record returned by
        // the call before, which by then core has stored.
        let flag = if self.started { 0x01 } else { 0x02 };
        let frames = match self.reader.records(Operation::LibraryRecords { flag }) {
            Ok(frames) => frames,
            Err(e) => {
                self.started = false;
                return Err(e);
            }
        };
        self.started = true;
        Ok(frames
            .iter()
            .flat_map(|frame| crate::library_gate::passes(frame))
            .map(|pass| {
                let mut read = GateRead::sighting(pass.uid.to_vec());
                read.device_direction_raw = Some(pass.direction_raw);
                read.device_time_raw = Some(pass.time_raw);
                read.flags_raw = serde_json::json!({
                    "source": "ecrfid_library_gate",
                    "alarm_raw": pass.alarm_raw,
                });
                (read, ReleaseHandle(0))
            })
            .collect())
    }

    pub fn stop_control(&self) -> Arc<StopControl> {
        self.reader.stop_control()
    }

    pub fn connected(&self) -> bool {
        self.reader.connected()
    }

    pub fn generation(&self) -> u64 {
        self.reader.generation()
    }

    pub fn probe(&mut self) -> DeviceResult<DeviceInfo> {
        self.reader.probe()
    }

    /// Red light and buzzer on a library gate, for a pass that is not let in.
    /// A gate on live inventory has no alarm this app knows, so nothing sounds.
    pub fn alarm(&mut self) -> DeviceResult<()> {
        if self.library != Some(true) {
            return Ok(());
        }
        // 0x00 sounds every panel, 0x01 ("independent alarm", the vendor
        // demo's choice) only the panel that read the sticker.
        let result = self
            .reader
            .library_alarm(if self.alarm_all_panels { 0x00 } else { 0x01 });
        // The alarm command cuts into the 0x02/0x01 record handshake: the gate
        // then ignores the next ack and serves the same pass again. Start a
        // fresh fetch so the queue moves on.
        self.started = false;
        result
    }

    /// Wipe the pass records stored on the gate. The next poll starts a fresh
    /// 0x02 fetch, as after an alarm.
    pub fn clear_records(&mut self) -> DeviceResult<()> {
        self.started = false;
        self.reader.clear_gate_records()
    }

    /// Raw library-gate frames for the operator's test. Steps through the
    /// queue as the live poll does; restarting it every press would show the
    /// oldest pass again and again.
    pub fn library_records(&mut self) -> DeviceResult<Vec<Vec<u8>>> {
        let flag = if self.started { 0x01 } else { 0x02 };
        let result = self.reader.records(Operation::LibraryRecords { flag });
        self.started = result.is_ok();
        result
    }
}

impl GateSource for EcrfidGate {
    fn info(&mut self) -> DeviceResult<DeviceInfo> {
        self.reader.cached_info()
    }

    fn poll(&mut self) -> DeviceResult<Vec<(GateRead, ReleaseHandle)>> {
        if self.library != Some(false) {
            match self.library_poll() {
                Ok(reads) => {
                    self.library = Some(true);
                    return Ok(reads);
                }
                Err(e) if self.library == Some(true) => return Err(e),
                Err(_) => self.library = Some(false),
            }
        }
        let tags = self.reader.inventory()?;
        Ok(tags
            .into_iter()
            .map(|tag| {
                (
                    GateRead {
                        tag,
                        // Live inventory sees a sticker, nothing more. No
                        // payload was read, no device direction or time was
                        // reported, and no record sequence was allocated: the
                        // configured station role stays the authority.
                        payload: None,
                        device_direction_raw: None,
                        device_time_raw: None,
                        device_record_seq: None,
                        flags_raw: serde_json::json!({
                            "source": "ecrfid_live_inventory",
                            "profile": "unverified",
                        }),
                    },
                    ReleaseHandle(0),
                )
            })
            .collect())
    }

    fn release(&mut self, _handle: ReleaseHandle) -> DeviceResult<()> {
        // Live inventory consumes nothing: the sticker leaves the field on its
        // own. There is no acknowledgement to send and nothing to delete, and
        // `release_verified` stays false so core never asks for one.
        Ok(())
    }

    fn capabilities(&self) -> GateCaps {
        GateCaps {
            kind: GateKind::LiveInventory,
            release_verified: false,
        }
    }
}

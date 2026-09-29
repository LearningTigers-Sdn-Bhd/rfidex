//! Concrete device adapters, and the shared simulated sticker library.
//!
//! `DeskDevice` and `GateDevice` are enums over real device objects rather than
//! boxed trait-object factories, so the runtime owns exactly one adapter per
//! station. The real variants own a reader that connects lazily and whose
//! native work happens outside this process or outside the Tokio worker threads.
//!
//! The simulator accessors are fallible on purpose: a command that only makes
//! sense on a fake reader must be refused on a real one, not panic and not
//! quietly change what the real reader does.

use std::collections::HashMap;
use std::sync::Arc;

use rfidex_core::codec;
use rfidex_core::contract::Role;
use rfidex_core::device::sim_desk::{SimDesk, SimTag};
use rfidex_core::device::sim_gate::SimGate;
use rfidex_core::device::{
    DeskCaps, DeviceInfo, DeviceResult, GateCaps, GateKind, GateRead, GateSource, ReleaseHandle,
    TagMemory, TagReaderWriter,
};
use rfidex_core::tag::{hex_upper, parse_hex};
use rfidex_hardware::adapters::{EcrfidDesk, EcrfidGate};
use rfidex_hardware::process::StopControl;
use uuid::Uuid;

use crate::RuntimeError;

/// What every simulator-only command says when it is asked of real hardware.
pub(crate) fn not_simulated() -> RuntimeError {
    RuntimeError::new("not_simulated", "This station uses a real reader.")
}

/// The simulated reader/writer a desk station drives. `SimDesk::info()` succeeds
/// even when the reader is unplugged, so never treat it as a connection check.
pub enum DeskDevice {
    Sim(SimDesk),
    /// Boxed: a real reader carries its transport, its cached info and its
    /// stop handle, and the enum is stored inside a station that already owns a
    /// boxed session. One allocation per station at startup.
    Ecrfid(Box<EcrfidDesk>),
}

impl DeskDevice {
    /// The last known connection state, never a fresh probe: the heartbeat must
    /// not open a reader.
    pub fn connected(&self) -> bool {
        match self {
            DeskDevice::Sim(d) => d.connected,
            DeskDevice::Ecrfid(d) => d.connected(),
        }
    }

    /// Simulator-only. A real reader's connection state is what its own calls
    /// report, and no operator command may declare it connected.
    pub fn set_connected(&mut self, connected: bool) -> Result<(), RuntimeError> {
        match self {
            DeskDevice::Sim(d) => {
                d.connected = connected;
                Ok(())
            }
            DeskDevice::Ecrfid(_) => Err(not_simulated()),
        }
    }

    /// The simulator handle, for the `sim_*` commands only.
    pub fn sim_mut(&mut self) -> Result<&mut SimDesk, RuntimeError> {
        match self {
            DeskDevice::Sim(d) => Ok(d),
            DeskDevice::Ecrfid(_) => Err(not_simulated()),
        }
    }

    /// Present only for real hardware. Shutdown uses it to end a reader that is
    /// stuck without waiting for the call that is stuck.
    pub fn stop_control(&self) -> Option<Arc<StopControl>> {
        match self {
            DeskDevice::Sim(_) => None,
            DeskDevice::Ecrfid(d) => Some(d.stop_control()),
        }
    }

    /// An explicit connection and identity check, for the operator's own test.
    /// Never called from a heartbeat.
    pub fn probe(&mut self) -> DeviceResult<DeviceInfo> {
        match self {
            DeskDevice::Sim(d) => d.info(),
            DeskDevice::Ecrfid(d) => d.probe(),
        }
    }

    pub fn is_simulated(&self) -> bool {
        matches!(self, DeskDevice::Sim(_))
    }

    /// Let go of a real reader so the sticker write test can open it.
    pub fn release(&mut self) {
        if let DeskDevice::Ecrfid(d) = self {
            d.release();
        }
    }
}

impl TagReaderWriter for DeskDevice {
    fn info(&mut self) -> DeviceResult<DeviceInfo> {
        match self {
            DeskDevice::Sim(d) => d.info(),
            DeskDevice::Ecrfid(d) => d.info(),
        }
    }

    fn inventory(&mut self) -> DeviceResult<Vec<rfidex_core::tag::TagRead>> {
        match self {
            DeskDevice::Sim(d) => d.inventory(),
            DeskDevice::Ecrfid(d) => d.inventory(),
        }
    }

    fn tag_memory(&mut self, uid_raw: &[u8]) -> DeviceResult<TagMemory> {
        match self {
            DeskDevice::Sim(d) => d.tag_memory(uid_raw),
            DeskDevice::Ecrfid(d) => d.tag_memory(uid_raw),
        }
    }

    fn read_blocks(&mut self, uid_raw: &[u8], start: u8, count: u8) -> DeviceResult<Vec<u8>> {
        match self {
            DeskDevice::Sim(d) => d.read_blocks(uid_raw, start, count),
            DeskDevice::Ecrfid(d) => d.read_blocks(uid_raw, start, count),
        }
    }

    fn write_blocks(&mut self, uid_raw: &[u8], start: u8, data: &[u8]) -> DeviceResult<()> {
        match self {
            DeskDevice::Sim(d) => d.write_blocks(uid_raw, start, data),
            DeskDevice::Ecrfid(d) => d.write_blocks(uid_raw, start, data),
        }
    }

    fn capabilities(&self) -> DeskCaps {
        match self {
            DeskDevice::Sim(d) => d.capabilities(),
            DeskDevice::Ecrfid(d) => d.capabilities(),
        }
    }
}

/// The simulated gate a gate station drives. `SimGate::info()` succeeds even
/// when disconnected; only a poll result tells the truth.
pub enum GateDevice {
    Sim(SimGate),
    /// Boxed for the same reason as [`DeskDevice::Ecrfid`].
    Ecrfid(Box<EcrfidGate>),
}

impl GateDevice {
    pub fn kind(&self) -> GateKind {
        match self {
            GateDevice::Sim(g) => g.capabilities().kind,
            GateDevice::Ecrfid(g) => g.capabilities().kind,
        }
    }

    pub fn set_connected(&mut self, connected: bool) -> Result<(), RuntimeError> {
        match self {
            GateDevice::Sim(g) => {
                g.connected = connected;
                Ok(())
            }
            GateDevice::Ecrfid(_) => Err(not_simulated()),
        }
    }

    /// The simulator handle, for the `sim_*` commands only.
    pub fn sim(&mut self) -> Result<&mut SimGate, RuntimeError> {
        match self {
            GateDevice::Sim(g) => Ok(g),
            GateDevice::Ecrfid(_) => Err(not_simulated()),
        }
    }

    pub fn stop_control(&self) -> Option<Arc<StopControl>> {
        match self {
            GateDevice::Sim(_) => None,
            GateDevice::Ecrfid(g) => Some(g.stop_control()),
        }
    }

    pub fn probe(&mut self) -> DeviceResult<DeviceInfo> {
        match self {
            GateDevice::Sim(g) => g.info(),
            GateDevice::Ecrfid(g) => g.probe(),
        }
    }

    pub fn is_simulated(&self) -> bool {
        matches!(self, GateDevice::Sim(_))
    }

    pub fn library_records(&mut self) -> DeviceResult<Vec<Vec<u8>>> {
        match self {
            GateDevice::Sim(_) => Ok(Vec::new()),
            GateDevice::Ecrfid(g) => g.library_records(),
        }
    }
}

impl GateSource for GateDevice {
    fn info(&mut self) -> DeviceResult<DeviceInfo> {
        match self {
            GateDevice::Sim(g) => g.info(),
            GateDevice::Ecrfid(g) => g.info(),
        }
    }

    fn poll(&mut self) -> DeviceResult<Vec<(GateRead, ReleaseHandle)>> {
        match self {
            GateDevice::Sim(g) => g.poll(),
            GateDevice::Ecrfid(g) => g.poll(),
        }
    }

    fn release(&mut self, handle: ReleaseHandle) -> DeviceResult<()> {
        match self {
            GateDevice::Sim(g) => g.release(handle),
            GateDevice::Ecrfid(g) => g.release(handle),
        }
    }

    fn capabilities(&self) -> GateCaps {
        match self {
            GateDevice::Sim(g) => g.capabilities(),
            GateDevice::Ecrfid(g) => g.capabilities(),
        }
    }
}

/// Stickers that are not currently sitting on a desk reader, and which station
/// is holding the ones that are.
///
/// Keyed by the un-reordered uppercase UID, because a physical sticker is the
/// same object whatever byte-order rule a station reads it with. Memory travels
/// with the sticker: removing it from a reader is not erasing it.
#[derive(Default)]
pub struct SimLibrary {
    stickers: HashMap<String, SimTag>,
    owners: HashMap<String, Uuid>,
}

/// A UID the operator typed, in the one canonical spelling the simulator uses.
pub fn uid_key(uid_hex: &str) -> Result<(String, Vec<u8>), RuntimeError> {
    let raw = parse_hex(uid_hex).map_err(|_| bad_sticker())?;
    if raw.is_empty() {
        return Err(bad_sticker());
    }
    Ok((hex_upper(&raw), raw))
}

fn bad_sticker() -> RuntimeError {
    RuntimeError::new(
        "bad_sticker",
        "Enter the sticker UID as hex digits, for example 3412CDAB500104E0.",
    )
}

impl SimLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a sticker on a desk reader. A UID that is already on this reader is
    /// the same tag, not a second one; a UID on another reader has to be taken
    /// off it first. A never-seen UID becomes a blank 4-byte-block sticker with
    /// 28 blocks, the memory map the vendor demo assumes.
    pub fn place(
        &mut self,
        station: Uuid,
        desk: &mut SimDesk,
        uid_hex: &str,
    ) -> Result<(), RuntimeError> {
        let (key, raw) = uid_key(uid_hex)?;
        if desk.tag(&raw).is_some() {
            return Ok(());
        }
        match self.owners.get(&key) {
            Some(owner) if *owner != station => {
                return Err(RuntimeError::new(
                    "sticker_in_use",
                    "Remove this sticker from the other reader first.",
                ))
            }
            _ => {}
        }
        let tag = self
            .stickers
            .remove(&key)
            .unwrap_or_else(|| SimTag::blank(raw, 4, 28));
        desk.place(tag);
        self.owners.insert(key, station);
        Ok(())
    }

    /// Take every sticker off a desk reader and park it in the library with
    /// whatever memory it now holds.
    pub fn clear(&mut self, station: Uuid, desk: &mut SimDesk) {
        for tag in desk.clear() {
            let key = hex_upper(&tag.uid_raw);
            self.owners.remove(&key);
            self.stickers.insert(key, tag);
        }
        self.owners.retain(|_, owner| *owner != station);
    }

    /// Walk a sticker past a gate. The gate sees a snapshot; the sticker itself
    /// stays where it is, so a written desk sticker keeps its memory. An unknown
    /// UID becomes a blank sticker so the unknown-sticker path stays testable.
    pub fn pass(
        &mut self,
        gate: &mut SimGate,
        uid_hex: &str,
        role: Role,
        device_record_seq: Option<u64>,
        write_start_block: u8,
    ) -> Result<(), RuntimeError> {
        let (key, raw) = uid_key(uid_hex)?;
        if self.owners.contains_key(&key) {
            return Err(RuntimeError::new(
                "sticker_in_use",
                "Remove this sticker from the reader first, then walk it past the gate.",
            ));
        }
        let tag = self
            .stickers
            .entry(key)
            .or_insert_with(|| SimTag::blank(raw.clone(), 4, 28));
        let read = GateRead {
            tag: rfidex_core::tag::TagRead {
                protocol: rfidex_core::tag::Protocol::Iso15693,
                uid_raw: tag.uid_raw.clone(),
                vendor_display: None,
                dsfid: Some(0),
                antenna: None,
            },
            payload: payload_of(tag, write_start_block),
            // The vendor demo shows 0x00 as IN and anything else as OUT; the
            // station's configured role stays authoritative.
            device_direction_raw: Some(match role {
                Role::Entry => 0,
                Role::Exit => 1,
            }),
            device_time_raw: None,
            device_record_seq,
            flags_raw: serde_json::Value::Null,
        };
        gate.push(read);
        Ok(())
    }

    /// Park a sticker that was never on a reader, so it can be found later.
    pub fn remember(&mut self, uid_hex: &str) -> Result<(), RuntimeError> {
        let (key, raw) = uid_key(uid_hex)?;
        self.stickers
            .entry(key)
            .or_insert_with(|| SimTag::blank(raw, 4, 28));
        Ok(())
    }

    pub fn owner(&self, uid_hex: &str) -> Option<Uuid> {
        uid_key(uid_hex)
            .ok()
            .and_then(|(key, _)| self.owners.get(&key).copied())
    }
}

/// The block data a gate would see, starting at the station's configured block
/// and long enough for one payload. Out of range means the gate returns none,
/// exactly as a real gate would when it cannot read that region.
fn payload_of(tag: &SimTag, write_start_block: u8) -> Option<Vec<u8>> {
    let from = write_start_block as usize * tag.block_size;
    let to = from.checked_add(codec::PAYLOAD_LEN)?;
    tag.memory.get(from..to).map(<[u8]>::to_vec)
}

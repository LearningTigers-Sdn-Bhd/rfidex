//! Device adapter boundary (spec §2.2). Every vendor SDK call lives behind
//! these traits. One OS thread owns each adapter instance.

use serde::{Deserialize, Serialize};

use crate::tag::{Protocol, TagRead};

pub mod sim_desk;
pub mod sim_gate;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("device disconnected")]
    Disconnected,
    #[error("tag not in the reader field")]
    TagNotFound,
    #[error("block range out of bounds")]
    OutOfRange,
    #[error("write is not supported on this device")]
    WriteUnsupported,
    #[error("device error: {0}")]
    Other(String),
}

pub type DeviceResult<T> = Result<T, DeviceError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub adapter: String,
    pub model: Option<String>,
    pub firmware: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagMemory {
    pub block_size: usize,
    pub block_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeskCaps {
    /// False for every real adapter until disposable-sticker tests pass (spec §7.1).
    pub write_supported: bool,
}

pub trait TagReaderWriter: Send {
    fn info(&mut self) -> DeviceResult<DeviceInfo>;
    fn inventory(&mut self) -> DeviceResult<Vec<TagRead>>;
    fn tag_memory(&mut self, uid_raw: &[u8]) -> DeviceResult<TagMemory>;
    fn read_blocks(&mut self, uid_raw: &[u8], start: u8, count: u8) -> DeviceResult<Vec<u8>>;
    fn write_blocks(&mut self, uid_raw: &[u8], start: u8, data: &[u8]) -> DeviceResult<()>;
    fn capabilities(&self) -> DeskCaps;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Records,
    LiveInventory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateCaps {
    pub kind: GateKind,
    /// True only when acknowledge/delete semantics were proven on hardware (spec §7 H5).
    pub release_verified: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateRead {
    pub tag: TagRead,
    pub payload: Option<Vec<u8>>,
    pub device_direction_raw: Option<u8>,
    pub device_time_raw: Option<[u8; 6]>,
    pub device_record_seq: Option<u64>,
    pub flags_raw: serde_json::Value,
}

impl GateRead {
    pub fn sighting(uid_raw: Vec<u8>) -> GateRead {
        GateRead {
            tag: TagRead {
                protocol: Protocol::Iso15693,
                uid_raw,
                vendor_display: None,
                dsfid: None,
                antenna: None,
            },
            payload: None,
            device_direction_raw: None,
            device_time_raw: None,
            device_record_seq: None,
            flags_raw: serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReleaseHandle(pub u64);

pub trait GateSource: Send {
    fn info(&mut self) -> DeviceResult<DeviceInfo>;
    fn poll(&mut self) -> DeviceResult<Vec<(GateRead, ReleaseHandle)>>;
    /// Only after the read is committed locally, and only when
    /// `capabilities().release_verified` is true.
    fn release(&mut self, handle: ReleaseHandle) -> DeviceResult<()>;
    fn capabilities(&self) -> GateCaps;
}

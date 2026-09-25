//! Station health: queue depth, conflicts, parked items, disk space.

use crate::store::{OutboxState, Store, StoreError};

pub const DEEP_QUEUE: u64 = 50_000;
pub const LOW_DISK_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alarm {
    DeepQueue(u64),
    LowDisk(u64),
    Conflicts(u64),
    Parked(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub pending: u64,
    pub conflicts: u64,
    pub parked: u64,
    pub alarms: Vec<Alarm>,
}

/// `free_bytes` comes from the OS (Plan 2); `None` skips the disk check.
pub fn health(
    store: &Store,
    free_bytes: Option<u64>,
    deep_queue_threshold: u64,
) -> Result<Health, StoreError> {
    let pending = store.count(OutboxState::Pending)?;
    let conflicts = store.count(OutboxState::Conflict)?;
    let parked = store.count(OutboxState::Parked)?;
    let mut alarms = Vec::new();
    if pending >= deep_queue_threshold {
        alarms.push(Alarm::DeepQueue(pending));
    }
    if let Some(free) = free_bytes.filter(|f| *f < LOW_DISK_BYTES) {
        alarms.push(Alarm::LowDisk(free));
    }
    if conflicts > 0 {
        alarms.push(Alarm::Conflicts(conflicts));
    }
    if parked > 0 {
        alarms.push(Alarm::Parked(parked));
    }
    Ok(Health {
        pending,
        conflicts,
        parked,
        alarms,
    })
}

//! Gate station (spec §3.3): poll → debounce → commit → release (if verified).
//! Direction comes from the configured role, never from the device.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::contract::{ObservationItem, Role};
use crate::device::{DeviceError, GateKind, GateSource};
use crate::store::{Enqueued, OutboxKind, Store, StoreError};
use crate::tag::{hex_upper, tag_key, UidRule};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalGuess {
    Known { name: String, ticket_type: String },
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    pub delivery_id: Uuid,
    pub local: LocalGuess,
}

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct GateStation<G> {
    pub gate: G,
    store: Arc<Mutex<Store>>,
    station: String,
    role: Role,
    uid_rule: UidRule,
    debounce: chrono::Duration,
    last_seen: HashMap<String, DateTime<Utc>>,
}

impl<G: GateSource> GateStation<G> {
    pub fn new(
        gate: G,
        store: Arc<Mutex<Store>>,
        station: &str,
        role: Role,
        uid_rule: UidRule,
        debounce: Duration,
    ) -> GateStation<G> {
        GateStation {
            gate,
            store,
            station: station.to_string(),
            role,
            uid_rule,
            debounce: chrono::Duration::from_std(debounce).expect("debounce fits chrono"),
            last_seen: HashMap::new(),
        }
    }

    pub fn tick(&mut self, now: DateTime<Utc>) -> Result<Vec<Captured>, GateError> {
        let caps = self.gate.capabilities();
        let mut out = Vec::new();
        for (read, handle) in self.gate.poll()? {
            let key = tag_key(&read.tag.uid_raw, self.uid_rule);
            if caps.kind == GateKind::LiveInventory {
                if self
                    .last_seen
                    .get(&key)
                    .is_some_and(|t| now - *t < self.debounce)
                {
                    continue;
                }
                self.last_seen.insert(key.clone(), now);
            }
            let delivery_id = Uuid::now_v7();
            let item = ObservationItem {
                delivery_id,
                role: self.role,
                protocol: read.tag.protocol,
                uid_raw_hex: hex_upper(&read.tag.uid_raw),
                payload_hex: read.payload.as_deref().map(hex_upper),
                device_direction_raw: read.device_direction_raw,
                device_time_raw_hex: read.device_time_raw.map(|t| hex_upper(&t)),
                device_record_seq: read.device_record_seq,
                flags_raw: read.flags_raw.clone(),
                captured_at: now,
            };
            let idem = match read.device_record_seq {
                Some(seq) => format!("{}:seq:{seq}", self.station),
                None => delivery_id.to_string(),
            };
            let (enqueued, local) = {
                let s = self.store.lock().unwrap();
                let payload = serde_json::to_value(&item).expect("serializable");
                let enqueued = s.enqueue(OutboxKind::Observation, &idem, &payload, now)?;
                let holder = s.binding_holder(&key)?;
                let local = match holder.map(|pid| s.ticket(pid)).transpose()?.flatten() {
                    Some(t) => LocalGuess::Known {
                        name: t.name,
                        ticket_type: t.ticket_type,
                    },
                    None => LocalGuess::Unknown,
                };
                (enqueued, local)
            };
            // Committed above; only now may the device forget the record.
            if caps.release_verified {
                self.gate.release(handle)?;
            }
            if let Enqueued::New(_) = enqueued {
                out.push(Captured { delivery_id, local });
            }
        }
        Ok(out)
    }
}

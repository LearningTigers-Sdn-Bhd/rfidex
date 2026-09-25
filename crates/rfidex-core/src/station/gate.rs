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

    /// Changing the rule changes which tags count as already seen, so the
    /// debounce memory is dropped with it.
    pub fn set_uid_rule(&mut self, uid_rule: UidRule) {
        if self.uid_rule != uid_rule {
            self.last_seen.clear();
            self.uid_rule = uid_rule;
        }
    }

    /// Two phases so one failed release can never lose a read:
    /// 1. save every polled read to the outbox (committed per row);
    /// 2. only then release them, trying all and reporting the first failure.
    ///
    /// On `Err` the reads are still saved; the UI shows them from the store.
    pub fn tick(&mut self, now: DateTime<Utc>) -> Result<Vec<Captured>, GateError> {
        let caps = self.gate.capabilities();
        let mut out = Vec::new();
        let mut saved = Vec::new();
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
            saved.push(handle);
            if let Enqueued::New(_) = enqueued {
                out.push(Captured { delivery_id, local });
            }
        }
        // Every read above is committed; only now may the device forget them.
        let mut first_err = None;
        if caps.release_verified {
            for handle in saved {
                if let Err(e) = self.gate.release(handle) {
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e.into()),
            None => Ok(out),
        }
    }
}

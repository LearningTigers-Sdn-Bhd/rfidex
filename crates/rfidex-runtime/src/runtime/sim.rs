//! Simulator commands: placing stickers and walking passes past simulated readers.

use std::sync::atomic::Ordering;

use rfidex_core::contract::Role;
use rfidex_core::device::GateKind;
use uuid::Uuid;

use super::device::StationDevice;
use super::messages::{store_failure, wrong_station};
use super::station::StationRuntime;
use super::{Runtime, SEQUENCE_KEY};
use crate::config::DeviceChoice;
use crate::RuntimeError;

impl StationRuntime {
    /// Allocate, and durably record, the next simulated gate record sequence.
    /// Persisting the allocation before the read is pushed means a restart
    /// cannot hand the same sequence to a different passage.
    fn allocate_sequence(&self) -> Result<Option<u64>, RuntimeError> {
        let records = matches!(
            self.config.device,
            DeviceChoice::SimGate {
                gate_kind: GateKind::Records,
                ..
            }
        );
        if !records {
            return Ok(None);
        }
        let seq = self.next_sequence.fetch_add(1, Ordering::SeqCst);
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_config(SEQUENCE_KEY, &(seq + 1).to_string())
            .map_err(|_| store_failure())?;
        Ok(Some(seq))
    }
}

impl Runtime {
    pub async fn sim_place(&self, station: Uuid, uid_hex: &str) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("place stickers on a desk"));
        };
        let mut session = device.lock().await;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        library.place(station, session.station.reader.sim_mut()?, uid_hex)?;
        drop(library);
        let connected = session.station.reader.connected();
        drop(session);
        let mut inner = runtime.lock();
        inner.connected = connected;
        inner.connection_checked = true;
        Ok(())
    }

    pub async fn sim_clear(&self, station: Uuid) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("remove stickers from a desk"));
        };
        let mut session = device.lock().await;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        library.clear(station, session.station.reader.sim_mut()?);
        Ok(())
    }

    pub async fn sim_pass(&self, station: Uuid, uid_hex: &str) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Gate(device) = &runtime.device else {
            return Err(wrong_station("walk a sticker past a gate"));
        };
        let role = runtime.role().unwrap_or(Role::Entry);
        let mut gate = device.lock().await;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        // Refused before a sequence is allocated: a rejected simulator command
        // must not advance a real station's record counter.
        let sim = gate.gate.sim()?;
        let seq = runtime.allocate_sequence()?;
        library.pass(sim, uid_hex, role, seq, runtime.config.write_start_block)
    }

    pub async fn sim_set_connected(
        &self,
        station: Uuid,
        connected: bool,
    ) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        // The refusal comes first: a real reader's connection state is reported
        // by the reader, never declared by an operator command.
        match &runtime.device {
            StationDevice::Desk(device) => {
                let mut session = device.lock().await;
                session.station.reader.set_connected(connected)?;
            }
            StationDevice::Gate(device) => {
                let mut gate = device.lock().await;
                gate.gate.set_connected(connected)?;
            }
        }
        let mut inner = runtime.lock();
        inner.connected = connected;
        inner.connection_checked = true;
        Ok(())
    }
}

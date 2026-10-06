//! Switching a gate between entry and exit without restarting anything.

use chrono::Utc;
use rfidex_core::contract::{Role, StationKind};
use uuid::Uuid;

use super::device::StationDevice;
use super::messages::wrong_station;
use super::Runtime;
use crate::config::AppConfig;
use crate::RuntimeError;

impl Runtime {
    /// Apply a setup change that only switches gate directions, without
    /// stopping any station or reconnecting any reader. Returns `false`, doing
    /// nothing, when `candidate` differs from the running setup in anything
    /// else: that needs the full restart.
    pub async fn apply_gate_roles(&self, candidate: &AppConfig) -> Result<bool, RuntimeError> {
        let mut expected = self.config();
        for station in &mut expected.stations {
            if station.kind != StationKind::Gate {
                continue;
            }
            if let Some(new) = candidate.stations.iter().find(|s| s.id == station.id) {
                station.role = new.role;
            }
        }
        let same = |a: &AppConfig, b: &AppConfig| {
            serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
        };
        if !same(&expected, candidate) {
            return Ok(false);
        }
        for station in &self.stations {
            let StationDevice::Gate(gate) = &station.device else {
                continue;
            };
            let Some(role) = candidate
                .stations
                .iter()
                .find(|s| s.id == station.config.id)
                .and_then(|s| s.role)
            else {
                continue;
            };
            if station.role() == Some(role) {
                continue;
            }
            gate.lock().await.set_role(role);
            *station.role.lock().unwrap_or_else(|e| e.into_inner()) = Some(role);
            // Tell the server now, not at the next heartbeat.
            station.heartbeat_once(Utc::now()).await;
        }
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = candidate.clone();
        Ok(true)
    }

    /// The Gate screen's quick switch: set one gate's direction and save it,
    /// with no restart. Same path as a role-only Setup save.
    pub async fn set_gate_role(&self, station: Uuid, role: Role) -> Result<(), RuntimeError> {
        let old = self.config();
        let mut candidate = old.clone();
        let gate = candidate
            .stations
            .iter_mut()
            .find(|s| s.id == station && s.kind == StationKind::Gate)
            .ok_or_else(|| wrong_station("switch direction"))?;
        gate.role = Some(role);
        if !self.apply_gate_roles(&candidate).await? {
            return Err(wrong_station("switch direction"));
        }
        if self.paths.save(&candidate).is_err() {
            let _ = self.apply_gate_roles(&old).await;
            return Err(RuntimeError::new(
                "config_unwritable",
                "The new direction could not be saved. Nothing was changed.",
            ));
        }
        Ok(())
    }
}

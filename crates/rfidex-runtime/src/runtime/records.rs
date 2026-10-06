//! Reading back what a station saved: recent gate passes and open problems.

use rfidex_core::store::{OutboxKind, OutboxState};
use rfidex_core::tag::UidRule;
use uuid::Uuid;

use super::device::StationDevice;
use super::messages::{store_failure, wrong_station};
use super::Runtime;
use crate::gate::GateView;
use crate::problems::ProblemView;
use crate::RuntimeError;

impl Runtime {
    /// The most recent passages this gate has saved, newest first. Zero asks
    /// for nothing; the cap keeps a long-running station from flooding the UI.
    pub async fn gate_recent(
        &self,
        station: Uuid,
        limit: usize,
    ) -> Result<Vec<GateView>, RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Gate(_) = &runtime.device else {
            return Err(wrong_station("read gate results"));
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let uid_rule = runtime
            .snapshot()
            .settings
            .map(|s| s.uid_rule)
            .unwrap_or(UidRule::AsIs);
        let store = runtime.store.lock().unwrap_or_else(|e| e.into_inner());
        let rows = store
            .rows(
                &[
                    OutboxState::Pending,
                    OutboxState::Sent,
                    OutboxState::Conflict,
                    OutboxState::Parked,
                    OutboxState::Dismissed,
                ],
                Some(OutboxKind::Observation),
                limit.min(200),
            )
            .map_err(|_| store_failure())?;
        Ok(rows
            .iter()
            .map(|row| crate::gate::view(row, &store, uid_rule))
            .collect())
    }

    /// Everything that still needs a person, from every configured station.
    pub async fn problems(&self) -> Result<Vec<ProblemView>, RuntimeError> {
        let mut all = Vec::new();
        for station in &self.stations {
            let uid_rule = station
                .snapshot()
                .settings
                .map(|s| s.uid_rule)
                .unwrap_or(UidRule::AsIs);
            let store = station.store.lock().unwrap_or_else(|e| e.into_inner());
            all.extend(crate::problems::for_station(
                station.config.id,
                &station.config.name,
                &store,
                uid_rule,
            )?);
        }
        crate::problems::sort(&mut all);
        Ok(all)
    }

    /// Hide one problem from the list. The row keeps its payload, its server
    /// reply and its error, so nothing is lost and nothing is claimed fixed.
    pub async fn dismiss(&self, station: Uuid, id: i64) -> Result<bool, RuntimeError> {
        let runtime = self.station(station)?;
        runtime
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_dismissed(id)
            .map_err(|_| store_failure())
    }
}

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

    /// Write every unsent, parked, conflicting or dismissed row to a fresh CSV
    /// under the exports folder and return its path. See
    /// `problems::export_lines` for what it holds and why.
    pub async fn export_problems(&self) -> Result<std::path::PathBuf, RuntimeError> {
        use std::io::Write;

        let failure = || {
            RuntimeError::new(
                "export_failed",
                "Could not write the problems file. Check the disk and try again.",
            )
        };
        let mut lines = Vec::new();
        for station in &self.stations {
            let uid_rule = station
                .snapshot()
                .settings
                .map(|s| s.uid_rule)
                .unwrap_or(UidRule::AsIs);
            let store = station.store.lock().unwrap_or_else(|e| e.into_inner());
            lines.extend(crate::problems::export_lines(
                station.config.id,
                &station.config.name,
                &store,
                uid_rule,
            )?);
        }
        lines.sort_by_key(|l| l.sort_key);

        let dir = self.paths().exports();
        std::fs::create_dir_all(&dir).map_err(|_| failure())?;
        let path = dir.join(format!(
            "problems-{}-{}.csv",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            Uuid::new_v4()
        ));
        let mut out = String::from(crate::problems::EXPORT_HEADER);
        out.push_str("\r\n");
        for entry in &lines {
            out.push_str(&entry.line);
            out.push_str("\r\n");
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| failure())?;
        if file
            .write_all(out.as_bytes())
            .and_then(|_| file.flush())
            .is_err()
        {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(failure());
        }
        Ok(path)
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

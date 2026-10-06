//! The status bar: one station's summary and the whole app's.

use chrono::{DateTime, Utc};
use rfidex_core::contract::{RfidMode, Role, StationKind};
use uuid::Uuid;

use super::messages::{
    store_failure, COULD_NOT_BE_SENT, COULD_NOT_CHECK_DISK, DIFFERENT_EVENT, LINKS_NEED_ATTENTION,
    LOW_DISK, MANY_WAITING, UNAUTHORIZED,
};
use super::station::StationRuntime;
use super::Runtime;
use crate::config::DeviceChoice;
use crate::RuntimeError;

#[derive(Debug, Clone, serde::Serialize)]
pub struct StationStatus {
    pub id: Uuid,
    pub name: String,
    pub kind: StationKind,
    pub role: Option<Role>,
    pub simulated: bool,
    pub online: bool,
    pub unauthorized: bool,
    pub connected: bool,
    pub connection_checked: bool,
    pub event_name: Option<String>,
    pub mode: Option<RfidMode>,
    pub settings_ready: bool,
    pub skew_secs: Option<i64>,
    pub last_sync: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub pending: u64,
    pub problems: u64,
    pub alarm: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AppStatus {
    pub print_provider: String,
    pub stations: Vec<StationStatus>,
    pub pending: u64,
    pub problems: u64,
    pub alarm: Option<String>,
}

fn station_status(
    station: &StationRuntime,
    free_bytes: Option<u64>,
    disk_note: Option<&str>,
) -> Result<StationStatus, RuntimeError> {
    let health = {
        let store = station.store.lock().unwrap_or_else(|e| e.into_inner());
        rfidex_core::health::health(&store, free_bytes, rfidex_core::health::DEEP_QUEUE)
            .map_err(|_| store_failure())?
    };
    let inner = station.snapshot();

    // Precedence: a rejected key, then a wrong event, then trouble saving or
    // reading here, and only then a network problem.
    let last_error = if inner.unauthorized {
        Some(UNAUTHORIZED.to_string())
    } else if inner.event_mismatch {
        Some(DIFFERENT_EVENT.to_string())
    } else if let Some(local) = inner.store_error.or(inner.device_error) {
        Some(local)
    } else {
        inner.network_error
    };

    let mut alarms = Vec::new();
    if let Some(note) = disk_note {
        alarms.push(note.to_string());
    }
    for alarm in &health.alarms {
        alarms.push(
            match alarm {
                rfidex_core::health::Alarm::DeepQueue(_) => MANY_WAITING,
                rfidex_core::health::Alarm::LowDisk(_) => LOW_DISK,
                rfidex_core::health::Alarm::Conflicts(_) => LINKS_NEED_ATTENTION,
                rfidex_core::health::Alarm::Parked(_) => COULD_NOT_BE_SENT,
            }
            .to_string(),
        );
    }
    if let Some(skew) = inner.skew_secs.filter(|s| s.abs() > 60) {
        alarms.push(format!(
            "This computer's clock is about {} minutes away from the server.",
            (skew.abs() + 30) / 60
        ));
    }

    Ok(StationStatus {
        id: station.config.id,
        name: station.config.name.clone(),
        kind: station.config.kind,
        role: station.role(),
        simulated: matches!(
            station.config.device,
            DeviceChoice::SimDesk | DeviceChoice::SimGate { .. }
        ),
        online: inner.online,
        unauthorized: inner.unauthorized,
        connected: inner.connected,
        connection_checked: inner.connection_checked,
        event_name: inner.event_name,
        mode: inner.settings.as_ref().map(|s| s.event.rfid_mode),
        settings_ready: inner.settings.is_some(),
        skew_secs: inner.skew_secs,
        last_sync: inner.last_sync,
        last_error,
        pending: health.pending,
        problems: health.conflicts + health.parked,
        alarm: (!alarms.is_empty()).then(|| alarms.join(" ")),
    })
}

impl Runtime {
    pub async fn status(&self) -> Result<AppStatus, RuntimeError> {
        // Measured once for the whole app rather than per station.
        let (free_bytes, disk_note) = match fs2::available_space(self.paths.root()) {
            Ok(bytes) => (Some(bytes), None),
            Err(_) => (None, Some(COULD_NOT_CHECK_DISK)),
        };
        let mut stations = Vec::with_capacity(self.stations.len());
        let mut pending = 0;
        let mut problems = 0;
        let mut alarms = Vec::new();
        for station in &self.stations {
            let status = station_status(station, free_bytes, disk_note)?;
            pending += status.pending;
            problems += status.problems;
            if let Some(alarm) = &status.alarm {
                alarms.push(format!("{}: {alarm}", status.name));
            }
            stations.push(status);
        }
        Ok(AppStatus {
            print_provider: self.badge.provider(),
            stations,
            pending,
            problems,
            alarm: (!alarms.is_empty()).then(|| alarms.join(" ")),
        })
    }
}

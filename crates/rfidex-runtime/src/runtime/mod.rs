//! Station ownership, background tasks, status, sync and shutdown.
//!
//! One `Runtime` owns every configured station. Each station keeps its own
//! SQLite store, its own `ApiClient` carrying its own UUID, and its own
//! `SyncWorker`, so no station can ever speak for another.

mod build;
mod desk_ops;
mod device;
mod gate_poll;
mod hardware_jobs;
mod hardware_tests;
mod heartbeat;
mod lifecycle;
mod messages;
mod options;
mod probe;
mod records;
mod roles;
mod sim;
mod station;
mod status;
mod sync;

pub use options::RuntimeOptions;
pub use station::StationRuntime;
pub use status::{AppStatus, StationStatus};

pub(crate) use messages::store_failure;

use std::sync::{Arc, Mutex};

use rfidex_hardware::process::HostLauncher;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{AppConfig, AppPaths};
use crate::devices::SimLibrary;
use crate::sticker_test::PendingTear;
use crate::RuntimeError;

use hardware_jobs::HardwareJobs;

/// Where a station's successful settings are remembered between runs.
const SETTINGS_KEY: &str = "heartbeat";
/// The next simulated gate record sequence this station must allocate.
const SEQUENCE_KEY: &str = "sim_next_sequence";

pub struct Runtime {
    paths: AppPaths,
    badge: crate::BadgeService,
    config: Mutex<AppConfig>,
    library: Arc<Mutex<SimLibrary>>,
    stations: Vec<Arc<StationRuntime>>,
    stop: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    hardware_jobs: Arc<HardwareJobs>,
    launcher: HostLauncher,
    /// The one tear write waiting for its check, across all desks.
    tear: Mutex<Option<PendingTear>>,
}

impl Runtime {
    pub fn with_badge(mut self, badge: crate::BadgeService) -> Self {
        self.badge = badge;
        self
    }
    pub fn badge(&self) -> &crate::BadgeService {
        &self.badge
    }

    /// End every real reader this runtime owns.
    fn cancel_hardware(&self) {
        for station in &self.stations {
            if let Some(control) = &station.hardware_stop {
                control.stop();
            }
        }
    }

    /// The ticket types of this event, from the stations' local ticket cache.
    pub fn ticket_types(&self) -> Vec<String> {
        let mut types: Vec<String> = Vec::new();
        for station in &self.stations {
            let store = station.store.lock().unwrap_or_else(|e| e.into_inner());
            for name in store.ticket_types().unwrap_or_default() {
                if !types.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
                    types.push(name);
                }
            }
        }
        types.sort_by_key(|t| t.to_lowercase());
        types
    }

    pub fn stations(&self) -> &[Arc<StationRuntime>] {
        &self.stations
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn config(&self) -> AppConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn station(&self, id: Uuid) -> Result<&Arc<StationRuntime>, RuntimeError> {
        self.stations
            .iter()
            .find(|s| s.config.id == id)
            .ok_or_else(|| RuntimeError::new("station_not_found", "There is no such station."))
    }
}

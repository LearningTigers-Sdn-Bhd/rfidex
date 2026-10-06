//! One configured station: its state, read-only views and locking helpers.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rfidex_core::client::ApiClient;
use rfidex_core::contract::{HeartbeatResp, RfidMode, Role, StationKind};
use rfidex_core::store::{OutboxKind, OutboxState, Store};
use rfidex_core::sync::SyncWorker;
use rfidex_hardware::process::StopControl;
use tokio::sync::watch;
use uuid::Uuid;

use super::device::StationDevice;
use super::hardware_jobs::HardwareJobs;
use super::messages::store_failure;
use crate::config::StationConfig;
use crate::RuntimeError;

/// Everything the status bar needs, kept apart from the device and store locks
/// so reading it can never wait on a network call.
#[derive(Default)]
pub(super) struct StationInner {
    pub(super) settings: Option<HeartbeatResp>,
    /// True once a heartbeat in this run confirmed the event the saved rows
    /// belong to. Sync stays parked until then.
    pub(super) event_ok: bool,
    pub(super) event_mismatch: bool,
    pub(super) online: bool,
    pub(super) unauthorized: bool,
    pub(super) connected: bool,
    pub(super) connection_checked: bool,
    pub(super) event_name: Option<String>,
    pub(super) skew_secs: Option<i64>,
    pub(super) last_sync: Option<DateTime<Utc>>,
    pub(super) network_error: Option<String>,
    pub(super) device_error: Option<String>,
    pub(super) store_error: Option<String>,
}

/// One configured station: its own store, its own client carrying its own
/// UUID, its own sync worker, and the device it drives.
pub struct StationRuntime {
    pub(super) config: StationConfig,
    /// A gate's direction can change while it runs, so it lives outside
    /// `config`; every read of the role goes through `role()`.
    pub(super) role: Mutex<Option<Role>>,
    pub(super) store: Arc<Mutex<Store>>,
    pub(super) client: ApiClient,
    pub(super) worker: SyncWorker,
    /// Serialises every sync pass for this station, manual or scheduled.
    pub(super) sync_lock: tokio::sync::Mutex<()>,
    /// Woken when the heartbeat confirms the event, so the first cache refresh
    /// and queue drain happen at once instead of after a poll interval.
    pub(super) notify: tokio::sync::Notify,
    pub(super) device: StationDevice,
    /// Present only for a real reader. Shutdown uses it to end hardware that is
    /// stuck without waiting for the call that is stuck.
    pub(super) hardware_stop: Option<Arc<StopControl>>,
    /// The runtime's blocking-worker tracker, so this station's own loops put
    /// device work where the rest of the hardware work goes.
    pub(super) hardware_jobs: Arc<HardwareJobs>,
    pub(super) inner: Mutex<StationInner>,
    pub(super) next_sequence: AtomicU64,
    pub(super) stop: watch::Receiver<bool>,
}

/// Read-only view of a station, for status and for tests. Never waits on the
/// device or the store.
impl StationRuntime {
    pub fn id(&self) -> Uuid {
        self.config.id
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn kind(&self) -> StationKind {
        self.config.kind
    }

    pub fn role(&self) -> Option<Role> {
        *self.role.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn online(&self) -> bool {
        self.lock().online
    }

    pub fn connected(&self) -> bool {
        self.lock().connected
    }

    pub fn unauthorized(&self) -> bool {
        self.lock().unauthorized
    }

    /// True once this run confirmed which event the station's rows belong to.
    pub fn event_ok(&self) -> bool {
        self.lock().event_ok
    }

    pub fn event_mismatch(&self) -> bool {
        self.lock().event_mismatch
    }

    pub fn settings(&self) -> Option<HeartbeatResp> {
        self.lock().settings.clone()
    }

    pub fn mode(&self) -> Option<RfidMode> {
        self.lock().settings.as_ref().map(|s| s.event.rfid_mode)
    }
}

impl StationRuntime {
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, StationInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn snapshot(&self) -> StationInner {
        let inner = self.lock();
        StationInner {
            settings: inner.settings.clone(),
            event_ok: inner.event_ok,
            event_mismatch: inner.event_mismatch,
            online: inner.online,
            unauthorized: inner.unauthorized,
            connected: inner.connected,
            connection_checked: inner.connection_checked,
            event_name: inner.event_name.clone(),
            skew_secs: inner.skew_secs,
            last_sync: inner.last_sync,
            network_error: inner.network_error.clone(),
            device_error: inner.device_error.clone(),
            store_error: inner.store_error.clone(),
        }
    }

    pub(super) fn pending_count(&self) -> Result<u64, RuntimeError> {
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .count(OutboxState::Pending)
            .map_err(|_| store_failure())
    }

    /// Outbox rows read under the store lock and returned before any file work
    /// begins, so the diagnostics export never writes while holding it.
    pub(crate) fn outbox_rows(
        &self,
        states: &[OutboxState],
        kind: Option<OutboxKind>,
        limit: usize,
    ) -> Result<Vec<rfidex_core::store::OutboxRow>, rfidex_core::store::StoreError> {
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows(states, kind, limit)
    }
}

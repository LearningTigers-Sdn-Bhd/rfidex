//! Station ownership, background tasks, status, sync and shutdown.
//!
//! One `Runtime` owns every configured station. Each station keeps its own
//! SQLite store, its own `ApiClient` carrying its own UUID, and its own
//! `SyncWorker`, so no station can ever speak for another.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rfidex_core::client::{ApiClient, ApiError};
use rfidex_core::contract::{HeartbeatReq, HeartbeatResp, RfidMode, Role, StationKind};
use rfidex_core::device::{DeviceError, GateKind, GateSource, TagReaderWriter};
use rfidex_core::station::desk::DeskStation;
use rfidex_core::station::gate::{GateError, GateStation};
use rfidex_core::store::{OutboxKind, OutboxState, Store};
use rfidex_core::sync::{SyncReport, SyncWorker, SENT_RETENTION_DAYS};
use rfidex_core::tag::UidRule;
use rfidex_core::APP_VERSION;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{AppConfig, AppPaths, DeviceChoice, StationConfig};
use crate::desk::{DeskSession, DeskView};
use crate::devices::{DeskDevice, GateDevice, SimLibrary};
use crate::gate::GateView;
use crate::problems::ProblemView;
use crate::RuntimeError;

/// Where a station's successful settings are remembered between runs.
const SETTINGS_KEY: &str = "heartbeat";
/// The next simulated gate record sequence this station must allocate.
const SEQUENCE_KEY: &str = "sim_next_sequence";

#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    pub heartbeat: Duration,
    pub gate_poll: Duration,
    pub client_timeout: Duration,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(30),
            gate_poll: Duration::from_millis(250),
            client_timeout: Duration::from_secs(5),
        }
    }
}

/// Boxed so the enum stays small whatever a device holds; one allocation per
/// station at startup.
enum StationDevice {
    Desk(Box<tokio::sync::Mutex<DeskSession>>),
    Gate(Box<tokio::sync::Mutex<GateStation<GateDevice>>>),
}

/// Everything the status bar needs, kept apart from the device and store locks
/// so reading it can never wait on a network call.
#[derive(Default)]
struct StationInner {
    settings: Option<HeartbeatResp>,
    /// True once a heartbeat in this run confirmed the event the saved rows
    /// belong to. Sync stays parked until then.
    event_ok: bool,
    event_mismatch: bool,
    online: bool,
    unauthorized: bool,
    connected: bool,
    event_name: Option<String>,
    skew_secs: Option<i64>,
    last_sync: Option<DateTime<Utc>>,
    network_error: Option<String>,
    device_error: Option<String>,
    store_error: Option<String>,
}

/// One configured station: its own store, its own client carrying its own
/// UUID, its own sync worker, and the device it drives.
pub struct StationRuntime {
    config: StationConfig,
    store: Arc<Mutex<Store>>,
    client: ApiClient,
    worker: SyncWorker,
    /// Serialises every sync pass for this station, manual or scheduled.
    sync_lock: tokio::sync::Mutex<()>,
    /// Woken when the heartbeat confirms the event, so the first cache refresh
    /// and queue drain happen at once instead of after a poll interval.
    notify: tokio::sync::Notify,
    device: StationDevice,
    inner: Mutex<StationInner>,
    next_sequence: AtomicU64,
    stop: watch::Receiver<bool>,
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
        self.config.role
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
    fn lock(&self) -> std::sync::MutexGuard<'_, StationInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn snapshot(&self) -> StationInner {
        let inner = self.lock();
        StationInner {
            settings: inner.settings.clone(),
            event_ok: inner.event_ok,
            event_mismatch: inner.event_mismatch,
            online: inner.online,
            unauthorized: inner.unauthorized,
            connected: inner.connected,
            event_name: inner.event_name.clone(),
            skew_secs: inner.skew_secs,
            last_sync: inner.last_sync,
            network_error: inner.network_error.clone(),
            device_error: inner.device_error.clone(),
            store_error: inner.store_error.clone(),
        }
    }

    fn pending_count(&self) -> Result<u64, RuntimeError> {
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

    async fn device_info(&self) -> Option<rfidex_core::device::DeviceInfo> {
        match &self.device {
            StationDevice::Desk(d) => {
                let mut session = d.lock().await;
                session.station.reader.info().ok()
            }
            StationDevice::Gate(g) => {
                let mut gate = g.lock().await;
                gate.gate.info().ok()
            }
        }
    }

    /// Read the simulated reader flag. Only the desk has one that is meaningful
    /// between operations; a gate learns its connection from an actual poll.
    async fn refresh_desk_connection(&self) {
        if let StationDevice::Desk(d) = &self.device {
            let connected = {
                let session = d.lock().await;
                session.station.reader.connected()
            };
            self.lock().connected = connected;
        }
    }

    async fn apply_settings(&self, resp: &HeartbeatResp) {
        match &self.device {
            StationDevice::Desk(d) => {
                let mut session = d.lock().await;
                session
                    .station
                    .configure(resp.event.rfid_mode, resp.uid_rule);
                session.view.mode = resp.event.rfid_mode;
                // A confirmation checked against the old mode or UID rule must
                // not carry over to the new one.
                session.pending = None;
            }
            StationDevice::Gate(g) => {
                let mut gate = g.lock().await;
                gate.set_uid_rule(resp.uid_rule);
            }
        }
    }

    async fn heartbeat_once(&self, now: DateTime<Utc>) {
        let info = self.device_info().await;
        let req = HeartbeatReq {
            name: self.config.name.clone(),
            kind: self.config.kind,
            role: self.config.role,
            hw_model: info.as_ref().and_then(|i| i.model.clone()),
            firmware: info.as_ref().and_then(|i| i.firmware.clone()),
            app_version: APP_VERSION.to_string(),
        };
        match self.client.heartbeat(&req).await {
            Ok(resp) => {
                let skew = (resp.server_time - now).num_seconds();
                let pending = self.pending_count().unwrap_or(0);
                let adopt = {
                    let inner = self.lock();
                    match &inner.settings {
                        None => true,
                        Some(saved) => saved.event.event_id == resp.event.event_id || pending == 0,
                    }
                };
                if adopt {
                    if let Err(e) = self.remember_settings(&resp) {
                        self.lock().store_error = Some(e.message);
                    }
                    self.apply_settings(&resp).await;
                    let mut inner = self.lock();
                    inner.event_name = Some(resp.event.name.clone());
                    inner.settings = Some(resp);
                    inner.event_ok = true;
                    inner.event_mismatch = false;
                } else {
                    // Rows belong to another event: keep the saved settings, the
                    // mode and the UID rule exactly as they are.
                    let mut inner = self.lock();
                    inner.event_ok = false;
                    inner.event_mismatch = true;
                }
                let mut inner = self.lock();
                inner.online = true;
                inner.unauthorized = false;
                inner.network_error = None;
                inner.skew_secs = Some(skew);
                let ready = inner.event_ok;
                drop(inner);
                if ready {
                    self.notify.notify_one();
                }
            }
            Err(ApiError::Unauthorized) => {
                let mut inner = self.lock();
                inner.online = false;
                inner.unauthorized = true;
                inner.network_error = None;
            }
            Err(e) => {
                let message = network_message(&e);
                let mut inner = self.lock();
                inner.online = false;
                inner.network_error = Some(message);
            }
        }
        self.refresh_desk_connection().await;
    }

    fn remember_settings(&self, resp: &HeartbeatResp) -> Result<(), RuntimeError> {
        let text = serde_json::to_string(resp).map_err(|_| store_failure())?;
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_config(SETTINGS_KEY, &text)
            .map_err(|_| store_failure())
    }

    async fn gate_tick_once(&self, now: DateTime<Utc>) {
        let StationDevice::Gate(device) = &self.device else {
            return;
        };
        if self.lock().settings.is_none() {
            // The UID rule is not known yet, so a read could not be keyed
            // correctly. Leave it in the device rather than consuming it.
            return;
        }
        let mut gate = device.lock().await;
        match gate.tick(now) {
            Ok(captured) => {
                let mut inner = self.lock();
                inner.connected = true;
                inner.device_error = None;
                let _ = captured;
            }
            Err(GateError::Device(DeviceError::Disconnected)) => {
                let mut inner = self.lock();
                inner.connected = false;
                inner.device_error = None;
            }
            Err(GateError::Device(e)) => {
                let mut inner = self.lock();
                inner.device_error = Some(match e {
                    DeviceError::Disconnected => unreachable!("handled above"),
                    DeviceError::TagNotFound => {
                        "The gate could not read a sticker. Try again.".to_string()
                    }
                    DeviceError::OutOfRange => {
                        "The gate could not store what it read. Ask for help.".to_string()
                    }
                    DeviceError::WriteUnsupported => "This gate cannot do that.".to_string(),
                    DeviceError::Other(_) => {
                        "The gate could not finish the last action. Check it and try again."
                            .to_string()
                    }
                });
            }
            Err(GateError::Store(_)) => {
                let mut inner = self.lock();
                inner.store_error = Some(store_failure().message);
            }
        }
    }

    fn note_store_error(&self, e: &rfidex_core::store::StoreError) {
        let _ = e;
        self.lock().store_error = Some(store_failure().message);
    }

    /// The heartbeat owns "online"; a sync pass only reports what the queue did.
    fn note_sync_report(&self, report: &SyncReport, now: DateTime<Utc>) {
        let mut inner = self.lock();
        if report.unauthorized {
            inner.unauthorized = true;
            inner.online = false;
            inner.network_error = None;
            return;
        }
        // A retried, conflicted or parked row is not a green status, so only a
        // fully clean pass moves the last-sync time.
        if report.retried == 0 && report.conflicts == 0 && report.parked == 0 {
            inner.last_sync = Some(now);
            inner.store_error = None;
        }
    }

    fn note_cache_error(&self, e: &rfidex_core::sync::SyncError) {
        let message = match e {
            rfidex_core::sync::SyncError::Api(api) => network_message(api),
            rfidex_core::sync::SyncError::Store(_) => store_failure().message,
        };
        self.lock().network_error = Some(message);
    }

    fn note_cache_ok(&self) {
        self.lock().network_error = None;
    }

    fn prune_sent(&self) {
        let cutoff = Utc::now() - chrono::Duration::days(SENT_RETENTION_DAYS);
        if let Err(e) = self
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prune_sent(cutoff)
        {
            self.note_store_error(&e);
        }
    }

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

fn store_failure() -> RuntimeError {
    RuntimeError::new(
        "save_failed",
        "Could not save this action on this computer. Stop and ask for help.",
    )
}

/// Wording the operator sees, kept in one place so the status bar, the alarm
/// line and the station list cannot drift apart.
const UNAUTHORIZED: &str = "The server did not accept the API key. Open Setup to check it.";
const DIFFERENT_EVENT: &str =
    "This API key belongs to a different event. Open Setup and enter the key for this event.";
const MANY_WAITING: &str = "Many actions are waiting to send.";
const LOW_DISK: &str = "Disk space is low. Free some space before continuing.";
const LINKS_NEED_ATTENTION: &str = "Some sticker links need attention.";
const COULD_NOT_BE_SENT: &str = "Some saved actions could not be sent.";
const COULD_NOT_CHECK_DISK: &str = "Could not check free disk space.";

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
        role: station.config.role,
        simulated: matches!(
            station.config.device,
            DeviceChoice::SimDesk | DeviceChoice::SimGate { .. }
        ),
        online: inner.online,
        unauthorized: inner.unauthorized,
        connected: inner.connected,
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

fn network_message(e: &ApiError) -> String {
    match e {
        ApiError::Unauthorized => {
            "The server did not accept the API key. Open Setup to check it.".to_string()
        }
        ApiError::Retryable(_) => {
            "Cannot reach the server. Try again when the connection returns.".to_string()
        }
        ApiError::BadResponse(_) => {
            "The server sent a reply this app cannot read. Ask for help.".to_string()
        }
        ApiError::Rejected { .. } => {
            "The server could not accept this action. Check the ticket and try again.".to_string()
        }
    }
}

pub struct Runtime {
    paths: AppPaths,
    config: AppConfig,
    library: Arc<Mutex<SimLibrary>>,
    stations: Vec<Arc<StationRuntime>>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    shutdown_error: Option<RuntimeError>,
}

impl Runtime {
    pub async fn start(
        paths: AppPaths,
        config: AppConfig,
        opts: RuntimeOptions,
    ) -> Result<Self, RuntimeError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            RuntimeError::new(
                "no_reactor",
                "The app must start its stations inside its own async runtime.",
            )
        })?;
        if opts.heartbeat.is_zero() || opts.gate_poll.is_zero() || opts.client_timeout.is_zero() {
            return Err(RuntimeError::new(
                "bad_options",
                "The station timing settings must be greater than zero.",
            ));
        }
        config.validate().map_err(|e| match e {
            crate::ConfigError::Invalid(message) => RuntimeError::new("invalid_setup", &message),
            other => RuntimeError::new("invalid_setup", &other.to_string()),
        })?;
        std::fs::create_dir_all(paths.root().join("stations")).map_err(|_| {
            RuntimeError::new(
                "no_storage",
                "Could not create the folder this app saves its work in.",
            )
        })?;

        // Build every station before spawning anything: a failure halfway
        // through must not leave earlier stations running with no owner.
        let library = Arc::new(Mutex::new(SimLibrary::new()));
        let (stop, _) = watch::channel(false);
        let mut stations = Vec::with_capacity(config.stations.len());
        for station_config in &config.stations {
            stations.push(Arc::new(
                StationRuntime::build(&paths, &config, station_config, &opts, &stop).await?,
            ));
        }

        let mut tasks = Vec::new();
        for station in &stations {
            tasks.push(tokio::spawn(heartbeat_loop(station.clone(), opts.clone())));
            tasks.push(tokio::spawn(sync_loop(station.clone())));
            if matches!(station.device, StationDevice::Gate(_)) {
                tasks.push(tokio::spawn(gate_loop(station.clone(), opts.clone())));
            }
        }

        Ok(Runtime {
            paths,
            config,
            library,
            stations,
            stop,
            tasks,
            shutdown_error: None,
        })
    }

    pub async fn shutdown(&mut self) -> Result<(), RuntimeError> {
        let _ = self.stop.send(true);
        let tasks = std::mem::take(&mut self.tasks);
        let mut failure = None;
        for task in tasks {
            match task.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {}
                Err(_) => {
                    failure.get_or_insert_with(|| {
                        RuntimeError::new(
                            "station_stopped",
                            "A station stopped before finishing. Start the app again.",
                        )
                    });
                }
            }
        }
        if let Some(e) = failure {
            self.shutdown_error = Some(e.clone());
            return Err(e);
        }
        Ok(())
    }

    /// Try a normal sync pass on every station, in configuration order.
    ///
    /// This respects retry backoff: it is "try now", not "ignore the schedule".
    pub async fn sync_now(&self) -> Result<(), RuntimeError> {
        let mut failures: Vec<String> = Vec::new();
        for station in &self.stations {
            let _guard = station.sync_lock.lock().await;
            if !station.lock().event_ok {
                failures.push(format!(
                    "{}: not syncing yet — this station is waiting to hear which event it belongs to.",
                    station.config.name
                ));
                continue;
            }
            match station.worker.refresh_cache().await {
                Ok(()) => station.note_cache_ok(),
                Err(e) => {
                    station.note_cache_error(&e);
                    failures.push(format!(
                        "{}: could not refresh the ticket list.",
                        station.config.name
                    ));
                }
            }
            // A failed cache refresh still gets a sync attempt, so queued work
            // moves if only the cache endpoint is unhappy.
            match station.worker.run_once(Utc::now()).await {
                Ok(report) => {
                    station.note_sync_report(&report, Utc::now());
                    if report.unauthorized {
                        failures.push(format!(
                            "{}: the server did not accept the API key.",
                            station.config.name
                        ));
                    }
                }
                Err(e) => {
                    station.note_store_error(&e);
                    failures.push(store_failure().message);
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            failures.dedup();
            Err(RuntimeError::new("sync_failed", &failures.join(" ")))
        }
    }

    /// Read a scanned ticket code at a desk. Business failures come back as an
    /// error step the desk screen can show; only a wrong station is rejected.
    pub async fn desk_scan(&self, station: Uuid, code: &str) -> Result<DeskView, RuntimeError> {
        let (runtime, mut session) = self.desk_session(station).await?;
        if runtime.settings().is_none() {
            return Ok(session.connect_first());
        }
        Ok(crate::desk::scan(&mut session, &runtime.store, code).await)
    }

    pub async fn desk_link(
        &self,
        station: Uuid,
        reason: Option<String>,
    ) -> Result<DeskView, RuntimeError> {
        let (runtime, mut session) = self.desk_session(station).await?;
        if runtime.settings().is_none() {
            return Ok(session.connect_first());
        }
        Ok(crate::desk::link(&mut session, &runtime.store, reason).await)
    }

    pub async fn desk_reset(&self, station: Uuid) -> Result<DeskView, RuntimeError> {
        let (_, mut session) = self.desk_session(station).await?;
        Ok(crate::desk::reset(&mut session))
    }

    async fn desk_session(
        &self,
        id: Uuid,
    ) -> Result<
        (
            &Arc<StationRuntime>,
            tokio::sync::MutexGuard<'_, DeskSession>,
        ),
        RuntimeError,
    > {
        let runtime = self.station(id)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("use this at a desk"));
        };
        Ok((runtime, device.lock().await))
    }

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
            stations,
            pending,
            problems,
            alarm: (!alarms.is_empty()).then(|| alarms.join(" ")),
        })
    }

    pub async fn sim_place(&self, station: Uuid, uid_hex: &str) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("place stickers on a desk"));
        };
        let mut session = device.lock().await;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        library.place(station, session.station.reader.sim_mut(), uid_hex)?;
        drop(library);
        let connected = session.station.reader.connected();
        drop(session);
        runtime.lock().connected = connected;
        Ok(())
    }

    pub async fn sim_clear(&self, station: Uuid) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("remove stickers from a desk"));
        };
        let mut session = device.lock().await;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        library.clear(station, session.station.reader.sim_mut());
        Ok(())
    }

    pub async fn sim_pass(&self, station: Uuid, uid_hex: &str) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Gate(device) = &runtime.device else {
            return Err(wrong_station("walk a sticker past a gate"));
        };
        let seq = runtime.allocate_sequence()?;
        let role = runtime.config.role.unwrap_or(Role::Entry);
        let mut gate = device.lock().await;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        library.pass(
            gate.gate.sim(),
            uid_hex,
            role,
            seq,
            runtime.config.write_start_block,
        )
    }

    pub async fn sim_set_connected(
        &self,
        station: Uuid,
        connected: bool,
    ) -> Result<(), RuntimeError> {
        let runtime = self.station(station)?;
        match &runtime.device {
            StationDevice::Desk(device) => {
                let mut session = device.lock().await;
                session.station.reader.set_connected(connected);
            }
            StationDevice::Gate(device) => {
                let mut gate = device.lock().await;
                gate.gate.set_connected(connected);
            }
        }
        runtime.lock().connected = connected;
        Ok(())
    }

    pub fn stations(&self) -> &[Arc<StationRuntime>] {
        &self.stations
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    fn station(&self, id: Uuid) -> Result<&Arc<StationRuntime>, RuntimeError> {
        self.stations
            .iter()
            .find(|s| s.config.id == id)
            .ok_or_else(|| RuntimeError::new("station_not_found", "There is no such station."))
    }
}

fn wrong_station(action: &str) -> RuntimeError {
    RuntimeError::new("wrong_station", &format!("This station cannot {action}."))
}

impl StationRuntime {
    async fn build(
        paths: &AppPaths,
        config: &AppConfig,
        station: &StationConfig,
        opts: &RuntimeOptions,
        stop: &watch::Sender<bool>,
    ) -> Result<StationRuntime, RuntimeError> {
        let store = Arc::new(Mutex::new(
            Store::open(&paths.station_db(station.id)).map_err(|_| {
                RuntimeError::new(
                    "no_storage",
                    "Could not open this station's saved work. Stop and ask for help.",
                )
            })?,
        ));

        let settings: Option<HeartbeatResp> = match store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_config(SETTINGS_KEY)
            .map_err(|_| store_failure())?
        {
            Some(text) => Some(serde_json::from_str(&text).map_err(|_| {
                RuntimeError::new(
                    "settings_damaged",
                    "The saved server settings on this computer cannot be read. Open Setup and save them again.",
                )
            })?),
            None => None,
        };
        let saved_sequence = store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_config(SEQUENCE_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let now_ms = u64::try_from(Utc::now().timestamp_millis()).unwrap_or(0);
        let next_sequence = now_ms.max(saved_sequence.saturating_add(1));

        let client = ApiClient::new(
            &config.server_url,
            &config.api_key,
            &station.id.to_string(),
            opts.client_timeout,
        );
        let worker = SyncWorker::new(store.clone(), client.clone());
        let mode = settings
            .as_ref()
            .map(|s| s.event.rfid_mode)
            .unwrap_or(RfidMode::Bind);
        let uid_rule = settings
            .as_ref()
            .map(|s| s.uid_rule)
            .unwrap_or(UidRule::AsIs);

        let device = match (&station.kind, &station.device) {
            (StationKind::Desk, DeviceChoice::SimDesk) => {
                let desk = DeskStation::new(
                    DeskDevice::Sim(rfidex_core::device::sim_desk::SimDesk::new()),
                    store.clone(),
                    client.clone(),
                    mode,
                    uid_rule,
                    station.write_start_block,
                );
                StationDevice::Desk(Box::new(tokio::sync::Mutex::new(DeskSession::new(
                    desk, mode,
                ))))
            }
            (
                StationKind::Gate,
                DeviceChoice::SimGate {
                    gate_kind,
                    release_verified,
                },
            ) => {
                let role = station.role.unwrap_or(Role::Entry);
                let gate = GateStation::new(
                    GateDevice::Sim(rfidex_core::device::sim_gate::SimGate::new(
                        *gate_kind,
                        *release_verified,
                    )),
                    store.clone(),
                    &station.id.to_string(),
                    role,
                    uid_rule,
                    Duration::from_secs(station.debounce_secs),
                );
                StationDevice::Gate(Box::new(tokio::sync::Mutex::new(gate)))
            }
            _ => {
                return Err(RuntimeError::new(
                    "invalid_setup",
                    "The chosen device does not match the station type.",
                ))
            }
        };

        let connected = match &device {
            StationDevice::Desk(d) => d
                .try_lock()
                .map(|s| s.station.reader.connected())
                .unwrap_or(false),
            StationDevice::Gate(_) => true,
        };

        Ok(StationRuntime {
            config: station.clone(),
            store,
            client,
            worker,
            sync_lock: tokio::sync::Mutex::new(()),
            notify: tokio::sync::Notify::new(),
            device,
            inner: Mutex::new(StationInner {
                event_name: settings.as_ref().map(|s| s.event.name.clone()),
                settings,
                connected,
                ..StationInner::default()
            }),
            next_sequence: AtomicU64::new(next_sequence),
            stop: stop.subscribe(),
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn heartbeat_loop(station: Arc<StationRuntime>, opts: RuntimeOptions) {
    let mut stop = station.stop.clone();
    let mut ticker = tokio::time::interval(opts.heartbeat);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // `interval` fires immediately the first time, which is the immediate
        // first attempt the station needs.
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => return,
        }
        let now = Utc::now();
        tokio::select! {
            _ = station.heartbeat_once(now) => {}
            _ = stop.changed() => return,
        }
    }
}

async fn sync_loop(station: Arc<StationRuntime>) {
    let mut stop = station.stop.clone();
    let mut last_cache: Option<Instant> = None;
    let mut last_prune: Option<Instant> = None;
    loop {
        if *stop.borrow() {
            return;
        }
        if station.lock().event_ok {
            let now = Utc::now();
            let refresh = last_cache.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
            let prune = last_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600));
            {
                let _guard = station.sync_lock.lock().await;
                tokio::select! {
                    _ = async {
                        if refresh {
                            match station.worker.refresh_cache().await {
                                Ok(()) => {
                                    last_cache = Some(Instant::now());
                                    station.note_cache_ok();
                                }
                                Err(e) => station.note_cache_error(&e),
                            }
                        }
                        match station.worker.run_once(now).await {
                            Ok(report) => station.note_sync_report(&report, now),
                            Err(e) => station.note_store_error(&e),
                        }
                    } => {}
                    _ = stop.changed() => return,
                }
            }
            if prune {
                station.prune_sent();
                last_prune = Some(Instant::now());
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            // The heartbeat tells us the moment syncing is allowed to start.
            _ = station.notify.notified() => {}
            _ = stop.changed() => return,
        }
    }
}

async fn gate_loop(station: Arc<StationRuntime>, opts: RuntimeOptions) {
    let mut stop = station.stop.clone();
    let mut ticker = tokio::time::interval(opts.gate_poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => return,
        }
        let now = Utc::now();
        tokio::select! {
            _ = station.gate_tick_once(now) => {}
            _ = stop.changed() => return,
        }
    }
}

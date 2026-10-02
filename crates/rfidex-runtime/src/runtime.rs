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
use rfidex_core::contract::{HeartbeatReq, HeartbeatResp, RfidMode, Role, SearchBy, StationKind};
use rfidex_core::device::{DeviceError, GateKind, GateSource, TagReaderWriter};
use rfidex_core::station::desk::DeskStation;
use rfidex_core::station::gate::{GateError, GateStation};
use rfidex_core::store::{OutboxKind, OutboxState, Store};
use rfidex_core::sync::{SyncReport, SyncWorker, SENT_RETENTION_DAYS};
use rfidex_core::tag::UidRule;
use rfidex_core::APP_VERSION;
use rfidex_hardware::adapters::{EcrfidDesk, EcrfidGate};
use rfidex_hardware::process::{HostLauncher, StopControl};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{AppConfig, AppPaths, DeviceChoice, StationConfig};
use crate::desk::{DeskSession, DeskView};
use crate::devices::{DeskDevice, GateDevice, SimLibrary};
use crate::gate::GateView;
use crate::hardware::{self, HardwareTestAction, HardwareTestView};
use crate::problems::ProblemView;
use crate::sticker_test::{self, PendingTear, StickerTestStep, StickerTestView};
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

/// Behind an `Arc` so the enum stays small whatever a device holds, and so a
/// blocking worker can own its station's device without borrowing the runtime.
/// One allocation per station at startup.
enum StationDevice {
    Desk(Arc<tokio::sync::Mutex<DeskSession>>),
    Gate(Arc<tokio::sync::Mutex<GateStation<GateDevice>>>),
}

/// Hardware work runs on blocking workers, off the async executor: a native
/// call or a stalled socket must never occupy a Tokio worker thread. Every such
/// task is tracked, so shutdown waits for the work it started instead of
/// dropping a handle and letting it run on.
#[derive(Default)]
struct HardwareJobs {
    jobs: Mutex<Vec<JoinHandle<()>>>,
    stopped: std::sync::atomic::AtomicBool,
}

impl HardwareJobs {
    /// Register under the same lock that closes registration during shutdown.
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, RuntimeError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::SeqCst) {
                return Err(reader_stopped());
            }
            jobs.retain(|job| !job.is_finished());
            jobs.push(tokio::task::spawn_blocking(move || {
                let _ = sender.send(work());
            }));
        }
        receiver.await.map_err(|_| reader_stopped())
    }

    /// Refuse new work and wait for the work already started.
    async fn join_all(&self) {
        let running = {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            self.stopped.store(true, Ordering::SeqCst);
            std::mem::take(&mut *jobs)
        };
        for job in running {
            let _ = job.await;
        }
    }
}

/// One station's own probe, with the session lock held only for that.
async fn probe_station(runtime: &StationRuntime, action: HardwareTestAction) -> HardwareTestView {
    if runtime
        .hardware_stop
        .as_ref()
        .is_some_and(|c| c.is_stopped())
    {
        return hardware::stopped();
    }
    let view = Runtime::probe_locked(&runtime.device, action).await;
    if runtime
        .hardware_stop
        .as_ref()
        .is_some_and(|c| c.is_stopped())
    {
        return hardware::stopped();
    }
    if runtime.hardware_stop.is_some() {
        let mut inner = runtime.lock();
        inner.connected = view.ok;
        inner.connection_checked = true;
    }
    view
}

/// A core tag read back as the vendor-shaped value the operator's view uses.
fn wire_tags(tags: Vec<rfidex_core::tag::TagRead>) -> Vec<rfidex_hardware::wire::WireTag> {
    tags.into_iter()
        .map(|tag| rfidex_hardware::wire::WireTag {
            uid: <[u8; 8]>::try_from(tag.uid_raw.as_slice()).unwrap_or([0; 8]),
            dsfid: tag.dsfid.unwrap_or(0),
            antenna: tag.antenna,
        })
        .collect()
}

fn reader_stopped() -> RuntimeError {
    RuntimeError::new(
        "reader_stopped",
        "The reader stopped. Reconnect it and try again.",
    )
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
    connection_checked: bool,
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
    /// A gate's direction can change while it runs, so it lives outside
    /// `config`; every read of the role goes through `role()`.
    role: Mutex<Option<Role>>,
    store: Arc<Mutex<Store>>,
    client: ApiClient,
    worker: SyncWorker,
    /// Serialises every sync pass for this station, manual or scheduled.
    sync_lock: tokio::sync::Mutex<()>,
    /// Woken when the heartbeat confirms the event, so the first cache refresh
    /// and queue drain happen at once instead of after a poll interval.
    notify: tokio::sync::Notify,
    device: StationDevice,
    /// Present only for a real reader. Shutdown uses it to end hardware that is
    /// stuck without waiting for the call that is stuck.
    hardware_stop: Option<Arc<StopControl>>,
    /// The runtime's blocking-worker tracker, so this station's own loops put
    /// device work where the rest of the hardware work goes.
    hardware_jobs: Arc<HardwareJobs>,
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
            connection_checked: inner.connection_checked,
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

    /// The reader's own last report. A real adapter answers this from what it
    /// already knows, so the heartbeat never opens a reader or waits on a native
    /// call.
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
            let mut inner = self.lock();
            if inner.connection_checked || connected {
                inner.connected = connected;
                inner.connection_checked = true;
            }
        }
    }

    async fn apply_settings(&self, resp: &HeartbeatResp) {
        match &self.device {
            StationDevice::Desk(d) => {
                let mut session = d.lock().await;
                session.apply_settings(resp.event.rfid_mode, resp.uid_rule);
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
            role: self.role(),
            hw_model: info.as_ref().and_then(|i| i.model.clone()),
            firmware: info.as_ref().and_then(|i| i.firmware.clone()),
            app_version: APP_VERSION.to_string(),
        };
        match self.client.heartbeat(&req).await {
            Ok(resp) => {
                let skew = (resp.server_time - now).num_seconds();
                // Fail closed: if the queue cannot be counted, assume work is
                // waiting so it can never be sent to a different event.
                let pending = self.pending_count().unwrap_or(u64::MAX);
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

    /// One gate poll.
    ///
    /// The poll itself is synchronous and can wait on a reader, so it runs on a
    /// blocking worker that owns its station's device. Nothing is published
    /// until the job comes back: the stop flag is read again inside the worker,
    /// so a station that was reconfigured or removed while the poll ran cannot
    /// show its result on the new screen.
    async fn gate_tick_once(&self, now: DateTime<Utc>) {
        let StationDevice::Gate(device) = &self.device else {
            return;
        };
        if self.lock().settings.is_none() {
            // The UID rule is not known yet, so a read could not be keyed
            // correctly. Leave it in the device rather than consuming it.
            return;
        }
        let device = device.clone();
        let mut stop = self.stop.clone();
        let outcome = self
            .hardware_jobs
            .run(move || {
                if *stop.borrow_and_update() {
                    return None;
                }
                let result = {
                    let mut gate = device.blocking_lock();
                    gate.tick(now)
                };
                // The last thing checked before the result can leave the worker.
                if *stop.borrow() {
                    return None;
                }
                Some(result)
            })
            .await;
        let Ok(Some(result)) = outcome else {
            return;
        };
        match result {
            Ok(captured) => {
                {
                    let mut inner = self.lock();
                    inner.connected = true;
                    inner.connection_checked = true;
                    inner.device_error = None;
                }
                self.alarm_declined(captured).await;
            }
            Err(GateError::Device(DeviceError::Disconnected)) => {
                let mut inner = self.lock();
                inner.connected = false;
                inner.connection_checked = true;
                inner.device_error = None;
            }
            Err(GateError::Device(e)) => {
                let message = match e {
                    DeviceError::TagNotFound => "The gate could not read a sticker. Try again.",
                    DeviceError::OutOfRange => {
                        "The gate could not store what it read. Ask for help."
                    }
                    DeviceError::WriteUnsupported => "This gate cannot do that.",
                    DeviceError::Disconnected | DeviceError::Other(_) => {
                        "The gate could not finish the last action. Check it and try again."
                    }
                };
                self.lock().device_error = Some(message.to_string());
            }
            Err(GateError::Store(_)) => {
                let mut inner = self.lock();
                inner.store_error = Some(store_failure().message);
            }
        }
    }

    /// Red light and buzzer, but only for a pass the server itself says is not
    /// allowed in. A pass the local cache does not admit is looked up on the
    /// server with the verification screen's own rule (bound to a valid
    /// ticket), so a guest who verifies as a pass never alarms. If the server
    /// cannot answer within `alarm_wait_ms`, nothing sounds: the pass is still
    /// saved and shows in the panel.
    async fn alarm_declined(&self, captured: Vec<rfidex_core::station::gate::Captured>) {
        let keys: Vec<String> = captured.iter().map(|c| c.tag_key.clone()).collect();
        let declined = self.declined(&keys);
        let to_check: Vec<String> = captured
            .into_iter()
            .filter(|c| declined.contains(&c.tag_key))
            .map(|c| c.uid_raw_hex)
            .collect();
        if to_check.is_empty() {
            return;
        }
        // ponytail: one lookup per declined pass, in order; a crowd of
        // declined passes at once shares the one wait.
        let confirmed =
            tokio::time::timeout(Duration::from_millis(self.config.alarm_wait_ms), async {
                for hex in &to_check {
                    if let Ok(reply) = self.client.lookup(hex).await {
                        if crate::verify::gate_declines(&reply) {
                            return true;
                        }
                    }
                }
                false
            })
            .await
            .unwrap_or(false);
        if !confirmed {
            return;
        }
        let StationDevice::Gate(device) = &self.device else {
            return;
        };
        let device = device.clone();
        // ponytail: a failed alarm is not surfaced; the pass itself is saved.
        let _ = self
            .hardware_jobs
            .run(move || device.blocking_lock().gate.alarm())
            .await;
    }

    /// Keys the local cache does not admit. A store error admits, so a disk
    /// problem never sounds the alarm on a guest.
    fn declined(&self, keys: &[String]) -> Vec<String> {
        let store = self.store.lock().unwrap();
        keys.iter()
            .filter(|k| !rfidex_core::station::gate::admitted(&store, k).unwrap_or(true))
            .cloned()
            .collect()
    }

    fn note_store_error(&self) {
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
        if self
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prune_sent(cutoff)
            .is_err()
        {
            self.note_store_error();
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

pub(crate) fn store_failure() -> RuntimeError {
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

fn network_message(e: &ApiError) -> String {
    match e {
        ApiError::Unauthorized => UNAUTHORIZED.to_string(),
        ApiError::Retryable(why) => format!(
            "Cannot reach the server: {why}. Saved scans are safe and send by themselves when it returns."
        ),
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
    /// Start over this executable: a real reader station starts the desktop
    /// binary again in its helper mode.
    pub async fn start(
        paths: AppPaths,
        config: AppConfig,
        opts: RuntimeOptions,
    ) -> Result<Self, RuntimeError> {
        let launcher = HostLauncher::current_exe().map_err(|_| {
            RuntimeError::new(
                "no_helper",
                "This app cannot find its own program file, so a real reader cannot be started.",
            )
        })?;
        Self::start_with_launcher(paths, config, opts, launcher).await
    }

    /// The same, with the helper program chosen by the caller. Tests point this
    /// at the developer host.
    pub async fn start_with_launcher(
        paths: AppPaths,
        config: AppConfig,
        opts: RuntimeOptions,
        launcher: HostLauncher,
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
        let hardware_jobs = Arc::new(HardwareJobs::default());
        let mut stations = Vec::with_capacity(config.stations.len());
        for station_config in &config.stations {
            stations.push(Arc::new(
                StationRuntime::build(
                    &paths,
                    &config,
                    station_config,
                    &opts,
                    &stop,
                    &launcher,
                    hardware_jobs.clone(),
                )
                .await?,
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
            config: Mutex::new(config),
            library,
            stations,
            stop,
            tasks: Mutex::new(tasks),
            hardware_jobs,
            launcher,
            tear: Mutex::new(None),
        })
    }

    /// Stop signal, hardware cancellation, then the join.
    ///
    /// Every real reader is cancelled **before** anything is joined: a native
    /// call or a socket read that is stuck is exactly what would otherwise hold
    /// the join open. Cancelling never takes the lock the stuck call holds, so
    /// this works even while a station's session is busy.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        let _ = self.stop.send(true);
        self.cancel_hardware();
        // The work already started comes back as soon as its reader is
        // cancelled, so waiting for it is bounded by the cancellation rather
        // than by the reader's own deadline.
        self.hardware_jobs.join_all().await;
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
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
            return Err(e);
        }
        Ok(())
    }

    /// Run one of the operator's reader tests on a saved station.
    ///
    /// It goes through the station's own adapter, so it cannot open a second
    /// handle to a reader that is already in use. The work is synchronous and
    /// can wait on a reader, so it runs on a blocking worker like every other
    /// hardware call.
    pub async fn hardware_test(
        &self,
        station: Uuid,
        action: HardwareTestAction,
    ) -> Result<HardwareTestView, RuntimeError> {
        let runtime = self.station(station)?.clone();
        let handle = tokio::runtime::Handle::current();
        self.hardware_jobs
            .run(move || handle.block_on(async move { probe_station(&runtime, action).await }))
            .await
            .map_err(|_| hardware::no_helper())
    }

    /// One step of the disposable-sticker write test on a real SDK desk.
    ///
    /// The desk's own session is held for the whole step and its reader is let
    /// go, so the one-off test helper is the only thing talking to the
    /// reader. The desk reconnects afterwards with writing still exactly as its
    /// saved profile says.
    // ponytail: the test helper is not registered with shutdown's stop control;
    // a step caught by shutdown finishes within the reader's own timeout per call.
    pub async fn sticker_test(
        &self,
        station: Uuid,
        step: StickerTestStep,
    ) -> Result<StickerTestView, RuntimeError> {
        let runtime = self.station(station)?.clone();
        let hardware = sdk_desk_hardware(&runtime.config)?;
        let write_enabled = hardware.write_verified();
        let root = self.paths.root().to_path_buf();
        let pending = self
            .tear
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|p| p.station == station);

        let outcome = match step {
            StickerTestStep::Status => sticker_test::status_outcome(),
            StickerTestStep::TearCheck if pending.is_none() => {
                sticker_test::refused("Press Start tear first, then put the sticker back.")
            }
            _ => {
                let launcher = self.launcher.clone();
                let start_block = runtime.config.write_start_block;
                let desk = runtime.clone();
                let waiting = pending.clone();
                let handle = tokio::runtime::Handle::current();
                let (outcome, next) = self
                    .hardware_jobs
                    .run(move || {
                        handle.block_on(async move {
                            let StationDevice::Desk(device) = &desk.device else {
                                return (sticker_test::refused(sticker_test::NOT_A_SDK_DESK), None);
                            };
                            let mut session = device.lock().await;
                            session.station.reader.release();
                            // The desktop helper refuses commissioning mode, so
                            // the test runs an ordinary helper whose own copy of
                            // the profile allows writes. The desk's session keeps
                            // the saved profile, so it still cannot write.
                            let mut test_profile = hardware.clone();
                            test_profile.set_write_verified(true);
                            let result = match rfidex_hardware::HardwareClient::start(
                                &launcher,
                                &test_profile,
                            ) {
                                Err(_) => (sticker_test::refused(TEST_READER_BUSY), None),
                                Ok(mut client) => {
                                    let result = match (step, waiting) {
                                        (StickerTestStep::TearWrite, _) => {
                                            sticker_test::tear_write(
                                                &mut client,
                                                station,
                                                start_block,
                                            )
                                        }
                                        (StickerTestStep::TearCheck, Some(p)) => {
                                            (sticker_test::tear_check(&mut client, &p), None)
                                        }
                                        _ => (
                                            sticker_test::check(&mut client, station, start_block),
                                            None,
                                        ),
                                    };
                                    if client.is_alive() {
                                        let _ = client.call(rfidex_hardware::Operation::Close);
                                    }
                                    result
                                }
                            };
                            // Give the desk its reader back now rather than on the
                            // next scan, so the status bar stays truthful.
                            let _ = session.station.reader.probe();
                            result
                        })
                    })
                    .await?;
                runtime.refresh_desk_connection().await;
                let mut tear = self.tear.lock().unwrap_or_else(|e| e.into_inner());
                match step {
                    StickerTestStep::TearWrite if next.is_some() => *tear = next,
                    // A wrong sticker leaves the trial open; anything else ends it.
                    StickerTestStep::TearCheck if outcome.ok || outcome.record.is_some() => {
                        *tear = None
                    }
                    _ => {}
                }
                outcome
            }
        };
        if let Some(record) = &outcome.record {
            sticker_test::append(&root, record).map_err(|_| {
                RuntimeError::new(
                    "save_failed",
                    "The test ran, but its result could not be saved. Check the disk and run it again.",
                )
            })?;
        }
        let tear_waiting = self
            .tear
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|p| p.station == station);
        Ok(sticker_test::view(
            outcome,
            &sticker_test::load(&root, station),
            tear_waiting,
            write_enabled,
        ))
    }

    /// The saved setup with writing turned on or off for one desk. Turning it
    /// on needs one passed sticker and no failure; turning it off always works.
    /// The caller saves it and restarts the stations, as Setup does.
    pub fn with_writing(&self, station: Uuid, on: bool) -> Result<AppConfig, RuntimeError> {
        sdk_desk_hardware(&self.station(station)?.config)?;
        if on {
            let tally = sticker_test::tally(&sticker_test::load(self.paths.root(), station));
            if !sticker_test::can_enable(&tally) {
                return Err(RuntimeError::new(
                    "test_not_passed",
                    "Writing can be turned on after at least one sticker passes the test with no failures.",
                ));
            }
        }
        let mut config = self.config();
        for s in config.stations.iter_mut().filter(|s| s.id == station) {
            if let DeviceChoice::EcrfidDesk { hardware } = &mut s.device {
                hardware.set_write_verified(on);
            }
        }
        Ok(config)
    }

    /// What a probe may touch: one station's own session, and nothing else.
    async fn probe_locked(device: &StationDevice, action: HardwareTestAction) -> HardwareTestView {
        match action {
            HardwareTestAction::Connect => match device {
                StationDevice::Desk(d) => {
                    let mut session = d.lock().await;
                    match session.station.reader.probe() {
                        Ok(info) => {
                            let simulator = session.station.reader.is_simulated();
                            hardware::connected(&info, simulator)
                        }
                        Err(e) => hardware::from_error(&e),
                    }
                }
                StationDevice::Gate(g) => {
                    let mut gate = g.lock().await;
                    match gate.gate.probe() {
                        Ok(info) => {
                            let simulator = gate.gate.is_simulated();
                            hardware::connected(&info, simulator)
                        }
                        Err(e) => hardware::from_error(&e),
                    }
                }
            },
            HardwareTestAction::ReadTags => match device {
                StationDevice::Desk(d) => {
                    let mut session = d.lock().await;
                    match session.station.reader.inventory() {
                        Ok(tags) => hardware::tags(&wire_tags(tags)),
                        Err(e) => hardware::from_error(&e),
                    }
                }
                StationDevice::Gate(g) => {
                    let mut gate = g.lock().await;
                    match gate.gate.poll() {
                        Ok(found) => {
                            let tags: Vec<rfidex_hardware::wire::WireTag> = found
                                .into_iter()
                                .map(|(read, _handle)| rfidex_hardware::wire::WireTag {
                                    uid: <[u8; 8]>::try_from(read.tag.uid_raw.as_slice())
                                        .unwrap_or_default(),
                                    dsfid: read.tag.dsfid.unwrap_or(0),
                                    antenna: read.tag.antenna,
                                })
                                .collect();
                            hardware::tags(&tags)
                        }
                        Err(e) => hardware::from_error(&e),
                    }
                }
            },
            HardwareTestAction::GateRecords => match device {
                StationDevice::Desk(_) => hardware::failed("Gate records are only for gates."),
                StationDevice::Gate(g) => {
                    let mut gate = g.lock().await;
                    match gate.gate.library_records() {
                        Ok(raw) => hardware::records(&raw),
                        Err(e) => hardware::from_error(&e),
                    }
                }
            },
        }
    }

    /// End every real reader this runtime owns.
    fn cancel_hardware(&self) {
        for station in &self.stations {
            if let Some(control) = &station.hardware_stop {
                control.stop();
            }
        }
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
                Err(_) => {
                    station.note_store_error();
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

    /// Link the sticker the operator just tapped.
    ///
    /// The reader half of this runs synchronously and can block for as long as
    /// the reader's own deadline, so it is moved off the async workers entirely:
    /// the existing linking workflow runs on a blocking worker with the Tokio
    /// handle, which keeps its confirmation, payload and print semantics
    /// unchanged while a stalled reader stays unable to occupy a worker thread.
    pub async fn desk_link(
        &self,
        station: Uuid,
        reason: Option<String>,
    ) -> Result<DeskView, RuntimeError> {
        let runtime = self.station(station)?.clone();
        let refresh = runtime.clone();
        let handle = tokio::runtime::Handle::current();
        let view = self
            .hardware_jobs
            .run(move || {
                handle.block_on(async move {
                    let StationDevice::Desk(device) = &runtime.device else {
                        return Err(wrong_station("link stickers"));
                    };
                    let mut session = device.lock().await;
                    if runtime.settings().is_none() {
                        return Ok(session.connect_first());
                    }
                    Ok(crate::desk::link(&mut session, &runtime.store, reason).await)
                })
            })
            .await;
        refresh.refresh_desk_connection().await;
        view?
    }

    pub async fn desk_reset(&self, station: Uuid) -> Result<DeskView, RuntimeError> {
        let (_, mut session) = self.desk_session(station).await?;
        Ok(crate::desk::reset(&mut session))
    }

    /// Find a guest by name, email or phone. A question, never an action: no
    /// check-in, no print, no queue row, and no lock held over the request.
    pub async fn desk_search(
        &self,
        station: Uuid,
        by: SearchBy,
        query: &str,
    ) -> Result<crate::search::SearchView, RuntimeError> {
        let runtime = self.station(station)?;
        if runtime.kind() != StationKind::Desk {
            return Err(wrong_station("search for a ticket"));
        }
        // Rows belong to another event: searching would show this desk the
        // wrong guests, so the cache is not used either.
        if runtime.event_mismatch() {
            return Err(RuntimeError::new("different_event", DIFFERENT_EVENT));
        }
        // Only a heartbeat that failed on the network skips the server; a
        // rejected key or a station not heard from yet still asks it.
        let known_offline = runtime.lock().network_error.is_some();
        crate::search::desk_search(&runtime.client, &runtime.store, !known_offline, by, query).await
    }

    /// One look at the desk reader for the Verify screen: whose sticker is
    /// this? Asked again and again while the screen is open, so it never holds
    /// a lock over the server and asks the server once per tap.
    pub async fn desk_verify(
        &self,
        station: Uuid,
    ) -> Result<crate::verify::VerifyView, RuntimeError> {
        use crate::verify::{answer, reader_problem, server_problem};
        let runtime = self.station(station)?.clone();
        let refresh = runtime.clone();
        let handle = tokio::runtime::Handle::current();
        let read = self
            .hardware_jobs
            .run(move || {
                handle.block_on(async move {
                    let StationDevice::Desk(device) = &runtime.device else {
                        return Err(wrong_station("verify stickers"));
                    };
                    let mut session = device.lock().await;
                    let read = session.station.detect_tag();
                    // A sticker that is gone forgets its answer, so the next
                    // tap of the same sticker asks the server afresh.
                    if read.is_err() {
                        session.verified = None;
                    }
                    let cached = match &read {
                        Ok(tag) => session.verified.clone().filter(|v| {
                            v.sticker.as_deref() == Some(&rfidex_core::tag::hex_upper(&tag.uid_raw))
                        }),
                        Err(_) => None,
                    };
                    Ok((read, cached))
                })
            })
            .await;
        refresh.refresh_desk_connection().await;
        let (read, cached) = read??;
        let tag = match read {
            Ok(tag) => tag,
            Err(e) => return Ok(reader_problem(&e)),
        };
        if let Some(cached) = cached {
            return Ok(cached);
        }
        let sticker = rfidex_core::tag::hex_upper(&tag.uid_raw);
        let view = match refresh.client.lookup(&sticker).await {
            Ok(reply) => answer(&sticker, reply),
            Err(e) => server_problem(&e),
        };
        if view.is_final() {
            if let StationDevice::Desk(device) = &refresh.device {
                device.lock().await.verified = Some(view.clone());
            }
        }
        Ok(view)
    }

    /// Print the badge for the guest on screen. The same call serves the first
    /// print and every Reprint: Rust never prints on its own.
    ///
    /// The desk lock is held only long enough to check the session and copy
    /// what is needed; the HTTP call happens with no lock held, so a print can
    /// never delay the sticker.
    pub async fn desk_print(
        &self,
        station: Uuid,
        session_id: Uuid,
    ) -> Result<crate::BadgeView, RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("print a badge"));
        };
        let printer_url = runtime.config.printer_url.clone();

        let prepared = {
            let mut session = device.lock().await;
            if session.session_id != session_id {
                return Err(RuntimeError::new(
                    "stale_session",
                    "That badge belongs to a guest who is no longer on screen. Scan the ticket again.",
                ));
            }
            let Some(ticket) = session.ticket.clone() else {
                return Err(RuntimeError::new(
                    "scan_first",
                    crate::desk::SCAN_FIRST_MESSAGE,
                ));
            };
            if !runtime.online() {
                // There is no printer when this PC cannot reach the event
                // server: event-printing looks the ticket up there.
                session.set_badge(crate::BadgeView {
                    print_now: false,
                    can_reprint: true,
                    hold: true,
                    message: Some(crate::desk::OFFLINE_PRINT_MESSAGE.to_string()),
                });
                return Ok(session.badge.clone());
            }
            if session.printing.swap(true, Ordering::SeqCst) {
                // One print in flight per desk: this press joins it.
                return Ok(session.badge.clone());
            }
            session.set_badge(crate::BadgeView {
                print_now: false,
                can_reprint: false,
                hold: true,
                message: Some(crate::desk::PRINTING_MESSAGE.to_string()),
            });
            (ticket.public_id, session.printing.clone())
        };

        let outcome = match crate::PrinterClient::new(&printer_url) {
            Ok(client) => client.reprint(prepared.0).await,
            Err(e) => Err(e),
        };

        let mut session = device.lock().await;
        prepared.1.store(false, Ordering::SeqCst);
        if session.session_id != session_id {
            // A newer scan owns the screen: the job went out for the guest who
            // was on it, and its result must not land on anyone else.
            return Ok(session.badge.clone());
        }
        session.set_badge(match outcome {
            // "Accepted" is not "paper came out": keep the guest and Reprint
            // on screen until the next scan replaces them.
            Ok(()) => crate::BadgeView {
                print_now: false,
                can_reprint: true,
                hold: true,
                message: Some(crate::desk::PRINTED_MESSAGE.to_string()),
            },
            Err(_) => crate::BadgeView {
                print_now: false,
                can_reprint: true,
                hold: true,
                message: Some(crate::desk::PRINT_FAILED_MESSAGE.to_string()),
            },
        });
        Ok(session.badge.clone())
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

    fn station(&self, id: Uuid) -> Result<&Arc<StationRuntime>, RuntimeError> {
        self.stations
            .iter()
            .find(|s| s.config.id == id)
            .ok_or_else(|| RuntimeError::new("station_not_found", "There is no such station."))
    }
}

const TEST_READER_BUSY: &str = "The test could not open the reader. Check that it is connected and no other program is using it.";

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

fn sdk_desk_hardware(
    station: &StationConfig,
) -> Result<rfidex_hardware::HardwareConfig, RuntimeError> {
    match &station.device {
        DeviceChoice::EcrfidDesk { hardware } if hardware.is_sdk() => Ok(hardware.clone()),
        _ => Err(RuntimeError::new(
            "not_sdk_desk",
            sticker_test::NOT_A_SDK_DESK,
        )),
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
        launcher: &HostLauncher,
        hardware_jobs: Arc<HardwareJobs>,
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
                    "A station's saved server settings cannot be read, so the stations cannot start. Ask for help, and do not delete the station data: it may hold work that has not been sent.",
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

        let (device, hardware_stop) = match (&station.kind, &station.device) {
            (StationKind::Desk, DeviceChoice::SimDesk) => {
                let desk = DeskStation::new(
                    DeskDevice::Sim(rfidex_core::device::sim_desk::SimDesk::new()),
                    store.clone(),
                    client.clone(),
                    mode,
                    uid_rule,
                    station.write_start_block,
                );
                (
                    StationDevice::Desk(Arc::new(tokio::sync::Mutex::new(DeskSession::new(
                        desk, mode, uid_rule,
                    )))),
                    None,
                )
            }
            (StationKind::Desk, DeviceChoice::EcrfidDesk { hardware }) => {
                let reader = EcrfidDesk::new(hardware.clone(), launcher.clone())
                    .map_err(|_| reader_failed())?;
                // Taken before the session owns the reader: the control has to
                // be usable while the stanza's own lock is held by a call.
                let control = reader.stop_control();
                let desk = DeskStation::new(
                    DeskDevice::Ecrfid(Box::new(reader)),
                    store.clone(),
                    client.clone(),
                    mode,
                    uid_rule,
                    station.write_start_block,
                );
                (
                    StationDevice::Desk(Arc::new(tokio::sync::Mutex::new(DeskSession::new(
                        desk, mode, uid_rule,
                    )))),
                    Some(control),
                )
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
                (
                    StationDevice::Gate(Arc::new(tokio::sync::Mutex::new(gate))),
                    None,
                )
            }
            (StationKind::Gate, DeviceChoice::EcrfidGate { hardware }) => {
                let role = station.role.unwrap_or(Role::Entry);
                let mut reader = EcrfidGate::new(hardware.clone(), launcher.clone())
                    .map_err(|_| reader_failed())?;
                reader.set_alarm_all_panels(station.alarm_all_panels);
                let control = reader.stop_control();
                let gate = GateStation::new(
                    GateDevice::Ecrfid(Box::new(reader)),
                    store.clone(),
                    &station.id.to_string(),
                    role,
                    uid_rule,
                    Duration::from_secs(station.debounce_secs),
                );
                (
                    StationDevice::Gate(Arc::new(tokio::sync::Mutex::new(gate))),
                    Some(control),
                )
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
            StationDevice::Gate(g) => g
                .try_lock()
                .map(|gate| matches!(&gate.gate, GateDevice::Sim(sim) if sim.connected))
                .unwrap_or(false),
        };
        let connection_checked = hardware_stop.is_none();

        Ok(StationRuntime {
            config: station.clone(),
            role: Mutex::new(station.role),
            store,
            client,
            worker,
            sync_lock: tokio::sync::Mutex::new(()),
            notify: tokio::sync::Notify::new(),
            device,
            hardware_stop,
            hardware_jobs,
            inner: Mutex::new(StationInner {
                event_name: settings.as_ref().map(|s| s.event.name.clone()),
                settings,
                connected,
                connection_checked,
                ..StationInner::default()
            }),
            next_sequence: AtomicU64::new(next_sequence),
            stop: stop.subscribe(),
        })
    }
}

fn reader_failed() -> RuntimeError {
    RuntimeError::new(
        "reader_config",
        "This station's reader settings cannot be used. Open Setup and check the reader.",
    )
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.cancel_hardware();
        // A blocking worker cannot be aborted, so the only thing that can end
        // one is its own reader being cancelled above.
        self.hardware_jobs.stopped.store(true, Ordering::SeqCst);
        for task in self.tasks.lock().unwrap_or_else(|e| e.into_inner()).iter() {
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
                            Err(_) => station.note_store_error(),
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

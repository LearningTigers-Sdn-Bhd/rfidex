//! The thin command layer. Every decision — what a failure means, what the
//! operator should read, whether a save is safe — belongs to `rfidex-runtime`.
//! Commands clone the running runtime before waiting on a reader; setup and
//! shutdown can then cancel stalled work without waiting for the state lock.

use rfidex_core::contract::SearchBy;
use rfidex_core::store::{OutboxState, Store};
use rfidex_runtime::config::{AppConfig, AppPaths, ConfigError, SetupInput, SetupView};
use rfidex_runtime::{
    test_connection, test_printer, AppStatus, BadgeView, ConnectionView, DeskView, GateView,
    HardwareTestAction, HardwareTestView, ProblemView, Runtime, RuntimeError, RuntimeOptions,
    SearchView, StickerTestStep, StickerTestView,
};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct AppState {
    pub paths: AppPaths,
    /// `None` until the first `app_state` call starts the stations, and `None`
    /// again after a save that could not start them.
    pub runtime: RwLock<Option<Arc<Runtime>>>,
}

#[derive(Serialize)]
pub struct AppView {
    pub configured: bool,
    pub status: Option<AppStatus>,
}

fn config_error(e: ConfigError) -> RuntimeError {
    match e {
        ConfigError::Invalid(message) => RuntimeError::new("invalid_setup", &message),
        // Never echo the file, the path or a parser error: it can contain the
        // API key and the station list.
        _ => RuntimeError::new(
            "config_unreadable",
            "The saved setup on this computer cannot be read, so the stations cannot start. Ask for help: config.json in the RfiDex data folder must be repaired or replaced. Station data is kept.",
        ),
    }
}

fn not_configured() -> RuntimeError {
    RuntimeError::new("not_configured", "Open Setup before using a station.")
}

fn store_failed() -> RuntimeError {
    RuntimeError::new(
        "save_failed",
        "Could not read this station's saved work. Stop and ask for help.",
    )
}

/// The running runtime, or the one fixed refusal every operational command
/// gives before Setup has been completed.
async fn current(state: &AppState) -> Result<Arc<Runtime>, RuntimeError> {
    state
        .runtime
        .read()
        .await
        .clone()
        .ok_or_else(not_configured)
}

async fn try_restore(state: &AppState, slot: &mut Option<Arc<Runtime>>) {
    match state.paths.load() {
        Ok(Some(config)) => {
            match Runtime::start(state.paths.clone(), config, RuntimeOptions::default()).await {
                Ok(runtime) => *slot = Some(Arc::new(runtime)),
                Err(_) => *slot = None,
            }
        }
        _ => *slot = None,
    }
}

/// Stations the candidate drops while they still have work waiting. Their
/// databases are kept either way; this only refuses to lose sight of the work.
fn removed_station_with_work(
    paths: &AppPaths,
    old: &AppConfig,
    candidate: &AppConfig,
) -> Result<Option<String>, RuntimeError> {
    for station in &old.stations {
        if candidate.stations.iter().any(|s| s.id == station.id) {
            continue;
        }
        let store = Store::open(&paths.station_db(station.id)).map_err(|_| store_failed())?;
        let waiting = store
            .count(OutboxState::Pending)
            .map_err(|_| store_failed())?
            + store
                .count(OutboxState::Conflict)
                .map_err(|_| store_failed())?
            + store
                .count(OutboxState::Parked)
                .map_err(|_| store_failed())?;
        if waiting > 0 {
            return Ok(Some(station.name.clone()));
        }
    }
    Ok(None)
}

#[tauri::command]
pub async fn app_state(state: tauri::State<'_, AppState>) -> Result<AppView, RuntimeError> {
    // Only the first call may start the stations, and it must not race another
    // one, so initialisation happens under the write lock.
    let mut slot = state.runtime.write().await;
    if let Some(runtime) = slot.as_ref() {
        return Ok(AppView {
            configured: true,
            status: Some(runtime.status().await?),
        });
    }
    let config = match state.paths.load().map_err(config_error)? {
        Some(config) => config,
        None => {
            return Ok(AppView {
                configured: false,
                status: None,
            })
        }
    };
    let runtime = Runtime::start(state.paths.clone(), config, RuntimeOptions::default()).await?;
    let status = runtime.status().await?;
    *slot = Some(Arc::new(runtime));
    Ok(AppView {
        configured: true,
        status: Some(status),
    })
}

#[tauri::command]
pub async fn setup_get(
    state: tauri::State<'_, AppState>,
) -> Result<Option<SetupView>, RuntimeError> {
    {
        let slot = state.runtime.read().await;
        if let Some(runtime) = slot.as_ref() {
            return Ok(Some(runtime.config().setup_view()));
        }
    }
    match state.paths.load().map_err(config_error)? {
        Some(config) => Ok(Some(config.setup_view())),
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn setup_test(
    state: tauri::State<'_, AppState>,
    url: String,
    key: String,
) -> Result<ConnectionView, RuntimeError> {
    let key = if key.trim().is_empty() {
        let saved = match state.runtime.read().await.as_ref() {
            Some(runtime) => Some(runtime.config().api_key.clone()),
            None => state.paths.load().map_err(config_error)?.map(|c| c.api_key),
        };
        match saved {
            Some(saved) => saved,
            None => {
                return Err(RuntimeError::new(
                    "invalid_setup",
                    "Enter the API key from the event.",
                ))
            }
        }
    } else {
        key
    };
    Ok(test_connection(&url, &key).await)
}

#[tauri::command]
pub async fn setup_test_printer(url: String) -> Result<ConnectionView, RuntimeError> {
    // An address that has not been saved yet, and no key at all: the printer
    // app never sees the event API key.
    Ok(test_printer(&url).await)
}

#[tauri::command]
pub async fn setup_save(
    state: tauri::State<'_, AppState>,
    input: serde_json::Value,
) -> Result<AppView, RuntimeError> {
    // Parsed here rather than by Tauri, so a field in the wrong shape (an empty
    // reader address, a cleared number) gets a sentence instead of a generic
    // failure. The parser's own text is never shown: it can echo input.
    let input: SetupInput = serde_json::from_value(input).map_err(|_| {
        RuntimeError::new(
            "invalid_setup",
            "A reader setting is not in the right format. Enter reader addresses as IP:port, for example 192.168.1.20:6688, and fill in every number field.",
        )
    })?;
    let mut slot = state.runtime.write().await;
    let old = match slot.as_ref() {
        Some(runtime) => Some(runtime.config().clone()),
        None => state.paths.load().map_err(config_error)?,
    };
    // Validate the whole candidate before anything is stopped or written.
    let candidate = AppConfig::from_input(old.as_ref(), input).map_err(config_error)?;
    if let Some(old) = &old {
        if let Some(name) = removed_station_with_work(&state.paths, old, &candidate)? {
            return Err(RuntimeError::new(
                "station_has_work",
                &format!(
                    "Station {name} still has work waiting to send. Let it finish, or keep the station."
                ),
            ));
        }
    }

    restart_with(&state, &mut slot, candidate).await
}

/// Stop the stations, start them on `candidate`, and save it only once they
/// run. Any failure brings the saved setup back.
async fn restart_with(
    state: &AppState,
    slot: &mut Option<Arc<Runtime>>,
    candidate: AppConfig,
) -> Result<AppView, RuntimeError> {
    if let Some(running) = slot.take() {
        if let Err(error) = running.shutdown().await {
            try_restore(state, slot).await;
            return Err(error);
        }
    }
    match Runtime::start(
        state.paths.clone(),
        candidate.clone(),
        RuntimeOptions::default(),
    )
    .await
    {
        Ok(runtime) => match state.paths.save(&candidate) {
            Ok(()) => {
                let status = runtime.status().await?;
                *slot = Some(Arc::new(runtime));
                Ok(AppView {
                    configured: true,
                    status: Some(status),
                })
            }
            Err(e) => {
                // Nothing was saved, so the old setup is still the truth.
                drop(runtime);
                let error = config_error(e);
                try_restore(state, slot).await;
                Err(error)
            }
        },
        Err(e) => {
            try_restore(state, slot).await;
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn status(state: tauri::State<'_, AppState>) -> Result<AppStatus, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).status().await
}

#[tauri::command]
pub async fn desk_scan(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    code: String,
) -> Result<DeskView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).desk_scan(station, &code).await
}

#[tauri::command]
pub async fn desk_link(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    reason: Option<String>,
) -> Result<DeskView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).desk_link(station, reason).await
}

#[tauri::command]
pub async fn desk_reset(
    state: tauri::State<'_, AppState>,
    station: Uuid,
) -> Result<DeskView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).desk_reset(station).await
}

#[tauri::command]
pub async fn desk_search(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    by: SearchBy,
    query: String,
) -> Result<SearchView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).desk_search(station, by, &query).await
}

#[tauri::command]
pub async fn desk_print(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    session_id: Uuid,
) -> Result<BadgeView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).desk_print(station, session_id).await
}

#[tauri::command]
pub async fn gate_recent(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    limit: usize,
) -> Result<Vec<GateView>, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).gate_recent(station, limit).await
}

#[tauri::command]
pub async fn problems(state: tauri::State<'_, AppState>) -> Result<Vec<ProblemView>, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).problems().await
}

#[tauri::command]
pub async fn dismiss_problem(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    id: i64,
) -> Result<bool, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).dismiss(station, id).await
}

#[tauri::command]
pub async fn sync_now(state: tauri::State<'_, AppState>) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).sync_now().await
}

#[tauri::command]
pub async fn export_diagnostics(state: tauri::State<'_, AppState>) -> Result<String, RuntimeError> {
    let slot = current(&state).await?;
    let path = runtime(&slot).export_diagnostics().await?;
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn sim_place(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    uid_hex: String,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).sim_place(station, &uid_hex).await
}

#[tauri::command]
pub async fn sim_clear(
    state: tauri::State<'_, AppState>,
    station: Uuid,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).sim_clear(station).await
}

#[tauri::command]
pub async fn sim_pass(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    uid_hex: String,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).sim_pass(station, &uid_hex).await
}

#[tauri::command]
pub async fn sim_set_connected(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    connected: bool,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).sim_set_connected(station, connected).await
}

/// The reader list the vendor library can see.
///
/// A bounded helper of its own, started and reaped inside the runtime's
/// hardware module: no station is disturbed, and no device is opened.
#[tauri::command]
pub async fn hardware_enumerate(
    dll_path: String,
    kind: rfidex_hardware::sdk::EnumerationKind,
) -> Result<Vec<String>, RuntimeError> {
    let launcher = rfidex_hardware::process::HostLauncher::current_exe()
        .map_err(|_| rfidex_runtime::hardware::no_helper())?;
    rfidex_runtime::hardware::enumerate(&launcher, std::path::Path::new(&dll_path), kind)
}

/// Gates that answer a network broadcast on one PC interface.
#[tauri::command]
pub async fn hardware_discover(
    dll_path: String,
    iface: String,
) -> Result<Vec<String>, RuntimeError> {
    let launcher = rfidex_hardware::process::HostLauncher::current_exe()
        .map_err(|_| rfidex_runtime::hardware::no_helper())?;
    rfidex_runtime::hardware::discover(&launcher, std::path::Path::new(&dll_path), &iface)
}

/// Test a saved station's own reader. Connect and ReadTags only: there is no
/// destructive test in the ordinary interface.
#[tauri::command]
pub async fn hardware_test(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    action: HardwareTestAction,
) -> Result<HardwareTestView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).hardware_test(station, action).await
}

/// One step of the disposable-sticker write test on a real desk reader.
#[tauri::command]
pub async fn sticker_test(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    step: StickerTestStep,
) -> Result<StickerTestView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot).sticker_test(station, step).await
}

/// Turn sticker writing on or off for one desk, then restart the stations on
/// the saved change, exactly as a Setup save does.
#[tauri::command]
pub async fn sticker_writing(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    on: bool,
) -> Result<AppView, RuntimeError> {
    let mut slot = state.runtime.write().await;
    let candidate = slot
        .as_ref()
        .ok_or_else(not_configured)?
        .with_writing(station, on)?;
    restart_with(&state, &mut slot, candidate).await
}

fn runtime(slot: &Arc<Runtime>) -> &Runtime {
    slot.as_ref()
}

/// Cancel active work without waiting on the state lock it used to own.
pub async fn shutdown_runtime(state: &AppState) {
    let running = state.runtime.write().await.take();
    if let Some(running) = running {
        let _ = running.shutdown().await;
    }
}

#[derive(Serialize)]
pub struct UpdateView {
    pub current: String,
    /// The newer version on the release page, or `None` when this is the latest.
    pub available: Option<String>,
    pub notes: Option<String>,
}

fn update_failed() -> RuntimeError {
    RuntimeError::new(
        "update_unavailable",
        "Could not reach the update server. Check the internet connection and try again. RfiDex keeps working as it is.",
    )
}

async fn find_update(
    app: &tauri::AppHandle,
) -> Result<Option<tauri_plugin_updater::Update>, RuntimeError> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|_| update_failed())?;
    updater.check().await.map_err(|_| update_failed())
}

#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> Result<UpdateView, RuntimeError> {
    let update = find_update(&app).await?;
    Ok(UpdateView {
        current: app.package_info().version.to_string(),
        available: update.as_ref().map(|u| u.version.clone()),
        notes: update.and_then(|u| u.body),
    })
}

/// Stops the stations, then hands over to the installer, which closes RfiDex
/// and reopens it on the new version. Saved setup and waiting work live in the
/// data folder, which the installer never touches.
#[tauri::command]
pub async fn update_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), RuntimeError> {
    let Some(update) = find_update(&app).await? else {
        return Err(RuntimeError::new(
            "up_to_date",
            "RfiDex is already on the latest version.",
        ));
    };
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|_| update_failed())?;
    shutdown_runtime(&state).await;
    if update.install(bytes).is_err() {
        // The stations must come back if the installer never started.
        try_restore(&state, &mut *state.runtime.write().await).await;
        return Err(RuntimeError::new(
            "update_failed",
            "The update was downloaded but could not start. RfiDex keeps working as it is; try again or install the new version from the release page.",
        ));
    }
    Ok(())
}

//! The thin command layer. Every decision — what a failure means, what the
//! operator should read, whether a save is safe — belongs to `rfidex-runtime`.
//! These functions hold the app state lock and forward arguments.

use rfidex_core::store::{OutboxState, Store};
use rfidex_runtime::config::{AppConfig, AppPaths, ConfigError, SetupInput, SetupView};
use rfidex_runtime::{
    test_connection, AppStatus, ConnectionView, DeskView, GateView, ProblemView, Runtime,
    RuntimeError, RuntimeOptions,
};
use serde::Serialize;
use tokio::sync::{RwLock, RwLockReadGuard};
use uuid::Uuid;

pub struct AppState {
    pub paths: AppPaths,
    /// `None` until the first `app_state` call starts the stations, and `None`
    /// again after a save that could not start them.
    pub runtime: RwLock<Option<Runtime>>,
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
            "The saved setup on this computer cannot be read. Open Setup and save it again.",
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
async fn current(state: &AppState) -> Result<RwLockReadGuard<'_, Option<Runtime>>, RuntimeError> {
    let guard = state.runtime.read().await;
    if guard.is_none() {
        return Err(not_configured());
    }
    Ok(guard)
}

async fn try_restore(state: &AppState, slot: &mut Option<Runtime>) {
    match state.paths.load() {
        Ok(Some(config)) => {
            match Runtime::start(state.paths.clone(), config, RuntimeOptions::default()).await {
                Ok(runtime) => *slot = Some(runtime),
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
    *slot = Some(runtime);
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
pub async fn setup_save(
    state: tauri::State<'_, AppState>,
    input: SetupInput,
) -> Result<AppView, RuntimeError> {
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

    if let Some(mut running) = slot.take() {
        let _ = running.shutdown().await;
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
                *slot = Some(runtime);
                Ok(AppView {
                    configured: true,
                    status: Some(status),
                })
            }
            Err(e) => {
                // Nothing was saved, so the old setup is still the truth.
                drop(runtime);
                let error = config_error(e);
                try_restore(&state, &mut slot).await;
                Err(error)
            }
        },
        Err(e) => {
            try_restore(&state, &mut slot).await;
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn status(state: tauri::State<'_, AppState>) -> Result<AppStatus, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.status().await
}

#[tauri::command]
pub async fn desk_scan(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    code: String,
) -> Result<DeskView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.desk_scan(station, &code).await
}

#[tauri::command]
pub async fn desk_link(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    reason: Option<String>,
) -> Result<DeskView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.desk_link(station, reason).await
}

#[tauri::command]
pub async fn desk_reset(
    state: tauri::State<'_, AppState>,
    station: Uuid,
) -> Result<DeskView, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.desk_reset(station).await
}

#[tauri::command]
pub async fn gate_recent(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    limit: usize,
) -> Result<Vec<GateView>, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.gate_recent(station, limit).await
}

#[tauri::command]
pub async fn problems(state: tauri::State<'_, AppState>) -> Result<Vec<ProblemView>, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.problems().await
}

#[tauri::command]
pub async fn dismiss_problem(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    id: i64,
) -> Result<bool, RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.dismiss(station, id).await
}

#[tauri::command]
pub async fn sync_now(state: tauri::State<'_, AppState>) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.sync_now().await
}

#[tauri::command]
pub async fn export_diagnostics(state: tauri::State<'_, AppState>) -> Result<String, RuntimeError> {
    let slot = current(&state).await?;
    let path = runtime(&slot)?.export_diagnostics().await?;
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn sim_place(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    uid_hex: String,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.sim_place(station, &uid_hex).await
}

#[tauri::command]
pub async fn sim_clear(
    state: tauri::State<'_, AppState>,
    station: Uuid,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.sim_clear(station).await
}

#[tauri::command]
pub async fn sim_pass(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    uid_hex: String,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.sim_pass(station, &uid_hex).await
}

#[tauri::command]
pub async fn sim_set_connected(
    state: tauri::State<'_, AppState>,
    station: Uuid,
    connected: bool,
) -> Result<(), RuntimeError> {
    let slot = current(&state).await?;
    runtime(&slot)?.sim_set_connected(station, connected).await
}

fn runtime(slot: &Option<Runtime>) -> Result<&Runtime, RuntimeError> {
    slot.as_ref().ok_or_else(not_configured)
}

/// Kept for the exit guard, which must stop the stations without holding the
/// lock while a request is in flight.
pub async fn shutdown_runtime(state: &AppState) {
    let mut slot = state.runtime.write().await;
    if let Some(mut running) = slot.take() {
        let _ = running.shutdown().await;
    }
}

//! Tauri lifecycle: manage the app state, register the commands, and stop the
//! stations before the window really closes.
//!
//! The runtime is started lazily by the `app_state` command rather than here:
//! `.setup` is not guaranteed to have a Tokio reactor, and a station must never
//! be spawned onto a thread that is about to go away.

mod commands;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rfidex_runtime::config::AppPaths;
use tauri::{Manager, WindowEvent};

use crate::commands::AppState;

pub fn run() {
    // First close request: stop the stations, then ask again. Second one: let
    // it through, so the app can actually exit.
    let closing = Arc::new(AtomicBool::new(false));
    let guard = closing.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            let root = app.path().app_local_data_dir()?;
            app.manage(AppState {
                paths: AppPaths::new(root),
                runtime: tokio::sync::RwLock::new(None),
            });
            Ok(())
        })
        .on_window_event(move |window, event| {
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            if guard.swap(true, Ordering::SeqCst) {
                return;
            }
            api.prevent_close();
            let window = window.clone();
            tauri::async_runtime::spawn(async move {
                let state = window.state::<AppState>();
                commands::shutdown_runtime(&state).await;
                let _ = window.close();
            });
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_state,
            commands::setup_get,
            commands::setup_save,
            commands::setup_test,
            commands::setup_test_printer,
            commands::status,
            commands::desk_scan,
            commands::desk_link,
            commands::desk_reset,
            commands::desk_search,
            commands::desk_print,
            commands::gate_recent,
            commands::problems,
            commands::dismiss_problem,
            commands::sync_now,
            commands::export_diagnostics,
            commands::sim_place,
            commands::sim_clear,
            commands::sim_pass,
            commands::sim_set_connected,
            commands::update_check,
            commands::update_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RfiDex");
}

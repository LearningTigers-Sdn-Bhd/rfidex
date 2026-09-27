// Prevents an extra console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Before anything else, and before Tauri exists.
///
/// A real reader is reached by starting this same program again in a helper
/// mode. That has to happen here: if the arguments were not recognised until
/// after `tauri::Builder` ran, a typo would open a second window instead of
/// exiting, and a station that wanted a helper would get a whole app.
///
/// `run_host_from_args` returns `None` only when the first argument is not the
/// helper switch, so an ordinary launch falls straight through to the app.
fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = rfidex_hardware::run_host_from_args(&args) {
        std::process::exit(code);
    }
    rfidex_app::run()
}

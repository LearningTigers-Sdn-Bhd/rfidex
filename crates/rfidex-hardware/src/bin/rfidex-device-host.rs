//! The developer host: the same child entry point as the desktop executable,
//! as a binary of its own.
//!
//! The runtime can point a station's launcher at this file instead of at the
//! app, which is how the integration tests drive a real host loop without a
//! Tauri window. It is also the executable to use for commissioning work.
//!
//! It is never started by hand: it is launched with the parent's port and
//! handshake token, and without them it exits rather than guessing.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = rfidex_hardware::run_host_from_args(&args) {
        std::process::exit(code);
    }
    eprintln!("rfidex-device-host talks to a reader for RfiDex and is started by it, not by hand.");
    std::process::exit(2);
}

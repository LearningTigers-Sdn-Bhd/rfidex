//! Starting every station, and stopping them again.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use rfidex_hardware::process::HostLauncher;
use tokio::sync::watch;

use super::device::StationDevice;
use super::gate_poll::gate_loop;
use super::hardware_jobs::HardwareJobs;
use super::heartbeat::heartbeat_loop;
use super::options::RuntimeOptions;
use super::station::StationRuntime;
use super::sync::sync_loop;
use super::Runtime;
use crate::config::{AppConfig, AppPaths};
use crate::devices::SimLibrary;
use crate::RuntimeError;

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
            badge: crate::BadgeService::system(paths.clone()),
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
        self.badge.cancel_pending();
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

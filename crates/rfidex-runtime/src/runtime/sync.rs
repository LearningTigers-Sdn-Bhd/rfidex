//! Sending the queue and refreshing the ticket cache, on a schedule or on demand.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rfidex_core::contract::StationKind;
use rfidex_core::sync::{SyncReport, SENT_RETENTION_DAYS};

use super::messages::{network_message, store_failure};
use super::station::StationRuntime;
use super::Runtime;
use crate::RuntimeError;

impl StationRuntime {
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
}

impl Runtime {
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
}

pub(super) async fn sync_loop(station: Arc<StationRuntime>) {
    let mut stop = station.stop.clone();
    let mut last_cache: Option<Instant> = None;
    let mut last_prune: Option<Instant> = None;
    loop {
        if *stop.borrow() {
            return;
        }
        if station.lock().event_ok {
            let now = Utc::now();
            // A desk asks the server on every scan and only reads the cache
            // offline, so it refreshes rarely; a gate decides from the cache
            // on every pass and keeps the 60s refresh.
            let every = match station.kind() {
                StationKind::Desk => Duration::from_secs(300),
                _ => Duration::from_secs(60),
            };
            let refresh = last_cache.is_none_or(|t| t.elapsed() >= every);
            let prune = last_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600));
            {
                let _guard = station.sync_lock.lock().await;
                tokio::select! {
                    _ = async {
                        // Send queued readings and check-ins first: on a weak
                        // link the full snapshot can run to its timeout, and
                        // the queue must not wait behind it.
                        match station.worker.run_once(now).await {
                            Ok(report) => station.note_sync_report(&report, now),
                            Err(_) => station.note_store_error(),
                        }
                        if refresh {
                            // Stamp before the result: a failed (e.g. timed-out)
                            // full snapshot waits the normal 60s instead of
                            // retrying every second and piling on a loaded server.
                            last_cache = Some(Instant::now());
                            match station.worker.refresh_cache().await {
                                Ok(()) => station.note_cache_ok(),
                                Err(e) => station.note_cache_error(&e),
                            }
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

//! The heartbeat: telling the server who this station is, and adopting its answer.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rfidex_core::client::ApiError;
use rfidex_core::contract::{HeartbeatReq, HeartbeatResp};
use rfidex_core::device::{GateSource, TagReaderWriter};
use rfidex_core::APP_VERSION;

use super::device::StationDevice;
use super::messages::{network_message, store_failure};
use super::options::RuntimeOptions;
use super::station::StationRuntime;
use super::SETTINGS_KEY;
use crate::RuntimeError;

impl StationRuntime {
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
    pub(super) async fn refresh_desk_connection(&self) {
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

    pub(super) async fn heartbeat_once(&self, now: DateTime<Utc>) {
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
}

pub(super) async fn heartbeat_loop(station: Arc<StationRuntime>, opts: RuntimeOptions) {
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

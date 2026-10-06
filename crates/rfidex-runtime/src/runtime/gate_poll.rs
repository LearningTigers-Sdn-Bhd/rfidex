//! A gate's poll loop, and the red-light alarm for passes the server declines.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rfidex_core::device::DeviceError;
use rfidex_core::station::gate::GateError;

use super::device::StationDevice;
use super::messages::store_failure;
use super::options::RuntimeOptions;
use super::station::StationRuntime;

impl StationRuntime {
    /// One gate poll.
    ///
    /// The poll itself is synchronous and can wait on a reader, so it runs on a
    /// blocking worker that owns its station's device. Nothing is published
    /// until the job comes back: the stop flag is read again inside the worker,
    /// so a station that was reconfigured or removed while the poll ran cannot
    /// show its result on the new screen.
    /// One poll. `true` when it captured at least one pass, so the loop can
    /// poll again at once instead of waiting out the tick.
    async fn gate_tick_once(self: &Arc<Self>, now: DateTime<Utc>) -> bool {
        let StationDevice::Gate(device) = &self.device else {
            return false;
        };
        if self.lock().settings.is_none() {
            // The UID rule is not known yet, so a read could not be keyed
            // correctly. Leave it in the device rather than consuming it.
            return false;
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
            return false;
        };
        let mut captured_any = false;
        match result {
            Ok(captured) => {
                captured_any = !captured.is_empty();
                {
                    let mut inner = self.lock();
                    inner.connected = true;
                    inner.connection_checked = true;
                    inner.device_error = None;
                }
                // The server lookup and the alarm must not hold up the next
                // poll. The alarm still takes the device lock, so it can never
                // run in the middle of a fetch; it restarts the fetch itself.
                let station = self.clone();
                let mut stop = self.stop.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = station.alarm_declined(captured) => {}
                        _ = stop.changed() => {}
                    }
                });
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
        captured_any
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
}

pub(super) async fn gate_loop(station: Arc<StationRuntime>, opts: RuntimeOptions) {
    let mut stop = station.stop.clone();
    let mut ticker = tokio::time::interval(opts.gate_poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => return,
        }
        // A library gate hands over one pass per fetch: keep fetching until it
        // is empty, so a crowd is not limited to one pass per tick. Each pass
        // is stored before the next fetch acks it, as before.
        for _ in 0..MAX_DRAIN {
            let now = Utc::now();
            let more = tokio::select! {
                more = station.gate_tick_once(now) => more,
                _ = stop.changed() => return,
            };
            if !more {
                break;
            }
        }
    }
}

/// Most polls in a row without resting, so a gate that keeps answering cannot
/// starve the rest of the app.
const MAX_DRAIN: u32 = 50;

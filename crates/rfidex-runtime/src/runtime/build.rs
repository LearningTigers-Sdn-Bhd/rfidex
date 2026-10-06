//! Building one station from its saved setup.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use rfidex_core::client::ApiClient;
use rfidex_core::contract::{HeartbeatResp, RfidMode, Role, StationKind};
use rfidex_core::station::desk::DeskStation;
use rfidex_core::station::gate::GateStation;
use rfidex_core::store::Store;
use rfidex_core::sync::SyncWorker;
use rfidex_core::tag::UidRule;
use rfidex_hardware::adapters::{EcrfidDesk, EcrfidGate};
use rfidex_hardware::process::HostLauncher;
use tokio::sync::watch;

use super::device::StationDevice;
use super::hardware_jobs::HardwareJobs;
use super::messages::{reader_failed, store_failure};
use super::options::RuntimeOptions;
use super::station::{StationInner, StationRuntime};
use super::{SEQUENCE_KEY, SETTINGS_KEY};
use crate::config::{AppConfig, AppPaths, DeviceChoice, StationConfig};
use crate::desk::DeskSession;
use crate::devices::{DeskDevice, GateDevice};
use crate::RuntimeError;

impl StationRuntime {
    pub(super) async fn build(
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

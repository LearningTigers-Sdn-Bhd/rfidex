#![allow(dead_code)]

//! A real mock server, a real runtime and a real temp data root per test.

use std::sync::Arc;
use std::time::Duration;

use rfidex_core::contract::{EventSettings, HeartbeatResp, RfidMode, Role, StationKind};
use rfidex_core::device::GateKind;
use rfidex_core::store::Store;
use rfidex_mock::http::{serve, AppState};
use rfidex_mock::state::{MockState, SeedTicket};
use rfidex_runtime::config::{AppConfig, AppPaths, DeviceChoice, StationConfig};
use rfidex_runtime::{Runtime, RuntimeOptions};
use uuid::Uuid;

pub const KEY: &str = "rfidex_test_key_0123456789abcdefghij";
pub const TAG_A: &str = "3412CDAB500104E0";
pub const TAG_B: &str = "5678CDAB500104E0";
pub const TAG_C: &str = "9ABCDEFF500104E0";

pub fn desk_id() -> Uuid {
    Uuid::from_u128(101)
}

pub fn entry_id() -> Uuid {
    Uuid::from_u128(102)
}

pub fn exit_id() -> Uuid {
    Uuid::from_u128(103)
}

pub fn ticket(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

pub fn seeds() -> Vec<SeedTicket> {
    let seed = |n, name: &str, paid, cancelled| SeedTicket {
        public_id: ticket(n),
        name: name.into(),
        ticket_type: "VIP".into(),
        paid,
        cancelled,
        email: None,
        phone: None,
        created_at: None,
        checked_in_at: None,
    };
    vec![
        seed(1, "Aina", true, false),
        seed(2, "Ben", true, false),
        seed(3, "Chong", false, false),
        seed(4, "Devi", true, true),
        // A second valid ticket with no sticker, so a test can create a
        // genuine two-sided conflict without one side having to replace.
        seed(5, "Ely", true, false),
    ]
}

/// The event the fixture helpers use: id 1, five tickets, four of them valid.
pub fn fixture_event(mode: RfidMode) -> EventSettings {
    EventSettings {
        event_id: 1,
        name: "Test Expo".into(),
        rfid_mode: mode,
        require_check_in: false,
    }
}

pub fn fast_options() -> RuntimeOptions {
    RuntimeOptions {
        heartbeat: Duration::from_millis(20),
        gate_poll: Duration::from_millis(10),
        // Generous: a slow CI runner (Windows, Defender) must not turn a normal
        // loopback request into "offline". Offline paths use the mock's `down`
        // switch, and the shutdown test's 1 s bound stays below this timeout.
        client_timeout: Duration::from_secs(2),
    }
}

pub fn desk_station() -> StationConfig {
    StationConfig {
        id: desk_id(),
        name: "Desk".into(),
        kind: StationKind::Desk,
        role: None,
        device: DeviceChoice::SimDesk,
        debounce_secs: 5,
        write_start_block: 0,
        printer_url: rfidex_runtime::default_printer_url(),
    }
}

pub fn gate_station(id: Uuid, name: &str, role: Role) -> StationConfig {
    StationConfig {
        id,
        name: name.into(),
        kind: StationKind::Gate,
        role: Some(role),
        device: DeviceChoice::SimGate {
            gate_kind: GateKind::Records,
            release_verified: false,
        },
        debounce_secs: 5,
        write_start_block: 0,
        printer_url: rfidex_runtime::default_printer_url(),
    }
}

pub fn three_stations() -> Vec<StationConfig> {
    vec![
        desk_station(),
        gate_station(entry_id(), "Entry gate", Role::Entry),
        gate_station(exit_id(), "Exit gate", Role::Exit),
    ]
}

pub struct Harness {
    pub runtime: Runtime,
    pub server: Arc<AppState>,
    pub paths: AppPaths,
    pub base: String,
    pub opts: RuntimeOptions,
    temp: tempfile::TempDir,
    handle: tokio::task::JoinHandle<()>,
}

impl Harness {
    pub async fn start(mode: RfidMode) -> Harness {
        let harness = Harness::build(mode, fast_options(), three_stations(), false).await;
        // A station is only operational once a heartbeat has landed, so tests
        // must not race the first one.
        harness.await_online().await;
        harness
    }

    pub async fn start_with(
        mode: RfidMode,
        opts: RuntimeOptions,
        stations: Vec<StationConfig>,
    ) -> Harness {
        let harness = Harness::build(mode, opts, stations, false).await;
        harness.await_online().await;
        harness
    }

    /// First run with the server unreachable: the stations exist, but no
    /// heartbeat has ever succeeded, so nothing is saved and nothing works yet.
    pub async fn start_with_server_down(mode: RfidMode) -> Harness {
        Harness::build(mode, fast_options(), three_stations(), true).await
    }

    /// A runtime whose key the server will reject: well formed, but wrong.
    pub async fn start_with_bad_key(mode: RfidMode) -> Harness {
        Harness::build_keyed(
            mode,
            fast_options(),
            three_stations(),
            false,
            "wrong_key_wrong_key_wrong_key_xx",
        )
        .await
    }

    /// A runtime over a caller-supplied event and ticket set. The rehearsal uses
    /// this so nothing it proves can come from the five-ticket fixture or from
    /// event id 1 by accident.
    pub async fn start_seeded(
        event: EventSettings,
        tickets: Vec<SeedTicket>,
        opts: RuntimeOptions,
        stations: Vec<StationConfig>,
    ) -> Harness {
        let first_ticket = tickets.first().map(|t| t.public_id);
        let harness = Harness::build_seeded(event, tickets, opts, stations, KEY, false).await;
        harness.await_seeded_ready(first_ticket).await;
        harness
    }

    async fn build(
        mode: RfidMode,
        opts: RuntimeOptions,
        stations: Vec<StationConfig>,
        down: bool,
    ) -> Harness {
        Harness::build_keyed(mode, opts, stations, down, KEY).await
    }

    async fn build_keyed(
        mode: RfidMode,
        opts: RuntimeOptions,
        stations: Vec<StationConfig>,
        down: bool,
        key: &str,
    ) -> Harness {
        Harness::build_seeded(fixture_event(mode), seeds(), opts, stations, key, down).await
    }

    /// The one place the server, the data root, the config and the runtime are
    /// put together. `key` is what the client sends; the server always expects
    /// [`KEY`], so a test can hand the runtime a wrong one.
    async fn build_seeded(
        event: EventSettings,
        tickets: Vec<SeedTicket>,
        opts: RuntimeOptions,
        stations: Vec<StationConfig>,
        key: &str,
        down: bool,
    ) -> Harness {
        let state = Arc::new(AppState::new(MockState::new(KEY.into(), event, tickets)));
        if down {
            state.faults.lock().unwrap().down = true;
        }
        let (addr, handle) = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let base = format!("http://{addr}");
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path().to_path_buf());
        let config = AppConfig {
            server_url: base.clone(),
            api_key: key.into(),
            stations,
        };
        paths.save(&config).unwrap();
        let runtime = Runtime::start(paths.clone(), config, opts.clone())
            .await
            .unwrap();
        Harness {
            runtime,
            server: state,
            paths,
            base,
            opts,
            temp,
            handle,
        }
    }

    /// Stop the runtime and start a fresh one over the same data root and the
    /// same server: a restart, not a fresh install.
    pub async fn restart_runtime(&mut self) {
        self.restart_runtime_offline().await;
        self.await_online().await;
    }

    /// The same restart, while the server is unreachable, so the test can check
    /// what the station knows from disk alone.
    pub async fn restart_runtime_offline(&mut self) {
        self.runtime.shutdown().await.unwrap();
        let config = self.runtime.config().clone();
        self.runtime = Runtime::start(self.paths.clone(), config, self.opts.clone())
            .await
            .unwrap();
    }

    /// A station is operational when every station is online, and the desk has
    /// the ticket cache a first scan needs.
    pub async fn await_online(&self) {
        eventually("every station to be online", || async {
            self.runtime
                .stations()
                .iter()
                .all(|s| s.connected() && s.online())
        })
        .await;
        self.await_cached(desk_station().id, ticket(1)).await;
    }

    /// Readiness for a seeded harness: every station online with the settings it
    /// asked for, then a real cache refresh, so no test has to guess a poll
    /// interval before its first scan. Nothing here is named after the fixture.
    async fn await_seeded_ready(&self, first_ticket: Option<Uuid>) {
        eventually("every station to be online with its settings", || async {
            self.runtime
                .stations()
                .iter()
                .all(|s| s.connected() && s.online() && s.event_ok())
        })
        .await;
        self.runtime
            .sync_now()
            .await
            .expect("the first sync pass of a fresh runtime");
        if let Some(id) = first_ticket {
            for station in self.runtime.stations() {
                self.await_cached(station.id(), id).await;
            }
        }
    }

    pub async fn await_cached(&self, station: Uuid, id: Uuid) {
        eventually("the ticket cache to be ready", || async {
            self.store_of(station).ticket(id).ok().flatten().is_some()
        })
        .await;
    }

    pub fn station(&self, id: Uuid) -> &rfidex_runtime::runtime::StationRuntime {
        self.runtime
            .stations()
            .iter()
            .find(|s| s.id() == id)
            .expect("station is configured")
    }

    /// Read the settings this station has saved to its own database, the way a
    /// restart would find them.
    pub fn saved_settings(&self, station: Uuid) -> Option<HeartbeatResp> {
        let store = Store::open(&self.paths.station_db(station)).expect("open station db");
        let text = store.get_config("heartbeat").expect("read config")?;
        serde_json::from_str(&text).expect("saved settings parse")
    }

    pub fn saved_sequence(&self, station: Uuid) -> u64 {
        let store = Store::open(&self.paths.station_db(station)).expect("open station db");
        store
            .get_config("sim_next_sequence")
            .expect("read config")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    pub fn store_of(&self, station: Uuid) -> Store {
        Store::open(&self.paths.station_db(station)).expect("open station db")
    }

    pub fn set_mode(&self, mode: RfidMode) {
        self.server.mock.lock().unwrap().event.rfid_mode = mode;
    }

    pub fn set_event(&self, event_id: i64) {
        self.server.mock.lock().unwrap().event.event_id = event_id;
    }

    pub fn set_down(&self, down: bool) {
        self.server.faults.lock().unwrap().down = down;
    }

    /// Disarm every fault, exactly as a fresh server would start.
    pub fn clear_faults(&self) {
        *self.server.faults.lock().unwrap() = rfidex_mock::http::Faults::default();
    }

    /// Arm the whole fault object, so a field left over from an earlier phase
    /// can never decide what the next request does.
    pub fn set_faults(&self, faults: rfidex_mock::http::Faults) {
        *self.server.faults.lock().unwrap() = faults;
    }

    pub fn observations(&self) -> usize {
        self.server.mock.lock().unwrap().observation_count()
    }

    pub async fn stop(&mut self) {
        let _ = self.runtime.shutdown().await;
        self.handle.abort();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.handle.abort();
        let _ = &self.temp;
    }
}

/// Wait for a specific state, with a bounded deadline instead of a guessed
/// sleep. Panics with the label when the state never arrives.
pub async fn eventually<F, Fut>(label: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use rfidex_core::contract::{EventSettings, RfidMode};
use rfidex_core::store::Store;
use rfidex_mock::http::{serve, AppState};
use rfidex_mock::state::{MockState, SeedTicket};
use uuid::Uuid;

pub const KEY: &str = "rfidex_test_key_0123456789abcdefghij";
pub const TAG_A: &str = "3412CDAB500104E0";
pub const TAG_B: &str = "5678CDAB500104E0";

pub fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

pub fn seeds() -> Vec<SeedTicket> {
    let seed = |n, name: &str, paid, cancelled| SeedTicket {
        public_id: id(n),
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
    ]
}

pub async fn spawn(mode: RfidMode) -> (String, Arc<AppState>) {
    let event = EventSettings {
        event_id: 1,
        name: "Test Expo".into(),
        rfid_mode: mode,
        require_check_in: false,
    };
    let state = Arc::new(AppState::new(MockState::new(KEY.into(), event, seeds())));
    let (addr, _handle) = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (format!("http://{addr}"), state)
}

pub fn store() -> Arc<Mutex<Store>> {
    Arc::new(Mutex::new(Store::open_in_memory().unwrap()))
}

pub fn client(base: &str, station: &str) -> rfidex_core::client::ApiClient {
    rfidex_core::client::ApiClient::new(base, KEY, station, std::time::Duration::from_millis(500))
}

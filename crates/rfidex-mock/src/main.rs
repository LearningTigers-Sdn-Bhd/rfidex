//! rfidex-mock --port 4010 --api-key <key> --tickets tickets.json [--event-name "Demo"] [--mode bind|write]

use std::collections::HashMap;
use std::sync::Arc;

use rfidex_core::contract::{EventSettings, RfidMode};
use rfidex_mock::http::{serve, AppState};
use rfidex_mock::state::{MockState, SeedTicket};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts: HashMap<&str, &str> = args
        .chunks(2)
        .filter_map(|c| Some((c.first()?.trim_start_matches("--"), c.get(1)?.as_str())))
        .collect();
    let port: u16 = opts
        .get("port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(4010);
    let api_key = opts
        .get("api-key")
        .expect("--api-key is required")
        .to_string();
    assert!(
        api_key.len() > 30 && !api_key.contains(' '),
        "api key must be >30 chars, no spaces (EventzFlow rule)"
    );
    let seeds: Vec<SeedTicket> = match opts.get("tickets") {
        Some(path) => {
            serde_json::from_str(&std::fs::read_to_string(path).expect("read tickets file"))
                .expect("parse tickets JSON")
        }
        None => Vec::new(),
    };
    let rfid_mode = if opts.get("mode") == Some(&"write") {
        RfidMode::Write
    } else {
        RfidMode::Bind
    };
    let event = EventSettings {
        event_id: 1,
        name: opts.get("event-name").unwrap_or(&"RfiDex Demo").to_string(),
        rfid_mode,
        require_check_in: false,
    };
    let count = seeds.len();
    let state = Arc::new(AppState::new(MockState::new(api_key, event, seeds)));
    let (addr, handle) = serve(state, ([127, 0, 0, 1], port).into())
        .await
        .expect("bind port");
    println!("rfidex-mock listening on http://{addr} with {count} tickets");
    handle.await.expect("server task");
}

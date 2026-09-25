//! Every fixture must deserialize into its type and serialize back to identical JSON.
//! These fixtures are the contract examples handed to the backend later.

use rfidex_core::contract::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let original: Value = serde_json::from_str(&text).unwrap();
    let typed: T =
        serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let back = serde_json::to_value(&typed).unwrap();
    assert_eq!(back, original, "{name} did not round-trip");
}

#[test]
fn fixtures_round_trip() {
    round_trip::<HeartbeatResp>("heartbeat_resp");
    round_trip::<DeskScanResp>("desk_scan_resp");
    round_trip::<BindingReq>("binding_req");
    round_trip::<ErrorBody>("error_uid_bound_elsewhere");
    round_trip::<ObservationsReq>("observations_req");
    round_trip::<ObservationsResp>("observations_resp");
}

//! Every fixture must deserialize into its type and serialize back to identical JSON.
//! These fixtures are the contract examples handed to the backend later.

use rfidex_core::contract::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) {
    let original: Value = fixture(name);
    let typed: T =
        serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let back = serde_json::to_value(&typed).unwrap();
    assert_eq!(back, original, "{name} did not round-trip");
}

#[test]
fn fixtures_round_trip() {
    round_trip::<HeartbeatResp>("heartbeat_resp");
    round_trip::<DeskScanResp>("desk_scan_resp");
    round_trip::<DeskScanResp>("desk_scan_already_resp");
    round_trip::<BindingReq>("binding_req");
    round_trip::<ErrorBody>("error_uid_bound_elsewhere");
    round_trip::<ObservationsReq>("observations_req");
    round_trip::<ObservationsResp>("observations_resp");
    round_trip::<TicketSearchResp>("ticket_search_resp");
    round_trip::<TicketSearchResp>("ticket_search_empty_resp");
}

#[test]
fn the_two_check_in_outcomes_are_distinct() {
    let first: DeskScanResp = serde_json::from_value(fixture("desk_scan_resp")).unwrap();
    let already: DeskScanResp = serde_json::from_value(fixture("desk_scan_already_resp")).unwrap();
    assert_eq!(first.check_in.result, CheckInResult::CheckedIn);
    assert_eq!(already.check_in.result, CheckInResult::AlreadyCheckedIn);
    assert_eq!(
        first.check_in.checked_in_at, already.check_in.checked_in_at,
        "the first check-in time is the same in both answers"
    );
    assert_eq!(
        first.ticket, already.ticket,
        "only the outcome differs between the two responses"
    );
}

#[test]
fn a_desk_scan_needs_a_readable_check_in() {
    let complete = fixture("desk_scan_resp");

    let mut missing = complete.clone();
    missing.as_object_mut().unwrap().remove("check_in");
    assert!(
        serde_json::from_value::<DeskScanResp>(missing).is_err(),
        "a successful response cannot leave check_in out"
    );

    let mut null = complete.clone();
    null["check_in"] = Value::Null;
    assert!(
        serde_json::from_value::<DeskScanResp>(null).is_err(),
        "check_in is not optional on a successful response"
    );

    let mut unknown = complete.clone();
    unknown["check_in"]["result"] = Value::String("reprinted".into());
    assert!(
        serde_json::from_value::<DeskScanResp>(unknown).is_err(),
        "only the two agreed outcomes are readable"
    );

    let mut bad_time = complete.clone();
    bad_time["check_in"]["checked_in_at"] = Value::String("yesterday".into());
    assert!(
        serde_json::from_value::<DeskScanResp>(bad_time).is_err(),
        "an unreadable timestamp is a bad response, not a silent default"
    );
}

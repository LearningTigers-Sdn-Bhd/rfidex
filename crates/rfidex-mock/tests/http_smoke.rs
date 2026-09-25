use rfidex_core::contract::*;
use serde_json::json;

mod common;

#[tokio::test]
async fn auth_station_header_and_faults() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    let http = reqwest::Client::new();
    let url = format!("{base}{}", paths::CACHE);
    let authed = || {
        http.get(&url)
            .header("Authorization", common::KEY)
            .header(HEADER_STATION, "desk-1")
    };

    assert_eq!(http.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        http.get(&url)
            .header("Authorization", common::KEY)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    let r = authed().send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body: CacheResp = r.json().await.unwrap();
    assert_eq!(body.tickets.len(), 4);

    state.faults.lock().unwrap().down = true;
    assert_eq!(authed().send().await.unwrap().status(), 503);

    let r = http
        .post(format!("{base}/__mock/faults"))
        .json(&json!({"fail_5xx": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(authed().send().await.unwrap().status(), 500);
    assert_eq!(authed().send().await.unwrap().status(), 200);
}

#[tokio::test]
async fn observation_batch_limit() {
    let (base, _state) = common::spawn(RfidMode::Bind).await;
    let item = |n: u128| {
        json!({
            "delivery_id": common::id(5000 + n), "role": "entry", "protocol": "iso15693",
            "uid_raw_hex": common::TAG_A, "payload_hex": null, "device_direction_raw": null,
            "device_time_raw_hex": null, "device_record_seq": null, "flags_raw": null,
            "captured_at": "2026-09-25T06:30:00Z"
        })
    };
    let batch: Vec<_> = (0..51).map(item).collect();
    let r = reqwest::Client::new()
        .post(format!("{base}{}", paths::OBSERVATIONS))
        .header("Authorization", common::KEY)
        .header(HEADER_STATION, "gate-in")
        .json(&json!({ "observations": batch }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

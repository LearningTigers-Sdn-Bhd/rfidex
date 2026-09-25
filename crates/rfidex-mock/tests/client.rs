use std::time::Duration;

use chrono::Utc;
use rfidex_core::client::{ApiClient, ApiError};
use rfidex_core::contract::*;
use rfidex_core::tag::Protocol;

mod common;
use common::{id, TAG_A};

fn bind_req(ticket: u128, op: u128) -> BindingReq {
    BindingReq {
        public_id: id(ticket),
        protocol: Protocol::Iso15693,
        uid_raw_hex: TAG_A.into(),
        mode: BindMode::Bind,
        payload_version: None,
        operation_id: id(2000 + op),
        captured_at: Utc::now(),
        replace: false,
        reason: None,
    }
}

#[tokio::test]
async fn happy_paths() {
    let (base, state) = common::spawn(RfidMode::Write).await;
    let c = common::client(&base, "desk-1");
    let hb = c
        .heartbeat(&HeartbeatReq {
            name: "Desk 1".into(),
            kind: StationKind::Desk,
            role: None,
            hw_model: None,
            firmware: None,
            app_version: "0.1.0".into(),
        })
        .await
        .unwrap();
    assert_eq!(hb.event.rfid_mode, RfidMode::Write);
    assert!(state.mock.lock().unwrap().stations.contains_key("desk-1"));
    assert_eq!(c.cache().await.unwrap().tickets.len(), 4);
    let scan = c
        .desk_scan(&DeskScanReq {
            public_id: id(1),
            operation_id: id(9),
            captured_at: Utc::now(),
        })
        .await
        .unwrap();
    assert!(scan.ticket.checked_in);
    assert_eq!(
        c.bind(&bind_req(1, 1)).await.unwrap().binding.tag_key,
        TAG_A
    );
    assert_eq!(c.lookup(TAG_A).await.unwrap().holder.unwrap().name, "Aina");
}

#[tokio::test]
async fn error_classification() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    let c = common::client(&base, "desk-1");

    let bad_key = ApiClient::new(
        &base,
        "wrong_key_wrong_key_wrong_key_xx",
        "desk-1",
        Duration::from_millis(500),
    );
    assert!(matches!(bad_key.cache().await, Err(ApiError::Unauthorized)));

    c.bind(&bind_req(1, 1)).await.unwrap();
    match c.bind(&bind_req(2, 2)).await {
        Err(ApiError::Rejected { status: 409, body }) => {
            assert_eq!(body.error, ErrorCode::UidBoundElsewhere);
            assert_eq!(body.holder.unwrap().name, "Aina");
        }
        other => panic!("expected 409, got {other:?}"),
    }

    state.faults.lock().unwrap().down = true;
    assert!(matches!(c.cache().await, Err(ApiError::Retryable(_))));
    state.faults.lock().unwrap().down = false;

    state.faults.lock().unwrap().hang_after_commit = 1;
    let r = c
        .desk_scan(&DeskScanReq {
            public_id: id(2),
            operation_id: id(10),
            captured_at: Utc::now(),
        })
        .await;
    assert!(
        matches!(r, Err(ApiError::Retryable(_))),
        "timeout must be retryable"
    );
    assert_eq!(
        state.mock.lock().unwrap().scan_log_count,
        1,
        "server committed before the timeout"
    );

    let dead = ApiClient::new(
        "http://127.0.0.1:9",
        common::KEY,
        "desk-1",
        Duration::from_millis(300),
    );
    assert!(matches!(dead.cache().await, Err(ApiError::Retryable(_))));
}

mod common;
use common::{client, id, spawn};
use rfidex_core::client::ApiError;
use rfidex_core::contract::RfidMode;
use serde_json::json;

#[tokio::test]
async fn public_ticket_fields_arrive_without_private_headers() {
    let (base, state) = spawn(RfidMode::Bind).await;
    state
        .mock
        .lock()
        .unwrap()
        .set_badge_fields(id(1), json!({"company":"Borneo Expo"}));
    let data = client(&base, "desk").badge_ticket(1, id(1)).await.unwrap();
    assert_eq!(data["attendee_name"], "Aina");
    assert_eq!(data["ticket_type"], "VIP");
    assert_eq!(data["custom_fields_data"]["company"], "Borneo Expo");
    assert_eq!(*state.public_requests.lock().unwrap(), vec![(false, false)]);
}

#[tokio::test]
async fn wrong_event_unknown_ticket_bad_body_and_down_are_refused() {
    let (base, state) = spawn(RfidMode::Bind).await;
    let api = client(&base, "desk");
    for (event, ticket) in [(2, id(1)), (1, id(999))] {
        assert!(matches!(
            api.badge_ticket(event, ticket).await,
            Err(ApiError::Rejected { status: 404, .. })
        ));
    }
    state.faults.lock().unwrap().bad_body = 1;
    assert!(matches!(
        api.badge_ticket(1, id(1)).await,
        Err(ApiError::BadResponse(_))
    ));
    state.faults.lock().unwrap().down = true;
    assert!(matches!(
        api.badge_ticket(1, id(1)).await,
        Err(ApiError::Retryable(_))
    ));
}

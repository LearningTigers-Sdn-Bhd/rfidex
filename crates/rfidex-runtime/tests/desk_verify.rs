//! Verify screen: hold a sticker near the desk reader and see whose it is,
//! without changing anything.

mod common;

use common::*;
use rfidex_core::contract::RfidMode;
use rfidex_runtime::{DeskStep, VerifyState};

#[tokio::test]
async fn no_sticker_waits_and_a_bound_sticker_names_its_guest() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();

    let idle = h.runtime.desk_verify(desk).await.unwrap();
    assert_eq!(idle.state, VerifyState::Waiting);

    h.runtime
        .desk_scan(desk, &ticket(1).to_string())
        .await
        .unwrap();
    h.runtime.sim_place(desk, TAG_A).await.unwrap();
    let linked = h.runtime.desk_link(desk, None).await.unwrap();
    assert_eq!(linked.step, DeskStep::Linked);

    let seen = h.runtime.desk_verify(desk).await.unwrap();
    assert_eq!(seen.state, VerifyState::Verified);
    assert_eq!(seen.holder.unwrap().name, "Aina");
    assert_eq!(seen.sticker.as_deref(), Some(TAG_A));
}

#[tokio::test]
async fn a_sticker_nobody_linked_is_unknown_and_nothing_is_written() {
    let h = Harness::start(RfidMode::Bind).await;
    let desk = desk_id();
    h.runtime.sim_place(desk, TAG_B).await.unwrap();

    let seen = h.runtime.desk_verify(desk).await.unwrap();
    assert_eq!(seen.state, VerifyState::Unknown);
    assert!(seen.holder.is_none());
    assert_eq!(
        h.server.mock.lock().unwrap().binding_count(),
        0,
        "verifying never links a sticker"
    );
}

#[tokio::test]
async fn a_gate_cannot_verify() {
    let h = Harness::start(RfidMode::Bind).await;
    assert!(h.runtime.desk_verify(entry_id()).await.is_err());
}

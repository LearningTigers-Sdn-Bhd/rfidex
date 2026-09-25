use rfidex_core::codec;
use rfidex_core::contract::*;
use rfidex_core::device::sim_desk::{SimDesk, SimTag};
use rfidex_core::station::desk::{Confirm, DeskError, DeskStation, Warning};
use rfidex_core::store::{OutboxKind, OutboxState};
use rfidex_core::tag::{parse_hex, UidRule};

mod common;
use common::{id, TAG_A, TAG_B};

fn tag(hex: &str, block_size: usize, blocks: usize) -> SimTag {
    SimTag::blank(parse_hex(hex).unwrap(), block_size, blocks)
}

async fn desk(
    mode: RfidMode,
) -> (
    DeskStation<SimDesk>,
    std::sync::Arc<rfidex_mock::http::AppState>,
) {
    let (base, state) = common::spawn(mode).await;
    let station = DeskStation::new(
        SimDesk::new(),
        common::store(),
        common::client(&base, "desk-1"),
        mode,
        UidRule::AsIs,
        0,
    );
    (station, state)
}

fn confirm(reason: &str) -> Option<Confirm> {
    Some(Confirm {
        reason: reason.into(),
    })
}

#[tokio::test]
async fn bind_mode_happy_path() {
    let (mut d, state) = desk(RfidMode::Bind).await;
    let scanned = d.scan_ticket(&id(1).to_string()).await.unwrap();
    assert!(scanned.ticket.checked_in && !scanned.offline);
    assert!(matches!(d.detect_tag(), Err(DeskError::NoTag)));
    d.reader.place(tag(TAG_A, 4, 28));
    let t = d.detect_tag().unwrap();
    let linked = d.link(&scanned.ticket, &t, None).await.unwrap();
    assert_eq!(linked.binding.unwrap().mode, BindMode::Bind);
    assert_eq!(state.mock.lock().unwrap().active_bindings().len(), 1);
    assert_eq!(
        d.reader.tag(&t.uid_raw).unwrap().memory,
        vec![0; 112],
        "bind mode never writes"
    );
}

#[tokio::test]
async fn rejects_bad_tickets_and_multiple_tags() {
    let (mut d, _state) = desk(RfidMode::Bind).await;
    assert!(matches!(
        d.scan_ticket("not-a-uuid").await,
        Err(DeskError::TicketNotFound)
    ));
    assert!(matches!(
        d.scan_ticket(&id(3).to_string()).await,
        Err(DeskError::TicketUnpaid)
    ));
    assert!(matches!(
        d.scan_ticket(&id(4).to_string()).await,
        Err(DeskError::TicketCancelled)
    ));
    d.reader.place(tag(TAG_A, 4, 28));
    d.reader.place(tag(TAG_B, 4, 28));
    assert!(matches!(d.detect_tag(), Err(DeskError::MultipleTags(2))));
}

#[tokio::test]
async fn sticker_bound_elsewhere_needs_confirm_with_reason() {
    let (mut d, state) = desk(RfidMode::Bind).await;
    let aina = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    d.reader.place(tag(TAG_A, 4, 28));
    let t = d.detect_tag().unwrap();
    d.link(&aina, &t, None).await.unwrap();

    let ben = d.scan_ticket(&id(2).to_string()).await.unwrap().ticket;
    match d.link(&ben, &t, None).await {
        Err(DeskError::NeedsConfirm(Warning::StickerBoundElsewhere { holder })) => {
            assert_eq!(holder.unwrap().name, "Aina")
        }
        other => panic!("expected confirm, got {other:?}"),
    }
    assert!(matches!(
        d.link(&ben, &t, confirm("  ")).await,
        Err(DeskError::ReasonRequired)
    ));
    d.link(&ben, &t, confirm("badge swapped")).await.unwrap();
    let active = state.mock.lock().unwrap().active_bindings();
    assert_eq!((active.len(), active[0].public_id), (1, id(2)));
}

#[tokio::test]
async fn write_mode_writes_verifies_and_binds() {
    let (mut d, state) = desk(RfidMode::Write).await;
    let aina = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    d.reader.place(tag(TAG_A, 4, 28));
    let t = d.detect_tag().unwrap();
    d.reader.tear_next_write_after = Some(6); // first attempt torn, retry succeeds
    let linked = d.link(&aina, &t, None).await.unwrap();
    assert_eq!(linked.binding.unwrap().mode, BindMode::Written);
    assert_eq!(
        codec::decode(&d.reader.tag(&t.uid_raw).unwrap().memory),
        Ok(id(1))
    );
    assert_eq!(state.mock.lock().unwrap().active_bindings().len(), 1);
}

#[tokio::test]
async fn write_mode_failures_create_no_binding() {
    let (mut d, state) = desk(RfidMode::Write).await;
    let aina = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;

    d.reader.place(tag(TAG_A, 4, 4)); // 16 bytes < 20-byte payload
    let small = d.detect_tag().unwrap();
    assert!(matches!(
        d.link(&aina, &small, None).await,
        Err(DeskError::PayloadTooLarge {
            capacity: 16,
            needed: 20
        })
    ));
    d.reader.clear();

    d.reader.place(tag(TAG_B, 4, 28));
    let t = d.detect_tag().unwrap();
    d.reader.fail_next_writes = 2;
    assert!(matches!(
        d.link(&aina, &t, None).await,
        Err(DeskError::WriteVerifyFailed)
    ));

    d.reader.write_supported = false;
    assert!(matches!(
        d.link(&aina, &t, None).await,
        Err(DeskError::WriteUnsupported)
    ));
    assert!(state.mock.lock().unwrap().active_bindings().is_empty());
}

#[tokio::test]
async fn write_mode_warns_about_existing_payload() {
    let (mut d, _state) = desk(RfidMode::Write).await;
    let aina = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    let mut other = tag(TAG_A, 4, 28);
    other.memory[..20].copy_from_slice(&codec::encode(id(2)));
    d.reader.place(other);
    let t = d.detect_tag().unwrap();
    assert!(matches!(
        d.link(&aina, &t, None).await,
        Err(DeskError::NeedsConfirm(Warning::StickerHasOtherPayload { public_id })) if public_id == id(2)
    ));
    d.reader.clear();
    let mut junk = tag(TAG_B, 4, 28);
    junk.memory[0] = 0x55;
    d.reader.place(junk);
    let t = d.detect_tag().unwrap();
    assert!(matches!(
        d.link(&aina, &t, None).await,
        Err(DeskError::NeedsConfirm(Warning::StickerHasUnknownData))
    ));
    d.link(&aina, &t, confirm("reused sticker")).await.unwrap();
}

#[tokio::test]
async fn offline_desk_uses_cache_and_queues() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    let store = common::store();
    let client = common::client(&base, "desk-1");
    rfidex_core::sync::SyncWorker::new(store.clone(), client.clone())
        .refresh_cache()
        .await
        .unwrap();
    let mut d = DeskStation::new(
        SimDesk::new(),
        store.clone(),
        client,
        RfidMode::Bind,
        UidRule::AsIs,
        0,
    );

    state.faults.lock().unwrap().down = true;
    let scanned = d.scan_ticket(&id(1).to_string()).await.unwrap();
    assert!(scanned.offline);
    assert!(matches!(
        d.scan_ticket(&id(3).to_string()).await,
        Err(DeskError::TicketInvalid)
    ));
    d.reader.place(tag(TAG_A, 4, 28));
    let t = d.detect_tag().unwrap();
    let linked = d.link(&scanned.ticket, &t, None).await.unwrap();
    assert!(linked.offline && linked.binding.is_none());

    let s = store.lock().unwrap();
    assert_eq!(s.count(OutboxState::Pending).unwrap(), 2);
    assert_eq!(
        s.due(OutboxKind::Binding, 10, chrono::Utc::now())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(s.binding_holder(TAG_A).unwrap(), Some(id(1)));
}

#[tokio::test]
async fn configure_switches_mode_and_uid_rule_on_a_live_desk() {
    let (base, state) = common::spawn(RfidMode::Bind).await;
    let store = common::store();
    let mut d = DeskStation::new(
        SimDesk::new(),
        store.clone(),
        common::client(&base, "desk-1"),
        RfidMode::Bind,
        UidRule::AsIs,
        0,
    );
    let aina = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    d.reader.place(tag(TAG_A, 4, 28));
    let t = d.detect_tag().unwrap();
    assert_eq!(
        d.link(&aina, &t, None).await.unwrap().binding.unwrap().mode,
        BindMode::Bind
    );
    assert_eq!(
        d.reader.tag(&t.uid_raw).unwrap().memory,
        vec![0; 112],
        "bind mode never writes"
    );

    d.configure(RfidMode::Write, UidRule::AsIs);
    assert_eq!(d.mode(), RfidMode::Write);
    let ben = d.scan_ticket(&id(2).to_string()).await.unwrap().ticket;
    d.reader.clear();
    d.reader.place(tag(TAG_B, 4, 28));
    let b = d.detect_tag().unwrap();
    let linked = d.link(&ben, &b, None).await.unwrap();
    assert_eq!(linked.binding.unwrap().mode, BindMode::Written);
    assert_eq!(
        codec::decode(&d.reader.tag(&b.uid_raw).unwrap().memory),
        Ok(id(2)),
        "the reconfigured desk wrote the ticket into the sticker"
    );
    assert_eq!(state.mock.lock().unwrap().active_bindings().len(), 2);

    // Still the same desk, now offline: the new UID rule decides the local
    // binding key, but the request still carries the raw UID unreordered.
    rfidex_core::sync::SyncWorker::new(store.clone(), common::client(&base, "desk-1"))
        .refresh_cache()
        .await
        .unwrap();
    state.faults.lock().unwrap().down = true;
    d.configure(RfidMode::Bind, UidRule::Reversed);
    let aina_again = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    d.reader.clear();
    d.reader.place(tag(TAG_A, 4, 28));
    let a = d.detect_tag().unwrap();
    // Offline the station cannot know that the reversed key is the very same
    // sticker, so it asks before replacing Aina's known sticker.
    assert!(matches!(
        d.link(&aina_again, &a, None).await,
        Err(DeskError::NeedsConfirm(Warning::TicketHasSticker))
    ));
    assert!(
        d.link(&aina_again, &a, confirm("uid rule changed"))
            .await
            .unwrap()
            .offline
    );

    let s = store.lock().unwrap();
    assert_eq!(s.binding_holder("E0040150ABCD1234").unwrap(), Some(id(1)));
    assert_eq!(s.binding_holder(TAG_A).unwrap(), None);
    let pending = s.due(OutboxKind::Binding, 10, chrono::Utc::now()).unwrap();
    assert_eq!(pending[0].payload["uid_raw_hex"], TAG_A);
}

#[tokio::test]
async fn write_mode_checks_existing_sticker_before_writing() {
    let (mut d, state) = desk(RfidMode::Write).await;
    state
        .mock
        .lock()
        .unwrap()
        .bind(BindingReq {
            public_id: id(1),
            protocol: rfidex_core::tag::Protocol::Iso15693,
            uid_raw_hex: TAG_A.into(),
            mode: BindMode::Written,
            payload_version: Some(1),
            operation_id: id(9001),
            captured_at: chrono::Utc::now(),
            replace: false,
            reason: None,
        })
        .unwrap();
    let aina = d.scan_ticket(&id(1).to_string()).await.unwrap().ticket;
    d.reader.place(tag(TAG_B, 4, 28));
    let t = d.detect_tag().unwrap();
    assert!(matches!(
        d.link(&aina, &t, None).await,
        Err(DeskError::NeedsConfirm(Warning::TicketHasSticker))
    ));
    assert_eq!(
        d.reader.tag(&t.uid_raw).unwrap().memory,
        vec![0; 112],
        "nothing written before staff confirm"
    );
}

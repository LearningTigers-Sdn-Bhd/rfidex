//! The legacy `0xEC` candidate profile, exercised over a real loopback socket.
//!
//! Every frame here is a synthetic candidate, not a captured vendor frame: the
//! CRC polynomial is unconfirmed. `support::frames::pinned_vectors` fixes the
//! exact bytes so these tests cannot agree with a wrong implementation.

mod support;

use std::time::Duration;

use rfidex_core::device::DeviceError;
use rfidex_hardware::tcp::TcpReader;
use rfidex_hardware::wire::WireTag;
use support::frames::{network_layout_frame, tag_frame, terminal_frame};
use support::{Chunk, FakeReader, Script, UID_A, UID_B, UID_C};

const REQUEST: [u8; 8] = [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x00, 0x38, 0x8B];
const REQUEST_ANTENNA: [u8; 8] = [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x04, 0x39, 0x48];

fn tag(uid: [u8; 8]) -> WireTag {
    WireTag {
        uid,
        dsfid: 0,
        antenna: None,
    }
}

fn response_body(uid: &[u8; 8]) -> Vec<u8> {
    let mut out = tag_frame(0x00, 0x00, uid, None);
    out.extend_from_slice(&terminal_frame(0x00, 0x00));
    out
}

#[test]
fn tcp_inventory_sends_the_pinned_candidate_request() {
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::answer(
        REQUEST.to_vec(),
        response_body(&UID_A),
    )]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    let tags = reader.inventory().unwrap();
    assert_eq!(tags, vec![tag(UID_A)]);
    assert_eq!(handle.join().unwrap(), vec![REQUEST.to_vec()]);
}

#[test]
fn tcp_inventory_reassembles_and_waits_for_end() {
    let body = response_body(&UID_A);
    // Split the whole exchange after every byte in turn: the reader must wait
    // for a complete frame and for the terminal, never guess from a short read.
    for split in 1..body.len() - 1 {
        let fake = FakeReader::bind();
        let handle = fake.expect_connections(vec![Script::scripted(
            REQUEST.to_vec(),
            vec![
                Chunk::now(body[..split].to_vec()),
                Chunk {
                    bytes: body[split..].to_vec(),
                    delay: Duration::from_millis(20),
                },
            ],
            false,
        )]);
        let mut reader = TcpReader::new(fake.config(1_000, false)).unwrap();
        let tags = reader.inventory().unwrap_or_else(|e| {
            panic!("split after {split} bytes failed: {e:?}");
        });
        assert_eq!(tags, vec![tag(UID_A)], "split after {split} bytes");
        handle.join().unwrap();
    }
}

#[test]
fn tcp_inventory_handles_coalesced_frames() {
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, None);
    body.extend_from_slice(&tag_frame(0x00, 0x00, &UID_B, None));
    body.extend_from_slice(&terminal_frame(0x00, 0x00));
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert_eq!(reader.inventory().unwrap(), vec![tag(UID_A), tag(UID_B)]);
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_handles_zero_one_two_and_five_tags() {
    let mut expected = Vec::new();
    for count in [0usize, 1, 2, 5] {
        let uids = [UID_A, UID_B, UID_C, UID_A, UID_B];
        let mut body = Vec::new();
        let mut tags = Vec::new();
        for uid in uids.iter().take(count) {
            body.extend_from_slice(&tag_frame(0x00, 0x00, uid, None));
            let candidate = tag(*uid);
            if !tags.contains(&candidate) {
                tags.push(candidate);
            }
        }
        body.extend_from_slice(&terminal_frame(0x00, 0x00));
        let fake = FakeReader::bind();
        let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
        let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
        assert_eq!(reader.inventory().unwrap(), tags, "{count} tags");
        handle.join().unwrap();
        expected.push(tags);
    }
    assert!(
        expected[0].is_empty(),
        "a terminal with no tags is an empty field"
    );
}

#[test]
fn tcp_inventory_deduplicates_repeated_tags() {
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, None);
    body.extend_from_slice(&tag_frame(0x00, 0x00, &UID_A, None));
    body.extend_from_slice(&tag_frame(0x00, 0x00, &UID_B, None));
    body.extend_from_slice(&terminal_frame(0x00, 0x00));
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert_eq!(
        reader.inventory().unwrap(),
        vec![tag(UID_A), tag(UID_B)],
        "the same tag seen twice in one exchange is one tag"
    );
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_reports_the_antenna_profile_it_was_configured_for() {
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, Some(0x02));
    body.extend_from_slice(&terminal_frame(0x00, 0x00));
    let handle = fake.expect_connections(vec![Script::answer(REQUEST_ANTENNA.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, true)).unwrap();
    assert_eq!(
        reader.inventory().unwrap(),
        vec![WireTag {
            uid: UID_A,
            dsfid: 0,
            antenna: Some(0x02),
        }]
    );
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_rejects_a_damaged_checksum() {
    let fake = FakeReader::bind();
    let mut body = response_body(&UID_A);
    body[15] ^= 0xFF;
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(
        matches!(reader.inventory(), Err(DeviceError::Other(_))),
        "a bad checksum is an error, never an empty field"
    );
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_rejects_an_impossible_length() {
    for frame in [vec![0xEC, 0x05, 0x00, 0xFE, 0x01, 0x00], vec![0xEC, 0x00]] {
        let fake = FakeReader::bind();
        let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), frame)]);
        let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
        assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
        handle.join().unwrap();
    }
}

#[test]
fn tcp_inventory_rejects_a_foreign_command() {
    let fake = FakeReader::bind();
    let body = vec![0xEC, 0x07, 0x00, 0xFE, 0x02, 0x00, 0x08, 0x6F];
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_rejects_a_foreign_address() {
    // An addressed request: every answer must come from the reader asked.
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x03, 0x00, &UID_A, None);
    body.extend_from_slice(&terminal_frame(0x09, 0x00));
    let handle = fake.expect_connections(vec![Script::answer(
        vec![0xEC, 0x07, 0x03, 0xFE, 0x01, 0x00, 0x08, 0xDB],
        body,
    )]);
    let mut reader = TcpReader::new(fake.config_with(0x03, 500, false)).unwrap();
    assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
    handle.join().unwrap();

    // A broadcast request accepts whoever answers first, but not a second
    // reader joining the same exchange.
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, None);
    body.extend_from_slice(&terminal_frame(0x07, 0x00));
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_rejects_a_layout_it_does_not_understand() {
    // A network-flagged answer carries two extra bytes the plain profile has no
    // rule for, and an antenna byte the profile was not configured for.
    for body in [
        {
            let mut b = network_layout_frame(0x00, &UID_A);
            b.extend_from_slice(&terminal_frame(0x00, 0x00));
            b
        },
        {
            let mut b = tag_frame(0x00, 0x00, &UID_A, Some(0x01));
            b.extend_from_slice(&terminal_frame(0x00, 0x00));
            b
        },
    ] {
        let fake = FakeReader::bind();
        let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
        let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
        assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
        handle.join().unwrap();
    }
}

#[test]
fn tcp_inventory_rejects_a_command_status_that_is_not_success() {
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, None);
    body[5] = 0x0F;
    let crc = support::frames::crc16_candidate(&body[1..body.len() - 2]);
    let last = body.len() - 2;
    body[last..].copy_from_slice(&crc.to_le_bytes());
    body.extend_from_slice(&terminal_frame(0x00, 0x00));
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_rejects_a_no_tag_terminal_after_a_tag() {
    let fake = FakeReader::bind();
    let mut body = tag_frame(0x00, 0x00, &UID_A, None);
    body.extend_from_slice(&terminal_frame(0x00, 0xE1));
    let handle = fake.expect_connections(vec![Script::answer(REQUEST.to_vec(), body)]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(
        matches!(reader.inventory(), Err(DeviceError::Other(_))),
        "an answer that both found and did not find a tag cannot be trusted"
    );
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_treats_a_no_tag_terminal_as_an_empty_field() {
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::answer(
        REQUEST.to_vec(),
        terminal_frame(0x00, 0xE1),
    )]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(reader.inventory().unwrap().is_empty());
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_rejects_a_connection_dropped_mid_frame() {
    let body = response_body(&UID_A);
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::scripted(
        REQUEST.to_vec(),
        vec![Chunk::now(body[..6].to_vec())],
        false,
    )]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(matches!(reader.inventory(), Err(DeviceError::Disconnected)));
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_requires_a_terminal_frame() {
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::scripted(
        REQUEST.to_vec(),
        vec![Chunk::now(tag_frame(0x00, 0x00, &UID_A, None))],
        true,
    )]);
    let mut reader = TcpReader::new(fake.config(300, false)).unwrap();
    let started = std::time::Instant::now();
    assert!(
        matches!(reader.inventory(), Err(DeviceError::Other(_))),
        "a tag with no end-of-inventory frame is not a finished answer"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2_000),
        "the deadline must end the exchange, not the fixture closing"
    );
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_stops_a_peer_that_trickles_past_the_deadline() {
    let body = response_body(&UID_A);
    let mut chunks = Vec::new();
    for byte in &body {
        chunks.push(Chunk {
            bytes: vec![*byte],
            delay: Duration::from_millis(20),
        });
    }
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::scripted(REQUEST.to_vec(), chunks, false)]);
    let mut reader = TcpReader::new(fake.config(250, false)).unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(reader.inventory(), Err(DeviceError::Other(_))));
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "a trickle cannot keep the call alive past its deadline"
    );
    handle.join().unwrap();
}

#[test]
fn tcp_inventory_reconnects_on_a_later_call() {
    let fake = FakeReader::bind();
    let mut damaged = response_body(&UID_A);
    damaged[15] ^= 0xFF;
    let handle = fake.expect_connections(vec![
        Script::answer(REQUEST.to_vec(), damaged),
        Script::answer(REQUEST.to_vec(), response_body(&UID_B)),
    ]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(reader.inventory().is_err());
    assert_eq!(
        reader.inventory().unwrap(),
        vec![tag(UID_B)],
        "a failed exchange must not be retried, but the next call may start again"
    );
    assert_eq!(handle.join().unwrap().len(), 2);
}

#[test]
fn tcp_inventory_fails_rather_than_retrying_a_lost_connection() {
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::scripted(
        REQUEST.to_vec(),
        vec![Chunk::now(vec![0xEC])],
        false,
    )]);
    let mut reader = TcpReader::new(fake.config(500, false)).unwrap();
    assert!(matches!(reader.inventory(), Err(DeviceError::Disconnected)));
    assert_eq!(
        handle.join().unwrap().len(),
        1,
        "one call is one exchange: the reader does not ask again on its own"
    );
}

#[test]
fn cancel_ends_an_exchange_that_is_waiting() {
    let fake = FakeReader::bind();
    let handle = fake.expect_connections(vec![Script::scripted(
        REQUEST.to_vec(),
        vec![Chunk::now(vec![0xEC])],
        true,
    )]);
    let reader = TcpReader::new(fake.config(9_000, false)).unwrap();
    let control = reader.stop_control();
    let started = std::time::Instant::now();
    let runner = std::thread::spawn(move || {
        let mut reader = reader;
        reader.inventory()
    });
    std::thread::sleep(Duration::from_millis(150));
    control.stop();
    let outcome = runner.join().unwrap();
    assert!(outcome.is_err());
    assert!(
        started.elapsed() < Duration::from_millis(2_000),
        "stopping the reader must end the wait instead of letting it run to its deadline"
    );
    handle.join().unwrap();
}

#[test]
fn stopped_tcp_reader_never_connects_on_later_inventory() {
    let fake = FakeReader::bind();
    let mut reader = TcpReader::new(fake.config(9_000, false)).unwrap();
    reader.stop_control().stop();
    let started = std::time::Instant::now();
    assert!(reader.inventory().is_err());
    assert!(!reader.is_connected());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "stop forbids a new socket"
    );
}

#[test]
fn stop_during_connect_publication_cannot_leave_a_live_socket() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(address) => address,
        _ => unreachable!(),
    };
    let (accepted, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 8];
        let _ = std::io::Read::read_exact(&mut socket, &mut request);
    });
    let reader = TcpReader::new(rfidex_hardware::config::HardwareConfig::EcV19PlainTcp {
        address,
        bus_address: 0xFF,
        antenna_byte: false,
        timeout_ms: 9_000,
    })
    .unwrap();
    let stop = reader.stop_control();
    let worker = std::thread::spawn(move || {
        let mut reader = reader;
        let answer = reader.inventory();
        (answer, reader.is_connected())
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = std::time::Instant::now();
    stop.stop();
    let (answer, connected) = worker.join().unwrap();
    assert!(answer.is_err());
    assert!(!connected);
    assert!(started.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}

//! Fixtures shared by the hardware integration tests. Nothing here is compiled
//! into the app.
//!
//! Several test targets include this module and each uses a different part of
//! it, so the unused-item lint is off here rather than per target.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use rfidex_hardware::config::HardwareConfig;

/// Synthetic `0xEC` frames.
///
/// These are **candidate** vectors: the vendor guide supplies the seed and the
/// byte order but not the polynomial, so every byte here is computed from the
/// bridge's documented reflected `0xA001` implementation. `pinned_vectors`
/// below fixes the exact bytes so a change to the CRC cannot silently agree
/// with itself.
pub mod frames {
    pub const SOF: u8 = 0xEC;

    pub fn crc16_candidate(data: &[u8]) -> u16 {
        let mut crc = 0xEEEE_u16;
        for byte in data {
            crc ^= u16::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xA001
                } else {
                    crc >> 1
                };
            }
        }
        crc
    }

    /// `body` is the frame after the start byte and before the checksum; its
    /// first byte is the length field.
    pub fn frame(body: &[u8]) -> Vec<u8> {
        let crc = crc16_candidate(body);
        let mut out = vec![SOF];
        out.extend_from_slice(body);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn tag_frame(address: u8, dsfid: u8, uid: &[u8; 8], antenna: Option<u8>) -> Vec<u8> {
        let mut body = vec![0u8; 0];
        body.push(if antenna.is_some() { 0x11 } else { 0x10 });
        body.push(address);
        body.extend_from_slice(&[0xFE, 0x01, 0x00, dsfid]);
        body.extend_from_slice(uid);
        if let Some(ant) = antenna {
            body.push(ant);
        }
        frame(&body)
    }

    pub fn terminal_frame(address: u8, status: u8) -> Vec<u8> {
        frame(&[0x07, address, 0xFE, 0x01, status])
    }

    /// A network-flagged answer: two extra bytes between status and the tag.
    pub fn network_layout_frame(address: u8, uid: &[u8; 8]) -> Vec<u8> {
        let mut body = vec![0x12, address, 0xFE, 0x01, 0x00, 0xAA, 0xBB, 0x00];
        body.extend_from_slice(uid);
        body.extend_from_slice(&[0x00, 0x00]);
        frame(&body)
    }

    #[test]
    fn pinned_vectors() {
        // Computed independently of this crate and pasted here on purpose: the
        // guide's `cal_crc16_ext(0xeeee, &DATA[1], DATA[1]-2)` example.
        assert_eq!(
            frame(&[0x07, 0xFF, 0xFE, 0x01, 0x00]),
            [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x00, 0x38, 0x8B]
        );
        assert_eq!(
            terminal_frame(0x00, 0x00),
            [0xEC, 0x07, 0x00, 0xFE, 0x01, 0x00, 0x08, 0x9F]
        );
        assert_eq!(
            tag_frame(
                0x00,
                0x00,
                &[0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34],
                None
            ),
            [
                0xEC, 0x10, 0x00, 0xFE, 0x01, 0x00, 0x00, 0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12,
                0x34, 0x0F, 0x02
            ]
        );
        assert_eq!(
            tag_frame(
                0x00,
                0x00,
                &[0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34],
                Some(0x01)
            ),
            [
                0xEC, 0x11, 0x00, 0xFE, 0x01, 0x00, 0x00, 0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12,
                0x34, 0x01, 0x42, 0xC4
            ]
        );
    }
}

pub const UID_A: [u8; 8] = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34];
pub const UID_B: [u8; 8] = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x56, 0x78];
pub const UID_C: [u8; 8] = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x9A, 0xBC];

/// One write to the client, optionally after a delay.
pub struct Chunk {
    pub bytes: Vec<u8>,
    pub delay: Duration,
}

impl Chunk {
    pub fn now(bytes: Vec<u8>) -> Chunk {
        Chunk {
            bytes,
            delay: Duration::ZERO,
        }
    }
}

/// What the fixture does with one accepted connection.
pub struct Script {
    pub expect: Option<Vec<u8>>,
    pub chunks: Vec<Chunk>,
    /// Keep the socket open after the last chunk instead of closing it.
    pub hold_open: bool,
}

impl Script {
    /// Read the request, then send everything in one write.
    pub fn answer(request: Vec<u8>, response: Vec<u8>) -> Script {
        Script {
            expect: Some(request),
            chunks: vec![Chunk::now(response)],
            hold_open: false,
        }
    }

    pub fn scripted(request: Vec<u8>, chunks: Vec<Chunk>, hold_open: bool) -> Script {
        Script {
            expect: Some(request),
            chunks,
            hold_open,
        }
    }
}

/// A loopback-only reader stand-in. It never binds anything but `127.0.0.1`
/// on a port the kernel chooses.
#[derive(Clone)]
pub struct FakeReader {
    listener: Arc<TcpListener>,
    address: SocketAddrV4,
}

impl FakeReader {
    pub fn bind() -> FakeReader {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let SocketAddr::V4(address) = listener.local_addr().expect("a bound address") else {
            panic!("the fixture binds IPv4 loopback");
        };
        FakeReader {
            listener: Arc::new(listener),
            address,
        }
    }

    pub fn config(&self, timeout_ms: u32, antenna_byte: bool) -> HardwareConfig {
        self.config_with(0xFF, timeout_ms, antenna_byte)
    }

    pub fn config_with(
        &self,
        bus_address: u8,
        timeout_ms: u32,
        antenna_byte: bool,
    ) -> HardwareConfig {
        HardwareConfig::EcV19PlainTcp {
            address: self.address,
            bus_address,
            antenna_byte,
            timeout_ms,
        }
    }

    /// Serve one connection per script, in order, and report the requests the
    /// fixture actually received.
    pub fn expect_connections(&self, scripts: Vec<Script>) -> JoinHandle<Vec<Vec<u8>>> {
        let listener = self.listener.clone();
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            for script in scripts {
                let (mut stream, _) = listener.accept().expect("a client connects");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("a read timeout");
                let wanted = script.expect.as_ref().map(Vec::len).unwrap_or(8);
                let request = read_request(&mut stream, wanted);
                match (&script.expect, request) {
                    (Some(expected), Some(request)) => {
                        assert_eq!(
                            &request, expected,
                            "the fixture received a different request"
                        );
                        seen.push(request);
                    }
                    (None, Some(request)) => seen.push(request),
                    (_, None) => panic!("the client never sent a complete request"),
                }
                for chunk in &script.chunks {
                    if !chunk.delay.is_zero() {
                        std::thread::sleep(chunk.delay);
                    }
                    if stream.write_all(&chunk.bytes).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
                if script.hold_open {
                    std::thread::sleep(Duration::from_millis(600));
                }
                let _ = stream.shutdown(Shutdown::Both);
            }
            seen
        })
    }
}

fn read_request(stream: &mut TcpStream, wanted: usize) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; wanted];
    let mut filled = 0;
    while filled < wanted {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    (filled == wanted).then_some(buffer)
}

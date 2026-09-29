//! A test-only stand-in for the device helper.
//!
//! It speaks the real parent/child protocol and nothing else: no vendor library
//! is loaded and no hardware is needed. It exists so the parent's deadline,
//! cancellation, reaping and malformed-reply handling can be tested with a
//! child that stalls, crashes or lies on purpose.
//!
//! It is built only with the `test-host` feature and only `tests/process.rs`
//! selects its behaviour, from the process environment. The desktop executable
//! and the developer host never read those variables, so no fake behaviour is
//! reachable through the app's own IPC.

use std::ffi::OsString;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use rfidex_hardware::config::{HardwareConfig, SdkConnection};
use rfidex_hardware::wire::{
    read_request, write_frame, Deadline, Operation, Reply, Response, WireError, WireTag,
};

const EXIT_BAD_ARGUMENTS: i32 = 2;

const BLOCK_SIZE: usize = 4;
const BLOCKS: usize = 28;
const UID_A: [u8; 8] = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34];
const UID_B: [u8; 8] = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x56, 0x78];

/// The sticker the fake reader is holding. It is only as real as the adapter
/// contract needs: a block map that reads back what was written.
struct Sticker {
    memory: Vec<u8>,
}

impl Sticker {
    fn new() -> Sticker {
        Sticker {
            memory: vec![0u8; BLOCKS * BLOCK_SIZE],
        }
    }

    fn read(&self, start: u8, count: u8) -> Option<Vec<u8>> {
        let from = usize::from(start) * BLOCK_SIZE;
        let to = from + usize::from(count) * BLOCK_SIZE;
        self.memory.get(from..to).map(<[u8]>::to_vec)
    }

    fn write(&mut self, start: u8, data: &[u8], tear_at: Option<usize>) -> bool {
        let from = usize::from(start) * BLOCK_SIZE;
        let to = from + data.len();
        if to > self.memory.len() {
            return false;
        }
        let written = tear_at.unwrap_or(data.len()).min(data.len());
        self.memory[from..from + written].copy_from_slice(&data[..written]);
        true
    }
}

/// Long enough that only the parent's own stop can end a stall.
fn stall_forever() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn log(line: &str) {
    let Ok(path) = std::env::var("RFIDEX_FAKE_HOST_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn scenario() -> String {
    std::env::var("RFIDEX_FAKE_HOST").unwrap_or_default()
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let [switch, port, token] = args.as_slice() else {
        std::process::exit(EXIT_BAD_ARGUMENTS);
    };
    if switch.to_str() != Some("--rfid-device-host") {
        std::process::exit(EXIT_BAD_ARGUMENTS);
    }
    let Some(port) = port.to_str().and_then(|text| text.parse::<u16>().ok()) else {
        std::process::exit(EXIT_BAD_ARGUMENTS);
    };
    let Some(token) = token.to_str() else {
        std::process::exit(EXIT_BAD_ARGUMENTS);
    };

    let scenario = scenario();
    if scenario == "exit_before_connect" {
        std::process::exit(0);
    }

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(stream) = TcpStream::connect(address) else {
        std::process::exit(3);
    };
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_nodelay(true);
    let mut stream = stream;

    if write_frame(&mut stream, &token.to_string(), &Deadline::started(5_000)).is_err() {
        std::process::exit(3);
    }

    serve(&mut stream, &scenario);
    std::process::exit(0);
}

/// The model the operator selected, which the child reports back as the model
/// it was told to use. Nothing is parsed out of the raw answer.
fn configured_model(operation: &Operation) -> Option<String> {
    let Operation::Open { config } = operation else {
        return None;
    };
    let HardwareConfig::EcrfidSdk { connection, .. } = config else {
        return None;
    };
    Some(match connection {
        SdkConnection::Hid { model, .. }
        | SdkConnection::Com { model, .. }
        | SdkConnection::Net { model, .. } => model.clone(),
    })
}

fn serve(stream: &mut TcpStream, scenario: &str) {
    let mut opens = 0usize;
    let mut sticker = Sticker::new();
    let mut model: Option<String> = None;
    while let Ok(request) = read_request(stream, &Deadline::started(u32::MAX)) {
        log(&format!("op={}", name(&request.operation)));
        match scenario {
            "die_after_open" if opens == 1 => {
                // Gone without answering: the parent must notice the silence,
                // not accept a reply that was never produced.
                std::process::exit(0);
            }
            "die_before_open_reply" if opens == 0 => std::process::exit(0),
            "oversized" => {
                // A length prefix no frame can honour. Nothing follows it.
                let prefix = 65_537u32.to_be_bytes();
                let _ = stream.write_all(&prefix);
                let _ = stream.flush();
                stall_forever();
            }
            "stall_inventory" if matches!(request.operation, Operation::Inventory) => {
                stall_forever()
            }
            "stall_write" if matches!(request.operation, Operation::Write { .. }) => {
                stall_forever()
            }
            "stall_close" if matches!(request.operation, Operation::Close) => stall_forever(),
            "stall_open_reply" if opens == 0 => stall_forever(),
            _ => {}
        }

        if let Some(selected) = configured_model(&request.operation) {
            model = Some(selected);
        }
        let result = answer(&request.operation, scenario, &mut sticker, model.as_deref());
        let reply = Reply {
            id: if scenario == "wrong_id" {
                request.id + 1
            } else {
                request.id
            },
            result,
        };
        if scenario == "late_partial_reply" && matches!(request.operation, Operation::Inventory) {
            let bytes = rfidex_hardware::wire::encode_frame(&reply).unwrap();
            let split = bytes.len() - 1;
            if stream.write_all(&bytes[..split]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(600));
            let _ = stream.write_all(&bytes[split..]);
            return;
        }
        if write_frame(stream, &reply, &Deadline::started(5_000)).is_err() {
            return;
        }
        if matches!(request.operation, Operation::Open { .. }) {
            opens += 1;
            sticker = Sticker::new();
        }
        if matches!(request.operation, Operation::Close) {
            return;
        }
    }
}

fn name(operation: &Operation) -> &'static str {
    match operation {
        Operation::Open { .. } => "open",
        Operation::Info => "info",
        Operation::Inventory => "inventory",
        Operation::Memory { .. } => "memory",
        Operation::Read { .. } => "read",
        Operation::Write { .. } => "write",
        Operation::RawRecords => "raw_records",
        Operation::LibraryRecords { .. } => "library_records",
        Operation::LibraryAlarm { .. } => "library_alarm",
        Operation::Close => "close",
    }
}

fn answer(
    operation: &Operation,
    scenario: &str,
    sticker: &mut Sticker,
    model: Option<&str>,
) -> Result<Response, WireError> {
    if scenario == "fail_everything" {
        return Err(WireError::Disconnected);
    }
    if scenario == "malformed_response" {
        return Err(WireError::BadResponse);
    }
    // The fake is not a library gate: gates fall back to live inventory.
    if matches!(operation, Operation::LibraryRecords { .. }) {
        return Err(WireError::Unsupported);
    }
    Ok(match operation {
        // A write is answered further down, by the arm that also touches the
        // sticker's memory.
        Operation::Open { .. } | Operation::Close | Operation::LibraryAlarm { .. } => {
            Response::Unit
        }
        Operation::Info => Response::Info {
            model: model.map(str::to_string),
            firmware: None,
            raw: vec![0xEC, 0x1E],
        },
        Operation::Inventory => Response::Tags {
            tags: if scenario == "two_tags" {
                vec![tag(UID_A), tag(UID_B)]
            } else {
                vec![tag(UID_A)]
            },
        },
        Operation::Memory { .. } => {
            if scenario == "memory_error" {
                return Err(WireError::BadResponse);
            }
            Response::Memory {
                block_size: BLOCK_SIZE,
                block_count: BLOCKS,
                raw: vec![0; 32],
            }
        }
        Operation::Read { start, count, .. } => {
            if scenario == "read_error" {
                return Err(WireError::NoTag);
            }
            let data = sticker.read(*start, *count).ok_or(WireError::OutOfRange)?;
            // A readback that does not match what was written, which is how a
            // torn or corrupted sticker is caught.
            if scenario == "read_corrupt" {
                Response::Bytes {
                    data: data.iter().map(|byte| byte ^ 0xFF).collect(),
                }
            } else {
                Response::Bytes { data }
            }
        }
        Operation::Write { start, data, .. } => {
            if scenario == "write_error" {
                return Err(WireError::WriteUnsupported);
            }
            let tear = (scenario == "write_tear").then_some(data.len() / 2);
            if !sticker.write(*start, data, tear) {
                return Err(WireError::OutOfRange);
            }
            Response::Unit
        }
        Operation::RawRecords | Operation::LibraryRecords { .. } => {
            Response::Records { raw: Vec::new() }
        }
    })
}

fn tag(uid: [u8; 8]) -> WireTag {
    WireTag {
        uid,
        dsfid: 0x00,
        antenna: None,
    }
}

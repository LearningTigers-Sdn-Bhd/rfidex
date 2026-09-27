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

use rfidex_hardware::wire::{
    read_request, write_frame, Deadline, Operation, Reply, Response, WireError, WireTag,
};

const EXIT_BAD_ARGUMENTS: i32 = 2;

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

fn serve(stream: &mut TcpStream, scenario: &str) {
    let mut opens = 0usize;
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

        let result = answer(&request.operation, scenario);
        let reply = Reply {
            id: if scenario == "wrong_id" {
                request.id + 1
            } else {
                request.id
            },
            result,
        };
        if write_frame(stream, &reply, &Deadline::started(5_000)).is_err() {
            return;
        }
        if matches!(request.operation, Operation::Open { .. }) {
            opens += 1;
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
        Operation::Close => "close",
    }
}

fn answer(operation: &Operation, scenario: &str) -> Result<Response, WireError> {
    if scenario == "fail_everything" {
        return Err(WireError::Disconnected);
    }
    Ok(match operation {
        Operation::Open { .. } | Operation::Close | Operation::Write { .. } => Response::Unit,
        Operation::Info => Response::Info {
            model: Some("FAKE".into()),
            firmware: None,
            raw: vec![0xEC, 0x1E],
        },
        Operation::Inventory => Response::Tags {
            tags: vec![WireTag {
                uid: [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34],
                dsfid: 0x00,
                antenna: None,
            }],
        },
        Operation::Memory { .. } => Response::Memory {
            block_size: 4,
            block_count: 28,
            raw: vec![0; 32],
        },
        Operation::Read { count, .. } => Response::Bytes {
            data: vec![0xAB; usize::from(*count) * 4],
        },
        Operation::RawRecords => Response::Records { raw: Vec::new() },
    })
}

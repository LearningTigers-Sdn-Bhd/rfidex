//! The isolated child: one SDK context, one thread, one request at a time.
//!
//! The desktop executable starts itself in this mode before Tauri, the updater
//! or the station database are touched, so a native crash or a wedged vendor
//! call cannot take the app with it. The child is told about a reader and
//! nothing else: no event key, no ticket, no database path.
//!
//! Two modes exist and neither is selectable once the process is running:
//!
//! * the default app mode, which the desktop executable and the runtime use;
//! * a commissioning mode used only by the developer binary for raw record
//!   capture and deliberate disposable-tag writes.

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use crate::config::HardwareConfig;
use crate::sdk::{EnumerationKind, SdkReader};
use crate::wire::{read_request, write_frame, Deadline, Operation, Reply, Response, WireError};

/// The switch that puts an executable into child mode.
pub const CHILD_SWITCH: &str = "--rfid-device-host";

const EXIT_OK: i32 = 0;
const EXIT_BAD_ARGUMENTS: i32 = 2;
const EXIT_SESSION_FAILED: i32 = 3;

/// How long the child waits to write one reply. The parent owns the deadline
/// for a request; this only stops a wedged parent holding the child forever.
const REPLY_WINDOW_MS: u32 = 5_000;

/// How long the child takes to reach the parent's loopback listener.
const HANDSHAKE_WINDOW_MS: u32 = 5_000;

/// The child waits for its parent indefinitely: the parent owns every request
/// deadline and kills the child when it stops caring.
const WAIT_FOR_PARENT_MS: u32 = u32::MAX;

pub const COMMISSIONING: &str = "commissioning";
const ENUMERATE: &str = "enumerate";

/// Parse the child's arguments and run the session they describe.
///
/// `None` means this is not a child invocation at all, and the caller must go
/// on to start the app. A malformed child invocation returns a nonzero code and
/// must never fall through: a second GUI window from a typo would be worse than
/// an exit.
pub fn run_host_from_args(args: &[OsString]) -> Option<i32> {
    let parsed = ChildMode::parse(args)?;
    Some(match parsed {
        Ok(mode) => match mode.run() {
            Ok(()) => EXIT_OK,
            Err(_) => EXIT_SESSION_FAILED,
        },
        Err(BadArguments) => EXIT_BAD_ARGUMENTS,
    })
}

/// A marker so a malformed *child* invocation is not confused with "not a child
/// invocation".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BadArguments;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChildMode {
    Session {
        port: u16,
        token: String,
        commissioning: bool,
    },
    Enumerate {
        port: u16,
        token: String,
        kind: EnumerationKind,
        dll_path: PathBuf,
    },
}

impl ChildMode {
    fn parse(args: &[OsString]) -> Option<Result<ChildMode, BadArguments>> {
        let (first, rest) = args.split_first()?;
        if first.to_str() != Some(CHILD_SWITCH) {
            return None;
        }
        Some(match rest {
            [port, token] => ChildMode::session(port, token, false),
            [port, token, mode] if mode.to_str() == Some(COMMISSIONING) => {
                ChildMode::session(port, token, true)
            }
            [port, token, mode, kind, dll_path] if mode.to_str() == Some(ENUMERATE) => {
                match ChildMode::base(port, token) {
                    Err(bad) => Err(bad),
                    Ok((port, token)) => match kind.to_str() {
                        Some("hid") => Ok(ChildMode::Enumerate {
                            port,
                            token,
                            kind: EnumerationKind::Hid,
                            dll_path: PathBuf::from(dll_path),
                        }),
                        Some("com") => Ok(ChildMode::Enumerate {
                            port,
                            token,
                            kind: EnumerationKind::Com,
                            dll_path: PathBuf::from(dll_path),
                        }),
                        Some("net") => Ok(ChildMode::Enumerate {
                            port,
                            token,
                            kind: EnumerationKind::Net,
                            dll_path: PathBuf::from(dll_path),
                        }),
                        _ => Err(BadArguments),
                    },
                }
            }
            _ => Err(BadArguments),
        })
    }

    fn session(
        port: &OsString,
        token: &OsString,
        commissioning: bool,
    ) -> Result<ChildMode, BadArguments> {
        let (port, token) = ChildMode::base(port, token)?;
        Ok(ChildMode::Session {
            port,
            token,
            commissioning,
        })
    }

    fn base(port: &OsString, token: &OsString) -> Result<(u16, String), BadArguments> {
        // Port zero is never a listening port, and a token the parent did not
        // mint cannot authenticate anything.
        let port: u16 = port
            .to_str()
            .and_then(|text| text.parse().ok())
            .filter(|port| *port != 0)
            .ok_or(BadArguments)?;
        let token = token
            .to_str()
            .filter(|token| !token.is_empty())
            .ok_or(BadArguments)?
            .to_string();
        Ok((port, token))
    }

    fn port(&self) -> u16 {
        match self {
            ChildMode::Session { port, .. } | ChildMode::Enumerate { port, .. } => *port,
        }
    }

    fn token(&self) -> &str {
        match self {
            ChildMode::Session { token, .. } | ChildMode::Enumerate { token, .. } => token,
        }
    }

    fn run(self) -> Result<(), WireError> {
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port())),
            Duration::from_millis(u64::from(HANDSHAKE_WINDOW_MS)),
        )
        .map_err(|_| WireError::Disconnected)?;
        stream
            .set_nodelay(true)
            .map_err(|_| WireError::Disconnected)?;
        stream
            .set_read_timeout(None)
            .map_err(|_| WireError::Disconnected)?;
        let mut stream = stream;

        // The token travels first and alone: the parent never reads a request
        // from a connection that has not proved it is the child it started.
        write_frame(
            &mut stream,
            &self.token().to_string(),
            &Deadline::started(HANDSHAKE_WINDOW_MS),
        )?;

        match self {
            ChildMode::Session { commissioning, .. } => serve_session(stream, commissioning),
            ChildMode::Enumerate { kind, dll_path, .. } => {
                let values = SdkReader::enumerate(&dll_path, kind)?;
                write_frame(
                    &mut stream,
                    &Reply {
                        id: 0,
                        result: Ok(Response::Strings { values }),
                    },
                    &Deadline::started(REPLY_WINDOW_MS),
                )
            }
        }
    }
}

fn serve_session(mut stream: TcpStream, commissioning: bool) -> Result<(), WireError> {
    let mut device: Option<SdkReader> = None;
    let mut config: Option<HardwareConfig> = None;
    // A closed or unreadable channel is how a session ends: the parent owns the
    // decision to stop, and the child goes with it.
    while let Ok(request) = read_request(&mut stream, &Deadline::started(WAIT_FOR_PARENT_MS)) {
        let result = dispatch(&mut device, &mut config, commissioning, request.operation);
        if write_frame(
            &mut stream,
            &Reply {
                id: request.id,
                result,
            },
            &Deadline::started(REPLY_WINDOW_MS),
        )
        .is_err()
        {
            break;
        }
    }
    // A context is closed exactly once on a normal path. A child the parent
    // killed skips this, and the parent reports that rather than pretending the
    // reader was released.
    if let Some(mut reader) = device.take() {
        let _ = reader.close();
    }
    Ok(())
}

/// The whole policy of what a helper may be asked to do.
///
/// Writes need either a profile whose physical acceptance tests passed or the
/// commissioning host; stored records are captured only from the commissioning
/// host, because the app's own polling keeps no raw gate records and never asks
/// a reader to delete a batch.
fn dispatch(
    device: &mut Option<SdkReader>,
    config: &mut Option<HardwareConfig>,
    commissioning: bool,
    operation: Operation,
) -> Result<Response, WireError> {
    match operation {
        Operation::Open { config: requested } => {
            // One context per child: a second Open replaces the first instead
            // of leaking it.
            *device = None;
            *config = None;
            let opened = SdkReader::open(&requested)?;
            *device = Some(opened);
            *config = Some(requested);
            Ok(Response::Unit)
        }
        Operation::Close => {
            if let Some(mut reader) = device.take() {
                reader.close()?;
            }
            *config = None;
            Ok(Response::Unit)
        }
        Operation::Info => with_reader(device, |reader| reader.info()),
        Operation::Inventory => with_reader(device, |reader| reader.inventory()),
        Operation::Memory { uid } => with_reader(device, |reader| reader.memory(&uid)),
        Operation::Read { uid, start, count } => {
            with_reader(device, |reader| reader.read(&uid, start, count))
        }
        Operation::Write { uid, start, data } => {
            if !commissioning && !config.as_ref().is_some_and(HardwareConfig::write_verified) {
                return Err(WireError::WriteUnsupported);
            }
            with_reader(device, |reader| reader.write(&uid, start, &data))
        }
        Operation::RawRecords => {
            if !commissioning {
                return Err(WireError::Unsupported);
            }
            with_reader(device, |reader| reader.raw_records())
        }
    }
}

fn with_reader<T>(
    device: &Option<SdkReader>,
    run: impl FnOnce(&SdkReader) -> Result<T, WireError>,
) -> Result<T, WireError> {
    match device {
        Some(reader) => run(reader),
        None => Err(WireError::Disconnected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_our_switch_starts_child_mode() {
        assert!(ChildMode::parse(&args(&[])).is_none());
        assert!(ChildMode::parse(&args(&["--help"])).is_none());
        assert!(ChildMode::parse(&args(&["rfidex", "--rfid-device-host"])).is_none());
    }

    #[test]
    fn a_well_formed_child_invocation_is_accepted() {
        assert_eq!(
            ChildMode::parse(&args(&["--rfid-device-host", "51000", "token"])).unwrap(),
            Ok(ChildMode::Session {
                port: 51000,
                token: "token".into(),
                commissioning: false,
            })
        );
        assert_eq!(
            ChildMode::parse(&args(&[
                "--rfid-device-host",
                "51000",
                "token",
                "commissioning"
            ]))
            .unwrap(),
            Ok(ChildMode::Session {
                port: 51000,
                token: "token".into(),
                commissioning: true,
            })
        );
        assert_eq!(
            ChildMode::parse(&args(&[
                "--rfid-device-host",
                "51000",
                "token",
                "enumerate",
                "hid",
                "C:\\rfidex\\ECRFID.dll"
            ]))
            .unwrap(),
            Ok(ChildMode::Enumerate {
                port: 51000,
                token: "token".into(),
                kind: EnumerationKind::Hid,
                dll_path: PathBuf::from("C:\\rfidex\\ECRFID.dll"),
            })
        );
    }

    #[test]
    fn a_malformed_child_invocation_is_rejected_and_never_falls_through() {
        for bad in [
            vec!["--rfid-device-host"],
            vec!["--rfid-device-host", "51000"],
            vec!["--rfid-device-host", "not-a-port", "token"],
            vec!["--rfid-device-host", "0", "token"],
            vec!["--rfid-device-host", "51000", ""],
            vec!["--rfid-device-host", "51000", "token", "enumerate", "hid"],
            vec![
                "--rfid-device-host",
                "51000",
                "token",
                "enumerate",
                "nfc",
                "/x.dll",
            ],
            vec!["--rfid-device-host", "51000", "token", "watch"],
            vec![
                "--rfid-device-host",
                "51000",
                "token",
                "commissioning",
                "extra",
            ],
        ] {
            let parsed = ChildMode::parse(&args(&bad)).unwrap_or_else(|| {
                panic!("{bad:?} must be recognised as a child invocation so it cannot start a GUI")
            });
            assert!(parsed.is_err(), "{bad:?} must be rejected");
        }
    }
}

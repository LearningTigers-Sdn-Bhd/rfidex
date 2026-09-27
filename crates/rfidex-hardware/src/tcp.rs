//! Bounded socket transport for the legacy `0xEC` candidate profile.
//!
//! Every wait is a slice of one absolute deadline fixed when the call started,
//! so a peer that trickles a byte at a time cannot keep the call alive. A
//! failed exchange drops the connection: the bytes still in the stream belong
//! to an answer nobody finished reading, and letting the next request see them
//! would be worse than reconnecting.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, SocketAddrV4, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rfidex_core::device::{DeviceError, DeviceInfo, DeviceResult};

use crate::config::HardwareConfig;
use crate::ec::{self, EcError, FrameDecoder, Inventory};
use crate::process::StopControl;
use crate::wire::{Deadline, WireTag};

/// Read buffer size. Frames are at most 256 bytes, so this holds several.
const CHUNK: usize = 512;

/// The longest one socket read may wait before the stop flag is checked again.
/// This bounds cancellation latency on platforms where `shutdown` does not
/// interrupt a blocking read.
const READ_POLL: Duration = Duration::from_millis(100);

const NOT_TCP: &str = "this profile is not the plain TCP reader";
const TIMED_OUT: &str = "the reader did not answer in time";

pub struct TcpReader {
    config: HardwareConfig,
    connection: Option<Connection>,
    /// A second handle to the live socket, kept apart from the transaction so
    /// shutdown can interrupt a read without taking the adapter lock.
    shutdown: Arc<Mutex<Option<TcpStream>>>,
    stopped: Arc<AtomicBool>,
}

struct Connection {
    stream: TcpStream,
    decoder: FrameDecoder,
}

impl TcpReader {
    /// Validates the profile. Nothing is connected here: a reader that is
    /// switched off must not stop the app from starting.
    pub fn new(config: HardwareConfig) -> DeviceResult<TcpReader> {
        if !matches!(config, HardwareConfig::EcV19PlainTcp { .. }) {
            return Err(DeviceError::Other(NOT_TCP.to_string()));
        }
        config.validate().map_err(DeviceError::Other)?;
        Ok(TcpReader {
            config,
            connection: None,
            shutdown: Arc::new(Mutex::new(None)),
            stopped: Arc::new(AtomicBool::new(false)),
        })
    }

    /// What this profile knows about the reader. It knows the address it was
    /// told to use and nothing else; a model or firmware string here would be
    /// invented, so both stay empty until a real answer supplies them.
    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            adapter: "ec-v19-plain-tcp".to_string(),
            model: None,
            firmware: None,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Usable before the first connection: the control shuts down whatever
    /// socket exists when it is called.
    pub fn stop_control(&self) -> Arc<StopControl> {
        let handle = self.shutdown.clone();
        let stopped = self.stopped.clone();
        StopControl::new(move || {
            let stream = {
                let mut socket = handle.lock().unwrap_or_else(|e| e.into_inner());
                stopped.store(true, Ordering::SeqCst);
                socket.take()
            };
            if let Some(stream) = stream {
                let _ = stream.shutdown(Shutdown::Both);
            }
        })
    }

    pub fn disconnect(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _ = connection.stream.shutdown(Shutdown::Both);
        }
        *self.shutdown.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// One inventory exchange. Connects if it is not connected yet, and never
    /// repeats a failed exchange: the next call is the next call.
    pub fn inventory(&mut self) -> DeviceResult<Vec<WireTag>> {
        let (address, bus_address, antenna_byte, timeout_ms) = match &self.config {
            HardwareConfig::EcV19PlainTcp {
                address,
                bus_address,
                antenna_byte,
                timeout_ms,
            } => (*address, *bus_address, *antenna_byte, *timeout_ms),
            _ => return Err(DeviceError::Other(NOT_TCP.to_string())),
        };
        let deadline = Deadline::started(timeout_ms);
        let request = ec::inventory_request(bus_address, antenna_byte).map_err(protocol)?;
        let mut inventory = Inventory::new(bus_address, antenna_byte);
        let outcome = self.exchange(address, &request, &mut inventory, &deadline);
        if outcome.is_err() || self.stopped.load(Ordering::SeqCst) {
            self.disconnect();
        }
        if self.stopped.load(Ordering::SeqCst) {
            return Err(DeviceError::Disconnected);
        }
        outcome.map(|()| inventory.tags().to_vec())
    }

    fn connect(&mut self, address: SocketAddrV4, deadline: &Deadline) -> DeviceResult<()> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(DeviceError::Disconnected);
        }
        if self.connection.is_some() {
            return Ok(());
        }
        let stream = TcpStream::connect_timeout(&SocketAddr::V4(address), slice(deadline)?)
            .map_err(|_| DeviceError::Disconnected)?;
        let _ = stream.set_nodelay(true);
        let handle = stream.try_clone().map_err(|_| DeviceError::Disconnected)?;
        let mut slot = self.shutdown.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped.load(Ordering::SeqCst) {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(DeviceError::Disconnected);
        }
        *slot = Some(handle);
        self.connection = Some(Connection {
            stream,
            decoder: FrameDecoder::new(),
        });
        drop(slot);
        Ok(())
    }

    fn exchange(
        &mut self,
        address: SocketAddrV4,
        request: &[u8],
        inventory: &mut Inventory,
        deadline: &Deadline,
    ) -> DeviceResult<()> {
        self.connect(address, deadline)?;
        let connection = self.connection.as_mut().ok_or(DeviceError::Disconnected)?;
        connection.decoder.clear();
        arm(connection, deadline)?;
        connection
            .stream
            .write_all(request)
            .map_err(|_| DeviceError::Disconnected)?;
        deadline.check().map_err(|_| timed_out())?;
        connection
            .stream
            .flush()
            .map_err(|_| DeviceError::Disconnected)?;

        deadline.check().map_err(|_| timed_out())?;
        let mut buffer = [0u8; CHUNK];
        loop {
            // Shutting down a cloned socket does not interrupt a blocking read
            // on Windows, so cancellation cannot rely on the stop handle: the
            // read waits in short slices and the stop flag is checked on every
            // wake, on every platform.
            if self.stopped.load(Ordering::SeqCst) {
                return Err(DeviceError::Disconnected);
            }
            while let Some(frame) = connection.decoder.next_frame() {
                deadline.check().map_err(|_| timed_out())?;
                if inventory.feed(frame.map_err(protocol)?).map_err(protocol)? {
                    deadline.check().map_err(|_| timed_out())?;
                    return Ok(());
                }
            }
            connection
                .stream
                .set_read_timeout(Some(slice(deadline)?.min(READ_POLL)))
                .map_err(|_| DeviceError::Disconnected)?;
            match connection.stream.read(&mut buffer) {
                Ok(0) => return Err(DeviceError::Disconnected),
                Ok(n) => {
                    deadline.check().map_err(|_| timed_out())?;
                    connection.decoder.push(&buffer[..n]);
                }
                // A socket timeout is only a slice of the deadline, so waking
                // up with nothing to read is not the end of the exchange.
                Err(e) if expired(&e) => deadline.check().map_err(|_| timed_out())?,
                Err(_) => return Err(DeviceError::Disconnected),
            }
        }
    }
}

fn arm(connection: &Connection, deadline: &Deadline) -> DeviceResult<()> {
    let left = slice(deadline)?;
    connection
        .stream
        .set_read_timeout(Some(left))
        .map_err(|_| DeviceError::Disconnected)?;
    connection
        .stream
        .set_write_timeout(Some(left))
        .map_err(|_| DeviceError::Disconnected)?;
    Ok(())
}

/// How long a socket call may wait. A remaining window below a millisecond is
/// treated as expired, because some platforms read a zero socket timeout as
/// "wait forever" rather than "give up now".
fn slice(deadline: &Deadline) -> DeviceResult<Duration> {
    let left = deadline.remaining().map_err(|_| timed_out())?;
    if left < Duration::from_millis(1) {
        return Err(timed_out());
    }
    Ok(left)
}

fn expired(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn timed_out() -> DeviceError {
    DeviceError::Other(TIMED_OUT.to_string())
}

fn protocol(e: EcError) -> DeviceError {
    DeviceError::Other(e.to_string())
}

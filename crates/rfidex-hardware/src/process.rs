//! Lifetime control for a hardware transport, and the parent side of the
//! helper connection.
//!
//! Shutdown must be able to end a reader that is stuck without waiting for the
//! call that is stuck, so a station holds one `Arc<StopControl>` per real
//! device and never a lock the working call also needs. The action inside is a
//! one-shot handle — a cloned socket, or a child process — not the adapter.
//!
//! The parent's rules:
//!
//! * Bind loopback only, on a port the kernel chooses, and authenticate the
//!   first thing the child says against a token minted for this launch.
//! * One request outstanding at a time, with its own absolute deadline.
//! * A broken exchange ends the session. Nothing is sent twice: a write whose
//!   outcome is unknown is reported as failed, never replayed.

use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::config::HardwareConfig;
use crate::host::{CHILD_SWITCH, COMMISSIONING, DISCOVER, ENUMERATE};
use crate::sdk::EnumerationKind;
use crate::wire::{
    match_reply, read_frame, read_reply, write_frame, Deadline, DeadlineSocket, Operation, Request,
    Response, WireError,
};

/// How long a helper is given to start, connect and prove it is ours.
const HANDSHAKE_WINDOW_MS: u32 = 5_000;
/// The request id a helper's enumeration answer carries. Enumeration is a
/// startup mode of its own, not an operation on an open reader, so it never
/// shares the numbering of a session's requests.
const ENUMERATE_REPLY_ID: u64 = 0;
/// The child-mode words for the three vendor enumeration functions.
const HID: &str = "hid";
const COM: &str = "com";
const NET: &str = "net";
/// How often the accept loop looks at the deadline and at the child.
const ACCEPT_POLL: Duration = Duration::from_millis(5);

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// One-shot, idempotent, non-blocking.
pub struct StopControl {
    stopped: AtomicBool,
    action: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl StopControl {
    pub fn new(action: impl FnOnce() + Send + 'static) -> Arc<StopControl> {
        Arc::new(StopControl {
            stopped: AtomicBool::new(false),
            action: Mutex::new(Some(Box::new(action))),
        })
    }

    /// The first caller runs the action; every later caller returns at once.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let action = self.action.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(action) = action {
            action();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Which executable to start in child mode.
///
/// In the app this is the desktop executable itself, so no sidecar has to be
/// packaged. In tests it is the developer host binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLauncher {
    pub executable: PathBuf,
}

impl HostLauncher {
    pub fn new(executable: impl Into<PathBuf>) -> HostLauncher {
        HostLauncher {
            executable: executable.into(),
        }
    }

    /// The gate helper written in the vendor's own language (C#, managed
    /// `rfidclib_reader.dll`), when it is installed beside the app or in its
    /// `gate-host/` folder. Absent in developer builds and tests, where gates
    /// keep using the helper the caller passes.
    pub fn gate_host_installed() -> Option<HostLauncher> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        [
            dir.join("gate-host").join(GATE_HOST_NAME),
            dir.join(GATE_HOST_NAME),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .map(HostLauncher::new)
    }

    /// The running program, started again in child mode.
    pub fn current_exe() -> Result<HostLauncher, WireError> {
        std::env::current_exe()
            .map(HostLauncher::new)
            .map_err(|_| WireError::Disconnected)
    }
}

/// Ask the vendor library what devices it can see.
///
/// This starts a bounded helper of its own rather than reusing a session: the
/// enumeration happens before any device is opened, and nothing about a device
/// is guessed from a list of names.
pub fn enumerate_devices(
    launcher: &HostLauncher,
    dll_path: &std::path::Path,
    kind: EnumerationKind,
) -> Result<Vec<String>, WireError> {
    list_from_child(
        launcher,
        &[
            ENUMERATE.as_ref(),
            kind_word(kind).as_ref(),
            dll_path.as_os_str(),
        ],
    )
}

/// Network readers that answer a broadcast on one PC interface. Like
/// enumeration, a short-lived helper that opens no device.
pub fn discover_readers(
    launcher: &HostLauncher,
    dll_path: &std::path::Path,
    iface: &str,
) -> Result<Vec<String>, WireError> {
    list_from_child(
        launcher,
        &[DISCOVER.as_ref(), dll_path.as_os_str(), iface.as_ref()],
    )
}

fn list_from_child(
    launcher: &HostLauncher,
    mode: &[&std::ffi::OsStr],
) -> Result<Vec<String>, WireError> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|_| WireError::Disconnected)?;
    let port = listener
        .local_addr()
        .map_err(|_| WireError::Disconnected)?
        .port();
    let token = uuid::Uuid::new_v4().to_string();
    let child = Command::new(&launcher.executable)
        .arg(CHILD_SWITCH)
        .arg(port.to_string())
        .arg(&token)
        .args(mode)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|_| WireError::Disconnected)?;
    let child = Arc::new(Mutex::new(Some(child)));
    let outcome = (|| {
        let deadline = Deadline::started(HANDSHAKE_WINDOW_MS);
        let mut socket = accept_child(&listener, &token, &child, &deadline, None, None)?;
        let reply = read_reply(&mut DeadlineSocket::new(&mut socket, &deadline), &deadline)?;
        match match_reply(reply, ENUMERATE_REPLY_ID)? {
            Response::Strings { values } => Ok(values),
            _ => Err(WireError::BadResponse),
        }
    })();
    reap(&child);
    outcome
}

/// The word the child's argument parser expects.
pub fn kind_word(kind: EnumerationKind) -> &'static str {
    match kind {
        EnumerationKind::Hid => HID,
        EnumerationKind::Com => COM,
        EnumerationKind::Net => NET,
    }
}

/// The other direction, so the parent and the child cannot drift apart.
pub fn kind_from_word(word: &str) -> Option<EnumerationKind> {
    match word {
        HID => Some(EnumerationKind::Hid),
        COM => Some(EnumerationKind::Com),
        NET => Some(EnumerationKind::Net),
        _ => None,
    }
}

/// The most the opening call may take.
const OPEN_WINDOW_MS: u32 = 10_000;

pub const GATE_HOST_NAME: &str = "rfidex-gate-host.exe";

/// A live helper: one child process, one open device context, one request at a
/// time.
pub struct HardwareClient {
    stream: Option<TcpStream>,
    /// Owns the child: the process handle lives inside this control's one-shot
    /// action, so killing and reaping never needs a lock the request path uses.
    control: Arc<StopControl>,
    next_id: u64,
    timeout_ms: u32,
    generation: u64,
}

impl HardwareClient {
    /// Start a helper, authenticate it and open the device.
    ///
    /// The config is sent to the child, which validates it again: a config that
    /// reached the parent's checks but not the child's is a programming error,
    /// and it fails here rather than mid-operation.
    pub fn start(
        launcher: &HostLauncher,
        config: &HardwareConfig,
    ) -> Result<HardwareClient, WireError> {
        Self::start_with_stop(launcher, config, |_| Ok(()))
    }

    /// Publish process control immediately after spawn, before waiting for a
    /// handshake or Open. The caller may stop it without its request lock.
    pub fn start_with_stop(
        launcher: &HostLauncher,
        config: &HardwareConfig,
        publish: impl FnOnce(Arc<StopControl>) -> Result<(), WireError>,
    ) -> Result<HardwareClient, WireError> {
        Self::start_mode(launcher, config, false, publish)
    }

    /// Explicit developer-only host mode; ordinary app sessions use `start`.
    pub fn start_commissioning(
        launcher: &HostLauncher,
        config: &HardwareConfig,
    ) -> Result<HardwareClient, WireError> {
        Self::start_mode(launcher, config, true, |_| Ok(()))
    }

    fn start_mode(
        launcher: &HostLauncher,
        config: &HardwareConfig,
        commissioning: bool,
        publish: impl FnOnce(Arc<StopControl>) -> Result<(), WireError>,
    ) -> Result<HardwareClient, WireError> {
        config.validate().map_err(|_| WireError::BadResponse)?;
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|_| WireError::Disconnected)?;
        let port = listener
            .local_addr()
            .map_err(|_| WireError::Disconnected)?
            .port();
        let token = uuid::Uuid::new_v4().to_string();
        let mut command = Command::new(&launcher.executable);
        command.arg(CHILD_SWITCH).arg(port.to_string()).arg(&token);
        if commissioning {
            command.arg(COMMISSIONING);
        }
        let child = command
            .stdin(Stdio::null())
            .spawn()
            .map_err(|_| WireError::Disconnected)?;
        let child = Arc::new(Mutex::new(Some(child)));
        let shutdown: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
        let child_for_stop = child.clone();
        let shutdown_for_stop = shutdown.clone();
        let control = StopControl::new(move || {
            if let Some(handle) = shutdown_for_stop
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = handle.shutdown(Shutdown::Both);
            }
            reap(&child_for_stop);
        });
        let outcome = (|| {
            publish(control.clone())?;
            if control.is_stopped() {
                return Err(WireError::Disconnected);
            }
            let deadline = Deadline::started(HANDSHAKE_WINDOW_MS);
            let socket = accept_child(
                &listener,
                &token,
                &child,
                &deadline,
                Some(&control),
                Some(&shutdown),
            )?;
            {
                let mut slot = shutdown.lock().unwrap_or_else(|e| e.into_inner());
                if control.is_stopped() {
                    return Err(WireError::Disconnected);
                }
                *slot = Some(socket.try_clone().map_err(|_| WireError::Disconnected)?);
            }
            if control.is_stopped() {
                return Err(WireError::Disconnected);
            }
            let mut client = HardwareClient::connect(socket, control.clone(), config.timeout_ms());
            // The first open loads the vendor library and connects over the
            // network; give it longer than a routine call.
            client.timeout_ms = client.timeout_ms.max(OPEN_WINDOW_MS);
            let opened = client.call(Operation::Open {
                config: config.clone(),
            });
            client.timeout_ms = config.timeout_ms();
            match opened {
                Ok(Response::Unit) if !control.is_stopped() => Ok(client),
                Ok(_) => Err(WireError::Disconnected),
                Err(e) => Err(e),
            }
        })();
        if outcome.is_err() {
            control.stop();
        }
        outcome
    }

    fn connect(socket: TcpStream, control: Arc<StopControl>, timeout_ms: u32) -> HardwareClient {
        HardwareClient {
            stream: Some(socket),
            control,
            next_id: 1,
            timeout_ms,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::SeqCst),
        }
    }

    pub fn stop_control(&self) -> Arc<StopControl> {
        self.control.clone()
    }

    pub fn is_alive(&self) -> bool {
        self.stream.is_some() && !self.control.is_stopped()
    }

    /// Which helper this is. A station compares it before publishing a result,
    /// so a late answer from a replaced reader cannot update a new screen.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// One operation, one attempt.
    ///
    /// A failure ends the session: the child's native context may be in any
    /// state, and repeating the operation could write a sticker twice.
    pub fn call(&mut self, operation: Operation) -> Result<Response, WireError> {
        if self.control.is_stopped() {
            return Err(WireError::Disconnected);
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(WireError::BadResponse)?;
        let deadline = Deadline::started(self.timeout_ms);
        let request = Request { id, operation };
        let Some(stream) = self.stream.as_mut() else {
            return Err(WireError::Disconnected);
        };
        let mut socket = DeadlineSocket::new(stream, &deadline);
        // A transport or protocol break ends the session. A well-formed error
        // reply does not: the channel is intact, the child answered "no"
        // deliberately (unsupported mode, no tag, out of range), and the next
        // request can still be made. `DeadlineSocket` is dropped with the
        // borrow, so the error mapping happens on owned values only.
        let outcome = write_frame(&mut socket, &request, &deadline)
            .and_then(|()| read_reply(&mut socket, &deadline));
        let reply = match outcome {
            Ok(reply) => reply,
            Err(e) => {
                self.stop();
                return Err(e);
            }
        };
        match match_reply(reply, id) {
            // The id matching is not enough: the answer has to be the kind of
            // answer this operation has. A helper that replies to something else
            // is a broken channel, not a result.
            Ok(response)
                if answers(&request.operation, &response) && !self.control.is_stopped() =>
            {
                Ok(response)
            }
            Ok(_) => {
                self.stop();
                Err(WireError::BadResponse)
            }
            Err(WireError::Unsupported) => Err(WireError::Unsupported),
            Err(WireError::WriteUnsupported) => Err(WireError::WriteUnsupported),
            Err(e) => {
                self.stop();
                Err(e)
            }
        }
    }

    /// End the session. Idempotent, does not wait on the request path, and
    /// leaves the child reaped so nothing is left running.
    pub fn stop(&self) {
        self.control.stop();
    }
}

impl Drop for HardwareClient {
    fn drop(&mut self) {
        self.stop();
        self.stream = None;
    }
}

/// Accept the helper's connection, authenticated, before the window closes.
///
/// A connection that does not carry this launch's token is closed and the
/// search continues — but only until the original deadline, which does not
/// move for an impostor.
fn accept_child(
    listener: &TcpListener,
    token: &str,
    child: &Arc<Mutex<Option<Child>>>,
    deadline: &Deadline,
    control: Option<&StopControl>,
    shutdown: Option<&Arc<Mutex<Option<TcpStream>>>>,
) -> Result<TcpStream, WireError> {
    listener
        .set_nonblocking(true)
        .map_err(|_| WireError::Disconnected)?;
    loop {
        if control.is_some_and(StopControl::is_stopped) {
            return Err(WireError::Disconnected);
        }
        deadline.check()?;
        match listener.accept() {
            Ok((mut candidate, _)) => {
                if let Some(shutdown) = shutdown {
                    let mut slot = shutdown.lock().unwrap_or_else(|e| e.into_inner());
                    if control.is_some_and(StopControl::is_stopped) {
                        return Err(WireError::Disconnected);
                    }
                    *slot = Some(candidate.try_clone().map_err(|_| WireError::Disconnected)?);
                }
                if authenticate(&mut candidate, token, deadline) {
                    if control.is_some_and(StopControl::is_stopped) {
                        return Err(WireError::Disconnected);
                    }
                    candidate
                        .set_nonblocking(false)
                        .map_err(|_| WireError::Disconnected)?;
                    return Ok(candidate);
                }
                if let Some(shutdown) = shutdown {
                    shutdown.lock().unwrap_or_else(|e| e.into_inner()).take();
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if exited(child) {
                    return Err(WireError::Disconnected);
                }
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => return Err(WireError::Disconnected),
        }
    }
}

fn authenticate(candidate: &mut TcpStream, token: &str, deadline: &Deadline) -> bool {
    matches!(read_frame::<_, String>(&mut DeadlineSocket::new(candidate, deadline), deadline), Ok(value) if value == token)
}

fn exited(child: &Arc<Mutex<Option<Child>>>) -> bool {
    let mut guard = lock(child);
    match guard.as_mut() {
        // Only a real exit ends the wait early. A query that failed says
        // nothing about the child — an interrupted wait is not an exit — and
        // the handshake window still bounds the loop.
        Some(child) => matches!(child.try_wait(), Ok(Some(_))),
        None => true,
    }
}

/// Kill and wait: a child that is merely killed and never waited for becomes a
/// zombie, which on Windows means a handle that is never released.
fn reap(child: &Arc<Mutex<Option<Child>>>) {
    let taken = lock(child).take();
    if let Some(mut child) = taken {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn lock(child: &Arc<Mutex<Option<Child>>>) -> MutexGuard<'_, Option<Child>> {
    child.lock().unwrap_or_else(|e| e.into_inner())
}

/// Every operation has exactly one answer shape.
fn answers(operation: &Operation, response: &Response) -> bool {
    matches!(
        (operation, response),
        (Operation::Open { .. }, Response::Unit)
            | (Operation::Close, Response::Unit)
            | (Operation::Write { .. }, Response::Unit)
            | (Operation::Info, Response::Info { .. })
            | (Operation::Inventory, Response::Tags { .. })
            | (Operation::Memory { .. }, Response::Memory { .. })
            | (Operation::Read { .. }, Response::Bytes { .. })
            | (Operation::RawRecords, Response::Records { .. })
            | (Operation::LibraryRecords { .. }, Response::Records { .. })
            | (Operation::LibraryRecords { .. }, Response::Passes { .. })
            | (Operation::LibraryAlarm { .. }, Response::Unit)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn stopping_runs_the_action_once_however_often_it_is_called() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        let control = StopControl::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        control.stop();
        control.stop();
        control.stop();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(control.is_stopped());
    }

    #[test]
    fn a_control_that_was_never_stopped_reports_so() {
        let control = StopControl::new(|| {});
        assert!(!control.is_stopped());
    }
}

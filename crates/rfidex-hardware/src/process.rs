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
use crate::host::CHILD_SWITCH;
use crate::wire::{
    match_reply, read_frame, read_reply, write_frame, Deadline, Operation, Request, Response,
    WireError,
};

/// How long a helper is given to start, connect and prove it is ours.
const HANDSHAKE_WINDOW_MS: u32 = 5_000;
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

    /// The running program, started again in child mode.
    pub fn current_exe() -> Result<HostLauncher, WireError> {
        std::env::current_exe()
            .map(HostLauncher::new)
            .map_err(|_| WireError::Disconnected)
    }
}

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
        config.validate().map_err(|_| WireError::BadResponse)?;

        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|_| WireError::Disconnected)?;
        let port = listener
            .local_addr()
            .map_err(|_| WireError::Disconnected)?
            .port();
        // A fresh, transient token per launch. It is process coordination, not
        // an event credential and not an operator login.
        let token = uuid::Uuid::new_v4().to_string();

        let child = Command::new(&launcher.executable)
            .arg(CHILD_SWITCH)
            .arg(port.to_string())
            .arg(&token)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|_| WireError::Disconnected)?;
        let child = Arc::new(Mutex::new(Some(child)));

        let socket = match accept_child(&listener, &token, &child) {
            Ok(socket) => socket,
            Err(e) => {
                reap(&child);
                return Err(e);
            }
        };

        let mut client = HardwareClient::connect(socket, child, config.timeout_ms());
        match client.call(Operation::Open {
            config: config.clone(),
        }) {
            Ok(Response::Unit) => Ok(client),
            Ok(_) => {
                client.stop();
                Err(WireError::BadResponse)
            }
            Err(e) => {
                client.stop();
                Err(e)
            }
        }
    }

    fn connect(
        socket: TcpStream,
        child: Arc<Mutex<Option<Child>>>,
        timeout_ms: u32,
    ) -> HardwareClient {
        let shutdown = socket.try_clone().ok();
        let child_for_stop = child.clone();
        let control = StopControl::new(move || {
            // The socket first: the child may be blocked on it, and a kill is
            // only authoritative once the read it was in has failed.
            if let Some(handle) = &shutdown {
                let _ = handle.shutdown(Shutdown::Both);
            }
            reap(&child_for_stop);
        });
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
        arm(stream, &deadline)?;
        let outcome = write_frame(stream, &request, &deadline)
            .and_then(|()| read_reply(stream, &deadline))
            .and_then(|reply| match_reply(reply, id));
        match outcome {
            // The id matching is not enough: the answer has to be the kind of
            // answer this operation has. A helper that replies to something else
            // is a broken channel, not a result.
            Ok(response) if answers(&request.operation, &response) => Ok(response),
            Ok(_) => {
                self.stop();
                Err(WireError::BadResponse)
            }
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
) -> Result<TcpStream, WireError> {
    listener
        .set_nonblocking(true)
        .map_err(|_| WireError::Disconnected)?;
    let deadline = Deadline::started(HANDSHAKE_WINDOW_MS);
    loop {
        match listener.accept() {
            Ok((mut candidate, _)) => {
                if authenticate(&mut candidate, token, &deadline) {
                    candidate
                        .set_nonblocking(false)
                        .map_err(|_| WireError::Disconnected)?;
                    return Ok(candidate);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // A helper that has already gone is not going to connect, so it
                // fails now instead of at the end of the window.
                if exited(child) {
                    return Err(WireError::Disconnected);
                }
                if deadline.check().is_err() {
                    return Err(WireError::Timeout);
                }
                std::thread::sleep(ACCEPT_POLL);
            }
            // A pending connection can be withdrawn before it is accepted, and
            // a signal can interrupt the call. Neither says anything about the
            // child, and the window still bounds the loop.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                if deadline.check().is_err() {
                    return Err(WireError::Timeout);
                }
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => return Err(WireError::Disconnected),
        }
    }
}

fn authenticate(candidate: &mut TcpStream, token: &str, deadline: &Deadline) -> bool {
    if arm(candidate, deadline).is_err() {
        return false;
    }
    // The first thing the helper says is the token and nothing else.
    matches!(read_frame::<_, String>(candidate, deadline), Ok(value) if value == token)
}

fn arm(stream: &TcpStream, deadline: &Deadline) -> Result<(), WireError> {
    let left = deadline.remaining()?;
    if left < Duration::from_millis(1) {
        return Err(WireError::Timeout);
    }
    stream
        .set_read_timeout(Some(left))
        .map_err(|_| WireError::Disconnected)?;
    stream
        .set_write_timeout(Some(left))
        .map_err(|_| WireError::Disconnected)?;
    Ok(())
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

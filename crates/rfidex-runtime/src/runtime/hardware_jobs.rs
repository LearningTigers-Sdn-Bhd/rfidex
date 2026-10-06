//! Blocking-worker tracking, so shutdown waits for hardware work it started.

use std::sync::atomic::Ordering;
use std::sync::Mutex;

use tokio::task::JoinHandle;

use crate::RuntimeError;

/// Hardware work runs on blocking workers, off the async executor: a native
/// call or a stalled socket must never occupy a Tokio worker thread. Every such
/// task is tracked, so shutdown waits for the work it started instead of
/// dropping a handle and letting it run on.
#[derive(Default)]
pub(super) struct HardwareJobs {
    jobs: Mutex<Vec<JoinHandle<()>>>,
    pub(super) stopped: std::sync::atomic::AtomicBool,
}

impl HardwareJobs {
    /// Register under the same lock that closes registration during shutdown.
    pub(super) async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, RuntimeError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::SeqCst) {
                return Err(reader_stopped());
            }
            jobs.retain(|job| !job.is_finished());
            jobs.push(tokio::task::spawn_blocking(move || {
                let _ = sender.send(work());
            }));
        }
        receiver.await.map_err(|_| reader_stopped())
    }

    /// Refuse new work and wait for the work already started.
    pub(super) async fn join_all(&self) {
        let running = {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            self.stopped.store(true, Ordering::SeqCst);
            std::mem::take(&mut *jobs)
        };
        for job in running {
            let _ = job.await;
        }
    }
}

fn reader_stopped() -> RuntimeError {
    RuntimeError::new(
        "reader_stopped",
        "The reader stopped. Reconnect it and try again.",
    )
}

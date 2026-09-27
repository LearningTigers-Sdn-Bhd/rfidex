//! Lifetime control for a hardware transport.
//!
//! Shutdown must be able to end a reader that is stuck without waiting for the
//! call that is stuck, so a station holds one `Arc<StopControl>` per real
//! device and never a lock the working call also needs. The action inside is a
//! one-shot handle — a cloned socket — not the adapter.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

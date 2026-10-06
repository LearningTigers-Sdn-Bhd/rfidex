//! Timing settings shared by every station loop.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    pub heartbeat: Duration,
    pub gate_poll: Duration,
    pub client_timeout: Duration,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(30),
            gate_poll: Duration::from_millis(250),
            // A slow server must not turn a desk scan into an "offline" one.
            // An unreachable server still fails fast on the connect timeout.
            client_timeout: Duration::from_secs(10),
        }
    }
}

//! Simulated gate: stored records or live inventory, with optional re-delivery.

use std::collections::VecDeque;

use super::{
    DeviceError, DeviceInfo, DeviceResult, GateCaps, GateKind, GateRead, GateSource, ReleaseHandle,
};

pub struct SimGate {
    caps: GateCaps,
    pending: VecDeque<(ReleaseHandle, GateRead)>,
    next_handle: u64,
    pub connected: bool,
    pub redeliver_unreleased: bool,
    pub released: Vec<ReleaseHandle>,
    /// The next N releases fail (device error during acknowledge/delete).
    pub fail_next_releases: u32,
    pub on_release: Option<Box<dyn FnMut(ReleaseHandle) + Send>>,
}

impl SimGate {
    pub fn new(kind: GateKind, release_verified: bool) -> SimGate {
        SimGate {
            caps: GateCaps {
                kind,
                release_verified,
            },
            pending: VecDeque::new(),
            next_handle: 1,
            connected: true,
            redeliver_unreleased: false,
            released: Vec::new(),
            fail_next_releases: 0,
            on_release: None,
        }
    }

    pub fn push(&mut self, read: GateRead) -> ReleaseHandle {
        let h = ReleaseHandle(self.next_handle);
        self.next_handle += 1;
        self.pending.push_back((h, read));
        h
    }
}

impl GateSource for SimGate {
    fn info(&mut self) -> DeviceResult<DeviceInfo> {
        Ok(DeviceInfo {
            adapter: "sim-gate".into(),
            model: Some("SIM".into()),
            firmware: None,
        })
    }

    fn poll(&mut self) -> DeviceResult<Vec<(GateRead, ReleaseHandle)>> {
        if !self.connected {
            return Err(DeviceError::Disconnected);
        }
        let out = self.pending.iter().map(|(h, r)| (r.clone(), *h)).collect();
        if !self.redeliver_unreleased {
            self.pending.clear();
        }
        Ok(out)
    }

    fn release(&mut self, handle: ReleaseHandle) -> DeviceResult<()> {
        if !self.caps.release_verified {
            return Err(DeviceError::Other("release not verified".into()));
        }
        if self.fail_next_releases > 0 {
            self.fail_next_releases -= 1;
            return Err(DeviceError::Other("release failed".into()));
        }
        if let Some(hook) = self.on_release.as_mut() {
            hook(handle);
        }
        self.pending.retain(|(h, _)| *h != handle);
        self.released.push(handle);
        Ok(())
    }

    fn capabilities(&self) -> GateCaps {
        self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_consumes_by_default() {
        let mut g = SimGate::new(GateKind::Records, false);
        g.push(GateRead::sighting(vec![1]));
        g.push(GateRead::sighting(vec![2]));
        assert_eq!(g.poll().unwrap().len(), 2);
        assert!(g.poll().unwrap().is_empty());
    }

    #[test]
    fn redelivers_until_released() {
        let mut g = SimGate::new(GateKind::Records, true);
        g.redeliver_unreleased = true;
        let h = g.push(GateRead::sighting(vec![1]));
        assert_eq!(g.poll().unwrap().len(), 1);
        assert_eq!(g.poll().unwrap().len(), 1);
        g.release(h).unwrap();
        assert!(g.poll().unwrap().is_empty());
        assert_eq!(g.released, vec![h]);
    }

    #[test]
    fn unverified_release_is_an_error() {
        let mut g = SimGate::new(GateKind::Records, false);
        let h = g.push(GateRead::sighting(vec![1]));
        assert!(g.release(h).is_err());
    }

    #[test]
    fn disconnected_poll_fails() {
        let mut g = SimGate::new(GateKind::LiveInventory, false);
        g.connected = false;
        assert_eq!(g.poll(), Err(DeviceError::Disconnected));
    }

    #[test]
    fn release_hook_runs() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut g = SimGate::new(GateKind::Records, true);
        g.on_release = Some(Box::new(move |h| seen2.lock().unwrap().push(h)));
        let h = g.push(GateRead::sighting(vec![1]));
        g.poll().unwrap();
        g.release(h).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![h]);
    }
}

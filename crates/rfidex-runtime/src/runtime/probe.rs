//! Operator reader tests: one station's own probe, and what each test touches.

use rfidex_core::device::{GateSource, TagReaderWriter};

use super::device::StationDevice;
use super::station::StationRuntime;
use crate::hardware::{self, HardwareTestAction, HardwareTestView};

/// One station's own probe, with the session lock held only for that.
pub(super) async fn probe_station(
    runtime: &StationRuntime,
    action: HardwareTestAction,
) -> HardwareTestView {
    if runtime
        .hardware_stop
        .as_ref()
        .is_some_and(|c| c.is_stopped())
    {
        return hardware::stopped();
    }
    let view = probe_locked(&runtime.device, action).await;
    if runtime
        .hardware_stop
        .as_ref()
        .is_some_and(|c| c.is_stopped())
    {
        return hardware::stopped();
    }
    if runtime.hardware_stop.is_some() {
        let mut inner = runtime.lock();
        inner.connected = view.ok;
        inner.connection_checked = true;
    }
    view
}

/// A core tag read back as the vendor-shaped value the operator's view uses.
fn wire_tags(tags: Vec<rfidex_core::tag::TagRead>) -> Vec<rfidex_hardware::wire::WireTag> {
    tags.into_iter()
        .map(|tag| rfidex_hardware::wire::WireTag {
            uid: <[u8; 8]>::try_from(tag.uid_raw.as_slice()).unwrap_or([0; 8]),
            dsfid: tag.dsfid.unwrap_or(0),
            antenna: tag.antenna,
        })
        .collect()
}

/// What a probe may touch: one station's own session, and nothing else.
async fn probe_locked(device: &StationDevice, action: HardwareTestAction) -> HardwareTestView {
    match action {
        HardwareTestAction::Connect => match device {
            StationDevice::Desk(d) => {
                let mut session = d.lock().await;
                match session.station.reader.probe() {
                    Ok(info) => {
                        let simulator = session.station.reader.is_simulated();
                        hardware::connected(&info, simulator)
                    }
                    Err(e) => hardware::from_error(&e),
                }
            }
            StationDevice::Gate(g) => {
                let mut gate = g.lock().await;
                match gate.gate.probe() {
                    Ok(info) => {
                        let simulator = gate.gate.is_simulated();
                        hardware::connected(&info, simulator)
                    }
                    Err(e) => hardware::from_error(&e),
                }
            }
        },
        HardwareTestAction::ReadTags => match device {
            StationDevice::Desk(d) => {
                let mut session = d.lock().await;
                match session.station.reader.inventory() {
                    Ok(tags) => hardware::tags(&wire_tags(tags)),
                    Err(e) => hardware::from_error(&e),
                }
            }
            StationDevice::Gate(g) => {
                let mut gate = g.lock().await;
                match gate.gate.poll() {
                    Ok(found) => {
                        let tags: Vec<rfidex_hardware::wire::WireTag> = found
                            .into_iter()
                            .map(|(read, _handle)| rfidex_hardware::wire::WireTag {
                                uid: <[u8; 8]>::try_from(read.tag.uid_raw.as_slice())
                                    .unwrap_or_default(),
                                dsfid: read.tag.dsfid.unwrap_or(0),
                                antenna: read.tag.antenna,
                            })
                            .collect();
                        hardware::tags(&tags)
                    }
                    Err(e) => hardware::from_error(&e),
                }
            }
        },
        HardwareTestAction::GateRecords => match device {
            StationDevice::Desk(_) => hardware::failed("Gate records are only for gates."),
            StationDevice::Gate(g) => {
                let mut gate = g.lock().await;
                match gate.gate.library_records() {
                    Ok(raw) => hardware::records(&raw),
                    Err(e) => hardware::from_error(&e),
                }
            }
        },
        HardwareTestAction::ClearGateRecords => match device {
            StationDevice::Desk(_) => hardware::failed("Gate records are only for gates."),
            StationDevice::Gate(g) => {
                let mut gate = g.lock().await;
                match gate.gate.clear_records() {
                    Ok(()) => hardware::cleared(),
                    Err(e) => hardware::from_error(&e),
                }
            }
        },
    }
}

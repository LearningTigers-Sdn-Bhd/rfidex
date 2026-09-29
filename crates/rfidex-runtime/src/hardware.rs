//! The operator's reader tests, and everything they are allowed to say.
//!
//! The wording lives here rather than in the UI, and so does the rule that a
//! successful test is not a verified reader: a connection proves the reader
//! answered, not that its model, firmware or writes are compatible.

use rfidex_core::device::DeviceInfo;
use rfidex_hardware::process::HostLauncher;
use rfidex_hardware::sdk::EnumerationKind;

pub use rfidex_hardware::wire::WireTag;

use crate::RuntimeError;

/// The two tests an operator may run. Neither writes to a sticker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareTestAction {
    Connect,
    ReadTags,
    /// Raw library-gate records, shown as hex to learn their layout.
    GateRecords,
}

/// The most UIDs the screen shows. The message always carries the true count,
/// so a busy reader cannot push a wall of hex at the operator.
pub const SHOWN_UIDS: usize = 20;

#[derive(Debug, Clone, serde::Serialize)]
pub struct HardwareTestView {
    pub ok: bool,
    pub message: String,
    pub uid_raw_hex: Vec<String>,
    /// Always false in this build: only the physical disposable-tag acceptance
    /// tests may turn it on, and no test in the app can reach that.
    pub hardware_verified: bool,
}

const CONNECTED: &str = "Reader answered.";
const CONNECTED_SIMULATOR: &str = "This is a simulated reader. Nothing is connected.";
const NO_TAGS: &str = "Reader answered. No sticker in the field.";
const UNREACHABLE: &str =
    "The reader did not answer. Check that it is switched on, connected, and set to the address above.";
const STOPPED: &str = "The reader was stopped. Open Setup and try again.";

/// The result of a test the reader refused.
pub fn failed(code: &str) -> HardwareTestView {
    HardwareTestView {
        ok: false,
        message: code.to_string(),
        uid_raw_hex: Vec::new(),
        hardware_verified: false,
    }
}

fn view(ok: bool, message: String, uids: Vec<String>) -> HardwareTestView {
    HardwareTestView {
        ok,
        message,
        uid_raw_hex: uids,
        hardware_verified: false,
    }
}

pub fn connected(info: &DeviceInfo, simulator: bool) -> HardwareTestView {
    let message = match (simulator, &info.model) {
        (true, _) => CONNECTED_SIMULATOR.to_string(),
        (false, Some(model)) => format!("{CONNECTED} It reports itself as {model}."),
        (false, None) => CONNECTED.to_string(),
    };
    view(true, message, Vec::new())
}

pub fn tags(tags: &[WireTag]) -> HardwareTestView {
    if tags.is_empty() {
        return view(true, NO_TAGS.to_string(), Vec::new());
    }
    let shown: Vec<String> = tags
        .iter()
        .take(SHOWN_UIDS)
        .map(|tag| rfidex_core::tag::hex_upper(&tag.uid))
        .collect();
    let message = if tags.len() > SHOWN_UIDS {
        format!(
            "Reader answered. {} stickers in the field; showing the first {SHOWN_UIDS}.",
            tags.len()
        )
    } else {
        format!(
            "Reader answered. {} sticker{} in the field.",
            tags.len(),
            if tags.len() == 1 { "" } else { "s" }
        )
    };
    view(true, message, shown)
}

/// One place where a reader failure becomes an operator sentence.
pub fn from_error(e: &rfidex_core::device::DeviceError) -> HardwareTestView {
    use rfidex_core::device::DeviceError;
    let message = match e {
        DeviceError::Disconnected => UNREACHABLE,
        DeviceError::TagNotFound => NO_TAGS,
        DeviceError::OutOfRange => UNREACHABLE,
        DeviceError::WriteUnsupported => UNREACHABLE,
        DeviceError::Other(_) => UNREACHABLE,
    };
    view(false, message.to_string(), Vec::new())
}

/// A library-gate fetch: the stickers it handed over, then the raw frames so
/// an unexpected layout can still be sent back.
pub fn records(raw: &[Vec<u8>]) -> HardwareTestView {
    let passes: Vec<_> = raw
        .iter()
        .flat_map(|f| rfidex_hardware::library_gate::passes(f))
        .collect();
    let message = match passes.len() {
        0 => "Gate answered. No sticker pass waiting. Walk a sticker through, then press again."
            .to_string(),
        n => format!(
            "Gate answered. {n} sticker pass{} waiting.",
            if n == 1 { "" } else { "es" }
        ),
    };
    let lines = passes
        .iter()
        .map(|p| format!("Sticker {}", rfidex_core::tag::hex_upper(&p.uid)))
        .chain(
            raw.iter()
                .map(|r| format!("Raw {}", rfidex_core::tag::hex_upper(r))),
        )
        .take(SHOWN_UIDS)
        .collect();
    view(true, message, lines)
}

pub fn stopped() -> HardwareTestView {
    view(false, STOPPED.to_string(), Vec::new())
}

/// A helper program that is not there is a configuration problem the operator
/// can act on, not a crash.
pub fn no_helper() -> RuntimeError {
    RuntimeError::new(
        "no_helper",
        "This app cannot start its reader helper. Reinstall RfiDex.",
    )
}

/// List the devices the vendor library can see. No device is opened and no
/// model is assumed from a name.
pub fn enumerate(
    launcher: &HostLauncher,
    dll_path: &std::path::Path,
    kind: EnumerationKind,
) -> Result<Vec<String>, RuntimeError> {
    dll_path
        .to_str()
        .ok_or_else(|| RuntimeError::new("bad_reader_path", BAD_PATH))?;
    rfidex_hardware::process::enumerate_devices(launcher, dll_path, kind).map_err(|e| match e {
        rfidex_hardware::wire::WireError::Timeout => RuntimeError::new(
            "reader_timeout",
            "The reader lists did not arrive in time. Check the library path and try again.",
        ),
        _ => RuntimeError::new(
            "reader_unavailable",
            "The reader library could not be loaded. Check the path to ECRFID.dll on this computer.",
        ),
    })
}

/// Network readers answering on one PC interface, as `address=IP:port;…` lines.
/// Nothing is opened; the operator picks one to fill the reader address.
pub fn discover(
    launcher: &HostLauncher,
    dll_path: &std::path::Path,
    iface: &str,
) -> Result<Vec<String>, RuntimeError> {
    dll_path
        .to_str()
        .ok_or_else(|| RuntimeError::new("bad_reader_path", BAD_PATH))?;
    if iface.trim().is_empty() {
        return Err(RuntimeError::new(
            "no_interface",
            "Pick the network card the gate is plugged into first: press Look for readers, then Use this one on the Ethernet entry.",
        ));
    }
    rfidex_hardware::process::discover_readers(launcher, dll_path, iface).map_err(|e| match e {
        rfidex_hardware::wire::WireError::Timeout => RuntimeError::new(
            "reader_timeout",
            "No answer in time. Check the gate is powered and its Ethernet cable is in, then try again.",
        ),
        _ => RuntimeError::new(
            "reader_unavailable",
            "The reader library could not search the network. Check the path to ECRFID.dll on this computer.",
        ),
    })
}

const BAD_PATH: &str = "Enter the full path to the ECRFID reader library on this computer.";

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(n: u8) -> WireTag {
        WireTag {
            uid: [n, 0, 0, 0, 0, 0, 4, 0xE0],
            dsfid: 0,
            antenna: None,
        }
    }

    #[test]
    fn a_connection_is_never_a_verified_reader() {
        let info = DeviceInfo {
            adapter: "ecrfid-sdk".into(),
            model: Some("EC1101".into()),
            firmware: None,
        };
        assert!(!connected(&info, false).hardware_verified);
        assert!(!tags(&[tag(1)]).hardware_verified);
        assert!(!failed(UNREACHABLE).hardware_verified);
    }

    #[test]
    fn the_uid_list_is_capped_while_the_message_counts_them_all() {
        let many: Vec<WireTag> = (0..25).map(tag).collect();
        let view = tags(&many);
        assert_eq!(view.uid_raw_hex.len(), SHOWN_UIDS);
        assert!(view.message.contains("25"), "{}", view.message);
        assert!(view.message.contains("first 20"), "{}", view.message);

        let few = tags(&[tag(1), tag(2)]);
        assert_eq!(few.uid_raw_hex.len(), 2);
        assert!(few.message.contains("2 stickers"));
        assert!(tags(&[tag(1)]).message.contains("1 sticker in"));
        assert!(tags(&[]).message.contains("No sticker"));
    }
}

//! Which reader a station drives and how to reach it.
//!
//! This is persisted configuration: it is validated before it reaches the disk
//! and validated again inside the child process, because the child is a second
//! process with its own copy of the file. No event credential ever appears
//! here — the child is told about a reader, never about an event.

use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The bounded window every reader call must finish inside.
pub const MIN_TIMEOUT_MS: u32 = 100;
pub const MAX_TIMEOUT_MS: u32 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum HardwareConfig {
    /// The legacy `0xEC` protocol spoken directly over TCP. This profile makes
    /// protocol assumptions that no vendor document confirms byte for byte;
    /// see `ec.rs` for the exact candidate rules.
    EcV19PlainTcp {
        address: SocketAddrV4,
        bus_address: u8,
        antenna_byte: bool,
        timeout_ms: u32,
    },
    /// The vendor `ECRFID` SDK, loaded in a separate child process.
    EcrfidSdk {
        dll_path: PathBuf,
        connection: SdkConnection,
        /// The vendor's `TagInventory` mode byte. The address/AIP byte is fixed
        /// at ISO15693 by the adapter and is not configurable.
        inventory_mode: u8,
        timeout_ms: u32,
        /// True only for a profile whose writes passed the disposable-sticker
        /// write test. Set only from that test's screen, never automatically.
        #[serde(default)]
        write_verified: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SdkConnection {
    Hid {
        model: String,
        path: String,
        address_mode: i32,
        exclusive: i32,
    },
    Com {
        model: String,
        port: String,
        baud: i32,
        frame: String,
        bus_address: u8,
    },
    Net {
        model: String,
        interface: String,
        address: SocketAddrV4,
    },
}

impl HardwareConfig {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            HardwareConfig::EcV19PlainTcp {
                address,
                timeout_ms,
                ..
            } => {
                check_timeout(*timeout_ms)?;
                check_unicast(address)
            }
            HardwareConfig::EcrfidSdk {
                dll_path,
                connection,
                timeout_ms,
                ..
            } => {
                check_timeout(*timeout_ms)?;
                check_dll_path(dll_path)?;
                match connection {
                    SdkConnection::Hid {
                        model,
                        path,
                        address_mode,
                        exclusive,
                    } => {
                        check_printable(model, "reader model")?;
                        check_printable(path, "HID device path")?;
                        if *address_mode < 0 {
                            return Err(
                                "The HID address mode cannot be negative. Leave it at 1.".into()
                            );
                        }
                        if *exclusive != 1 {
                            return Err(
                                "Open the HID reader in exclusive mode so the desk is the only program using it."
                                    .into(),
                            );
                        }
                    }
                    SdkConnection::Com {
                        model,
                        port,
                        baud,
                        frame,
                        ..
                    } => {
                        check_printable(model, "reader model")?;
                        check_printable(port, "serial port")?;
                        check_printable(frame, "serial frame")?;
                        if *baud <= 0 {
                            return Err("The serial baud rate must be greater than zero.".into());
                        }
                    }
                    SdkConnection::Net {
                        model,
                        interface,
                        address,
                    } => {
                        check_printable(model, "reader model")?;
                        check_printable(interface, "network interface")?;
                        check_unicast(address)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// The one key two stations may not share. Model, DLL path and deadline are
    /// not identity: what matters is which physical endpoint an operator would
    /// have to walk over to unplug.
    pub fn endpoint_key(&self) -> String {
        match self {
            HardwareConfig::EcV19PlainTcp { address, .. } => net_key(address),
            HardwareConfig::EcrfidSdk { connection, .. } => match connection {
                SdkConnection::Hid { path, .. } => format!("hid:{}", path.to_ascii_lowercase()),
                SdkConnection::Com { port, .. } => format!("com:{}", port.to_uppercase()),
                SdkConnection::Net { address, .. } => net_key(address),
            },
        }
    }

    pub fn timeout_ms(&self) -> u32 {
        match self {
            HardwareConfig::EcV19PlainTcp { timeout_ms, .. } => *timeout_ms,
            HardwareConfig::EcrfidSdk { timeout_ms, .. } => *timeout_ms,
        }
    }

    /// Never derived from a successful connection: only the physical
    /// disposable-tag acceptance tests may turn this on.
    pub fn write_verified(&self) -> bool {
        match self {
            HardwareConfig::EcrfidSdk { write_verified, .. } => *write_verified,
            HardwareConfig::EcV19PlainTcp { .. } => false,
        }
    }

    /// Turn writing on or off for an SDK profile. Only the sticker write test
    /// in Setup calls this, after a sticker passed. Returns false for a
    /// profile that cannot write at all.
    pub fn set_write_verified(&mut self, on: bool) -> bool {
        match self {
            HardwareConfig::EcrfidSdk { write_verified, .. } => {
                *write_verified = on;
                true
            }
            HardwareConfig::EcV19PlainTcp { .. } => false,
        }
    }

    pub fn is_sdk(&self) -> bool {
        matches!(self, HardwareConfig::EcrfidSdk { .. })
    }
}

fn net_key(address: &SocketAddrV4) -> String {
    format!("net:{address}")
}

fn check_timeout(ms: u32) -> Result<(), String> {
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&ms) {
        return Err(format!(
            "The reader timeout must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS} milliseconds."
        ));
    }
    Ok(())
}

/// A reader this app may talk to is a single device on the network, not
/// "everything", not a group and not this computer's default address.
fn check_unicast(address: &SocketAddrV4) -> Result<(), String> {
    if address.port() == 0 {
        return Err("The reader address must include its port, for example 6688.".into());
    }
    let ip = address.ip();
    if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
        return Err(
            "Enter the reader's own IP address, for example 192.168.1.20 — not a broadcast or group address."
                .into(),
        );
    }
    Ok(())
}

fn check_dll_path(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| "The reader library path must be ordinary text.".to_string())?;
    if text.trim().is_empty() || !path.is_absolute() {
        return Err("Enter the full path to the ECRFID reader library on this computer.".into());
    }
    if text.contains('\0') {
        return Err("The reader library path contains a character that cannot be used.".into());
    }
    Ok(())
}

/// Vendor arguments are narrow ANSI strings. A blank one, a control character
/// or anything outside printable ASCII is a mistake worth catching at save
/// time rather than in a native call.
fn check_printable(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("Enter the reader's {label}."));
    }
    if !value.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(format!(
            "The reader's {label} must use ordinary keyboard characters."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute on every host this test runs on, installed nowhere.
    fn dll() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\rfidex\ECRFID.dll")
        } else {
            PathBuf::from("/opt/rfidex/ECRFID.dll")
        }
    }

    fn missing_dll() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\nowhere\ECRFID.dll")
        } else {
            PathBuf::from("/nonexistent/nowhere/ECRFID.dll")
        }
    }

    fn tcp(address: &str, bus_address: u8, antenna_byte: bool, timeout_ms: u32) -> HardwareConfig {
        HardwareConfig::EcV19PlainTcp {
            address: address.parse().unwrap(),
            bus_address,
            antenna_byte,
            timeout_ms,
        }
    }

    fn hid(path: &str) -> HardwareConfig {
        HardwareConfig::EcrfidSdk {
            dll_path: dll(),
            connection: SdkConnection::Hid {
                model: "EC1101".into(),
                path: path.into(),
                address_mode: 1,
                exclusive: 1,
            },
            inventory_mode: 4,
            timeout_ms: 2_000,
            write_verified: false,
        }
    }

    fn com(port: &str) -> HardwareConfig {
        HardwareConfig::EcrfidSdk {
            dll_path: dll(),
            connection: SdkConnection::Com {
                model: "EC1101".into(),
                port: port.into(),
                baud: 38_400,
                frame: "8E1".into(),
                bus_address: 255,
            },
            inventory_mode: 4,
            timeout_ms: 2_000,
            write_verified: false,
        }
    }

    fn net(address: &str) -> HardwareConfig {
        HardwareConfig::EcrfidSdk {
            dll_path: dll(),
            connection: SdkConnection::Net {
                model: "EC1101".into(),
                interface: "192.168.1.10".into(),
                address: address.parse().unwrap(),
            },
            inventory_mode: 4,
            timeout_ms: 2_000,
            write_verified: false,
        }
    }

    fn refusal(config: &HardwareConfig) -> String {
        config.validate().expect_err("this config must be refused")
    }

    #[test]
    fn deadline_bounds_are_inclusive_and_reject_anything_outside() {
        for ms in [100, 2_000, 10_000] {
            assert!(tcp("192.168.1.20:6688", 255, false, ms).validate().is_ok());
            assert!(net("192.168.1.20:6688").validate().is_ok());
        }
        for ms in [0, 1, 99, 10_001, u32::MAX] {
            let short = tcp("192.168.1.20:6688", 255, false, ms);
            assert!(short.validate().is_err(), "{ms} ms must be refused");
            let mut sdk = net("192.168.1.20:6688");
            if let HardwareConfig::EcrfidSdk { timeout_ms, .. } = &mut sdk {
                *timeout_ms = ms;
            }
            assert!(sdk.validate().is_err(), "{ms} ms must be refused");
        }
    }

    #[test]
    fn tcp_address_must_be_a_unicast_endpoint_with_a_port() {
        assert!(tcp("192.168.1.20:6688", 255, false, 500).validate().is_ok());
        assert!(tcp("127.0.0.1:6688", 255, false, 500).validate().is_ok());
        for address in [
            "0.0.0.0:6688",
            "255.255.255.255:6688",
            "239.1.1.1:6688",
            "224.0.0.1:6688",
            "192.168.1.20:0",
            "127.0.0.1:0",
        ] {
            let config = tcp(address, 255, false, 500);
            assert!(config.validate().is_err(), "{address} must be refused");
        }
    }

    #[test]
    fn a_malformed_address_cannot_be_deserialized_at_all() {
        let json = r#"{"transport":"ec_v19_plain_tcp","address":"not-an-address",
            "bus_address":255,"antenna_byte":false,"timeout_ms":500}"#;
        assert!(serde_json::from_str::<HardwareConfig>(json).is_err());
    }

    #[test]
    fn unknown_fields_are_refused_in_both_transports() {
        let tcp_with_extra = r#"{"transport":"ec_v19_plain_tcp","address":"192.168.1.20:6688",
            "bus_address":255,"antenna_byte":false,"timeout_ms":500,"extra":1}"#;
        assert!(serde_json::from_str::<HardwareConfig>(tcp_with_extra).is_err());

        let sdk_with_extra = r#"{"transport":"ecrfid_sdk","dll_path":"/opt/x.dll",
            "connection":{"kind":"com","model":"EC1101","port":"COM3","baud":38400,
            "frame":"8E1","bus_address":255,"extra":1},
            "inventory_mode":4,"timeout_ms":500}"#;
        assert!(serde_json::from_str::<HardwareConfig>(sdk_with_extra).is_err());

        let unknown_transport = r#"{"transport":"ecrfid_udp","address":"192.168.1.20:6688"}"#;
        assert!(serde_json::from_str::<HardwareConfig>(unknown_transport).is_err());
    }

    #[test]
    fn sdk_dll_path_must_be_absolute_and_carry_no_nul() {
        let mut relative = net("192.168.1.20:6688");
        if let HardwareConfig::EcrfidSdk { dll_path, .. } = &mut relative {
            *dll_path = "ECRFID.dll".into();
        }
        assert!(refusal(&relative).contains("full path"));

        let mut empty = net("192.168.1.20:6688");
        if let HardwareConfig::EcrfidSdk { dll_path, .. } = &mut empty {
            *dll_path = "".into();
        }
        assert!(empty.validate().is_err());

        let mut nul = net("192.168.1.20:6688");
        if let HardwareConfig::EcrfidSdk { dll_path, .. } = &mut nul {
            *dll_path = dll().parent().unwrap().join("ECRFID\0.dll");
        }
        assert!(refusal(&nul).contains("cannot be used"));
    }

    #[test]
    fn vendor_strings_must_be_printable_ascii() {
        for bad in ["", "   ", "EC\u{4e2d}\u{6587}", "EC\n1101", "EC\u{7f}"] {
            let config = hid(bad);
            assert!(config.validate().is_err(), "{bad:?} must be refused");
        }
        assert!(hid("\\\\?\\hid#vid_0483&pid_5750#7&1c2f0b3&0&0000")
            .validate()
            .is_ok());

        let mut bad_port = com("COM3");
        if let HardwareConfig::EcrfidSdk {
            connection: SdkConnection::Com { port, .. },
            ..
        } = &mut bad_port
        {
            *port = "COM\u{4e2d}".into();
        }
        assert!(bad_port.validate().is_err());

        let mut bad_frame = com("COM3");
        if let HardwareConfig::EcrfidSdk {
            connection: SdkConnection::Com { frame, .. },
            ..
        } = &mut bad_frame
        {
            *frame = "".into();
        }
        assert!(bad_frame.validate().is_err());
    }

    #[test]
    fn com_baud_must_be_positive_and_hid_must_be_exclusive() {
        let mut zero_baud = com("COM3");
        if let HardwareConfig::EcrfidSdk {
            connection: SdkConnection::Com { baud, .. },
            ..
        } = &mut zero_baud
        {
            *baud = 0;
        }
        assert!(refusal(&zero_baud).contains("baud"));
        let mut negative_baud = com("COM3");
        if let HardwareConfig::EcrfidSdk {
            connection: SdkConnection::Com { baud, .. },
            ..
        } = &mut negative_baud
        {
            *baud = -1;
        }
        assert!(negative_baud.validate().is_err());

        let mut shared = hid("\\\\?\\hid#vid_0483&pid_5750");
        if let HardwareConfig::EcrfidSdk {
            connection: SdkConnection::Hid { exclusive, .. },
            ..
        } = &mut shared
        {
            *exclusive = 0;
        }
        assert!(refusal(&shared).contains("exclusive"));

        let mut negative_mode = hid("\\\\?\\hid#vid_0483&pid_5750");
        if let HardwareConfig::EcrfidSdk {
            connection: SdkConnection::Hid { address_mode, .. },
            ..
        } = &mut negative_mode
        {
            *address_mode = -1;
        }
        assert!(negative_mode.validate().is_err());
    }

    #[test]
    fn endpoint_keys_are_unique_across_transports() {
        assert_eq!(
            tcp("192.168.1.20:6688", 255, false, 500).endpoint_key(),
            net("192.168.1.20:6688").endpoint_key(),
            "one address is one device whatever transport reaches it"
        );
        assert_ne!(
            tcp("192.168.1.20:6688", 255, false, 500).endpoint_key(),
            tcp("192.168.1.20:6689", 255, false, 500).endpoint_key()
        );
        assert_eq!(
            hid("\\\\?\\HID#Vid_0483").endpoint_key(),
            hid("\\\\?\\hid#vid_0483").endpoint_key(),
            "a HID path is the same device whatever its case"
        );
        assert_eq!(com("com3").endpoint_key(), com("COM3").endpoint_key());
        assert_ne!(com("COM3").endpoint_key(), com("COM4").endpoint_key());
        assert_ne!(
            com("COM3").endpoint_key(),
            hid("\\\\?\\hid#vid_0483").endpoint_key(),
            "a HID path and a COM port are never the same device"
        );
        assert_ne!(
            hid("\\\\?\\hid#vid_0483&pid_5750#1").endpoint_key(),
            hid("\\\\?\\hid#vid_0483&pid_5750#2").endpoint_key()
        );
    }

    #[test]
    fn a_missing_dll_or_reader_still_validates() {
        let mut config = hid("\\\\?\\hid#vid_0483&pid_5750");
        if let HardwareConfig::EcrfidSdk { dll_path, .. } = &mut config {
            // Nothing is installed at this path on any host, and that must not
            // block saving the setup: a disconnected reader is a status, not a
            // refusal.
            *dll_path = missing_dll();
        }
        assert!(config.validate().is_ok());
        assert!(tcp("10.44.0.9:6688", 255, false, 500).validate().is_ok());
    }

    #[test]
    fn config_round_trips_through_json() {
        let original = net("192.168.1.20:6688");
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("ecrfid_sdk"));
        let back: HardwareConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.endpoint_key(), original.endpoint_key());
        assert_eq!(back.timeout_ms(), original.timeout_ms());

        let tcp_json = serde_json::to_string(&tcp("192.168.1.20:6688", 7, true, 900)).unwrap();
        assert!(tcp_json.contains("ec_v19_plain_tcp"));
        let back: HardwareConfig = serde_json::from_str(&tcp_json).unwrap();
        assert_eq!(back.endpoint_key(), "net:192.168.1.20:6688");
        assert_eq!(back.timeout_ms(), 900);
    }

    #[test]
    fn write_verification_defaults_to_false_and_never_appears_in_tcp() {
        let json = r#"{"transport":"ecrfid_sdk","dll_path":"/opt/x.dll",
            "connection":{"kind":"com","model":"EC1101","port":"COM3","baud":38400,
            "frame":"8E1","bus_address":255},
            "inventory_mode":4,"timeout_ms":500}"#;
        let config: HardwareConfig = serde_json::from_str(json).unwrap();
        assert!(
            !config.write_verified(),
            "unverified until physical tests pass"
        );

        let mut verified = config.clone();
        if let HardwareConfig::EcrfidSdk { write_verified, .. } = &mut verified {
            *write_verified = true;
        }
        assert!(verified.write_verified());
        assert!(!tcp("192.168.1.20:6688", 255, false, 500).write_verified());
    }
}

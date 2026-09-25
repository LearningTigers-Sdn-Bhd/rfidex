//! Tag identity. Raw UID bytes are kept exactly as a device reported them;
//! `tag_key` is the only place a per-station byte-order rule is applied.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Iso15693,
    Iso14443a,
    #[serde(rename = "iso18000_6c")]
    Iso180006c,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRead {
    pub protocol: Protocol,
    /// Exactly as the adapter received it. Never reordered.
    pub uid_raw: Vec<u8>,
    pub vendor_display: Option<String>,
    pub dsfid: Option<u8>,
    pub antenna: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UidRule {
    #[default]
    AsIs,
    Reversed,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HexError {
    #[error("hex string has odd length")]
    OddLength,
    #[error("invalid hex character {0:?}")]
    InvalidChar(char),
}

pub fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>, HexError> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err(HexError::OddLength);
    }
    s.as_bytes()
        .chunks(2)
        .map(|pair| Ok((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

fn nibble(c: u8) -> Result<u8, HexError> {
    (c as char)
        .to_digit(16)
        .map(|d| d as u8)
        .ok_or(HexError::InvalidChar(c as char))
}

pub fn tag_key(uid_raw: &[u8], rule: UidRule) -> String {
    match rule {
        UidRule::AsIs => hex_upper(uid_raw),
        UidRule::Reversed => {
            let mut v = uid_raw.to_vec();
            v.reverse();
            hex_upper(&v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let raw = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34];
        let h = hex_upper(&raw);
        assert_eq!(h, "E0040150ABCD1234");
        assert_eq!(parse_hex(&h).unwrap(), raw.to_vec());
        assert_eq!(parse_hex("e004").unwrap(), vec![0xE0, 0x04]);
        assert_eq!(parse_hex("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn parse_hex_rejects_bad_input() {
        assert_eq!(parse_hex("E0G1"), Err(HexError::InvalidChar('G')));
        assert_eq!(parse_hex("E00"), Err(HexError::OddLength));
    }

    #[test]
    fn tag_key_applies_rule_without_touching_raw() {
        let raw = vec![0x34, 0x12, 0xCD, 0xAB, 0x50, 0x01, 0x04, 0xE0];
        assert_eq!(tag_key(&raw, UidRule::AsIs), "3412CDAB500104E0");
        assert_eq!(tag_key(&raw, UidRule::Reversed), "E0040150ABCD1234");
        assert_eq!(raw[0], 0x34);
    }

    #[test]
    fn protocol_and_rule_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&Protocol::Iso180006c).unwrap(),
            "\"iso18000_6c\""
        );
        assert_eq!(
            serde_json::to_string(&Protocol::Iso15693).unwrap(),
            "\"iso15693\""
        );
        assert_eq!(serde_json::to_string(&UidRule::AsIs).unwrap(), "\"as_is\"");
    }
}

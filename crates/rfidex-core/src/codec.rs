//! Sticker payload v1. An identifier, not a credential: it carries the same
//! ticket public_id as the QR code and no personal data.
//!
//! Layout (20 bytes): 'R' 'X' | version 0x01 | CRC-8 over bytes 4..20 | public_id (16 bytes)

use uuid::Uuid;

pub const PAYLOAD_LEN: usize = 20;
pub const MAGIC: [u8; 2] = *b"RX";
pub const VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    #[error("payload too short: {0} bytes")]
    TooShort(usize),
    #[error("sticker memory is blank")]
    Blank,
    #[error("not an RfiDex payload")]
    WrongMagic,
    #[error("unsupported payload version {0}")]
    UnsupportedVersion(u8),
    #[error("payload checksum mismatch")]
    BadCrc,
}

/// CRC-8, polynomial 0x07, init 0x00, no reflection (CRC-8/SMBUS). Check value for "123456789" is 0xF4.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn encode(public_id: Uuid) -> [u8; PAYLOAD_LEN] {
    let mut out = [0u8; PAYLOAD_LEN];
    out[0..2].copy_from_slice(&MAGIC);
    out[2] = VERSION;
    out[4..20].copy_from_slice(public_id.as_bytes());
    out[3] = crc8(&out[4..20]);
    out
}

pub fn decode(bytes: &[u8]) -> Result<Uuid, PayloadError> {
    if bytes.len() < PAYLOAD_LEN {
        return Err(PayloadError::TooShort(bytes.len()));
    }
    let b = &bytes[..PAYLOAD_LEN];
    if b.iter().all(|&x| x == 0x00) || b.iter().all(|&x| x == 0xFF) {
        return Err(PayloadError::Blank);
    }
    if b[0..2] != MAGIC {
        return Err(PayloadError::WrongMagic);
    }
    if b[2] != VERSION {
        return Err(PayloadError::UnsupportedVersion(b[2]));
    }
    if crc8(&b[4..20]) != b[3] {
        return Err(PayloadError::BadCrc);
    }
    let id: [u8; 16] = b[4..20].try_into().expect("slice is 16 bytes");
    Ok(Uuid::from_bytes(id))
}

pub fn blocks_needed(block_size: usize) -> usize {
    PAYLOAD_LEN.div_ceil(block_size)
}

pub fn padded(public_id: Uuid, block_size: usize) -> Vec<u8> {
    let mut v = encode(public_id).to_vec();
    v.resize(blocks_needed(block_size) * block_size, 0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag::{hex_upper, parse_hex};

    fn id() -> Uuid {
        Uuid::parse_str("8d6f3a52-0c1e-4b7a-9f2d-6e5c4b3a2918").unwrap()
    }

    #[test]
    fn crc8_check_value() {
        assert_eq!(crc8(b"123456789"), 0xF4);
    }

    #[test]
    fn encode_matches_known_bytes() {
        assert_eq!(
            hex_upper(&encode(id())),
            "525801CA8D6F3A520C1E4B7A9F2D6E5C4B3A2918"
        );
    }

    #[test]
    fn round_trip_with_padding() {
        let mut bytes = padded(id(), 8);
        assert_eq!(bytes.len(), 24);
        assert_eq!(decode(&bytes), Ok(id()));
        bytes.truncate(PAYLOAD_LEN);
        assert_eq!(decode(&bytes), Ok(id()));
    }

    #[test]
    fn torn_write_is_detected() {
        let mut bytes = encode(id()).to_vec();
        for b in &mut bytes[12..20] {
            *b = 0;
        }
        assert_eq!(decode(&bytes), Err(PayloadError::BadCrc));
    }

    #[test]
    fn rejects_blank_foreign_and_future_payloads() {
        assert_eq!(decode(&[0u8; 20]), Err(PayloadError::Blank));
        assert_eq!(decode(&[0xFFu8; 20]), Err(PayloadError::Blank));
        assert_eq!(decode(&[0u8; 19]), Err(PayloadError::TooShort(19)));
        let mut foreign = encode(id());
        foreign[0] = b'Q';
        assert_eq!(decode(&foreign), Err(PayloadError::WrongMagic));
        let mut future = encode(id());
        future[2] = 2;
        assert_eq!(decode(&future), Err(PayloadError::UnsupportedVersion(2)));
        let known = parse_hex("525801CA8D6F3A520C1E4B7A9F2D6E5C4B3A2918").unwrap();
        assert_eq!(decode(&known), Ok(id()));
    }

    #[test]
    fn block_math() {
        assert_eq!(blocks_needed(4), 5);
        assert_eq!(blocks_needed(8), 3);
        assert_eq!(blocks_needed(32), 1);
        assert_eq!(padded(id(), 4).len(), 20);
    }
}

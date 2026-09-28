//! The vendor `ECRFID` SDK boundary.
//!
//! The native library is reached only from a child process, one device context
//! per process, on that process's main thread. Everything here that touches a
//! pointer is from the vendor header `ECRFID.h`; nothing is inferred from the
//! C# demo's integer widths.
//!
//! The buffer rules below are **unverified vendor assumptions**, taken from the
//! demo the guide ships with, not from a documented guarantee:
//!
//! * `TagInventory` takes no output-capacity argument. The demo allocates 96
//!   pointers and this profile does the same. That is a named profile, not a
//!   proof of a maximum, and it is not memory-safety protection.
//! * The system-information layout (`[16]` count, `[17]` size) is the demo's
//!   layout. Both are ISO 15693 "minus one" values: a real EC reader reported
//!   `4F 03` for an 80-block, 4-byte-block sticker (2026-09-28).
//!
//! Both are contained by the child process: a violation crashes the helper, not
//! the app, and the parent records the failure and starts a fresh session.

use crate::wire::{WireError, WireTag};

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;
#[cfg(all(windows, target_arch = "x86_64"))]
pub use windows::SdkReader;

#[cfg(not(all(windows, target_arch = "x86_64")))]
mod other;
#[cfg(not(all(windows, target_arch = "x86_64")))]
pub use other::SdkReader;

/// The vendor's address/technology byte for ISO15693. Fixed here on purpose:
/// this profile never inventories another tag technology.
pub const ISO15693_AIP: u8 = 0x00;

/// The demo's pointer-array size for `TagInventory` and `MeetingGateTakeRecords`.
pub const INVENTORY_SLOTS: usize = 96;

/// The demo's slot count for a one-record gate fetch.
pub const RECORD_SLOTS: usize = 1;

/// The demo's block size for ISO15693 stickers.
pub const BLOCK_SIZE: usize = 4;

/// The most blocks one call may read or write, from the demo's `<= 8` branches.
pub const MAX_BLOCKS_PER_CALL: u8 = 8;

/// `readSecSta`. The demo branches on this value; zero is the branch whose
/// layout this profile implements, and it is the only one used.
pub const READ_SECURITY_NO: u8 = 0;

/// A buffer whose first byte is this is the vendor's end-of-inventory marker,
/// not a tag.
pub const TERMINAL_LENGTH: u8 = 0x07;

/// The shortest buffer that can hold a tag: length, three unknowns, status,
/// DSFID and eight UID bytes.
pub const TAG_MIN_LENGTH: u8 = 15;

/// The shortest read response the demo's arithmetic can describe.
pub const READ_MIN_LENGTH: usize = 17;

/// The shortest system-information response that holds the demo's fields.
pub const GEOMETRY_MIN_LENGTH: usize = 19;

/// How many devices and how many characters an enumeration may return. The
/// vendor array is trusted to be NUL-terminated; its contents are not trusted
/// to be short, so the copy is capped.
pub const MAX_ENUMERATED: usize = 256;
pub const MAX_ENUMERATED_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnumerationKind {
    Hid,
    Com,
    Net,
}

/// One buffer from a `TagInventory` batch.
///
/// `buffer[0]` is the buffer's own length, `[4]` the command status, `[5]` the
/// DSFID and `[6..14]` the UID in the order the reader reported it. A seven-byte
/// buffer is the demo's end marker and is not a tag.
pub fn decode_inventory_buffer(buffer: &[u8]) -> Result<Option<WireTag>, WireError> {
    let Some(&length) = buffer.first() else {
        return Err(WireError::BadResponse);
    };
    if length == 0 || usize::from(length) != buffer.len() {
        return Err(WireError::BadResponse);
    }
    if length != TERMINAL_LENGTH && length < TAG_MIN_LENGTH {
        return Err(WireError::BadResponse);
    }
    if buffer[4] != 0x00 {
        return Err(WireError::BadResponse);
    }
    if length == TERMINAL_LENGTH {
        return Ok(None);
    }
    let mut uid = [0u8; 8];
    uid.copy_from_slice(&buffer[6..14]);
    Ok(Some(WireTag {
        uid,
        dsfid: buffer[5],
        // The demo reads no antenna address on the ISO15693 branch, so there is
        // none to report. The vendor's hardcoded `0x01` is not evidence.
        antenna: None,
    }))
}

/// A read response.
///
/// `buffer[0]` is the frame length. The data region is `[15, len-2)`, a run of
/// five-byte groups: one security byte and four data bytes. The security bytes
/// stay part of the frame this function was given; they are not returned as
/// block data, and a non-zero one is not an error — a locked block is still
/// readable.
pub fn decode_read(buffer: &[u8], uid: &[u8; 8], blocks: u8) -> Result<Vec<u8>, WireError> {
    let Some(&raw_length) = buffer.first() else {
        return Err(WireError::BadResponse);
    };
    let length = usize::from(raw_length);
    if length < READ_MIN_LENGTH || length > buffer.len() {
        return Err(WireError::BadResponse);
    }
    let frame = &buffer[..length];
    if frame[4] != 0x00 {
        return Err(WireError::BadResponse);
    }
    if frame[5..13] != *uid {
        return Err(WireError::BadResponse);
    }
    let region = &frame[15..length - 2];
    if !region.len().is_multiple_of(5) {
        return Err(WireError::BadResponse);
    }
    let groups = region.len() / 5;
    if groups != usize::from(blocks) {
        return Err(WireError::BadResponse);
    }
    let mut data = Vec::with_capacity(groups * BLOCK_SIZE);
    for group in region.as_chunks::<5>().0 {
        data.extend_from_slice(&group[1..5]);
    }
    Ok(data)
}

/// The sticker's geometry from a system-information response.
///
/// The demo reads `receive[4]` as the status, `[16]` as the block count and
/// `[17]` as the block size. ISO 15693 stores both as "value minus one", the
/// size in the low five bits: a real reader answered `4F 03` for an 80-block
/// sticker with 4-byte blocks. Other block sizes are reported as they are;
/// the reader refuses to read or write them.
pub fn decode_geometry(buffer: &[u8]) -> Result<(usize, usize), WireError> {
    if buffer.len() < GEOMETRY_MIN_LENGTH || buffer[4] != 0x00 {
        return Err(WireError::BadResponse);
    }
    let block_count = usize::from(buffer[16]) + 1;
    let block_size = usize::from(buffer[17] & 0x1F) + 1;
    Ok((block_size, block_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_buffer(dsfid: u8, uid: [u8; 8]) -> Vec<u8> {
        let mut buffer = vec![0u8; 15];
        buffer[0] = 15;
        buffer[4] = 0x00;
        buffer[5] = dsfid;
        buffer[6..14].copy_from_slice(&uid);
        buffer
    }

    fn read_buffer(uid: [u8; 8], groups: &[(u8, [u8; 4])]) -> Vec<u8> {
        let mut frame = vec![0u8; 0];
        let region: Vec<u8> = groups
            .iter()
            .flat_map(|(security, data)| {
                let mut group = vec![*security];
                group.extend_from_slice(data);
                group
            })
            .collect();
        // length prefix (1) + address + command + command + status (4) + uid (8)
        // + two bytes the demo skips + region + checksum (2). The region starts
        // at index 15, which is where the demo starts reading it.
        let length = 17 + region.len();
        frame.push(u8::try_from(length).unwrap());
        frame.extend_from_slice(&[0x00, 0xFE, 0x01, 0x00]);
        frame.extend_from_slice(&uid);
        frame.extend_from_slice(&[0x00, 0x00]);
        frame.extend_from_slice(&region);
        frame.extend_from_slice(&[0x00, 0x00]);
        frame
    }

    #[test]
    fn an_inventory_buffer_decodes_to_the_uid_the_vendor_reported() {
        let uid = [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34];
        let tag = decode_inventory_buffer(&tag_buffer(0x07, uid))
            .unwrap()
            .unwrap();
        assert_eq!(tag.uid, uid);
        assert_eq!(tag.dsfid, 0x07);
        assert_eq!(tag.antenna, None);
    }

    #[test]
    fn the_seven_byte_end_marker_is_not_a_tag() {
        let mut marker = vec![0u8; 7];
        marker[0] = 7;
        assert_eq!(decode_inventory_buffer(&marker).unwrap(), None);
    }

    #[test]
    fn terminal_marker_with_failed_status_is_not_empty_inventory() {
        let mut marker = [0u8; 7];
        marker[0] = 7;
        marker[4] = 0x0f;
        assert_eq!(
            decode_inventory_buffer(&marker),
            Err(WireError::BadResponse)
        );
    }

    #[test]
    fn a_buffer_that_is_not_a_tag_is_refused() {
        let uid = [0u8; 8];
        let mut short = tag_buffer(0, uid);
        short[0] = 14;
        short.truncate(14);
        assert_eq!(
            decode_inventory_buffer(&short),
            Err(WireError::BadResponse),
            "a length that does not describe a tag is not guessed at"
        );

        let mut bad_status = tag_buffer(0, uid);
        bad_status[4] = 0x0F;
        assert_eq!(
            decode_inventory_buffer(&bad_status),
            Err(WireError::BadResponse)
        );

        assert_eq!(decode_inventory_buffer(&[]), Err(WireError::BadResponse));
        assert_eq!(
            decode_inventory_buffer(&[0u8; 15]),
            Err(WireError::BadResponse),
            "a zero length byte is not a buffer"
        );
        assert_eq!(
            decode_inventory_buffer(&[15u8; 14]),
            Err(WireError::BadResponse),
            "the length byte must describe the buffer it arrived in"
        );
    }

    #[test]
    fn a_read_returns_only_the_block_bytes() {
        let uid = [1, 2, 3, 4, 5, 6, 7, 8];
        let buffer = read_buffer(
            uid,
            &[
                (0x00, [0xAA, 0xBB, 0xCC, 0xDD]),
                (0x00, [0x11, 0x22, 0x33, 0x44]),
            ],
        );
        assert_eq!(
            decode_read(&buffer, &uid, 2).unwrap(),
            vec![0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44]
        );
    }

    #[test]
    fn a_locked_block_is_still_a_successful_read() {
        let uid = [1, 2, 3, 4, 5, 6, 7, 8];
        let buffer = read_buffer(uid, &[(0x01, [9, 9, 9, 9])]);
        assert_eq!(
            decode_read(&buffer, &uid, 1).unwrap(),
            vec![9, 9, 9, 9],
            "a non-zero security byte reports a lock, not a failed read"
        );
    }

    #[test]
    fn a_read_that_answers_about_another_sticker_is_refused() {
        let uid = [1, 2, 3, 4, 5, 6, 7, 8];
        let other = [8, 7, 6, 5, 4, 3, 2, 1];
        let buffer = read_buffer(other, &[(0x00, [1, 2, 3, 4])]);
        assert_eq!(decode_read(&buffer, &uid, 1), Err(WireError::BadResponse));
    }

    #[test]
    fn a_read_that_is_truncated_or_the_wrong_size_is_refused() {
        let uid = [1, 2, 3, 4, 5, 6, 7, 8];
        let two = read_buffer(uid, &[(0, [1, 2, 3, 4]), (0, [5, 6, 7, 8])]);
        assert_eq!(
            decode_read(&two, &uid, 1),
            Err(WireError::BadResponse),
            "one block was asked for and two came back"
        );
        let truncated = read_buffer(uid, &[(0, [1, 2, 3, 4])]);
        assert_eq!(
            decode_read(&truncated[..10], &uid, 1),
            Err(WireError::BadResponse)
        );
        let mut short_length = read_buffer(uid, &[(0, [1, 2, 3, 4])]);
        short_length[0] = 15;
        assert_eq!(
            decode_read(&short_length, &uid, 1),
            Err(WireError::BadResponse)
        );
    }

    #[test]
    fn geometry_is_iso_minus_one_as_a_real_reader_reports_it() {
        let mut receive = [0u8; 32];
        // What an EC reader reported for an ICODE SLIX2 sticker.
        receive[16] = 0x4F;
        receive[17] = 0x03;
        assert_eq!(decode_geometry(&receive).unwrap(), (4, 80));
        // A 28-block sticker, and the size byte's upper bits are not size.
        receive[16] = 27;
        receive[17] = 0xE3;
        assert_eq!(decode_geometry(&receive).unwrap(), (4, 28));
        // Another block size is reported as it is, for the reader to refuse.
        receive[17] = 0x07;
        assert_eq!(decode_geometry(&receive).unwrap(), (8, 28));
        receive[17] = 0x03;
        receive[4] = 0x0F;
        assert_eq!(decode_geometry(&receive), Err(WireError::BadResponse));
    }
}

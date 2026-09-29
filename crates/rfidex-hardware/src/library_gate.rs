//! Library (security) gate record frames, as `LibraryGateTakeRecords` returns
//! them. Layout from the vendor Java demo 260704 (in/out counters at 7..11 and
//! 11..15, little-endian) and from captures on the installed gates:
//!
//! ```text
//! 0      length of the whole frame
//! 1..4   01 DB 01
//! 4      status (00)
//! 5..7   unknown, changes with no pass
//! 7..11  in counter, little-endian
//! 11..15 out counter, little-endian
//! 15     number of records that follow (absent when there are none)
//! 16..   records of 16 bytes: byte 0 unknown (likely direction),
//!        UID (8, as the desk reads it), byte 9 unknown (likely alarm),
//!        time YY MM DD hh mm ss (6)
//! last 2 checksum
//! ```

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryPass {
    pub uid: [u8; 8],
    pub direction_raw: u8,
    pub alarm_raw: u8,
    pub time_raw: [u8; 6],
}

const HEADER: usize = 16;
const RECORD: usize = 16;
const CHECKSUM: usize = 2;

/// Every pass in one frame. A status-only frame, or anything that does not
/// look like a library-gate frame, yields none.
pub fn passes(frame: &[u8]) -> Vec<LibraryPass> {
    if frame.len() < HEADER + CHECKSUM
        || usize::from(frame[0]) != frame.len()
        || frame[2] != 0xDB
        || frame[4] != 0x00
    {
        return Vec::new();
    }
    let body = &frame[HEADER..frame.len() - CHECKSUM];
    let count = usize::from(frame[15]);
    body.chunks_exact(RECORD)
        .take(count)
        .map(|r| LibraryPass {
            direction_raw: r[0],
            uid: r[1..9].try_into().unwrap(),
            alarm_raw: r[9],
            time_raw: r[10..16].try_into().unwrap(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn a_captured_pass_decodes() {
        let frame = hex("2201DB01000001210000002A0000000100517884CA500104E00026011317353625CD");
        assert_eq!(
            passes(&frame),
            vec![LibraryPass {
                uid: [0x51, 0x78, 0x84, 0xCA, 0x50, 0x01, 0x04, 0xE0],
                direction_raw: 0x00,
                alarm_raw: 0x00,
                time_raw: [0x26, 0x01, 0x13, 0x17, 0x35, 0x36],
            }]
        );
    }

    #[test]
    fn a_captured_status_frame_has_no_pass() {
        assert!(passes(&hex("1101DB01006B6207000000050000003BD4")).is_empty());
    }

    #[test]
    fn a_frame_whose_length_byte_disagrees_is_ignored() {
        assert!(passes(&hex("2201DB01006B6207000000050000003BD4")).is_empty());
    }
}

//! The legacy `0xEC` protocol, as a **candidate**.
//!
//! Everything here is derived from `HF-reader-protocol-v1.9` and the bridge's
//! documented reflected `0xA001` implementation. The guide supplies the CRC
//! seed (`0xEEEE`), the start byte, the length rule and the byte order, but not
//! the polynomial, and no exchange with a real reader has been captured yet.
//! The constants below are therefore labeled candidates, not vendor-confirmed
//! behaviour, and must be re-checked against a captured frame before this
//! profile is called compatible.
//!
//! Only ISO15693 inventory is implemented. The acknowledged UDP variant has
//! extra sequence/ack fields and is deliberately not reachable from here.

/// Start of every frame, both directions.
pub const SOF: u8 = 0xEC;
/// The shortest response the guide shows (a status-only end frame).
pub const MIN_LEN: usize = 7;
/// The ISO15693 command family, inventory command code.
pub const INVENTORY: [u8; 2] = [0xFE, 0x01];
/// End frames allowed to carry no tag: the reader found nothing.
pub const STATUS_OK: u8 = 0x00;
pub const STATUS_NO_TAG: u8 = 0xE1;
/// An exchange that needs more frames than this is not an inventory any more.
pub const MAX_FRAMES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EcError {
    #[error("the reader sent a frame that does not start with 0xEC")]
    BadStart,
    #[error("the reader sent an impossible frame length")]
    BadLength,
    #[error("the reader's checksum did not match")]
    BadChecksum,
    #[error("the reader answered a different command")]
    WrongCommand,
    #[error("the reader answered from a different address")]
    WrongAddress,
    #[error("the reader reported status {0:#04X}")]
    Status(u8),
    #[error("the reader's answer is not the documented inventory layout")]
    UnsupportedLayout,
    #[error("the reader never ended the inventory")]
    NoTerminal,
    #[error("the reader kept sending frames without ending the inventory")]
    TooManyFrames,
    #[error("the frame is longer than the protocol can express")]
    TooLong,
}

/// The candidate checksum: reflected `0xA001`, seed `0xEEEE`, low byte first.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xEEEE_u16;
    for byte in bytes {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Build a request frame. `Len` excludes the start byte and counts everything
/// that follows it, which is the rule the guide's own examples use.
pub fn request(address: u8, command: [u8; 2], data: &[u8]) -> Result<Vec<u8>, EcError> {
    let len = 6_usize.checked_add(data.len()).ok_or(EcError::TooLong)?;
    let length = u8::try_from(len).map_err(|_| EcError::TooLong)?;
    let mut frame = vec![SOF, length, address, command[0], command[1]];
    frame.extend_from_slice(data);
    frame.extend_from_slice(&crc16(&frame[1..]).to_le_bytes());
    Ok(frame)
}

/// The inventory request. `Mode` bit 2 asks the reader for the antenna address
/// alongside each tag; the guide's own example sends `0x00`.
pub fn inventory_request(address: u8, antenna_byte: bool) -> Result<Vec<u8>, EcError> {
    request(
        address,
        INVENTORY,
        &[if antenna_byte { 0x04 } else { 0x00 }],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub address: u8,
    pub command: [u8; 2],
    pub status: u8,
    /// Everything after the status byte and before the checksum.
    pub payload: Vec<u8>,
    pub raw: Vec<u8>,
}

/// Reassembles frames from a byte stream.
///
/// A stream that starts with anything but `SOF`, or whose length field cannot
/// describe a response, is a broken stream: the decoder reports it rather than
/// scanning forward for something that looks like a frame start.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> FrameDecoder {
        FrameDecoder::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// The next complete frame, `None` while more bytes are needed.
    pub fn next_frame(&mut self) -> Option<Result<Frame, EcError>> {
        if self.buffer.is_empty() {
            return None;
        }
        if self.buffer[0] != SOF {
            return Some(Err(EcError::BadStart));
        }
        if self.buffer.len() < 2 {
            return None;
        }
        let len = usize::from(self.buffer[1]);
        if len < MIN_LEN {
            return Some(Err(EcError::BadLength));
        }
        let total = len + 1;
        if self.buffer.len() < total {
            return None;
        }
        let raw: Vec<u8> = self.buffer.drain(..total).collect();
        Some(decode(&raw))
    }
}

fn decode(raw: &[u8]) -> Result<Frame, EcError> {
    let total = raw.len();
    let expected = crc16(&raw[1..total - 2]);
    let got = u16::from_le_bytes([raw[total - 2], raw[total - 1]]);
    if expected != got {
        return Err(EcError::BadChecksum);
    }
    Ok(Frame {
        address: raw[2],
        command: [raw[3], raw[4]],
        status: raw[5],
        payload: raw[6..total - 2].to_vec(),
        raw: raw.to_vec(),
    })
}

/// One inventory exchange: every frame the reader sends until the end frame.
#[derive(Debug)]
pub struct Inventory {
    address: u8,
    broadcast: bool,
    antenna_byte: bool,
    respondent: Option<u8>,
    tags: Vec<crate::wire::WireTag>,
    frames: usize,
    finished: bool,
}

impl Inventory {
    pub fn new(address: u8, antenna_byte: bool) -> Inventory {
        Inventory {
            address,
            broadcast: address == 0xFF,
            antenna_byte,
            respondent: None,
            tags: Vec::new(),
            frames: 0,
            finished: false,
        }
    }

    pub fn tags(&self) -> &[crate::wire::WireTag] {
        &self.tags
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Feed one frame. Returns true once the end frame has arrived.
    pub fn feed(&mut self, frame: Frame) -> Result<bool, EcError> {
        self.frames += 1;
        if self.frames > MAX_FRAMES {
            return Err(EcError::TooManyFrames);
        }
        if frame.command != INVENTORY {
            return Err(EcError::WrongCommand);
        }
        match self.respondent {
            // A broadcast request accepts whoever answers first, but one
            // exchange is one reader: a second address in the same answer is a
            // different device joining, not more tags.
            Some(address) if frame.address != address => return Err(EcError::WrongAddress),
            Some(_) => {}
            None => {
                if !self.broadcast && frame.address != self.address {
                    return Err(EcError::WrongAddress);
                }
                self.respondent = Some(frame.address);
            }
        }

        if frame.payload.is_empty() {
            return match frame.status {
                STATUS_OK => {
                    self.finished = true;
                    Ok(true)
                }
                // "Found nothing" is an end frame only for an exchange that
                // found nothing; after a tag it contradicts itself.
                STATUS_NO_TAG if self.tags.is_empty() => {
                    self.finished = true;
                    Ok(true)
                }
                other => Err(EcError::Status(other)),
            };
        }

        if frame.status != STATUS_OK {
            return Err(EcError::Status(frame.status));
        }
        let expected = if self.antenna_byte { 10 } else { 9 };
        if frame.payload.len() != expected {
            return Err(EcError::UnsupportedLayout);
        }
        let mut uid = [0u8; 8];
        uid.copy_from_slice(&frame.payload[1..9]);
        if !self.tags.iter().any(|tag| tag.uid == uid) {
            self.tags.push(crate::wire::WireTag {
                uid,
                dsfid: frame.payload[0],
                antenna: self.antenna_byte.then(|| frame.payload[9]),
            });
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_body(address: u8, dsfid: u8, uid: &[u8; 8]) -> Vec<u8> {
        let mut body = vec![0x10, address, 0xFE, 0x01, 0x00, dsfid];
        body.extend_from_slice(uid);
        body
    }

    fn framed(body: &[u8]) -> Vec<u8> {
        let mut out = vec![SOF];
        out.extend_from_slice(body);
        out.extend_from_slice(&crc16(body).to_le_bytes());
        out
    }

    #[test]
    fn candidate_inventory_request_vector() {
        assert_eq!(
            request(0xFF, [0xFE, 0x01], &[0]).unwrap(),
            [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x00, 0x38, 0x8B]
        );
        assert_eq!(
            inventory_request(0xFF, true).unwrap(),
            [0xEC, 0x07, 0xFF, 0xFE, 0x01, 0x04, 0x39, 0x48]
        );
        assert_eq!(
            inventory_request(0x03, false).unwrap(),
            [0xEC, 0x07, 0x03, 0xFE, 0x01, 0x00, 0x08, 0xDB]
        );
        assert_eq!(
            request(0x00, [0xFE, 0x01], &[0; 250]).unwrap_err(),
            EcError::TooLong,
            "a length the protocol cannot express is refused before anything is sent"
        );
    }

    #[test]
    fn the_checksum_matches_the_pinned_vector() {
        assert_eq!(crc16(&[0x07, 0xFF, 0xFE, 0x01, 0x00]), 0x8B38);
        assert_eq!(crc16(&[0x07, 0x00, 0xFE, 0x01, 0x00]), 0x9F08);
    }

    #[test]
    fn a_frame_arrives_in_any_number_of_pieces() {
        let bytes = framed(&tag_body(0x00, 0x07, &[1, 2, 3, 4, 5, 6, 7, 8]));
        for split in 0..bytes.len() {
            let mut decoder = FrameDecoder::new();
            decoder.push(&bytes[..split]);
            assert!(decoder.next_frame().is_none(), "split at {split}");
            decoder.push(&bytes[split..]);
            let frame = decoder.next_frame().unwrap().unwrap();
            assert_eq!(frame.address, 0x00);
            assert_eq!(frame.payload, vec![0x07, 1, 2, 3, 4, 5, 6, 7, 8]);
            assert!(decoder.next_frame().is_none());
        }
    }

    #[test]
    fn two_frames_in_one_read_are_two_frames() {
        let mut bytes = framed(&tag_body(0x00, 0x00, &[1, 2, 3, 4, 5, 6, 7, 8]));
        bytes.extend_from_slice(&framed(&[0x07, 0x00, 0xFE, 0x01, 0x00]));
        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes);
        assert_eq!(decoder.next_frame().unwrap().unwrap().payload.len(), 9);
        assert_eq!(decoder.next_frame().unwrap().unwrap().payload.len(), 0);
        assert!(decoder.next_frame().is_none());
    }

    #[test]
    fn a_broken_stream_is_reported_and_never_resynchronised() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x00, 0xEC, 0x07]);
        assert_eq!(decoder.next_frame(), Some(Err(EcError::BadStart)));

        let mut decoder = FrameDecoder::new();
        decoder.push(&[SOF, 0x05, 0x00, 0xFE, 0x01, 0x00]);
        assert_eq!(decoder.next_frame(), Some(Err(EcError::BadLength)));

        let mut decoder = FrameDecoder::new();
        let mut damaged = framed(&[0x07, 0x00, 0xFE, 0x01, 0x00]);
        let last = damaged.len() - 1;
        damaged[last] ^= 0xFF;
        decoder.push(&damaged);
        assert_eq!(decoder.next_frame(), Some(Err(EcError::BadChecksum)));
    }

    #[test]
    fn an_exchange_needs_an_end_frame() {
        let mut inventory = Inventory::new(0xFF, false);
        assert!(!inventory.is_finished());
        let frame = Frame {
            address: 0,
            command: INVENTORY,
            status: STATUS_OK,
            payload: vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
            raw: Vec::new(),
        };
        assert!(!inventory.feed(frame).unwrap());
        assert_eq!(inventory.tags().len(), 1);
        assert!(!inventory.is_finished());
    }

    #[test]
    fn a_repeated_tag_collapses_but_a_different_one_does_not() {
        let mut inventory = Inventory::new(0xFF, false);
        let read = |uid: [u8; 8]| Frame {
            address: 0,
            command: INVENTORY,
            status: STATUS_OK,
            payload: {
                let mut payload = vec![0u8];
                payload.extend_from_slice(&uid);
                payload
            },
            raw: Vec::new(),
        };
        inventory.feed(read([1, 0, 0, 0, 0, 0, 4, 0xE0])).unwrap();
        inventory.feed(read([1, 0, 0, 0, 0, 0, 4, 0xE0])).unwrap();
        inventory.feed(read([2, 0, 0, 0, 0, 0, 4, 0xE0])).unwrap();
        assert_eq!(inventory.tags().len(), 2);
        assert_eq!(inventory.tags()[0].uid, [1, 0, 0, 0, 0, 0, 4, 0xE0]);
    }

    #[test]
    fn a_multi_reader_answer_is_not_one_inventory() {
        let mut inventory = Inventory::new(0xFF, false);
        let read = |address: u8| Frame {
            address,
            command: INVENTORY,
            status: STATUS_OK,
            payload: vec![0; 9],
            raw: Vec::new(),
        };
        inventory.feed(read(0x01)).unwrap();
        assert_eq!(inventory.feed(read(0x02)), Err(EcError::WrongAddress));
    }

    #[test]
    fn an_addressed_request_insists_on_its_own_address() {
        let mut inventory = Inventory::new(0x03, false);
        let frame = Frame {
            address: 0x04,
            command: INVENTORY,
            status: STATUS_OK,
            payload: Vec::new(),
            raw: Vec::new(),
        };
        assert_eq!(inventory.feed(frame), Err(EcError::WrongAddress));
    }

    #[test]
    fn the_frame_cap_ends_a_reader_that_never_stops() {
        let mut inventory = Inventory::new(0xFF, false);
        let read = Frame {
            address: 0,
            command: INVENTORY,
            status: STATUS_OK,
            payload: vec![0, 0, 0, 0, 0, 0, 0, 0, 0],
            raw: Vec::new(),
        };
        for _ in 0..MAX_FRAMES {
            inventory.feed(read.clone()).unwrap();
        }
        assert_eq!(inventory.feed(read), Err(EcError::TooManyFrames));
    }

    #[test]
    fn layouts_without_a_rule_are_refused() {
        let mut inventory = Inventory::new(0xFF, false);
        let network = Frame {
            address: 0,
            command: INVENTORY,
            status: STATUS_OK,
            payload: vec![0xAA, 0xBB, 0, 0, 1, 2, 3, 4, 5, 6, 7],
            raw: Vec::new(),
        };
        assert_eq!(
            inventory.feed(network),
            Err(EcError::UnsupportedLayout),
            "two extra flag bytes are not an ISO15693 UID"
        );

        let mut inventory = Inventory::new(0xFF, false);
        let with_antenna = Frame {
            address: 0,
            command: INVENTORY,
            status: STATUS_OK,
            payload: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            raw: Vec::new(),
        };
        assert_eq!(
            inventory.feed(with_antenna),
            Err(EcError::UnsupportedLayout),
            "an antenna byte the profile did not ask for"
        );
    }
}

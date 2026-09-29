//! The parent/child protocol.
//!
//! One request at a time over a loopback socket, four-byte big-endian length
//! prefix then JSON, hard-capped so a broken peer cannot ask either side to
//! allocate an unbounded buffer. Every read is bounded by an absolute deadline
//! fixed when the exchange started, not by a timeout renewed per byte.

use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::HardwareConfig;

/// The largest frame either side will send or accept.
pub const MAX_FRAME: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Open {
        config: HardwareConfig,
    },
    Info,
    Inventory,
    Memory {
        uid: [u8; 8],
    },
    Read {
        uid: [u8; 8],
        start: u8,
        count: u8,
    },
    Write {
        uid: [u8; 8],
        start: u8,
        data: Vec<u8>,
    },
    RawRecords,
    /// Library (security) gate records. `flag` 0x02 starts a fetch; 0x01
    /// acknowledges the record returned by the previous call and asks for
    /// the next, as the vendor demo polls.
    LibraryRecords {
        flag: u8,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub id: u64,
    pub result: Result<Response, WireError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Unit,
    Info {
        model: Option<String>,
        firmware: Option<String>,
        raw: Vec<u8>,
    },
    Tags {
        tags: Vec<WireTag>,
    },
    Memory {
        block_size: usize,
        block_count: usize,
        raw: Vec<u8>,
    },
    Bytes {
        data: Vec<u8>,
    },
    Records {
        raw: Vec<Vec<u8>>,
    },
    /// The bounded result of a device enumeration, which is a startup command
    /// of its own rather than an operation on an open reader.
    Strings {
        values: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireTag {
    pub uid: [u8; 8],
    pub dsfid: u8,
    pub antenna: Option<u8>,
}

/// A failure the child can name without shipping a vendor error string back
/// across the boundary. `Sdk` carries the vendor return code only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum WireError {
    #[error("the reader is not connected")]
    Disconnected,
    #[error("no tag in the reader field")]
    NoTag,
    #[error("that block range is outside the tag")]
    OutOfRange,
    #[error("writing is not enabled for this reader")]
    WriteUnsupported,
    #[error("this build or this mode does not offer that")]
    Unsupported,
    #[error("the reader or the helper process sent something unreadable")]
    BadResponse,
    #[error("the reader did not answer in time")]
    Timeout,
    #[error("the reader library returned {0}")]
    Sdk(i32),
}

/// An absolute point in time by which an exchange must be over.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Instant,
}

impl Deadline {
    pub fn started(timeout_ms: u32) -> Deadline {
        let now = Instant::now();
        Deadline {
            at: now
                .checked_add(Duration::from_millis(u64::from(timeout_ms)))
                .unwrap_or(now),
        }
    }

    pub fn at(instant: Instant) -> Deadline {
        Deadline { at: instant }
    }

    pub fn remaining(&self) -> Result<Duration, WireError> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(WireError::Timeout)
    }

    pub fn check(&self) -> Result<(), WireError> {
        self.remaining().map(|_| ())
    }
}

/// A TCP stream whose individual socket attempts use the exchange's remaining
/// time, rather than reusing the timeout set before the first byte arrived.
pub struct DeadlineSocket<'a> {
    stream: &'a mut std::net::TcpStream,
    deadline: &'a Deadline,
}

impl<'a> DeadlineSocket<'a> {
    pub fn new(stream: &'a mut std::net::TcpStream, deadline: &'a Deadline) -> Self {
        Self { stream, deadline }
    }

    fn arm(&self) -> std::io::Result<()> {
        let left = self
            .deadline
            .remaining()
            .map_err(|_| std::io::ErrorKind::TimedOut)?;
        if left < Duration::from_millis(1) {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.set_write_timeout(Some(left))
    }
}

impl Read for DeadlineSocket<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.arm()?;
        let count = self.stream.read(buf)?;
        self.deadline
            .check()
            .map_err(|_| std::io::ErrorKind::TimedOut)?;
        Ok(count)
    }
}

impl Write for DeadlineSocket<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.arm()?;
        let count = self.stream.write(buf)?;
        self.deadline
            .check()
            .map_err(|_| std::io::ErrorKind::TimedOut)?;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.arm()?;
        self.stream.flush()?;
        self.deadline
            .check()
            .map_err(|_| std::io::ErrorKind::TimedOut.into())
    }
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let body = serde_json::to_vec(value).map_err(|_| WireError::BadResponse)?;
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(WireError::BadResponse);
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn write_frame<W: Write + ?Sized, T: Serialize>(
    sink: &mut W,
    value: &T,
    deadline: &Deadline,
) -> Result<(), WireError> {
    let bytes = encode_frame(value)?;
    deadline.check()?;
    sink.write_all(&bytes).map_err(io_error)?;
    deadline.check()?;
    sink.flush().map_err(io_error)?;
    deadline.check()
}

/// Read one frame's payload. The length is rejected **before** the body is
/// allocated, so a peer cannot make either side reserve 4 GiB by sending a
/// prefix it never backs with data.
pub fn read_frame_bytes<R: Read + ?Sized>(
    source: &mut R,
    deadline: &Deadline,
) -> Result<Vec<u8>, WireError> {
    let mut prefix = [0u8; 4];
    read_exact(source, &mut prefix, deadline)?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(WireError::BadResponse);
    }
    let mut body = vec![0u8; len];
    read_exact(source, &mut body, deadline)?;
    deadline.check()?;
    Ok(body)
}

pub fn read_frame<R: Read + ?Sized, T: DeserializeOwned>(
    source: &mut R,
    deadline: &Deadline,
) -> Result<T, WireError> {
    let body = read_frame_bytes(source, deadline)?;
    let value = serde_json::from_slice(&body).map_err(|_| WireError::BadResponse)?;
    deadline.check()?;
    Ok(value)
}

pub fn read_request<R: Read + ?Sized>(
    source: &mut R,
    deadline: &Deadline,
) -> Result<Request, WireError> {
    let request = parse_request(&read_frame_bytes(source, deadline)?)?;
    deadline.check()?;
    Ok(request)
}

pub fn read_reply<R: Read + ?Sized>(
    source: &mut R,
    deadline: &Deadline,
) -> Result<Reply, WireError> {
    let reply = parse_reply(&read_frame_bytes(source, deadline)?)?;
    deadline.check()?;
    Ok(reply)
}

/// A request may carry only the fields its own operation defines.
///
/// `serde` applies `deny_unknown_fields` to a struct but silently drops an
/// unknown field inside an internally tagged variant, so the closed shape is
/// checked here against the operation that was actually named. Without this a
/// peer could send fields that look like they did something and did not.
pub fn parse_request(body: &[u8]) -> Result<Request, WireError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| WireError::BadResponse)?;
    let request: Request =
        serde_json::from_value(value.clone()).map_err(|_| WireError::BadResponse)?;
    let canonical = serde_json::to_value(&request.operation).map_err(|_| WireError::BadResponse)?;
    if carries_unknown_key(value.get("operation"), &canonical) {
        return Err(WireError::BadResponse);
    }
    Ok(request)
}

pub fn parse_reply(body: &[u8]) -> Result<Reply, WireError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| WireError::BadResponse)?;
    let reply: Reply = serde_json::from_value(value.clone()).map_err(|_| WireError::BadResponse)?;
    if let Ok(response) = &reply.result {
        let canonical = serde_json::to_value(response).map_err(|_| WireError::BadResponse)?;
        // `result` is externally tagged: `{"Ok": {…}}` or `{"Err": …}`.
        let wrapped = object_of(value.get("result"))
            .and_then(|wrapper| wrapper.get("Ok"))
            .ok_or(WireError::BadResponse)?;
        if carries_unknown_key(Some(wrapped), &canonical) {
            return Err(WireError::BadResponse);
        }
    }
    Ok(reply)
}

fn carries_unknown_key(
    received: Option<&serde_json::Value>,
    canonical: &serde_json::Value,
) -> bool {
    let (Some(received), Some(allowed)) = (object_of(received), object_of(Some(canonical))) else {
        return true;
    };
    received.keys().any(|key| !allowed.contains_key(key))
}

fn object_of(
    value: Option<&serde_json::Value>,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    value.and_then(serde_json::Value::as_object)
}

/// A reply only counts for the request it answers. A stale or mismatched id is
/// a broken channel, not something to guess about.
pub fn match_reply(reply: Reply, expected_id: u64) -> Result<Response, WireError> {
    if reply.id != expected_id {
        return Err(WireError::BadResponse);
    }
    reply.result
}

fn read_exact<R: Read + ?Sized>(
    source: &mut R,
    buf: &mut [u8],
    deadline: &Deadline,
) -> Result<(), WireError> {
    let mut filled = 0;
    while filled < buf.len() {
        deadline.check()?;
        match source.read(&mut buf[filled..]) {
            Ok(0) => return Err(WireError::Disconnected),
            Ok(n) => {
                filled += n;
                deadline.check()?;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                return Err(WireError::Timeout)
            }
            Err(_) => return Err(WireError::Disconnected),
        }
    }
    Ok(())
}

fn io_error(e: std::io::Error) -> WireError {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => WireError::Timeout,
        _ => WireError::Disconnected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct PrefixThenPanic {
        prefix: Vec<u8>,
        at: usize,
    }

    impl Read for PrefixThenPanic {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let left = &self.prefix[self.at..];
            if left.is_empty() {
                panic!("the frame body was read despite an impossible length prefix");
            }
            let n = left.len().min(buf.len());
            buf[..n].copy_from_slice(&left[..n]);
            self.at += n;
            Ok(n)
        }
    }

    #[test]
    fn framing_rejects_oversized_input_before_allocation() {
        let deadline = Deadline::started(1_000);
        let mut over = PrefixThenPanic {
            prefix: 65_537u32.to_be_bytes().to_vec(),
            at: 0,
        };
        assert_eq!(
            read_request(&mut over, &deadline),
            Err(WireError::BadResponse)
        );

        let mut huge = PrefixThenPanic {
            prefix: u32::MAX.to_be_bytes().to_vec(),
            at: 0,
        };
        assert_eq!(
            read_request(&mut huge, &deadline),
            Err(WireError::BadResponse)
        );
    }

    #[test]
    fn framing_rejects_a_zero_length_prefix() {
        let mut reader = Cursor::new(0u32.to_be_bytes().to_vec());
        let deadline = Deadline::started(1_000);
        assert_eq!(
            read_request(&mut reader, &deadline),
            Err(WireError::BadResponse)
        );
    }

    #[test]
    fn framing_rejects_a_truncated_prefix_and_a_truncated_payload() {
        let deadline = Deadline::started(1_000);
        let mut short_prefix = Cursor::new(vec![0u8, 0, 0]);
        assert_eq!(
            read_request(&mut short_prefix, &deadline),
            Err(WireError::Disconnected)
        );

        let mut body = 40u32.to_be_bytes().to_vec();
        body.extend_from_slice(b"{\"id\":1}");
        let mut short_body = Cursor::new(body);
        assert_eq!(
            read_request(&mut short_body, &deadline),
            Err(WireError::Disconnected)
        );
    }

    #[test]
    fn framing_rejects_a_payload_that_is_not_the_command_shape() {
        let deadline = Deadline::started(1_000);
        let bytes = encode_frame(&serde_json::json!({"id": 1, "nonsense": true})).unwrap();
        let mut reader = Cursor::new(bytes);
        assert_eq!(
            read_request(&mut reader, &deadline),
            Err(WireError::BadResponse)
        );
    }

    #[test]
    fn an_expired_deadline_stops_a_read_that_would_otherwise_wait() {
        let deadline = Deadline::at(Instant::now());
        let mut reader = Cursor::new(encode_frame(&64u32).unwrap());
        assert_eq!(
            read_frame::<_, u32>(&mut reader, &deadline),
            Err(WireError::Timeout)
        );
    }

    struct LateFinalRead {
        frame: Vec<u8>,
        first: bool,
    }

    impl Read for LateFinalRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.first {
                self.first = false;
                buf.copy_from_slice(&self.frame[..buf.len()]);
                self.frame.drain(..buf.len());
                return Ok(buf.len());
            }
            std::thread::sleep(Duration::from_millis(80));
            let n = self.frame.len().min(buf.len());
            buf[..n].copy_from_slice(&self.frame[..n]);
            self.frame.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn completed_frame_after_deadline_is_not_accepted() {
        let frame = encode_frame(&42u32).unwrap();
        let mut reader = LateFinalRead { frame, first: true };
        assert_eq!(
            read_frame::<_, u32>(&mut reader, &Deadline::started(25)),
            Err(WireError::Timeout)
        );
    }

    #[test]
    fn request_and_reply_round_trip_through_a_frame() {
        let request = Request {
            id: 7,
            operation: Operation::Read {
                uid: [0xE0, 0x04, 0x01, 0x50, 0xAB, 0xCD, 0x12, 0x34],
                start: 2,
                count: 1,
            },
        };
        let bytes = encode_frame(&request).unwrap();
        assert_eq!(&bytes[..4], &(bytes.len() as u32 - 4).to_be_bytes());
        let mut reader = Cursor::new(bytes);
        let back = read_request(&mut reader, &Deadline::started(1_000)).unwrap();
        assert_eq!(back.id, 7);
        assert!(matches!(
            back.operation,
            Operation::Read {
                start: 2,
                count: 1,
                ..
            }
        ));

        let reply = Reply {
            id: 7,
            result: Ok(Response::Bytes {
                data: vec![1, 2, 3, 4],
            }),
        };
        let bytes = encode_frame(&reply).unwrap();
        let back = read_reply(&mut Cursor::new(bytes), &Deadline::started(1_000)).unwrap();
        assert_eq!(back.id, 7);
        match back.result {
            Ok(Response::Bytes { data }) => assert_eq!(data, vec![1, 2, 3, 4]),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    #[test]
    fn a_reply_for_another_request_is_refused() {
        let reply = Reply {
            id: 8,
            result: Ok(Response::Unit),
        };
        assert_eq!(match_reply(reply, 7), Err(WireError::BadResponse));

        let ok = Reply {
            id: 7,
            result: Ok(Response::Unit),
        };
        assert_eq!(match_reply(ok, 7), Ok(Response::Unit));

        let failed = Reply {
            id: 7,
            result: Err(WireError::NoTag),
        };
        assert_eq!(match_reply(failed, 7), Err(WireError::NoTag));
    }

    #[test]
    fn operation_and_error_spellings_are_stable() {
        let json = serde_json::to_string(&Operation::RawRecords).unwrap();
        assert_eq!(json, r#"{"op":"raw_records"}"#);
        let json = serde_json::to_string(&Operation::Inventory).unwrap();
        assert_eq!(json, r#"{"op":"inventory"}"#);
        let json = serde_json::to_string(&WireError::WriteUnsupported).unwrap();
        assert_eq!(json, r#""write_unsupported""#);
        let json = serde_json::to_string(&WireError::Sdk(-3)).unwrap();
        assert_eq!(json, r#"{"sdk":-3}"#);
        assert_eq!(
            serde_json::from_str::<WireError>(r#""bad_response""#).unwrap(),
            WireError::BadResponse
        );
    }

    #[test]
    fn a_message_with_an_unknown_field_is_refused() {
        let json = br#"{"id":1,"operation":{"op":"inventory"},"extra":1}"#;
        assert_eq!(parse_request(json), Err(WireError::BadResponse));
        let json = br#"{"id":1,"operation":{"op":"read","uid":[0,0,0,0,0,0,0,0],
            "start":0,"count":1,"extra":1}}"#;
        assert_eq!(parse_request(json), Err(WireError::BadResponse));
        let json = br#"{"id":1,"operation":{"op":"write","uid":[0,0,0,0,0,0,0,0],
            "start":0,"data":[1,2,3,4]}}"#;
        assert!(
            parse_request(json).is_ok(),
            "the fields the operation does define are still accepted"
        );

        let reply = Reply {
            id: 1,
            result: Ok(Response::Tags {
                tags: vec![WireTag {
                    uid: [0; 8],
                    dsfid: 0,
                    antenna: None,
                }],
            }),
        };
        let mut body = serde_json::to_value(&reply).unwrap();
        body["result"]["Ok"]["extra"] = serde_json::json!(1);
        let body = serde_json::to_vec(&body).unwrap();
        assert_eq!(parse_reply(&body), Err(WireError::BadResponse));
    }

    #[test]
    fn a_uid_that_is_not_eight_bytes_is_refused() {
        let json = r#"{"id":1,"operation":{"op":"read","uid":[0,0,0],"start":0,"count":1}}"#;
        assert!(serde_json::from_str::<Request>(json).is_err());
    }
}

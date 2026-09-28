//! The disposable-sticker write test (spec §7.1), run from Setup.
//!
//! Every step runs over a one-off commissioning helper, never the desk's own
//! reader session, so the desk's refusal to write stays exactly as it is until
//! the operator turns writing on. The steps only read, write whole blocks and
//! read back: the helper has no lock, password, AFI, DSFID or EAS operation.
//!
//! Results are appended to one local file, so the tally survives restarts and
//! the count toward the spec's targets reflects what was actually run.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rfidex_core::codec;
use rfidex_core::tag::hex_upper;
use rfidex_hardware::wire::{Operation, Response, WireError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The spec's pass targets. The owner decided (2026-09-28) that writing may be
/// turned on before they are reached; the screen always shows the gap.
pub const TARGET_STICKERS: usize = 50;
pub const TARGET_TEARS: usize = 20;

/// The most blocks one helper call may read or write.
const MAX_BLOCKS_PER_CALL: usize = rfidex_hardware::sdk::MAX_BLOCKS_PER_CALL as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickerTestStep {
    /// Only read the tally.
    Status,
    /// The full check on one sticker: geometry, baseline, write, readback,
    /// neighbouring blocks, restore, UID and out-of-range refusal.
    Check,
    /// Write a fresh payload while the operator pulls the sticker away.
    TearWrite,
    /// Read the sticker back after a tear and judge whether it was caught.
    TearCheck,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StickerTally {
    /// Distinct stickers that passed the full check.
    pub passed: usize,
    pub failed: usize,
    /// Writes that were really interrupted.
    pub tears: usize,
    pub tears_undetected: usize,
    pub target_stickers: usize,
    pub target_tears: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StickerTestView {
    pub ok: bool,
    pub message: String,
    pub details: Vec<String>,
    pub tally: StickerTally,
    /// A tear write is waiting for its check.
    pub tear_waiting: bool,
    pub write_enabled: bool,
    pub can_enable: bool,
}

/// What a step concluded, before the tally is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub ok: bool,
    pub message: String,
    pub details: Vec<String>,
    pub record: Option<Record>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub at: DateTime<Utc>,
    pub station: Uuid,
    pub uid: String,
    pub result: RecordResult,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordResult {
    Pass,
    Fail,
    TearDetected,
    TearUndetected,
}

/// A tear write whose sticker has not been read back yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTear {
    pub station: Uuid,
    pub uid: [u8; 8],
    pub start: u8,
    pub block_size: usize,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

pub const NOT_A_SDK_DESK: &str =
    "The sticker write test needs a desk that uses the ECRFID reader library.";
const ONE_STICKER: &str = "Put exactly one sticker marked DISPOSABLE on the reader.";
const HELPER_FAILED: &str = "The reader stopped answering during the test. Nothing was retried. Check the reader and run the test again on a new sticker.";

pub fn results_file(root: &Path) -> PathBuf {
    root.join("sticker-tests.jsonl")
}

/// Every saved result for one station. A line that does not parse is skipped
/// rather than blocking the tally: the file is evidence, not configuration.
pub fn load(root: &Path, station: Uuid) -> Vec<Record> {
    std::fs::read_to_string(results_file(root))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .filter(|record| record.station == station)
        .collect()
}

pub fn append(root: &Path, record: &Record) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(results_file(root))?;
    writeln!(file, "{}", serde_json::to_string(record)?)
}

pub fn tally(records: &[Record]) -> StickerTally {
    let mut passed = std::collections::HashSet::new();
    let mut tally = StickerTally {
        target_stickers: TARGET_STICKERS,
        target_tears: TARGET_TEARS,
        ..StickerTally::default()
    };
    for record in records {
        match record.result {
            RecordResult::Pass => {
                passed.insert(record.uid.as_str());
            }
            RecordResult::Fail => tally.failed += 1,
            RecordResult::TearDetected => tally.tears += 1,
            RecordResult::TearUndetected => {
                tally.tears += 1;
                tally.tears_undetected += 1;
            }
        }
    }
    tally.passed = passed.len();
    tally
}

/// One passed sticker and no failure of any kind.
pub fn can_enable(tally: &StickerTally) -> bool {
    tally.passed >= 1 && tally.failed == 0 && tally.tears_undetected == 0
}

pub fn view(
    outcome: Outcome,
    records: &[Record],
    tear_waiting: bool,
    write_enabled: bool,
) -> StickerTestView {
    let tally = tally(records);
    StickerTestView {
        ok: outcome.ok,
        message: outcome.message,
        details: outcome.details,
        can_enable: can_enable(&tally),
        tally,
        tear_waiting,
        write_enabled,
    }
}

pub fn status_outcome() -> Outcome {
    Outcome {
        ok: true,
        message: String::new(),
        details: Vec::new(),
        record: None,
    }
}

pub fn refused(message: &str) -> Outcome {
    Outcome {
        ok: false,
        message: message.to_string(),
        details: Vec::new(),
        record: None,
    }
}

/// One helper call, as the steps see it.
pub trait Helper {
    fn call(&mut self, operation: Operation) -> Result<Response, WireError>;
}

impl Helper for rfidex_hardware::HardwareClient {
    fn call(&mut self, operation: Operation) -> Result<Response, WireError> {
        rfidex_hardware::HardwareClient::call(self, operation)
    }
}

enum Stop {
    /// The sticker failed: recorded.
    Fail(String),
    /// The step could not run (no sticker, helper gone): not recorded.
    Refuse(String),
}

impl Stop {
    fn message(self) -> String {
        match self {
            Stop::Fail(m) | Stop::Refuse(m) => m,
        }
    }
}

type Step<T> = Result<T, Stop>;

struct Run<'a, H: Helper> {
    helper: &'a mut H,
    station: Uuid,
    details: Vec<String>,
}

impl<H: Helper> Run<'_, H> {
    fn new(helper: &mut H, station: Uuid) -> Run<'_, H> {
        Run {
            helper,
            station,
            details: Vec::new(),
        }
    }

    fn note(&mut self, line: String) {
        self.details.push(line);
    }

    fn one_tag(&mut self) -> Step<[u8; 8]> {
        match self.helper.call(Operation::Inventory) {
            Ok(Response::Tags { tags }) if tags.len() == 1 => Ok(tags[0].uid),
            Ok(Response::Tags { .. }) | Err(WireError::NoTag) => {
                Err(Stop::Refuse(ONE_STICKER.into()))
            }
            _ => Err(Stop::Refuse(HELPER_FAILED.into())),
        }
    }

    fn geometry(&mut self, uid: [u8; 8]) -> Step<(usize, usize)> {
        match self.helper.call(Operation::Memory { uid }) {
            Ok(Response::Memory {
                block_size,
                block_count,
                ..
            }) if block_size > 0 => Ok((block_size, block_count)),
            _ => Err(Stop::Fail(
                "The reader could not report this sticker's memory layout.".into(),
            )),
        }
    }

    /// Read `count` blocks from `start`, in the chunks the helper allows.
    fn read(&mut self, uid: [u8; 8], start: usize, count: usize, size: usize) -> Step<Vec<u8>> {
        let mut data = Vec::with_capacity(count * size);
        let mut block = start;
        while block < start + count {
            let n = (start + count - block).min(MAX_BLOCKS_PER_CALL);
            let first = u8::try_from(block).map_err(|_| {
                Stop::Fail("This sticker has more blocks than the reader can address.".into())
            })?;
            match self.helper.call(Operation::Read {
                uid,
                start: first,
                count: n as u8,
            }) {
                Ok(Response::Bytes { data: chunk }) if chunk.len() == n * size => {
                    data.extend_from_slice(&chunk)
                }
                _ => {
                    return Err(Stop::Fail(format!(
                        "Blocks {block} to {} could not be read.",
                        block + n - 1
                    )))
                }
            }
            block += n;
        }
        Ok(data)
    }

    fn write(&mut self, uid: [u8; 8], start: u8, data: &[u8]) -> Result<(), WireError> {
        match self.helper.call(Operation::Write {
            uid,
            start,
            data: data.to_vec(),
        }) {
            Ok(Response::Unit) => Ok(()),
            Ok(_) => Err(WireError::BadResponse),
            Err(e) => Err(e),
        }
    }

    fn finish(self, uid: &[u8], outcome: Step<String>) -> Outcome {
        let (ok, message, result) = match outcome {
            Ok(message) => (true, message, Some(RecordResult::Pass)),
            Err(Stop::Fail(message)) => (false, message, Some(RecordResult::Fail)),
            Err(Stop::Refuse(message)) => (false, message, None),
        };
        let record = result.map(|result| Record {
            at: Utc::now(),
            station: self.station,
            uid: hex_upper(uid),
            result,
            detail: message.clone(),
        });
        Outcome {
            ok,
            message,
            details: self.details,
            record,
        }
    }
}

/// The full check on one disposable sticker. Order matters: the refusals a
/// reader may answer by ending the session come last, after the sticker has
/// its original contents back.
pub fn check(helper: &mut impl Helper, station: Uuid, start_block: u8) -> Outcome {
    let mut run = Run::new(helper, station);
    let uid = match run.one_tag() {
        Ok(uid) => uid,
        Err(stop) => return refused(&stop.message()),
    };
    let outcome = check_tag(&mut run, uid, start_block);
    run.finish(&uid, outcome)
}

fn check_tag<H: Helper>(run: &mut Run<'_, H>, uid: [u8; 8], start_block: u8) -> Step<String> {
    let (size, count) = run.geometry(uid)?;
    run.note(format!(
        "Sticker {}: {count} blocks of {size} bytes.",
        hex_upper(&uid)
    ));
    let start = usize::from(start_block);
    let blocks = codec::blocks_needed(size);
    // A setting that does not fit is not a sticker failure, so it is not recorded.
    if start + blocks > count {
        return Err(Stop::Refuse(format!(
            "The payload needs {blocks} blocks from block {start}, but this sticker has only {count}. Choose a lower write start block."
        )));
    }
    let baseline = run.read(uid, 0, count, size)?;
    run.note(format!("Saved the original {count} blocks."));
    let original = baseline[start * size..(start + blocks) * size].to_vec();

    let mut pattern = codec::padded(Uuid::new_v4(), size);
    while pattern == original {
        pattern = codec::padded(Uuid::new_v4(), size);
    }
    if run.write(uid, start_block, &pattern).is_err() {
        return Err(Stop::Fail(
            "The test write failed. Do not reuse this sticker.".into(),
        ));
    }
    if run.read(uid, start, blocks, size)? != pattern {
        return Err(Stop::Fail(
            "The sticker did not read back what was written. Writing is not safe on this reader and sticker."
                .into(),
        ));
    }
    run.note(format!(
        "Wrote blocks {start} to {} and read them back exactly.",
        start + blocks - 1
    ));

    let after = run.read(uid, 0, count, size)?;
    let changed: Vec<usize> = (0..count)
        .filter(|b| !(start..start + blocks).contains(b))
        .filter(|b| after[b * size..(b + 1) * size] != baseline[b * size..(b + 1) * size])
        .collect();
    if !changed.is_empty() {
        return Err(Stop::Fail(format!(
            "Blocks outside the payload changed: {changed:?}. Writing is not safe on this reader and sticker."
        )));
    }
    run.note(format!(
        "The other {} blocks are unchanged.",
        count - blocks
    ));

    if run.write(uid, start_block, &original).is_err()
        || run.read(uid, start, blocks, size)? != original
    {
        return Err(Stop::Fail(
            "The original data could not be written back. Rewriting a sticker is not safe yet."
                .into(),
        ));
    }
    run.note("Wrote the original data back and read it back exactly.".into());

    match run.one_tag() {
        Ok(again) if again == uid => run.note("The sticker ID did not change.".into()),
        Ok(_) => {
            return Err(Stop::Fail(
                "The sticker reported a different ID after writing.".into(),
            ))
        }
        Err(_) => {
            return Err(Stop::Fail(
                "The sticker could not be found again after writing.".into(),
            ))
        }
    }

    // Whether the vendor's count means "count" or "highest block" is unknown.
    // A block one past the reported end that answers is that ambiguity showing.
    if let Ok(past) = u8::try_from(count) {
        match run.helper.call(Operation::Read {
            uid,
            start: past,
            count: 1,
        }) {
            Ok(Response::Bytes { .. }) => {
                return Err(Stop::Fail(format!(
                    "Block {count} can be read although the sticker reports {count} blocks. The block count is off by one; ask for help before writing."
                )))
            }
            Err(WireError::Disconnected | WireError::Timeout) => {
                return Err(Stop::Refuse(HELPER_FAILED.into()))
            }
            _ => {}
        }
        match run.write(uid, past, &vec![0u8; size]) {
            Ok(()) => {
                return Err(Stop::Fail(format!(
                    "The reader accepted a write to block {count}, past the end of the sticker."
                )))
            }
            Err(WireError::Disconnected | WireError::Timeout) => {
                return Err(Stop::Refuse(HELPER_FAILED.into()))
            }
            Err(_) => run.note(format!("A write past the end (block {count}) was refused.")),
        }
    }
    Ok("This sticker passed. Put the next disposable sticker on the reader.".into())
}

/// Start one tear trial. The write is sent once and never retried, whatever
/// it answers; the sticker is judged by [`tear_check`].
pub fn tear_write(
    helper: &mut impl Helper,
    station: Uuid,
    start_block: u8,
) -> (Outcome, Option<PendingTear>) {
    let mut run = Run::new(helper, station);
    let prepared = (|| {
        let uid = run.one_tag()?;
        let (size, count) = run.geometry(uid)?;
        let start = usize::from(start_block);
        let blocks = codec::blocks_needed(size);
        if start + blocks > count {
            return Err(Stop::Refuse(
                "The payload does not fit on this sticker from the write start block.".into(),
            ));
        }
        let old = run.read(uid, start, blocks, size)?;
        let mut new = codec::padded(Uuid::new_v4(), size);
        while new == old {
            new = codec::padded(Uuid::new_v4(), size);
        }
        Ok((uid, size, old, new))
    })();
    let (uid, block_size, old, new) = match prepared {
        Ok(ready) => ready,
        Err(stop) => return (refused(&stop.message()), None),
    };
    let finished = run.write(uid, start_block, &new).is_ok();
    let message = if finished {
        "The write finished before the sticker left the field. Put the sticker back and press Check to confirm."
    } else {
        "The write was interrupted. Put the same sticker back on the reader and press Check."
    };
    (
        Outcome {
            ok: true,
            message: message.into(),
            details: Vec::new(),
            record: None,
        },
        Some(PendingTear {
            station,
            uid,
            start: start_block,
            block_size,
            old,
            new,
        }),
    )
}

/// Judge a tear. Caught means the desk would report a failure: the payload does
/// not decode, or the sticker still holds exactly its old contents. Missed
/// means a mixed sticker that still decodes as a valid ticket.
pub fn tear_check(helper: &mut impl Helper, pending: &PendingTear) -> Outcome {
    let mut run = Run::new(helper, pending.station);
    match run.one_tag() {
        Ok(uid) if uid == pending.uid => {}
        Ok(_) => {
            return refused("That is a different sticker. Put the sticker from the tear test back.")
        }
        Err(stop) => return refused(&stop.message()),
    }
    let blocks = pending.new.len() / pending.block_size;
    let data = match run.read(
        pending.uid,
        usize::from(pending.start),
        blocks,
        pending.block_size,
    ) {
        Ok(data) => data,
        Err(stop) => return refused(&stop.message()),
    };
    let (result, message) = if data == pending.new {
        return Outcome {
            ok: true,
            message: "The write had finished: the sticker holds the new data. This does not count as a tear. Try again and pull sooner.".into(),
            details: Vec::new(),
            record: None,
        };
    } else if data == pending.old {
        (
            RecordResult::TearDetected,
            "Tear caught: nothing was written, and the desk would report the write as failed.",
        )
    } else if codec::decode(&data).is_err() {
        (
            RecordResult::TearDetected,
            "Tear caught: the half-written payload fails its checksum, so the desk would report the write as failed.",
        )
    } else {
        (
            RecordResult::TearUndetected,
            "Tear NOT caught: the half-written sticker still reads as a valid ticket. Do not enable writing; ask for help.",
        )
    };
    Outcome {
        ok: result == RecordResult::TearDetected,
        message: message.into(),
        details: Vec::new(),
        record: Some(Record {
            at: Utc::now(),
            station: pending.station,
            uid: hex_upper(&pending.uid),
            result,
            detail: message.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rfidex_hardware::wire::WireTag;

    const UID: [u8; 8] = [0x53, 0xCD, 0x4A, 0x1C, 0x09, 0x01, 0x04, 0xE0];

    /// A 28-block sticker, with switches for the faults each check exists for.
    struct Fake {
        memory: Vec<u8>,
        blocks: usize,
        /// Extra blocks the sticker really has beyond what it reports.
        hidden: usize,
        /// A write also flips this block.
        collateral: Option<usize>,
        /// The next write lands only its first N bytes and then fails.
        tear_after: Option<usize>,
        tags: usize,
        writes: usize,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                memory: (0..29 * 4).map(|i| i as u8).collect(),
                blocks: 28,
                hidden: 0,
                collateral: None,
                tear_after: None,
                tags: 1,
                writes: 0,
            }
        }

        fn real_blocks(&self) -> usize {
            self.blocks + self.hidden
        }
    }

    impl Helper for Fake {
        fn call(&mut self, operation: Operation) -> Result<Response, WireError> {
            match operation {
                Operation::Inventory => Ok(Response::Tags {
                    tags: (0..self.tags)
                        .map(|n| WireTag {
                            uid: if n == 0 { UID } else { [n as u8; 8] },
                            dsfid: 0,
                            antenna: None,
                        })
                        .collect(),
                }),
                Operation::Memory { .. } => Ok(Response::Memory {
                    block_size: 4,
                    block_count: self.blocks,
                    raw: Vec::new(),
                }),
                Operation::Read { start, count, .. } => {
                    let (from, to) = (start as usize, start as usize + count as usize);
                    if count as usize > MAX_BLOCKS_PER_CALL || to > self.real_blocks() {
                        return Err(WireError::OutOfRange);
                    }
                    Ok(Response::Bytes {
                        data: self.memory[from * 4..to * 4].to_vec(),
                    })
                }
                Operation::Write { start, data, .. } => {
                    self.writes += 1;
                    let from = start as usize * 4;
                    if from + data.len() > self.real_blocks() * 4 {
                        return Err(WireError::Sdk(-1));
                    }
                    if let Some(n) = self.tear_after.take() {
                        self.memory[from..from + n].copy_from_slice(&data[..n]);
                        return Err(WireError::Sdk(-2));
                    }
                    self.memory[from..from + data.len()].copy_from_slice(&data);
                    if let Some(block) = self.collateral {
                        self.memory[block * 4] ^= 0xFF;
                    }
                    Ok(Response::Unit)
                }
                _ => Err(WireError::Unsupported),
            }
        }
    }

    fn station() -> Uuid {
        Uuid::from_u128(7)
    }

    #[test]
    fn a_good_sticker_passes_and_gets_its_original_data_back() {
        let mut fake = Fake::new();
        let before = fake.memory.clone();
        let outcome = check(&mut fake, station(), 0);
        assert!(outcome.ok, "{}", outcome.message);
        assert_eq!(outcome.record.unwrap().result, RecordResult::Pass);
        assert_eq!(fake.memory, before, "the sticker is left as it was found");
    }

    #[test]
    fn a_write_that_changes_another_block_fails() {
        let mut fake = Fake::new();
        fake.collateral = Some(20);
        let outcome = check(&mut fake, station(), 0);
        assert!(!outcome.ok);
        assert!(outcome.message.contains("[20]"), "{}", outcome.message);
        assert_eq!(outcome.record.unwrap().result, RecordResult::Fail);
    }

    #[test]
    fn a_block_count_that_is_off_by_one_fails() {
        let mut fake = Fake::new();
        fake.hidden = 1;
        let outcome = check(&mut fake, station(), 0);
        assert!(!outcome.ok);
        assert!(
            outcome.message.contains("off by one"),
            "{}",
            outcome.message
        );
    }

    #[test]
    fn a_payload_that_does_not_fit_fails_before_any_write() {
        let mut fake = Fake::new();
        let outcome = check(&mut fake, station(), 25);
        assert!(!outcome.ok);
        assert!(
            outcome.record.is_none(),
            "a setting problem is not a sticker failure"
        );
        assert_eq!(fake.writes, 0);
    }

    #[test]
    fn no_sticker_or_two_stickers_is_refused_and_not_recorded() {
        for tags in [0, 2] {
            let mut fake = Fake::new();
            fake.tags = tags;
            let outcome = check(&mut fake, station(), 0);
            assert!(!outcome.ok);
            assert!(outcome.record.is_none());
            assert_eq!(fake.writes, 0);
        }
    }

    #[test]
    fn a_torn_payload_is_caught_by_its_checksum() {
        let mut fake = Fake::new();
        fake.tear_after = Some(10);
        let (started, pending) = tear_write(&mut fake, station(), 0);
        assert!(started.message.contains("interrupted"));
        let outcome = tear_check(&mut fake, &pending.unwrap());
        assert!(outcome.ok, "{}", outcome.message);
        assert_eq!(outcome.record.unwrap().result, RecordResult::TearDetected);
    }

    #[test]
    fn a_finished_write_is_not_counted_as_a_tear() {
        let mut fake = Fake::new();
        let (_, pending) = tear_write(&mut fake, station(), 0);
        let outcome = tear_check(&mut fake, &pending.unwrap());
        assert!(outcome.record.is_none());
    }

    #[test]
    fn a_mixed_sticker_that_still_decodes_is_a_missed_tear() {
        let mut fake = Fake::new();
        let (_, pending) = tear_write(&mut fake, station(), 0);
        let mut pending = pending.unwrap();
        // Pretend the sticker held a different valid payload: it decodes, but
        // it is neither what was there nor what was written.
        pending.new = codec::padded(Uuid::new_v4(), 4);
        pending.old = codec::padded(Uuid::new_v4(), 4);
        let outcome = tear_check(&mut fake, &pending);
        assert!(!outcome.ok);
        assert_eq!(outcome.record.unwrap().result, RecordResult::TearUndetected);
    }

    #[test]
    fn the_tally_counts_distinct_stickers_and_gates_enabling() {
        let at = Utc::now();
        let record = |uid: &str, result| Record {
            at,
            station: station(),
            uid: uid.into(),
            result,
            detail: String::new(),
        };
        let mut records = vec![
            record("A", RecordResult::Pass),
            record("A", RecordResult::Pass),
            record("B", RecordResult::TearDetected),
        ];
        let t = tally(&records);
        assert_eq!((t.passed, t.tears, t.failed), (1, 1, 0));
        assert!(can_enable(&t));
        records.push(record("C", RecordResult::TearUndetected));
        assert!(!can_enable(&tally(&records)));
        assert!(!can_enable(&tally(&[record("A", RecordResult::Fail)])));
        assert!(!can_enable(&tally(&[])));
    }

    #[test]
    fn results_round_trip_through_the_file_per_station() {
        let dir = tempfile::tempdir().unwrap();
        let mine = Record {
            at: Utc::now(),
            station: station(),
            uid: "A".into(),
            result: RecordResult::Pass,
            detail: String::new(),
        };
        let other = Record {
            station: Uuid::from_u128(8),
            ..mine.clone()
        };
        append(dir.path(), &mine).unwrap();
        append(dir.path(), &other).unwrap();
        assert_eq!(load(dir.path(), station()), vec![mine]);
    }
}

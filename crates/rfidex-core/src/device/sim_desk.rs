//! Simulated desk reader/writer with fault injection.

use super::{DeskCaps, DeviceError, DeviceInfo, DeviceResult, TagMemory, TagReaderWriter};
use crate::tag::{Protocol, TagRead};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimTag {
    pub uid_raw: Vec<u8>,
    pub memory: Vec<u8>,
    pub block_size: usize,
}

impl SimTag {
    pub fn blank(uid_raw: Vec<u8>, block_size: usize, block_count: usize) -> SimTag {
        SimTag {
            uid_raw,
            memory: vec![0; block_size * block_count],
            block_size,
        }
    }
}

#[derive(Debug, Default)]
pub struct SimDesk {
    pub field: Vec<SimTag>,
    pub connected: bool,
    pub write_supported: bool,
    /// The next N writes fail without touching memory.
    pub fail_next_writes: u32,
    /// The next write stores only the first N bytes, then fails (sticker pulled away).
    pub tear_next_write_after: Option<usize>,
}

impl SimDesk {
    pub fn new() -> SimDesk {
        SimDesk {
            connected: true,
            write_supported: true,
            ..SimDesk::default()
        }
    }

    pub fn place(&mut self, tag: SimTag) {
        self.field.push(tag);
    }

    pub fn clear(&mut self) -> Vec<SimTag> {
        std::mem::take(&mut self.field)
    }

    pub fn tag(&self, uid_raw: &[u8]) -> Option<&SimTag> {
        self.field.iter().find(|t| t.uid_raw == uid_raw)
    }

    fn tag_mut(&mut self, uid_raw: &[u8]) -> DeviceResult<&mut SimTag> {
        if !self.connected {
            return Err(DeviceError::Disconnected);
        }
        self.field
            .iter_mut()
            .find(|t| t.uid_raw == uid_raw)
            .ok_or(DeviceError::TagNotFound)
    }
}

impl TagReaderWriter for SimDesk {
    fn info(&mut self) -> DeviceResult<DeviceInfo> {
        Ok(DeviceInfo {
            adapter: "sim-desk".into(),
            model: Some("SIM".into()),
            firmware: None,
        })
    }

    fn inventory(&mut self) -> DeviceResult<Vec<TagRead>> {
        if !self.connected {
            return Err(DeviceError::Disconnected);
        }
        Ok(self
            .field
            .iter()
            .map(|t| TagRead {
                protocol: Protocol::Iso15693,
                uid_raw: t.uid_raw.clone(),
                vendor_display: None,
                dsfid: Some(0),
                antenna: None,
            })
            .collect())
    }

    fn tag_memory(&mut self, uid_raw: &[u8]) -> DeviceResult<TagMemory> {
        let t = self.tag_mut(uid_raw)?;
        Ok(TagMemory {
            block_size: t.block_size,
            block_count: t.memory.len() / t.block_size,
        })
    }

    fn read_blocks(&mut self, uid_raw: &[u8], start: u8, count: u8) -> DeviceResult<Vec<u8>> {
        let t = self.tag_mut(uid_raw)?;
        let from = start as usize * t.block_size;
        let to = from + count as usize * t.block_size;
        t.memory
            .get(from..to)
            .map(<[u8]>::to_vec)
            .ok_or(DeviceError::OutOfRange)
    }

    fn write_blocks(&mut self, uid_raw: &[u8], start: u8, data: &[u8]) -> DeviceResult<()> {
        if !self.write_supported {
            return Err(DeviceError::WriteUnsupported);
        }
        let fail = self.fail_next_writes > 0;
        if fail {
            self.fail_next_writes -= 1;
        }
        let tear = self.tear_next_write_after.take();
        let t = self.tag_mut(uid_raw)?;
        if !data.len().is_multiple_of(t.block_size) {
            return Err(DeviceError::OutOfRange);
        }
        let from = start as usize * t.block_size;
        let to = from + data.len();
        if to > t.memory.len() {
            return Err(DeviceError::OutOfRange);
        }
        if fail {
            return Err(DeviceError::Other("write failed".into()));
        }
        if let Some(n) = tear {
            let n = n.min(data.len());
            t.memory[from..from + n].copy_from_slice(&data[..n]);
            return Err(DeviceError::Other("tag left the field during write".into()));
        }
        t.memory[from..to].copy_from_slice(data);
        Ok(())
    }

    fn capabilities(&self) -> DeskCaps {
        DeskCaps {
            write_supported: self.write_supported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(n: u8) -> Vec<u8> {
        vec![n, 0, 0, 0, 0, 0, 0x04, 0xE0]
    }

    #[test]
    fn inventory_reports_what_is_in_the_field() {
        let mut d = SimDesk::new();
        assert!(d.inventory().unwrap().is_empty());
        d.place(SimTag::blank(uid(1), 4, 28));
        assert_eq!(d.inventory().unwrap()[0].uid_raw, uid(1));
        d.place(SimTag::blank(uid(2), 4, 28));
        assert_eq!(d.inventory().unwrap().len(), 2);
        d.connected = false;
        assert_eq!(d.inventory(), Err(DeviceError::Disconnected));
    }

    #[test]
    fn write_then_read_back() {
        let mut d = SimDesk::new();
        d.place(SimTag::blank(uid(1), 4, 28));
        assert_eq!(
            d.tag_memory(&uid(1)).unwrap(),
            TagMemory {
                block_size: 4,
                block_count: 28
            }
        );
        d.write_blocks(&uid(1), 2, &[1, 2, 3, 4, 5, 6, 7, 8])
            .unwrap();
        assert_eq!(
            d.read_blocks(&uid(1), 2, 2).unwrap(),
            vec![1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(d.read_blocks(&uid(1), 1, 1).unwrap(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn range_alignment_and_support_errors() {
        let mut d = SimDesk::new();
        d.place(SimTag::blank(uid(1), 4, 4));
        assert_eq!(d.read_blocks(&uid(1), 3, 2), Err(DeviceError::OutOfRange));
        assert_eq!(
            d.write_blocks(&uid(1), 0, &[1, 2, 3]),
            Err(DeviceError::OutOfRange)
        );
        assert_eq!(d.read_blocks(&uid(9), 0, 1), Err(DeviceError::TagNotFound));
        d.write_supported = false;
        assert_eq!(
            d.write_blocks(&uid(1), 0, &[0; 4]),
            Err(DeviceError::WriteUnsupported)
        );
    }

    #[test]
    fn injected_failures_and_tears() {
        let mut d = SimDesk::new();
        d.place(SimTag::blank(uid(1), 4, 8));
        d.fail_next_writes = 1;
        assert!(d.write_blocks(&uid(1), 0, &[9; 8]).is_err());
        assert_eq!(d.read_blocks(&uid(1), 0, 2).unwrap(), vec![0; 8]);
        d.tear_next_write_after = Some(3);
        assert!(d.write_blocks(&uid(1), 0, &[9; 8]).is_err());
        assert_eq!(
            d.read_blocks(&uid(1), 0, 2).unwrap(),
            vec![9, 9, 9, 0, 0, 0, 0, 0]
        );
        d.write_blocks(&uid(1), 0, &[7; 8]).unwrap();
        assert_eq!(d.read_blocks(&uid(1), 0, 2).unwrap(), vec![7; 8]);
    }
}

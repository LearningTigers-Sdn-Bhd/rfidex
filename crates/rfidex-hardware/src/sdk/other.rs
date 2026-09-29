//! The SDK vendor library is only reachable from a Windows x64 build.
//!
//! This module keeps the same shape as the Windows one so the host loop has a
//! single code path, and refuses every call rather than pretending a reader
//! exists. Nothing here loads, walks or frees a pointer.

use std::path::Path;

use super::EnumerationKind;
use crate::config::HardwareConfig;
use crate::wire::{Response, WireError};

/// Never constructed: [`open`](SdkReader::open) always fails here.
pub struct SdkReader {
    _private: (),
}

impl SdkReader {
    pub fn open(_config: &HardwareConfig) -> Result<SdkReader, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn info(&self) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn inventory(&self) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn memory(&self, _uid: &[u8; 8]) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn read(&self, _uid: &[u8; 8], _start: u8, _count: u8) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn write(&self, _uid: &[u8; 8], _start: u8, _data: &[u8]) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn raw_records(&self) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn library_records(&self, _flag: u8) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn library_alarm(&self, _mode: u8) -> Result<Response, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn close(&mut self) -> Result<(), WireError> {
        Ok(())
    }

    pub fn enumerate(_dll_path: &Path, _kind: EnumerationKind) -> Result<Vec<String>, WireError> {
        Err(WireError::Unsupported)
    }

    pub fn discover(_dll_path: &Path, _iface: &str) -> Result<Vec<String>, WireError> {
        Err(WireError::Unsupported)
    }
}

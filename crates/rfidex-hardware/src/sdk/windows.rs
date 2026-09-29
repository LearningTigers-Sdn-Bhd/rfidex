//! The vendor `ECRFID` DLL boundary. Windows x64 only.
//!
//! The signatures below are transcribed from the vendor header `ECRFID.h`, not
//! from the C# demo: the header declares `unsigned char` where the demo declares
//! `uint`, and the adapter follows the header.
//!
//! Ownership rules, all from the demo's own freeing code:
//!
//! * A connection string and each enumerated string are released with
//!   `FreeHGlobal`, and the pointer array of an enumeration with `FreeHGlobal`
//!   as well.
//! * A batch returned by `TagInventory` or `MeetingGateTakeRecords` is released
//!   with exactly one `FreeBufferArray` call for the whole batch. Its members
//!   are never also passed to `FreeHGlobal`.
//!
//! The context stays on the thread that opened it; this type is not `Send`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use super::{
    decode_geometry, decode_inventory_buffer, decode_read, EnumerationKind, BLOCK_SIZE,
    INVENTORY_SLOTS, ISO15693_AIP, MAX_BLOCKS_PER_CALL, MAX_ENUMERATED, MAX_ENUMERATED_BYTES,
    READ_SECURITY_NO, RECORD_SLOTS,
};
use crate::config::{HardwareConfig, SdkConnection};
use crate::wire::{Response, WireError};

/// The vendor library is loaded from beside itself and from System32, and from
/// nowhere else: a DLL dropped in the working directory must not be preferred
/// over the one the operator selected.
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(file: *const u16, reserved: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

type FreeBufferArray = unsafe extern "system" fn(*mut *mut u8, i32) -> i32;
type FreeHGlobal = unsafe extern "system" fn(*mut c_void) -> i32;
type StringList = unsafe extern "system" fn() -> *mut *mut c_char;
type ComConnectString =
    unsafe extern "system" fn(*const c_char, *const c_char, i32, *const c_char, i32) -> *mut c_char;
type HidConnectString =
    unsafe extern "system" fn(*const c_char, *const c_char, i32, i32) -> *mut c_char;
type NetConnectString =
    unsafe extern "system" fn(*const c_char, *const c_char, *const c_char, i32) -> *mut c_char;
type Open = unsafe extern "system" fn(*const c_char) -> *mut c_void;
type Close = unsafe extern "system" fn(*mut c_void) -> i32;
type GetDeviceInfo = unsafe extern "system" fn(*mut c_void, *mut u8) -> i32;
type TagInventory = unsafe extern "system" fn(*mut c_void, *mut *mut u8, u8, u8) -> i32;
type TagSystemInfo = unsafe extern "system" fn(*mut c_void, *const u8, *mut u8) -> i32;
type ReadBlocks = unsafe extern "system" fn(*mut c_void, *const u8, u8, u8, u8, *mut u8) -> i32;
type WriteBlocks =
    unsafe extern "system" fn(*mut c_void, *const u8, u8, u8, *const u8, *mut u8) -> i32;
type TakeRecords = unsafe extern "system" fn(*mut c_void, *mut *mut u8, u8, u8) -> i32;
type LibraryAlarm = unsafe extern "system" fn(*mut c_void, u8, *mut u8) -> i32;
type NetworkDiscovery =
    unsafe extern "system" fn(*mut *mut u8, *const c_char, *const c_char, i32, i32) -> i32;

/// The vendor demo's broadcast target and reader port (`Reader.cs`).
const DISCOVERY_BROADCAST: &str = "255.255.255.255";
pub const DISCOVERY_PORT: i32 = 6688;
const DISCOVERY_TIMEOUT_MS: i32 = 1_000;
/// The demo allocates 10 slots; more slots cost nothing and leave room.
const DISCOVERY_SLOTS: usize = 64;
/// Length byte, four unknown bytes, then IP, mask, gateway and MAC.
const DISCOVERY_MIN_LENGTH: usize = 21;

struct Api {
    free_buffer_array: FreeBufferArray,
    free_h_global: FreeHGlobal,
    get_port_name_list: StringList,
    get_hid_driver_list: StringList,
    get_network_list: StringList,
    get_com_connect_string: ComConnectString,
    get_hid_connect_string: HidConnectString,
    get_net_connect_string: NetConnectString,
    open: Open,
    close: Close,
    get_device_info: GetDeviceInfo,
    tag_inventory: TagInventory,
    tag_system_info: TagSystemInfo,
    read_blocks: ReadBlocks,
    write_blocks: WriteBlocks,
    take_records: TakeRecords,
    library_take_records: TakeRecords,
    library_alarm: LibraryAlarm,
}

impl Api {
    fn load(dll_path: &Path) -> Result<(Library, Api), WireError> {
        // An empty path means the copy installed beside RfiDex. The device host
        // is this same executable, so its folder is the install folder.
        let beside;
        let dll_path = if dll_path.as_os_str().to_string_lossy().trim().is_empty() {
            let exe = std::env::current_exe().map_err(|_| WireError::Disconnected)?;
            beside = exe.with_file_name("ECRFID.dll");
            beside.as_path()
        } else {
            dll_path
        };
        let wide: Vec<u16> = dll_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let module = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(WireError::Disconnected);
        }
        let library = Library { module };
        let api = Api {
            free_buffer_array: symbol(module, "FreeBufferArray")?,
            free_h_global: symbol(module, "FreeHGlobal")?,
            get_port_name_list: symbol(module, "GetPortNameList")?,
            get_hid_driver_list: symbol(module, "GetHIDDriverList")?,
            get_network_list: symbol(module, "GetNetworkList")?,
            get_com_connect_string: symbol(module, "GetComConnectString")?,
            get_hid_connect_string: symbol(module, "GetHidConnectString")?,
            get_net_connect_string: symbol(module, "GetNetConnectString")?,
            open: symbol(module, "Open")?,
            close: symbol(module, "Close")?,
            get_device_info: symbol(module, "GetDeviceInfo")?,
            tag_inventory: symbol(module, "TagInventory")?,
            tag_system_info: symbol(module, "ISO15693_GetTagSystemInfo")?,
            read_blocks: symbol(module, "ISO15693_ReadMultipleBlocks")?,
            write_blocks: symbol(module, "ISO15693_WriteMultipleBlocks")?,
            take_records: symbol(module, "MeetingGateTakeRecords")?,
            library_take_records: symbol(module, "LibraryGateTakeRecords")?,
            library_alarm: symbol(module, "LibraryGateAlarm")?,
        };
        Ok((library, api))
    }
}

fn symbol<T: Copy>(module: *mut c_void, name: &str) -> Result<T, WireError> {
    let name = CString::new(name).map_err(|_| WireError::BadResponse)?;
    let address = unsafe { GetProcAddress(module, name.as_ptr()) };
    if address.is_null() {
        return Err(WireError::Disconnected);
    }
    Ok(unsafe { std::mem::transmute_copy(&address) })
}

struct Library {
    module: *mut c_void,
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.module);
        }
    }
}

/// Caller owns the pointer array on its stack; vendor owns only returned buffers.
struct Batch {
    slots: *mut *mut u8,
    len: i32,
    free: FreeBufferArray,
}

impl Batch {
    fn new(slots: *mut *mut u8, len: i32, free: FreeBufferArray) -> Batch {
        Batch { slots, len, free }
    }

    /// Copy every returned buffer before releasing native storage.
    fn copy(&self, count: usize) -> Result<Vec<Vec<u8>>, WireError> {
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            let pointer = unsafe { *self.slots.add(index) };
            if pointer.is_null() {
                return Err(WireError::BadResponse);
            }
            let length = usize::from(unsafe { std::ptr::read(pointer) });
            if length == 0 {
                return Err(WireError::BadResponse);
            }
            let mut buffer = vec![0u8; length];
            unsafe { std::ptr::copy_nonoverlapping(pointer, buffer.as_mut_ptr(), length) };
            out.push(buffer);
        }
        Ok(out)
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        unsafe { (self.free)(self.slots, self.len) };
    }
}

pub struct SdkReader {
    /// Read by every method; kept before the library so the unload below cannot
    /// happen while a call is still in flight.
    api: Api,
    /// Held only to keep the module loaded, and dropped after the context is
    /// closed. The name says so: nothing else reads it on purpose.
    _library: Library,
    context: *mut c_void,
    /// The connection string `Open` was given, owned here for as long as the
    /// context exists because the vendor keeps a reference to it.
    _connection: CString,
    model: String,
    inventory_mode: u8,
    closed: bool,
}

impl SdkReader {
    pub fn open(config: &HardwareConfig) -> Result<SdkReader, WireError> {
        let HardwareConfig::EcrfidSdk {
            dll_path,
            connection,
            inventory_mode,
            ..
        } = config
        else {
            return Err(WireError::BadResponse);
        };
        config.validate().map_err(|_| WireError::BadResponse)?;
        let (library, api) = Api::load(dll_path)?;
        let (connect_string, model) = build_connection(&api, connection)?;
        let context = unsafe { (api.open)(connect_string.as_ptr()) };
        if context.is_null() {
            return Err(WireError::Disconnected);
        }
        Ok(SdkReader {
            api,
            _library: library,
            context,
            _connection: connect_string,
            model,
            inventory_mode: *inventory_mode,
            closed: false,
        })
    }

    pub fn info(&self) -> Result<Response, WireError> {
        let mut receive = [0u8; 256];
        let rc = unsafe { (self.api.get_device_info)(self.context, receive.as_mut_ptr()) };
        if rc < 0 {
            return Err(WireError::Sdk(rc));
        }
        Ok(Response::Info {
            // The configured model is reported as what it is: what the operator
            // selected. No serial or version field is parsed out of the raw
            // answer, so the detected firmware stays unknown.
            model: Some(self.model.clone()),
            firmware: None,
            raw: receive.to_vec(),
        })
    }

    pub fn inventory(&self) -> Result<Response, WireError> {
        let mut slots: [*mut u8; INVENTORY_SLOTS] = [std::ptr::null_mut(); INVENTORY_SLOTS];
        let count = unsafe {
            (self.api.tag_inventory)(
                self.context,
                slots.as_mut_ptr(),
                ISO15693_AIP,
                self.inventory_mode,
            )
        };
        if count < 0 {
            return Err(WireError::Sdk(count));
        }
        if count == 0 {
            return Ok(Response::Tags { tags: Vec::new() });
        }
        let count = usize::try_from(count).map_err(|_| WireError::BadResponse)?;
        if count > INVENTORY_SLOTS {
            // More buffers than the array can describe. Walking indices this
            // process does not own is exactly the walk that must not happen, so
            // the session ends here and the parent starts a fresh one.
            return Err(WireError::BadResponse);
        }
        let batch = Batch::new(
            slots.as_mut_ptr(),
            i32::try_from(count).map_err(|_| WireError::BadResponse)?,
            self.api.free_buffer_array,
        );
        let buffers = batch.copy(count)?;
        drop(batch);
        let mut tags = Vec::new();
        for buffer in &buffers {
            if let Some(tag) = decode_inventory_buffer(buffer)? {
                tags.push(tag);
            }
        }
        Ok(Response::Tags { tags })
    }

    pub fn memory(&self, uid: &[u8; 8]) -> Result<Response, WireError> {
        let mut receive = [0u8; 32];
        let rc =
            unsafe { (self.api.tag_system_info)(self.context, uid.as_ptr(), receive.as_mut_ptr()) };
        if rc < 0 {
            return Err(WireError::Sdk(rc));
        }
        let (block_size, block_count) = decode_geometry(&receive)?;
        Ok(Response::Memory {
            block_size,
            block_count,
            raw: receive.to_vec(),
        })
    }

    fn checked_range(&self, uid: &[u8; 8], start: u8, count: u8) -> Result<(), WireError> {
        if count == 0 || count > MAX_BLOCKS_PER_CALL {
            return Err(WireError::OutOfRange);
        }
        let Response::Memory {
            block_size,
            block_count,
            ..
        } = self.memory(uid)?
        else {
            return Err(WireError::BadResponse);
        };
        if block_size != BLOCK_SIZE {
            return Err(WireError::Unsupported);
        }
        if usize::from(start) + usize::from(count) > block_count {
            return Err(WireError::OutOfRange);
        }
        Ok(())
    }

    pub fn read(&self, uid: &[u8; 8], start: u8, count: u8) -> Result<Response, WireError> {
        self.checked_range(uid, start, count)?;
        let mut receive = [0u8; 256];
        // The vendor read counts from zero: 0 reads one block, 7 reads eight
        // (demo Reader.cs: "0为读1个块"). Write takes the real count.
        let rc = unsafe {
            (self.api.read_blocks)(
                self.context,
                uid.as_ptr(),
                READ_SECURITY_NO,
                start,
                count - 1,
                receive.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(WireError::Sdk(rc));
        }
        let data = decode_read(&receive, uid, count)?;
        Ok(Response::Bytes { data })
    }

    pub fn write(&self, uid: &[u8; 8], start: u8, data: &[u8]) -> Result<Response, WireError> {
        if data.is_empty() || !data.len().is_multiple_of(BLOCK_SIZE) {
            return Err(WireError::OutOfRange);
        }
        let blocks = data.len() / BLOCK_SIZE;
        let count = u8::try_from(blocks).map_err(|_| WireError::OutOfRange)?;
        if count > MAX_BLOCKS_PER_CALL {
            return Err(WireError::OutOfRange);
        }
        self.checked_range(uid, start, count)?;
        let Response::Tags { tags } = self.inventory()? else {
            return Err(WireError::BadResponse);
        };
        if tags.len() != 1 || tags[0].uid != *uid {
            return Err(WireError::NoTag);
        }
        let mut receive = [0u8; 8];
        let rc = unsafe {
            (self.api.write_blocks)(
                self.context,
                uid.as_ptr(),
                start,
                count,
                data.as_ptr(),
                receive.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(WireError::Sdk(rc));
        }
        if receive[4] != 0x00 {
            return Err(WireError::BadResponse);
        }
        Ok(Response::Unit)
    }

    /// One raw stored-record fetch.
    ///
    /// `flag` is fixed at 0 and `maxcrp` at 1: this profile reads and keeps
    /// bytes, and never asks the reader to delete a batch, initialise its store
    /// or shift anything. Nothing here decodes a record layout.
    pub fn raw_records(&self) -> Result<Response, WireError> {
        self.records(self.api.take_records, 0)
    }

    /// One library (security) gate fetch, one record at most, as the vendor
    /// demo (`HelloController.onLibraryGateTakeRecordsButtonClick`) polls.
    pub fn library_records(&self, flag: u8) -> Result<Response, WireError> {
        self.records(self.api.library_take_records, flag)
    }

    /// The library gate's alarm, as the vendor demo sounds it.
    pub fn library_alarm(&self, mode: u8) -> Result<Response, WireError> {
        let mut receive = [0u8; 16];
        let rc = unsafe { (self.api.library_alarm)(self.context, mode, receive.as_mut_ptr()) };
        if rc < 0 {
            return Err(WireError::Sdk(rc));
        }
        Ok(Response::Unit)
    }

    fn records(&self, take: TakeRecords, flag: u8) -> Result<Response, WireError> {
        let mut slots: [*mut u8; RECORD_SLOTS] = [std::ptr::null_mut(); RECORD_SLOTS];
        let count = unsafe { take(self.context, slots.as_mut_ptr(), flag, 1) };
        if count < 0 {
            return Err(WireError::Sdk(count));
        }
        if count == 0 {
            return Ok(Response::Records { raw: Vec::new() });
        }
        let count = usize::try_from(count).map_err(|_| WireError::BadResponse)?;
        if count > RECORD_SLOTS {
            return Err(WireError::BadResponse);
        }
        let batch = Batch::new(
            slots.as_mut_ptr(),
            i32::try_from(count).map_err(|_| WireError::BadResponse)?,
            self.api.free_buffer_array,
        );
        let raw = batch.copy(count)?;
        drop(batch);
        Ok(Response::Records { raw })
    }

    pub fn close(&mut self) -> Result<(), WireError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let rc = unsafe { (self.api.close)(self.context) };
        if rc < 0 {
            return Err(WireError::Sdk(rc));
        }
        Ok(())
    }

    pub fn enumerate(dll_path: &Path, kind: EnumerationKind) -> Result<Vec<String>, WireError> {
        let (library, api) = Api::load(dll_path)?;
        let head = match kind {
            EnumerationKind::Hid => unsafe { (api.get_hid_driver_list)() },
            EnumerationKind::Com => unsafe { (api.get_port_name_list)() },
            EnumerationKind::Net => unsafe { (api.get_network_list)() },
        };
        if head.is_null() {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        let mut index = 0usize;
        while values.len() < MAX_ENUMERATED {
            let entry = unsafe { *head.add(index) };
            if entry.is_null() {
                break;
            }
            let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
            let bytes = &bytes[..bytes.len().min(MAX_ENUMERATED_BYTES)];
            values.push(String::from_utf8_lossy(bytes).into_owned());
            // Each string first, then the array: the order the demo frees in.
            unsafe { (api.free_h_global)(entry as *mut c_void) };
            index += 1;
        }
        // Whatever was left unread is still released as one array.
        unsafe { (api.free_h_global)(head as *mut c_void) };
        drop(library);
        Ok(values)
    }
}

impl SdkReader {
    /// Find EC readers on one PC network interface by broadcast, as the vendor
    /// demo's `NetworkDiscovery` does. Nothing is opened. Each answer reads
    /// `address=IP:6688;mask=…;gateway=…;mac=…` from the demo's byte offsets, and the
    /// batch is released with one `FreeBufferArray` call.
    pub fn discover(dll_path: &Path, iface: &str) -> Result<Vec<String>, WireError> {
        let (library, api) = Api::load(dll_path)?;
        let discovery: NetworkDiscovery = symbol(library.module, "NetworkDiscovery")?;
        let iface = ansi(iface)?;
        let broadcast = ansi(DISCOVERY_BROADCAST)?;
        let mut slots = [std::ptr::null_mut::<u8>(); DISCOVERY_SLOTS];
        let found = unsafe {
            discovery(
                slots.as_mut_ptr(),
                iface.as_ptr(),
                broadcast.as_ptr(),
                DISCOVERY_PORT,
                DISCOVERY_TIMEOUT_MS,
            )
        };
        if found <= 0 {
            return Ok(Vec::new());
        }
        let found = (found as usize).min(DISCOVERY_SLOTS);
        let mut values = Vec::new();
        for &buffer in &slots[..found] {
            if buffer.is_null() {
                continue;
            }
            let length = usize::from(unsafe { *buffer });
            if length < DISCOVERY_MIN_LENGTH {
                continue;
            }
            let b = unsafe { std::slice::from_raw_parts(buffer, length) };
            values.push(format!(
                "address={}.{}.{}.{}:{DISCOVERY_PORT};mask={}.{}.{}.{};gateway={}.{}.{}.{};mac={:02X}:{:02X}:{:02X}:{:02X}",
                b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15], b[16],
                b[17], b[18], b[19], b[20]
            ));
        }
        unsafe { (api.free_buffer_array)(slots.as_mut_ptr(), found as i32) };
        drop(library);
        Ok(values)
    }
}

impl Drop for SdkReader {
    fn drop(&mut self) {
        // A context that was never closed is closed here, once. A child killed
        // by its parent skips this entirely, which is reported rather than
        // pretended away.
        let _ = self.close();
    }
}

fn build_connection(api: &Api, connection: &SdkConnection) -> Result<(CString, String), WireError> {
    let (native, model) = match connection {
        SdkConnection::Hid {
            model,
            path,
            address_mode,
            exclusive,
        } => {
            let model_ansi = ansi(model)?;
            let path_ansi = ansi(path)?;
            (
                unsafe {
                    (api.get_hid_connect_string)(
                        model_ansi.as_ptr(),
                        path_ansi.as_ptr(),
                        *address_mode,
                        *exclusive,
                    )
                },
                model.clone(),
            )
        }
        SdkConnection::Com {
            model,
            port,
            baud,
            frame,
            bus_address,
        } => {
            let model_ansi = ansi(model)?;
            let port_ansi = ansi(port)?;
            let frame_ansi = ansi(frame)?;
            (
                unsafe {
                    (api.get_com_connect_string)(
                        model_ansi.as_ptr(),
                        port_ansi.as_ptr(),
                        *baud,
                        frame_ansi.as_ptr(),
                        i32::from(*bus_address),
                    )
                },
                model.clone(),
            )
        }
        SdkConnection::Net {
            model,
            interface,
            address,
        } => {
            let model_ansi = ansi(model)?;
            let interface_ansi = ansi(interface)?;
            let ip_ansi = ansi(&address.ip().to_string())?;
            (
                unsafe {
                    (api.get_net_connect_string)(
                        model_ansi.as_ptr(),
                        interface_ansi.as_ptr(),
                        ip_ansi.as_ptr(),
                        i32::from(address.port()),
                    )
                },
                model.clone(),
            )
        }
    };
    if native.is_null() {
        return Err(WireError::Disconnected);
    }
    let bytes = unsafe { CStr::from_ptr(native) }.to_bytes().to_vec();
    // The builder allocates with the SDK allocator. It is released exactly once
    // here, and the copy is what the context keeps.
    unsafe { (api.free_h_global)(native as *mut c_void) };
    let connect_string = CString::new(bytes).map_err(|_| WireError::BadResponse)?;
    Ok((connect_string, model))
}

fn ansi(value: &str) -> Result<CString, WireError> {
    CString::new(value).map_err(|_| WireError::BadResponse)
}

//! The small part of the UEFI interface this boot stage uses.
//!
//! UEFI is the firmware on modern PCs. It starts our program and passes a
//! pointer to its "system table", a structure of pointers to firmware
//! services. Only the fields the boot stage reads are declared here; the
//! layouts come from the UEFI specification, version 2.10: section 4.3
//! (system table) and section 12.4 (simple text output) in this file, and the
//! sections named in the submodules for the rest.
//!
//! - `boot_services`: memory allocation and protocol lookup.
//! - `file`: reading a whole file from the disk the boot stage came from.

use core::ffi::c_void;
use core::ptr::NonNull;

mod boot_services;
mod file;

pub use boot_services::BootServices;
pub use file::FilePath;

/// An opaque firmware object reference.
pub type Handle = *mut c_void;

/// A firmware status code. Zero means success (section 2.3.1, `EFI_STATUS`;
/// the values are in appendix D).
pub type Status = usize;

/// `EFI_SUCCESS`, appendix D.
const SUCCESS: Status = 0;

/// A 128-bit identifier naming a protocol (section 2.3.1, `EFI_GUID`, which
/// is 64-bit aligned; appendix A gives the field layout).
#[repr(C, align(8))]
struct Guid(u32, u16, u16, [u8; 8]);

/// The value the firmware puts at the start of a genuine system table: the
/// ASCII text "IBI SYST" read as a little-endian number.
const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;

/// The header that begins every UEFI table.
#[repr(C)]
pub struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

/// The start of the UEFI system table (section 4.3). Fields after the boot
/// services table are not declared because nothing here reads them.
#[repr(C)]
pub struct SystemTable {
    header: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: Handle,
    console_in: *mut c_void,
    console_out_handle: Handle,
    console_out: *mut SimpleTextOutput,
    standard_error_handle: Handle,
    standard_error: *mut c_void,
    runtime_services: *mut c_void,
    boot_services: *mut boot_services::Table,
}

/// The start of the firmware's text-output interface.
#[repr(C)]
struct SimpleTextOutput {
    reset: unsafe extern "efiapi" fn(this: *mut SimpleTextOutput, extended: bool) -> Status,
    output_string:
        unsafe extern "efiapi" fn(this: *mut SimpleTextOutput, string: *const u16) -> Status,
}

/// The firmware's text console, usable while boot services are running.
pub struct Console {
    out: NonNull<SimpleTextOutput>,
}

impl Console {
    /// How many UTF-16 units are sent to the firmware per call, including the
    /// terminating zero.
    const CHUNK: usize = 64;

    /// Finds the console in the system table.
    ///
    /// Returns `None` if the pointer is null, the table's signature is wrong,
    /// or the firmware provides no console.
    ///
    /// # Safety
    ///
    /// `table` must be null or the pointer the firmware passed to the entry
    /// point, and boot services must still be running for as long as the
    /// returned console is used.
    pub unsafe fn from_system_table(table: *mut SystemTable) -> Option<Self> {
        // SAFETY: the caller guarantees `table` is null or points to the
        // firmware's system table, which stays valid during boot services.
        let table = unsafe { table.as_ref() }?;
        if table.header.signature != SYSTEM_TABLE_SIGNATURE {
            return None;
        }
        NonNull::new(table.console_out).map(|out| Console { out })
    }

    /// Prints text, turning each newline into carriage return plus newline as
    /// the firmware console expects.
    pub fn write_str(&self, text: &str) {
        let mut buffer = [0u16; Self::CHUNK];
        let mut len = 0;
        for unit in text.encode_utf16() {
            if unit == u16::from(b'\n') {
                self.push(&mut buffer, &mut len, u16::from(b'\r'));
            }
            self.push(&mut buffer, &mut len, unit);
        }
        self.flush(&mut buffer, &mut len);
    }

    /// Adds one unit to the buffer, sending the buffer first if it is full.
    fn push(&self, buffer: &mut [u16; Self::CHUNK], len: &mut usize, unit: u16) {
        // Keep the last slot free for the terminating zero.
        if *len == Self::CHUNK - 1 {
            self.flush(buffer, len);
        }
        buffer[*len] = unit;
        *len += 1;
    }

    /// Sends the buffered units to the firmware and empties the buffer.
    fn flush(&self, buffer: &mut [u16; Self::CHUNK], len: &mut usize) {
        if *len == 0 {
            return;
        }
        buffer[*len] = 0;
        // SAFETY: `self.out` came from the system table and boot services are
        // running (see `from_system_table`). `buffer` is zero-terminated at
        // `*len`, which `push` keeps below `CHUNK`.
        unsafe {
            let output_string = self.out.as_ref().output_string;
            // A failed console write cannot be reported anywhere better, and
            // the serial port already carries the boot message.
            let _ = output_string(self.out.as_ptr(), buffer.as_ptr());
        }
        *len = 0;
    }
}

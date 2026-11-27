//! Reading a whole file from the device this program was loaded from. UEFI
//! specification 2.10, section 9.1 (Loaded Image Protocol), section 13.4
//! (Simple File System Protocol) and section 13.5 (File Protocol).

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use super::{BootServices, Guid, Handle, SUCCESS, Status, SystemTable};
use crate::report::{Failure, Step};

/// `EFI_LOADED_IMAGE_PROTOCOL_GUID`, section 9.1.
const LOADED_IMAGE: Guid = Guid(
    0x5B1B_31A1,
    0x9562,
    0x11D2,
    [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
);

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID`, section 13.4.
const SIMPLE_FILE_SYSTEM: Guid = Guid(
    0x964E_5B22,
    0x6459,
    0x11D2,
    [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
);

/// `EFI_NOT_FOUND`, appendix D: error code 14 with the top bit set.
const NOT_FOUND: Status = (1 << (usize::BITS - 1)) | 14;

/// `EFI_FILE_MODE_READ`, section 13.5, `Open()`.
const MODE_READ: u64 = 0x1;

/// The position `SetPosition()` takes to mean the end of the file (section
/// 13.5).
const END_OF_FILE: u64 = u64::MAX;

/// The start of `EFI_LOADED_IMAGE_PROTOCOL` (section 9.1), up to the device
/// the image was loaded from.
#[repr(C)]
struct LoadedImage {
    revision: u32,
    parent_handle: Handle,
    system_table: *mut SystemTable,
    device_handle: Handle,
}

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL` (section 13.4).
#[repr(C)]
struct SimpleFileSystem {
    revision: u64,
    open_volume: Option<
        unsafe extern "efiapi" fn(this: *mut SimpleFileSystem, root: *mut *mut Protocol) -> Status,
    >,
}

/// The start of `EFI_FILE_PROTOCOL` (section 13.5), up to `SetPosition()`.
/// `Delete()` and `Write()` are not called, so they are plain words.
#[repr(C)]
struct Protocol {
    revision: u64,
    open: Option<
        unsafe extern "efiapi" fn(
            this: *mut Protocol,
            new_handle: *mut *mut Protocol,
            file_name: *const u16,
            open_mode: u64,
            attributes: u64,
        ) -> Status,
    >,
    close: Option<unsafe extern "efiapi" fn(this: *mut Protocol) -> Status>,
    _delete: usize,
    read: Option<
        unsafe extern "efiapi" fn(
            this: *mut Protocol,
            buffer_size: *mut usize,
            buffer: *mut c_void,
        ) -> Status,
    >,
    _write: usize,
    get_position:
        Option<unsafe extern "efiapi" fn(this: *mut Protocol, position: *mut u64) -> Status>,
    set_position: Option<unsafe extern "efiapi" fn(this: *mut Protocol, position: u64) -> Status>,
}

/// A file path as the firmware takes it: UCS-2, zero-terminated, `N` units
/// including the zero. Folders are separated by `\`.
pub struct FilePath<const N: usize>([u16; N]);

impl<const N: usize> FilePath<N> {
    /// Converts an ASCII path. Used in a `const`, a wrong `N` or a character
    /// outside ASCII stops the build.
    pub const fn new(ascii: &str) -> Self {
        let bytes = ascii.as_bytes();
        assert!(bytes.len() + 1 == N, "N must be the length plus one");
        let mut units = [0u16; N];
        let mut i = 0;
        while i < bytes.len() {
            assert!(
                bytes[i].is_ascii() && bytes[i] != 0,
                "the path must be ASCII"
            );
            units[i] = bytes[i] as u16;
            i += 1;
        }
        FilePath(units)
    }
}

/// An open file or folder, closed when dropped.
struct File(NonNull<Protocol>);

impl File {
    fn protocol(&self) -> &Protocol {
        // SAFETY: the firmware handed this pointer back from a successful
        // open, and it stays valid until `Close()`, which only `drop` calls.
        unsafe { self.0.as_ref() }
    }

    /// Opens `path` for reading, relative to this folder.
    fn open<const N: usize>(&self, path: &FilePath<N>) -> Result<File, Failure> {
        let open = self
            .protocol()
            .open
            .ok_or(Failure::NoInterface(Step::Open))?;
        let mut new = ptr::null_mut();
        // SAFETY: `open` belongs to this open file; `path` is zero-terminated
        // by construction and `new` is a valid place for the result.
        let status = unsafe { open(self.0.as_ptr(), &mut new, path.0.as_ptr(), MODE_READ, 0) };
        match status {
            SUCCESS => NonNull::new(new)
                .map(File)
                .ok_or(Failure::NoInterface(Step::Open)),
            NOT_FOUND => Err(Failure::Missing),
            other => Err(Failure::Firmware(Step::Open, other)),
        }
    }

    /// Moves to `position`. A folder refuses any position but zero.
    fn set_position(&self, position: u64) -> Result<(), Failure> {
        let set = self
            .protocol()
            .set_position
            .ok_or(Failure::NoInterface(Step::Size))?;
        // SAFETY: `set` belongs to this open file and takes it by value.
        match unsafe { set(self.0.as_ptr(), position) } {
            SUCCESS => Ok(()),
            status => Err(Failure::Firmware(Step::Size, status)),
        }
    }

    /// The file's size: the position at its end. Leaves the position at the
    /// start.
    fn size(&self) -> Result<u64, Failure> {
        self.set_position(END_OF_FILE)?;
        let get = self
            .protocol()
            .get_position
            .ok_or(Failure::NoInterface(Step::Size))?;
        let mut position = 0;
        // SAFETY: `get` belongs to this open file; `position` is a valid
        // place for the result.
        let status = unsafe { get(self.0.as_ptr(), &mut position) };
        if status != SUCCESS {
            return Err(Failure::Firmware(Step::Size, status));
        }
        self.set_position(0)?;
        Ok(position)
    }

    /// Reads exactly `size` bytes from the start of the file into `buffer`.
    ///
    /// Every call must return at least one byte and no more than asked, so
    /// the loop ends after at most `size` calls.
    ///
    /// # Safety
    ///
    /// `buffer` must be valid for writing `size` bytes.
    unsafe fn read_exact(&self, buffer: NonNull<u8>, size: usize) -> Result<(), Failure> {
        let read = self
            .protocol()
            .read
            .ok_or(Failure::NoInterface(Step::Read))?;
        let mut done = 0;
        while done < size {
            let wanted = size - done;
            let mut got = wanted;
            // SAFETY: `read` belongs to this open file. `done < size`, so the
            // destination lies inside `buffer`, which by this function's
            // contract has room for the `wanted` bytes left.
            let status =
                unsafe { read(self.0.as_ptr(), &mut got, buffer.as_ptr().add(done).cast()) };
            if status != SUCCESS {
                return Err(Failure::Firmware(Step::Read, status));
            }
            if got == 0 || got > wanted {
                return Err(Failure::ShortRead {
                    read: done as u64,
                    size: size as u64,
                });
            }
            done += got;
        }
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let Some(close) = self.protocol().close {
            // SAFETY: `close` belongs to this open file, which is not used
            // again. `Close()` always returns success (section 13.5), so the
            // status is not checked.
            let _ = unsafe { close(self.0.as_ptr()) };
        }
    }
}

impl BootServices {
    /// Reads the whole file at `path` on the device this program was loaded
    /// from into memory that is never freed.
    ///
    /// Fails without allocating if the file is missing, empty, or larger
    /// than `limit` bytes.
    pub fn read_file<const N: usize>(
        &self,
        path: &FilePath<N>,
        limit: u64,
    ) -> Result<&'static [u8], Failure> {
        // SAFETY: the GUID names the Loaded Image Protocol, whose interface
        // starts with `LoadedImage` (section 9.1).
        let image = unsafe {
            self.protocol::<LoadedImage>(self.image(), &LOADED_IMAGE, Step::LoadedImage)?
        };
        // SAFETY: the firmware keeps this program's loaded image interface
        // valid while the program runs.
        let device = unsafe { image.as_ref() }.device_handle;
        if device.is_null() {
            return Err(Failure::NoInterface(Step::LoadedImage));
        }
        // SAFETY: the GUID names the Simple File System Protocol, whose
        // interface is `SimpleFileSystem` (section 13.4).
        let file_system = unsafe {
            self.protocol::<SimpleFileSystem>(device, &SIMPLE_FILE_SYSTEM, Step::FileSystem)?
        };
        let root = open_volume(file_system)?;
        let file = root.open(path)?;
        let size = file.size()?;
        let too_large = Failure::TooLarge { size, limit };
        if size == 0 {
            return Err(Failure::Empty);
        }
        if size > limit {
            return Err(too_large);
        }
        let size = usize::try_from(size).map_err(|_| too_large)?;
        let buffer = self.allocate(size)?;
        // SAFETY: `allocate` returned `size` bytes of memory for us alone.
        unsafe { file.read_exact(buffer, size)? };
        // SAFETY: all `size` bytes were written by the read. The memory is
        // never freed and nothing else refers to it, so it may be shared for
        // the rest of the program.
        Ok(unsafe { core::slice::from_raw_parts(buffer.as_ptr(), size) })
    }
}

/// Opens the root folder of `file_system`.
fn open_volume(file_system: NonNull<SimpleFileSystem>) -> Result<File, Failure> {
    // SAFETY: the firmware's file system interface stays valid while boot
    // services run.
    let interface = unsafe { file_system.as_ref() };
    let open_volume = interface
        .open_volume
        .ok_or(Failure::NoInterface(Step::OpenVolume))?;
    let mut root = ptr::null_mut();
    // SAFETY: `open_volume` belongs to this interface; `root` is a valid
    // place for the result (section 13.4).
    let status = unsafe { open_volume(file_system.as_ptr(), &mut root) };
    if status != SUCCESS {
        return Err(Failure::Firmware(Step::OpenVolume, status));
    }
    NonNull::new(root)
        .map(File)
        .ok_or(Failure::NoInterface(Step::OpenVolume))
}

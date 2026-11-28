//! The boot services this stage calls: allocating memory and pages and
//! finding a protocol on a handle. UEFI specification 2.10, section 4.4 (the table),
//! section 7.2 (memory allocation) and section 7.3 (protocol handler
//! services).

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use super::{Guid, Handle, SUCCESS, Status, SystemTable, TableHeader};
use crate::report::{Failure, PlaceFailure, Step};

/// "BOOTSERV" read as a little-endian number, the boot services table's
/// signature (section 4.4).
const SIGNATURE: u64 = 0x5652_4553_544F_4F42;

/// `EfiLoaderData` from `EFI_MEMORY_TYPE` (section 7.2, `AllocatePages()`): memory for the
/// loaded program's data, which the firmware leaves alone afterwards.
const LOADER_DATA: u32 = 2;

/// `AllocateAnyPages` from `EFI_ALLOCATE_TYPE` (section 7.2,
/// `AllocatePages()`): pages anywhere in memory.
const ANY_PAGES: u32 = 0;

/// The size of a page `AllocatePages()` hands out (section 7.2).
const PAGE_SIZE: usize = 4096;

/// `EFI_OPEN_PROTOCOL_GET_PROTOCOL` (section 7.3): only look the interface
/// up. An interface opened this way need not be closed.
const GET_PROTOCOL: u32 = 0x0000_0002;

/// The start of `EFI_BOOT_SERVICES` (section 4.4), up to `OpenProtocol`.
/// Entries this stage does not call are kept as plain words so the offsets
/// stay right. Function pointers are `Option` so a null one can be refused.
#[repr(C)]
pub(super) struct Table {
    header: TableHeader,
    /// `RaiseTPL`, `RestoreTPL`.
    _before_allocate_pages: [usize; 2],
    /// Section 7.2, `AllocatePages()`: the allocation type, the memory type,
    /// the number of pages, and the physical address, read for
    /// `AllocateAddress` and written with the first page's address.
    allocate_pages: Option<
        unsafe extern "efiapi" fn(
            allocate_type: u32,
            memory_type: u32,
            pages: usize,
            memory: *mut u64,
        ) -> Status,
    >,
    /// `FreePages`, `GetMemoryMap`.
    _before_allocate_pool: [usize; 2],
    /// Section 7.2, `AllocatePool()`.
    allocate_pool: Option<
        unsafe extern "efiapi" fn(pool_type: u32, size: usize, buffer: *mut *mut c_void) -> Status,
    >,
    /// `FreePool`, six event and timer services, three protocol interface
    /// services, `HandleProtocol`, a reserved entry, `RegisterProtocolNotify`,
    /// `LocateHandle`, `LocateDevicePath`, `InstallConfigurationTable`, four
    /// image services, `ExitBootServices`, `GetNextMonotonicCount`, `Stall`,
    /// `SetWatchdogTimer`, `ConnectController`, `DisconnectController`.
    _before_open_protocol: [usize; 26],
    /// Section 7.3, `OpenProtocol()`.
    open_protocol: Option<
        unsafe extern "efiapi" fn(
            handle: Handle,
            protocol: *const Guid,
            interface: *mut *mut c_void,
            agent: Handle,
            controller: Handle,
            attributes: u32,
        ) -> Status,
    >,
}

/// The firmware's boot services, usable while they are running, and the
/// handle of this program's image.
pub struct BootServices {
    table: NonNull<Table>,
    image: Handle,
}

impl BootServices {
    /// Finds the boot services in the system table.
    ///
    /// Returns `None` if either pointer is null or either table's signature
    /// is wrong.
    ///
    /// # Safety
    ///
    /// `image` and `table` must be the image handle and the system table
    /// pointer the firmware passed to the entry point (or `table` null), and
    /// boot services must still be running for as long as the returned value
    /// is used.
    pub unsafe fn from_system_table(image: Handle, table: *mut SystemTable) -> Option<Self> {
        // SAFETY: the caller guarantees `table` is null or points to the
        // firmware's system table, and so is its boot services pointer while
        // boot services run (section 4.3).
        let (system, boot) = unsafe {
            let system = table.as_ref()?;
            (system, system.boot_services.as_ref()?)
        };
        if system.header.signature != super::SYSTEM_TABLE_SIGNATURE
            || boot.header.signature != SIGNATURE
            || image.is_null()
        {
            return None;
        }
        Some(BootServices {
            table: NonNull::from(boot),
            image,
        })
    }

    /// The handle of this program's image.
    pub(super) fn image(&self) -> Handle {
        self.image
    }

    fn table(&self) -> &Table {
        // SAFETY: `from_system_table` checked the pointer, and its contract
        // keeps the table valid while `self` is used.
        unsafe { self.table.as_ref() }
    }

    /// Allocates `size` bytes of loader data, 8-byte aligned, which are never
    /// freed. The memory is not initialised.
    pub(super) fn allocate(&self, size: usize) -> Result<NonNull<u8>, Failure> {
        let allocate_pool = self
            .table()
            .allocate_pool
            .ok_or(Failure::NoInterface(Step::Allocate))?;
        let mut buffer = ptr::null_mut();
        // SAFETY: the function comes from the checked table, and `buffer` is
        // a valid place for the firmware to store the address (section
        // 7.2).
        let status = unsafe { allocate_pool(LOADER_DATA, size, &mut buffer) };
        if status != SUCCESS {
            return Err(Failure::Firmware(Step::Allocate, status));
        }
        NonNull::new(buffer.cast()).ok_or(Failure::NoInterface(Step::Allocate))
    }

    /// Allocates `pages` 4 KiB pages of loader data at any address, which
    /// are never freed, for segment `segment` of the kernel, and clears
    /// them. Returns their physical address and their memory.
    ///
    /// Loader data, like the file's buffer: the memory type only tells the
    /// firmware the pages are this program's, and they stay the kernel's
    /// after boot services end. Nothing runs from them under the firmware's
    /// address tables, so loader code would add nothing.
    pub fn allocate_pages(
        &self,
        pages: u64,
        segment: usize,
    ) -> Result<(u64, &'static mut [u8]), PlaceFailure> {
        let no_pages = PlaceFailure::NoPages { segment };
        let allocate_pages = self.table().allocate_pages.ok_or(no_pages)?;
        let count = usize::try_from(pages).map_err(|_| no_pages)?;
        let size = count.checked_mul(PAGE_SIZE).ok_or(no_pages)?;
        let mut physical = 0;
        // SAFETY: the function comes from the checked table, and `physical`
        // is a valid place for the firmware to store the address (section
        // 7.2).
        let status = unsafe { allocate_pages(ANY_PAGES, LOADER_DATA, count, &mut physical) };
        if status != SUCCESS {
            return Err(PlaceFailure::Firmware { segment, status });
        }
        // Section 2.3.4, x64 platforms: memory is identity mapped while boot
        // services run, so the physical address is also a pointer.
        let start = NonNull::new(physical as *mut u8).ok_or(no_pages)?;
        // SAFETY: the firmware has just given this program `size` bytes at
        // `start`, which nothing else uses and which are never freed. They
        // are cleared before the slice is made, so every byte is initialised.
        let memory = unsafe {
            ptr::write_bytes(start.as_ptr(), 0, size);
            core::slice::from_raw_parts_mut(start.as_ptr(), size)
        };
        Ok((physical, memory))
    }

    /// Looks up the interface of protocol `guid` on `handle`.
    ///
    /// # Safety
    ///
    /// `guid` must name a protocol whose interface has the layout `T`.
    pub(super) unsafe fn protocol<T>(
        &self,
        handle: Handle,
        guid: &Guid,
        step: Step,
    ) -> Result<NonNull<T>, Failure> {
        let open_protocol = self
            .table()
            .open_protocol
            .ok_or(Failure::NoInterface(step))?;
        let mut interface = ptr::null_mut();
        // SAFETY: the function comes from the checked table; `guid` and
        // `interface` are valid for the call, and the image handle is the
        // agent. A null controller is allowed for an agent that is not a
        // driver (section 7.3).
        let status = unsafe {
            open_protocol(
                handle,
                guid,
                &mut interface,
                self.image,
                ptr::null_mut(),
                GET_PROTOCOL,
            )
        };
        if status != SUCCESS {
            return Err(Failure::Firmware(step, status));
        }
        NonNull::new(interface.cast()).ok_or(Failure::NoInterface(step))
    }
}

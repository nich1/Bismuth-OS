//! The boot services this stage calls: allocating memory and pages, reading
//! the memory map, finding a protocol on a handle, and ending boot services.
//! UEFI specification 2.10, section 4.4 (the table), section 7.2 (memory
//! allocation and the memory map), section 7.3 (protocol handler services)
//! and section 7.4 (`ExitBootServices()`).

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use bismuth_handoff::Region;

use super::{Console, Guid, Handle, SUCCESS, Status, SystemTable, TableHeader};
use crate::handoff;
use crate::leave::{self, ExitFailure, Map, MapRead};
use crate::memory;
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
    /// `FreePages`.
    _before_get_memory_map: usize,
    /// Section 7.2, `GetMemoryMap()`: the buffer's size, read and written
    /// with the map's size (or the size it needs); the buffer; and places
    /// for the map key, the descriptor size and the descriptor version.
    get_memory_map: Option<GetMemoryMap>,
    /// Section 7.2, `AllocatePool()`.
    allocate_pool: Option<
        unsafe extern "efiapi" fn(pool_type: u32, size: usize, buffer: *mut *mut c_void) -> Status,
    >,
    /// `FreePool` (6); six event and timer services: `CreateEvent`,
    /// `SetTimer`, `WaitForEvent`, `SignalEvent`, `CloseEvent`, `CheckEvent`
    /// (7 to 12); `InstallProtocolInterface`, `ReinstallProtocolInterface`,
    /// `UninstallProtocolInterface`, `HandleProtocol`, a reserved entry,
    /// `RegisterProtocolNotify`, `LocateHandle`, `LocateDevicePath`,
    /// `InstallConfigurationTable` (13 to 21); `LoadImage`, `StartImage`,
    /// `Exit`, `UnloadImage` (22 to 25): 20 entries.
    _before_exit_boot_services: [usize; 20],
    /// Section 7.4, `ExitBootServices()`: this program's image handle and the
    /// key of the current memory map. Entry 26, counting `RaiseTPL` as 0.
    exit_boot_services: Option<ExitBootServices>,
    /// `GetNextMonotonicCount`, `Stall`, `SetWatchdogTimer` (27 to 29),
    /// `ConnectController`, `DisconnectController` (30, 31).
    _before_open_protocol: [usize; 5],
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

/// Section 7.2, `GetMemoryMap()`.
type GetMemoryMap = unsafe extern "efiapi" fn(
    memory_map_size: *mut usize,
    memory_map: *mut u8,
    map_key: *mut usize,
    descriptor_size: *mut usize,
    descriptor_version: *mut u32,
) -> Status;

/// Section 7.4, `ExitBootServices()`.
type ExitBootServices = unsafe extern "efiapi" fn(image: Handle, map_key: usize) -> Status;

// Each function's place in the table: after the 24-byte header, entry `n`
// (counting `RaiseTPL` as 0, in the order of section 4.4) is at
// 24 + 8 * n. A wrong place would call another function.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(Table, allocate_pages) == 24 + 8 * 2);
    assert!(offset_of!(Table, get_memory_map) == 24 + 8 * 4);
    assert!(offset_of!(Table, allocate_pool) == 24 + 8 * 5);
    assert!(offset_of!(Table, exit_boot_services) == 24 + 8 * 26);
    assert!(offset_of!(Table, open_protocol) == 24 + 8 * 32);
};

/// The firmware's boot services, usable while they are running, and the
/// handle of this program's image.
pub struct BootServices {
    table: NonNull<Table>,
    image: Handle,
    /// The system table, read once more after the exit in the test build.
    #[cfg(feature = "qemu-exit")]
    system: NonNull<SystemTable>,
}

/// What [`BootServices::leave`] hands back once boot services have ended:
/// all of it in memory that stays this program's.
pub struct Left {
    /// The memory map as it was when they ended.
    pub map: Map,
    /// Its bytes.
    pub bytes: &'static [u8],
    /// The hand-off builder's working space, allocated before the map was
    /// read, for as many descriptors as its buffer holds.
    pub space: &'static mut [Region],
    /// In the test build, the system table's boot services pointer read
    /// after the exit, which the firmware should have cleared.
    pub boot_services: Option<u64>,
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
            #[cfg(feature = "qemu-exit")]
            system: NonNull::from(system),
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

    /// Reads the memory map and ends boot services, in the order
    /// [`leave::leave`] makes the calls. Takes `self` and the firmware's
    /// console, so no boot service and no console call can be made through
    /// them afterwards: none may be after a success, and after a failed
    /// `ExitBootServices()` only the memory allocation services may be
    /// (section 7.4), and nothing here calls them then.
    pub fn leave(self, _console: Option<Console>) -> Result<Left, ExitFailure> {
        let table = self.table();
        let (Some(get_memory_map), Some(exit_boot_services)) =
            (table.get_memory_map, table.exit_boot_services)
        else {
            return Err(ExitFailure::NoService);
        };
        let mut firmware = Leaving {
            services: &self,
            get_memory_map,
            exit_boot_services,
            buffer: None,
            space: None,
        };
        let map = leave::leave(&mut firmware)?;
        // `leave` succeeded, so it asked for the working space first.
        let space = firmware.space.ok_or(ExitFailure::Space(None))?;
        // SAFETY: `map.address` is the buffer `Leaving::allocate` got from
        // the firmware: loader data, 8-byte aligned, never freed, and only
        // written by `GetMemoryMap()`, which reported `map.size` bytes,
        // which `leave` checked fit it. Boot services have ended, so
        // nothing writes it any more, and the firmware's lower mappings,
        // under which its physical address is its address, are still in
        // the tables in use (`map.rs` keeps them).
        let bytes = unsafe { core::slice::from_raw_parts(map.address as *const u8, map.size) };
        #[cfg(feature = "qemu-exit")]
        // SAFETY: `system` is the system table the firmware passed to the
        // entry point, checked by `from_system_table`. It lies in memory
        // the firmware keeps for its runtime services (section 4.3), which
        // outlives boot services and is still mapped at its address. Read
        // as volatile, since the firmware has just written it, if it did.
        let boot_services = Some(unsafe {
            core::ptr::addr_of!((*self.system.as_ptr()).boot_services).read_volatile()
        } as u64);
        #[cfg(not(feature = "qemu-exit"))]
        let boot_services = None;
        Ok(Left {
            map,
            bytes,
            space,
            boot_services,
        })
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

/// The real calls behind [`leave::Firmware`], the map's buffer once it is
/// allocated (its address and size), and the hand-off builder's working
/// space.
struct Leaving<'a> {
    services: &'a BootServices,
    get_memory_map: GetMemoryMap,
    exit_boot_services: ExitBootServices,
    buffer: Option<(NonNull<u8>, usize)>,
    space: Option<&'static mut [Region]>,
}

impl leave::Firmware for Leaving<'_> {
    fn memory_map(&mut self) -> MapRead {
        let (buffer, mut size) = match self.buffer {
            Some((buffer, capacity)) => (buffer.as_ptr(), capacity),
            None => (ptr::null_mut(), 0),
        };
        let (mut key, mut descriptor_size, mut version) = (0, 0, 0);
        // SAFETY: the function comes from the checked table. Boot services
        // are running, or `ExitBootServices()` has just failed, after which
        // `GetMemoryMap()` may still be called (section 7.4). The four
        // places are valid for the firmware to write. The buffer is null
        // with a size of zero, which only asks for the size (section 7.2),
        // or the `size` bytes `allocate` got from the firmware, 8-byte
        // aligned as descriptors need, which nothing else uses.
        let status = unsafe {
            (self.get_memory_map)(
                &mut size,
                buffer,
                &mut key,
                &mut descriptor_size,
                &mut version,
            )
        };
        MapRead {
            status,
            size,
            key,
            descriptor_size,
            version,
        }
    }

    fn allocate(&mut self, size: usize) -> Result<u64, Option<Status>> {
        let buffer = self
            .services
            .allocate(size)
            .map_err(|failure| match failure {
                Failure::Firmware(_, status) => Some(status),
                _ => None,
            })?;
        self.buffer = Some((buffer, size));
        Ok(buffer.as_ptr() as u64)
    }

    fn allocate_space(&mut self, descriptors: usize) -> Result<(), Option<Status>> {
        let regions = handoff::space_for(descriptors);
        let pages = regions
            .checked_mul(size_of::<Region>())
            .map(|bytes| bytes.div_ceil(PAGE_SIZE) as u64)
            .ok_or(None)?;
        // Loader data, cleared, never freed: the builder's alone.
        let (_, bytes) =
            self.services
                .allocate_pages(pages, 0)
                .map_err(|failure| match failure {
                    PlaceFailure::Firmware { status, .. } => Some(status),
                    _ => None,
                })?;
        self.space = Some(memory::regions(bytes).ok_or(None)?);
        Ok(())
    }

    fn exit(&mut self, key: usize) -> Status {
        // SAFETY: the function comes from the checked table and the handle
        // is the image handle the firmware passed to the entry point. After
        // it succeeds no boot service is called: `leave` makes no call
        // after a successful exit, and `BootServices::leave` consumed the
        // boot services.
        unsafe { (self.exit_boot_services)(self.services.image, key) }
    }
}

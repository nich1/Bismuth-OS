//! The hand-off structure: what the boot stage tells the kernel about the
//! machine, in a layout of Bismuth's own. Today it holds one thing, the
//! memory map: which ranges of physical memory exist and what each may be
//! used for. `docs/handoff.md` explains it.
//!
//! The structure is a run of bytes with every field at a stated offset,
//! little-endian, so that the boot stage and the kernel agree on it without
//! sharing a compiler's idea of how a Rust type is laid out. The boot stage
//! writes it with [`Builder`]; the kernel checks and reads it with
//! [`Handoff`], which treats the bytes as untrusted: any bytes give either
//! an error or a view whose regions all lie inside them. Nothing here
//! allocates or can panic, and the lints below keep it that way.
//!
//! The firmware memory types come from the UEFI specification, version 2.10,
//! section 7.2 "Memory Allocation Services": `EFI_MEMORY_TYPE` under
//! `EFI_BOOT_SERVICES.AllocatePages()`, and the tables of how each type may
//! be used before and after `ExitBootServices()`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result,
    clippy::string_slice,
    clippy::fallible_impl_from
)]
// No lint covers `assert!` outside a function returning `Result`,
// `split_at`, `copy_from_slice`, `chunks` or `windows`; the library uses
// `copy_from_slice` only on ranges it has just cut to the source's length.

#[cfg(test)]
mod tests;

/// The size of a page, in bytes. Every region is whole pages.
pub const PAGE_SIZE: u64 = 4096;
/// The lowest physical address no region may reach: x86-64 physical
/// addresses have at most 52 bits.
pub const PHYSICAL_LIMIT: u64 = 1 << 52;
/// The first eight bytes of every hand-off structure.
pub const MARKER: [u8; 8] = *b"BISMHAND";
/// The version this library writes and the only major version it reads.
pub const MAJOR: u16 = 1;
/// The minor version this library writes. Any minor version is read.
pub const MINOR: u16 = 0;
/// The size of the header this library writes; a newer minor version may
/// state a larger one.
pub const HEADER_SIZE: usize = 24;
/// The size of one region entry.
pub const REGION_SIZE: usize = 24;

const PAGE_MASK: u64 = PAGE_SIZE.wrapping_sub(1);

// Header field offsets; the table in `docs/handoff.md` is the same.
const AT_MAJOR: usize = 8;
const AT_MINOR: usize = 10;
const AT_LENGTH: usize = 12;
const AT_HEADER_SIZE: usize = 16;
const AT_COUNT: usize = 20;
// Region entry field offsets. Bytes 20 to 23 are written as zero and not
// read.
const AT_START: usize = 0;
const AT_PAGES: usize = 8;
const AT_KIND: usize = 16;

/// What a range of physical memory is, in Bismuth's terms. The number of
/// each is what the structure stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// Free: the kernel may use it at once.
    Usable = 1,
    /// The kernel's own: its segments, its stack and the frames of the
    /// address tables it starts on. One kind for all of them, because the
    /// kernel can tell which frame is which by walking those tables.
    Kernel = 2,
    /// Lent by the firmware for its boot services. Free once the kernel runs
    /// on address tables of its own, since the first tables' lower levels
    /// are here.
    BootServices = 3,
    /// The boot stage's own memory that is not the kernel's: its code, its
    /// data, the kernel file it read and this structure itself. Free once
    /// the kernel runs on tables of its own and has read the structure.
    Loader = 4,
    /// The firmware's runtime services. Never touched.
    FirmwareRuntime = 5,
    /// Holds ACPI tables. Free once the kernel has read them.
    AcpiReclaimable = 6,
    /// ACPI firmware memory, kept across sleep states. Never touched.
    AcpiPreserved = 7,
    /// Registers of a device, reached at memory addresses. Not memory.
    Device = 8,
    /// Memory that keeps its contents without power. Not for general use.
    Persistent = 9,
    /// Unusable, reserved, or of a type Bismuth does not know. Never
    /// touched.
    #[default]
    Reserved = 10,
}

impl Kind {
    /// The number the structure stores for this kind.
    pub fn number(self) -> u32 {
        self as u32
    }

    /// The kind a stored number stands for. A number this library does not
    /// know, which a newer minor version may write, is [`Kind::Reserved`].
    pub fn from_number(number: u32) -> Kind {
        match number {
            1 => Kind::Usable,
            2 => Kind::Kernel,
            3 => Kind::BootServices,
            4 => Kind::Loader,
            5 => Kind::FirmwareRuntime,
            6 => Kind::AcpiReclaimable,
            7 => Kind::AcpiPreserved,
            8 => Kind::Device,
            9 => Kind::Persistent,
            _ => Kind::Reserved,
        }
    }

    /// The kind of memory the firmware describes with this `EFI_MEMORY_TYPE`
    /// (UEFI 2.10, section 7.2). Every type the specification names, every
    /// number it leaves unassigned and both ranges it sets aside map to one
    /// kind, and only conventional memory is usable.
    pub fn from_firmware_type(firmware_type: u32) -> Kind {
        match firmware_type {
            // EfiLoaderCode, EfiLoaderData: what the boot stage allocated.
            1 | 2 => Kind::Loader,
            // EfiBootServicesCode, EfiBootServicesData: the firmware's, until
            // the kernel no longer runs on tables that live there.
            3 | 4 => Kind::BootServices,
            // EfiRuntimeServicesCode, EfiRuntimeServicesData: the firmware
            // keeps using it after the boot stage leaves.
            5 | 6 => Kind::FirmwareRuntime,
            // EfiConventionalMemory: free.
            7 => Kind::Usable,
            // EfiACPIReclaimMemory: ACPI tables, free once read.
            9 => Kind::AcpiReclaimable,
            // EfiACPIMemoryNVS: the firmware needs it kept across sleep.
            10 => Kind::AcpiPreserved,
            // EfiMemoryMappedIO, EfiMemoryMappedIOPortSpace: device
            // registers, not memory.
            11 | 12 => Kind::Device,
            // EfiPersistentMemory: keeps its contents, so not scratch memory.
            14 => Kind::Persistent,
            // EfiReservedMemoryType (0) and EfiUnusableMemory (8): not to be
            // used. EfiPalCode (13): processor firmware, to be kept.
            // EfiUnacceptedMemoryType (15): faults until accepted, which
            // Bismuth does not do. EfiMaxMemoryType (16) and the numbers up
            // to 0x6FFFFFFF: not valid types. 0x70000000 to 0x7FFFFFFF: for
            // the machine's maker, meaning unknown. 0x80000000 to 0xFFFFFFFF:
            // for operating system loaders, and Bismuth's uses none, so
            // someone else's.
            _ => Kind::Reserved,
        }
    }
}

/// A range of physical memory of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    /// The physical address of its first byte, a multiple of 4 KiB.
    pub start: u64,
    /// Its length in 4 KiB pages.
    pub pages: u64,
    /// What it is.
    pub kind: Kind,
}

impl Region {
    /// The address one past its last byte. Exact for every region the
    /// builder holds or the reader returns, which all end at or below
    /// [`PHYSICAL_LIMIT`]; for others it stops at `u64::MAX`.
    pub fn end(&self) -> u64 {
        self.start
            .saturating_add(self.pages.saturating_mul(PAGE_SIZE))
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start < end && start < self.end()
    }
}

/// The end of `pages` pages from `start`, if `start` is page-aligned and the
/// end does not pass [`PHYSICAL_LIMIT`].
fn checked_end(start: u64, pages: u64) -> Result<u64, BuildError> {
    if start & PAGE_MASK != 0 {
        return Err(BuildError::Unaligned);
    }
    pages
        .checked_mul(PAGE_SIZE)
        .and_then(|length| start.checked_add(length))
        .filter(|end| *end <= PHYSICAL_LIMIT)
        .ok_or(BuildError::BeyondPhysicalLimit)
}

/// The number of pages from `start` to `end`, both page-aligned.
fn pages_between(start: u64, end: u64) -> u64 {
    end.saturating_sub(start)
        .checked_div(PAGE_SIZE)
        .unwrap_or(0)
}

/// Why the builder refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// The start address is not a multiple of 4 KiB.
    Unaligned,
    /// The range ends past 2^52, or its end does not fit in 64 bits.
    BeyondPhysicalLimit,
    /// The firmware descriptor overlaps memory already described.
    Overlap,
    /// The range to set aside has no pages.
    Empty,
    /// The range to set aside is not wholly inside memory the firmware
    /// described as the boot stage's (`EfiLoaderCode` or `EfiLoaderData`)
    /// and not yet set aside.
    NotLoaderMemory,
    /// The space given to the builder has no room for another region.
    Full,
    /// The bytes given to [`Builder::write`] are fewer than the structure
    /// needs.
    OutputTooSmall {
        /// How many bytes it needs.
        needed: usize,
    },
}

/// Collects the memory map in space the caller lends it, then writes the
/// structure.
///
/// Firmware descriptors go in one at a time, in any order, followed by the
/// ranges set aside for the kernel. Every refusal leaves the map as it was.
pub struct Builder<'a> {
    space: &'a mut [Region],
    count: usize,
}

impl<'a> Builder<'a> {
    /// A builder holding at most `space.len()` regions before merging. What
    /// `space` holds does not matter.
    pub fn new(space: &'a mut [Region]) -> Self {
        Builder { space, count: 0 }
    }

    fn regions(&self) -> &[Region] {
        self.space.get(..self.count).unwrap_or(&[])
    }

    fn push(&mut self, region: Region) -> Result<(), BuildError> {
        let slot = self.space.get_mut(self.count).ok_or(BuildError::Full)?;
        *slot = region;
        self.count = self.count.saturating_add(1);
        Ok(())
    }

    /// Adds one descriptor of the firmware's memory map: its type
    /// (`EFI_MEMORY_TYPE`), physical start and number of 4 KiB pages. A
    /// descriptor with no pages describes nothing and is skipped.
    pub fn add_firmware(
        &mut self,
        firmware_type: u32,
        start: u64,
        pages: u64,
    ) -> Result<(), BuildError> {
        if pages == 0 {
            return Ok(());
        }
        let end = checked_end(start, pages)?;
        if self.regions().iter().any(|r| r.overlaps(start, end)) {
            return Err(BuildError::Overlap);
        }
        self.push(Region {
            start,
            pages,
            kind: Kind::from_firmware_type(firmware_type),
        })
    }

    /// Marks `pages` pages from `start` as the kernel's. They must all be
    /// the boot stage's memory, which is where it puts everything of the
    /// kernel's: memory the firmware calls free has not been allocated, so
    /// marking it would hide a mistake.
    pub fn set_aside_for_kernel(&mut self, start: u64, pages: u64) -> Result<(), BuildError> {
        if pages == 0 {
            return Err(BuildError::Empty);
        }
        let end = checked_end(start, pages)?;
        // Regions do not overlap, so the range is wholly loader memory when
        // the loader regions it touches cover as many pages as it has.
        let mut covered: u64 = 0;
        let mut pieces: usize = 0;
        for region in self.regions() {
            if region.kind == Kind::Loader && region.overlaps(start, end) {
                covered = covered.saturating_add(pages_between(
                    region.start.max(start),
                    region.end().min(end),
                ));
                pieces = pieces
                    .saturating_add(usize::from(region.start < start))
                    .saturating_add(usize::from(region.end() > end));
            }
        }
        if covered != pages {
            return Err(BuildError::NotLoaderMemory);
        }
        if pieces > self.space.len().saturating_sub(self.count) {
            return Err(BuildError::Full);
        }
        for index in 0..self.count {
            let Some(region) = self.space.get(index).copied() else {
                break;
            };
            if region.kind != Kind::Loader || !region.overlaps(start, end) {
                continue;
            }
            // The parts outside the range stay the loader's.
            if region.start < start {
                self.push(Region {
                    start: region.start,
                    pages: pages_between(region.start, start),
                    kind: Kind::Loader,
                })?;
            }
            if region.end() > end {
                self.push(Region {
                    start: end,
                    pages: pages_between(end, region.end()),
                    kind: Kind::Loader,
                })?;
            }
            let (from, to) = (region.start.max(start), region.end().min(end));
            if let Some(slot) = self.space.get_mut(index) {
                *slot = Region {
                    start: from,
                    pages: pages_between(from, to),
                    kind: Kind::Kernel,
                };
            }
        }
        Ok(())
    }

    /// Sorts the regions by address and merges neighbours of the same kind.
    fn normalise(&mut self) {
        let Some(regions) = self.space.get_mut(..self.count) else {
            return;
        };
        regions.sort_unstable_by_key(|region| region.start);
        let mut kept: usize = 0;
        for index in 0..regions.len() {
            let Some(current) = regions.get(index).copied() else {
                break;
            };
            match kept.checked_sub(1).and_then(|last| regions.get_mut(last)) {
                Some(last) if last.kind == current.kind && last.end() == current.start => {
                    last.pages = last.pages.saturating_add(current.pages);
                }
                _ => {
                    if let Some(slot) = regions.get_mut(kept) {
                        *slot = current;
                    }
                    kept = kept.saturating_add(1);
                }
            }
        }
        self.count = kept;
    }

    /// Writes the structure at the start of `out`, regions sorted by address
    /// with neighbours of the same kind merged, and returns its length.
    /// Bytes of `out` past that length are left as they were.
    pub fn write(&mut self, out: &mut [u8]) -> Result<usize, BuildError> {
        self.normalise();
        let regions = self.regions();
        // The count and the length must fit their 32-bit fields.
        let (count, length) = u32::try_from(regions.len())
            .ok()
            .and_then(|count| {
                let length = u64::from(count)
                    .checked_mul(REGION_SIZE as u64)?
                    .checked_add(HEADER_SIZE as u64)?;
                Some((count, u32::try_from(length).ok()?))
            })
            .ok_or(BuildError::Full)?;
        // A u32 always fits in a usize on x86-64.
        let needed = length as usize;
        let out = out
            .get_mut(..needed)
            .ok_or(BuildError::OutputTooSmall { needed })?;
        let (header, table) = out
            .split_at_mut_checked(HEADER_SIZE)
            .ok_or(BuildError::OutputTooSmall { needed })?;
        put(header, 0, &MARKER);
        put(header, AT_MAJOR, &MAJOR.to_le_bytes());
        put(header, AT_MINOR, &MINOR.to_le_bytes());
        put(header, AT_LENGTH, &length.to_le_bytes());
        put(header, AT_HEADER_SIZE, &(HEADER_SIZE as u32).to_le_bytes());
        put(header, AT_COUNT, &count.to_le_bytes());
        let (entries, _) = table.as_chunks_mut::<REGION_SIZE>();
        for (entry, region) in entries.iter_mut().zip(regions) {
            put(entry, AT_START, &region.start.to_le_bytes());
            put(entry, AT_PAGES, &region.pages.to_le_bytes());
            put(entry, AT_KIND, &region.kind.number().to_le_bytes());
            put(entry, AT_KIND.saturating_add(4), &[0; 4]);
        }
        Ok(needed)
    }
}

/// Copies `value` to `at` in `out`, if it fits; callers write only inside
/// the header or an entry, where it always does.
fn put(out: &mut [u8], at: usize, value: &[u8]) {
    if let Some(target) = out
        .get_mut(at..)
        .and_then(|rest| rest.get_mut(..value.len()))
    {
        target.copy_from_slice(value);
    }
}

/// Reads a little-endian number of `N` bytes at `at`, or zero past the end;
/// callers read only inside a header or an entry they have the whole of.
fn get<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    bytes
        .get(at..)
        .and_then(|rest| rest.first_chunk::<N>())
        .copied()
        .unwrap_or([0; N])
}

fn get_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(get(bytes, at))
}

fn get_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(get(bytes, at))
}

/// Why the reader refused a structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Fewer bytes than a header.
    Truncated,
    /// The bytes do not start with [`MARKER`].
    BadMarker,
    /// The major version is not [`MAJOR`].
    UnsupportedVersion {
        /// The major version stated.
        major: u16,
        /// The minor version stated.
        minor: u16,
    },
    /// The stated length is more than the bytes given.
    LengthPastEnd,
    /// The stated header size is less than [`HEADER_SIZE`] or more than the
    /// stated length.
    BadHeaderSize,
    /// The stated number of regions does not fit in the stated length.
    RegionsPastLength,
    /// A region does not start on a page, has no pages, or ends past 2^52.
    BadRegion {
        /// Its position in the list.
        index: u32,
    },
    /// A region starts before the end of the one before it: the list is out
    /// of order or two regions overlap.
    NotSorted {
        /// Its position in the list.
        index: u32,
    },
}

/// A checked hand-off structure.
#[derive(Debug, Clone, Copy)]
pub struct Handoff<'a> {
    /// The region entries, inside the bytes given to [`Handoff::read`].
    table: &'a [[u8; REGION_SIZE]],
}

impl<'a> Handoff<'a> {
    /// Checks `bytes` and returns a view of the structure at their start.
    /// Bytes past its stated length are not read. A newer minor version is
    /// read like this one: what it added lies past the fields this library
    /// knows, inside the stated sizes, and is skipped.
    pub fn read(bytes: &'a [u8]) -> Result<Self, ReadError> {
        let header = bytes
            .first_chunk::<HEADER_SIZE>()
            .ok_or(ReadError::Truncated)?;
        if get::<8>(header, 0) != MARKER {
            return Err(ReadError::BadMarker);
        }
        let major = u16::from_le_bytes(get(header, AT_MAJOR));
        if major != MAJOR {
            let minor = u16::from_le_bytes(get(header, AT_MINOR));
            return Err(ReadError::UnsupportedVersion { major, minor });
        }
        // A u32 always fits in a usize on x86-64.
        let length = get_u32(header, AT_LENGTH) as usize;
        if length > bytes.len() {
            return Err(ReadError::LengthPastEnd);
        }
        let header_size = get_u32(header, AT_HEADER_SIZE) as usize;
        if header_size < HEADER_SIZE || header_size > length {
            return Err(ReadError::BadHeaderSize);
        }
        let count = get_u32(header, AT_COUNT) as usize;
        let table_end = count
            .checked_mul(REGION_SIZE)
            .and_then(|size| size.checked_add(header_size))
            .filter(|end| *end <= length)
            .ok_or(ReadError::RegionsPastLength)?;
        let (table, _) = bytes
            .get(header_size..table_end)
            .ok_or(ReadError::RegionsPastLength)?
            .as_chunks::<REGION_SIZE>();
        let mut previous_end = 0;
        for (index, entry) in (0u32..).zip(table) {
            let region = decode(entry);
            if region.pages == 0 {
                return Err(ReadError::BadRegion { index });
            }
            let end = checked_end(region.start, region.pages)
                .map_err(|_| ReadError::BadRegion { index })?;
            if region.start < previous_end {
                return Err(ReadError::NotSorted { index });
            }
            previous_end = end;
        }
        Ok(Handoff { table })
    }

    /// The regions, sorted by address and not overlapping.
    pub fn regions(&self) -> impl Iterator<Item = Region> + use<'a> {
        self.table.iter().map(decode)
    }

    /// The number of pages the kernel may use at once.
    pub fn usable_pages(&self) -> u64 {
        self.regions()
            .filter(|region| region.kind == Kind::Usable)
            .fold(0, |sum, region| sum.saturating_add(region.pages))
    }
}

fn decode(entry: &[u8; REGION_SIZE]) -> Region {
    Region {
        start: get_u64(entry, AT_START),
        pages: get_u64(entry, AT_PAGES),
        kind: Kind::from_number(get_u32(entry, AT_KIND)),
    }
}

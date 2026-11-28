//! Builds x86-64 4-level page tables ("address tables"): maps a range of
//! virtual addresses to a range of physical addresses, 4 KiB at a time.
//!
//! The library holds no memory of its own. The caller lends it page-sized
//! frames and access to them through [`PhysicalMemory`], and names the
//! top-level table. Large pages, unmapping, changing permissions and 5-level
//! paging are not supported.
//!
//! The table and entry layouts come from the Intel 64 and IA-32 Architectures
//! Software Developer's Manual (SDM), volume 3A, chapter 4 "Paging", section
//! "4-Level Paging and 5-Level Paging" and its entry-format tables, and match
//! the AMD64 Architecture Programmer's Manual volume 2, chapter 5 "Page
//! Translation and Protection", section "Long-Mode Page Translation".
//!
//! The no-execute bit only takes effect once the processor has it turned on
//! (`IA32_EFER.NXE`); before that the processor treats it as a reserved bit.
//! Turning it on is the caller's job.

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

#[cfg(test)]
mod tests;

/// The size of a page, a frame and a table, in bytes.
pub const PAGE_SIZE: u64 = 4096;

/// The memory the tables live in, as the caller provides it.
pub trait PhysicalMemory {
    /// Hands out the physical address of an unused 4 KiB-aligned frame below
    /// 2^52, or `None` if there are none left. Its contents do not matter:
    /// [`map`] zeroes every frame it is given before using it as a table.
    fn allocate_frame(&mut self) -> Option<u64>;

    /// The 512 entries of the table at physical address `address`. The
    /// library only asks for the top-level table it was given and for frames
    /// it received from [`PhysicalMemory::allocate_frame`].
    fn table(&mut self, address: u64) -> &mut [u64; 512];
}

/// What a mapped page allows besides reading, which is always allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// The page may be written.
    pub write: bool,
    /// Instructions may be fetched from the page.
    pub execute: bool,
}

/// Why [`map`] refused or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The virtual address, physical address or length is not a multiple of
    /// 4 KiB.
    Unaligned,
    /// The length is zero.
    Empty,
    /// The virtual range does not lie wholly inside one canonical half of
    /// the 48-bit address range, or it wraps past the top.
    NonCanonical,
    /// The physical range reaches beyond 2^52, the architectural limit.
    PhysicalTooLarge,
    /// The page at this virtual address is already mapped.
    AlreadyMapped(u64),
    /// On the way down to the page at this virtual address, an entry maps a
    /// large page (its page-size bit is set), so the page cannot be mapped
    /// separately.
    LargePage(u64),
    /// [`PhysicalMemory::allocate_frame`] had no frames left.
    OutOfFrames,
}

/// Maps the `length` bytes starting at virtual address `virtual_start` to
/// the same number of bytes starting at physical address `physical_start`,
/// under the top-level table at physical address `root`.
///
/// Every page gets the same permissions and is never accessible from user
/// mode. Tables that already exist are reused, and their entries are left
/// as they are, so tables made elsewhere with stricter upper-level entries
/// would also limit the new pages; missing tables are taken from `memory`
/// and zeroed.
///
/// The arguments are checked first, in the order of the [`Error`] variants,
/// and a bad argument changes nothing. After that the pages are mapped in
/// order, and an error stops the mapping at the page it names (for
/// [`Error::OutOfFrames`], the first page not mapped): the pages before it
/// stay mapped, it and the pages after it are left as they were, and tables
/// taken from `memory` on the way stay in place, possibly empty. Frames are
/// never given back.
pub fn map(
    memory: &mut impl PhysicalMemory,
    root: u64,
    virtual_start: u64,
    physical_start: u64,
    length: u64,
    permissions: Permissions,
) -> Result<(), Error> {
    if !virtual_start.is_multiple_of(PAGE_SIZE)
        || !physical_start.is_multiple_of(PAGE_SIZE)
        || !length.is_multiple_of(PAGE_SIZE)
    {
        return Err(Error::Unaligned);
    }
    let Some(last_offset) = length.checked_sub(1) else {
        return Err(Error::Empty);
    };
    match virtual_start.checked_add(last_offset) {
        Some(last) if half(virtual_start).is_some() && half(virtual_start) == half(last) => {}
        _ => return Err(Error::NonCanonical),
    }
    match physical_start.checked_add(length) {
        Some(end) if end <= PHYSICAL_LIMIT => {}
        _ => return Err(Error::PhysicalTooLarge),
    }

    let mut flags = PRESENT;
    if permissions.write {
        flags |= WRITABLE;
    }
    if !permissions.execute {
        flags |= NO_EXECUTE;
    }
    // Neither sum below can overflow: both ranges were checked above.
    for offset in (0..length).step_by(PAGE_SIZE as usize) {
        let page = virtual_start.wrapping_add(offset);
        let mut table = root;
        for shift in [39, 30, 21] {
            let current = *entry(memory.table(table), page, shift);
            table = if current & PRESENT == 0 {
                let frame = memory.allocate_frame().ok_or(Error::OutOfFrames)?;
                *memory.table(frame) = [0; 512];
                *entry(memory.table(table), page, shift) = PRESENT | WRITABLE | (frame & ADDRESS);
                frame
            } else if current & PAGE_SIZE_BIT != 0 {
                return Err(Error::LargePage(page));
            } else {
                current & ADDRESS
            };
        }
        let leaf = entry(memory.table(table), page, 12);
        if *leaf & PRESENT != 0 {
            return Err(Error::AlreadyMapped(page));
        }
        *leaf = flags | physical_start.wrapping_add(offset);
    }
    Ok(())
}

// SDM vol. 3A ch. 4, "4-Level Paging and 5-Level Paging": the entry-format
// tables for PML4, page-directory-pointer, page-directory and page-table
// entries. Upper-level entries here are present and writable with no
// execute-disable bit, so the page entry alone decides what is allowed.
/// Present: the entry is used.
const PRESENT: u64 = 1 << 0;
/// Read/write: writes are allowed (in an upper-level entry, below it).
const WRITABLE: u64 = 1 << 1;
/// Page size: in a directory-pointer or directory entry, the entry maps a
/// 1 GiB or 2 MiB page instead of pointing to a table.
const PAGE_SIZE_BIT: u64 = 1 << 7;
/// Execute-disable (AMD: no-execute): instruction fetches are not allowed.
const NO_EXECUTE: u64 = 1 << 63;
/// The physical address of the frame an entry points to: bits 51:12. Bits
/// above the processor's own physical address width are reserved; staying
/// below that width is the caller's job.
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
/// Physical addresses are at most 52 bits wide.
const PHYSICAL_LIMIT: u64 = 1 << 52;

/// Which canonical half `address` is in (`Some(false)` for the low half), or
/// `None` if it is not canonical. With 4-level paging, bits 63:47 of a
/// linear address must all be equal (SDM vol. 3A ch. 4, "4-Level Paging and
/// 5-Level Paging").
fn half(address: u64) -> Option<bool> {
    match address >> 47 {
        0 => Some(false),
        0x1_FFFF => Some(true),
        _ => None,
    }
}

/// The entry for `address` in `table`, whose index is the 9 address bits
/// starting at bit `shift`: 39, 30, 21 and 12 from the top level down.
#[allow(clippy::indexing_slicing)] // A 9-bit index is always below 512.
fn entry(table: &mut [u64; 512], address: u64, shift: u32) -> &mut u64 {
    &mut table[((address >> shift) & 0x1FF) as usize]
}

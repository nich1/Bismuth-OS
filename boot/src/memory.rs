//! Memory the boot stage reaches by raw address while it gives the kernel
//! its addresses: the firmware's top-level address table, the frames the new
//! tables live in, and the kernel's pages at their kernel addresses.
//!
//! While boot services run, the firmware maps all memory at addresses equal
//! to its physical ones (UEFI specification 2.10, section 2.3.4, x64
//! platforms), so a physical address is also a pointer. The new tables keep
//! that mapping.

use bismuth_paging::{PAGE_SIZE, PhysicalMemory};

use crate::report::{MapFailure, PlaceFailure};
use crate::uefi::BootServices;

/// Physical addresses are at most 52 bits wide (SDM vol. 3A ch. 4).
const PHYSICAL_LIMIT: u64 = 1 << 52;

/// One address table: 512 eight-byte entries, one 4 KiB frame.
type Table = [u64; 512];

/// Frames for new address tables, all from one block of cleared pages the
/// firmware gave this program and never takes back. The first frame is the
/// new top-level table; the rest are handed out in order, each once.
///
/// One block taken up front, rather than a page at a time, so that whether
/// an address is one of these frames is a range check, and every table is
/// reached through the one exclusive borrow of the block: `table` borrows
/// `self` mutably, so two live references to one table cannot exist.
pub struct Frames {
    /// The block as tables. The processor also reads these entries once
    /// they are in use, and sets their accessed and dirty bits.
    tables: &'static mut [Table],
    /// The block's physical address.
    physical: u64,
    /// How many frames are in use, the top-level table among them.
    used: usize,
    /// What `table` lends for an address outside the frames in use: an
    /// empty table, cleared each time.
    outside: Table,
}

impl Frames {
    /// Takes `count` frames, at least one, from the firmware.
    ///
    /// `allocate_pages` hands back whole 4 KiB pages, cleared, that nothing
    /// else uses. The block is also refused unless it is 4 KiB-aligned and
    /// ends at or below 2^52, so every frame handed out is aligned, below
    /// 2^52 and given out once.
    pub fn new(boot_services: &BootServices, count: u64) -> Result<Frames, MapFailure> {
        // The segment number only labels a `PlaceFailure`, replaced here.
        let (physical, bytes) =
            boot_services
                .allocate_pages(count, 0)
                .map_err(|failure| MapFailure::NoFrames {
                    status: match failure {
                        PlaceFailure::Firmware { status, .. } => Some(status),
                        _ => None,
                    },
                })?;
        let end = (bytes.len() as u64).checked_add(physical);
        if !physical.is_multiple_of(PAGE_SIZE) || end.is_none_or(|end| end > PHYSICAL_LIMIT) {
            return Err(MapFailure::FramesUnusable);
        }
        // SAFETY: every bit pattern is a valid `u64`, so the cleared bytes
        // are valid tables of `u64` entries. `align_to_mut` leaves bytes it
        // cannot view that way in the first and last part, and those are
        // checked to be empty below, so the tables are the whole block.
        let (head, tables, tail) = unsafe { bytes.align_to_mut::<Table>() };
        if !head.is_empty() || !tail.is_empty() || tables.is_empty() {
            return Err(MapFailure::FramesUnusable);
        }
        Ok(Frames {
            tables,
            physical,
            used: 1,
            outside: [0; 512],
        })
    }

    /// The physical address of the new top-level table.
    pub fn root(&self) -> u64 {
        self.physical
    }
}

impl PhysicalMemory for Frames {
    fn allocate_frame(&mut self) -> Option<u64> {
        if self.used >= self.tables.len() {
            return None;
        }
        // Inside the block, which `new` checked ends below 2^52.
        let address = self.physical + self.used as u64 * PAGE_SIZE;
        self.used += 1;
        Some(address)
    }

    /// The frame at `address` if it is one in use. Any other address, which
    /// `map` never asks for, reads as an empty table: the boot stage only
    /// walks kernel addresses, whose tables are all its own, so a walk that
    /// leaves them finds nothing mapped.
    fn table(&mut self, address: u64) -> &mut Table {
        let index = address
            .checked_sub(self.physical)
            .filter(|offset| offset.is_multiple_of(PAGE_SIZE))
            .and_then(|offset| usize::try_from(offset / PAGE_SIZE).ok())
            .filter(|&index| index < self.used);
        match index.and_then(|index| self.tables.get_mut(index)) {
            Some(table) => table,
            None => {
                self.outside = [0; 512];
                &mut self.outside
            }
        }
    }
}

/// A copy of the 512 entries of the top-level table at physical address
/// `address`.
///
/// # Safety
///
/// `address` must be the address part of CR3, read while boot services
/// run: the top-level table the processor uses, 4 KiB-aligned, in memory
/// the firmware maps at its own address. Nothing changes it while it is
/// read, since only this program runs.
pub unsafe fn firmware_root(address: u64) -> Table {
    // SAFETY: by the contract, `address` is a mapped, aligned table of 512
    // entries. It is only read, since the firmware may map it read-only.
    unsafe { core::ptr::read(address as *const Table) }
}

/// The `pages` pages at kernel address `address`, read through the address
/// tables in use, or `None` if they would end at the top of the address
/// range, where a slice cannot end.
///
/// # Safety
///
/// The tables in CR3 must map every page of the range, readable, to the
/// kernel's copied pages, and keep doing so while the slice is used. Those
/// pages are initialised and nothing writes to them.
pub unsafe fn kernel_pages<'a>(address: u64, pages: u64) -> Option<&'a [u8]> {
    // A guard: for a segment the placement rules accepted, `None` cannot
    // happen, since no segment may use the last page of the address range
    // and the segments take at most 16384 pages.
    let size = pages.checked_mul(PAGE_SIZE)?;
    address.checked_add(size)?;
    let size = usize::try_from(size).ok()?;
    // SAFETY: by the contract the range is mapped to initialised memory
    // that nothing writes. It does not wrap, and a mapped range lies in one
    // canonical half, 2^47 bytes, far below `isize::MAX`.
    Some(unsafe { core::slice::from_raw_parts(address as *const u8, size) })
}

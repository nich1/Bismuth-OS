//! Copies the kernel's loadable segments into pages from the firmware,
//! after the placement rules in `bismuth_elf::place` have accepted the file.
//! Safe code over the pages `BootServices::allocate_pages` hands back.

use bismuth_elf::place::{self, Fnv1a, MAX_SEGMENTS, Placed};
use bismuth_elf::{Elf, Flags};

use crate::report::PlaceFailure;
use crate::uefi::BootServices;

/// One segment copied into memory: what the address tables will map.
#[expect(
    dead_code,
    reason = "the address tables, built next (issue 27), read the addresses and flags"
)]
pub struct Loaded {
    /// The physical address of its first page.
    pub physical: u64,
    /// The kernel address of its first page, where it is linked.
    pub address: u64,
    /// How many 4 KiB pages it takes.
    pub pages: u64,
    /// What its memory may be used for.
    pub flags: Flags,
    /// Its pages, as copied.
    memory: &'static [u8],
}

/// The kernel's segments in memory, in file order.
pub struct Kernel {
    loaded: [Option<Loaded>; MAX_SEGMENTS],
}

impl Kernel {
    /// Checks that `elf`, read from `bytes`, can be placed as linked, and
    /// copies each segment that takes memory into cleared pages of its own.
    pub fn copy(
        boot_services: &BootServices,
        bytes: &[u8],
        elf: &Elf,
    ) -> Result<Kernel, PlaceFailure> {
        let placement = place::place(elf).map_err(PlaceFailure::Rule)?;
        let mut kernel = Kernel {
            loaded: [const { None }; MAX_SEGMENTS],
        };
        // The placement holds at most `MAX_SEGMENTS`, so none is left out.
        for (slot, placed) in kernel.loaded.iter_mut().zip(placement.segments()) {
            let (physical, memory) = boot_services.allocate_pages(placed.pages, placed.segment)?;
            fill(memory, bytes, placed);
            *slot = Some(Loaded {
                physical,
                address: placed.page,
                pages: placed.pages,
                flags: placed.flags,
                memory,
            });
        }
        Ok(kernel)
    }

    /// The copied segments, in file order.
    pub fn segments(&self) -> impl Iterator<Item = &Loaded> {
        self.loaded.iter().flatten()
    }

    /// How many pages the segments take together.
    pub fn pages(&self) -> u64 {
        self.segments().map(|loaded| loaded.pages).sum()
    }

    /// The 64-bit FNV-1a sum of every byte of every segment's pages, read
    /// back from memory in file order.
    pub fn sum(&self) -> u64 {
        let mut sum = Fnv1a::new();
        for loaded in self.segments() {
            sum.add(loaded.memory);
        }
        sum.value()
    }
}

/// Copies the segment's contents from the file to its place in its first
/// page. The rest of `memory` is already zero.
///
/// The ELF reader keeps the contents inside the file, and the placement
/// rules keep them inside the pages (`file_size` is at most `memory_size`),
/// so the ranges are in bounds.
fn fill(memory: &mut [u8], bytes: &[u8], placed: &Placed) {
    let (offset, size, start) = (
        placed.offset as usize,
        placed.file_size as usize,
        placed.start as usize,
    );
    memory[start..start + size].copy_from_slice(&bytes[offset..offset + size]);
}

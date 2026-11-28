//! Gives the kernel its addresses: builds new address tables that keep
//! everything the firmware mapped and add each copied segment at its kernel
//! address with its own permissions, switches the processor to them while
//! boot services still run, and proves the kernel is there by walking the
//! tables in use and reading every byte back through the kernel addresses.
//! Safe code over `cpu` and `memory`.

use bismuth_elf::Flags;
use bismuth_elf::place::{Fnv1a, MAX_SEGMENTS, PAGE_LIMIT};
use bismuth_paging::{self as paging, PAGE_SIZE, Permissions, PhysicalMemory};

use crate::cpu;
use crate::load::Kernel;
use crate::memory::{self, Frames};
use crate::report::MapFailure;
use crate::uefi::BootServices;

/// Frames enough for any kernel the placement rules accept, which lies in
/// the top 2 GiB: the top-level table, one directory-pointer table, two
/// directories (each covers 1 GiB), and a last-level table for each 2 MiB a
/// segment touches. A segment of `p` pages touches at most `p / 512 + 2`
/// of those, and the segments take at most `PAGE_LIMIT` pages together.
const TABLE_FRAMES: u64 = 4 + PAGE_LIMIT / 512 + 2 * MAX_SEGMENTS as u64;

/// The top-level entry for the top 512 GiB, where the kernel goes.
const KERNEL_ENTRY: usize = 511;

/// Present bit of a table entry (SDM vol. 3A ch. 4, "4-Level Paging and
/// 5-Level Paging", the entry-format tables).
const PRESENT: u64 = 1;

/// What the boot stage found once the kernel was at its addresses.
pub struct Placed {
    /// The sum of every page, read through the kernel addresses.
    pub sum: u64,
    /// The permissions found for each segment, in file order.
    permissions: [Permissions; MAX_SEGMENTS],
    /// How many segments there are.
    count: usize,
    /// Whether `CR0.WP` is set, so ring 0 cannot write read-only pages.
    pub write_protect: bool,
}

impl Placed {
    /// The permissions found for each segment, in file order.
    pub fn permissions(&self) -> impl ExactSizeIterator<Item = Permissions> {
        self.permissions.into_iter().take(self.count)
    }
}

/// Builds the tables for `kernel`, switches to them and checks the result.
pub fn place(boot_services: &BootServices, kernel: &Kernel) -> Result<Placed, MapFailure> {
    if cpu::cr4() & cpu::CR4_LA57 != 0 {
        return Err(MapFailure::FiveLevel);
    }
    let mut frames = Frames::new(boot_services, TABLE_FRAMES)?;
    let root = frames.root();
    let firmware = cpu::cr3();
    // SAFETY: `firmware` was just read from CR3, and boot services run.
    let entries = unsafe { memory::firmware_root(firmware & cpu::CR3_ADDRESS) };
    if entries
        .get(KERNEL_ENTRY)
        .is_some_and(|entry| entry & PRESENT != 0)
    {
        return Err(MapFailure::TopEntryUsed);
    }
    *frames.table(root) = entries;

    let efer = cpu::efer();
    if efer & cpu::EFER_NXE == 0 {
        // SAFETY: only `EFER_NXE` is turned on, while it is off.
        unsafe { cpu::set_efer(efer | cpu::EFER_NXE) }
    }
    for (segment, loaded) in kernel.segments().enumerate() {
        // At most `PAGE_LIMIT` pages, so this cannot overflow.
        let length = loaded.pages * PAGE_SIZE;
        let permissions = permissions(loaded.flags);
        paging::map(
            &mut frames,
            root,
            loaded.address,
            loaded.physical,
            length,
            permissions,
        )
        .map_err(|error| MapFailure::Map {
            segment,
            address: loaded.address,
            error,
        })?;
    }
    // SAFETY: `root` is a 4 KiB-aligned frame below 2^52 (`Frames::new`).
    // It holds the firmware's 512 top-level entries, so everything mapped
    // before, this program and the firmware among it, translates as
    // before. Entry 511 was empty and now leads only to new tables, in
    // frames of their own, mapping the kernel's pages, which nothing runs
    // from. The frames are loader data, never freed, and nothing but the
    // processor changes them from here on.
    unsafe { cpu::set_cr3(firmware & !cpu::CR3_ADDRESS | root) }
    check(&mut frames, kernel)
}

/// Walks the tables in use for every page of every segment, then reads the
/// segments back through their kernel addresses.
fn check(frames: &mut Frames, kernel: &Kernel) -> Result<Placed, MapFailure> {
    let live = cpu::cr3() & cpu::CR3_ADDRESS;
    let mut placed = Placed {
        sum: 0,
        permissions: [Permissions {
            write: false,
            execute: false,
        }; MAX_SEGMENTS],
        count: 0,
        write_protect: cpu::cr0() & cpu::CR0_WP != 0,
    };
    let mut sum = Fnv1a::new();
    for ((segment, loaded), slot) in kernel.segments().enumerate().zip(&mut placed.permissions) {
        let expected = permissions(loaded.flags);
        for page in 0..loaded.pages {
            let offset = page * PAGE_SIZE;
            let address = loaded.address.wrapping_add(offset);
            let frame = loaded.physical + offset;
            let found = paging::translate(frames, live, address);
            if found != Some((frame, expected)) {
                return Err(MapFailure::Walk {
                    segment,
                    address,
                    found,
                    expected: (frame, expected),
                });
            }
        }
        *slot = expected;
        placed.count += 1;
        // SAFETY: the walk just found every page of the segment mapped,
        // readable, to its copied pages under the tables in CR3, and the
        // boot stage changes neither before it stops.
        let bytes = unsafe { memory::kernel_pages(loaded.address, loaded.pages) }
            .ok_or(MapFailure::ReadBack { segment })?;
        sum.add(bytes);
    }
    placed.sum = sum.value();
    let copied = kernel.sum();
    if placed.sum != copied {
        return Err(MapFailure::Sum {
            copied,
            read: placed.sum,
        });
    }
    Ok(placed)
}

/// What a segment's pages allow besides reading.
fn permissions(flags: Flags) -> Permissions {
    Permissions {
        write: flags.write,
        execute: flags.execute,
    }
}

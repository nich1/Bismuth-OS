//! Gives the kernel its addresses: builds new address tables that keep
//! everything the firmware mapped and add each copied segment at its kernel
//! address with its own permissions, and the kernel's stack below them,
//! switches the processor to them while boot services still run, and
//! proves the kernel is there by walking the tables in use and reading
//! every byte back through the kernel addresses, and that the stack is
//! there with its guard page missing. Safe code over `cpu` and `memory`.

use bismuth_elf::Flags;
use bismuth_elf::place::{Fnv1a, MAX_SEGMENTS, PAGE_LIMIT, STACK_BOTTOM, STACK_GUARD, STACK_PAGES};
use bismuth_paging::{self as paging, PAGE_SIZE, Permissions, PhysicalMemory};

use crate::cpu;
use crate::load::Kernel;
use crate::memory::{self, Frames};
use crate::report::{MapFailure, PlaceFailure};
use crate::uefi::BootServices;

/// Frames enough for any kernel the placement rules accept, which lies in
/// the top 2 GiB, and its stack: the top-level table, one directory-pointer
/// table, two directories for the kernel (each covers 1 GiB), and a
/// last-level table for each 2 MiB a segment touches. A segment of `p`
/// pages touches at most `p / 512 + 2` of those, and the segments take at
/// most `PAGE_LIMIT` pages together. The stack and its guard page lie in
/// the 2 MiB just below the kernel's 2 GiB, in the 1 GiB below them, so
/// they take one directory and one last-level table more, shared with no
/// segment. In all, 4 + 16384 / 512 + 2 * 8 + 2 = 54 frames.
const TABLE_FRAMES: u64 = 4 + PAGE_LIMIT / 512 + 2 * MAX_SEGMENTS as u64 + 2;

/// What the stack's pages allow: reading and writing, never running code.
const STACK: Permissions = Permissions {
    write: true,
    execute: false,
};

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

/// Builds the tables for `kernel` and its stack, switches to them and
/// checks the result.
pub fn place(boot_services: &BootServices, kernel: &Kernel) -> Result<Placed, MapFailure> {
    if cpu::cr4() & cpu::CR4_LA57 != 0 {
        return Err(MapFailure::FiveLevel);
    }
    if !cpu::has_no_execute() {
        return Err(MapFailure::NoExecuteUnsupported);
    }
    let mut frames = Frames::new(boot_services, TABLE_FRAMES)?;
    // Loader data, like the segments' pages, cleared and never freed.
    let (stack, _) = boot_services
        .allocate_pages(STACK_PAGES, 0)
        .map_err(|failure| MapFailure::NoStack {
            status: match failure {
                PlaceFailure::Firmware { status, .. } => Some(status),
                _ => None,
            },
        })?;
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
        // SAFETY: only `EFER_NXE` is turned on, while it is off, and the
        // processor has no-execute pages (checked above).
        unsafe { cpu::set_efer(efer | cpu::EFER_NXE) }
        if cpu::efer() & cpu::EFER_NXE == 0 {
            return Err(MapFailure::NoExecuteNotOn);
        }
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
    // At most 16 pages, below every segment; the guard page is left out.
    paging::map(
        &mut frames,
        root,
        STACK_BOTTOM,
        stack,
        STACK_PAGES * PAGE_SIZE,
        STACK,
    )
    .map_err(|error| MapFailure::StackMap { error })?;
    // SAFETY: `root` is a 4 KiB-aligned frame below 2^52 (`Frames::new`).
    // It holds the firmware's 512 top-level entries, so everything mapped
    // before, this program and the firmware among it, translates as
    // before. Entry 511 was empty and now leads only to new tables, in
    // frames of their own, mapping the kernel's pages, which nothing runs
    // from. The frames are loader data, never freed. From here on the boot
    // stage does not write them; the processor sets accessed and dirty
    // bits, and a firmware service may edit tables it finds through CR3:
    // the lower tables it shares with the firmware's own, and the new
    // top-level table. Relied on: such edits keep the firmware's own
    // mappings valid, as they must for the firmware's tables, and leave
    // entry 511, which the firmware did not use, alone. The walk below
    // checks the kernel's and the stack's mappings after any such edits.
    // The stack's pages are like the segments': loader data, never freed,
    // mapped only under entry 511, and not used by anything yet.
    unsafe { cpu::set_cr3(firmware & !cpu::CR3_ADDRESS | root) }
    let placed = check(&mut frames, kernel)?;
    check_stack(&mut frames, stack)?;
    Ok(placed)
}

/// Walks the tables in use for every page of the stack, whose first page
/// is at physical address `stack`: each must lead to its own frame, read
/// and write, not executable. The guard page below it must not be mapped.
fn check_stack(frames: &mut Frames, stack: u64) -> Result<(), MapFailure> {
    let live = cpu::cr3() & cpu::CR3_ADDRESS;
    for page in 0..STACK_PAGES {
        let address = STACK_BOTTOM + page * PAGE_SIZE;
        let expected = stack + page * PAGE_SIZE;
        let found = paging::translate(frames, live, address);
        if found != Some((expected, STACK)) {
            return Err(MapFailure::StackWalk {
                address,
                found,
                expected,
            });
        }
    }
    match paging::translate(frames, live, STACK_GUARD) {
        None => Ok(()),
        Some(_) => Err(MapFailure::GuardMapped {
            address: STACK_GUARD,
        }),
    }
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

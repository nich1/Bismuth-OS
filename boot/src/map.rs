//! Gives the kernel its addresses: builds new address tables that keep
//! everything the firmware mapped and add each copied segment at its kernel
//! address with its own permissions, the kernel's stack below them and the
//! hand-off window below the stack, read-only, switches the processor to
//! them while boot services still run, and proves the kernel is there by
//! walking the tables in use and reading every byte back through the
//! kernel addresses, that the stack is there with its guard page missing,
//! and that the window is there, read-only. Safe code over `cpu` and
//! `memory`.

use bismuth_elf::Flags;
use bismuth_elf::place::{
    Fnv1a, HANDOFF_ADDRESS, HANDOFF_PAGES, MAX_SEGMENTS, STACK_BOTTOM, STACK_GUARD, STACK_PAGES,
    TABLE_FRAMES,
};
use bismuth_paging::{self as paging, PAGE_SIZE, Permissions, PhysicalMemory};

use crate::cpu;
use crate::load::Kernel;
use crate::memory::{self, Frames};
use crate::report::{MapFailure, PlaceFailure};
use crate::uefi::BootServices;

/// What the stack's pages allow: reading and writing, never running code.
const STACK: Permissions = Permissions {
    write: true,
    execute: false,
};
// The stack must be writable and must not run code; `check_stack` holds the
// tables to the same rule on its own.
const _: () = assert!(STACK.write && !STACK.execute);

/// What the hand-off window allows the kernel: reading only.
const HANDOFF: Permissions = Permissions {
    write: false,
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
    /// The physical address of the stack's first page.
    pub stack: u64,
    /// The physical address of the block of table frames, which has
    /// `TABLE_FRAMES` pages.
    pub tables: u64,
    /// The hand-off structure's pages, cleared, at their physical address
    /// (the firmware's mappings keep that), which the tables also map
    /// read-only at `HANDOFF_ADDRESS`.
    pub handoff: &'static mut [u8],
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
    let status = |failure| match failure {
        PlaceFailure::Firmware { status, .. } => Some(status),
        _ => None,
    };
    // Loader data, like the segments' pages, cleared and never freed.
    let (stack, _) = boot_services
        .allocate_pages(STACK_PAGES, 0)
        .map_err(|failure| MapFailure::NoStack {
            status: status(failure),
        })?;
    // The same, for the hand-off structure, which is written only after
    // boot services end: allocating then would be too late.
    let (handoff_physical, handoff) =
        boot_services
            .allocate_pages(HANDOFF_PAGES, 0)
            .map_err(|failure| MapFailure::NoHandoffPages {
                status: status(failure),
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
    // Eight pages below the stack's guard page, in the stack's 2 MiB.
    paging::map(
        &mut frames,
        root,
        HANDOFF_ADDRESS,
        handoff_physical,
        HANDOFF_PAGES * PAGE_SIZE,
        HANDOFF,
    )
    .map_err(|error| MapFailure::HandoffMap { error })?;
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
    // The stack's pages and the hand-off pages are like the segments':
    // loader data, never freed, mapped only under entry 511 (besides the
    // firmware's own mapping of all memory), and not used by anything yet.
    unsafe { cpu::set_cr3(firmware & !cpu::CR3_ADDRESS | root) }
    let mut placed = check(&mut frames, kernel, handoff)?;
    check_stack(&mut frames, stack)?;
    check_handoff(&mut frames, handoff_physical)?;
    placed.stack = stack;
    placed.tables = frames.root();
    Ok(placed)
}

/// Walks the tables in use for every page of the hand-off window, whose
/// first page is at physical address `handoff`: each must lead to its own
/// frame, read-only and not executable, the rule stated here as for the
/// stack.
fn check_handoff(frames: &mut Frames, handoff: u64) -> Result<(), MapFailure> {
    let live = cpu::cr3() & cpu::CR3_ADDRESS;
    for page in 0..HANDOFF_PAGES {
        let address = HANDOFF_ADDRESS + page * PAGE_SIZE;
        let expected = handoff + page * PAGE_SIZE;
        let found = paging::translate(frames, live, address);
        let good = matches!(found, Some((frame, Permissions { write: false, execute: false }))
            if frame == expected);
        if !good {
            return Err(MapFailure::HandoffWalk {
                address,
                found,
                expected,
            });
        }
    }
    Ok(())
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
        // The rule, stated here rather than taken from `STACK`, so that a
        // mistake in the mapping cannot also change what is expected: the
        // page's own frame, writable, and not executable.
        let good = matches!(found, Some((frame, Permissions { write: true, execute: false }))
            if frame == expected);
        if !good {
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
fn check(
    frames: &mut Frames,
    kernel: &Kernel,
    handoff: &'static mut [u8],
) -> Result<Placed, MapFailure> {
    let live = cpu::cr3() & cpu::CR3_ADDRESS;
    let mut placed = Placed {
        sum: 0,
        permissions: [Permissions {
            write: false,
            execute: false,
        }; MAX_SEGMENTS],
        count: 0,
        write_protect: cpu::cr0() & cpu::CR0_WP != 0,
        stack: 0,
        tables: 0,
        handoff,
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

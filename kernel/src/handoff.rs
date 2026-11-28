//! The window the boot stage maps the hand-off structure in, as bytes for
//! the reader in `bismuth-handoff`, which checks them. Its one `unsafe`
//! function is the kernel's only step in reading the structure that needs
//! it.

use bismuth_elf::place::{HANDOFF_ADDRESS, HANDOFF_PAGES, PAGE_SIZE};

/// The [`HANDOFF_PAGES`] pages at [`HANDOFF_ADDRESS`], exactly the window
/// the boot stage mapped.
///
/// # Safety
///
/// Only once the boot stage has entered the kernel as `docs/boot.md`, "How
/// the kernel is entered", says: it mapped the window read-only at that
/// address, every byte written (the structure, then the cleared rest of
/// its pages), and nothing writes it while the kernel runs on the tables
/// the boot stage built. The bytes are not trusted beyond that: the reader
/// checks every field.
pub unsafe fn window() -> &'static [u8] {
    // SAFETY: by the contract the window is mapped, readable and
    // initialised, and nothing changes it; it is 32 KiB, far below
    // `isize::MAX`, and does not wrap, lying below the stack.
    unsafe {
        core::slice::from_raw_parts(
            HANDOFF_ADDRESS as *const u8,
            (HANDOFF_PAGES * PAGE_SIZE) as usize,
        )
    }
}

//! The checks the kernel makes of the state the boot stage entered it in
//! (`docs/boot.md`, "How the kernel is entered"), from the stack pointer
//! and flags the entry stub in `cpu` recorded before it changed either,
//! and of the hand-off structure's address; what the kernel reports of the
//! structure; and decimal numbers for the serial port.
//!
//! This file has no hardware code, so `xtask` also compiles it on the
//! development machine (with `#[path]`) and `cargo test --package xtask`
//! runs the tests at the bottom.

use bismuth_elf::place::{HANDOFF_ADDRESS, PAGE_SIZE};
use bismuth_handoff::{Handoff, Kind, ReadError};

/// `RFLAGS.IF`, bit 9: interrupts are on (SDM volume 1, section "EFLAGS
/// Register", which `RFLAGS` extends).
const INTERRUPT_FLAG: u64 = 1 << 9;
/// `RFLAGS.DF`, bit 10: string instructions run backwards.
const DIRECTION_FLAG: u64 = 1 << 10;
/// The lowest address of the top half of the address range, where the
/// kernel and its stack live.
const UPPER_HALF: u64 = 0xFFFF_8000_0000_0000;

/// A way the entry state breaks the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryError {
    /// Interrupts were on.
    InterruptsOn,
    /// The direction flag was set.
    DirectionSet,
    /// The stack pointer was not 8 below a multiple of 16, as it must be
    /// on entry to a System V function.
    Misaligned(u64),
    /// The stack pointer was in the lower half, not on the kernel's stack.
    LowerHalf(u64),
}

/// Checks the stack pointer and flags as they were at the entry point.
pub fn check(stack_pointer: u64, flags: u64) -> Result<(), EntryError> {
    if flags & INTERRUPT_FLAG != 0 {
        return Err(EntryError::InterruptsOn);
    }
    if flags & DIRECTION_FLAG != 0 {
        return Err(EntryError::DirectionSet);
    }
    if stack_pointer % 16 != 8 {
        return Err(EntryError::Misaligned(stack_pointer));
    }
    if stack_pointer < UPPER_HALF {
        return Err(EntryError::LowerHalf(stack_pointer));
    }
    Ok(())
}

/// What went wrong, as the text after `BISMUTH KERNEL ENTRY ERROR: `, and
/// the stack pointer to print after it, if it is the stack pointer.
pub fn reason(error: EntryError) -> (&'static str, Option<u64>) {
    match error {
        EntryError::InterruptsOn => ("interrupts are on", None),
        EntryError::DirectionSet => ("the direction flag is set", None),
        EntryError::Misaligned(at) => ("stack pointer not 8 below a multiple of 16:", Some(at)),
        EntryError::LowerHalf(at) => ("stack pointer in the lower half:", Some(at)),
    }
}

/// Why the hand-off structure cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    /// Its address is in the lower half.
    LowerHalf(u64),
    /// Its address does not start a page.
    Unaligned(u64),
    /// Its address is not the window the entry contract names.
    Elsewhere(u64),
    /// The reader refused the bytes.
    Read(ReadError),
}

/// Checks the address the boot stage passed: in the top half, on a page,
/// and the fixed window `HANDOFF_ADDRESS`, which the boot stage mapped.
pub fn check_handoff(address: u64) -> Result<(), HandoffError> {
    if address < UPPER_HALF {
        return Err(HandoffError::LowerHalf(address));
    }
    if !address.is_multiple_of(PAGE_SIZE) {
        return Err(HandoffError::Unaligned(address));
    }
    if address != HANDOFF_ADDRESS {
        return Err(HandoffError::Elsewhere(address));
    }
    Ok(())
}

/// What went wrong, as the text after `BISMUTH KERNEL HANDOFF ERROR: `,
/// and the address to print after it, if there is one.
pub fn handoff_reason(error: HandoffError) -> (&'static str, Option<u64>) {
    match error {
        HandoffError::LowerHalf(at) => ("address in the lower half:", Some(at)),
        HandoffError::Unaligned(at) => ("address not on a page:", Some(at)),
        HandoffError::Elsewhere(at) => ("address not the hand-off window:", Some(at)),
        HandoffError::Read(error) => (read_reason(error), None),
    }
}

fn read_reason(error: ReadError) -> &'static str {
    match error {
        ReadError::Truncated => "shorter than a header",
        ReadError::BadMarker => "the marker is wrong",
        ReadError::UnsupportedVersion { .. } => "unsupported major version",
        ReadError::LengthPastEnd => "its length runs past the window",
        ReadError::BadHeaderSize => "bad header size",
        ReadError::RegionsPastLength => "the regions run past its length",
        ReadError::BadRegion { .. } => "a region is unaligned, empty or past 2^52",
        ReadError::NotSorted { .. } => "the regions are out of order or overlap",
    }
}

/// What the kernel reports of the structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// How many regions it lists.
    pub regions: u64,
    /// The pages the kernel may use at once, as the reader sums them.
    pub usable: u64,
    /// The pages that are the kernel's own.
    pub kernel: u64,
}

/// Counts the regions of `handoff`, its usable pages and the kernel's own.
pub fn memory(handoff: &Handoff) -> Memory {
    let mut memory = Memory {
        regions: 0,
        usable: handoff.usable_pages(),
        kernel: 0,
    };
    for region in handoff.regions() {
        memory.regions += 1;
        if region.kind == Kind::Kernel {
            memory.kernel = memory.kernel.saturating_add(region.pages);
        }
    }
    memory
}

/// `value` in decimal, in `buffer`, without leading zeros.
pub fn decimal(value: u64, buffer: &mut [u8; 20]) -> &str {
    let mut rest = value;
    let mut start = buffer.len();
    // 20 digits hold `u64::MAX`, so `start` never passes zero.
    while let Some(slot) = start.checked_sub(1).and_then(|at| buffer.get_mut(at)) {
        start -= 1;
        *slot = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    core::str::from_utf8(buffer.get(start..).unwrap_or_default()).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state the boot stage enters the kernel in: reserved flag bit 1
    /// set, as it always is, and the stack's top less 8.
    const FLAGS: u64 = 0x2;
    const STACK: u64 = 0xFFFF_FFFF_7FFF_FFF8;

    #[test]
    fn the_documented_entry_state_passes() {
        assert_eq!(check(STACK, FLAGS), Ok(()));
        // Other flags, such as the arithmetic ones, are not checked.
        assert_eq!(check(STACK, FLAGS | 0x8D5), Ok(()));
    }

    #[test]
    fn each_broken_part_is_named() {
        let cases = [
            (STACK, FLAGS | 1 << 9, EntryError::InterruptsOn),
            (STACK, FLAGS | 1 << 10, EntryError::DirectionSet),
            (STACK + 8, FLAGS, EntryError::Misaligned(STACK + 8)),
            (STACK - 8, FLAGS, EntryError::Misaligned(STACK - 8)),
            (STACK - 4, FLAGS, EntryError::Misaligned(STACK - 4)),
            // What `mov edi, esp` leaves: the low half of the stack pointer.
            (0x7FFF_FFF8, FLAGS, EntryError::LowerHalf(0x7FFF_FFF8)),
            (UPPER_HALF - 8, FLAGS, EntryError::LowerHalf(UPPER_HALF - 8)),
        ];
        for (stack, flags, error) in cases {
            assert_eq!(check(stack, flags), Err(error), "{stack:#x} {flags:#x}");
        }
        assert_eq!(check(UPPER_HALF + 8, FLAGS), Ok(()));
        // Interrupts are named first when several parts are wrong.
        assert_eq!(check(1, !0), Err(EntryError::InterruptsOn));
    }

    #[test]
    fn every_reason_reads_differently() {
        let mut texts: Vec<_> = [
            EntryError::InterruptsOn,
            EntryError::DirectionSet,
            EntryError::Misaligned(0),
            EntryError::LowerHalf(0),
        ]
        .map(|error| reason(error).0)
        .to_vec();
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), 4);
        assert_eq!(reason(EntryError::Misaligned(7)).1, Some(7));
        assert_eq!(reason(EntryError::InterruptsOn).1, None);
    }

    /// The window the entry contract names, written out rather than taken
    /// from the constant under test.
    const WINDOW: u64 = 0xFFFF_FFFF_7FFE_0000;

    #[test]
    fn only_the_contracts_window_is_accepted_as_the_handoff_address() {
        assert_eq!(check_handoff(WINDOW), Ok(()));
        let cases = [
            // What the old stub's `rdi` held: the stack pointer.
            (STACK, HandoffError::Unaligned(STACK)),
            (0, HandoffError::LowerHalf(0)),
            (0x7FFE_0000, HandoffError::LowerHalf(0x7FFE_0000)),
            (WINDOW + 8, HandoffError::Unaligned(WINDOW + 8)),
            (WINDOW + 0x1000, HandoffError::Elsewhere(WINDOW + 0x1000)),
            (UPPER_HALF, HandoffError::Elsewhere(UPPER_HALF)),
        ];
        for (address, error) in cases {
            assert_eq!(check_handoff(address), Err(error), "{address:#x}");
        }
    }

    #[test]
    fn every_handoff_reason_reads_differently() {
        let errors = [
            HandoffError::LowerHalf(0),
            HandoffError::Unaligned(0),
            HandoffError::Elsewhere(0),
            HandoffError::Read(ReadError::Truncated),
            HandoffError::Read(ReadError::BadMarker),
            HandoffError::Read(ReadError::UnsupportedVersion { major: 2, minor: 0 }),
            HandoffError::Read(ReadError::LengthPastEnd),
            HandoffError::Read(ReadError::BadHeaderSize),
            HandoffError::Read(ReadError::RegionsPastLength),
            HandoffError::Read(ReadError::BadRegion { index: 0 }),
            HandoffError::Read(ReadError::NotSorted { index: 1 }),
        ];
        let mut texts: Vec<_> = errors.map(|error| handoff_reason(error).0).to_vec();
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), errors.len());
        assert_eq!(handoff_reason(HandoffError::Elsewhere(9)).1, Some(9));
        assert_eq!(
            handoff_reason(HandoffError::Read(ReadError::BadMarker)),
            ("the marker is wrong", None)
        );
    }

    /// A structure written byte by byte as `docs/handoff.md` lays it out:
    /// each region a start, a number of pages and a kind.
    fn structure(regions: &[(u64, u64, u32)]) -> Vec<u8> {
        let length = 24 + 24 * regions.len() as u32;
        let mut bytes = b"BISMHAND".to_vec();
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(length.to_le_bytes());
        bytes.extend(24u32.to_le_bytes());
        bytes.extend((regions.len() as u32).to_le_bytes());
        for &(start, pages, kind) in regions {
            bytes.extend(start.to_le_bytes());
            bytes.extend(pages.to_le_bytes());
            bytes.extend(kind.to_le_bytes());
            bytes.extend([0; 4]);
        }
        bytes
    }

    #[test]
    fn the_memory_counts_usable_and_kernel_pages_apart() {
        // Usable, kernel, loader, kernel, usable, boot services, reserved.
        let bytes = structure(&[
            (0, 100, 1),
            (0x64_000, 7, 2),
            (0x6B_000, 3, 4),
            (0x6E_000, 16, 2),
            (0x7E_000, 50, 1),
            (0xB0_000, 9, 3),
            (0xC0_000, 1000, 10),
        ]);
        let handoff = Handoff::read(&bytes).expect("readable");
        assert_eq!(
            memory(&handoff),
            Memory {
                regions: 7,
                usable: 150,
                kernel: 23
            }
        );
        let empty = structure(&[]);
        let handoff = Handoff::read(&empty).expect("readable");
        assert_eq!(
            memory(&handoff),
            Memory {
                regions: 0,
                usable: 0,
                kernel: 0
            }
        );
    }

    #[test]
    fn decimal_numbers_have_no_leading_zeros() {
        let mut buffer = [0; 20];
        for (value, text) in [
            (0, "0"),
            (7, "7"),
            (10, "10"),
            (52_559, "52559"),
            (u64::MAX, "18446744073709551615"),
        ] {
            assert_eq!(decimal(value, &mut buffer), text);
        }
    }
}

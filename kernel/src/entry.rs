//! The checks the kernel makes of the state the boot stage entered it in
//! (`docs/boot.md`, "How the kernel is entered"), from the stack pointer
//! and flags the entry stub in `cpu` recorded before it changed either.
//!
//! This file has no hardware code, so `xtask` also compiles it on the
//! development machine (with `#[path]`) and `cargo test --package xtask`
//! runs the tests at the bottom.

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
}

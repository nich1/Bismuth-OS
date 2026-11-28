//! The kernel's way in when the processor raises an exception or takes an
//! interrupt: one entry stub per vector, 0 to 255, that makes the stack the
//! same [`Frame`] whatever the vector, then one Rust handler that reports
//! it and stops.
//!
//! The stubs are assembled by a `.rept` loop from [`ERROR_CODE_VECTORS`],
//! the mask built from the library's exception table, so which vectors get
//! the stub's zero comes from that table alone. Each stub:
//!
//! 1. pushes a zero, unless the processor pushed an error code;
//! 2. pushes its vector number;
//! 3. jumps to the common stub, which passes the stack pointer, now the
//!    frame's address, to [`report`] with the stack aligned and the
//!    direction flag clear, as a System V function expects.
//!
//! No handler returns yet, so no stub restores registers or runs `iretq`;
//! the general-purpose registers are not saved. The stubs' addresses go into
//! a table, `bismuth_vector_stubs`, that `takeover` builds the gates from.
//!
//! The block is in AT&T syntax, where `$` marks a number to push, so the
//! loop's counter cannot be mistaken for a memory operand.

use core::arch::global_asm;

use crate::fault::{ERROR_CODE_VECTORS, Frame};
use crate::{cpu, serial};

/// How many vectors there are, each with a stub.
pub const VECTORS: usize = 256;

// SAFETY (of what the stubs do): each is entered only through its gate,
// with the processor's frame on the stack. It pushes two words below it and
// calls `report`, which never returns. The common stub then aligns the
// stack down, staying on the same stack.
global_asm!(
    ".pushsection .rodata.bismuth_vector_stubs, \"a\"",
    ".balign 8",
    ".globl bismuth_vector_stubs",
    "bismuth_vector_stubs:",
    ".popsection",
    ".pushsection .text.bismuth_vectors, \"ax\"",
    ".set bismuth_vector, 0",
    ".rept {vectors}",
    "2:",
    ".if bismuth_vector >= 32",
    "pushq $0",
    ".elseif (({mask} >> bismuth_vector) & 1) == 0",
    "pushq $0",
    ".endif",
    "pushq $bismuth_vector",
    "jmp bismuth_vector_common",
    // Record this stub's address in the table.
    ".pushsection .rodata.bismuth_vector_stubs, \"a\"",
    ".quad 2b",
    ".popsection",
    ".set bismuth_vector, bismuth_vector + 1",
    ".endr",
    "bismuth_vector_common:",
    "movq %rsp, %rdi",
    "andq $-16, %rsp",
    "cld",
    "call {report}",
    "ud2",
    ".popsection",
    vectors = const VECTORS,
    mask = const ERROR_CODE_VECTORS,
    report = sym report,
    options(att_syntax),
);

unsafe extern "C" {
    /// The address of each vector's stub, by vector, filled in by the
    /// assembler above.
    static bismuth_vector_stubs: [u64; VECTORS];
}

/// The address of `vector`'s entry stub, or `None` past the last vector.
pub fn stub(vector: usize) -> Option<u64> {
    // SAFETY: the table is in read-only memory, written by the assembler and
    // linker, never by anything at run time, and has `VECTORS` entries, one
    // per `.rept` turn above.
    let stubs = unsafe { &bismuth_vector_stubs };
    stubs.get(vector).copied()
}

/// The handler every stub calls: prints the kernel's fault line on the
/// serial port and stops through the kernel's failure path.
extern "sysv64" fn report(frame: *const Frame) -> ! {
    // SAFETY: only the common stub calls this, with the address of the
    // seven words its vector's stub and the processor have just pushed,
    // which is a `Frame`; nothing else writes them while this runs.
    let frame = unsafe { frame.read() };
    let serial = serial::Com1::init();
    crate::fault::write_report(&frame, cpu::cr2(), &mut |text| serial.write_str(text));
    crate::stop_failed()
}

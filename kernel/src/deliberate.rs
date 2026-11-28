//! Deliberate faults, for the boot test: with one of the kernel's `fault-*`
//! features on, the kernel makes that fault once its tables are loaded, and
//! the test checks the kernel's own report of it. Each one that is about a
//! page's permissions shows the processor enforcing what the boot stage
//! mapped. With no such feature on, [`make_if_asked`] does nothing.
//!
//! Each fault is made by one instruction in an `asm!` block, so what the
//! processor sees is exactly that instruction. If a fault did not happen,
//! the block would go on and the kernel would print its normal line, which
//! the test fails.

use core::arch::asm;
use core::hint::black_box;
use core::sync::atomic::AtomicU8;

/// The page just below the kernel's stack, which the boot stage leaves
/// unmapped (`docs/boot.md`, "How the kernel is entered").
const STACK_GUARD: u64 = 0xFFFF_FFFF_7FFE_F000;

/// A byte in the kernel's read-only segment.
static READ_ONLY: u8 = 0;
/// A byte in the kernel's writable segment, not executable: `ret`, so that
/// if it ran, the call below would come back and the kernel go on.
static NOT_CODE: AtomicU8 = AtomicU8::new(0xC3);

/// Makes the fault the build's `fault-*` feature asks for, if any.
pub fn make_if_asked() {
    if cfg!(feature = "fault-divide") {
        // SAFETY: divides by zero, which raises #DE before anything
        // changes; only `eax`, `ecx` and `edx`, declared, could change.
        unsafe {
            asm!("xor ecx, ecx", "div ecx", out("eax") _, out("ecx") _, out("edx") _,
                options(nomem, nostack));
        }
    }
    if cfg!(feature = "fault-invalid-opcode") {
        // SAFETY: `ud2` raises #UD and does nothing else.
        unsafe { asm!("ud2", options(nomem, nostack)) }
    }
    if cfg!(feature = "fault-write-read-only") {
        // SAFETY: the byte's page is mapped read-only and `takeover` has set
        // CR0.WP, so the processor refuses the write with #PF before any
        // byte changes, and the handler never returns.
        unsafe {
            asm!("mov byte ptr [{}], 1", in(reg) &raw const READ_ONLY, options(nostack));
        }
    }
    if cfg!(feature = "fault-run-data") {
        // SAFETY: the byte's page is mapped not executable, so fetching it
        // raises #PF; were it run, it is `ret`, which comes straight back
        // and changes nothing but the stack pointer, back to where it was.
        unsafe { asm!("call {}", in(reg) NOT_CODE.as_ptr()) }
    }
    if cfg!(feature = "fault-guard-page") {
        // SAFETY: reads one byte from a page that is not mapped, which
        // raises #PF; only the declared output register could change.
        unsafe {
            asm!("mov {}, byte ptr [{}]", out(reg_byte) _, in(reg) STACK_GUARD,
                options(nostack, readonly));
        }
    }
    if cfg!(feature = "fault-stack-overflow") {
        black_box(overflow(0));
    }
    if cfg!(feature = "fault-last-vector") {
        // SAFETY: `int 255` enters the last gate, a kernel stub that reports
        // and never returns.
        unsafe { asm!("int 255", options(nomem)) }
    }
}

/// Calls itself until the stack runs into its guard page, with a buffer in
/// each call so that takes a few hundred calls, not thousands.
#[allow(unconditional_recursion, reason = "it ends in a fault, as intended")]
fn overflow(depth: u64) -> u64 {
    let buffer = black_box([depth; 32]);
    overflow(depth + 1).wrapping_add(buffer[0])
}

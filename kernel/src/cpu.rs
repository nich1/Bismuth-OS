//! Processor instructions the kernel uses. The kernel's `unsafe` lives
//! here and in `port` and `serial`.

use core::arch::asm;

/// Stops the processor for good, waking only to go back to sleep.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `hlt` pauses the processor until the next interrupt. The
        // kernel runs in ring 0, where the instruction is allowed, and it
        // changes no memory or registers.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) }
    }
}

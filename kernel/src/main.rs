//! Bismuth's kernel.
//!
//! The boot stage maps this ELF file at its addresses, gives it a stack and
//! jumps to [`_start`]; the state it is entered in is written down in
//! `docs/boot.md`, "How the kernel is entered". For now the kernel prints
//! one line on the serial port and stops.

#![no_std]
#![no_main]

mod cpu;
mod port;
mod serial;

use core::panic::PanicInfo;

/// The line the boot test looks for once the kernel runs.
const KERNEL_MESSAGE: &str = "BISMUTH KERNEL OK\n";
/// The line the panic handler prints.
const PANIC_MESSAGE: &str = "BISMUTH KERNEL PANIC\n";

/// Entry point. The linker's default entry symbol for this target is
/// `_start`, so the ELF file's entry address points here.
///
/// The boot stage enters it with a jump, not a call, but leaves the
/// processor exactly as a call from System V x86-64 code would: the stack
/// pointer is 8 below a 16-byte boundary, as after a `call` pushes its
/// return address, and the direction flag is clear, which is what the
/// System V AMD64 psABI, section 3.2 "Function Calling Sequence", asks of
/// a function's entry; and there are no arguments to pass. So an ordinary `extern "sysv64"` function is sound
/// here, and no hand-written stub is needed to set up the stack first. The
/// return address the compiler sees is the zero in the stack's top eight
/// bytes; the function never returns, so it is never used.
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start() -> ! {
    if cfg!(feature = "panic-at-entry") {
        panic!("the panic-at-entry test feature is on");
    }
    serial::Com1::init().write_str(KERNEL_MESSAGE);
    #[cfg(feature = "qemu-exit")]
    port::qemu_exit(port::KERNEL_SUCCESS);
    cpu::halt_forever()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial::Com1::init().write_str(PANIC_MESSAGE);
    #[cfg(feature = "qemu-exit")]
    port::qemu_exit(port::KERNEL_FAILURE);
    cpu::halt_forever()
}

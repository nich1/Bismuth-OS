//! Bismuth's kernel.
//!
//! The boot stage will load this ELF file and jump to [`_start`]. For now the
//! kernel does nothing but stop the processor.

#![no_std]
#![no_main]

mod cpu;

use core::panic::PanicInfo;

/// Entry point. The linker's default entry symbol for this target is
/// `_start`, so the ELF file's entry address points here.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    cpu::halt_forever()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    cpu::halt_forever()
}

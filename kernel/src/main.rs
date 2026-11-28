//! Bismuth's kernel.
//!
//! The boot stage maps this ELF file at its addresses, gives it a stack and
//! jumps to its entry point, the stub `cpu::_start`; the state it is entered
//! in is written down in `docs/boot.md`, "How the kernel is entered". The
//! kernel checks what of that state it can, loads its own descriptor tables
//! in place of the firmware's, prints one line on the serial port and stops.

#![no_std]
#![no_main]

mod cpu;
mod deliberate;
mod entry;
mod fault;
mod load;
mod port;
mod serial;
mod takeover;
mod vectors;

use core::panic::PanicInfo;

/// The line the boot test looks for once the kernel runs.
const KERNEL_MESSAGE: &str = "BISMUTH KERNEL OK\n";
/// The line the panic handler prints.
const PANIC_MESSAGE: &str = "BISMUTH KERNEL PANIC\n";
/// The start of the line printed when the entry state breaks the contract.
const ENTRY_ERROR: &str = "BISMUTH KERNEL ENTRY ERROR: ";

/// The kernel proper, called by the entry stub with the stack pointer and
/// `RFLAGS` as they were at the entry point.
extern "sysv64" fn kernel_main(stack_pointer: u64, flags: u64) -> ! {
    if let Err(error) = entry::check(stack_pointer, flags) {
        let serial = serial::Com1::init();
        let (text, value) = entry::reason(error);
        serial.write_str(ENTRY_ERROR);
        serial.write_str(text);
        if let Some(value) = value {
            serial.write_str(" ");
            serial.write_hex(value);
        }
        serial.write_str("\n");
        stop_failed();
    }
    takeover::run();
    if cfg!(feature = "panic-at-entry") {
        panic!("the panic-at-entry test feature is on");
    }
    deliberate::make_if_asked();
    serial::Com1::init().write_str(KERNEL_MESSAGE);
    #[cfg(feature = "qemu-exit")]
    port::qemu_exit(port::KERNEL_SUCCESS);
    cpu::halt_forever()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial::Com1::init().write_str(PANIC_MESSAGE);
    stop_failed()
}

/// Stops after a failure: with `qemu-exit`, QEMU exits with the kernel's
/// failure code.
fn stop_failed() -> ! {
    #[cfg(feature = "qemu-exit")]
    port::qemu_exit(port::KERNEL_FAILURE);
    cpu::halt_forever()
}

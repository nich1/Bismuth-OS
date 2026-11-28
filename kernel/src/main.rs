//! Bismuth's kernel.
//!
//! The boot stage maps this ELF file at its addresses, gives it a stack and
//! jumps to its entry point, the stub `cpu::_start`; the state it is entered
//! in is written down in `docs/boot.md`, "How the kernel is entered". The
//! kernel checks what of that state it can, reads the hand-off structure
//! the boot stage left it, prints what it was told and one more line on the
//! serial port, and stops.

#![no_std]
#![no_main]

mod cpu;
mod entry;
mod handoff;
mod port;
mod serial;

use core::panic::PanicInfo;

/// The line the boot test looks for once the kernel runs.
const KERNEL_MESSAGE: &str = "BISMUTH KERNEL OK\n";
/// The line the panic handler prints.
const PANIC_MESSAGE: &str = "BISMUTH KERNEL PANIC\n";
/// The start of the line printed when the entry state breaks the contract.
const ENTRY_ERROR: &str = "BISMUTH KERNEL ENTRY ERROR: ";
/// The start of the line printed when the hand-off structure is unusable.
const HANDOFF_ERROR: &str = "BISMUTH KERNEL HANDOFF ERROR: ";

/// The kernel proper, called by the entry stub with the hand-off
/// structure's address, and the stack pointer and `RFLAGS` as they were at
/// the entry point.
extern "sysv64" fn kernel_main(handoff: u64, stack_pointer: u64, flags: u64) -> ! {
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
    let serial = serial::Com1::init();
    let read = entry::check_handoff(handoff).and_then(|()| {
        // SAFETY: the entry checks passed and the address is the window
        // the entry contract names, which the boot stage mapped and wrote.
        let bytes = unsafe { handoff::window() };
        bismuth_handoff::Handoff::read(bytes).map_err(entry::HandoffError::Read)
    });
    match read {
        Ok(read) => {
            let memory = entry::memory(&read);
            let mut digits = [0; 20];
            serial.write_str("BISMUTH KERNEL MEMORY regions=");
            serial.write_str(entry::decimal(memory.regions, &mut digits));
            serial.write_str(" usable=");
            serial.write_str(entry::decimal(memory.usable, &mut digits));
            serial.write_str(" kernel=");
            serial.write_str(entry::decimal(memory.kernel, &mut digits));
            serial.write_str("\n");
        }
        Err(error) => {
            let (text, value) = entry::handoff_reason(error);
            serial.write_str(HANDOFF_ERROR);
            serial.write_str(text);
            if let Some(value) = value {
                serial.write_str(" ");
                serial.write_hex(value);
            }
            serial.write_str("\n");
            stop_failed();
        }
    }
    if cfg!(feature = "panic-at-entry") {
        panic!("the panic-at-entry test feature is on");
    }
    serial.write_str(KERNEL_MESSAGE);
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

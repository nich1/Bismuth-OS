//! Bismuth's first boot stage.
//!
//! The firmware (UEFI) loads this program from the boot disk and calls
//! [`efi_main`]. For now it only proves the boot path end to end: it writes
//! one line to the serial port and one to the firmware console, then stops.
//! Loading a kernel comes later. See `docs/boot.md`.

#![no_std]
#![no_main]

mod port;
mod serial;
mod uefi;

use core::arch::asm;
use core::panic::PanicInfo;

/// The line the automated boot test looks for on the serial port.
const BOOT_MESSAGE: &str = "BISMUTH BOOT OK\n";

/// Entry point. The firmware calls this once, with boot services running.
///
/// # Safety
///
/// Only UEFI firmware may call this, passing the system table pointer the
/// UEFI specification defines for a loaded program's entry point.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(
    _image: uefi::Handle,
    system_table: *mut uefi::SystemTable,
) -> uefi::Status {
    let serial = serial::Com1::init();
    serial.write_str(BOOT_MESSAGE);

    // SAFETY: by this function's contract, `system_table` is the pointer the
    // firmware handed to the entry point, and boot services are still running.
    if let Some(console) = unsafe { uefi::Console::from_system_table(system_table) } {
        console.write_str("Bismuth boot stage reached.\n");
    }

    #[cfg(feature = "qemu-exit")]
    qemu_exit_success();

    halt()
}

/// Tells QEMU to quit with a success code, so the boot test can finish.
#[cfg(feature = "qemu-exit")]
fn qemu_exit_success() {
    /// Port of QEMU's `isa-debug-exit` device, as configured by `xtask`.
    const DEBUG_EXIT_PORT: u16 = 0xF4;
    /// QEMU exits with `(value << 1) | 1`, so this becomes exit code 33.
    const SUCCESS: u32 = 0x10;

    // SAFETY: this function only exists in the `qemu-exit` build, which the
    // boot test runs inside QEMU with the debug-exit device at this port.
    unsafe { port::write_u32(DEBUG_EXIT_PORT, SUCCESS) }
}

/// Stops the processor for good, waking only to go back to sleep.
fn halt() -> ! {
    loop {
        // SAFETY: `hlt` pauses the processor until the next interrupt. UEFI
        // programs run in ring 0, where the instruction is allowed, and it
        // changes no memory or registers.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial::Com1::init().write_str("BISMUTH PANIC\n");
    halt()
}

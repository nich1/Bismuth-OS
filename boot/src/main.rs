//! Bismuth's first boot stage.
//!
//! The firmware (UEFI) loads this program from the boot disk and calls
//! [`efi_main`]. It writes one line to the serial port and one to the
//! firmware console, reads the kernel file from the disk it was loaded from
//! and checks it with the ELF reader, copies its segments into memory, gives
//! them their kernel addresses in new address tables, reports each step on
//! the serial port, then stops. Starting the kernel comes later. See
//! `docs/boot.md`.

#![no_std]
#![no_main]

mod cpu;
mod load;
mod map;
mod memory;
mod port;
mod report;
mod serial;
mod uefi;

use core::arch::asm;
use core::panic::PanicInfo;

use bismuth_elf::Elf;
use report::{Failure, Step};

/// The line the automated boot test looks for on the serial port.
const BOOT_MESSAGE: &str = "BISMUTH BOOT OK\n";

/// Where the kernel file is on the boot disk.
const KERNEL_PATH: uefi::FilePath<24> = uefi::FilePath::new(r"\EFI\BISMUTH\KERNEL.ELF");

/// The largest kernel file accepted, 64 MiB: far more than a kernel needs,
/// small enough to fit in the memory of any machine that could run it.
const KERNEL_LIMIT: u64 = 64 * 1024 * 1024;

/// Entry point. The firmware calls this once, with boot services running.
///
/// # Safety
///
/// Only UEFI firmware may call this, passing the system table pointer the
/// UEFI specification defines for a loaded program's entry point.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(
    image: uefi::Handle,
    system_table: *mut uefi::SystemTable,
) -> uefi::Status {
    let serial = serial::Com1::init();
    serial.write_str(BOOT_MESSAGE);

    // SAFETY: by this function's contract, `system_table` is the pointer the
    // firmware handed to the entry point, and boot services are still running.
    let console = unsafe { uefi::Console::from_system_table(system_table) };
    if let Some(console) = &console {
        console.write_str("Bismuth boot stage reached.\n");
    }

    // SAFETY: as above, and `image` is the handle the firmware passed with
    // it. Boot services stay running: nothing here exits them.
    let boot_services = unsafe { uefi::BootServices::from_system_table(image, system_table) };
    let checked = read_kernel(boot_services.as_ref());
    let line = match &checked {
        Ok((bytes, elf)) => report::ok_line(elf.entry(), elf.segments().count(), bytes.len()),
        Err(failure) => report::error_line(failure),
    };
    serial.write_str(line.as_str());
    serial.write_str("\n");

    let copied = match (&checked, &boot_services) {
        (Ok((bytes, elf)), Some(boot_services)) => {
            Some(load::Kernel::copy(boot_services, bytes, elf))
        }
        _ => None,
    };
    if let Some(copied) = &copied {
        let line = match copied {
            Ok(kernel) => {
                report::copied_line(kernel.segments().count(), kernel.pages(), kernel.sum())
            }
            Err(failure) => report::place_error_line(failure),
        };
        serial.write_str(line.as_str());
        serial.write_str("\n");
    }

    let placed = match (&copied, &boot_services) {
        (Some(Ok(kernel)), Some(boot_services)) => {
            Some((kernel, map::place(boot_services, kernel)))
        }
        _ => None,
    };
    if let Some((kernel, placed)) = &placed {
        let line = match placed {
            Ok(placed) => report::placed_line(
                kernel.pages(),
                placed.sum,
                placed.permissions(),
                placed.write_protect,
            ),
            Err(failure) => report::map_error_line(failure),
        };
        serial.write_str(line.as_str());
        serial.write_str("\n");
        // A firmware service, called under the new tables.
        if let Some(console) = &console {
            console.write_str("Bismuth kernel addresses checked.\n");
        }
    }

    #[cfg(feature = "qemu-exit")]
    qemu_exit(matches!(placed, Some((_, Ok(_)))));

    halt()
}

/// Reads the kernel file and checks it with the ELF reader. Its bytes stay
/// in memory for the rest of the boot.
fn read_kernel(
    boot_services: Option<&uefi::BootServices>,
) -> Result<(&'static [u8], Elf<'static>), Failure> {
    let boot_services = boot_services.ok_or(Failure::NoInterface(Step::BootServices))?;
    let bytes = boot_services.read_file(&KERNEL_PATH, KERNEL_LIMIT)?;
    let elf = Elf::parse(bytes).map_err(Failure::Elf)?;
    Ok((bytes, elf))
}

/// Tells QEMU to quit, so the boot test can finish: with exit code 33 if the
/// kernel file was read, checked, copied and given its addresses, 35 if
/// not.
#[cfg(feature = "qemu-exit")]
fn qemu_exit(success: bool) {
    /// Port of QEMU's `isa-debug-exit` device, as configured by `xtask`.
    const DEBUG_EXIT_PORT: u16 = 0xF4;
    /// QEMU exits with `(value << 1) | 1`: 33 and 35.
    const SUCCESS: u32 = 0x10;
    const FAILURE: u32 = 0x11;

    // SAFETY: this function only exists in the `qemu-exit` build, which the
    // boot test runs inside QEMU with the debug-exit device at this port.
    unsafe { port::write_u32(DEBUG_EXIT_PORT, if success { SUCCESS } else { FAILURE }) }
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

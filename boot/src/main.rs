//! Bismuth's first boot stage.
//!
//! The firmware (UEFI) loads this program from the boot disk and calls
//! [`efi_main`]. It writes one line to the serial port and one to the
//! firmware console, reads the kernel file from the disk it was loaded from
//! and checks it with the ELF reader, copies its segments into memory, gives
//! them their kernel addresses in new address tables along with a stack,
//! reads the memory map and ends the firmware's boot services, builds the
//! hand-off structure from the map, reports each step on the serial port,
//! then jumps to the kernel with the structure's address. On a failure it
//! stops instead. See `docs/boot.md`.

#![no_std]
#![no_main]

mod cpu;
mod handoff;
mod leave;
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
use bismuth_elf::place::{HANDOFF_ADDRESS, STACK_PAGES, STACK_TOP, TABLE_FRAMES};
use bismuth_handoff::Kind;
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

    let mut placed = match (&copied, &boot_services) {
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
        if let (Some(console), Ok(_)) = (&console, placed) {
            console.write_str("Bismuth kernel addresses checked.\n");
        }
    }

    // Leaving the firmware, once everything that allocates is done (the
    // kernel's pages, its tables, its stack and the hand-off pages: any
    // allocation changes the map key) and the firmware's console has
    // printed its last line. `leave` takes the console, so it cannot be
    // used past here.
    let mut left = match (&placed, boot_services) {
        (Some((_, Ok(_))), Some(boot_services)) => {
            let left = boot_services.leave(console);
            // At once, before anything else runs: the firmware's timer and
            // handlers belong to the boot services that have just ended.
            // Not before the call, because a failed call leaves boot
            // services running, which expect interrupts as they left them.
            if left.is_ok() {
                cpu::disable_interrupts();
            }
            Some(left)
        }
        _ => None,
    };
    if let Some(left) = &left {
        let line = match left {
            Ok(left) => {
                let usable = |kind| Kind::from_firmware_type(kind) == Kind::Usable;
                let (descriptors, pages) =
                    leave::count(left.bytes, left.map.descriptor_size, usable);
                report::firmware_left_line(descriptors, pages, left.boot_services)
            }
            Err(failure) => report::exit_error_line(failure),
        };
        serial.write_str(line.as_str());
        serial.write_str("\n");
    }

    // The hand-off structure, from the saved map, written after the exit
    // into pages set aside before it. The kernel's own: each segment's
    // pages, the stack and the whole block of table frames.
    let built = match (&mut left, &mut placed) {
        (Some(Ok(left)), Some((kernel, Ok(placed)))) => {
            let segments = kernel
                .segments()
                .map(|loaded| (loaded.physical, loaded.pages));
            let own = [(placed.stack, STACK_PAGES), (placed.tables, TABLE_FRAMES)];
            let built = handoff::build(
                left.bytes,
                left.map.descriptor_size,
                segments.chain(own),
                left.space,
                placed.handoff,
            );
            #[cfg(feature = "damage-handoff")]
            if let Some(byte) = placed.handoff.first_mut() {
                *byte ^= 0xFF;
            }
            if let Err(failure) = &built {
                serial.write_str(report::handoff_error_line(failure).as_str());
                serial.write_str("\n");
            }
            built.is_ok()
        }
        _ => false,
    };

    if let (true, Ok((_, elf))) = (built, &checked) {
        let entry = elf.entry();
        serial.write_str(report::jump_line(entry, STACK_TOP, HANDOFF_ADDRESS).as_str());
        serial.write_str("\n");
        // SAFETY: the boot services have ended (`built` is only true after
        // `leave` and `map::place` succeeded) and nothing calls the
        // firmware any more. `map::place` succeeded, so the tables in CR3
        // map every segment of the kernel at its address with its own
        // permissions, and the placement rules put `entry` inside a
        // segment that may be run; the walk found every page where it
        // belongs. They also map the stack's pages below `STACK_TOP`, a
        // 2 MiB boundary, read and write, cleared, with the guard page
        // below them missing, and the hand-off pages read-only at
        // `HANDOFF_ADDRESS`, into which the structure has just been
        // written in full. The kernel's entry is a stub that never returns
        // (`kernel/src/cpu.rs`). Nothing of this program's runs after the
        // jump: its memory, the tables and the copied pages are the
        // kernel's from then on.
        unsafe { cpu::enter_kernel(entry, STACK_TOP, HANDOFF_ADDRESS) }
    }

    #[cfg(feature = "qemu-exit")]
    qemu_exit_failure();

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

/// Tells QEMU to quit with exit code 35, so the boot test can finish: the
/// kernel could not be started. On success the kernel exits QEMU itself,
/// with codes of its own. (33, for `0x10`, was the boot stage's success
/// before it started the kernel, and is not used now.)
#[cfg(feature = "qemu-exit")]
fn qemu_exit_failure() {
    /// Port of QEMU's `isa-debug-exit` device, as configured by `xtask`.
    const DEBUG_EXIT_PORT: u16 = 0xF4;
    /// QEMU exits with `(value << 1) | 1`: 35.
    const FAILURE: u32 = 0x11;

    // SAFETY: this function only exists in the `qemu-exit` build, which the
    // boot test runs inside QEMU with the debug-exit device at this port.
    unsafe { port::write_u32(DEBUG_EXIT_PORT, FAILURE) }
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

//! x86 I/O port access, and the write that makes QEMU exit.
//!
//! Ports are a second address space, separate from memory, that older PC
//! devices such as the serial port are controlled through. The instructions
//! are `IN` and `OUT` (Intel SDM volume 2A, "IN—Input from Port", and volume
//! 2B, "OUT—Output to Port"). In ring 0, where the kernel runs, they are
//! always allowed, and they change no flags.

use core::arch::asm;

/// Writes one byte to an I/O port.
///
/// # Safety
///
/// A port write can reconfigure hardware. The caller must know which device
/// answers at `port` and that `value` is valid for it.
pub unsafe fn write_u8(port: u16, value: u8) {
    // SAFETY: `out` touches only the two named registers. The caller vouches
    // for the port and value.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value,
            options(nomem, nostack, preserves_flags));
    }
}

/// Reads one byte from an I/O port.
///
/// # Safety
///
/// Reading some ports has side effects, such as clearing a status flag. The
/// caller must know which device answers at `port`.
pub unsafe fn read_u8(port: u16) -> u8 {
    let value: u8;
    // SAFETY: `in` touches only the two named registers. The caller vouches
    // for the port.
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value,
            options(nomem, nostack, preserves_flags));
    }
    value
}

/// What the kernel writes to QEMU's debug-exit device once it has printed
/// its line. QEMU exits with `(value << 1) | 1`: 65, unlike the boot
/// stage's 33 and 35.
#[cfg(feature = "qemu-exit")]
pub const KERNEL_SUCCESS: u32 = 0x20;
/// What the kernel writes on a panic: QEMU exits with 67.
#[cfg(feature = "qemu-exit")]
pub const KERNEL_FAILURE: u32 = 0x21;

/// Tells QEMU to quit with exit code `(value << 1) | 1`. If no such device
/// is there, nothing happens and the caller goes on.
#[cfg(feature = "qemu-exit")]
pub fn qemu_exit(value: u32) {
    /// Port of QEMU's `isa-debug-exit` device, as `xtask` configures it.
    const DEBUG_EXIT_PORT: u16 = 0xF4;
    // SAFETY: this function only exists in the `qemu-exit` build, which the
    // boot test runs inside QEMU with the debug-exit device at this port;
    // `out` touches only the two named registers.
    unsafe {
        asm!("out dx, eax", in("dx") DEBUG_EXIT_PORT, in("eax") value,
            options(nomem, nostack, preserves_flags));
    }
}

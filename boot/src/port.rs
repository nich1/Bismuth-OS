//! x86 I/O port access.
//!
//! Ports are a second address space, separate from memory, that older PC
//! devices such as the serial port are controlled through.

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

/// Writes four bytes to an I/O port.
///
/// # Safety
///
/// Same contract as [`write_u8`].
#[cfg(feature = "qemu-exit")]
pub unsafe fn write_u32(port: u16, value: u32) {
    // SAFETY: `out` touches only the two named registers. The caller vouches
    // for the port and value.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value,
            options(nomem, nostack, preserves_flags));
    }
}

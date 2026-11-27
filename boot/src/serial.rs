//! Output on the first serial port (COM1).
//!
//! The serial port is the simplest way to get text out of a machine: no
//! graphics, no firmware help, one byte at a time. QEMU can connect it to the
//! terminal, which is how the boot test reads what the system printed.

use crate::port;

/// Base I/O port of COM1 on PC-compatible machines.
const COM1: u16 = 0x3F8;

/// Offsets of the 16550 UART registers from the base port.
mod reg {
    pub const DATA: u16 = 0;
    pub const INTERRUPT_ENABLE: u16 = 1;
    pub const FIFO_CONTROL: u16 = 2;
    pub const LINE_CONTROL: u16 = 3;
    pub const MODEM_CONTROL: u16 = 4;
    pub const LINE_STATUS: u16 = 5;
}

/// Line-status bit that is set when the port can accept another byte.
const TRANSMIT_READY: u8 = 0x20;

/// How many times to check for [`TRANSMIT_READY`] before sending anyway, so a
/// missing or stuck port can never hang the boot.
const SPIN_LIMIT: u32 = 100_000;

/// A handle to COM1 after it has been set up.
pub struct Com1 {
    _private: (),
}

impl Com1 {
    /// Sets COM1 to 115200 baud, 8 data bits, no parity, one stop bit.
    pub fn init() -> Self {
        // SAFETY: ports 0x3F8 to 0x3FF belong to the first serial port on
        // PC-compatible machines. This is the standard 16550 setup sequence,
        // and the writes are ignored where no serial port is fitted.
        unsafe {
            port::write_u8(COM1 + reg::INTERRUPT_ENABLE, 0x00);
            // Open the divisor latch, set divisor 1 (115200 baud), close it
            // again while selecting 8 data bits, no parity, one stop bit.
            port::write_u8(COM1 + reg::LINE_CONTROL, 0x80);
            port::write_u8(COM1 + reg::DATA, 0x01);
            port::write_u8(COM1 + reg::INTERRUPT_ENABLE, 0x00);
            port::write_u8(COM1 + reg::LINE_CONTROL, 0x03);
            // Enable and clear the send and receive buffers.
            port::write_u8(COM1 + reg::FIFO_CONTROL, 0xC7);
            // Raise the "terminal ready" and "request to send" lines.
            port::write_u8(COM1 + reg::MODEM_CONTROL, 0x03);
        }
        Com1 { _private: () }
    }

    /// Sends one byte, waiting briefly for the port to be ready.
    pub fn write_byte(&self, byte: u8) {
        for _ in 0..SPIN_LIMIT {
            // SAFETY: reading COM1's line-status register has no side effects
            // that matter here.
            let status = unsafe { port::read_u8(COM1 + reg::LINE_STATUS) };
            if status & TRANSMIT_READY != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // SAFETY: COM1's data register accepts any byte.
        unsafe { port::write_u8(COM1 + reg::DATA, byte) }
    }

    /// Sends text, turning each newline into carriage return plus newline as
    /// serial terminals expect.
    pub fn write_str(&self, text: &str) {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }
}

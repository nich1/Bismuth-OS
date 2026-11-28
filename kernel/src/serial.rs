//! Output on the first serial port (COM1), a 16550-compatible UART at I/O
//! port 0x3F8 on PC-compatible machines. The registers and bits are those
//! of the PC16550D data sheet.
//!
//! The kernel sets the port up itself rather than rely on how the boot
//! stage left it, so it depends on nothing the boot stage did here. It
//! first waits for the boot stage's last bytes to leave, since setting the
//! port up clears its send buffer.

use crate::port;

/// Base I/O port of COM1.
const COM1: u16 = 0x3F8;

/// Offsets of the UART's registers from the base port.
const DATA: u16 = 0;
const INTERRUPT_ENABLE: u16 = 1;
const FIFO_CONTROL: u16 = 2;
const LINE_CONTROL: u16 = 3;
const MODEM_CONTROL: u16 = 4;
const LINE_STATUS: u16 = 5;

/// Line-status bit 5: the port can take another byte.
const TRANSMIT_READY: u8 = 1 << 5;
/// Line-status bit 6: every byte has been sent.
const TRANSMITTER_EMPTY: u8 = 1 << 6;

/// How many times to check a line-status bit before going on anyway, so a
/// missing or stuck port can never hang the kernel.
const SPIN_LIMIT: u32 = 100_000;

/// A handle to COM1 after it has been set up.
pub struct Com1 {
    _private: (),
}

impl Com1 {
    /// Sets COM1 to 115200 baud, 8 data bits, no parity, one stop bit, with
    /// its interrupts off.
    pub fn init() -> Self {
        wait_for(TRANSMITTER_EMPTY);
        // SAFETY: ports 0x3F8 to 0x3FF belong to the first serial port on
        // PC-compatible machines. This is the standard 16550 setup sequence,
        // and the writes are ignored where no serial port is fitted.
        unsafe {
            port::write_u8(COM1 + INTERRUPT_ENABLE, 0x00);
            // Open the divisor latch, set divisor 1 (115200 baud), close it
            // again while selecting 8 data bits, no parity, one stop bit.
            port::write_u8(COM1 + LINE_CONTROL, 0x80);
            port::write_u8(COM1 + DATA, 0x01);
            port::write_u8(COM1 + INTERRUPT_ENABLE, 0x00);
            port::write_u8(COM1 + LINE_CONTROL, 0x03);
            // Enable and clear the send and receive buffers.
            port::write_u8(COM1 + FIFO_CONTROL, 0xC7);
            // Raise the "terminal ready" and "request to send" lines.
            port::write_u8(COM1 + MODEM_CONTROL, 0x03);
        }
        Com1 { _private: () }
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

    /// Sends one byte, waiting briefly for the port to be ready.
    fn write_byte(&self, byte: u8) {
        wait_for(TRANSMIT_READY);
        // SAFETY: COM1's data register accepts any byte.
        unsafe { port::write_u8(COM1 + DATA, byte) }
    }
}

/// Waits, a bounded time, for `bit` of the line-status register to be set.
fn wait_for(bit: u8) {
    for _ in 0..SPIN_LIMIT {
        // SAFETY: reading COM1's line-status register only clears error
        // bits, which nothing here uses.
        if unsafe { port::read_u8(COM1 + LINE_STATUS) } & bit != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

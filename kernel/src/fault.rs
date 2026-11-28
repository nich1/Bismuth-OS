//! What the kernel reports when the processor raises an exception: the
//! frame the entry stubs in `vectors` leave on the stack, which vectors get
//! an error code from the processor, and the line printed.
//!
//! This file has no hardware code, so `xtask` also compiles it on the
//! development machine (with `#[path]`) and `cargo test --package xtask`
//! runs the tests at the bottom.

use bismuth_descriptors::{Class, EXCEPTIONS};

/// The stack as the common entry stub hands it to the Rust handler, lowest
/// address first. The processor pushes `ss` down to `rip`, after aligning
/// the stack pointer to 16 bytes, then for some vectors an error code (SDM
/// vol. 3A, "Interrupt and Exception Handling", sections "64-Bit Mode
/// Stack Frame" and "Error Code"). The vector's own stub pushes a zero
/// where the processor pushes no error code, then the vector number, so
/// every vector leaves this same frame.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The vector, pushed by its stub.
    pub vector: u64,
    /// The processor's error code, or the stub's zero.
    pub error_code: u64,
    /// Where the interrupted instruction is (for a fault, the faulting one).
    pub rip: u64,
    /// The code segment it ran in.
    pub cs: u64,
    /// `RFLAGS` as they were.
    pub rflags: u64,
    /// The stack pointer as it was.
    pub rsp: u64,
    /// The stack segment as it was.
    pub ss: u64,
}

/// The vector of a page fault, whose report adds CR2.
pub const PAGE_FAULT: u64 = 14;

/// Bit `n` is set when the processor pushes an error code for vector `n`,
/// taken from the library's exception table, the one source of it. The
/// stubs in `vectors` are assembled from this mask; vectors from 32 up
/// never get one.
pub const ERROR_CODE_VECTORS: u32 = error_code_vectors();

const fn error_code_vectors() -> u32 {
    let mut mask = 0;
    let mut vector = 0;
    while vector < EXCEPTIONS.len() {
        if EXCEPTIONS[vector].error_code {
            mask |= 1 << vector;
        }
        vector += 1;
    }
    mask
}

// The table has one entry per vector, each at its own index, so bit `n` of
// the mask is vector `n`'s.
const _: () = {
    assert!(EXCEPTIONS.len() == 32);
    let mut vector = 0;
    while vector < EXCEPTIONS.len() {
        assert!(EXCEPTIONS[vector].vector as usize == vector);
        vector += 1;
    }
};

/// The name the report gives `vector`: the manual's mnemonic, `reserved`
/// for a reserved vector, the manual's name for the two exceptions it gives
/// no mnemonic (2, the non-maskable interrupt, and 9), and `interrupt` for
/// 32 to 255, which the manual leaves to software and devices.
pub fn name(vector: u64) -> &'static str {
    let exception = usize::try_from(vector)
        .ok()
        .and_then(|index| EXCEPTIONS.get(index));
    match exception {
        Some(exception) => match (exception.mnemonic, exception.class) {
            (Some(mnemonic), _) => mnemonic,
            (None, Class::Reserved) => "reserved",
            (None, _) => exception.name,
        },
        None => "interrupt",
    }
}

/// Writes the report of `frame` through `out`, in pieces, ending with a
/// newline: `BISMUTH KERNEL FAULT vector=<n> <name> error=0x<code>
/// rip=0x<address> rsp=0x<address>`, and for a page fault ` cr2=0x<cr2>`.
/// Numbers are hexadecimal without leading zeros, the vector decimal.
pub fn write_report(frame: &Frame, cr2: u64, out: &mut dyn FnMut(&str)) {
    let mut digits = Digits::new();
    out("BISMUTH KERNEL FAULT vector=");
    out(digits.decimal(frame.vector));
    out(" ");
    out(name(frame.vector));
    for (label, value) in [
        (" error=0x", frame.error_code),
        (" rip=0x", frame.rip),
        (" rsp=0x", frame.rsp),
    ] {
        out(label);
        out(digits.hex(value));
    }
    if frame.vector == PAGE_FAULT {
        out(" cr2=0x");
        out(digits.hex(cr2));
    }
    out("\n");
}

/// Room for the digits of one number, written from the end.
struct Digits([u8; 20]);

impl Digits {
    fn new() -> Digits {
        Digits([0; 20])
    }

    fn hex(&mut self, value: u64) -> &str {
        self.write(value, 16)
    }

    fn decimal(&mut self, value: u64) -> &str {
        self.write(value, 10)
    }

    /// `value` in `base` (10 or 16), at least one digit. A `u64` has at
    /// most 20 decimal digits, so the buffer never runs out.
    fn write(&mut self, mut value: u64, base: u64) -> &str {
        let mut start = self.0.len();
        while let Some(slot) = start.checked_sub(1).and_then(|at| self.0.get_mut(at)) {
            let digit = (value % base) as u8;
            *slot = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            };
            start -= 1;
            value /= base;
            if value == 0 {
                break;
            }
        }
        self.0
            .get(start..)
            .and_then(|digits| core::str::from_utf8(digits).ok())
            .unwrap_or("?")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    fn report(frame: &Frame, cr2: u64) -> String {
        let mut text = String::new();
        write_report(frame, cr2, &mut |piece| text.push_str(piece));
        text
    }

    const FRAME: Frame = Frame {
        vector: 13,
        error_code: 0x7fa,
        rip: 0xFFFF_FFFF_8000_1234,
        cs: 0x8,
        rflags: 0x2,
        rsp: 0xFFFF_FFFF_7FFF_FF00,
        ss: 0x10,
    };

    #[test]
    fn the_frame_is_what_the_stubs_and_the_processor_push() {
        // The stub's two pushes, then the processor's five, eight bytes
        // each, lowest address first.
        let order = [
            offset_of!(Frame, vector),
            offset_of!(Frame, error_code),
            offset_of!(Frame, rip),
            offset_of!(Frame, cs),
            offset_of!(Frame, rflags),
            offset_of!(Frame, rsp),
            offset_of!(Frame, ss),
        ];
        assert_eq!(order, [0, 8, 16, 24, 32, 40, 48]);
        assert_eq!(size_of::<Frame>(), 56);
    }

    #[test]
    fn the_stubs_push_a_zero_exactly_where_the_table_has_no_error_code() {
        for exception in EXCEPTIONS {
            let bit = ERROR_CODE_VECTORS >> exception.vector & 1 == 1;
            assert_eq!(bit, exception.error_code, "vector {}", exception.vector);
        }
        // The vectors the manual gives an error code: #DF, #TS, #NP, #SS,
        // #GP, #PF, #AC, #CP, and AMD's #VC and #SX.
        let listed = [8, 10, 11, 12, 13, 14, 17, 21, 29, 30];
        assert_eq!(ERROR_CODE_VECTORS, listed.iter().map(|v| 1u32 << v).sum());
    }

    #[test]
    fn a_fault_is_reported_on_one_line() {
        assert_eq!(
            report(&FRAME, 0x1234),
            "BISMUTH KERNEL FAULT vector=13 #GP error=0x7fa rip=0xffffffff80001234 \
             rsp=0xffffffff7fffff00\n"
        );
    }

    #[test]
    fn a_page_fault_adds_the_address_touched() {
        let frame = Frame {
            vector: 14,
            error_code: 0,
            ..FRAME
        };
        assert_eq!(
            report(&frame, 0xFFFF_FFFF_7FFE_F000),
            "BISMUTH KERNEL FAULT vector=14 #PF error=0x0 rip=0xffffffff80001234 \
             rsp=0xffffffff7fffff00 cr2=0xffffffff7ffef000\n"
        );
    }

    #[test]
    fn every_vector_has_a_name() {
        assert_eq!(name(0), "#DE");
        assert_eq!(name(2), "NMI Interrupt");
        assert_eq!(name(8), "#DF");
        assert_eq!(name(9), "Coprocessor Segment Overrun");
        assert_eq!(name(15), "reserved");
        assert_eq!(name(31), "reserved");
        assert_eq!(name(32), "interrupt");
        assert_eq!(name(255), "interrupt");
        // A value no stub pushes still gets a name, not a panic.
        assert_eq!(name(u64::MAX), "interrupt");
    }

    #[test]
    fn numbers_are_written_whole() {
        let mut digits = Digits::new();
        assert_eq!(digits.hex(0), "0");
        assert_eq!(digits.hex(u64::MAX), "ffffffffffffffff");
        assert_eq!(digits.decimal(0), "0");
        assert_eq!(digits.decimal(255), "255");
        assert_eq!(digits.decimal(u64::MAX), "18446744073709551615");
    }
}

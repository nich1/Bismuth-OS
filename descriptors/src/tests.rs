//! Tests for the descriptor encodings. Every expected entry is a literal
//! worked out by hand from the layouts in the Intel SDM volume 3A, with the
//! working in a comment beside it, so a test failing means the library and
//! the manual disagree, not that two copies of the same code do. Bit `n` of
//! a 64-bit entry is bit `n % 8` of byte `n / 8`; the hex digits below are
//! written with byte 7 first.

// Tests index and add freely; the crate's lints are for the library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

const KERNEL: Privilege = Privilege::KERNEL;
const USER: Privilege = Privilege::USER;

/// Every privilege level.
fn rings() -> [Privilege; 4] {
    [0, 1, 2, 3].map(|level| Privilege::new(level).unwrap())
}

fn selector(index: u16, privilege: Privilege) -> Selector {
    Selector::new(index, privilege).unwrap()
}

fn stack(index: u8) -> InterruptStack {
    InterruptStack::new(index).unwrap()
}

/// A canonical high-half address whose every byte differs, so a byte in the
/// wrong place shows: bit 47 is the top bit of 0x8765, so bits 63:48 are all
/// ones.
const SPREAD: u64 = 0xFFFF_8765_4321_0ABC;

/// Addresses at the edges of the two canonical halves of a 48-bit address
/// space, and the first non-canonical ones beside them.
const CANONICAL_EDGES: [u64; 5] = [
    0,
    0x0000_7FFF_FFFF_FFFF,
    0xFFFF_8000_0000_0000,
    0xFFFF_FFFF_FFFF_FFFF,
    SPREAD,
];
const NON_CANONICAL: [u64; 4] = [
    0x0000_8000_0000_0000,
    0xFFFF_7FFF_FFFF_FFFF,
    0x8000_0000_0000_0000,
    0x0001_0000_0000_0000,
];

// --- Privilege levels and selectors ---

#[test]
fn refuses_a_privilege_level_above_3() {
    assert_eq!(Privilege::new(3).map(Privilege::level), Ok(3));
    for level in [4, 5, 0x80, u8::MAX] {
        assert_eq!(
            Privilege::new(level),
            Err(Error::PrivilegeTooHigh),
            "{level}"
        );
    }
}

#[test]
fn selectors_match_hand_worked_values() {
    // SDM vol. 3A, "Segment Selectors": bits 1:0 RPL, bit 2 TI (0 = GDT),
    // bits 15:3 index.
    // Index 1, RPL 0: 1 << 3 = 0x08.
    assert_eq!(selector(1, KERNEL).value(), 0x0008);
    // Index 4, RPL 3: 4 << 3 | 3 = 0x20 | 3 = 0x23.
    assert_eq!(selector(4, USER).value(), 0x0023);
    // Index 8191 (all 13 bits), RPL 3: 0x1FFF << 3 = 0xFFF8, | 3 = 0xFFFB.
    assert_eq!(selector(8191, USER).value(), 0xFFFB);
    // Index 0, RPL 0: the null selector.
    assert_eq!(selector(0, KERNEL).value(), 0x0000);
    // Index 2, RPL 2: 0x10 | 2 = 0x12.
    assert_eq!(selector(2, Privilege::new(2).unwrap()).value(), 0x0012);
}

#[test]
fn selectors_round_trip_at_each_field_extreme() {
    for index in [0, 1, 4095, 8190, 8191] {
        for ring in rings() {
            let built = selector(index, ring);
            assert_eq!(built.index(), index);
            assert_eq!(built.privilege(), ring);
            assert_eq!(Selector::from_value(built.value()), Some(built));
        }
    }
}

#[test]
fn refuses_a_selector_index_beyond_13_bits() {
    for index in [8192, 0x8000, u16::MAX] {
        assert_eq!(
            Selector::new(index, KERNEL),
            Err(Error::SelectorIndexTooLarge),
            "{index}"
        );
    }
}

#[test]
fn a_selector_into_the_local_table_is_not_read() {
    // TI (bit 2) set names the LDT, which Bismuth does not have.
    for value in [0x0004, 0x000F, 0xFFFF] {
        assert_eq!(Selector::from_value(value), None, "{value:#x}");
    }
}

// --- Segment table entries ---

#[test]
fn segment_entries_match_hand_worked_values() {
    // SDM vol. 3A, "Segment Descriptors": limit 15:0 in bits 15:0, base 23:0
    // in bits 39:16, type in 43:40 (A, R/W, C/E, code), S in 44, DPL in
    // 46:45, P in 47, limit 19:16 in 51:48, AVL 52, L 53, D/B 54, G 55,
    // base 31:24 in 63:56. Every entry here: base 0, limit 0xFFFFF, G = 1,
    // and A already set.
    assert_eq!(Segment::Null.encode(), 0);
    // Kernel code: type 0b1011 (code, readable, accessed) = 0xB, S = 1,
    // DPL 0, P = 1, so byte 5 = 0x80 | 0x10 | 0xB = 0x9B. Byte 6 = G | L
    // (0x8 | 0x2 = 0xA) in the high nibble, limit 19:16 = 0xF below: 0xAF.
    assert_eq!(Segment::KernelCode.encode(), 0x00AF_9B00_0000_FFFF);
    // Kernel data: type 0b0011 (data, writable, accessed) = 0x3: byte 5 =
    // 0x93. Byte 6 = G | D/B (0x8 | 0x4 = 0xC), limit 0xF: 0xCF.
    assert_eq!(Segment::KernelData.encode(), 0x00CF_9300_0000_FFFF);
    // User data: as kernel data with DPL 3 (0x60): byte 5 = 0xF3.
    assert_eq!(Segment::UserData.encode(), 0x00CF_F300_0000_FFFF);
    // User code: as kernel code with DPL 3: byte 5 = 0xFB.
    assert_eq!(Segment::UserCode.encode(), 0x00AF_FB00_0000_FFFF);
}

#[test]
fn segments_sit_where_syscall_and_sysret_expect_them() {
    // SDM vol. 3A, "Fast System Calls in 64-Bit Mode": SYSCALL loads CS
    // from STAR[47:32] and SS from that plus 8; SYSRET to 64-bit code loads
    // SS from STAR[63:48] plus 8 and CS from STAR[63:48] plus 16, each with
    // RPL 3. With STAR[47:32] = 0x08 and STAR[63:48] = 0x10:
    assert_eq!(Segment::Null.selector().value(), 0x00);
    // SYSCALL: CS = 0x08, SS = 0x08 + 8 = 0x10.
    assert_eq!(Segment::KernelCode.selector().value(), 0x08);
    assert_eq!(Segment::KernelData.selector().value(), 0x10);
    // SYSRET: SS = (0x10 + 8) | 3 = 0x1B, CS = (0x10 + 16) | 3 = 0x23.
    assert_eq!(Segment::UserData.selector().value(), 0x1B);
    assert_eq!(Segment::UserCode.selector().value(), 0x23);
    // The TSS descriptor takes indices 5 and 6: selector 5 << 3 = 0x28.
    assert_eq!(TSS_SELECTOR.value(), 0x28);
    assert_eq!(GDT_ENTRIES, 7);
}

#[test]
fn segment_entries_decode_to_what_they_are() {
    for (index, segment) in [
        Segment::Null,
        Segment::KernelCode,
        Segment::KernelData,
        Segment::UserData,
        Segment::UserCode,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(usize::from(segment.index()), index);
        assert_eq!(Segment::decode(segment.encode()), Some(segment));
    }
}

#[test]
fn other_segment_entries_are_not_recognised() {
    for entry in [
        // Kernel code with A clear (byte 5 = 0x9A).
        0x00AF_9A00_0000_FFFF,
        // 32-bit kernel code: D (0x4) instead of L in byte 6.
        0x00CF_9B00_0000_FFFF,
        // Kernel code, not present (byte 5 = 0x1B).
        0x00AF_1B00_0000_FFFF,
        // Kernel code with DPL 1 (byte 5 = 0xBB).
        0x00AF_BB00_0000_FFFF,
        // Kernel data with a base of 0x1000.
        0x00CF_9300_1000_FFFF,
        // A system descriptor: an available 64-bit TSS's low half.
        0x0000_8900_0000_0067,
        // Every bit set.
        u64::MAX,
    ] {
        assert_eq!(Segment::decode(entry), None, "{entry:#x}");
    }
}

// --- The TSS descriptor ---

#[test]
fn tss_descriptor_matches_hand_worked_values() {
    // SDM vol. 3A, "TSS Descriptor in 64-bit mode": the low half is laid out
    // like a segment descriptor; the high half holds base 63:32 in bits
    // 31:0, and its bits 44:40 must be zero.
    // Base SPREAD = 0xFFFF_8765_4321_0ABC, limit 103 = 0x67.
    // Low: limit 15:0 = 0x0067; base 23:0 = 0x21_0ABC in bits 39:16;
    // byte 5 = P (0x80) | DPL 0 | S = 0 | type 0b1001 (available 64-bit TSS)
    // = 0x89; byte 6 = G 0, limit 19:16 = 0: 0x00; byte 7 = base 31:24 =
    // 0x43.
    let descriptor = TssDescriptor::new(SPREAD).unwrap();
    assert_eq!(
        descriptor.encode(),
        [0x4300_8921_0ABC_0067, 0x0000_0000_FFFF_8765]
    );
    assert!(!descriptor.is_busy());
    // Base 0: only the limit, the type and P.
    assert_eq!(
        TssDescriptor::new(0).unwrap().encode(),
        [0x0000_8900_0000_0067, 0]
    );
    // After `ltr` the processor sets the busy bit: type 0b1011, byte 5 =
    // 0x8B.
    let busy = TssDescriptor::decode([0x4300_8B21_0ABC_0067, 0x0000_0000_FFFF_8765]).unwrap();
    assert_eq!(busy.base(), SPREAD);
    assert!(busy.is_busy());
}

#[test]
fn tss_descriptors_round_trip_at_each_canonical_edge() {
    for base in CANONICAL_EDGES {
        let descriptor = TssDescriptor::new(base).unwrap();
        assert_eq!(descriptor.base(), base);
        assert_eq!(
            TssDescriptor::decode(descriptor.encode()),
            Some(descriptor),
            "{base:#x}"
        );
    }
    // A busy one too.
    let busy = TssDescriptor::decode([0x0000_8B00_0000_0067, 0xFFFF_FFFF]).unwrap();
    assert_eq!(busy.base(), 0xFFFF_FFFF_0000_0000);
    assert_eq!(busy.encode(), [0x0000_8B00_0000_0067, 0xFFFF_FFFF]);
}

#[test]
fn tss_descriptor_refuses_a_non_canonical_base() {
    for base in NON_CANONICAL {
        assert_eq!(
            TssDescriptor::new(base),
            Err(Error::NonCanonical),
            "{base:#x}"
        );
    }
    // Read back: base 63:32 = 0x0000_8000, so the base is 0x0000_8000_0000_0000.
    assert_eq!(
        TssDescriptor::decode([0x0000_8900_0000_0067, 0x0000_8000]),
        None
    );
}

#[test]
fn other_entries_are_not_read_as_a_tss_descriptor() {
    let good = 0x0000_8900_0000_0067;
    for (low, high) in [
        // Not present (byte 5 = 0x09).
        (0x0000_0900_0000_0067, 0),
        // Type 0b0010, a local descriptor table.
        (0x0000_8200_0000_0067, 0),
        // DPL 3 (byte 5 = 0xE9).
        (0x0000_E900_0000_0067, 0),
        // S set: a code or data segment.
        (0x0000_9900_0000_0067, 0),
        // A limit of 0x68.
        (0x0000_8900_0000_0068, 0),
        // G set (byte 6 = 0x80).
        (0x0080_8900_0000_0067, 0),
        // A type in the high half (bits 44:40 = 0b00001).
        (good, 0x0000_0100_0000_0000),
        // A reserved bit at the top of the high half.
        (good, 0x8000_0000_0000_0000),
    ] {
        assert_eq!(
            TssDescriptor::decode([low, high]),
            None,
            "{low:#x} {high:#x}"
        );
    }
}

// --- The TSS ---

/// The little-endian bytes of `value` at `at` in `bytes`.
fn bytes_at(bytes: &[u8; TSS_SIZE], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[test]
fn tss_fields_sit_at_the_manuals_offsets() {
    // SDM vol. 3A, "Task Management in 64-bit Mode", the 64-bit TSS figure:
    // reserved 0..4, RSP0 4..12, RSP1 12..20, RSP2 20..28, reserved 28..36,
    // IST1 36..44 up to IST7 84..92, reserved 92..102, I/O map base 102..104.
    let mut tss = Tss::new();
    // A new TSS: all zero but the I/O map base, 104 = 0x68 (at or past the
    // limit, so there is no I/O permission bitmap).
    let mut expected = [0u8; TSS_SIZE];
    expected[102] = 0x68;
    assert_eq!(tss.as_bytes(), &expected);
    assert_eq!(tss.io_map_base(), 104);

    let values = [
        (0x0000_7FFF_FFFF_FFF0, 4),
        (0xFFFF_8000_0000_1000, 12),
        (0xFFFF_FFFF_8000_2000, 20),
    ];
    for (ring, (value, at)) in values.into_iter().enumerate() {
        let ring = Privilege::new(ring as u8).unwrap();
        tss.set_privilege_stack(ring, value).unwrap();
        assert_eq!(bytes_at(tss.as_bytes(), at), value, "ring {}", ring.level());
    }
    for index in 1..=7u8 {
        // IST n at 36 + 8 * (n - 1): 36, 44, 52, 60, 68, 76, 84.
        let at = [36, 44, 52, 60, 68, 76, 84][usize::from(index - 1)];
        let value = SPREAD + u64::from(index);
        tss.set_interrupt_stack(stack(index), value).unwrap();
        assert_eq!(bytes_at(tss.as_bytes(), at), value, "IST{index}");
    }
    tss.set_io_map_base(0xBEEF);
    assert_eq!(tss.as_bytes()[102..104], [0xEF, 0xBE]);
    // The reserved bytes are still zero.
    for at in (0..4).chain(28..36).chain(92..102) {
        assert_eq!(tss.as_bytes()[at], 0, "byte {at}");
    }
    // The TSS read from these bytes says the same.
    let copy = Tss::from_bytes(*tss.as_bytes());
    assert_eq!(copy, tss);
    assert_eq!(copy.privilege_stack(KERNEL), Ok(0x0000_7FFF_FFFF_FFF0));
    assert_eq!(copy.interrupt_stack(stack(7)), SPREAD + 7);
    assert_eq!(copy.io_map_base(), 0xBEEF);
}

#[test]
fn tss_fields_round_trip_at_each_extreme() {
    for value in CANONICAL_EDGES {
        for ring in &rings()[..3] {
            let mut tss = Tss::new();
            tss.set_privilege_stack(*ring, value).unwrap();
            assert_eq!(tss.privilege_stack(*ring), Ok(value));
        }
        for index in [1, 7] {
            let mut tss = Tss::new();
            tss.set_interrupt_stack(stack(index), value).unwrap();
            assert_eq!(tss.interrupt_stack(stack(index)), value);
        }
    }
    for base in [0, u16::MAX] {
        let mut tss = Tss::default();
        tss.set_io_map_base(base);
        assert_eq!(tss.io_map_base(), base);
    }
}

#[test]
fn tss_refuses_a_stack_for_ring_3() {
    let mut tss = Tss::new();
    assert_eq!(
        tss.set_privilege_stack(USER, 0x1000),
        Err(Error::NoPrivilegeStack)
    );
    assert_eq!(tss.privilege_stack(USER), Err(Error::NoPrivilegeStack));
    assert_eq!(tss, Tss::new());
}

#[test]
fn tss_refuses_a_non_canonical_stack() {
    let mut tss = Tss::new();
    for top in NON_CANONICAL {
        assert_eq!(
            tss.set_privilege_stack(KERNEL, top),
            Err(Error::NonCanonical)
        );
        assert_eq!(
            tss.set_interrupt_stack(stack(1), top),
            Err(Error::NonCanonical)
        );
    }
    assert_eq!(tss, Tss::new());
}

#[test]
fn refuses_an_interrupt_stack_index_outside_1_to_7() {
    assert_eq!(InterruptStack::new(1).map(InterruptStack::index), Ok(1));
    assert_eq!(InterruptStack::new(7).map(InterruptStack::index), Ok(7));
    for index in [0, 8, 9, u8::MAX] {
        assert_eq!(
            InterruptStack::new(index),
            Err(Error::InterruptStackIndex),
            "{index}"
        );
    }
}

// --- Interrupt table gates ---

#[test]
fn gates_match_hand_worked_values() {
    // SDM vol. 3A, "64-Bit Mode IDT", the 64-bit IDT gate figure: offset
    // 15:0 in bits 15:0, selector in 31:16, IST in 34:32, zero in 39:35,
    // type in 43:40, zero in 44, DPL in 46:45, P in 47, offset 31:16 in
    // 63:48; the high half holds offset 63:32 in 31:0, the rest reserved.
    // Handler SPREAD = 0xFFFF_8765_4321_0ABC, selector 0x0008, IST 2,
    // interrupt gate (type 0xE), DPL 0: byte 4 = 0x02, byte 5 = P | 0xE =
    // 0x8E, offset 31:16 = 0x4321 on top, offset 15:0 = 0x0ABC below the
    // selector.
    let gate = Gate::new(
        SPREAD,
        selector(1, KERNEL),
        Some(stack(2)),
        GateKind::Interrupt,
        KERNEL,
    )
    .unwrap();
    assert_eq!(
        gate.encode(),
        [0x4321_8E02_0008_0ABC, 0x0000_0000_FFFF_8765]
    );
    // Handler 0xFFFF_FFFF_8000_1234, selector 0x08, no IST, trap gate (type
    // 0xF), DPL 3 so `int3` works from user mode: byte 4 = 0x00, byte 5 =
    // 0x80 | 0x60 | 0xF = 0xEF, offset 31:16 = 0x8000, offset 15:0 = 0x1234,
    // offset 63:32 = 0xFFFF_FFFF.
    let gate = Gate::new(
        0xFFFF_FFFF_8000_1234,
        selector(1, KERNEL),
        None,
        GateKind::Trap,
        USER,
    )
    .unwrap();
    assert_eq!(
        gate.encode(),
        [0x8000_EF00_0008_1234, 0x0000_0000_FFFF_FFFF]
    );
    // The not-present entry: P = 0 and nothing else.
    assert_eq!(Gate::MISSING, [0, 0]);
}

#[test]
fn gates_round_trip_at_each_field_extreme() {
    for handler in CANONICAL_EDGES {
        for code in [
            selector(0, KERNEL),
            selector(1, KERNEL),
            selector(8191, USER),
        ] {
            for ist in [None, Some(stack(1)), Some(stack(7))] {
                for kind in [GateKind::Interrupt, GateKind::Trap] {
                    for ring in rings() {
                        let gate = Gate::new(handler, code, ist, kind, ring).unwrap();
                        assert_eq!(
                            (gate.handler(), gate.selector(), gate.stack()),
                            (handler, code, ist)
                        );
                        assert_eq!((gate.kind(), gate.privilege()), (kind, ring));
                        assert_eq!(Gate::decode(gate.encode()), Some(gate), "{gate:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn gate_refuses_a_non_canonical_handler() {
    for handler in NON_CANONICAL {
        assert_eq!(
            Gate::new(
                handler,
                selector(1, KERNEL),
                None,
                GateKind::Interrupt,
                KERNEL
            ),
            Err(Error::NonCanonical),
            "{handler:#x}"
        );
    }
}

#[test]
fn other_entries_are_not_read_as_a_gate() {
    let low = 0x4321_8E02_0008_0ABC;
    let high = 0x0000_0000_FFFF_8765;
    assert!(Gate::decode([low, high]).is_some());
    for (low, high) in [
        // The not-present entry.
        (Gate::MISSING[0], Gate::MISSING[1]),
        // P clear (byte 5 = 0x0E).
        (0x4321_0E02_0008_0ABC, high),
        // Type 0xC, a call gate.
        (0x4321_8C02_0008_0ABC, high),
        // Type 0x6, a 16-bit interrupt gate.
        (0x4321_8602_0008_0ABC, high),
        // Bit 35 set (byte 4 = 0x0A), reserved.
        (0x4321_8E0A_0008_0ABC, high),
        // Bit 44 set (byte 5 = 0x9E), must be zero.
        (0x4321_9E02_0008_0ABC, high),
        // A selector with TI set: 0x000C.
        (0x4321_8E02_000C_0ABC, high),
        // A reserved bit in the high half.
        (low, 0x0000_0001_FFFF_8765),
        // Offset 63:32 = 0x0000_8765, making the handler non-canonical.
        (low, 0x0000_0000_0000_8765),
    ] {
        assert_eq!(Gate::decode([low, high]), None, "{low:#x} {high:#x}");
    }
}

// --- Pseudo-descriptors ---

#[test]
fn pseudo_descriptors_match_hand_worked_values() {
    // SDM vol. 3A, "Segment Descriptor Tables", the pseudo-descriptor
    // figure: limit in bytes 0..2, base in bytes 2..10, both little-endian.
    // The kernel's GDT, 7 entries of 8 bytes at 0xFFFF_FFFF_8000_1000:
    // limit 7 * 8 - 1 = 55 = 0x37.
    let gdt = PseudoDescriptor::gdt(0xFFFF_FFFF_8000_1000, 7).unwrap();
    assert_eq!(gdt.limit(), 55);
    assert_eq!(
        gdt.to_bytes(),
        [0x37, 0x00, 0x00, 0x10, 0x00, 0x80, 0xFF, 0xFF, 0xFF, 0xFF]
    );
    // A full IDT, 256 entries of 16 bytes: limit 4096 - 1 = 0x0FFF.
    let idt = PseudoDescriptor::idt(SPREAD, 256).unwrap();
    assert_eq!(
        idt.to_bytes(),
        [0xFF, 0x0F, 0xBC, 0x0A, 0x21, 0x43, 0x65, 0x87, 0xFF, 0xFF]
    );
    // The largest GDT, 8192 entries: limit 65536 - 1 = 0xFFFF.
    assert_eq!(PseudoDescriptor::gdt(0, 8192).unwrap().limit(), 0xFFFF);
    // The smallest tables: limit 7 and 15.
    assert_eq!(PseudoDescriptor::gdt(0, 1).unwrap().limit(), 7);
    assert_eq!(PseudoDescriptor::idt(0, 1).unwrap().limit(), 15);
}

#[test]
fn pseudo_descriptors_round_trip_at_each_extreme() {
    for base in CANONICAL_EDGES {
        for table in [
            PseudoDescriptor::gdt(base, 1).unwrap(),
            PseudoDescriptor::gdt(base, 8192).unwrap(),
            PseudoDescriptor::idt(base, 1).unwrap(),
            PseudoDescriptor::idt(base, 256).unwrap(),
        ] {
            assert_eq!(table.base(), base);
            assert_eq!(PseudoDescriptor::from_bytes(table.to_bytes()), Some(table));
        }
    }
    // Any limit is read, including 0 and 0xFFFF.
    let read = PseudoDescriptor::from_bytes([0; 10]).unwrap();
    assert_eq!((read.limit(), read.base()), (0, 0));
    let read = PseudoDescriptor::from_bytes([0xFF; 10]).unwrap();
    assert_eq!((read.limit(), read.base()), (0xFFFF, u64::MAX));
}

#[test]
fn refuses_a_table_with_no_entries() {
    assert_eq!(PseudoDescriptor::gdt(0, 0), Err(Error::EmptyTable));
    assert_eq!(PseudoDescriptor::idt(0, 0), Err(Error::EmptyTable));
}

#[test]
fn refuses_a_table_too_large() {
    // A GDT of 8193 entries would need a limit of 65543. 2^61 entries of 8
    // bytes are 2^64 bytes, which wraps to 0 if the size is not checked.
    for entries in [8193, 1 << 20, 1 << 61, usize::MAX] {
        assert_eq!(PseudoDescriptor::gdt(0, entries), Err(Error::TableTooLarge));
    }
    // An IDT has one entry per vector, 256 at most.
    for entries in [257, 4096, usize::MAX] {
        assert_eq!(PseudoDescriptor::idt(0, entries), Err(Error::TableTooLarge));
    }
}

#[test]
fn pseudo_descriptor_refuses_a_non_canonical_base() {
    for base in NON_CANONICAL {
        assert_eq!(PseudoDescriptor::gdt(base, 7), Err(Error::NonCanonical));
        assert_eq!(PseudoDescriptor::idt(base, 256), Err(Error::NonCanonical));
    }
    // Bytes 2..10 = 0x0000_8000_0000_0000.
    assert_eq!(
        PseudoDescriptor::from_bytes([0x37, 0, 0, 0, 0, 0, 0, 0x80, 0, 0]),
        None
    );
}

// --- The exception table ---

#[test]
fn exceptions_match_the_manuals_table() {
    use Class::*;
    // SDM vol. 3A, "Exception and Interrupt Vectors", the protected-mode
    // exceptions and interrupts table, entry by entry; vectors 28 to 30 from
    // the AMD64 manual volume 2, "Exceptions and Interrupts". (vector,
    // mnemonic, error code, class)
    let expected: [(u8, Option<&str>, bool, Class); 32] = [
        (0, Some("#DE"), false, Fault),
        (1, Some("#DB"), false, FaultOrTrap),
        (2, None, false, Interrupt),
        (3, Some("#BP"), false, Trap),
        (4, Some("#OF"), false, Trap),
        (5, Some("#BR"), false, Fault),
        (6, Some("#UD"), false, Fault),
        (7, Some("#NM"), false, Fault),
        (8, Some("#DF"), true, Abort),
        (9, None, false, Fault),
        (10, Some("#TS"), true, Fault),
        (11, Some("#NP"), true, Fault),
        (12, Some("#SS"), true, Fault),
        (13, Some("#GP"), true, Fault),
        (14, Some("#PF"), true, Fault),
        (15, None, false, Reserved),
        (16, Some("#MF"), false, Fault),
        (17, Some("#AC"), true, Fault),
        (18, Some("#MC"), false, Abort),
        (19, Some("#XM"), false, Fault),
        (20, Some("#VE"), false, Fault),
        (21, Some("#CP"), true, Fault),
        (22, None, false, Reserved),
        (23, None, false, Reserved),
        (24, None, false, Reserved),
        (25, None, false, Reserved),
        (26, None, false, Reserved),
        (27, None, false, Reserved),
        (28, Some("#HV"), false, Interrupt),
        (29, Some("#VC"), true, Fault),
        (30, Some("#SX"), true, Interrupt),
        (31, None, false, Reserved),
    ];
    for (exception, (vector, mnemonic, error_code, class)) in EXCEPTIONS.iter().zip(expected) {
        assert_eq!(exception.vector, vector);
        assert_eq!(
            (exception.mnemonic, exception.error_code, exception.class),
            (mnemonic, error_code, class),
            "vector {vector}"
        );
        assert_eq!(
            exception.class == Reserved,
            exception.name == "Reserved",
            "vector {vector}"
        );
    }
    // The vectors that push an error code, listed on their own.
    let pushing: Vec<u8> = EXCEPTIONS
        .iter()
        .filter(|exception| exception.error_code)
        .map(|exception| exception.vector)
        .collect();
    assert_eq!(pushing, [8, 10, 11, 12, 13, 14, 17, 21, 29, 30]);
    assert_eq!(EXCEPTIONS[14].name, "Page Fault");
    assert_eq!(EXCEPTIONS[2].name, "NMI Interrupt");
}

// --- Random values ---

/// A tiny xorshift generator (Marsaglia, 2003) so the randomised test is the
/// same on every run without a dependency.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }

    /// A random canonical address: bits 47:0 random, bit 47 copied up.
    fn canonical(&mut self) -> u64 {
        let low = self.next() & 0x0000_FFFF_FFFF_FFFF;
        if low & (1 << 47) == 0 {
            low
        } else {
            low | 0xFFFF_0000_0000_0000
        }
    }

    fn ring(&mut self) -> Privilege {
        Privilege::new(self.below(4) as u8).unwrap()
    }

    fn selector(&mut self) -> Selector {
        let index = self.below(8192) as u16;
        let ring = self.ring();
        selector(index, ring)
    }
}

#[test]
fn random_values_round_trip_and_random_bytes_do_not_panic() {
    let mut random = Random(0x0DE5_C819_7AB1_E500);
    let mut recognised = [0u32; 4];
    for _ in 0..20_000 {
        // Random valid values come back as they went in.
        let code = random.selector();
        assert_eq!(Selector::from_value(code.value()), Some(code));
        let base = random.canonical();
        let descriptor = TssDescriptor::new(base).unwrap();
        assert_eq!(TssDescriptor::decode(descriptor.encode()), Some(descriptor));
        let ist = match random.below(8) as u8 {
            0 => None,
            index => Some(stack(index)),
        };
        let kind = [GateKind::Interrupt, GateKind::Trap][random.below(2) as usize];
        let gate = Gate::new(random.canonical(), code, ist, kind, random.ring()).unwrap();
        assert_eq!(Gate::decode(gate.encode()), Some(gate));
        let entries = 1 + random.below(8192) as usize;
        let table = PseudoDescriptor::gdt(base, entries).unwrap();
        assert_eq!(table.limit() as usize, entries * 8 - 1);
        assert_eq!(PseudoDescriptor::from_bytes(table.to_bytes()), Some(table));
        let mut tss = Tss::new();
        let (ring, top) = (random.below(3) as u8, random.canonical());
        let ring = Privilege::new(ring).unwrap();
        tss.set_privilege_stack(ring, top).unwrap();
        let (index, ist_top) = (1 + random.below(7) as u8, random.canonical());
        tss.set_interrupt_stack(stack(index), ist_top).unwrap();
        assert_eq!(tss.privilege_stack(ring), Ok(top));
        assert_eq!(tss.interrupt_stack(stack(index)), ist_top);

        // Random bytes give no panic, and whatever a decoder accepts encodes
        // back to exactly the same bits.
        let (low, high) = (random.next(), random.next());
        let value = low as u16;
        if let Some(read) = Selector::from_value(value) {
            assert_eq!(read.value(), value);
            recognised[0] += 1;
        }
        if let Some(segment) = Segment::decode(low) {
            assert_eq!(segment.encode(), low);
        }
        // Also nearly-right entries: random bits around fixed type bits, with
        // the top half of a canonical address, so the decoders accept some.
        let top = random.canonical() >> 32;
        // Clears the limit and bytes 5 and 6; sets limit 0x67, P and type 9.
        let tss_low = (low & !0x00FF_FF00_0000_FFFF) | 0x0000_8900_0000_0067;
        for entry in [[low, high], [tss_low, top]] {
            if let Some(read) = TssDescriptor::decode(entry) {
                assert_eq!(read.encode(), entry);
                recognised[1] += 1;
            }
        }
        // Clears bits 39:35 and bits 44:41 and 47 of byte 5, keeping bit 40
        // (interrupt or trap) and the DPL; sets P and type 0xE.
        let gate_low = (low & !0x0000_9EF8_0000_0000) | 0x0000_8E00_0000_0000;
        for entry in [[low, high], [gate_low, top]] {
            if let Some(read) = Gate::decode(entry) {
                assert_eq!(read.encode(), entry);
                recognised[2] += 1;
            }
        }
        let mut bytes = [0u8; 10];
        bytes[..8].copy_from_slice(&low.to_le_bytes());
        bytes[8..].copy_from_slice(&high.to_le_bytes()[..2]);
        let mut near = bytes;
        near[2..].copy_from_slice(&random.canonical().to_le_bytes());
        for bytes in [bytes, near] {
            if let Some(read) = PseudoDescriptor::from_bytes(bytes) {
                assert_eq!(read.to_bytes(), bytes);
                recognised[3] += 1;
            }
        }
        let mut tss_bytes = [0u8; TSS_SIZE];
        for chunk in tss_bytes.chunks_mut(8) {
            let word = random.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        let read = Tss::from_bytes(tss_bytes);
        for ring in rings() {
            let _ = read.privilege_stack(ring);
        }
        for index in 1..=7 {
            let _ = read.interrupt_stack(stack(index));
        }
        assert_eq!(
            read.io_map_base(),
            u16::from_le_bytes([tss_bytes[102], tss_bytes[103]])
        );
    }
    // Each decoder accepted a good share, so the checks above ran.
    assert!(
        recognised.iter().all(|&count| count > 1000),
        "{recognised:?}"
    );
}

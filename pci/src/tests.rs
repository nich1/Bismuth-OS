//! Tests for the reader. Every structure is built byte by byte from the
//! field tables of the PCI Local Bus Specification, the PCI Express Base
//! Specification and the PCI Firmware Specification, with the offsets
//! written out here rather than taken from the library, so a failing test
//! means the reader and the specifications disagree.

// Building test data indexes and adds freely; the crate's lints are for the
// reader itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}
fn put16(bytes: &mut [u8], at: usize, value: u16) {
    put(bytes, at, &value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    put(bytes, at, &value.to_le_bytes());
}
fn put64(bytes: &mut [u8], at: usize, value: u64) {
    put(bytes, at, &value.to_le_bytes());
}

/// Sets the byte at `at` so that `bytes` sum to zero modulo 256.
fn fix_checksum(bytes: &mut [u8], at: usize) {
    bytes[at] = 0;
    let sum = bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    bytes[at] = sum.wrapping_neg();
}

/// One MCFG allocation (PCI Firmware Specification, "MCFG Table
/// Description"): base address at 0, segment group at 8, start bus at 10,
/// end bus at 11, four reserved bytes at 12.
fn allocation_bytes(base: u64, segment: u16, start: u8, end: u8) -> [u8; 16] {
    let mut e = [0u8; 16];
    put64(&mut e, 0, base);
    put16(&mut e, 8, segment);
    e[10] = start;
    e[11] = end;
    put32(&mut e, 12, 0xA5A5_A5A5);
    e
}

/// An MCFG: the ACPI table header (36 bytes: signature at 0, length at 4,
/// checksum at 9), eight reserved bytes, then the allocations.
fn mcfg(entries: &[[u8; 16]]) -> Vec<u8> {
    let mut t = vec![0u8; 44];
    put(&mut t, 0, b"MCFG");
    t[8] = 1; // revision
    put(&mut t, 10, b"OEMID1");
    put(&mut t, 36, &[0x5A; 8]); // reserved
    for e in entries {
        t.extend_from_slice(e);
    }
    let length = t.len() as u32;
    put32(&mut t, 4, length);
    fix_checksum(&mut t, 9);
    t
}

/// A general device header (PCI Local Bus Specification, "Configuration
/// Space Organization", header type 0) in 256 bytes: vendor at 0, device
/// at 2, command at 4, status at 6, revision at 8, programming interface
/// at 9, subclass at 0x0A, class at 0x0B, header type at 0x0E, the six
/// address registers from 0x10, subsystem vendor at 0x2C, subsystem at
/// 0x2E, capabilities pointer at 0x34, interrupt line at 0x3C and pin at
/// 0x3D.
fn general() -> Vec<u8> {
    let mut h = vec![0u8; 256];
    put16(&mut h, 0x00, 0x1B36);
    put16(&mut h, 0x02, 0x0010);
    put16(&mut h, 0x04, 0x0407); // I/O, memory, bus master, interrupt disable
    put16(&mut h, 0x06, 0x0010); // capabilities list
    h[0x08] = 2;
    h[0x09] = 0x02;
    h[0x0A] = 0x08;
    h[0x0B] = 0x01;
    h[0x0C] = 0x10; // cache line size, not read
    h[0x0E] = 0x80; // multi-function, type 0
    put32(&mut h, 0x10, 0xFEBF_0000); // 32-bit memory
    put32(&mut h, 0x14, 0x0000_C001); // I/O
    put32(&mut h, 0x18, 0x8000_000C); // 64-bit prefetchable, low half
    put32(&mut h, 0x1C, 0x0000_0001); // its high half
    put32(&mut h, 0x20, 0); // not implemented
    put32(&mut h, 0x24, 0x000F_0002); // memory, reserved type 01
    put16(&mut h, 0x2C, 0x1AF4);
    put16(&mut h, 0x2E, 0x1100);
    put32(&mut h, 0x30, 0xFEB0_0001); // expansion ROM, not read
    h[0x34] = 0x40;
    h[0x3C] = 11;
    h[0x3D] = 1;
    h
}

/// A PCI-to-PCI bridge header (header type 1): the two address registers at
/// 0x10 and 0x14, primary bus at 0x18, secondary at 0x19, subordinate at
/// 0x1A, capabilities pointer at 0x34.
fn bridge() -> Vec<u8> {
    let mut h = vec![0u8; 256];
    put16(&mut h, 0x00, 0x8086);
    put16(&mut h, 0x02, 0x244E);
    put16(&mut h, 0x04, 0x0006);
    put16(&mut h, 0x06, 0x0010);
    h[0x08] = 0xA1;
    h[0x0A] = 0x04;
    h[0x0B] = 0x06;
    h[0x0E] = 0x01;
    put32(&mut h, 0x10, 0xFE80_0000);
    put32(&mut h, 0x14, 0);
    h[0x18] = 0;
    h[0x19] = 1;
    h[0x1A] = 4;
    h[0x1B] = 0x20; // secondary latency timer, not read
    h[0x34] = 0x60;
    h[0x60] = 0x10; // PCI Express, root port, version 2
    put16(&mut h, 0x62, 0x0042);
    h
}

/// Writes each capability at its offset and links them in the order given
/// from the capabilities pointer at 0x34: id at +0, next pointer at +1.
fn link(h: &mut [u8], caps: &[(usize, Vec<u8>)]) {
    h[0x34] = caps.first().map_or(0, |c| c.0 as u8);
    for (i, (at, body)) in caps.iter().enumerate() {
        put(h, *at, body);
        h[*at + 1] = caps.get(i + 1).map_or(0, |c| c.0 as u8);
    }
}

/// MSI (PCI Local Bus Specification, "MSI Capability Structure"): message
/// control at +2, address at +4, then for the 32-bit form data at +8; for
/// the 64-bit form the upper address at +8 and data at +0x0C; mask and
/// pending bits after them when per-vector masking is capable.
fn msi(control: u16) -> Vec<u8> {
    let long = control & 0x80 != 0;
    let masked = control & 0x100 != 0;
    let size = match (long, masked) {
        (false, false) => 10,
        (true, false) => 14,
        (false, true) => 20,
        (true, true) => 24,
    };
    let mut c = vec![0xEEu8; size];
    c[0] = 0x05;
    put16(&mut c, 2, control);
    c
}

/// MSI-X (PCI Local Bus Specification, "MSI-X Capability and Table
/// Structure"): message control at +2, table offset and indicator at +4,
/// pending bits' offset and indicator at +8.
fn msix(control: u16, table: u32, pending: u32) -> Vec<u8> {
    let mut c = vec![0u8; 12];
    c[0] = 0x11;
    put16(&mut c, 2, control);
    put32(&mut c, 4, table);
    put32(&mut c, 8, pending);
    c
}

/// PCI Express (PCI Express Base Specification, "PCI Express Capability
/// Structure"): the capabilities register at +2, version in bits 3:0 and
/// device/port type in bits 7:4. The rest of the 60 bytes are left zero.
fn express(register: u16) -> Vec<u8> {
    let mut c = vec![0u8; 60];
    c[0] = 0x10;
    put16(&mut c, 2, register);
    c
}

/// A general header with one capability of each kind, not in address order.
fn with_capabilities() -> Vec<u8> {
    let mut h = general();
    link(
        &mut h,
        &[
            // 32-bit MSI: enabled, 4 vectors capable, 2 enabled.
            (0x40, msi(0x0015)),
            // Vendor-specific (id 0x09): length byte, then its own data.
            (0xF0, vec![0x09, 0, 6, 0xAB, 0xCD, 0xEF]),
            // 64-bit MSI with per-vector masking, 32 vectors capable.
            (0x50, msi(0x018A)),
            (0x80, express(0x0042)),
            // MSI-X: 64 entries, enabled and masked; the table in register
            // 0 at 0x2000, the pending bits in register 4 at 0x3000.
            (0x70, msix(0xC03F, 0x0000_2000, 0x0000_3004)),
        ],
    );
    h
}

/// Runs `work` on a thread of its own and returns what it returns, failing
/// the test if it has not ended within `seconds`: a reader that loops
/// forever then fails a test instead of hanging the run.
fn ends<T: Send + 'static>(seconds: u64, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(seconds))
        .expect("the work ended in time")
}

/// The capability list of `bytes`, read on a thread of its own, as
/// `(id, offset)` pairs.
fn walk(bytes: Vec<u8>) -> Result<Vec<(u8, usize)>, Error> {
    ends(10, move || {
        let header = Header::read(&bytes)?.expect("present");
        let list = header.capabilities()?;
        Ok(list.map(|c| (c.id, c.offset)).collect())
    })
}

fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let (outer, inner) = (outer.as_ptr_range(), inner.as_ptr_range());
    outer.start <= inner.start && inner.end <= outer.end
}

fn read(bytes: &[u8]) -> Header<'_> {
    Header::read(bytes).expect("accepted").expect("present")
}

// ---- MCFG and addresses ----

#[test]
fn reads_an_mcfg_with_two_allocations() {
    let bytes = mcfg(&[
        allocation_bytes(0xE000_0000, 0, 0, 0xFF),
        allocation_bytes(0x1_0000_0000, 1, 0x10, 0x1F),
    ]);
    let table = Mcfg::read(&bytes).expect("accepted");
    assert_eq!(table.table().bytes(), &bytes[..]);
    assert_eq!(
        table.allocations().collect::<Vec<_>>(),
        [
            Allocation {
                base: 0xE000_0000,
                segment: 0,
                start_bus: 0,
                end_bus: 0xFF
            },
            Allocation {
                base: 0x1_0000_0000,
                segment: 1,
                start_bus: 0x10,
                end_bus: 0x1F
            },
        ]
    );
}

#[test]
fn an_mcfg_with_no_allocations_lists_none() {
    let bytes = mcfg(&[]);
    assert_eq!(Mcfg::read(&bytes).map(|t| t.allocations().count()), Ok(0));
}

#[test]
fn rejects_an_mcfg_with_another_signature() {
    let mut bytes = mcfg(&[allocation_bytes(0xE000_0000, 0, 0, 0xFF)]);
    put(&mut bytes, 0, b"MCFX");
    fix_checksum(&mut bytes, 9);
    assert_eq!(Mcfg::read(&bytes).err(), Some(Error::BadSignature));
}

#[test]
fn rejects_an_mcfg_without_its_reserved_bytes() {
    let mut bytes = mcfg(&[]);
    bytes.truncate(43);
    put32(&mut bytes, 4, 43);
    fix_checksum(&mut bytes, 9);
    assert_eq!(Mcfg::read(&bytes).err(), Some(Error::Truncated));
}

#[test]
fn rejects_an_mcfg_that_is_not_whole_allocations() {
    for extra in [1, 8, 15] {
        let mut bytes = mcfg(&[allocation_bytes(0xE000_0000, 0, 0, 0xFF)]);
        bytes.extend_from_slice(&vec![0u8; extra]);
        let length = bytes.len() as u32;
        put32(&mut bytes, 4, length);
        fix_checksum(&mut bytes, 9);
        assert_eq!(
            Mcfg::read(&bytes).err(),
            Some(Error::PartialEntry),
            "{extra} bytes"
        );
    }
}

#[test]
fn an_mcfg_is_checked_like_any_acpi_table() {
    let mut bytes = mcfg(&[allocation_bytes(0xE000_0000, 0, 0, 0xFF)]);
    bytes[50] ^= 1;
    assert_eq!(
        Mcfg::read(&bytes).err(),
        Some(Error::Table(bismuth_acpi::Error::BadChecksum))
    );
    assert_eq!(
        Mcfg::read(&bytes[..30]).err(),
        Some(Error::Table(bismuth_acpi::Error::Truncated))
    );
}

const SEGMENT: Allocation = Allocation {
    base: 0x1_0000_0000,
    segment: 0,
    start_bus: 0x10,
    end_bus: 0x1F,
};

#[test]
fn computes_a_function_s_configuration_address() {
    // base + ((bus - start bus) << 20 | device << 15 | function << 12).
    assert_eq!(SEGMENT.address(0x10, 0, 0), Ok(0x1_0000_0000));
    assert_eq!(SEGMENT.address(0x11, 0, 0), Ok(0x1_0010_0000));
    assert_eq!(SEGMENT.address(0x10, 1, 0), Ok(0x1_0000_8000));
    assert_eq!(SEGMENT.address(0x10, 0, 1), Ok(0x1_0000_1000));
    assert_eq!(SEGMENT.address(0x1F, 31, 7), Ok(0x1_00FF_F000));
    let whole = Allocation {
        base: 0xE000_0000,
        segment: 0,
        start_bus: 0,
        end_bus: 0xFF,
    };
    assert_eq!(whole.address(0xFF, 31, 7), Ok(0xEFFF_F000));
}

#[test]
fn refuses_a_bus_outside_the_allocation() {
    assert_eq!(SEGMENT.address(0x0F, 0, 0), Err(Error::BusOutOfRange));
    assert_eq!(SEGMENT.address(0x20, 0, 0), Err(Error::BusOutOfRange));
    assert_eq!(SEGMENT.address(0, 0, 0), Err(Error::BusOutOfRange));
    // An allocation whose end comes before its start holds no bus.
    let reversed = Allocation {
        start_bus: 5,
        end_bus: 4,
        ..SEGMENT
    };
    for bus in [3, 4, 5, 6] {
        assert_eq!(reversed.address(bus, 0, 0), Err(Error::BusOutOfRange));
    }
}

#[test]
fn refuses_a_device_or_function_out_of_range() {
    assert_eq!(SEGMENT.address(0x10, 32, 0), Err(Error::DeviceOutOfRange));
    assert_eq!(SEGMENT.address(0x10, 0xFF, 0), Err(Error::DeviceOutOfRange));
    assert_eq!(SEGMENT.address(0x10, 0, 8), Err(Error::FunctionOutOfRange));
    assert_eq!(
        SEGMENT.address(0x10, 0, 0xFF),
        Err(Error::FunctionOutOfRange)
    );
}

#[test]
fn refuses_an_address_that_overflows() {
    let high = Allocation {
        base: 0xFFFF_FFFF_FFF0_0000,
        ..SEGMENT
    };
    assert_eq!(high.address(0x10, 31, 7), Ok(0xFFFF_FFFF_FFFF_F000));
    assert_eq!(high.address(0x11, 0, 0), Err(Error::AddressOverflow));
}

// ---- The header ----

#[test]
fn reads_a_general_device_header() {
    let bytes = general();
    let h = read(&bytes);
    assert_eq!(h.bytes(), &bytes[..]);
    assert_eq!(h.vendor_id(), 0x1B36);
    assert_eq!(h.device_id(), 0x0010);
    assert_eq!(h.command(), Command(0x0407));
    for bit in [
        Command::IO_SPACE,
        Command::MEMORY_SPACE,
        Command::BUS_MASTER,
        Command::INTERRUPT_DISABLE,
    ] {
        assert!(h.command().has(bit), "{bit:#x}");
    }
    assert_eq!(h.status(), Status(0x0010));
    assert!(h.status().has(Status::CAPABILITIES_LIST));
    assert_eq!(h.revision(), 2);
    assert_eq!(
        h.class(),
        Class {
            class: 0x01,
            subclass: 0x08,
            interface: 0x02
        }
    );
    assert!(h.multi_function());
    assert_eq!(
        h.layout(),
        Layout::General(General {
            bars: [
                0xFEBF_0000,
                0x0000_C001,
                0x8000_000C,
                0x0000_0001,
                0,
                0x000F_0002
            ],
            subsystem_vendor_id: 0x1AF4,
            subsystem_id: 0x1100,
            capabilities_pointer: 0x40,
            interrupt_line: 11,
            interrupt_pin: 1,
        })
    );
}

#[test]
fn the_named_bits_are_the_specification_s() {
    // Command: I/O space bit 0, memory space bit 1, bus master bit 2,
    // interrupt disable bit 10. Status: capabilities list bit 4.
    assert_eq!(Command::IO_SPACE, 1 << 0);
    assert_eq!(Command::MEMORY_SPACE, 1 << 1);
    assert_eq!(Command::BUS_MASTER, 1 << 2);
    assert_eq!(Command::INTERRUPT_DISABLE, 1 << 10);
    assert_eq!(Status::CAPABILITIES_LIST, 1 << 4);
    let off = Command(!0x0407);
    assert!(!off.has(Command::IO_SPACE) && !off.has(Command::BUS_MASTER));
    assert!(!Status(!0x0010).has(Status::CAPABILITIES_LIST));
}

#[test]
fn reads_a_bridge_header() {
    let bytes = bridge();
    let h = read(&bytes);
    assert_eq!(h.vendor_id(), 0x8086);
    assert!(!h.multi_function());
    assert_eq!(
        h.class(),
        Class {
            class: 0x06,
            subclass: 0x04,
            interface: 0
        }
    );
    assert_eq!(
        h.layout(),
        Layout::Bridge(Bridge {
            bars: [0xFE80_0000, 0],
            primary_bus: 0,
            secondary_bus: 1,
            subordinate_bus: 4,
            capabilities_pointer: 0x60,
        })
    );
    let bars: Vec<_> = h.bars().expect("accepted").collect();
    assert_eq!(
        bars,
        [(
            0,
            Bar::Memory {
                base: 0xFE80_0000,
                width: Width::Bits32,
                prefetchable: false
            }
        )]
    );
    let caps: Vec<_> = h.capabilities().expect("accepted").collect();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].offset, 0x60);
    assert_eq!(
        caps[0].decoded,
        Decoded::Express(Express {
            version: 2,
            port_type: PortType::RootPort
        })
    );
}

#[test]
fn an_unknown_header_type_is_reported_as_such() {
    let mut bytes = general();
    bytes[0x0E] = 0x82; // multi-function, type 2
    let h = read(&bytes);
    assert_eq!(h.layout(), Layout::Unknown(2));
    assert!(h.multi_function());
    assert_eq!(h.bars().err(), Some(Error::UnknownLayout));
    assert_eq!(h.capabilities().err(), Some(Error::UnknownLayout));
    bytes[0x0E] = 0x7F;
    assert_eq!(read(&bytes).layout(), Layout::Unknown(0x7F));
    assert!(!read(&bytes).multi_function());
}

#[test]
fn a_vendor_of_all_ones_means_no_function() {
    // What a read of an absent function returns: all ones.
    assert!(matches!(Header::read(&[0xFF; 256]), Ok(None)));
    let mut bytes = general();
    put16(&mut bytes, 0, 0xFFFF);
    assert!(matches!(Header::read(&bytes), Ok(None)));
}

#[test]
fn rejects_a_header_shorter_than_64_bytes() {
    let bytes = general();
    assert_eq!(Header::read(&bytes[..63]).err(), Some(Error::Truncated));
    assert_eq!(Header::read(&[]).err(), Some(Error::Truncated));
    assert!(Header::read(&bytes[..64]).is_ok());
}

#[test]
fn a_header_keeps_at_most_the_256_byte_region() {
    let mut bytes = general();
    bytes.resize(4096, 0x11);
    let h = read(&bytes);
    assert_eq!(h.bytes().len(), 256);
    let bytes = general();
    assert_eq!(read(&bytes[..100]).bytes().len(), 100);
}

// ---- Address registers ----

#[test]
fn decodes_each_kind_of_address_register() {
    // PCI Local Bus Specification, "Base Addresses": bit 0 is 1 for I/O,
    // whose base is bits 31:2; for memory bits 2:1 are the type (00 32-bit,
    // 10 64-bit, 01 and 11 reserved), bit 3 prefetchable, base bits 31:4.
    assert_eq!(
        Bar::decode(0xFEBF_0000, 0xFFFF_FFFF),
        Bar::Memory {
            base: 0xFEBF_0000,
            width: Width::Bits32,
            prefetchable: false
        }
    );
    assert_eq!(Bar::decode(0x0000_C0FF, 0), Bar::Io { base: 0xC0FC });
    assert_eq!(
        Bar::decode(0x8000_000C, 0x0000_0001),
        Bar::Memory {
            base: 0x1_8000_0000,
            width: Width::Bits64,
            prefetchable: true
        }
    );
    assert_eq!(
        Bar::decode(0x000F_0002, 0x1234),
        Bar::Memory {
            base: 0xF_0000,
            width: Width::Reserved(1),
            prefetchable: false
        }
    );
    assert_eq!(
        Bar::decode(0xFFFF_FFFE, 0xFFFF_FFFF),
        Bar::Memory {
            base: 0xFFFF_FFF0,
            width: Width::Reserved(3),
            prefetchable: true
        }
    );
}

#[test]
fn lists_the_implemented_registers_of_a_general_header() {
    let bytes = general();
    let bars: Vec<_> = read(&bytes).bars().expect("accepted").collect();
    assert_eq!(
        bars,
        [
            (
                0,
                Bar::Memory {
                    base: 0xFEBF_0000,
                    width: Width::Bits32,
                    prefetchable: false
                }
            ),
            (1, Bar::Io { base: 0xC000 }),
            // Register 3 is the high half of register 2.
            (
                2,
                Bar::Memory {
                    base: 0x1_8000_0000,
                    width: Width::Bits64,
                    prefetchable: true
                }
            ),
            // Register 4 reads as zero: not implemented.
            (
                5,
                Bar::Memory {
                    base: 0xF_0000,
                    width: Width::Reserved(1),
                    prefetchable: false
                }
            ),
        ]
    );
}

#[test]
fn refuses_a_64_bit_register_in_the_last_slot() {
    let mut bytes = general();
    put32(&mut bytes, 0x24, 0x0000_0004);
    assert_eq!(
        read(&bytes).bars().err(),
        Some(Error::WideBarInLastSlot { index: 5 })
    );
    let mut bytes = bridge();
    put32(&mut bytes, 0x14, 0xFE00_000C);
    assert_eq!(
        read(&bytes).bars().err(),
        Some(Error::WideBarInLastSlot { index: 1 })
    );
    // In the slot before the last it pairs with the last.
    put32(&mut bytes, 0x10, 0x0000_0004);
    put32(&mut bytes, 0x14, 0x0000_0002);
    assert_eq!(
        read(&bytes).bars().map(|b| b.collect::<Vec<_>>()),
        Ok(vec![(
            0,
            Bar::Memory {
                base: 0x2_0000_0000,
                width: Width::Bits64,
                prefetchable: false
            }
        )])
    );
}

#[test]
fn sizes_a_32_bit_memory_register() {
    assert_eq!(bar_size(0xFFFF_F000, 0), Ok(Some(0x1000)));
    // Flag bits are not part of the size: prefetchable, 1 MiB.
    assert_eq!(bar_size(0xFFF0_0008, 0), Ok(Some(0x10_0000)));
    // The smallest, 16 bytes, and the largest, 2 GiB.
    assert_eq!(bar_size(0xFFFF_FFF0, 0), Ok(Some(16)));
    assert_eq!(bar_size(0x8000_0000, 0), Ok(Some(0x8000_0000)));
    // The high half is not read for a 32-bit register.
    assert_eq!(bar_size(0xFFFF_F000, 0x1234), Ok(Some(0x1000)));
}

#[test]
fn sizes_an_io_register() {
    assert_eq!(bar_size(0xFFFF_FFE1, 0), Ok(Some(32)));
    assert_eq!(bar_size(0xFFFF_FFFD, 0), Ok(Some(4)));
    // A register decoding only 16 bits of I/O address reads back zeros in
    // the upper half.
    assert_eq!(bar_size(0x0000_FFE1, 0), Ok(Some(32)));
}

#[test]
fn sizes_a_64_bit_register_above_4_gib() {
    // 64 GiB: the low half's address bits all read back as zero.
    assert_eq!(bar_size(0x0000_000C, 0xFFFF_FFF0), Ok(Some(0x10_0000_0000)));
    // 16 KiB, with every high bit writable.
    assert_eq!(bar_size(0xFFFF_C004, 0xFFFF_FFFF), Ok(Some(0x4000)));
    // 2^63 bytes, the most 64 bits can describe.
    assert_eq!(bar_size(0x0000_0004, 0x8000_0000), Ok(Some(1 << 63)));
}

#[test]
fn a_read_back_of_zero_means_not_implemented() {
    assert_eq!(bar_size(0, 0), Ok(None));
    assert_eq!(bar_size(0, 0xFFFF_FFFF), Ok(None));
    // Only flag bits, no address bits.
    assert_eq!(bar_size(0x0000_0001, 0), Ok(None));
    assert_eq!(bar_size(0x0000_0008, 0), Ok(None));
    assert_eq!(bar_size(0x0000_000C, 0), Ok(None));
}

#[test]
fn refuses_a_read_back_that_is_not_a_power_of_two() {
    assert_eq!(bar_size(0xFFF0_F000, 0), Err(Error::BadSize));
    assert_eq!(bar_size(0x7FFF_F000, 0), Err(Error::BadSize));
    assert_eq!(bar_size(0xFFFF_0F01, 0), Err(Error::BadSize));
    assert_eq!(bar_size(0xFFFF_C004, 0x0000_0000), Err(Error::BadSize));
    assert_eq!(bar_size(0xFFFF_C004, 0xFFFE_FFFF), Err(Error::BadSize));
}

#[test]
fn refuses_to_size_a_reserved_memory_type() {
    assert_eq!(bar_size(0xFFFF_F002, 0), Err(Error::ReservedWidth));
    assert_eq!(bar_size(0xFFFF_F006, 0), Err(Error::ReservedWidth));
}

// ---- The capability list ----

#[test]
fn reads_one_capability_of_each_kind() {
    let bytes = with_capabilities();
    let caps: Vec<_> = read(&bytes).capabilities().expect("accepted").collect();
    let ids: Vec<_> = caps.iter().map(|c| (c.id, c.offset)).collect();
    assert_eq!(
        ids,
        [
            (0x05, 0x40),
            (0x09, 0xF0),
            (0x05, 0x50),
            (0x10, 0x80),
            (0x11, 0x70)
        ]
    );
    for c in &caps {
        // Each gets the bytes from its offset to the end of the region.
        assert_eq!(c.bytes, &bytes[c.offset..256]);
    }
    assert_eq!(
        caps[0].decoded,
        Decoded::Msi(Msi {
            control: 0x0015,
            enabled: true,
            multiple_message_capable: 2,
            multiple_message_enable: 1,
            is_64bit: false,
            per_vector_masking: false,
            address_offset: 0x44,
            upper_address_offset: None,
            data_offset: 0x48,
        })
    );
    assert_eq!(caps[1].decoded, Decoded::Other);
    assert_eq!(&caps[1].bytes[..6], &[0x09, 0x50, 6, 0xAB, 0xCD, 0xEF]);
    assert_eq!(
        caps[2].decoded,
        Decoded::Msi(Msi {
            control: 0x018A,
            enabled: false,
            multiple_message_capable: 5,
            multiple_message_enable: 0,
            is_64bit: true,
            per_vector_masking: true,
            address_offset: 0x54,
            upper_address_offset: Some(0x58),
            data_offset: 0x5C,
        })
    );
    assert_eq!(
        caps[3].decoded,
        Decoded::Express(Express {
            version: 2,
            port_type: PortType::RootPort
        })
    );
    assert_eq!(
        caps[4].decoded,
        Decoded::MsiX(MsiX {
            control: 0xC03F,
            table_size: 64,
            enabled: true,
            function_mask: true,
            table: Location {
                bar: 0,
                offset: 0x2000
            },
            pending: Location {
                bar: 4,
                offset: 0x3000
            },
        })
    );
}

#[test]
fn decodes_every_express_port_type() {
    let types = [
        (0x0, PortType::Endpoint),
        (0x1, PortType::LegacyEndpoint),
        (0x4, PortType::RootPort),
        (0x5, PortType::UpstreamSwitchPort),
        (0x6, PortType::DownstreamSwitchPort),
        (0x7, PortType::ExpressToPciBridge),
        (0x8, PortType::PciToExpressBridge),
        (0x9, PortType::IntegratedEndpoint),
        (0xA, PortType::EventCollector),
        (0x2, PortType::Other(0x2)),
        (0xF, PortType::Other(0xF)),
    ];
    for (code, port_type) in types {
        let mut bytes = general();
        link(&mut bytes, &[(0x40, express(0xFF02 | code << 4))]);
        let caps: Vec<_> = read(&bytes).capabilities().expect("accepted").collect();
        assert_eq!(
            caps[0].decoded,
            Decoded::Express(Express {
                version: 2,
                port_type
            }),
            "type {code:#x}"
        );
    }
}

#[test]
fn an_msi_x_table_size_is_one_more_than_its_field() {
    // Bits 10:0 hold N - 1; bits 13:11 are reserved.
    for (control, size) in [(0x0000, 1), (0x07FF, 2048), (0x3800, 1)] {
        let mut bytes = general();
        link(&mut bytes, &[(0x40, msix(control, 0x7, 0xFFFF_FFFD))]);
        let caps: Vec<_> = read(&bytes).capabilities().expect("accepted").collect();
        let Decoded::MsiX(m) = caps[0].decoded else {
            panic!("not MSI-X")
        };
        assert_eq!(m.table_size, size, "{control:#x}");
        assert!(!m.enabled && !m.function_mask);
        assert_eq!(m.table, Location { bar: 7, offset: 0 });
        assert_eq!(
            m.pending,
            Location {
                bar: 5,
                offset: 0xFFFF_FFF8
            }
        );
    }
}

#[test]
fn reads_msi_x_enable_and_function_mask_apart() {
    // Enable is bit 15, function mask bit 14.
    for (control, enabled, function_mask) in [(0x8000, true, false), (0x4000, false, true)] {
        let mut bytes = general();
        link(&mut bytes, &[(0x40, msix(control, 0, 0))]);
        let caps: Vec<_> = read(&bytes).capabilities().expect("accepted").collect();
        let Decoded::MsiX(m) = caps[0].decoded else {
            panic!("not MSI-X")
        };
        assert_eq!((m.enabled, m.function_mask), (enabled, function_mask));
    }
}

#[test]
fn each_msi_form_needs_its_own_length() {
    // 32-bit 10 bytes, 64-bit 14, 32-bit with masking 20, 64-bit with
    // masking 24: each fits exactly at the end of the region, and is
    // refused one dword further on.
    for (control, size) in [(0x0000, 10), (0x0080, 14), (0x0100, 20), (0x0180, 24)] {
        let fits = (256 - size) & !3;
        for (at, accepted) in [(fits, true), (fits + 4, false)] {
            let mut bytes = general();
            link(&mut bytes, &[(at, msi(control)[..2].to_vec())]);
            put16(&mut bytes, at + 2, control);
            let expected = if accepted {
                Ok(vec![(0x05, at)])
            } else {
                Err(Error::CapabilityTooShort { offset: at })
            };
            assert_eq!(walk(bytes), expected, "control {control:#x} at {at:#x}");
        }
    }
}

#[test]
fn masks_the_low_two_bits_of_every_pointer() {
    let mut bytes = general();
    link(&mut bytes, &[(0x40, msi(0)), (0x50, express(0x0002))]);
    bytes[0x34] = 0x43;
    bytes[0x41] = 0x51;
    bytes[0x51] = 0x02; // masks to zero: the end
    assert_eq!(walk(bytes), Ok(vec![(0x05, 0x40), (0x10, 0x50)]));
}

#[test]
fn no_capabilities_without_the_status_bit() {
    let mut bytes = with_capabilities();
    put16(&mut bytes, 0x06, !0x0010);
    assert_eq!(walk(bytes), Ok(vec![]));
    // And a pointer of zero with the bit set is an empty list.
    let mut bytes = general();
    bytes[0x34] = 0;
    assert_eq!(walk(bytes), Ok(vec![]));
}

#[test]
fn refuses_a_pointer_into_the_header() {
    let mut bytes = general();
    bytes[0x34] = 0x3C;
    assert_eq!(walk(bytes), Err(Error::CapabilityInHeader { offset: 0x3C }));
    let mut bytes = general();
    bytes[0x34] = 0x04;
    assert_eq!(walk(bytes), Err(Error::CapabilityInHeader { offset: 0x04 }));
    // A next pointer into the header is refused the same way.
    let mut bytes = with_capabilities();
    bytes[0x81] = 0x34;
    assert_eq!(walk(bytes), Err(Error::CapabilityInHeader { offset: 0x34 }));
}

#[test]
fn refuses_a_pointer_past_the_bytes_given() {
    let mut bytes = general();
    bytes[0x34] = 0x80;
    bytes.truncate(0x80);
    assert_eq!(walk(bytes), Err(Error::CapabilityPastEnd { offset: 0x80 }));
    // The id fits but the next pointer does not.
    let mut bytes = with_capabilities();
    bytes.truncate(0xF1);
    assert_eq!(walk(bytes), Err(Error::CapabilityPastEnd { offset: 0xF0 }));
}

#[test]
fn refuses_a_capability_longer_than_the_bytes_left() {
    // MSI-X needs 12 bytes; at 0xF8 only 8 remain in the 256-byte region,
    // even when the bytes given go on to 4096.
    let mut bytes = general();
    link(&mut bytes, &[(0x40, msi(0))]);
    bytes[0x41] = 0xF8;
    bytes[0xF8] = 0x11;
    bytes.resize(4096, 0);
    assert_eq!(walk(bytes), Err(Error::CapabilityTooShort { offset: 0xF8 }));
    // 64-bit MSI with masking needs 24; at 0xF0 there are 16.
    let mut bytes = general();
    link(&mut bytes, &[(0xF0, msi(0))]);
    put16(&mut bytes, 0xF2, 0x0180);
    assert_eq!(walk(bytes), Err(Error::CapabilityTooShort { offset: 0xF0 }));
    // Without masking 14 is enough.
    let mut bytes = general();
    link(&mut bytes, &[(0xF0, msi(0x0080))]);
    assert_eq!(walk(bytes), Ok(vec![(0x05, 0xF0)]));
    // PCI Express needs its capabilities register: 4 bytes.
    let mut bytes = general();
    link(&mut bytes, &[(0xFC, vec![0x10, 0])]);
    bytes.truncate(0xFE);
    assert_eq!(walk(bytes), Err(Error::CapabilityTooShort { offset: 0xFC }));
}

#[test]
fn refuses_a_list_that_loops() {
    let mut bytes = general();
    link(&mut bytes, &[(0x40, msi(0))]);
    bytes[0x41] = 0x40;
    assert_eq!(walk(bytes), Err(Error::CapabilityLoop { offset: 0x40 }));
    let mut bytes = with_capabilities();
    bytes[0x71] = 0x50; // the last points back at the third
    assert_eq!(walk(bytes), Err(Error::CapabilityLoop { offset: 0x50 }));
}

#[test]
fn a_list_of_every_dword_ends_and_one_more_step_loops() {
    // 48 capabilities, one in each dword from 0x40 to 0xFC: the longest
    // list that fits. Each is an unknown id with nothing but its header.
    let mut bytes = general();
    let caps: Vec<_> = (0..48).map(|i| (0x40 + 4 * i, vec![0x0A, 0])).collect();
    link(&mut bytes, &caps);
    let list = walk(bytes.clone()).expect("accepted");
    assert_eq!(list.len(), 48);
    assert_eq!(list.last(), Some(&(0x0A, 0xFC)));
    bytes[0xFD] = 0x40;
    assert_eq!(walk(bytes), Err(Error::CapabilityLoop { offset: 0x40 }));
}

// ---- Real data ----

/// The configuration data QEMU's `q35` machine presents, captured as
/// `tests/data/qemu-q35.md` describes: records of a 64-bit physical
/// address, a 32-bit length and that many bytes, all little-endian.
fn captured() -> Vec<(u64, &'static [u8])> {
    let mut rest: &[u8] = include_bytes!("../tests/data/qemu-q35.bin");
    let mut records = Vec::new();
    while !rest.is_empty() {
        let address = u64::from_le_bytes(rest[..8].try_into().unwrap());
        let length = u32::from_le_bytes(rest[8..12].try_into().unwrap()) as usize;
        records.push((address, &rest[12..12 + length]));
        rest = &rest[12 + length..];
    }
    records
}

fn memory(base: u64, width: Width, prefetchable: bool) -> Bar {
    Bar::Memory {
        base,
        width,
        prefetchable,
    }
}

/// A function on bus 0 as the test below expects it: device, function,
/// vendor, device ID, class, subclass and interface, multi-function, and
/// its address registers.
type Expected = (u8, u8, u16, u16, [u8; 3], bool, Vec<(usize, Bar)>);

#[test]
fn reads_the_configuration_data_qemu_presents() {
    let records = captured();
    assert_eq!(records.len(), 8);

    // The MCFG the firmware installs: one allocation for every bus of
    // segment 0, at 0xE0000000.
    let (address, bytes) = records[0];
    assert_eq!(address, 0x0F77_6000);
    let table = Mcfg::read(bytes).expect("MCFG");
    let allocations: Vec<_> = table.allocations().collect();
    let all = Allocation {
        base: 0xE000_0000,
        segment: 0,
        start_bus: 0,
        end_bus: 0xFF,
    };
    assert_eq!(allocations, [all]);

    // Each function on bus 0, by device and function: vendor, device,
    // class, subclass, interface, multi-function, address registers.
    let expected: [Expected; 7] = [
        (0, 0, 0x8086, 0x29C0, [0x06, 0x00, 0x00], false, vec![]),
        (
            1,
            0,
            0x1234,
            0x1111,
            [0x03, 0x00, 0x00],
            false,
            vec![
                (0, memory(0x8000_0000, Width::Bits32, true)),
                (2, memory(0x8101_1000, Width::Bits32, false)),
            ],
        ),
        (
            4,
            0,
            0x1B36,
            0x000D,
            [0x0C, 0x03, 0x30],
            false,
            vec![(0, memory(0xC0_0000_4000, Width::Bits64, false))],
        ),
        (
            5,
            0,
            0x1B36,
            0x0010,
            [0x01, 0x08, 0x02],
            false,
            vec![(0, memory(0xC0_0000_0000, Width::Bits64, false))],
        ),
        (31, 0, 0x8086, 0x2918, [0x06, 0x01, 0x00], true, vec![]),
        (
            31,
            2,
            0x8086,
            0x2922,
            [0x01, 0x06, 0x01],
            true,
            vec![
                (4, Bar::Io { base: 0x6040 }),
                (5, memory(0x8101_0000, Width::Bits32, false)),
            ],
        ),
        (
            31,
            3,
            0x8086,
            0x2930,
            [0x0C, 0x05, 0x00],
            true,
            vec![(4, Bar::Io { base: 0x6000 })],
        ),
    ];
    for ((address, bytes), (device, function, vendor, id, class, multi, bars)) in
        records[1..].iter().zip(expected)
    {
        let at = format!("00:{device:02x}.{function}");
        assert_eq!(all.address(0, device, function), Ok(*address), "{at}");
        assert_eq!(bytes.len(), 256);
        let h = read(bytes);
        assert_eq!((h.vendor_id(), h.device_id()), (vendor, id), "{at}");
        let [class, subclass, interface] = class;
        assert_eq!(
            h.class(),
            Class {
                class,
                subclass,
                interface
            },
            "{at}"
        );
        assert_eq!(h.multi_function(), multi, "{at}");
        assert!(matches!(h.layout(), Layout::General(_)), "{at}");
        assert_eq!(h.bars().map(|b| b.collect::<Vec<_>>()), Ok(bars), "{at}");
    }

    // The functions with capabilities: the USB controller (xHCI), the NVMe
    // controller and the SATA controller.
    let caps = |index: usize| -> Vec<(usize, Decoded)> {
        read(records[index].1)
            .capabilities()
            .expect("accepted")
            .map(|c| (c.offset, c.decoded))
            .collect()
    };
    let integrated = Decoded::Express(Express {
        version: 2,
        port_type: PortType::IntegratedEndpoint,
    });
    assert_eq!(
        caps(3),
        [
            (
                0x90,
                Decoded::MsiX(MsiX {
                    control: 0x000F,
                    table_size: 16,
                    enabled: false,
                    function_mask: false,
                    table: Location {
                        bar: 0,
                        offset: 0x3000
                    },
                    pending: Location {
                        bar: 0,
                        offset: 0x3800
                    },
                })
            ),
            (0xA0, integrated),
        ]
    );
    assert_eq!(
        caps(4),
        [
            (
                0x40,
                Decoded::MsiX(MsiX {
                    control: 0x0040,
                    table_size: 65,
                    enabled: false,
                    function_mask: false,
                    table: Location {
                        bar: 0,
                        offset: 0x2000
                    },
                    pending: Location {
                        bar: 0,
                        offset: 0x3000
                    },
                })
            ),
            (0x80, integrated),
            // Power management (id 0x01), not decoded here.
            (0x60, Decoded::Other),
        ]
    );
    assert_eq!(
        caps(6),
        [
            (
                0x80,
                Decoded::Msi(Msi {
                    control: 0x0080,
                    enabled: false,
                    multiple_message_capable: 0,
                    multiple_message_enable: 0,
                    is_64bit: true,
                    per_vector_masking: false,
                    address_offset: 0x84,
                    upper_address_offset: Some(0x88),
                    data_offset: 0x8C,
                })
            ),
            // SATA (id 0x12), not decoded here.
            (0xA8, Decoded::Other),
        ]
    );
    // The rest have no capabilities list.
    for index in [1, 2, 5, 7] {
        assert!(
            !read(records[index].1)
                .status()
                .has(Status::CAPABILITIES_LIST)
        );
        assert_eq!(caps(index), []);
    }
}

// ---- Corruption ----

/// A tiny xorshift generator (Marsaglia, 2003) so the corruption test is
/// the same on every run without a dependency.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }
}

/// Runs every reader on `bytes` and checks that each view lies inside them
/// and each iterator ends. Returns whether a header was read and how many
/// capabilities it listed.
fn read_everything(bytes: &[u8]) -> (bool, usize) {
    if let Ok(table) = Mcfg::read(bytes) {
        assert!(inside(bytes, table.table().bytes()));
        let count = table.allocations().take(bytes.len() + 1).count();
        assert!(count <= bytes.len() / 16, "{count} allocations");
    }
    let Ok(Some(header)) = Header::read(bytes) else {
        return (false, 0);
    };
    assert!(inside(bytes, header.bytes()));
    assert!(header.bytes().len() >= 64 && header.bytes().len() <= 256);
    let _ = (header.layout(), header.class(), header.command());
    if let Ok(bars) = header.bars() {
        // At most six registers, so seven means it did not end.
        let bars: Vec<_> = bars.take(7).collect();
        assert!(bars.len() <= 6);
        for (index, _) in &bars {
            assert!(*index < 6);
        }
    }
    let mut count = 0;
    if let Ok(caps) = header.capabilities() {
        // At most one capability per dword above the header.
        for c in caps.take(49) {
            count += 1;
            assert!(c.offset >= 64 && c.offset <= 0xFC && c.offset % 4 == 0);
            assert!(inside(header.bytes(), c.bytes));
            assert!(c.bytes.len() >= 2);
            assert_eq!(c.bytes.as_ptr(), header.bytes()[c.offset..].as_ptr());
            if let Decoded::Msi(m) = c.decoded {
                let end = m.data_offset + 2;
                assert!(end <= c.offset + c.bytes.len());
            }
        }
        assert!(count <= 48, "{count} capabilities");
    }
    (true, count)
}

/// Damages headers, capability lists and MCFG tables many thousands of
/// times, and checks that nothing panics, every view stays inside its
/// bytes and every walk ends. It runs on a thread of its own, so a reader
/// that loops forever fails the test.
#[test]
fn damaged_data_is_rejected_or_stays_inside_its_bytes() {
    let (walked, refused, mcfgs) = ends(300, damage_many_times);
    println!("{walked} lists walked, {refused} refused, {mcfgs} MCFGs read");
    // Both outcomes must be common, or the test explores too little.
    assert!(
        walked > 5_000 && refused > 5_000 && mcfgs > 1_000,
        "{walked} lists walked, {refused} refused, {mcfgs} MCFGs read"
    );
}

/// The cases of the test above: how many capability lists were walked, how
/// many refused, and how many MCFGs read.
fn damage_many_times() -> (usize, usize, usize) {
    let mut long_list = general();
    let caps: Vec<_> = (0..48).map(|i| (0x40 + 4 * i, vec![0x0A, 0])).collect();
    link(&mut long_list, &caps);
    let bases: [(Vec<u8>, Vec<usize>); 4] = [
        // Status, header type, the capabilities pointer, every next
        // pointer and message control word.
        (
            with_capabilities(),
            vec![
                0x06, 0x0E, 0x34, 0x41, 0x42, 0x51, 0x52, 0x53, 0x71, 0x72, 0x81, 0xF1,
            ],
        ),
        (bridge(), vec![0x06, 0x0E, 0x34, 0x10, 0x14, 0x61]),
        // Pointers and ids along the longest list.
        (
            long_list,
            vec![0x34, 0x40, 0x41, 0x80, 0x81, 0xC1, 0xF0, 0xFD],
        ),
        (
            mcfg(&[
                allocation_bytes(0xE000_0000, 0, 0, 0xFF),
                allocation_bytes(u64::MAX, 1, 0xFF, 0),
            ]),
            vec![4, 4, 44, 54, 55],
        ),
    ];
    // Pointers near the header's end and the region's, the three decoded
    // ids, and the 64-bit and masking bits of MSI's control word.
    let edges: [u8; 14] = [
        0, 1, 0x3C, 0x3F, 0x40, 0x41, 0xFC, 0xFE, 0xFF, 0x05, 0x10, 0x11, 0x80, 0x81,
    ];

    let mut random = Random(0x9C1_0000_5EED_0002);
    let (mut walked, mut refused, mut mcfgs) = (0, 0, 0);
    for case in 0..60_000 {
        let kind = case % bases.len();
        let (base, fields) = &bases[kind];
        let mut bytes = base.clone();
        for _ in 0..1 + random.below(3) {
            match random.below(4) {
                // Any byte becomes any value.
                0 => {
                    let at = random.below(bytes.len());
                    bytes[at] = random.next() as u8;
                }
                // A pointer, length or control field becomes a value near
                // an edge.
                1 | 2 => {
                    let at = fields[random.below(fields.len())];
                    let value = edges[random.below(edges.len())];
                    if kind == 3 && at == 4 {
                        let near = bytes.len() as u32 + random.below(3) as u32;
                        put32(&mut bytes, 4, near.wrapping_sub(1));
                    } else {
                        bytes[at] = value;
                    }
                }
                // A 64-bit register appears anywhere among the six.
                _ => {
                    let slot = random.below(6);
                    put32(&mut bytes, 0x10 + 4 * slot, 0x0000_0004);
                }
            }
        }
        if kind == 3 && random.below(4) != 0 {
            let stated = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            let end = stated.clamp(10, bytes.len());
            fix_checksum(&mut bytes[..end], 9);
        }
        // Sometimes the bytes are cut short.
        if random.below(4) == 0 {
            bytes.truncate(random.below(bytes.len() + 1));
        }
        let (header, _) = read_everything(&bytes);
        if kind == 3 {
            if Mcfg::read(&bytes).is_ok() {
                mcfgs += 1;
            }
        } else if header {
            match Header::read(&bytes).unwrap().unwrap().capabilities() {
                Ok(_) => walked += 1,
                Err(_) => refused += 1,
            }
        }
        // The pure functions take any values.
        let (low, high) = (random.next() as u32, random.next() as u32);
        if let Ok(Some(size)) = bar_size(low, high) {
            assert!(size.is_power_of_two());
        }
        let _ = Bar::decode(low, high);
        let allocation = Allocation {
            base: random.next(),
            segment: 0,
            start_bus: random.next() as u8,
            end_bus: random.next() as u8,
        };
        let (bus, device, function) = (
            random.next() as u8,
            random.below(40) as u8,
            random.below(10) as u8,
        );
        if let Ok(address) = allocation.address(bus, device, function) {
            assert!(address >= allocation.base);
            assert_eq!((address - allocation.base) % 4096, 0);
            assert!(device < 32 && function < 8);
            assert!(allocation.start_bus <= bus && bus <= allocation.end_bus);
        }
    }
    (walked, refused, mcfgs)
}

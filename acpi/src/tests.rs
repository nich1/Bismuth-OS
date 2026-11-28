//! Tests for the reader. Every structure is built byte by byte from the
//! field tables in the ACPI specification, version 6.5, with the offsets
//! written out here rather than taken from the library, so a failing test
//! means the reader and the specification disagree.

// Building test tables indexes and adds freely; the crate's lints are for
// the reader itself.
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

/// Sets both root pointer checksums: byte 8 over the first 20 bytes, then
/// byte 32 over the first 36 (section 5.2.5.3).
fn fix_rsdp(rsdp: &mut [u8]) {
    fix_checksum(&mut rsdp[..20], 8);
    if rsdp.len() >= 36 {
        fix_checksum(&mut rsdp[..36], 32);
    }
}

const RSDT_AT: u32 = 0x0F77_D074;
const XSDT_AT: u64 = 0x1_2345_6000;

/// An ACPI 1.0 root pointer (section 5.2.5.3): 20 bytes.
fn rsdp_v0(rsdt: u32) -> Vec<u8> {
    let mut rsdp = vec![0u8; 20];
    put(&mut rsdp, 0, b"RSD PTR ");
    put(&mut rsdp, 9, b"OEMID1");
    rsdp[15] = 0; // revision
    put32(&mut rsdp, 16, rsdt);
    fix_rsdp(&mut rsdp);
    rsdp
}

/// A revision 2 root pointer (section 5.2.5.3): 36 bytes.
fn rsdp_v2(rsdt: u32, xsdt: u64) -> Vec<u8> {
    let mut rsdp = vec![0u8; 36];
    put(&mut rsdp, 0, b"RSD PTR ");
    put(&mut rsdp, 9, b"OEMID2");
    rsdp[15] = 2; // revision
    put32(&mut rsdp, 16, rsdt);
    put32(&mut rsdp, 20, 36); // length
    put64(&mut rsdp, 24, xsdt);
    rsdp[33..36].copy_from_slice(&[0xA5, 0x5A, 0xA5]); // reserved
    fix_rsdp(&mut rsdp);
    rsdp
}

/// A table with the header of section 5.2.6 followed by `body`.
fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut table = vec![0u8; 36];
    put(&mut table, 0, signature);
    put32(&mut table, 4, (36 + body.len()) as u32);
    table[8] = 3; // revision
    put(&mut table, 10, b"OEMID3");
    put(&mut table, 16, b"TABLEID4");
    put32(&mut table, 24, 0x1234_5678); // OEM revision
    put(&mut table, 28, b"CRTR"); // creator ID
    put32(&mut table, 32, 0x9ABC_DEF0); // creator revision
    table.extend_from_slice(body);
    fix_checksum(&mut table, 9);
    table
}

fn rsdt(addresses: &[u32]) -> Vec<u8> {
    let body: Vec<u8> = addresses.iter().flat_map(|a| a.to_le_bytes()).collect();
    table(b"RSDT", &body)
}

fn xsdt(addresses: &[u64]) -> Vec<u8> {
    let body: Vec<u8> = addresses.iter().flat_map(|a| a.to_le_bytes()).collect();
    table(b"XSDT", &body)
}

/// The MADT of section 5.2.12: the local interrupt controller address at
/// 36, the flags at 40, the entries from 44.
fn madt(flags: u32, entries: &[Vec<u8>]) -> Vec<u8> {
    let mut body = vec![0u8; 8];
    put32(&mut body, 0, 0xFEE0_0000);
    put32(&mut body, 4, flags);
    for entry in entries {
        body.extend_from_slice(entry);
    }
    table(b"APIC", &body)
}

/// Section 5.2.12.2: processor local APIC, 8 bytes.
fn local_apic(uid: u8, id: u8, flags: u32) -> Vec<u8> {
    let mut e = vec![0, 8, uid, id, 0, 0, 0, 0];
    put32(&mut e, 4, flags);
    e
}

/// Section 5.2.12.3: I/O APIC, 12 bytes.
fn io_apic(id: u8, address: u32, base: u32) -> Vec<u8> {
    let mut e = vec![1, 12, id, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    put32(&mut e, 4, address);
    put32(&mut e, 8, base);
    e
}

/// Section 5.2.12.5: interrupt source override, 10 bytes.
fn source_override(bus: u8, source: u8, gsi: u32, flags: u16) -> Vec<u8> {
    let mut e = vec![2, 10, bus, source, 0, 0, 0, 0, 0, 0];
    put32(&mut e, 4, gsi);
    put16(&mut e, 8, flags);
    e
}

/// Section 5.2.12.7: local APIC NMI, 6 bytes.
fn local_nmi(uid: u8, flags: u16, lint: u8) -> Vec<u8> {
    let mut e = vec![4, 6, uid, 0, 0, lint];
    put16(&mut e, 3, flags);
    e
}

/// Section 5.2.12.8: local APIC address override, 12 bytes.
fn address_override(address: u64) -> Vec<u8> {
    let mut e = vec![5, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    put64(&mut e, 4, address);
    e
}

/// Section 5.2.12.12: processor local x2APIC, 16 bytes.
fn local_x2apic(id: u32, flags: u32, uid: u32) -> Vec<u8> {
    let mut e = vec![9, 16, 0, 0];
    e.extend_from_slice(&id.to_le_bytes());
    e.extend_from_slice(&flags.to_le_bytes());
    e.extend_from_slice(&uid.to_le_bytes());
    e
}

/// One entry of each kind the reader knows, then one of a type it does not
/// (0x7F, with three bytes of its own).
fn every_entry() -> Vec<Vec<u8>> {
    vec![
        local_apic(1, 2, 0b01),
        local_apic(3, 4, 0b10),
        io_apic(5, 0xFEC0_0000, 24),
        source_override(0, 9, 19, 0b1111),
        local_nmi(0xFF, 0b0101, 1),
        address_override(0x1_FEE0_0000),
        local_x2apic(0x1_0000, 0b01, 0x2_0000),
        vec![0x7F, 5, 0xAA, 0xBB, 0xCC],
    ]
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

/// [`Madt::read`] on `bytes`, failing the test if it does not end.
fn read_madt(bytes: Vec<u8>) -> Result<(), Error> {
    ends(10, move || Madt::read(&bytes).map(|_| ()))
}

fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let (outer, inner) = (outer.as_ptr_range(), inner.as_ptr_range());
    outer.start <= inner.start && inner.end <= outer.end
}

#[test]
fn reads_a_revision_0_root_pointer() {
    let rsdp = Rsdp::read(&rsdp_v0(RSDT_AT)).expect("accepted");
    assert_eq!(rsdp.revision, 0);
    assert_eq!(&rsdp.oem_id, b"OEMID1");
    assert_eq!(rsdp.rsdt_address, RSDT_AT);
    assert_eq!(rsdp.xsdt_address, 0);
    assert_eq!(rsdp.root(), Root::Rsdt(RSDT_AT));
}

#[test]
fn a_revision_0_root_pointer_ignores_the_bytes_after_it() {
    // Past byte 20 a revision 0 pointer has nothing, so what follows is
    // neither read as a length nor summed.
    let mut bytes = rsdp_v0(RSDT_AT);
    bytes.extend_from_slice(&[0xFF; 16]);
    assert_eq!(
        Rsdp::read(&bytes).map(|r| r.root()),
        Ok(Root::Rsdt(RSDT_AT))
    );
}

#[test]
fn reads_a_revision_2_root_pointer() {
    let mut bytes = rsdp_v2(RSDT_AT, XSDT_AT);
    bytes.extend_from_slice(&[0xFF; 4]); // past the stated length
    let rsdp = Rsdp::read(&bytes).expect("accepted");
    assert_eq!(rsdp.revision, 2);
    assert_eq!(&rsdp.oem_id, b"OEMID2");
    assert_eq!(rsdp.rsdt_address, RSDT_AT);
    assert_eq!(rsdp.xsdt_address, XSDT_AT);
    assert_eq!(rsdp.root(), Root::Xsdt(XSDT_AT));
}

#[test]
fn a_longer_revision_2_root_pointer_is_summed_over_its_length() {
    // A later revision may be longer; its extended checksum covers it all.
    let mut bytes = rsdp_v2(RSDT_AT, XSDT_AT);
    bytes[15] = 3;
    bytes.extend_from_slice(&[0x11; 4]);
    put32(&mut bytes, 20, 40);
    fix_checksum(&mut bytes[..20], 8);
    fix_checksum(&mut bytes, 32);
    assert_eq!(
        Rsdp::read(&bytes).map(|r| r.root()),
        Ok(Root::Xsdt(XSDT_AT))
    );
    bytes[39] ^= 1;
    assert_eq!(Rsdp::read(&bytes), Err(Error::BadExtendedChecksum));
}

#[test]
fn a_revision_2_root_pointer_without_an_xsdt_falls_back_to_the_rsdt() {
    let rsdp = Rsdp::read(&rsdp_v2(RSDT_AT, 0)).expect("accepted");
    assert_eq!(rsdp.root(), Root::Rsdt(RSDT_AT));
}

#[test]
fn rejects_a_root_pointer_that_names_no_table() {
    assert_eq!(Rsdp::read(&rsdp_v0(0)), Err(Error::NoRootTable));
    assert_eq!(Rsdp::read(&rsdp_v2(0, 0)), Err(Error::NoRootTable));
    // An XSDT alone is enough.
    assert_eq!(
        Rsdp::read(&rsdp_v2(0, XSDT_AT)).map(|r| r.root()),
        Ok(Root::Xsdt(XSDT_AT))
    );
}

#[test]
fn rejects_a_truncated_root_pointer() {
    assert_eq!(Rsdp::read(&[]), Err(Error::Truncated));
    assert_eq!(Rsdp::read(&rsdp_v0(RSDT_AT)[..19]), Err(Error::Truncated));
    // Revision 2 needs 36 bytes to be read at all.
    assert_eq!(
        Rsdp::read(&rsdp_v2(RSDT_AT, XSDT_AT)[..35]),
        Err(Error::Truncated)
    );
}

#[test]
fn rejects_a_wrong_root_pointer_signature() {
    for at in 0..8 {
        let mut bytes = rsdp_v2(RSDT_AT, XSDT_AT);
        bytes[at] ^= 0x20;
        fix_rsdp(&mut bytes);
        assert_eq!(Rsdp::read(&bytes), Err(Error::BadSignature), "byte {at}");
    }
}

#[test]
fn rejects_a_wrong_first_checksum() {
    for mut bytes in [rsdp_v0(RSDT_AT), rsdp_v2(RSDT_AT, XSDT_AT)] {
        bytes[8] = bytes[8].wrapping_add(1);
        assert_eq!(Rsdp::read(&bytes), Err(Error::BadChecksum));
    }
}

#[test]
fn rejects_a_wrong_extended_checksum() {
    // Byte 33 is outside the first 20, so only the extended sum changes.
    let mut bytes = rsdp_v2(RSDT_AT, XSDT_AT);
    bytes[33] ^= 1;
    assert_eq!(Rsdp::read(&bytes), Err(Error::BadExtendedChecksum));
    let mut bytes = rsdp_v2(RSDT_AT, XSDT_AT);
    bytes[32] = bytes[32].wrapping_add(1);
    assert_eq!(Rsdp::read(&bytes), Err(Error::BadExtendedChecksum));
}

#[test]
fn rejects_a_revision_2_root_pointer_with_a_bad_length() {
    for (length, error) in [
        (0, Error::LengthTooShort),
        (20, Error::LengthTooShort),
        (35, Error::LengthTooShort),
        (37, Error::LengthPastEnd),
        (u32::MAX, Error::LengthPastEnd),
    ] {
        let mut bytes = rsdp_v2(RSDT_AT, XSDT_AT);
        put32(&mut bytes, 20, length);
        fix_rsdp(&mut bytes);
        assert_eq!(Rsdp::read(&bytes), Err(error), "length {length}");
    }
}

#[test]
fn reads_a_table_header_and_exactly_its_bytes() {
    let mut bytes = table(b"TEST", &[1, 2, 3]);
    bytes.extend_from_slice(&[0xEE; 5]); // past the stated length
    let t = Table::read(&bytes).expect("accepted");
    assert_eq!(&t.signature(), b"TEST");
    assert_eq!(t.revision(), 3);
    assert_eq!(&t.oem_id(), b"OEMID3");
    assert_eq!(&t.oem_table_id(), b"TABLEID4");
    assert_eq!(t.oem_revision(), 0x1234_5678);
    assert_eq!(t.bytes(), &bytes[..39]);
    assert_eq!(t.body(), &[1, 2, 3]);
}

#[test]
fn a_table_of_just_a_header_has_an_empty_body() {
    let bytes = table(b"NONE", &[]);
    assert_eq!(Table::read(&bytes).map(|t| t.body().len()), Ok(0));
}

#[test]
fn rejects_a_table_shorter_than_its_header() {
    let bytes = table(b"TEST", &[1, 2, 3]);
    assert_eq!(Table::read(&bytes[..35]).err(), Some(Error::Truncated));
    for length in [0, 1, 35] {
        let mut bytes = table(b"TEST", &[1, 2, 3]);
        put32(&mut bytes, 4, length);
        fix_checksum(&mut bytes, 9);
        assert_eq!(
            Table::read(&bytes).err(),
            Some(Error::LengthTooShort),
            "length {length}"
        );
    }
}

#[test]
fn rejects_a_table_longer_than_its_bytes() {
    let bytes = table(b"TEST", &[1, 2, 3]);
    assert_eq!(Table::read(&bytes[..38]).err(), Some(Error::LengthPastEnd));
    let mut bytes = table(b"TEST", &[1, 2, 3]);
    put32(&mut bytes, 4, u32::MAX);
    fix_checksum(&mut bytes, 9);
    assert_eq!(Table::read(&bytes).err(), Some(Error::LengthPastEnd));
}

#[test]
fn rejects_a_wrong_table_checksum() {
    for at in [9, 0, 36, 38] {
        let mut bytes = table(b"TEST", &[1, 2, 3]);
        bytes[at] = bytes[at].wrapping_add(1);
        assert_eq!(
            Table::read(&bytes).err(),
            Some(Error::BadChecksum),
            "byte {at}"
        );
    }
}

#[test]
fn the_checksum_covers_only_the_stated_length() {
    let mut bytes = table(b"TEST", &[1, 2, 3]);
    bytes.push(0x42);
    assert!(Table::read(&bytes).is_ok());
}

#[test]
fn reads_an_rsdt() {
    let bytes = rsdt(&[0x1000, 0xFFFF_FFFF, 0]);
    let list = TableList::read(&bytes).expect("accepted");
    assert!(!list.is_extended());
    assert_eq!(
        list.addresses().collect::<Vec<_>>(),
        [0x1000, 0xFFFF_FFFF, 0]
    );
}

#[test]
fn reads_an_xsdt_whose_entries_are_not_aligned() {
    // One byte in front puts every 64-bit entry on an odd address.
    let mut bytes = vec![0xCC];
    bytes.extend(xsdt(&[0x1_0000_2000, u64::MAX, 7]));
    let list = TableList::read(&bytes[1..]).expect("accepted");
    assert!(list.is_extended());
    assert_eq!(
        list.addresses().collect::<Vec<_>>(),
        [0x1_0000_2000, u64::MAX, 7]
    );
}

#[test]
fn an_empty_table_list_has_no_addresses() {
    for bytes in [rsdt(&[]), xsdt(&[])] {
        let list = TableList::read(&bytes).expect("accepted");
        assert_eq!(list.addresses().count(), 0);
    }
}

#[test]
fn rejects_a_table_list_with_another_signature() {
    let bytes = table(b"RSDX", &[0; 8]);
    assert_eq!(TableList::read(&bytes).err(), Some(Error::BadSignature));
}

#[test]
fn rejects_a_table_list_that_is_not_whole_entries() {
    for (signature, size) in [(b"RSDT", 6), (b"RSDT", 1), (b"XSDT", 12), (b"XSDT", 4)] {
        let bytes = table(signature, &vec![0u8; size]);
        assert_eq!(
            TableList::read(&bytes).err(),
            Some(Error::PartialEntry),
            "{size} bytes"
        );
    }
}

#[test]
fn a_table_list_is_checked_like_any_table() {
    let mut bytes = xsdt(&[1]);
    bytes[40] ^= 1;
    assert_eq!(TableList::read(&bytes).err(), Some(Error::BadChecksum));
}

#[test]
fn reads_a_madt_with_one_entry_of_each_kind() {
    let bytes = madt(1, &every_entry());
    let madt = Madt::read(&bytes).expect("accepted");
    assert_eq!(madt.local_apic_address(), 0xFEE0_0000);
    assert!(madt.has_legacy_pics());
    assert_eq!(madt.table().bytes(), &bytes[..]);
    let entries: Vec<_> = madt.entries().collect();
    assert_eq!(
        entries,
        [
            Entry::LocalApic {
                processor_uid: 1,
                apic_id: 2,
                flags: ProcessorFlags {
                    enabled: true,
                    online_capable: false
                },
            },
            Entry::LocalApic {
                processor_uid: 3,
                apic_id: 4,
                flags: ProcessorFlags {
                    enabled: false,
                    online_capable: true
                },
            },
            Entry::IoApic {
                id: 5,
                address: 0xFEC0_0000,
                interrupt_base: 24,
            },
            Entry::InterruptOverride {
                bus: 0,
                source: 9,
                interrupt: 19,
                flags: InterruptFlags {
                    polarity: Polarity::ActiveLow,
                    trigger: Trigger::Level,
                },
            },
            Entry::LocalApicNmi {
                processor_uid: 0xFF,
                flags: InterruptFlags {
                    polarity: Polarity::ActiveHigh,
                    trigger: Trigger::Edge,
                },
                lint: 1,
            },
            Entry::LocalApicAddress {
                address: 0x1_FEE0_0000
            },
            Entry::LocalX2Apic {
                x2apic_id: 0x1_0000,
                flags: ProcessorFlags {
                    enabled: true,
                    online_capable: false
                },
                processor_uid: 0x2_0000,
            },
            Entry::Unknown {
                kind: 0x7F,
                bytes: &[0x7F, 5, 0xAA, 0xBB, 0xCC],
            },
        ]
    );
}

#[test]
fn reads_every_interrupt_flag_encoding() {
    // Section 5.2.12.5, "MPS INTI Flags": polarity in bits 0 and 1, trigger
    // mode in bits 2 and 3.
    let polarities = [
        Polarity::Conforming,
        Polarity::ActiveHigh,
        Polarity::Reserved,
        Polarity::ActiveLow,
    ];
    let triggers = [
        Trigger::Conforming,
        Trigger::Edge,
        Trigger::Reserved,
        Trigger::Level,
    ];
    for bits in 0..16u16 {
        let bytes = madt(0, &[source_override(0, 0, 0, bits | 0xFFF0)]);
        let madt = Madt::read(&bytes).expect("accepted");
        let expected = InterruptFlags {
            polarity: polarities[usize::from(bits & 3)],
            trigger: triggers[usize::from(bits >> 2)],
        };
        assert!(
            matches!(madt.entries().next(), Some(Entry::InterruptOverride { flags, .. }) if flags == expected),
            "flags {bits:#06b}"
        );
    }
}

#[test]
fn a_madt_without_legacy_pics_says_so() {
    let bytes = madt(0xFFFF_FFFE, &[]);
    let madt = Madt::read(&bytes).expect("accepted");
    assert!(!madt.has_legacy_pics());
    assert_eq!(madt.entries().count(), 0);
}

#[test]
fn a_longer_entry_than_its_type_needs_is_read() {
    // A later revision may add fields at the end of a structure.
    let mut entry = io_apic(5, 0xFEC0_0000, 24);
    entry[1] = 14;
    entry.extend_from_slice(&[0xDD, 0xDD]);
    let bytes = madt(0, &[entry, local_apic(1, 2, 1)]);
    let madt = Madt::read(&bytes).expect("accepted");
    let entries: Vec<_> = madt.entries().collect();
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        entries[0],
        Entry::IoApic {
            address: 0xFEC0_0000,
            ..
        }
    ));
    assert!(matches!(entries[1], Entry::LocalApic { apic_id: 2, .. }));
}

#[test]
fn rejects_a_madt_with_another_signature() {
    let mut bytes = madt(1, &every_entry());
    put(&mut bytes, 0, b"APIX");
    fix_checksum(&mut bytes, 9);
    assert_eq!(Madt::read(&bytes).err(), Some(Error::BadSignature));
}

#[test]
fn rejects_a_madt_too_short_for_its_own_fields() {
    let bytes = table(b"APIC", &[0; 7]);
    assert_eq!(Madt::read(&bytes).err(), Some(Error::LengthTooShort));
}

#[test]
fn a_madt_is_checked_like_any_table() {
    let mut bytes = madt(1, &every_entry());
    bytes[50] ^= 1;
    assert_eq!(Madt::read(&bytes).err(), Some(Error::BadChecksum));
}

#[test]
fn rejects_a_madt_entry_of_length_zero() {
    // The second entry starts at 44 + 8 = 52.
    // Read on a thread of its own: a reader that accepted length zero would
    // never move on.
    let bytes = madt(0, &[local_apic(0, 0, 1), vec![0, 0, 0, 0]]);
    assert_eq!(read_madt(bytes), Err(Error::EntryTooShort { offset: 52 }));
    let bytes = madt(0, &[vec![0x7F, 1]]);
    assert_eq!(read_madt(bytes), Err(Error::EntryTooShort { offset: 44 }));
}

#[test]
fn rejects_a_madt_entry_shorter_than_its_type_needs() {
    for entry in &every_entry()[..7] {
        let mut entry = entry.clone();
        entry[1] -= 1;
        entry.pop();
        let kind = entry[0];
        let bytes = madt(0, &[entry]);
        assert_eq!(
            Madt::read(&bytes).err(),
            Some(Error::EntryTooShort { offset: 44 }),
            "type {kind}"
        );
    }
}

#[test]
fn rejects_a_madt_entry_running_past_the_end() {
    let mut entry = io_apic(5, 0xFEC0_0000, 24);
    entry[1] = 13;
    let bytes = madt(0, &[local_apic(0, 0, 1), entry]);
    assert_eq!(
        Madt::read(&bytes).err(),
        Some(Error::EntryPastEnd { offset: 52 })
    );
    // A single byte left over is an entry whose length is past the end.
    let bytes = madt(0, &[local_apic(0, 0, 1), vec![1]]);
    assert_eq!(
        Madt::read(&bytes).err(),
        Some(Error::EntryPastEnd { offset: 52 })
    );
}

/// The tables QEMU generates, captured as `tests/data/qemu-q35.md`
/// describes: records of a 64-bit physical address, a 32-bit length and
/// that many bytes, all little-endian.
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

#[test]
fn reads_the_tables_qemu_generates() {
    let records = captured();
    assert_eq!(records.len(), 9);
    let at = |address: u64| {
        records
            .iter()
            .find(|(a, _)| *a == address)
            .map(|(_, bytes)| *bytes)
            .expect("captured")
    };

    // The root pointer the firmware publishes for ACPI 2.0 and later.
    let rsdp = Rsdp::read(at(0x0F77_E014)).expect("root pointer");
    assert_eq!(rsdp.revision, 2);
    assert_eq!(&rsdp.oem_id, b"BOCHS ");
    assert_eq!(rsdp.rsdt_address, 0x0F77_D074);
    assert_eq!(rsdp.xsdt_address, 0x0F77_D0E8);
    assert_eq!(rsdp.root(), Root::Xsdt(0x0F77_D0E8));

    // QEMU makes every table but the last, which the firmware adds for its
    // boot logo.
    let listed = [
        (0x0F77_9000, b"FACP", b"BOCHS "),
        (0x0F77_8000, b"APIC", b"BOCHS "),
        (0x0F77_7000, b"HPET", b"BOCHS "),
        (0x0F77_6000, b"MCFG", b"BOCHS "),
        (0x0F77_5000, b"WAET", b"BOCHS "),
        (0x0F77_4000, b"BGRT", b"INTEL "),
    ];
    let xsdt = TableList::read(at(0x0F77_D0E8)).expect("XSDT");
    assert!(xsdt.is_extended());
    let rsdt = TableList::read(at(0x0F77_D074)).expect("RSDT");
    assert!(!rsdt.is_extended());
    for list in [xsdt, rsdt] {
        let addresses: Vec<u64> = list.addresses().collect();
        assert_eq!(addresses, listed.map(|(address, _, _)| address));
    }
    for (address, signature, oem_id) in listed {
        let table = Table::read(at(address)).expect("listed table");
        assert_eq!(&table.signature(), signature);
        assert_eq!(&table.oem_id(), oem_id, "{address:#x}");
    }

    let madt = Madt::read(at(0x0F77_8000)).expect("MADT");
    assert_eq!(madt.table().revision(), 3);
    assert_eq!(madt.local_apic_address(), 0xFEE0_0000);
    assert!(madt.has_legacy_pics());
    let entries: Vec<_> = madt.entries().collect();
    let processors: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e, Entry::LocalApic { .. }))
        .collect();
    assert_eq!(
        processors,
        [&Entry::LocalApic {
            processor_uid: 0,
            apic_id: 0,
            flags: ProcessorFlags {
                enabled: true,
                online_capable: false
            },
        }]
    );
    let io_apics: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e, Entry::IoApic { .. }))
        .collect();
    assert_eq!(
        io_apics,
        [&Entry::IoApic {
            id: 0,
            address: 0xFEC0_0000,
            interrupt_base: 0,
        }]
    );
    let edge = InterruptFlags {
        polarity: Polarity::Conforming,
        trigger: Trigger::Conforming,
    };
    let level_high = InterruptFlags {
        polarity: Polarity::ActiveHigh,
        trigger: Trigger::Level,
    };
    let overrides: Vec<_> = entries
        .iter()
        .filter_map(|e| match *e {
            Entry::InterruptOverride {
                bus,
                source,
                interrupt,
                flags,
            } => Some((bus, source, interrupt, flags)),
            _ => None,
        })
        .collect();
    // The timer on ISA line 0 moved to 2; lines 5, 9, 10 and 11 made
    // level-triggered and active high.
    assert_eq!(
        overrides,
        [
            (0, 0, 2, edge),
            (0, 5, 5, level_high),
            (0, 9, 9, level_high),
            (0, 10, 10, level_high),
            (0, 11, 11, level_high),
        ]
    );
    assert_eq!(
        entries.last(),
        Some(&Entry::LocalApicNmi {
            processor_uid: 0xFF,
            flags: edge,
            lint: 1,
        })
    );
    assert_eq!(entries.len(), 8);
}

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
/// and each iterator ends. Returns how many readers accepted the bytes.
fn read_everything(bytes: &[u8]) -> usize {
    let mut accepted = 0;
    if let Ok(rsdp) = Rsdp::read(bytes) {
        accepted += 1;
        match rsdp.root() {
            Root::Rsdt(address) => assert_ne!(address, 0),
            Root::Xsdt(address) => assert_ne!(address, 0),
        }
    }
    if let Ok(table) = Table::read(bytes) {
        accepted += 1;
        assert!(inside(bytes, table.bytes()));
        assert!(inside(table.bytes(), table.body()));
        assert!(table.bytes().len() >= 36);
    }
    if let Ok(list) = TableList::read(bytes) {
        accepted += 1;
        // Each address takes at least four bytes of the table.
        let count = list.addresses().take(bytes.len() + 1).count();
        assert!(count <= bytes.len() / 4, "{count} addresses");
    }
    if let Ok(madt) = Madt::read(bytes) {
        accepted += 1;
        assert!(inside(bytes, madt.table().bytes()));
        // Each entry takes at least two bytes of the table, so an iterator
        // that has not ended by then never will.
        let mut count = 0;
        for entry in madt.entries().take(bytes.len() + 1) {
            count += 1;
            if let Entry::Unknown { bytes: own, .. } = entry {
                assert!(inside(madt.table().bytes(), own));
                assert!(own.len() >= 2);
            }
        }
        assert!(count <= bytes.len() / 2, "{count} entries");
    }
    accepted
}

/// Damages each kind of structure many thousands of times, with the
/// checksums sometimes recomputed so the damage reaches the checks behind
/// them, and checks that nothing panics, every view stays inside its bytes
/// and every iterator ends. It runs on a thread of its own, so a reader
/// that loops forever fails the test.
#[test]
fn damaged_tables_are_rejected_or_stay_inside_their_bytes() {
    let (accepted, rejected, madts) = ends(300, damage_many_times);
    println!("{accepted} accepted, {rejected} rejected, {madts} MADTs read");
    // Both outcomes must be common, or the test explores too little.
    assert!(
        accepted > 5_000 && rejected > 5_000 && madts > 1_000,
        "{accepted} accepted, {rejected} rejected, {madts} MADTs read"
    );
}

/// The cases of the test above: how many some reader accepted, how many
/// every reader rejected, and how many MADTs were read.
fn damage_many_times() -> (usize, usize, usize) {
    let madt_entries = every_entry();
    let bases: [(Vec<u8>, Vec<usize>); 4] = [
        // The root pointer: revision, the RSDT address, length, the XSDT
        // address.
        (rsdp_v2(RSDT_AT, XSDT_AT), vec![15, 16, 20, 24]),
        (rsdt(&[1, 2, 3]), vec![4]),
        (xsdt(&[1, 2, 3]), vec![4]),
        // The MADT: its length, and each entry's length byte.
        (madt(1, &madt_entries), {
            let mut at = 44;
            let mut fields = vec![4];
            for entry in &madt_entries {
                fields.push(at + 1);
                at += entry.len();
            }
            fields
        }),
    ];
    let edges: [u64; 10] = [0, 1, 2, 5, 6, 35, 36, 44, 0xFF, u64::MAX];

    let mut random = Random(0xAC91_0000_5EED_0001);
    let (mut accepted, mut rejected) = (0, 0);
    let mut madts = 0;
    for case in 0..60_000 {
        let (base, fields) = &bases[case % bases.len()];
        let mut bytes = base.clone();
        for _ in 0..1 + random.below(3) {
            match random.below(4) {
                // Any byte becomes any value.
                0 => {
                    let at = random.below(bytes.len());
                    bytes[at] = random.next() as u8;
                }
                // A length or count field becomes a value near an edge, or
                // near the length of the bytes.
                1 | 2 => {
                    let at = fields[random.below(fields.len())];
                    let value = match random.below(3) {
                        0 => bytes.len() as u64,
                        _ => edges[random.below(edges.len())],
                    };
                    let value = value.wrapping_add(random.below(3) as u64).wrapping_sub(1);
                    // Entry lengths are one byte; others are four.
                    if at == 4 || at == 20 || at == 16 || at == 24 {
                        put32(&mut bytes, at, value as u32);
                    } else {
                        bytes[at] = value as u8;
                    }
                }
                // Bytes are added at the end.
                _ => {
                    for _ in 0..random.below(8) {
                        bytes.push(random.next() as u8);
                    }
                }
            }
        }
        // Usually the checksums are recomputed, so the damage gets past them.
        if random.below(4) != 0 {
            if case % bases.len() == 0 {
                if bytes.len() >= 20 {
                    fix_rsdp(&mut bytes);
                }
            } else {
                let stated = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
                let end = stated.clamp(10, bytes.len());
                fix_checksum(&mut bytes[..end], 9);
            }
        }
        // Sometimes the bytes are also cut short.
        if random.below(4) == 0 {
            bytes.truncate(random.below(bytes.len() + 1));
        }
        let count = read_everything(&bytes);
        if count == 0 {
            rejected += 1;
        } else {
            accepted += 1;
        }
        if case % bases.len() == 3 && Madt::read(&bytes).is_ok() {
            madts += 1;
        }
    }
    (accepted, rejected, madts)
}

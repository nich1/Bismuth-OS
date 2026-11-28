//! Tests for the hand-off structure. Structures for the reader are written
//! byte by byte from the layout in `docs/handoff.md` by [`encode`], not by
//! the builder, so a test failing means the code and the documented layout
//! disagree, not that two copies of the same code do.

// Building test inputs indexes and adds freely; the crate's lints are for the
// library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use std::collections::BTreeMap;

const PAGE: u64 = PAGE_SIZE;

// UEFI 2.10, section 7.2, `EFI_MEMORY_TYPE`: the values the tests use by name.
const RESERVED_TYPE: u32 = 0;
const LOADER_CODE: u32 = 1;
const LOADER_DATA: u32 = 2;
const BOOT_CODE: u32 = 3;
const BOOT_DATA: u32 = 4;
const CONVENTIONAL: u32 = 7;
const ACPI_RECLAIM: u32 = 9;

/// A region by page numbers rather than addresses.
fn at(first_page: u64, pages: u64, kind: Kind) -> Region {
    Region {
        start: first_page * PAGE,
        pages,
        kind,
    }
}

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

/// One region as the layout stores it: start address, pages, kind number.
type Raw = (u64, u64, u32);

/// Writes a structure by hand from the documented layout: the header, then
/// the regions from `header_size` on, then `trailing` bytes of later fields,
/// all counted in the stated length.
fn encode(major: u16, minor: u16, header_size: u32, regions: &[Raw], trailing: usize) -> Vec<u8> {
    let length = header_size as usize + 24 * regions.len() + trailing;
    let mut bytes = vec![0xEE; length.max(24)];
    put(&mut bytes, 0, b"BISMHAND");
    put16(&mut bytes, 8, major);
    put16(&mut bytes, 10, minor);
    put32(&mut bytes, 12, length as u32);
    put32(&mut bytes, 16, header_size);
    put32(&mut bytes, 20, regions.len() as u32);
    for (i, &(start, pages, kind)) in regions.iter().enumerate() {
        let entry = header_size as usize + 24 * i;
        put64(&mut bytes, entry, start);
        put64(&mut bytes, entry + 8, pages);
        put32(&mut bytes, entry + 16, kind);
        put32(&mut bytes, entry + 20, 0);
    }
    bytes
}

/// Three regions of version 1.0, as the reader tests start from.
fn three() -> Vec<u8> {
    encode(
        1,
        0,
        24,
        &[(0, 16, 1), (16 * PAGE, 4, 2), (0x10_0000, 0x100, 1)],
        0,
    )
}

fn refused(bytes: &[u8]) -> ReadError {
    Handoff::read(bytes).expect_err("the structure is refused")
}

fn read_back(bytes: &[u8]) -> Vec<Region> {
    Handoff::read(bytes)
        .expect("the structure is accepted")
        .regions()
        .collect()
}

/// What the builder writes, cut to the length it reports.
fn written(builder: &mut Builder) -> Vec<u8> {
    let mut out = vec![0u8; 4096];
    let length = builder.write(&mut out).expect("the structure is written");
    out.truncate(length);
    out
}

/// What the builder writes, read back.
fn finish(builder: &mut Builder) -> Vec<Region> {
    read_back(&written(builder))
}

// ---------------------------------------------------------------------------
// Firmware memory types (UEFI 2.10, section 7.2, `EFI_MEMORY_TYPE`).

macro_rules! firmware_type {
    ($($name:ident: $number:expr => $kind:ident,)*) => {
        $(
            #[test]
            fn $name() {
                assert_eq!(Kind::from_firmware_type($number), Kind::$kind);
            }
        )*
    };
}

firmware_type! {
    reserved_memory_type_is_reserved: 0 => Reserved,
    loader_code_is_loader: 1 => Loader,
    loader_data_is_loader: 2 => Loader,
    boot_services_code_is_boot_services: 3 => BootServices,
    boot_services_data_is_boot_services: 4 => BootServices,
    runtime_services_code_is_firmware_runtime: 5 => FirmwareRuntime,
    runtime_services_data_is_firmware_runtime: 6 => FirmwareRuntime,
    conventional_memory_is_usable: 7 => Usable,
    unusable_memory_is_reserved: 8 => Reserved,
    acpi_reclaim_memory_is_acpi_reclaimable: 9 => AcpiReclaimable,
    acpi_memory_nvs_is_acpi_preserved: 10 => AcpiPreserved,
    memory_mapped_io_is_device: 11 => Device,
    memory_mapped_io_port_space_is_device: 12 => Device,
    pal_code_is_reserved: 13 => Reserved,
    persistent_memory_is_persistent: 14 => Persistent,
    unaccepted_memory_is_reserved: 15 => Reserved,
    max_memory_type_is_reserved: 16 => Reserved,
    first_unassigned_number_is_reserved: 17 => Reserved,
    last_unassigned_number_is_reserved: 0x6FFF_FFFF => Reserved,
    first_oem_number_is_reserved: 0x7000_0000 => Reserved,
    last_oem_number_is_reserved: 0x7FFF_FFFF => Reserved,
    first_operating_system_number_is_reserved: 0x8000_0000 => Reserved,
    last_operating_system_number_is_reserved: 0xFFFF_FFFF => Reserved,
}

#[test]
fn only_conventional_memory_is_usable() {
    let mut random = Random(0x0DD_F1AE_0000_0001);
    let samples = (0..=0x1_0000u32)
        .chain((0..100_000).map(|_| random.next() as u32))
        .chain([0x6FFF_FFFF, 0x7000_0000, 0x7FFF_FFFF, 0x8000_0000, u32::MAX]);
    for number in samples {
        let kind = Kind::from_firmware_type(number);
        assert_eq!(kind == Kind::Usable, number == 7, "type {number:#x}");
    }
}

#[test]
fn kind_numbers_round_trip_and_unknown_numbers_are_reserved() {
    let kinds = [
        Kind::Usable,
        Kind::Kernel,
        Kind::BootServices,
        Kind::Loader,
        Kind::FirmwareRuntime,
        Kind::AcpiReclaimable,
        Kind::AcpiPreserved,
        Kind::Device,
        Kind::Persistent,
        Kind::Reserved,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        assert_eq!(kind.number(), i as u32 + 1);
        assert_eq!(Kind::from_number(kind.number()), kind);
    }
    for unknown in [0, 11, 0x100, u32::MAX] {
        assert_eq!(Kind::from_number(unknown), Kind::Reserved, "{unknown}");
    }
}

// ---------------------------------------------------------------------------
// The builder.

#[test]
fn an_empty_map_is_written_with_no_regions() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    assert_eq!(written(&mut builder), encode(1, 0, 24, &[], 0));
}

#[test]
fn the_written_layout_is_the_documented_one() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 0, 16).unwrap();
    builder.add_firmware(LOADER_DATA, 16 * PAGE, 4).unwrap();
    builder
        .add_firmware(ACPI_RECLAIM, 0x10_0000, 0x100)
        .unwrap();
    let expected = encode(
        1,
        0,
        24,
        &[(0, 16, 1), (16 * PAGE, 4, 4), (0x10_0000, 0x100, 6)],
        0,
    );
    assert_eq!(written(&mut builder), expected);
}

#[test]
fn setting_aside_from_the_middle_of_a_region() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(12 * PAGE, 3).unwrap();
    assert_eq!(
        finish(&mut builder),
        [
            at(10, 2, Kind::Loader),
            at(12, 3, Kind::Kernel),
            at(15, 5, Kind::Loader)
        ]
    );
}

#[test]
fn setting_aside_from_the_start_of_a_region() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(10 * PAGE, 3).unwrap();
    assert_eq!(
        finish(&mut builder),
        [at(10, 3, Kind::Kernel), at(13, 7, Kind::Loader)]
    );
}

#[test]
fn setting_aside_from_the_end_of_a_region() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(17 * PAGE, 3).unwrap();
    assert_eq!(
        finish(&mut builder),
        [at(10, 7, Kind::Loader), at(17, 3, Kind::Kernel)]
    );
}

#[test]
fn setting_aside_a_whole_region() {
    let mut space = [Region::default(); 1];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_CODE, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(10 * PAGE, 10).unwrap();
    assert_eq!(finish(&mut builder), [at(10, 10, Kind::Kernel)]);
}

#[test]
fn setting_aside_across_several_regions() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    // Out of order, with the boot stage's code and data next to each other.
    builder.add_firmware(LOADER_DATA, 20 * PAGE, 10).unwrap();
    builder.add_firmware(LOADER_CODE, 10 * PAGE, 4).unwrap();
    builder.add_firmware(CONVENTIONAL, 40 * PAGE, 5).unwrap();
    builder.add_firmware(LOADER_DATA, 14 * PAGE, 6).unwrap();
    builder.set_aside_for_kernel(12 * PAGE, 13).unwrap();
    assert_eq!(
        finish(&mut builder),
        [
            at(10, 2, Kind::Loader),
            at(12, 13, Kind::Kernel),
            at(25, 5, Kind::Loader),
            at(40, 5, Kind::Usable),
        ]
    );
}

#[test]
fn neighbouring_set_asides_are_merged() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(14 * PAGE, 2).unwrap();
    builder.set_aside_for_kernel(12 * PAGE, 2).unwrap();
    assert_eq!(
        finish(&mut builder),
        [
            at(10, 2, Kind::Loader),
            at(12, 4, Kind::Kernel),
            at(16, 4, Kind::Loader)
        ]
    );
}

#[test]
fn neighbours_of_the_same_kind_are_merged_and_no_others() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 0, 4).unwrap();
    builder.add_firmware(CONVENTIONAL, 4 * PAGE, 4).unwrap();
    // Two firmware types of one kind merge too.
    builder.add_firmware(BOOT_CODE, 8 * PAGE, 1).unwrap();
    builder.add_firmware(BOOT_DATA, 9 * PAGE, 1).unwrap();
    // The same kind after a gap does not.
    builder.add_firmware(BOOT_DATA, 11 * PAGE, 1).unwrap();
    // Nor a different kind without one.
    builder.add_firmware(CONVENTIONAL, 12 * PAGE, 1).unwrap();
    assert_eq!(
        finish(&mut builder),
        [
            at(0, 8, Kind::Usable),
            at(8, 2, Kind::BootServices),
            at(11, 1, Kind::BootServices),
            at(12, 1, Kind::Usable),
        ]
    );
}

#[test]
fn unsorted_input_is_written_sorted() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 300 * PAGE, 1).unwrap();
    builder.add_firmware(RESERVED_TYPE, 100 * PAGE, 1).unwrap();
    builder.add_firmware(ACPI_RECLAIM, 200 * PAGE, 1).unwrap();
    builder.add_firmware(BOOT_DATA, 0, 1).unwrap();
    assert_eq!(
        finish(&mut builder),
        [
            at(0, 1, Kind::BootServices),
            at(100, 1, Kind::Reserved),
            at(200, 1, Kind::AcpiReclaimable),
            at(300, 1, Kind::Usable),
        ]
    );
}

#[test]
fn writing_twice_gives_the_same_structure() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 4 * PAGE, 4).unwrap();
    builder.add_firmware(CONVENTIONAL, 0, 4).unwrap();
    let first = written(&mut builder);
    assert_eq!(written(&mut builder), first);
}

#[test]
fn overlapping_descriptors_are_refused() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 10 * PAGE, 10).unwrap();
    for (first, pages) in [(5, 6), (19, 3), (12, 2), (5, 20), (10, 10), (19, 1)] {
        assert_eq!(
            builder.add_firmware(LOADER_DATA, first * PAGE, pages),
            Err(BuildError::Overlap),
            "pages {first}..{}",
            first + pages
        );
    }
    // Touching on either side is not overlapping.
    builder.add_firmware(LOADER_DATA, 5 * PAGE, 5).unwrap();
    builder.add_firmware(LOADER_DATA, 20 * PAGE, 5).unwrap();
    assert_eq!(
        finish(&mut builder),
        [
            at(5, 5, Kind::Loader),
            at(10, 10, Kind::Usable),
            at(20, 5, Kind::Loader)
        ]
    );
}

#[test]
fn a_descriptor_overlapping_a_set_aside_range_is_refused() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(12 * PAGE, 2).unwrap();
    assert_eq!(
        builder.add_firmware(CONVENTIONAL, 13 * PAGE, 1),
        Err(BuildError::Overlap)
    );
}

#[test]
fn a_descriptor_with_no_pages_is_skipped() {
    let mut space = [Region::default(); 1];
    let mut builder = Builder::new(&mut space);
    // Skipped before anything else is checked, so even these are fine.
    builder.add_firmware(CONVENTIONAL, 0x123, 0).unwrap();
    builder.add_firmware(CONVENTIONAL, u64::MAX, 0).unwrap();
    builder.add_firmware(CONVENTIONAL, 0, 1).unwrap();
    // It takes no space and does not overlap.
    builder.add_firmware(CONVENTIONAL, 0, 0).unwrap();
    assert_eq!(finish(&mut builder), [at(0, 1, Kind::Usable)]);
}

#[test]
fn an_unaligned_start_is_refused() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    for start in [1, 0x800, 0xFFF, 0x1001] {
        assert_eq!(
            builder.add_firmware(CONVENTIONAL, start, 1),
            Err(BuildError::Unaligned),
            "{start:#x}"
        );
    }
    builder.add_firmware(LOADER_DATA, 0, 4).unwrap();
    assert_eq!(
        builder.set_aside_for_kernel(0x800, 1),
        Err(BuildError::Unaligned)
    );
    assert_eq!(finish(&mut builder), [at(0, 4, Kind::Loader)]);
}

#[test]
fn a_region_past_the_physical_limit_is_refused() {
    let top = PHYSICAL_LIMIT / PAGE;
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    let cases = [
        // Ends one page past 2^52.
        (PHYSICAL_LIMIT - PAGE, 2),
        // Starts at 2^52.
        (PHYSICAL_LIMIT, 1),
        // The end does not fit in 64 bits.
        (u64::MAX - 0xFFF, 1),
        // The length does not fit in 64 bits.
        (0, u64::MAX),
        (0, u64::MAX / PAGE + 1),
        // Large but representable.
        (0, top + 1),
    ];
    for (start, pages) in cases {
        assert_eq!(
            builder.add_firmware(CONVENTIONAL, start, pages),
            Err(BuildError::BeyondPhysicalLimit),
            "{start:#x} + {pages:#x} pages"
        );
    }
    // Ending exactly at 2^52 is accepted.
    builder
        .add_firmware(LOADER_DATA, PHYSICAL_LIMIT - 4 * PAGE, 4)
        .unwrap();
    for (start, pages) in cases {
        assert_eq!(
            builder.set_aside_for_kernel(start, pages),
            Err(BuildError::BeyondPhysicalLimit),
            "{start:#x} + {pages:#x} pages"
        );
    }
    builder
        .set_aside_for_kernel(PHYSICAL_LIMIT - PAGE, 1)
        .unwrap();
    assert_eq!(
        finish(&mut builder),
        [at(top - 4, 3, Kind::Loader), at(top - 1, 1, Kind::Kernel)]
    );
}

#[test]
fn setting_aside_nothing_is_refused() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 0, 4).unwrap();
    assert_eq!(builder.set_aside_for_kernel(0, 0), Err(BuildError::Empty));
    // Even with a start that would be refused otherwise.
    assert_eq!(builder.set_aside_for_kernel(1, 0), Err(BuildError::Empty));
}

#[test]
fn setting_aside_memory_that_is_not_the_loaders_is_refused() {
    let mut space = [Region::default(); 8];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.add_firmware(CONVENTIONAL, 20 * PAGE, 10).unwrap();
    builder.add_firmware(BOOT_DATA, 30 * PAGE, 10).unwrap();
    builder.add_firmware(LOADER_DATA, 41 * PAGE, 4).unwrap();
    builder.set_aside_for_kernel(10 * PAGE, 2).unwrap();
    let before = finish(&mut builder);
    let cases = [
        // Usable memory, which the firmware may still hand out.
        (20, 2),
        // Boot services memory.
        (30, 2),
        // Loader memory running on into usable memory.
        (18, 4),
        // Memory no descriptor covers, below, between and after.
        (0, 2),
        (8, 4),
        (40, 2),
        (44, 2),
        (100, 1),
        // Two loader regions with a page of nothing between them.
        (39, 3),
        // Already set aside, in part or whole.
        (10, 2),
        (11, 2),
    ];
    for (first, pages) in cases {
        assert_eq!(
            builder.set_aside_for_kernel(first * PAGE, pages),
            Err(BuildError::NotLoaderMemory),
            "pages {first}..{}",
            first + pages
        );
    }
    // A refusal changes nothing.
    assert_eq!(finish(&mut builder), before);
}

#[test]
fn adding_to_exactly_full_space_and_one_over() {
    let mut space = [Region::default(); 3];
    let mut builder = Builder::new(&mut space);
    for i in 0..3 {
        builder.add_firmware(CONVENTIONAL, 2 * i * PAGE, 1).unwrap();
    }
    assert_eq!(
        builder.add_firmware(CONVENTIONAL, 10 * PAGE, 1),
        Err(BuildError::Full)
    );
    assert_eq!(
        finish(&mut builder),
        [
            at(0, 1, Kind::Usable),
            at(2, 1, Kind::Usable),
            at(4, 1, Kind::Usable)
        ]
    );
}

#[test]
fn no_space_at_all_is_full_at_once() {
    let mut builder = Builder::new(&mut []);
    assert_eq!(
        builder.add_firmware(CONVENTIONAL, 0, 1),
        Err(BuildError::Full)
    );
    // A descriptor with no pages needs no space.
    builder.add_firmware(CONVENTIONAL, 0, 0).unwrap();
    assert_eq!(finish(&mut builder), []);
}

#[test]
fn setting_aside_needs_space_for_the_pieces_left_over() {
    // From the middle: two more regions. Exactly enough, then one short.
    let mut space = [Region::default(); 3];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    builder.set_aside_for_kernel(12 * PAGE, 1).unwrap();
    assert_eq!(finish(&mut builder).len(), 3);

    let mut space = [Region::default(); 2];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(LOADER_DATA, 10 * PAGE, 10).unwrap();
    assert_eq!(
        builder.set_aside_for_kernel(12 * PAGE, 1),
        Err(BuildError::Full)
    );
    // Nothing was changed by the refusal.
    assert_eq!(finish(&mut builder), [at(10, 10, Kind::Loader)]);
    // From the start: one more region, which fits.
    builder.set_aside_for_kernel(10 * PAGE, 1).unwrap();
    // Now full; the whole of a region needs no more space.
    assert_eq!(
        builder.set_aside_for_kernel(12 * PAGE, 1),
        Err(BuildError::Full)
    );
    builder.set_aside_for_kernel(11 * PAGE, 9).unwrap();
    assert_eq!(finish(&mut builder), [at(10, 10, Kind::Kernel)]);
}

#[test]
fn writing_into_exactly_enough_bytes_and_one_short() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 0, 1).unwrap();
    builder.add_firmware(CONVENTIONAL, 4 * PAGE, 1).unwrap();
    // Merged neighbours take one entry, so the size counts the merged list.
    builder.add_firmware(CONVENTIONAL, PAGE, 1).unwrap();
    let needed = 24 + 2 * 24;
    let mut out = vec![0u8; needed - 1];
    assert_eq!(
        builder.write(&mut out),
        Err(BuildError::OutputTooSmall { needed })
    );
    let mut out = vec![0u8; needed];
    assert_eq!(builder.write(&mut out), Ok(needed));
    assert_eq!(
        read_back(&out),
        [at(0, 2, Kind::Usable), at(4, 1, Kind::Usable)]
    );
    // Too few bytes for even the header.
    assert_eq!(
        builder.write(&mut [0u8; 3]),
        Err(BuildError::OutputTooSmall { needed })
    );
}

#[test]
fn bytes_past_the_written_length_are_left_alone() {
    let mut space = [Region::default(); 4];
    let mut builder = Builder::new(&mut space);
    builder.add_firmware(CONVENTIONAL, 0, 1).unwrap();
    let mut out = vec![0xAB; 100];
    assert_eq!(builder.write(&mut out), Ok(48));
    assert!(out[48..].iter().all(|&b| b == 0xAB));
    // The reader takes the stated length, not the buffer's.
    assert_eq!(read_back(&out), [at(0, 1, Kind::Usable)]);
}

// ---------------------------------------------------------------------------
// The reader.

#[test]
fn reads_a_valid_structure() {
    let bytes = three();
    let handoff = Handoff::read(&bytes).expect("accepted");
    assert_eq!(
        handoff.regions().collect::<Vec<_>>(),
        [
            at(0, 16, Kind::Usable),
            at(16, 4, Kind::Kernel),
            at(256, 0x100, Kind::Usable)
        ]
    );
    assert_eq!(handoff.usable_pages(), 16 + 0x100);
}

#[test]
fn usable_pages_count_only_usable_memory() {
    let kinds = (1..=12).collect::<Vec<u32>>();
    let regions: Vec<Raw> = kinds
        .iter()
        .map(|&k| (u64::from(k) * 0x10_0000, u64::from(k), k))
        .collect();
    let bytes = encode(1, 0, 24, &regions, 0);
    let handoff = Handoff::read(&bytes).expect("accepted");
    assert_eq!(handoff.usable_pages(), 1);
    assert_eq!(handoff.regions().count(), 12);
}

#[test]
fn an_unknown_kind_reads_as_reserved_and_padding_is_ignored() {
    let mut bytes = encode(1, 0, 24, &[(0, 1, 99), (PAGE, 1, 1)], 0);
    put32(&mut bytes, 24 + 20, 0xFFFF_FFFF);
    assert_eq!(
        read_back(&bytes),
        [at(0, 1, Kind::Reserved), at(1, 1, Kind::Usable)]
    );
}

#[test]
fn refuses_too_few_bytes_for_the_header() {
    assert_eq!(refused(&[]), ReadError::Truncated);
    assert_eq!(refused(&three()[..23]), ReadError::Truncated);
    assert!(Handoff::read(&encode(1, 0, 24, &[], 0)).is_ok());
}

#[test]
fn refuses_a_wrong_marker() {
    for at in 0..8 {
        let mut bytes = three();
        bytes[at] ^= 0x20;
        assert_eq!(refused(&bytes), ReadError::BadMarker, "byte {at}");
    }
}

#[test]
fn refuses_another_major_version() {
    for (major, minor) in [(0, 0), (2, 0), (2, 5), (u16::MAX, u16::MAX)] {
        let mut bytes = three();
        put16(&mut bytes, 8, major);
        put16(&mut bytes, 10, minor);
        assert_eq!(
            refused(&bytes),
            ReadError::UnsupportedVersion { major, minor }
        );
    }
}

#[test]
fn accepts_a_newer_minor_version_and_skips_what_it_added() {
    // Version 1.7 with eight more header bytes and sixteen bytes of later
    // fields after the regions.
    let bytes = encode(1, 7, 32, &[(0, 2, 1), (4 * PAGE, 1, 2)], 16);
    assert_eq!(
        read_back(&bytes),
        [at(0, 2, Kind::Usable), at(4, 1, Kind::Kernel)]
    );
    assert!(Handoff::read(&encode(1, u16::MAX, 24, &[], 0)).is_ok());
}

#[test]
fn refuses_a_length_past_the_bytes() {
    let mut bytes = three();
    let length = bytes.len() as u32;
    put32(&mut bytes, 12, length + 1);
    assert_eq!(refused(&bytes), ReadError::LengthPastEnd);
    put32(&mut bytes, 12, u32::MAX);
    assert_eq!(refused(&bytes), ReadError::LengthPastEnd);
    // Bytes past the stated length are not read.
    let mut bytes = three();
    bytes.extend([0xFF; 30]);
    assert_eq!(read_back(&bytes).len(), 3);
}

#[test]
fn refuses_a_header_size_that_does_not_fit() {
    for size in [0, 1, 23] {
        let mut bytes = three();
        put32(&mut bytes, 16, size);
        assert_eq!(refused(&bytes), ReadError::BadHeaderSize, "{size}");
    }
    // Past the stated length, though inside the bytes.
    let mut bytes = encode(1, 0, 24, &[], 0);
    bytes.extend([0; 8]);
    put32(&mut bytes, 16, 25);
    assert_eq!(refused(&bytes), ReadError::BadHeaderSize);
    put32(&mut bytes, 16, u32::MAX);
    assert_eq!(refused(&bytes), ReadError::BadHeaderSize);
    // A header as long as the whole structure, with no regions.
    put32(&mut bytes, 12, 32);
    put32(&mut bytes, 16, 32);
    assert!(Handoff::read(&bytes).is_ok());
}

#[test]
fn refuses_regions_past_the_stated_length() {
    let mut bytes = three();
    put32(&mut bytes, 20, 4);
    assert_eq!(refused(&bytes), ReadError::RegionsPastLength);
    put32(&mut bytes, 20, u32::MAX);
    assert_eq!(refused(&bytes), ReadError::RegionsPastLength);
    // Inside the bytes but past the stated length.
    let mut bytes = three();
    let length = bytes.len() as u32;
    // A fourth entry, after the third.
    bytes.extend([0; 24]);
    put64(&mut bytes, 96, 0x20_0000);
    put64(&mut bytes, 104, 1);
    put32(&mut bytes, 112, 1);
    put32(&mut bytes, 20, 4);
    assert_eq!(refused(&bytes), ReadError::RegionsPastLength);
    put32(&mut bytes, 12, length + 24);
    assert!(Handoff::read(&bytes).is_ok());
    // Fewer regions than fit is fine: the rest is later fields.
    assert_eq!(read_back(&bytes).len(), 4);
    put32(&mut bytes, 20, 1);
    assert_eq!(read_back(&bytes).len(), 1);
}

#[test]
fn refuses_a_region_that_is_not_whole_pages_below_the_limit() {
    let bad: [(u64, u64); 6] = [
        (0x800, 1),
        (PAGE, 0),
        (PHYSICAL_LIMIT - PAGE, 2),
        (PHYSICAL_LIMIT, 1),
        (u64::MAX - 0xFFF, 1),
        (PAGE, u64::MAX),
    ];
    for (start, pages) in bad {
        let bytes = encode(1, 0, 24, &[(0, 1, 1), (start, pages, 1)], 0);
        assert_eq!(
            refused(&bytes),
            ReadError::BadRegion { index: 1 },
            "{start:#x} + {pages:#x} pages"
        );
    }
    let bytes = encode(1, 0, 24, &[(PHYSICAL_LIMIT - PAGE, 1, 1)], 0);
    assert!(Handoff::read(&bytes).is_ok());
}

#[test]
fn refuses_regions_out_of_order_or_overlapping() {
    let cases: [[Raw; 3]; 4] = [
        // Out of order.
        [(0, 1, 1), (8 * PAGE, 1, 1), (4 * PAGE, 1, 1)],
        // Overlapping the one before.
        [(0, 1, 1), (4 * PAGE, 4, 1), (7 * PAGE, 2, 1)],
        // Starting at the same address.
        [(0, 1, 1), (4 * PAGE, 1, 1), (4 * PAGE, 1, 2)],
        // Inside the one before.
        [(0, 1, 1), (4 * PAGE, 8, 1), (5 * PAGE, 1, 2)],
    ];
    for regions in cases {
        let bytes = encode(1, 0, 24, &regions, 0);
        assert_eq!(
            refused(&bytes),
            ReadError::NotSorted { index: 2 },
            "{regions:x?}"
        );
    }
    // Touching, and of the same kind, is accepted: only the builder merges.
    let bytes = encode(1, 0, 24, &[(0, 4, 1), (4 * PAGE, 4, 1)], 0);
    assert_eq!(read_back(&bytes).len(), 2);
}

// ---------------------------------------------------------------------------
// Randomised tests.

/// A tiny xorshift generator (Marsaglia, 2003) so the randomised tests are the
/// same on every run without a dependency.
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

/// A firmware type and the kind the model expects for it, weighted towards
/// the loader's memory so that set-asides often succeed.
const TYPES: [(u32, Kind); 9] = [
    (LOADER_CODE, Kind::Loader),
    (LOADER_DATA, Kind::Loader),
    (LOADER_DATA, Kind::Loader),
    (CONVENTIONAL, Kind::Usable),
    (CONVENTIONAL, Kind::Usable),
    (BOOT_DATA, Kind::BootServices),
    (RESERVED_TYPE, Kind::Reserved),
    (ACPI_RECLAIM, Kind::AcpiReclaimable),
    (0x8000_0001, Kind::Reserved),
];

/// A start address and page count: mostly whole pages among the first 96,
/// sometimes an unaligned start, a range near or past 2^52, or a page count
/// whose length does not fit in 64 bits.
fn random_range(random: &mut Random) -> (u64, u64) {
    match random.below(16) {
        0 => (
            random.below(96) as u64 * PAGE + 1 + random.below(4095) as u64,
            1 + random.below(4) as u64,
        ),
        1 => (
            PHYSICAL_LIMIT - random.below(4) as u64 * PAGE,
            random.below(5) as u64,
        ),
        2 => (
            random.below(96) as u64 * PAGE,
            u64::MAX - random.below(2) as u64,
        ),
        _ => (random.below(96) as u64 * PAGE, random.below(25) as u64),
    }
}

/// The model of a map: the kind of each page the firmware described.
type Model = BTreeMap<u64, Kind>;

/// The page numbers of a range the model accepts, or the error it predicts
/// for the checks every range goes through.
fn model_pages(start: u64, pages: u64) -> Result<std::ops::Range<u64>, BuildError> {
    if !start.is_multiple_of(PAGE) {
        return Err(BuildError::Unaligned);
    }
    let end = u128::from(start) + u128::from(pages) * u128::from(PAGE);
    if end > u128::from(PHYSICAL_LIMIT) {
        return Err(BuildError::BeyondPhysicalLimit);
    }
    Ok(start / PAGE..start / PAGE + pages)
}

fn model_add(model: &mut Model, kind: Kind, start: u64, pages: u64) -> Result<(), BuildError> {
    if pages == 0 {
        return Ok(());
    }
    let range = model_pages(start, pages)?;
    if range.clone().any(|page| model.contains_key(&page)) {
        return Err(BuildError::Overlap);
    }
    model.extend(range.map(|page| (page, kind)));
    Ok(())
}

fn model_set_aside(model: &mut Model, start: u64, pages: u64) -> Result<(), BuildError> {
    if pages == 0 {
        return Err(BuildError::Empty);
    }
    let range = model_pages(start, pages)?;
    if range
        .clone()
        .any(|page| model.get(&page) != Some(&Kind::Loader))
    {
        return Err(BuildError::NotLoaderMemory);
    }
    model.extend(range.map(|page| (page, Kind::Kernel)));
    Ok(())
}

/// Random firmware maps and set-asides, each operation's result compared with
/// the model's prediction, then the written structure compared with the model
/// page by page.
#[test]
fn random_maps_match_a_page_by_page_model() {
    let mut random = Random(0x5EED_4A4D_0FF0_0001);
    let (mut set_aside, mut refused_ops) = (0, 0);
    for case in 0..20_000 {
        // At most 16 operations, each adding at most two regions, so the
        // space never runs out; running out is tested on its own above.
        let mut space = [Region::default(); 32];
        let mut builder = Builder::new(&mut space);
        let mut model = Model::new();
        for _ in 0..1 + random.below(16) {
            let (start, pages) = random_range(&mut random);
            let (got, expected) = if random.below(3) == 0 {
                // Often a range inside the model's loader memory.
                let loader: Vec<u64> = model
                    .iter()
                    .filter(|(_, kind)| **kind == Kind::Loader)
                    .map(|(page, _)| *page)
                    .collect();
                let (start, pages) = if !loader.is_empty() && random.below(2) == 0 {
                    let first = loader[random.below(loader.len())];
                    (first * PAGE, 1 + random.below(6) as u64)
                } else {
                    (start, pages)
                };
                let got = builder.set_aside_for_kernel(start, pages);
                if got.is_ok() {
                    set_aside += 1;
                }
                (got, model_set_aside(&mut model, start, pages))
            } else {
                let (firmware_type, kind) = TYPES[random.below(TYPES.len())];
                (
                    builder.add_firmware(firmware_type, start, pages),
                    model_add(&mut model, kind, start, pages),
                )
            };
            if got.is_err() {
                refused_ops += 1;
            }
            assert_eq!(got, expected, "case {case}");
        }

        let bytes = written(&mut builder);
        let handoff = Handoff::read(&bytes).expect("the reader accepts the builder's output");
        let regions: Vec<Region> = handoff.regions().collect();
        let mut pages = Model::new();
        for (i, region) in regions.iter().enumerate() {
            if i > 0 {
                let before = regions[i - 1];
                assert!(
                    before.end() < region.start || before.kind != region.kind,
                    "case {case}: {before:?} and {region:?} were not merged"
                );
            }
            let first = region.start / PAGE;
            pages.extend((first..first + region.pages).map(|page| (page, region.kind)));
        }
        assert_eq!(pages, model, "case {case}");
        let usable = model.values().filter(|kind| **kind == Kind::Usable).count();
        assert_eq!(handoff.usable_pages(), usable as u64, "case {case}");
    }
    // Every outcome must be common, or the test explores too little.
    assert!(
        set_aside > 5_000 && refused_ops > 5_000,
        "{set_aside} set aside, {refused_ops} refused"
    );
}

/// Damages a valid structure tens of thousands of times and checks that the
/// reader never panics and never reports a region outside the bytes or one
/// that breaks the rules it checks.
#[test]
fn damaged_structures_are_refused_or_stay_inside_the_bytes() {
    let regions: [Raw; 6] = [
        (0, 16, 1),
        (16 * PAGE, 4, 2),
        (20 * PAGE, 12, 4),
        (0x10_0000, 0x100, 3),
        (0x20_0000, 8, 8),
        (PHYSICAL_LIMIT - PAGE, 1, 10),
    ];
    let mut valid = encode(1, 0, 24, &regions, 8);
    let length = valid.len();
    // Bytes past the stated length, so that a larger length can still fit.
    valid.extend([0; 64]);
    let total = valid.len();

    // Each field a mutation is aimed at, with its width in bytes.
    let mut fields: Vec<(usize, usize)> = vec![(8, 2), (10, 2), (12, 4), (16, 4), (20, 4)];
    for i in 0..regions.len() {
        fields.extend([(0, 8), (8, 8), (16, 4)].map(|(f, w)| (24 + 24 * i + f, w)));
    }
    let interesting = [
        0,
        1,
        23,
        24,
        25,
        48,
        length as u64,
        total as u64,
        PAGE,
        PHYSICAL_LIMIT,
        u64::from(u32::MAX),
        u64::MAX,
    ];
    let put_field = |bytes: &mut Vec<u8>, (at, width): (usize, usize), value: u64| {
        put(bytes, at, &value.to_le_bytes()[..width]);
    };

    let mut random = Random(0x5EED_B15B_4A4D_0001);
    let (mut accepted, mut rejected) = (0, 0);
    for case in 0..50_000 {
        let mut bytes = valid.clone();
        for _ in 0..1 + random.below(4) {
            match random.below(3) {
                // Any byte becomes any value.
                0 => {
                    let at = random.below(total);
                    bytes[at] = random.next() as u8;
                }
                // A field becomes a value near an edge.
                1 => {
                    let field = fields[random.below(fields.len())];
                    let value = interesting[random.below(interesting.len())];
                    let delta = random.below(3) as u64;
                    put_field(&mut bytes, field, value.wrapping_sub(delta));
                }
                // A field becomes any value.
                _ => {
                    let field = fields[random.below(fields.len())];
                    put_field(&mut bytes, field, random.next());
                }
            }
        }
        // Sometimes the bytes are also cut short.
        if random.below(4) == 0 {
            bytes.truncate(random.below(total + 1));
        }
        match Handoff::read(&bytes) {
            Err(_) => rejected += 1,
            Ok(handoff) => {
                accepted += 1;
                let inside = bytes.as_ptr_range();
                let table = handoff.table.as_ptr_range();
                assert!(
                    inside.start <= table.start.cast() && table.end.cast() <= inside.end,
                    "case {case}: the regions are outside the bytes"
                );
                let mut end = 0;
                let mut sum = 0;
                for region in handoff.regions() {
                    assert!(region.start >= end, "case {case}: {region:?}");
                    assert_eq!(region.start % PAGE, 0, "case {case}: {region:?}");
                    assert!(region.pages > 0, "case {case}: {region:?}");
                    end = region.start + region.pages * PAGE;
                    assert!(end <= PHYSICAL_LIMIT, "case {case}: {region:?}");
                    if region.kind == Kind::Usable {
                        sum += region.pages;
                    }
                }
                assert_eq!(handoff.usable_pages(), sum, "case {case}");
            }
        }
    }
    // Both outcomes must be common, or the test explores too little.
    assert!(
        accepted > 1_000 && rejected > 1_000,
        "{accepted} accepted, {rejected} rejected"
    );
}

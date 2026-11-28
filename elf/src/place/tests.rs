//! Tests for the placement rules and the checksum. Segments are written out
//! by hand, so a failing test means a rule and its description disagree.

// Building test inputs adds and indexes freely; the crate's lints are for the
// library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::FileType::{Executable, SharedObject};

/// A loadable segment at `address`, read from `offset`, taking `memory`
/// bytes, all of them from the file, with the kernel linker's alignment.
fn segment(address: u64, offset: u64, memory: u64, execute: bool) -> Segment {
    Segment {
        offset,
        file_size: memory,
        address,
        memory_size: memory,
        align: 0x1000,
        flags: Flags {
            read: true,
            write: !execute,
            execute,
        },
    }
}

/// The shape the kernel is linked to: read-only, code and data segments on
/// pages of their own, from [`KERNEL_BASE`] up.
fn good_layout() -> Vec<Segment> {
    vec![
        segment(KERNEL_BASE, 0, 0x274, false),
        segment(KERNEL_BASE + 0x1280, 0x280, 0x15, true),
        segment(KERNEL_BASE + 0x2298, 0x298, 0xd68, false),
    ]
}

fn place_at(entry: u64, segments: &[Segment]) -> Result<Placement, Error> {
    place_segments(Executable, entry, segments.iter().copied())
}

fn error(entry: u64, segments: &[Segment]) -> Error {
    place_at(entry, segments)
        .map(|_| ())
        .expect_err("is refused")
}

#[test]
fn the_kernel_layout_is_placed_one_page_per_segment() {
    let placement = place_at(KERNEL_BASE + 0x1280, &good_layout()).expect("placed");
    let placed = placement.segments();
    assert_eq!(placed.len(), 3);
    assert_eq!(placement.pages(), 3);
    assert_eq!(
        placed[1],
        Placed {
            segment: 1,
            page: KERNEL_BASE + 0x1000,
            pages: 1,
            start: 0x280,
            offset: 0x280,
            file_size: 0x15,
            flags: good_layout()[1].flags,
        }
    );
    assert_eq!(placed[0].page, KERNEL_BASE);
    assert_eq!(placed[2].page, KERNEL_BASE + 0x2000);
    assert_eq!(placed[2].start, 0x298);
}

#[test]
fn a_segment_crossing_a_page_edge_takes_both_pages() {
    let mut segments = [segment(KERNEL_BASE + 0xFF0, 0xFF0, 0x20, true)];
    segments[0].file_size = 0x8;
    let placement = place_at(KERNEL_BASE + 0xFF0, &segments).expect("placed");
    let placed = placement.segments()[0];
    assert_eq!((placed.page, placed.pages), (KERNEL_BASE, 2));
    assert_eq!((placed.start, placed.file_size), (0xFF0, 0x8));
}

#[test]
fn a_position_independent_file_is_refused() {
    let result = place_segments(SharedObject, KERNEL_BASE + 0x1280, good_layout());
    assert_eq!(result.map(|_| ()), Err(Error::NotExecutable));
}

#[test]
fn the_entry_point_may_be_the_base_but_not_below_it() {
    let mut segments = good_layout();
    segments[0].flags.execute = true;
    assert!(place_at(KERNEL_BASE, &segments).is_ok());
    assert_eq!(error(KERNEL_BASE - 1, &segments), Error::EntryBelowBase);
}

#[test]
fn a_segment_below_the_base_is_refused_wherever_it_is() {
    for index in [0, 2] {
        let mut segments = good_layout();
        segments[index].address = KERNEL_BASE - 0x1000 + segments[index].offset;
        assert_eq!(
            error(KERNEL_BASE + 0x1280, &segments),
            Error::BelowBase { segment: index }
        );
    }
}

#[test]
fn a_segment_whose_address_and_offset_disagree_is_refused() {
    let mut segments = good_layout();
    segments[1].offset = 0x290;
    assert_eq!(
        error(KERNEL_BASE + 0x1280, &segments),
        Error::PageOffsetMismatch { segment: 1 }
    );
}

#[test]
fn a_segment_with_more_file_than_memory_is_refused() {
    let mut segments = good_layout();
    segments[1].file_size = 0x16;
    assert_eq!(
        error(KERNEL_BASE + 0x1280, &segments),
        Error::MemorySmallerThanFile { segment: 1 }
    );
}

#[test]
fn any_two_segments_sharing_a_page_are_refused() {
    // The code starts on the read-only segment's page.
    let mut zero_one = good_layout();
    zero_one[1].address = KERNEL_BASE + 0x280;
    // The data starts on the code's page.
    let mut one_two = good_layout();
    one_two[2].address = KERNEL_BASE + 0x1298;
    // The data starts on the read-only segment's page, past the code.
    let mut zero_two = good_layout();
    zero_two[2].address = KERNEL_BASE + 0x298;
    for (segments, first, second) in [(zero_one, 0, 1), (one_two, 1, 2), (zero_two, 0, 2)] {
        assert_eq!(
            error(KERNEL_BASE, &segments),
            Error::SharedPage { first, second }
        );
    }
}

#[test]
fn a_segment_starting_on_a_later_page_of_another_shares_it() {
    let long = segment(KERNEL_BASE, 0, 0x1800, false);
    let after = segment(KERNEL_BASE + 0x1900, 0x1900, 0x10, true);
    for (segments, first, second) in [([long, after], 0, 1), ([after, long], 0, 1)] {
        assert_eq!(
            error(KERNEL_BASE, &segments),
            Error::SharedPage { first, second }
        );
    }
}

#[test]
fn segments_ending_and_starting_on_page_edges_do_not_share() {
    let segments = [
        segment(KERNEL_BASE, 0, 0x1000, false),
        segment(KERNEL_BASE + 0x1000, 0x1000, 0x1000, true),
        // Takes no memory, so no page.
        segment(KERNEL_BASE + 0x1000, 0x1000, 0, false),
    ];
    let placement = place_at(KERNEL_BASE + 0x1000, &segments).expect("placed");
    assert_eq!(placement.segments().len(), 2);
}

#[test]
fn a_segment_with_no_memory_inside_another_ones_page_takes_no_page() {
    let mut segments = good_layout();
    segments.insert(1, segment(KERNEL_BASE + 0x100, 0x100, 0, false));
    let placement = place_at(KERNEL_BASE + 0x1280, &segments).expect("placed");
    let numbers: Vec<_> = placement.segments().iter().map(|p| p.segment).collect();
    assert_eq!(numbers, [0, 2, 3]);
    assert_eq!(placement.pages(), 3);
}

#[test]
fn a_segment_may_end_at_the_top_of_the_address_range_but_not_wrap() {
    let top_page = 0u64.wrapping_sub(0x1000);
    let mut segments = [segment(top_page + 0x800, 0x800, 0x800, true)];
    let placement = place_at(top_page + 0x800, &segments).expect("ends at 2^64");
    assert_eq!(
        (placement.segments()[0].page, placement.pages()),
        (top_page, 1)
    );
    segments[0].memory_size = 0x801;
    assert_eq!(
        error(KERNEL_BASE, &segments),
        Error::PastTheTop { segment: 0 }
    );
    // The very last byte alone.
    let last = [segment(u64::MAX, 0xFFF, 1, true)];
    assert_eq!(place_at(u64::MAX, &last).expect("placed").pages(), 1);
}

/// `count` one-page segments, each on its own page, the first one code.
fn one_page_each(count: u64) -> Vec<Segment> {
    (0..count)
        .map(|i| segment(KERNEL_BASE + 0x1000 * i, 0x1000 * i, 0x10, i == 0))
        .collect()
}

#[test]
fn more_segments_than_the_table_holds_are_refused() {
    let full = one_page_each(MAX_SEGMENTS as u64);
    assert_eq!(
        place_at(KERNEL_BASE, &full).expect("fits").segments().len(),
        MAX_SEGMENTS
    );
    assert_eq!(
        error(KERNEL_BASE, &one_page_each(MAX_SEGMENTS as u64 + 1)),
        Error::TooManySegments
    );
    // A segment without memory needs no room in the table.
    let mut with_empty = full;
    with_empty.push(segment(KERNEL_BASE + 0x10_0000, 0, 0, false));
    assert!(place_at(KERNEL_BASE, &with_empty).is_ok());
}

#[test]
fn more_pages_than_the_limit_are_refused() {
    let mut segments = [
        segment(KERNEL_BASE, 0, 0x10, false),
        segment(KERNEL_BASE + 0x1000, 0x1000, 0x10, true),
    ];
    segments[1].memory_size = (PAGE_LIMIT - 1) * PAGE_SIZE;
    assert_eq!(
        place_at(KERNEL_BASE + 0x1000, &segments)
            .expect("at the limit")
            .pages(),
        PAGE_LIMIT
    );
    segments[1].memory_size += 1;
    assert_eq!(
        error(KERNEL_BASE, &segments),
        Error::TooManyPages {
            pages: PAGE_LIMIT + 1
        }
    );
}

#[test]
fn the_entry_point_must_be_inside_an_executable_segment() {
    // The code is 0x15 bytes at `KERNEL_BASE + 0x1280`.
    let code = KERNEL_BASE + 0x1280;
    for entry in [code, code + 0x14] {
        assert!(place_at(entry, &good_layout()).is_ok(), "{entry:#x}");
    }
    // In the read-only and the writable segment, in the gap before the code
    // on its own page, and just past the code.
    for entry in [
        KERNEL_BASE + 0x10,
        KERNEL_BASE + 0x2300,
        code - 1,
        code + 0x15,
    ] {
        assert_eq!(
            error(entry, &good_layout()),
            Error::EntryNotExecutable,
            "{entry:#x}"
        );
    }
}

#[test]
fn the_entry_point_may_be_in_memory_the_file_does_not_fill() {
    let mut segments = good_layout();
    segments[1].memory_size = 0x2000;
    segments[2].address += 0x2000;
    assert!(place_at(KERNEL_BASE + 0x2000, &segments).is_ok());
    // A code segment taking no memory holds no entry point.
    let empty = [segment(KERNEL_BASE, 0, 0, true)];
    assert_eq!(error(KERNEL_BASE, &empty), Error::EntryNotExecutable);
}

#[test]
fn the_entry_point_rule_comes_after_the_layout_rules() {
    // Entered in the read-only segment, which also shares a page.
    let mut segments = good_layout();
    segments[2].address = KERNEL_BASE + 0x298;
    assert_eq!(
        error(KERNEL_BASE, &segments),
        Error::SharedPage {
            first: 0,
            second: 2
        }
    );
}

#[test]
fn the_checksum_is_64_bit_fnv_1a() {
    // Values from the FNV authors' published test vectors.
    let sum = |bytes: &[u8]| {
        let mut sum = Fnv1a::new();
        sum.add(bytes);
        sum.value()
    };
    assert_eq!(sum(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(sum(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(sum(b"foobar"), 0x8594_4171_f739_67e8);
    // Adding in pieces gives the same value as adding at once.
    let mut pieces = Fnv1a::new();
    pieces.add(b"foo");
    pieces.add(b"bar");
    assert_eq!(pieces.value(), sum(b"foobar"));
}

//! Tests for the reader. Inputs are built byte by byte from the layouts in
//! the System V ABI, so a test failing means the reader and the
//! specification disagree, not that two copies of the same code do.

// Building test files indexes and adds freely; the crate's lints are for the
// reader itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

/// Length of every test file.
const LEN: usize = 512;
/// Entry address of the valid file: inside its code segment.
const ENTRY: u64 = 0x20_1110;
/// Where the program header table starts in a test file.
const PH_AT: usize = 64;
/// Mixed into each segment's physical address so that it differs from the
/// virtual one.
const PADDR_MARK: u64 = 0x5A00_0000;

fn put(file: &mut [u8], at: usize, value: &[u8]) {
    file[at..at + value.len()].copy_from_slice(value);
}
fn put16(file: &mut [u8], at: usize, value: u16) {
    put(file, at, &value.to_le_bytes());
}
fn put32(file: &mut [u8], at: usize, value: u32) {
    put(file, at, &value.to_le_bytes());
}
fn put64(file: &mut [u8], at: usize, value: u64) {
    put(file, at, &value.to_le_bytes());
}

/// One program header, as a test writes it.
#[derive(Clone, Copy)]
struct Ph {
    kind: u32,
    flags: u32,
    offset: u64,
    address: u64,
    file_size: u64,
    memory_size: u64,
    align: u64,
}

/// A loadable, readable and executable segment holding the entry point.
fn code() -> Ph {
    Ph {
        kind: 1,
        flags: 0b101,
        offset: 0x100,
        address: 0x20_1100,
        file_size: 0x40,
        memory_size: 0x40,
        align: 0x1000,
    }
}

/// A note segment, which a loader skips.
fn note() -> Ph {
    Ph {
        kind: 4,
        flags: 0b100,
        offset: 0xF0,
        address: 0,
        file_size: 0x10,
        memory_size: 0x10,
        align: 4,
    }
}

/// A loadable, readable and writable segment with zero-filled memory after
/// its file contents.
fn data() -> Ph {
    Ph {
        kind: 1,
        flags: 0b110,
        offset: 0x140,
        address: 0x20_2140,
        file_size: 0x20,
        memory_size: 0x100,
        align: 0x1000,
    }
}

/// Builds a little-endian ELF64 executable for x86-64 with these program
/// headers, following the layouts in the gABI "ELF Header" and "Program
/// Header" sections.
fn build(headers: &[Ph]) -> Vec<u8> {
    let mut file = vec![0u8; LEN];
    put(&mut file, 0, &[0x7F, b'E', b'L', b'F', 2, 1, 1]);
    put16(&mut file, 16, 2); // e_type: ET_EXEC
    put16(&mut file, 18, 62); // e_machine: EM_X86_64
    put32(&mut file, 20, 1); // e_version
    put64(&mut file, 24, ENTRY);
    put64(&mut file, 32, PH_AT as u64);
    put16(&mut file, 52, 64); // e_ehsize
    put16(&mut file, 54, 56); // e_phentsize
    put16(&mut file, 56, headers.len() as u16);
    for (i, ph) in headers.iter().enumerate() {
        let at = PH_AT + 56 * i;
        put32(&mut file, at, ph.kind);
        put32(&mut file, at + 4, ph.flags);
        put64(&mut file, at + 8, ph.offset);
        put64(&mut file, at + 16, ph.address);
        // p_paddr: a value no test expects, so reading it in place of
        // p_vaddr fails.
        put64(&mut file, at + 24, PADDR_MARK ^ ph.address);
        put64(&mut file, at + 32, ph.file_size);
        put64(&mut file, at + 40, ph.memory_size);
        put64(&mut file, at + 48, ph.align);
    }
    file
}

fn valid() -> Vec<u8> {
    build(&[code(), note(), data()])
}

fn error(file: &[u8]) -> Error {
    Elf::parse(file).expect_err("the file is rejected")
}

/// The valid file with one program header replaced.
fn with_header(index: usize, ph: Ph) -> Vec<u8> {
    let mut headers = [code(), note(), data()];
    headers[index] = ph;
    build(&headers)
}

#[test]
fn reads_a_valid_file() {
    let file = valid();
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.entry(), ENTRY);
    assert_eq!(elf.file_type(), FileType::Executable);
    let code = Segment {
        offset: 0x100,
        file_size: 0x40,
        address: 0x20_1100,
        memory_size: 0x40,
        align: 0x1000,
        flags: Flags {
            read: true,
            write: false,
            execute: true,
        },
    };
    let data = Segment {
        offset: 0x140,
        file_size: 0x20,
        address: 0x20_2140,
        memory_size: 0x100,
        align: 0x1000,
        flags: Flags {
            read: true,
            write: true,
            execute: false,
        },
    };
    assert_eq!(elf.segments().collect::<Vec<_>>(), [code, data]);
    assert_eq!(elf.entry_segment(), Some(code));
}

#[test]
fn reads_a_position_independent_file() {
    let mut file = valid();
    put16(&mut file, 16, 3); // ET_DYN
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.file_type(), FileType::SharedObject);
}

#[test]
fn a_file_without_program_headers_has_no_segments() {
    let file = build(&[]);
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.segments().count(), 0);
    assert_eq!(elf.entry_segment(), None);
}

#[test]
fn the_entry_segment_is_the_one_whose_memory_holds_the_entry() {
    let mut file = valid();
    // In the zero-filled tail of the data segment.
    put64(&mut file, 24, 0x20_2200);
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.entry_segment().map(|s| s.address), Some(0x20_2140));
    // One past the end of the data segment's memory.
    put64(&mut file, 24, 0x20_2240);
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.entry_segment(), None);
    // Below every segment.
    put64(&mut file, 24, 0x1000);
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.entry_segment(), None);
}

#[test]
fn rejects_empty_input() {
    assert_eq!(error(&[]), Error::Truncated);
}

#[test]
fn rejects_a_truncated_header() {
    assert_eq!(error(&valid()[..63]), Error::Truncated);
    assert_eq!(error(&valid()[..4]), Error::Truncated);
}

#[test]
fn rejects_a_wrong_signature() {
    for at in 0..4 {
        let mut file = valid();
        file[at] ^= 0x20;
        assert_eq!(error(&file), Error::BadSignature, "byte {at}");
    }
}

#[test]
fn rejects_a_32_bit_file() {
    let mut file = valid();
    file[4] = 1; // ELFCLASS32
    assert_eq!(error(&file), Error::NotElf64);
}

#[test]
fn rejects_a_big_endian_file() {
    let mut file = valid();
    file[5] = 2; // ELFDATA2MSB
    assert_eq!(error(&file), Error::NotLittleEndian);
}

#[test]
fn rejects_an_unknown_version() {
    let mut file = valid();
    file[6] = 2;
    assert_eq!(error(&file), Error::BadVersion);
    let mut file = valid();
    put32(&mut file, 20, 0);
    assert_eq!(error(&file), Error::BadVersion);
}

#[test]
fn rejects_another_processor() {
    let mut file = valid();
    put16(&mut file, 18, 3); // EM_386
    assert_eq!(error(&file), Error::NotX86_64);
}

#[test]
fn rejects_a_file_that_is_not_a_program() {
    let mut file = valid();
    put16(&mut file, 16, 1); // ET_REL
    assert_eq!(error(&file), Error::UnsupportedType(1));
}

#[test]
fn rejects_a_wrong_header_size() {
    let mut file = valid();
    put16(&mut file, 52, 52);
    assert_eq!(error(&file), Error::BadHeaderSize);
}

#[test]
fn rejects_a_wrong_program_header_size() {
    let mut file = valid();
    put16(&mut file, 54, 32);
    assert_eq!(error(&file), Error::BadProgramHeaderSize);
    let mut file = build(&[]);
    put16(&mut file, 54, 0);
    assert_eq!(error(&file), Error::BadProgramHeaderSize);
}

#[test]
fn rejects_an_extended_program_header_count() {
    let mut file = valid();
    put16(&mut file, 56, 0xFFFF); // PN_XNUM
    assert_eq!(error(&file), Error::TooManyProgramHeaders);
}

#[test]
fn rejects_a_program_header_table_past_the_end() {
    // Three entries from 400 end at 568, past the 512-byte file.
    let mut file = valid();
    put64(&mut file, 32, 400);
    assert_eq!(error(&file), Error::ProgramHeadersOutsideFile);
    // One entry more than the file holds.
    let mut file = valid();
    put16(&mut file, 56, 9);
    assert_eq!(error(&file), Error::ProgramHeadersOutsideFile);
    // An offset that overflows when the table length is added.
    let mut file = valid();
    put64(&mut file, 32, u64::MAX - 8);
    assert_eq!(error(&file), Error::ProgramHeadersOutsideFile);
}

#[test]
fn a_table_ending_at_the_end_of_the_file_is_accepted() {
    let mut file = build(&[]);
    put16(&mut file, 56, 1);
    put64(&mut file, 32, (LEN - 56) as u64);
    assert!(Elf::parse(&file).is_ok());
}

#[test]
fn rejects_a_segment_past_the_end() {
    let file = with_header(
        0,
        Ph {
            offset: 0x1F0,
            file_size: 0x11,
            memory_size: 0x11,
            address: 0x20_11F0,
            ..code()
        },
    );
    assert_eq!(error(&file), Error::SegmentOutsideFile { index: 0 });
}

#[test]
fn a_segment_ending_at_the_end_of_the_file_is_accepted() {
    let file = with_header(
        0,
        Ph {
            offset: 0x1F0,
            file_size: 0x10,
            memory_size: 0x10,
            address: 0x20_11F0,
            ..code()
        },
    );
    assert!(Elf::parse(&file).is_ok());
}

#[test]
fn rejects_a_segment_whose_offset_and_size_overflow() {
    let file = with_header(
        2,
        Ph {
            offset: u64::MAX - 0xF,
            file_size: 0x20,
            memory_size: 0x20,
            address: 0x20_2FF0,
            ..data()
        },
    );
    assert_eq!(error(&file), Error::SegmentOutsideFile { index: 2 });
}

#[test]
fn rejects_memory_smaller_than_the_file_contents() {
    let file = with_header(
        2,
        Ph {
            memory_size: 0x1F,
            ..data()
        },
    );
    assert_eq!(error(&file), Error::MemorySmallerThanFile { index: 2 });
}

#[test]
fn rejects_memory_that_wraps_around_the_address_space() {
    let file = with_header(
        2,
        Ph {
            address: u64::MAX - 0xEBF, // keeps the alignment rule satisfied
            memory_size: 0x1000,
            ..data()
        },
    );
    assert_eq!(error(&file), Error::AddressOverflow { index: 2 });
}

#[test]
fn rejects_an_alignment_that_is_not_a_power_of_two() {
    let file = with_header(
        0,
        Ph {
            align: 0x3000,
            ..code()
        },
    );
    assert_eq!(error(&file), Error::BadAlignment { index: 0 });
}

#[test]
fn rejects_an_address_and_offset_that_disagree_modulo_the_alignment() {
    let file = with_header(
        0,
        Ph {
            address: 0x20_1108,
            ..code()
        },
    );
    assert_eq!(error(&file), Error::BadAlignment { index: 0 });
}

#[test]
fn alignment_zero_or_one_means_none() {
    for align in [0, 1] {
        let file = with_header(
            0,
            Ph {
                address: 0x20_1108,
                align,
                ..code()
            },
        );
        assert!(Elf::parse(&file).is_ok(), "align {align}");
    }
}

#[test]
fn segments_that_are_not_loadable_are_not_checked() {
    let file = with_header(
        1,
        Ph {
            offset: u64::MAX,
            file_size: u64::MAX,
            memory_size: 0,
            align: 3,
            ..note()
        },
    );
    let elf = Elf::parse(&file).expect("the file is accepted");
    assert_eq!(elf.segments().count(), 2);
}

/// A tiny xorshift generator (Marsaglia, 2003) so the mutation test is the
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

/// Damages the valid file many thousands of times and checks that the
/// reader never panics and never reports a segment outside the file.
#[test]
fn damaged_files_are_rejected_or_stay_inside_the_file() {
    // The offsets of every header field a mutation is aimed at: the file
    // header, then each field of the three program headers.
    let mut fields: Vec<usize> = vec![16, 18, 20, 24, 32, 52, 54, 56];
    for i in 0..3 {
        fields.extend([0, 4, 8, 16, 32, 40, 48].map(|f| PH_AT + 56 * i + f));
    }
    let interesting = [0, 1, 0x38, 0x40, 0x1000, LEN as u64, u64::MAX];

    let mut random = Random(0x5EED_B15B_0000_0001);
    let (mut accepted, mut rejected) = (0, 0);
    for _ in 0..50_000 {
        let mut file = valid();
        for _ in 0..1 + random.below(4) {
            match random.below(3) {
                // Any byte becomes any value.
                0 => {
                    let at = random.below(LEN);
                    file[at] = random.next() as u8;
                }
                // A header field becomes a value near an edge.
                1 => {
                    let at = fields[random.below(fields.len())];
                    let value = interesting[random.below(interesting.len())];
                    let delta = random.below(3) as u64;
                    put64(&mut file, at, value.wrapping_sub(delta));
                }
                // A header field becomes any value.
                _ => {
                    let at = fields[random.below(fields.len())];
                    put64(&mut file, at, random.next());
                }
            }
        }
        // Sometimes the file is also cut short.
        if random.below(4) == 0 {
            file.truncate(random.below(LEN + 1));
        }
        match Elf::parse(&file) {
            Err(_) => rejected += 1,
            Ok(elf) => {
                accepted += 1;
                for segment in elf.segments() {
                    let end = segment
                        .offset
                        .checked_add(segment.file_size)
                        .expect("offset + file size does not overflow");
                    assert!(end <= file.len() as u64, "{segment:?} is outside the file");
                    assert!(segment.memory_size >= segment.file_size, "{segment:?}");
                }
                let _ = elf.entry_segment();
            }
        }
    }
    // Both outcomes must be common, or the test explores too little.
    assert!(
        accepted > 1_000 && rejected > 1_000,
        "{accepted} accepted, {rejected} rejected"
    );
}

#[test]
fn an_empty_table_follows_the_same_bounds_as_any_other() {
    // No program headers, so the table is empty. Starting at the end of the
    // file it is an empty range inside the file, as for a table that ends
    // there; starting past the end it is not inside the file at all.
    let mut file = build(&[]);
    put64(&mut file, 32, LEN as u64);
    assert_eq!(Elf::parse(&file).map(|elf| elf.segments().count()), Ok(0));
    for past in [LEN as u64 + 1, u64::MAX] {
        put64(&mut file, 32, past);
        assert_eq!(error(&file), Error::ProgramHeadersOutsideFile);
    }
}

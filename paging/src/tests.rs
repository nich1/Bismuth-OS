//! Tests for the table builder. Tables are built in ordinary memory under
//! made-up physical addresses, then read back by [`walk`], which is written
//! separately from the library from the entry formats in the Intel SDM
//! volume 3A, chapter 4 "Paging", so a test failing means the library and
//! the manual disagree, not that two copies of the same code do.

// Building and walking test tables indexes and adds freely; the crate's lints
// are for the library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// The made-up physical address of the first test frame, which is the
/// top-level table. Its high bits are set so that an entry losing them shows.
const BASE: u64 = 0x000A_BCD0_0000_0000;
/// What a frame holds when it is handed out: present, writable, user and
/// page-size bits all set, so a table the library forgot to zero shows.
const GARBAGE: u64 = 0xDEAD_BEEF_DEAD_BEEF;

// Entry bits, from the SDM's 4-level paging entry-format tables.
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const NO_EXECUTE: u64 = 1 << 63;
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;

const PAGE: u64 = 0x1000;
const READ: Permissions = Permissions {
    write: false,
    execute: false,
};
const READ_WRITE: Permissions = Permissions {
    write: true,
    execute: false,
};
const READ_EXECUTE: Permissions = Permissions {
    write: false,
    execute: true,
};
const ALL: Permissions = Permissions {
    write: true,
    execute: true,
};

/// Frames in a vector. Frame `n` has the physical address `BASE + n * 4 KiB`;
/// frame 0 is the top-level table, zeroed as the caller must provide it.
struct Memory {
    frames: Vec<[u64; 512]>,
    /// How many more frames `allocate_frame` may hand out.
    left: usize,
}

impl Memory {
    fn new(left: usize) -> Self {
        Memory {
            frames: vec![[0; 512]],
            left,
        }
    }

    fn root(&self) -> u64 {
        BASE
    }

    /// How many frames the library has taken.
    fn taken(&self) -> usize {
        self.frames.len() - 1
    }

    fn frame(&self, address: u64) -> &[u64; 512] {
        assert_eq!(address % PAGE, 0, "{address:#x} is not a frame");
        &self.frames[((address - BASE) / PAGE) as usize]
    }
}

impl PhysicalMemory for Memory {
    fn allocate_frame(&mut self) -> Option<u64> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        self.frames.push([GARBAGE; 512]);
        Some(BASE + self.taken() as u64 * PAGE)
    }

    fn table(&mut self, address: u64) -> &mut [u64; 512] {
        assert_eq!(address % PAGE, 0, "{address:#x} is not a frame");
        &mut self.frames[((address - BASE) / PAGE) as usize]
    }
}

/// Whether `address` is in the low or the high half of the 48-bit range.
fn canonical(address: u64) -> bool {
    !(0x0000_8000_0000_0000..0xFFFF_8000_0000_0000).contains(&address)
}

/// What the tables say about one virtual page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Found {
    Unmapped,
    Page(u64, Permissions),
}

/// Translates `address` by reading the tables the way the processor does,
/// and checks that every entry on the way holds only the bits it should.
///
/// The indices come from the page number by division, not by shifting and
/// masking as the library does.
fn walk(memory: &Memory, address: u64) -> Found {
    assert!(canonical(address), "{address:#x} cannot be walked");
    let page_number = (address % (1 << 48)) / PAGE;
    let indices = [
        page_number / (512 * 512 * 512),
        page_number / (512 * 512) % 512,
        page_number / 512 % 512,
        page_number % 512,
    ];
    let mut table = memory.root();
    for (level, index) in indices.into_iter().enumerate() {
        let entry = memory.frame(table)[index as usize];
        if entry & PRESENT == 0 {
            assert_eq!(
                entry, 0,
                "a non-present entry for {address:#x} is not empty"
            );
            return Found::Unmapped;
        }
        if level < 3 {
            // Present and writable, with no no-execute, user or page-size bit.
            assert_eq!(
                entry & !ADDRESS,
                PRESENT | WRITABLE,
                "table entry {entry:#x} on the way to {address:#x}"
            );
            table = entry & ADDRESS;
        } else {
            assert_eq!(
                entry & !(ADDRESS | WRITABLE | NO_EXECUTE),
                PRESENT,
                "page entry {entry:#x} for {address:#x}"
            );
            let permissions = Permissions {
                write: entry & WRITABLE != 0,
                execute: entry & NO_EXECUTE == 0,
            };
            return Found::Page(entry & ADDRESS, permissions);
        }
    }
    panic!("four levels always end in a page entry");
}

/// Maps `pages` pages and checks the result.
fn map_pages(
    memory: &mut Memory,
    virtual_start: u64,
    physical_start: u64,
    pages: u64,
    permissions: Permissions,
) {
    let root = memory.root();
    assert_eq!(
        map(
            memory,
            root,
            virtual_start,
            physical_start,
            pages * PAGE,
            permissions
        ),
        Ok(()),
        "mapping {virtual_start:#x}"
    );
}

/// Checks that the `pages` pages from `virtual_start` are mapped in order
/// from `physical_start`, and that the page on each side is not mapped.
fn assert_mapped(
    memory: &Memory,
    virtual_start: u64,
    physical_start: u64,
    pages: u64,
    permissions: Permissions,
) {
    for i in 0..pages {
        assert_eq!(
            walk(memory, virtual_start + i * PAGE),
            Found::Page(physical_start + i * PAGE, permissions),
            "page {i} from {virtual_start:#x}"
        );
    }
    for neighbour in [
        virtual_start.wrapping_sub(PAGE),
        virtual_start.wrapping_add(pages * PAGE),
    ] {
        if canonical(neighbour) {
            assert_eq!(walk(memory, neighbour), Found::Unmapped, "{neighbour:#x}");
        }
    }
}

/// Maps with the given arguments into fresh tables, expecting an error, and
/// checks that nothing was touched.
fn refused(virtual_start: u64, physical_start: u64, length: u64) -> Error {
    let mut memory = Memory::new(100);
    let root = memory.root();
    let result = map(
        &mut memory,
        root,
        virtual_start,
        physical_start,
        length,
        READ,
    );
    assert_eq!(memory.taken(), 0, "a refused mapping took frames");
    assert_eq!(
        memory.frames[0], [0; 512],
        "a refused mapping wrote an entry"
    );
    result.expect_err("the mapping is refused")
}

#[test]
fn maps_the_first_and_last_page_of_each_half() {
    for address in [
        0,
        0x0000_7FFF_FFFF_F000,
        0xFFFF_8000_0000_0000,
        0xFFFF_FFFF_FFFF_F000,
    ] {
        let mut memory = Memory::new(100);
        map_pages(&mut memory, address, 0x1234_5000, 1, READ_WRITE);
        assert_mapped(&memory, address, 0x1234_5000, 1, READ_WRITE);
        // One table at each level below the top.
        assert_eq!(memory.taken(), 3, "{address:#x}");
    }
}

#[test]
fn maps_a_range_crossing_a_table_boundary_at_each_level() {
    // The last page covered by one table and the first page of the next:
    // (start, tables needed below the top).
    let cases = [
        // Two last-level tables under one directory.
        (0x0000_0000_001F_F000, 4),
        // Two directories under one directory-pointer table.
        (0x0000_0000_3FFF_F000, 5),
        // Two directory-pointer tables under two top-level entries.
        (0x0000_007F_FFFF_F000, 6),
        // The same in the high half: top-level entries 510 and 511.
        (0xFFFF_FF7F_FFFF_F000, 6),
    ];
    for (start, tables) in cases {
        let mut memory = Memory::new(100);
        map_pages(&mut memory, start, 0x40_0000, 2, READ_EXECUTE);
        assert_mapped(&memory, start, 0x40_0000, 2, READ_EXECUTE);
        assert_eq!(memory.taken(), tables, "{start:#x}");
    }
}

#[test]
fn maps_the_kernel_at_its_base() {
    // Laid out like a kernel: code, read-only data, writable data.
    let base = 0xFFFF_FFFF_8000_0000;
    let mut memory = Memory::new(100);
    map_pages(&mut memory, base, 0x20_0000, 3, READ_EXECUTE);
    map_pages(&mut memory, base + 3 * PAGE, 0x20_3000, 2, READ);
    map_pages(&mut memory, base + 5 * PAGE, 0x30_0000, 1, READ_WRITE);
    for (page, expected) in [
        (0, Found::Page(0x20_0000, READ_EXECUTE)),
        (2, Found::Page(0x20_2000, READ_EXECUTE)),
        (3, Found::Page(0x20_3000, READ)),
        (4, Found::Page(0x20_4000, READ)),
        (5, Found::Page(0x30_0000, READ_WRITE)),
        (6, Found::Unmapped),
    ] {
        assert_eq!(walk(&memory, base + page * PAGE), expected, "page {page}");
    }
    assert_eq!(walk(&memory, base - PAGE), Found::Unmapped);
    // All three calls share one table at each level.
    assert_eq!(memory.taken(), 3);
}

#[test]
fn page_entries_carry_the_permissions() {
    let mut memory = Memory::new(100);
    for (i, permissions) in [READ, READ_WRITE, READ_EXECUTE, ALL]
        .into_iter()
        .enumerate()
    {
        let address = 0x10_0000 + i as u64 * PAGE;
        map_pages(
            &mut memory,
            address,
            0x9000_0000 + i as u64 * PAGE,
            1,
            permissions,
        );
    }
    // The bits themselves, as the processor reads them.
    let table = memory.frame(BASE + 3 * PAGE);
    assert_eq!(table[256], 0x9000_0000 | PRESENT | NO_EXECUTE);
    assert_eq!(table[257], 0x9000_1000 | PRESENT | WRITABLE | NO_EXECUTE);
    assert_eq!(table[258], 0x9000_2000 | PRESENT);
    assert_eq!(table[259], 0x9000_3000 | PRESENT | WRITABLE);
}

#[test]
fn existing_tables_are_reused() {
    let mut memory = Memory::new(100);
    map_pages(&mut memory, 0x5000, 0x1000, 1, READ);
    assert_eq!(memory.taken(), 3);
    // The next page: same tables.
    map_pages(&mut memory, 0x6000, 0x2000, 1, READ);
    assert_eq!(memory.taken(), 3);
    // The next 2 MiB: one new last-level table.
    map_pages(&mut memory, 0x20_0000, 0x3000, 1, READ);
    assert_eq!(memory.taken(), 4);
    assert_mapped(&memory, 0x5000, 0x1000, 2, READ);
    assert_mapped(&memory, 0x20_0000, 0x3000, 1, READ);
}

#[test]
fn new_tables_are_zeroed() {
    // Every frame starts full of set bits; after mapping one page each new
    // table must hold exactly one entry.
    let mut memory = Memory::new(100);
    map_pages(&mut memory, 0x0000_1234_5678_9000, 0x1000, 1, ALL);
    assert_eq!(memory.taken(), 3);
    for frame in &memory.frames {
        assert_eq!(frame.iter().filter(|&&entry| entry != 0).count(), 1);
    }
}

#[test]
fn maps_the_last_physical_page() {
    let last = (1 << 52) - PAGE;
    let mut memory = Memory::new(100);
    map_pages(&mut memory, 0x7000, last, 1, READ);
    assert_mapped(&memory, 0x7000, last, 1, READ);
}

#[test]
fn refuses_unaligned_arguments() {
    assert_eq!(refused(0x1800, 0x1000, PAGE), Error::Unaligned);
    assert_eq!(refused(0x1000, 0x1001, PAGE), Error::Unaligned);
    assert_eq!(refused(0x1000, 0x1000, PAGE + 1), Error::Unaligned);
}

#[test]
fn refuses_an_empty_range() {
    assert_eq!(refused(0x1000, 0x1000, 0), Error::Empty);
}

#[test]
fn refuses_non_canonical_addresses() {
    for address in [
        0x0000_8000_0000_0000,
        0xFFFF_7FFF_FFFF_F000,
        0x1234_0000_0000_0000,
        0x8000_0000_0000_0000,
    ] {
        assert_eq!(
            refused(address, 0, PAGE),
            Error::NonCanonical,
            "{address:#x}"
        );
    }
}

#[test]
fn refuses_ranges_that_leave_the_canonical_halves() {
    // Running off the top of the low half.
    assert_eq!(
        refused(0x0000_7FFF_FFFF_F000, 0, 2 * PAGE),
        Error::NonCanonical
    );
    // Running off the top of the high half, which wraps to 0.
    assert_eq!(
        refused(0xFFFF_FFFF_FFFF_F000, 0, 2 * PAGE),
        Error::NonCanonical
    );
    // Wrapping all the way round from the low half back into it: the last
    // byte would be 0xFFF, which is canonical and in the same half.
    assert_eq!(
        refused(0x2000, 0, 0xFFFF_FFFF_FFFF_F000),
        Error::NonCanonical
    );
    // Jumping the gap: both ends canonical, but in different halves.
    assert_eq!(refused(0, 0, 0xFFFF_8000_0000_1000), Error::NonCanonical);
}

#[test]
fn refuses_physical_addresses_beyond_52_bits() {
    assert_eq!(refused(0, 1 << 52, PAGE), Error::PhysicalTooLarge);
    assert_eq!(
        refused(0, (1 << 52) - PAGE, 2 * PAGE),
        Error::PhysicalTooLarge
    );
    // The end overflows 64 bits.
    assert_eq!(
        refused(0, 0xFFFF_FFFF_FFFF_F000, 2 * PAGE),
        Error::PhysicalTooLarge
    );
}

#[test]
fn refuses_a_mapped_page_and_keeps_the_pages_before_it() {
    let mut memory = Memory::new(100);
    map_pages(&mut memory, 0x3000, 0xA000, 1, READ);
    let root = memory.root();
    // Pages 0x1000 and 0x2000 are new, 0x3000 is taken, 0x4000 is never
    // reached.
    assert_eq!(
        map(&mut memory, root, 0x1000, 0x5_0000, 4 * PAGE, ALL),
        Err(Error::AlreadyMapped(0x3000))
    );
    assert_eq!(walk(&memory, 0x1000), Found::Page(0x5_0000, ALL));
    assert_eq!(walk(&memory, 0x2000), Found::Page(0x5_1000, ALL));
    assert_eq!(walk(&memory, 0x3000), Found::Page(0xA000, READ));
    assert_eq!(walk(&memory, 0x4000), Found::Unmapped);
}

#[test]
fn refuses_to_map_inside_a_large_page() {
    let mut memory = Memory::new(100);
    map_pages(&mut memory, 0x1F_F000, 0x1000, 1, READ);
    // Frames 1, 2 and 3 are the directory-pointer table, the directory and
    // the last-level table. Make the second 1 GiB a 1 GiB page and the second
    // 2 MiB a 2 MiB page (page-size bit 7).
    memory.frames[1][1] = 0x4000_0000 | 1 << 7 | WRITABLE | PRESENT;
    memory.frames[2][1] = 0x20_0000 | 1 << 7 | WRITABLE | PRESENT;
    let root = memory.root();
    assert_eq!(
        map(&mut memory, root, 0x4000_0000, 0, PAGE, READ),
        Err(Error::LargePage(0x4000_0000))
    );
    // The page before the 2 MiB page is free again after this, so the range
    // maps one page and stops at the large page.
    memory.frames[3][511] = 0;
    assert_eq!(
        map(&mut memory, root, 0x1F_F000, 0x7000, 2 * PAGE, READ),
        Err(Error::LargePage(0x20_0000))
    );
    assert_eq!(walk(&memory, 0x1F_F000), Found::Page(0x7000, READ));
    assert_eq!(memory.taken(), 3);
}

#[test]
fn stops_when_frames_run_out() {
    // Two frames: enough for the directory-pointer table and the directory,
    // which stay linked in, but not for the last-level table.
    let mut memory = Memory::new(2);
    let root = memory.root();
    assert_eq!(
        map(&mut memory, root, 0x1000, 0, PAGE, READ),
        Err(Error::OutOfFrames)
    );
    assert_eq!(memory.taken(), 2);
    assert_eq!(walk(&memory, 0x1000), Found::Unmapped);
    assert_eq!(memory.frames[2], [0; 512]);
    // One more frame maps the page, reusing the two tables.
    memory.left = 1;
    map_pages(&mut memory, 0x1000, 0, 1, READ);
    assert_eq!(memory.taken(), 3);
}

#[test]
fn running_out_part_way_keeps_the_pages_before() {
    // Three frames map the first page; the second needs a fourth.
    let mut memory = Memory::new(3);
    let root = memory.root();
    assert_eq!(
        map(&mut memory, root, 0x1F_F000, 0x8000, 2 * PAGE, READ),
        Err(Error::OutOfFrames)
    );
    assert_eq!(walk(&memory, 0x1F_F000), Found::Page(0x8000, READ));
    assert_eq!(walk(&memory, 0x20_0000), Found::Unmapped);
}

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
}

/// What the tables should hold, kept without any tables.
struct Model {
    pages: BTreeMap<u64, (u64, Permissions)>,
    /// The tables below the top that exist, as (level, the address bits
    /// above the part that table covers).
    tables: BTreeSet<(u8, u64)>,
    frames_left: u64,
}

impl Model {
    /// Applies a mapping the way the library's documentation describes it,
    /// and returns what the library should return.
    fn map(
        &mut self,
        start: u64,
        physical: u64,
        length: u64,
        permissions: Permissions,
    ) -> Result<(), Error> {
        if !start.is_multiple_of(PAGE)
            || !physical.is_multiple_of(PAGE)
            || !length.is_multiple_of(PAGE)
        {
            return Err(Error::Unaligned);
        }
        if length == 0 {
            return Err(Error::Empty);
        }
        // In 128 bits nothing overflows.
        let last = u128::from(start) + u128::from(length) - 1;
        let canonical = |a: u128| {
            a < 0x0000_8000_0000_0000
                || (0xFFFF_8000_0000_0000..=0xFFFF_FFFF_FFFF_FFFF).contains(&a)
        };
        let same_half =
            (u128::from(start) >= 0xFFFF_8000_0000_0000) == (last >= 0xFFFF_8000_0000_0000);
        if !canonical(u128::from(start)) || !canonical(last) || !same_half {
            return Err(Error::NonCanonical);
        }
        if u128::from(physical) + u128::from(length) > 1 << 52 {
            return Err(Error::PhysicalTooLarge);
        }
        for i in 0..length / PAGE {
            let page = start + i * PAGE;
            if self.pages.contains_key(&page) {
                return Err(Error::AlreadyMapped(page));
            }
            let low = page % (1 << 48);
            for (level, covers) in [(1, 1 << 39), (2, 1 << 30), (3, 1 << 21)] {
                if !self.tables.contains(&(level, low / covers)) {
                    if self.frames_left == 0 {
                        return Err(Error::OutOfFrames);
                    }
                    self.frames_left -= 1;
                    self.tables.insert((level, low / covers));
                }
            }
            self.pages.insert(page, (physical + i * PAGE, permissions));
        }
        Ok(())
    }

    fn expected(&self, page: u64) -> Found {
        match self.pages.get(&page) {
            Some(&(physical, permissions)) => Found::Page(physical, permissions),
            None => Found::Unmapped,
        }
    }
}

/// Maps many random ranges against [`Model`] and checks every result and
/// every page it covers with [`walk`].
#[test]
fn random_mappings_match_the_model() {
    // Starting points near the edges, so ranges collide, cross table
    // boundaries and run off the canonical halves.
    let bases: [u64; 11] = [
        0,
        0x0000_0000_001F_0000,
        0x0000_0000_3FFF_0000,
        0x0000_007F_FFFF_0000,
        0x0000_7FFF_FFFF_0000,
        0x0000_8000_0000_0000,
        0xFFFF_7FFF_FFFF_0000,
        0xFFFF_8000_0000_0000,
        0xFFFF_FF7F_FFFF_0000,
        0xFFFF_FFFF_8000_0000,
        0xFFFF_FFFF_FFFF_0000,
    ];
    let mut random = Random(0x5EED_0A6E_7AB1_E500);
    let mut outcomes: BTreeMap<&str, u32> = BTreeMap::new();
    for _ in 0..40 {
        let frames = 3 + random.below(40);
        let mut memory = Memory::new(frames as usize);
        let mut model = Model {
            pages: BTreeMap::new(),
            tables: BTreeSet::new(),
            frames_left: frames,
        };
        for _ in 0..100 {
            let mut start = match random.below(8) {
                0 => random.next() & !0xFFF,
                _ => bases[random.below(bases.len() as u64) as usize]
                    .wrapping_add(random.below(32) * PAGE),
            };
            let mut length = match random.below(20) {
                0 => random.next() & !0xFFF,
                _ => random.below(9) * PAGE,
            };
            let mut physical = match random.below(10) {
                0 => random.next() & !0xFFF,
                1 => (1 << 52) - random.below(8) * PAGE,
                _ => random.below(1 << 40) * PAGE,
            };
            match random.below(40) {
                0 => start = start.wrapping_add(1 + random.below(0xFFF)),
                1 => length = length.wrapping_add(1 + random.below(0xFFF)),
                2 => physical = physical.wrapping_add(1 + random.below(0xFFF)),
                _ => {}
            }
            let permissions = Permissions {
                write: random.below(2) == 0,
                execute: random.below(2) == 0,
            };

            let expected = model.map(start, physical, length, permissions);
            let root = memory.root();
            let actual = map(&mut memory, root, start, physical, length, permissions);
            assert_eq!(
                actual, expected,
                "map({start:#x}, {physical:#x}, {length:#x}, {permissions:?})"
            );
            assert_eq!(memory.taken(), model.tables.len());
            let name = match actual {
                Ok(()) => "mapped",
                Err(Error::Unaligned) => "unaligned",
                Err(Error::Empty) => "empty",
                Err(Error::NonCanonical) => "non-canonical",
                Err(Error::PhysicalTooLarge) => "physical too large",
                Err(Error::AlreadyMapped(_)) => "already mapped",
                Err(Error::LargePage(_)) => "large page",
                Err(Error::OutOfFrames) => "out of frames",
            };
            *outcomes.entry(name).or_default() += 1;

            // The pages of the range and one on each side, where they can be
            // walked.
            if length <= 9 * PAGE {
                let first = (start & !0xFFF).wrapping_sub(PAGE);
                for i in 0..length / PAGE + 2 {
                    let page = first.wrapping_add(i * PAGE);
                    if canonical(page) {
                        assert_eq!(walk(&memory, page), model.expected(page), "{page:#x}");
                    }
                }
            }
        }
        for &page in model.pages.keys() {
            assert_eq!(walk(&memory, page), model.expected(page), "{page:#x}");
        }
    }
    // Every outcome but a large page, which `map` never creates, must be
    // common, or the test explores too little.
    for name in [
        "mapped",
        "unaligned",
        "empty",
        "non-canonical",
        "physical too large",
        "already mapped",
        "out of frames",
    ] {
        let count = outcomes.get(name).copied().unwrap_or(0);
        assert!(count >= 20, "{outcomes:?}");
    }
}

//! Whether a checked file can be placed in memory at the addresses it was
//! linked for, and which 4 KiB pages each segment then takes. The boot stage
//! uses these rules before copying the kernel, and `cargo xtask check` uses
//! them on the built kernel, so there is one copy of them.
//!
//! Segments are numbered by their position among the loadable segments, as
//! [`Elf::segments`] returns them. Program headers' physical addresses are
//! never read: a [`Segment`] does not carry them.

use crate::{Elf, FileType, Flags, Segment};

#[cfg(test)]
mod tests;

/// The lowest address the kernel may use: the start of the top 2 GiB of the
/// address range, where the kernel is linked (see `docs/decisions.md`).
pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;
/// The size of the pages segments are placed in.
pub const PAGE_SIZE: u64 = 4096;
/// The bits of an address that give its place within a page.
const IN_PAGE: u64 = PAGE_SIZE - 1;
/// The last page of the address range, which no segment may use.
const LAST_PAGE: u64 = !IN_PAGE;
/// The most pages all segments together may take: 64 MiB, the same as the
/// largest kernel file the boot stage reads. A kernel needs far less, and a
/// damaged memory size must not ask the firmware for most of the machine.
pub const PAGE_LIMIT: u64 = 16 * 1024;
/// The top of the kernel's stack, which the boot stage maps and the kernel
/// starts on: [`KERNEL_BASE`] itself, so the stack and its guard page lie
/// just below the lowest address a segment may use. The rule that no
/// segment starts below [`KERNEL_BASE`] ([`Error::BelowBase`]), and that
/// none runs past the top of the address range ([`Error::PastTheTop`]),
/// keeps every segment out of them. It is a 2 MiB boundary, and the stack
/// and its guard page lie in the 2 MiB below it, which no segment shares,
/// so mapping the stack takes exactly one directory and one last-level
/// table of its own.
pub const STACK_TOP: u64 = KERNEL_BASE;
/// How many 4 KiB pages the stack has: 64 KiB, plenty for a kernel that
/// does not yet recurse or keep large values on the stack, even unoptimised.
pub const STACK_PAGES: u64 = 16;
/// The lowest address of the stack: [`STACK_TOP`] less [`STACK_PAGES`]
/// pages.
pub const STACK_BOTTOM: u64 = 0xFFFF_FFFF_7FFF_0000;
/// The page just below the stack, which is left unmapped, so that running
/// off the bottom of the stack faults instead of writing other memory.
pub const STACK_GUARD: u64 = 0xFFFF_FFFF_7FFE_F000;

// The stack must stay below every segment, so a change to the constants
// that breaks that does not build.
const _: () = assert!(STACK_TOP <= KERNEL_BASE);
const _: () = assert!(STACK_BOTTOM == STACK_TOP - STACK_PAGES * PAGE_SIZE);
const _: () = assert!(STACK_GUARD == STACK_BOTTOM - PAGE_SIZE);
// One 2 MiB region holds the stack and its guard page.
const _: () = assert!(STACK_GUARD >> 21 == (STACK_TOP - 1) >> 21);

/// The most loadable segments with memory a placement holds. The kernel has
/// one per kind of permission (read, read and execute, read and write), so
/// eight leaves room without making the table large.
pub const MAX_SEGMENTS: usize = 8;

/// Why a file cannot be placed as linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file is not a fixed-address executable (gABI ch. 4, "ELF Header",
    /// `ET_EXEC`), so its addresses would need fixing up.
    NotExecutable,
    /// The entry point is below [`KERNEL_BASE`].
    EntryBelowBase,
    /// A segment starts below [`KERNEL_BASE`].
    BelowBase {
        /// The segment's number.
        segment: usize,
    },
    /// A segment's address and file offset are at different places in a
    /// page, so its pages cannot be filled straight from the file (gABI
    /// ch. 5, "Program Loading").
    PageOffsetMismatch {
        /// The segment's number.
        segment: usize,
    },
    /// A segment holds more bytes in the file than it takes in memory.
    MemorySmallerThanFile {
        /// The segment's number.
        segment: usize,
    },
    /// A segment's memory runs past the end of the address range.
    PastTheTop {
        /// The segment's number.
        segment: usize,
    },
    /// A segment's memory uses the last page of the address range, which
    /// the boot stage cannot read as a block: its end would be 2^64.
    LastPage {
        /// The segment's number.
        segment: usize,
    },
    /// More than [`MAX_SEGMENTS`] segments take memory.
    TooManySegments,
    /// Two segments take the same page, which can only have one set of
    /// permissions.
    SharedPage {
        /// The lower-numbered segment.
        first: usize,
        /// The higher-numbered segment.
        second: usize,
    },
    /// The segments take more than [`PAGE_LIMIT`] pages together.
    TooManyPages {
        /// How many they take.
        pages: u64,
    },
    /// The entry point is not inside the memory of a segment that may be
    /// run, so starting the kernel would fault.
    EntryNotExecutable,
}

/// Where one segment goes: the pages it takes at its linked address, and
/// what to copy into them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    /// The segment's number.
    pub segment: usize,
    /// The linked address of its first page.
    pub page: u64,
    /// How many pages it takes.
    pub pages: u64,
    /// Where in the first page its memory starts.
    pub start: u64,
    /// Where its contents start in the file.
    pub offset: u64,
    /// How many bytes of contents the file holds. The rest of the pages are
    /// zero.
    pub file_size: u64,
    /// What its memory may be used for.
    pub flags: Flags,
}

impl Placed {
    const EMPTY: Placed = Placed {
        segment: 0,
        page: 0,
        pages: 0,
        start: 0,
        offset: 0,
        file_size: 0,
        flags: Flags {
            read: false,
            write: false,
            execute: false,
        },
    };

    /// The linked address of its last page.
    fn last_page(&self) -> u64 {
        let more = self.pages.saturating_sub(1).saturating_mul(PAGE_SIZE);
        self.page.saturating_add(more)
    }
}

/// The segments that take memory, in file order, each on pages of its own.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    placed: [Placed; MAX_SEGMENTS],
    count: usize,
}

impl Placement {
    /// The placed segments, in file order. Segments with no memory are left
    /// out.
    pub fn segments(&self) -> &[Placed] {
        self.placed.get(..self.count).unwrap_or_default()
    }

    /// How many pages the segments take together.
    pub fn pages(&self) -> u64 {
        self.segments()
            .iter()
            .fold(0, |sum, placed| sum.saturating_add(placed.pages))
    }
}

/// Checks that `elf` can be placed as linked and says where each segment
/// goes. That the entry point lies in a segment that may be run is checked
/// last, once every segment's layout has passed.
pub fn place(elf: &Elf) -> Result<Placement, Error> {
    place_segments(elf.file_type(), elf.entry(), elf.segments())
}

/// As [`place`], for a file of `file_type` entered at `entry` whose loadable
/// segments are `segments`.
pub fn place_segments(
    file_type: FileType,
    entry: u64,
    segments: impl IntoIterator<Item = Segment>,
) -> Result<Placement, Error> {
    if file_type != FileType::Executable {
        return Err(Error::NotExecutable);
    }
    if entry < KERNEL_BASE {
        return Err(Error::EntryBelowBase);
    }
    let mut placement = Placement {
        placed: [Placed::EMPTY; MAX_SEGMENTS],
        count: 0,
    };
    let mut entered = false;
    for (segment, s) in segments.into_iter().enumerate() {
        entered |= s.flags.execute && s.contains(entry);
        if s.address < KERNEL_BASE {
            return Err(Error::BelowBase { segment });
        }
        if s.address & IN_PAGE != s.offset & IN_PAGE {
            return Err(Error::PageOffsetMismatch { segment });
        }
        if s.file_size > s.memory_size {
            return Err(Error::MemorySmallerThanFile { segment });
        }
        let Some(last_byte) = s.memory_size.checked_sub(1) else {
            continue;
        };
        let last = s
            .address
            .checked_add(last_byte)
            .ok_or(Error::PastTheTop { segment })?;
        if last & !IN_PAGE == LAST_PAGE {
            return Err(Error::LastPage { segment });
        }
        let page = s.address & !IN_PAGE;
        let pages = ((last & !IN_PAGE).saturating_sub(page) / PAGE_SIZE).saturating_add(1);
        let slot = placement
            .placed
            .get_mut(placement.count)
            .ok_or(Error::TooManySegments)?;
        *slot = Placed {
            segment,
            page,
            pages,
            start: s.address & IN_PAGE,
            offset: s.offset,
            file_size: s.file_size,
            flags: s.flags,
        };
        placement.count = placement.count.saturating_add(1);
    }
    let placed = placement.segments();
    for (at, a) in placed.iter().enumerate() {
        for b in placed.iter().skip(at.saturating_add(1)) {
            if a.page <= b.last_page() && b.page <= a.last_page() {
                return Err(Error::SharedPage {
                    first: a.segment,
                    second: b.segment,
                });
            }
        }
    }
    let pages = placement.pages();
    if pages > PAGE_LIMIT {
        return Err(Error::TooManyPages { pages });
    }
    if !entered {
        return Err(Error::EntryNotExecutable);
    }
    Ok(placement)
}

/// The 64-bit FNV-1a hash, by Glenn Fowler, Landon Curt Noll and Kiem-Phong
/// Vo (published at `www.isthe.com/chongo/tech/comp/fnv`, in the public
/// domain). The boot stage uses it to show which bytes it copied.
#[derive(Debug, Clone, Copy)]
pub struct Fnv1a(u64);

impl Fnv1a {
    /// The 64-bit offset basis.
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    /// The 64-bit FNV prime, 2^40 + 2^8 + 0xb3.
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    /// The hash of no bytes.
    pub const fn new() -> Self {
        Fnv1a(Self::BASIS)
    }

    /// Adds `bytes` to the hash: for each byte, exclusive-or it into the
    /// low bits, then multiply by the prime, keeping the low 64 bits.
    pub fn add(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(Self::PRIME);
        }
    }

    /// The hash of the bytes added so far.
    pub fn value(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv1a {
    fn default() -> Self {
        Self::new()
    }
}

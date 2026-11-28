//! The lines the boot stage prints about the kernel file and its copy in
//! memory, built without allocation.
//!
//! This file has no firmware or hardware code, so `xtask` also compiles it
//! on the development machine (with `#[path]`) and `cargo test --package
//! xtask` runs the tests at the bottom.

use bismuth_elf::Error as ElfError;
use bismuth_elf::place::{
    Error as PlaceError, KERNEL_BASE, MAX_SEGMENTS, PAGE_LIMIT, STACK_BOTTOM,
};
use bismuth_paging::{Error as PagingError, Permissions};

/// A step of reading the kernel file that goes through the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Finding the boot services table.
    BootServices,
    /// Asking which device the boot stage was loaded from.
    LoadedImage,
    /// Finding the file system on that device.
    FileSystem,
    /// Opening the file system's root folder.
    OpenVolume,
    /// Opening the kernel file.
    Open,
    /// Finding the kernel file's size.
    Size,
    /// Allocating memory for the file's contents.
    Allocate,
    /// Reading the file's contents.
    Read,
}

/// Why the kernel file could not be read or checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// A firmware call at this step returned this error status.
    Firmware(Step, usize),
    /// A firmware call at this step reported success but handed back a null
    /// pointer, or the table it needed was missing.
    NoInterface(Step),
    /// There is no kernel file.
    Missing,
    /// The kernel file holds no bytes.
    Empty,
    /// The kernel file is larger than the boot stage accepts.
    TooLarge {
        /// The file's size in bytes.
        size: u64,
        /// The largest size accepted.
        limit: u64,
    },
    /// The firmware stopped returning bytes before the end of the file.
    ShortRead {
        /// How many bytes were read.
        read: u64,
        /// The file's size in bytes.
        size: u64,
    },
    /// The ELF reader rejected the file.
    Elf(ElfError),
}

/// Why the kernel could not be copied into memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceFailure {
    /// The file breaks a placement rule.
    Rule(PlaceError),
    /// The firmware refused pages for this segment, with this status.
    Firmware {
        /// The segment's number, as the placement rules count.
        segment: usize,
        /// The firmware's error status.
        status: usize,
    },
    /// The firmware has no page allocation service, or reported success but
    /// handed back page zero, for this segment.
    NoPages {
        /// The segment's number.
        segment: usize,
    },
    /// The segment's contents do not lie inside the file, or would not fit
    /// in its pages where they start.
    Contents {
        /// The segment's number.
        segment: usize,
    },
}

/// Why the kernel could not be given its addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapFailure {
    /// The processor uses 5-level paging, which the table builder does not
    /// support.
    FiveLevel,
    /// The processor cannot forbid running code from a page, which the
    /// table builder relies on for pages that are not code.
    NoExecuteUnsupported,
    /// Turning no-execute on in `IA32_EFER` did not take effect.
    NoExecuteNotOn,
    /// The firmware already uses the top-level entry the kernel goes under.
    TopEntryUsed,
    /// The firmware refused frames for the tables, with this status, or
    /// has no page allocation service.
    NoFrames {
        /// The firmware's error status, if it returned one.
        status: Option<usize>,
    },
    /// The frames the firmware gave are not 4 KiB-aligned or reach past
    /// 2^52.
    FramesUnusable,
    /// The table builder refused to map this segment.
    Map {
        /// The segment's number, in file order among those with memory.
        segment: usize,
        /// The segment's kernel address.
        address: u64,
        /// What the builder returned.
        error: PagingError,
    },
    /// Walking the tables in use found a page of a segment not mapped, or
    /// mapped to another frame or with other permissions than it should be.
    Walk {
        /// The segment's number.
        segment: usize,
        /// The page's kernel address.
        address: u64,
        /// The frame and permissions found, if the page is mapped.
        found: Option<(u64, Permissions)>,
        /// The frame and permissions it should have.
        expected: (u64, Permissions),
    },
    /// A segment's pages cannot be read as one block at their kernel
    /// address, because they end at the top of the address range.
    ReadBack {
        /// The segment's number.
        segment: usize,
    },
    /// The pages read through their kernel addresses sum differently from
    /// the pages as copied.
    Sum {
        /// The sum of the pages as copied.
        copied: u64,
        /// The sum read through the kernel addresses.
        read: u64,
    },
    /// The firmware refused pages for the kernel's stack, with this status,
    /// or has no page allocation service.
    NoStack {
        /// The firmware's error status, if it returned one.
        status: Option<usize>,
    },
    /// The table builder refused to map the stack.
    StackMap {
        /// What the builder returned.
        error: PagingError,
    },
    /// Walking the tables in use found a page of the stack not mapped, or
    /// mapped to another frame or with other permissions than read and
    /// write.
    StackWalk {
        /// The page's kernel address.
        address: u64,
        /// The frame and permissions found, if the page is mapped.
        found: Option<(u64, Permissions)>,
        /// The frame it should lead to.
        expected: u64,
    },
    /// The guard page below the stack is mapped.
    GuardMapped {
        /// The guard page's address.
        address: u64,
    },
}

/// A line of ASCII text in a fixed buffer. Text past the end is dropped.
pub struct Line {
    bytes: [u8; Line::CAPACITY],
    len: usize,
}

impl Line {
    /// Longer than any line this module builds.
    const CAPACITY: usize = 160;

    fn new(text: &str) -> Self {
        let mut line = Line {
            bytes: [0; Line::CAPACITY],
            len: 0,
        };
        line.push(text);
        line
    }

    /// The text so far.
    pub fn as_str(&self) -> &str {
        let bytes = self.bytes.get(..self.len).unwrap_or_default();
        core::str::from_utf8(bytes).unwrap_or_default()
    }

    /// Adds ASCII text. Other characters are written as `?`, so the buffer
    /// always holds valid text.
    fn push(&mut self, text: &str) -> &mut Self {
        for byte in text.bytes() {
            let byte = if byte.is_ascii() { byte } else { b'?' };
            if let Some(slot) = self.bytes.get_mut(self.len) {
                *slot = byte;
                self.len += 1;
            }
        }
        self
    }

    /// Adds `value` in base `radix` (2 to 16), without leading zeros.
    fn push_number(&mut self, mut value: u64, radix: u64) -> &mut Self {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        // 64 binary digits is the longest a `u64` can be.
        let mut digits = [0u8; 64];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = DIGITS[(value % radix) as usize];
            value /= radix;
            if value == 0 || start == 0 {
                break;
            }
        }
        let text = core::str::from_utf8(&digits[start..]).unwrap_or_default();
        self.push(text)
    }

    fn push_decimal(&mut self, value: u64) -> &mut Self {
        self.push_number(value, 10)
    }

    fn push_hex(&mut self, value: u64) -> &mut Self {
        self.push("0x").push_number(value, 16)
    }
}

/// `BISMUTH KERNEL FILE OK entry=0x<hex> segments=<n> bytes=<n>`.
pub fn ok_line(entry: u64, segments: usize, bytes: usize) -> Line {
    let mut line = Line::new("BISMUTH KERNEL FILE OK entry=");
    line.push_hex(entry)
        .push(" segments=")
        .push_decimal(segments as u64)
        .push(" bytes=")
        .push_decimal(bytes as u64);
    line
}

/// `BISMUTH KERNEL FILE ERROR: <short reason>`.
pub fn error_line(failure: &Failure) -> Line {
    let mut line = Line::new("BISMUTH KERNEL FILE ERROR: ");
    match *failure {
        Failure::Firmware(step, status) => {
            line.push(step_text(step))
                .push(": status ")
                .push_hex(status as u64);
        }
        Failure::NoInterface(step) => {
            line.push(step_text(step)).push(": no interface");
        }
        Failure::Missing => {
            line.push("file not found");
        }
        Failure::Empty => {
            line.push("file is empty");
        }
        Failure::TooLarge { size, limit } => {
            line.push("file is too large: ")
                .push_decimal(size)
                .push(" bytes, limit ")
                .push_decimal(limit);
        }
        Failure::ShortRead { read, size } => {
            line.push("reading failed: got ")
                .push_decimal(read)
                .push(" of ")
                .push_decimal(size)
                .push(" bytes");
        }
        Failure::Elf(error) => {
            let (name, index) = elf_error_name(error);
            line.push("ELF reader: ").push(name);
            if let Some(index) = index {
                line.push(" in program header ")
                    .push_decimal(u64::from(index));
            }
        }
    }
    line
}

/// `BISMUTH KERNEL COPIED segments=<n> pages=<n> sum=0x<hex>`.
pub fn copied_line(segments: usize, pages: u64, sum: u64) -> Line {
    let mut line = Line::new("BISMUTH KERNEL COPIED segments=");
    line.push_decimal(segments as u64)
        .push(" pages=")
        .push_decimal(pages)
        .push(" sum=")
        .push_hex(sum);
    line
}

/// `BISMUTH KERNEL PLACED segments=<n> pages=<n> sum=0x<hex>
/// perms=<list> wp=<0|1>`: `perms` gives each segment's permissions, in
/// file order, as `r`, then `w` or `-`, then `x` or `-`.
pub fn placed_line(
    pages: u64,
    sum: u64,
    permissions: impl ExactSizeIterator<Item = Permissions>,
    write_protect: bool,
) -> Line {
    let mut line = Line::new("BISMUTH KERNEL PLACED segments=");
    line.push_decimal(permissions.len() as u64)
        .push(" pages=")
        .push_decimal(pages)
        .push(" sum=")
        .push_hex(sum)
        .push(" perms=");
    for (at, permissions) in permissions.enumerate() {
        line.push(if at == 0 { "" } else { "," })
            .push(permissions_text(permissions));
    }
    line.push(if write_protect { " wp=1" } else { " wp=0" });
    line
}

/// `BISMUTH KERNEL MAP ERROR: <short reason>`.
pub fn map_error_line(failure: &MapFailure) -> Line {
    let mut line = Line::new("BISMUTH KERNEL MAP ERROR: ");
    match *failure {
        MapFailure::FiveLevel => {
            line.push("5-level paging is on");
        }
        MapFailure::NoExecuteUnsupported => {
            line.push("the processor cannot mark pages no-execute");
        }
        MapFailure::NoExecuteNotOn => {
            line.push("no-execute did not turn on");
        }
        MapFailure::TopEntryUsed => {
            line.push("top-level entry 511 is already used");
        }
        MapFailure::NoFrames { status } => {
            line.push("no frames for the tables: ");
            match status {
                Some(status) => line.push("status ").push_hex(status as u64),
                None => line.push("no interface"),
            };
        }
        MapFailure::FramesUnusable => {
            line.push("the frames for the tables are unaligned or above 2^52");
        }
        MapFailure::Map {
            segment,
            address,
            error,
        } => {
            line.push("could not map segment ")
                .push_decimal(segment as u64)
                .push(" at ")
                .push_hex(address)
                .push(": ");
            push_paging_error(&mut line, error);
        }
        MapFailure::Walk {
            segment,
            address,
            found,
            expected,
        } => {
            line.push("segment ")
                .push_decimal(segment as u64)
                .push(" page ")
                .push_hex(address)
                .push(" is ");
            push_found(&mut line, found);
            line.push(", expected ")
                .push_hex(expected.0)
                .push(" ")
                .push(permissions_text(expected.1));
        }
        MapFailure::ReadBack { segment } => {
            line.push("segment ")
                .push_decimal(segment as u64)
                .push(" ends at the top of the address range and cannot be read back");
        }
        MapFailure::Sum { copied, read } => {
            line.push("sum read at the kernel addresses ")
                .push_hex(read)
                .push(" differs from the copied sum ")
                .push_hex(copied);
        }
        MapFailure::NoStack { status } => {
            line.push("no pages for the stack: ");
            match status {
                Some(status) => line.push("status ").push_hex(status as u64),
                None => line.push("no interface"),
            };
        }
        MapFailure::StackMap { error } => {
            line.push("could not map the stack at ")
                .push_hex(STACK_BOTTOM)
                .push(": ");
            push_paging_error(&mut line, error);
        }
        MapFailure::StackWalk {
            address,
            found,
            expected,
        } => {
            line.push("stack page ").push_hex(address).push(" is ");
            push_found(&mut line, found);
            line.push(", expected ").push_hex(expected).push(" rw-");
        }
        MapFailure::GuardMapped { address } => {
            line.push("the guard page ")
                .push_hex(address)
                .push(" below the stack is mapped");
        }
    }
    line
}

/// The table builder's error by its name in `bismuth-paging`.
fn push_paging_error(line: &mut Line, error: PagingError) {
    match error {
        PagingError::Unaligned => line.push("Unaligned"),
        PagingError::Empty => line.push("Empty"),
        PagingError::NonCanonical => line.push("NonCanonical"),
        PagingError::PhysicalTooLarge => line.push("PhysicalTooLarge"),
        PagingError::AlreadyMapped(at) => line.push("AlreadyMapped at ").push_hex(at),
        PagingError::LargePage(at) => line.push("LargePage at ").push_hex(at),
        PagingError::OutOfFrames => line.push("OutOfFrames"),
    };
}

/// `at <frame> <permissions>`, or `not mapped`.
fn push_found(line: &mut Line, found: Option<(u64, Permissions)>) {
    match found {
        Some((frame, permissions)) => line
            .push("at ")
            .push_hex(frame)
            .push(" ")
            .push(permissions_text(permissions)),
        None => line.push("not mapped"),
    };
}

/// `BISMUTH KERNEL JUMP entry=0x<hex> stack=0x<hex>`: where the boot stage
/// is about to jump, and the top of the stack the kernel starts on.
pub fn jump_line(entry: u64, stack_top: u64) -> Line {
    let mut line = Line::new("BISMUTH KERNEL JUMP entry=");
    line.push_hex(entry).push(" stack=").push_hex(stack_top);
    line
}

/// `r--`, `rw-`, `r-x` or `rwx`: reading is always allowed.
fn permissions_text(permissions: Permissions) -> &'static str {
    match (permissions.write, permissions.execute) {
        (false, false) => "r--",
        (true, false) => "rw-",
        (false, true) => "r-x",
        (true, true) => "rwx",
    }
}

/// `BISMUTH KERNEL PLACE ERROR: <short reason>`.
pub fn place_error_line(failure: &PlaceFailure) -> Line {
    let mut line = Line::new("BISMUTH KERNEL PLACE ERROR: ");
    let segment = |line: &mut Line, segment: usize, reason: &str| {
        line.push("segment ")
            .push_decimal(segment as u64)
            .push(reason);
    };
    match *failure {
        PlaceFailure::Firmware { segment, status } => {
            line.push("no pages for segment ")
                .push_decimal(segment as u64)
                .push(": status ")
                .push_hex(status as u64);
        }
        PlaceFailure::NoPages { segment } => {
            line.push("no pages for segment ")
                .push_decimal(segment as u64)
                .push(": no interface");
        }
        PlaceFailure::Contents { segment: s } => {
            segment(&mut line, s, " lies outside the file or its pages");
        }
        PlaceFailure::Rule(PlaceError::NotExecutable) => {
            line.push("not a fixed-address executable");
        }
        PlaceFailure::Rule(PlaceError::EntryBelowBase) => {
            line.push("entry point below ").push_hex(KERNEL_BASE);
        }
        PlaceFailure::Rule(PlaceError::BelowBase { segment: s }) => {
            segment(&mut line, s, " below ");
            line.push_hex(KERNEL_BASE);
        }
        PlaceFailure::Rule(PlaceError::PageOffsetMismatch { segment: s }) => {
            segment(
                &mut line,
                s,
                " at a different place in its page than in the file",
            );
        }
        PlaceFailure::Rule(PlaceError::MemorySmallerThanFile { segment: s }) => {
            segment(&mut line, s, " has more bytes in the file than in memory");
        }
        PlaceFailure::Rule(PlaceError::PastTheTop { segment: s }) => {
            segment(&mut line, s, " runs past the end of the address range");
        }
        PlaceFailure::Rule(PlaceError::LastPage { segment: s }) => {
            segment(&mut line, s, " uses the last page of the address range");
        }
        PlaceFailure::Rule(PlaceError::TooManySegments) => {
            line.push("more than ")
                .push_decimal(MAX_SEGMENTS as u64)
                .push(" segments");
        }
        PlaceFailure::Rule(PlaceError::SharedPage { first, second }) => {
            line.push("segments ")
                .push_decimal(first as u64)
                .push(" and ")
                .push_decimal(second as u64)
                .push(" share a page");
        }
        PlaceFailure::Rule(PlaceError::TooManyPages { pages }) => {
            line.push_decimal(pages)
                .push(" pages, more than the limit of ")
                .push_decimal(PAGE_LIMIT);
        }
        PlaceFailure::Rule(PlaceError::EntryNotExecutable) => {
            line.push("entry point not in an executable segment");
        }
    }
    line
}

/// What was being done when a firmware call failed.
fn step_text(step: Step) -> &'static str {
    match step {
        Step::BootServices => "no boot services",
        Step::LoadedImage => "could not find the boot device",
        Step::FileSystem => "could not find the boot device's file system",
        Step::OpenVolume => "could not open the boot volume",
        Step::Open => "could not open the file",
        Step::Size => "could not find the file's size",
        Step::Allocate => "could not allocate memory for the file",
        Step::Read => "reading failed",
    }
}

/// The ELF reader's error by its name in `bismuth-elf`, and the program
/// header it concerns, if any. `UnsupportedType`'s value is left out.
fn elf_error_name(error: ElfError) -> (&'static str, Option<u16>) {
    match error {
        ElfError::Truncated => ("Truncated", None),
        ElfError::BadSignature => ("BadSignature", None),
        ElfError::NotElf64 => ("NotElf64", None),
        ElfError::NotLittleEndian => ("NotLittleEndian", None),
        ElfError::BadVersion => ("BadVersion", None),
        ElfError::NotX86_64 => ("NotX86_64", None),
        ElfError::UnsupportedType(_) => ("UnsupportedType", None),
        ElfError::BadHeaderSize => ("BadHeaderSize", None),
        ElfError::BadProgramHeaderSize => ("BadProgramHeaderSize", None),
        ElfError::TooManyProgramHeaders => ("TooManyProgramHeaders", None),
        ElfError::ProgramHeadersOutsideFile => ("ProgramHeadersOutsideFile", None),
        ElfError::SegmentOutsideFile { index } => ("SegmentOutsideFile", Some(index)),
        ElfError::MemorySmallerThanFile { index } => ("MemorySmallerThanFile", Some(index)),
        ElfError::BadAlignment { index } => ("BadAlignment", Some(index)),
        ElfError::AddressOverflow { index } => ("AddressOverflow", Some(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(failure: Failure) -> String {
        error_line(&failure).as_str().to_string()
    }

    #[test]
    fn the_ok_line_has_the_documented_shape() {
        assert_eq!(
            ok_line(0x1280, 3, 6152).as_str(),
            "BISMUTH KERNEL FILE OK entry=0x1280 segments=3 bytes=6152"
        );
    }

    #[test]
    fn numbers_at_the_edges() {
        assert_eq!(
            ok_line(0, 0, 0).as_str(),
            "BISMUTH KERNEL FILE OK entry=0x0 segments=0 bytes=0"
        );
        assert_eq!(
            ok_line(u64::MAX, 10, 1_000_000).as_str(),
            "BISMUTH KERNEL FILE OK entry=0xffffffffffffffff segments=10 bytes=1000000"
        );
        assert_eq!(Line::new("").push_number(u64::MAX, 2).as_str().len(), 64);
        assert_eq!(
            Line::new("").push_decimal(u64::MAX).as_str(),
            "18446744073709551615"
        );
    }

    #[test]
    fn each_failure_has_its_own_reason() {
        let prefix = "BISMUTH KERNEL FILE ERROR: ";
        let cases = [
            (Failure::Missing, "file not found"),
            (Failure::Empty, "file is empty"),
            (
                Failure::TooLarge {
                    size: 70_000_000,
                    limit: 67_108_864,
                },
                "file is too large: 70000000 bytes, limit 67108864",
            ),
            (
                Failure::Firmware(Step::Read, 0x8000_0000_0000_0007),
                "reading failed: status 0x8000000000000007",
            ),
            (
                Failure::ShortRead {
                    read: 100,
                    size: 6152,
                },
                "reading failed: got 100 of 6152 bytes",
            ),
            (
                Failure::NoInterface(Step::FileSystem),
                "could not find the boot device's file system: no interface",
            ),
            (
                Failure::Elf(ElfError::BadSignature),
                "ELF reader: BadSignature",
            ),
            (
                Failure::Elf(ElfError::SegmentOutsideFile { index: 2 }),
                "ELF reader: SegmentOutsideFile in program header 2",
            ),
            (
                Failure::Elf(ElfError::UnsupportedType(4)),
                "ELF reader: UnsupportedType",
            ),
        ];
        for (failure, reason) in cases {
            assert_eq!(error(failure), format!("{prefix}{reason}"));
        }
    }

    #[test]
    fn every_step_reads_differently() {
        let steps = [
            Step::BootServices,
            Step::LoadedImage,
            Step::FileSystem,
            Step::OpenVolume,
            Step::Open,
            Step::Size,
            Step::Allocate,
            Step::Read,
        ];
        let mut texts: Vec<_> = steps.iter().map(|step| step_text(*step)).collect();
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), steps.len());
    }

    #[test]
    fn the_copied_line_has_the_documented_shape() {
        assert_eq!(
            copied_line(2, 3, 0xcbf2_9ce4_8422_2325).as_str(),
            "BISMUTH KERNEL COPIED segments=2 pages=3 sum=0xcbf29ce484222325"
        );
    }

    #[test]
    fn each_placement_failure_has_its_own_reason() {
        let prefix = "BISMUTH KERNEL PLACE ERROR: ";
        let cases = [
            (PlaceError::NotExecutable, "not a fixed-address executable"),
            (
                PlaceError::EntryBelowBase,
                "entry point below 0xffffffff80000000",
            ),
            (
                PlaceError::BelowBase { segment: 2 },
                "segment 2 below 0xffffffff80000000",
            ),
            (
                PlaceError::PageOffsetMismatch { segment: 1 },
                "segment 1 at a different place in its page than in the file",
            ),
            (
                PlaceError::MemorySmallerThanFile { segment: 1 },
                "segment 1 has more bytes in the file than in memory",
            ),
            (
                PlaceError::PastTheTop { segment: 0 },
                "segment 0 runs past the end of the address range",
            ),
            (
                PlaceError::LastPage { segment: 1 },
                "segment 1 uses the last page of the address range",
            ),
            (PlaceError::TooManySegments, "more than 8 segments"),
            (
                PlaceError::SharedPage {
                    first: 1,
                    second: 2,
                },
                "segments 1 and 2 share a page",
            ),
            (
                PlaceError::TooManyPages { pages: 16385 },
                "16385 pages, more than the limit of 16384",
            ),
            (
                PlaceError::EntryNotExecutable,
                "entry point not in an executable segment",
            ),
        ];
        for (error, reason) in cases {
            let line = place_error_line(&PlaceFailure::Rule(error));
            assert_eq!(line.as_str(), format!("{prefix}{reason}"));
        }
        let firmware = PlaceFailure::Firmware {
            segment: 1,
            status: 0x8000_0000_0000_0009,
        };
        assert_eq!(
            place_error_line(&firmware).as_str(),
            format!("{prefix}no pages for segment 1: status 0x8000000000000009")
        );
        let none = PlaceFailure::NoPages { segment: 0 };
        assert_eq!(
            place_error_line(&none).as_str(),
            format!("{prefix}no pages for segment 0: no interface")
        );
        let contents = PlaceFailure::Contents { segment: 2 };
        assert_eq!(
            place_error_line(&contents).as_str(),
            format!("{prefix}segment 2 lies outside the file or its pages")
        );
    }

    const READ: Permissions = Permissions {
        write: false,
        execute: false,
    };
    const READ_EXECUTE: Permissions = Permissions {
        write: false,
        execute: true,
    };

    #[test]
    fn the_placed_line_has_the_documented_shape() {
        let all = [
            READ,
            READ_EXECUTE,
            Permissions {
                write: true,
                execute: false,
            },
            Permissions {
                write: true,
                execute: true,
            },
        ];
        assert_eq!(
            placed_line(5, 0xcbf2_9ce4_8422_2325, all.into_iter(), true).as_str(),
            "BISMUTH KERNEL PLACED segments=4 pages=5 sum=0xcbf29ce484222325 \
             perms=r--,r-x,rw-,rwx wp=1"
        );
        assert_eq!(
            placed_line(1, 0x1, [READ].into_iter(), false).as_str(),
            "BISMUTH KERNEL PLACED segments=1 pages=1 sum=0x1 perms=r-- wp=0"
        );
        // The longest: eight segments and the most pages.
        let longest = placed_line(PAGE_LIMIT, u64::MAX, [READ; MAX_SEGMENTS].into_iter(), true);
        assert!(
            longest.as_str().ends_with("r--,r-- wp=1"),
            "{}",
            longest.as_str()
        );
        assert!(longest.as_str().len() < Line::CAPACITY);
    }

    #[test]
    fn each_map_failure_has_its_own_reason() {
        let prefix = "BISMUTH KERNEL MAP ERROR: ";
        let base = KERNEL_BASE;
        let cases = [
            (MapFailure::FiveLevel, "5-level paging is on"),
            (
                MapFailure::NoExecuteUnsupported,
                "the processor cannot mark pages no-execute",
            ),
            (MapFailure::NoExecuteNotOn, "no-execute did not turn on"),
            (
                MapFailure::TopEntryUsed,
                "top-level entry 511 is already used",
            ),
            (
                MapFailure::NoFrames {
                    status: Some(0x8000_0000_0000_0009),
                },
                "no frames for the tables: status 0x8000000000000009",
            ),
            (
                MapFailure::NoFrames { status: None },
                "no frames for the tables: no interface",
            ),
            (
                MapFailure::FramesUnusable,
                "the frames for the tables are unaligned or above 2^52",
            ),
            (
                MapFailure::Map {
                    segment: 1,
                    address: base + 0x1000,
                    error: PagingError::AlreadyMapped(base + 0x2000),
                },
                "could not map segment 1 at 0xffffffff80001000: \
                 AlreadyMapped at 0xffffffff80002000",
            ),
            (
                MapFailure::Map {
                    segment: 0,
                    address: base,
                    error: PagingError::OutOfFrames,
                },
                "could not map segment 0 at 0xffffffff80000000: OutOfFrames",
            ),
            (
                MapFailure::Walk {
                    segment: 1,
                    address: base + 0x3000,
                    found: None,
                    expected: (0x7000, READ_EXECUTE),
                },
                "segment 1 page 0xffffffff80003000 is not mapped, expected 0x7000 r-x",
            ),
            (
                MapFailure::Walk {
                    segment: 0,
                    address: base,
                    found: Some((0x6000, READ_EXECUTE)),
                    expected: (0x5000, READ),
                },
                "segment 0 page 0xffffffff80000000 is at 0x6000 r-x, expected 0x5000 r--",
            ),
            (
                MapFailure::ReadBack { segment: 3 },
                "segment 3 ends at the top of the address range and cannot be read back",
            ),
            (
                MapFailure::Sum {
                    copied: 0x12,
                    read: 0x34,
                },
                "sum read at the kernel addresses 0x34 differs from the copied sum 0x12",
            ),
            (
                MapFailure::NoStack {
                    status: Some(0x8000_0000_0000_0009),
                },
                "no pages for the stack: status 0x8000000000000009",
            ),
            (
                MapFailure::NoStack { status: None },
                "no pages for the stack: no interface",
            ),
            (
                MapFailure::StackMap {
                    error: PagingError::AlreadyMapped(STACK_BOTTOM),
                },
                "could not map the stack at 0xffffffff7fff0000: \
                 AlreadyMapped at 0xffffffff7fff0000",
            ),
            (
                MapFailure::StackWalk {
                    address: STACK_BOTTOM,
                    found: None,
                    expected: 0x9000,
                },
                "stack page 0xffffffff7fff0000 is not mapped, expected 0x9000 rw-",
            ),
            (
                MapFailure::StackWalk {
                    address: STACK_BOTTOM + 0x1000,
                    found: Some((0xa000, READ)),
                    expected: 0xa000,
                },
                "stack page 0xffffffff7fff1000 is at 0xa000 r--, expected 0xa000 rw-",
            ),
            (
                MapFailure::GuardMapped {
                    address: STACK_BOTTOM - 0x1000,
                },
                "the guard page 0xffffffff7ffef000 below the stack is mapped",
            ),
        ];
        for (failure, reason) in cases {
            assert_eq!(
                map_error_line(&failure).as_str(),
                format!("{prefix}{reason}")
            );
        }
        // Every builder error has a name of its own.
        let mut names: Vec<_> = [
            PagingError::Unaligned,
            PagingError::Empty,
            PagingError::NonCanonical,
            PagingError::PhysicalTooLarge,
            PagingError::AlreadyMapped(0),
            PagingError::LargePage(0),
            PagingError::OutOfFrames,
        ]
        .map(|error| {
            let failure = MapFailure::Map {
                segment: 0,
                address: 0,
                error,
            };
            map_error_line(&failure).as_str().to_string()
        })
        .to_vec();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn the_jump_line_has_the_documented_shape() {
        assert_eq!(
            jump_line(0xffff_ffff_8000_1190, 0xffff_ffff_8000_0000).as_str(),
            "BISMUTH KERNEL JUMP entry=0xffffffff80001190 stack=0xffffffff80000000"
        );
        let longest = jump_line(u64::MAX, u64::MAX);
        assert!(longest.as_str().len() < Line::CAPACITY);
    }

    #[test]
    fn the_longest_line_fits() {
        let longest = error(Failure::Firmware(Step::FileSystem, usize::MAX));
        assert!(longest.ends_with("status 0xffffffffffffffff"), "{longest}");
        assert!(longest.len() < Line::CAPACITY);
    }

    #[test]
    fn text_past_the_end_is_dropped_and_non_ascii_replaced() {
        let mut line = Line::new("é");
        assert_eq!(line.as_str(), "??");
        for _ in 0..Line::CAPACITY {
            line.push("ab");
        }
        assert_eq!(line.as_str().len(), Line::CAPACITY);
    }
}

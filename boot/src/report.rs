//! The line the boot stage prints about the kernel file, built without
//! allocation.
//!
//! This file has no firmware or hardware code, so `xtask` also compiles it
//! on the development machine (with `#[path]`) and `cargo test --package
//! xtask` runs the tests at the bottom.

use bismuth_elf::Error as ElfError;

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

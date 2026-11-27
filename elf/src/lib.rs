//! Reads what a loader needs from an ELF file: the file header and the
//! program headers. Sections, symbols and relocations are not read.
//!
//! Only little-endian 64-bit files for x86-64 are accepted. The input is
//! treated as hostile: any bytes give either an error or a description whose
//! loadable segments all lie inside the file. Nothing here can panic, and the
//! lints below keep it that way.
//!
//! The layouts come from the System V ABI generic specification (the gABI),
//! chapter 4 "Object Files" and chapter 5 "Program Loading and Dynamic
//! Linking", and from the AMD64 processor supplement (the psABI), chapter 4
//! "Object Files".

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

#[cfg(test)]
mod tests;

// gABI ch. 4, "ELF Header": the size of `Elf64_Ehdr`, from the sizes in
// "Data Representation" (Elf64_Half 2, Elf64_Word 4, Elf64_Addr and
// Elf64_Off 8 bytes).
const HEADER_SIZE: usize = 64;
// gABI ch. 5, "Program Header": the size of `Elf64_Phdr`.
const PROGRAM_HEADER_SIZE: usize = 56;

// gABI ch. 4, "ELF Identification": the `e_ident` bytes.
const MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;

// gABI ch. 4, "ELF Header": `e_type` values a loader can run.
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
// psABI ch. 4, "Machine Information": the x86-64 `e_machine` value.
const EM_X86_64: u16 = 62;
// gABI ch. 4, "ELF Header", `e_phnum`: the count does not fit and is kept in
// section header 0, which this reader does not read.
const PN_XNUM: u16 = 0xFFFF;

// gABI ch. 5, "Program Header": a loadable segment.
const PT_LOAD: u32 = 1;
// gABI ch. 5, "Segment Permissions".
const PF_X: u32 = 0x1;
const PF_W: u32 = 0x2;
const PF_R: u32 = 0x4;

/// What is wrong with a file the reader rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file is shorter than the ELF file header.
    Truncated,
    /// The file does not start with the ELF signature.
    BadSignature,
    /// The file is not a 64-bit ELF file.
    NotElf64,
    /// The file is not little-endian.
    NotLittleEndian,
    /// The file's ELF version is not the current one.
    BadVersion,
    /// The file is for another processor than x86-64.
    NotX86_64,
    /// The file is not a program (`e_type`, given here, is not 2 or 3).
    UnsupportedType(u16),
    /// The file header gives a size other than 64 bytes for itself.
    BadHeaderSize,
    /// The file header gives a size other than 56 bytes for a program header.
    BadProgramHeaderSize,
    /// The program header count is kept outside the file header.
    TooManyProgramHeaders,
    /// The program header table does not lie inside the file.
    ProgramHeadersOutsideFile,
    /// A loadable segment's contents do not lie inside the file.
    SegmentOutsideFile {
        /// The program header's position in the table.
        index: u16,
    },
    /// A loadable segment needs less memory than its contents fill.
    MemorySmallerThanFile {
        /// The program header's position in the table.
        index: u16,
    },
    /// A loadable segment's alignment is not a power of two, or its address
    /// and file offset disagree modulo the alignment.
    BadAlignment {
        /// The program header's position in the table.
        index: u16,
    },
    /// A loadable segment's memory runs past the end of the address space.
    AddressOverflow {
        /// The program header's position in the table.
        index: u16,
    },
}

/// The kind of program a file holds (gABI ch. 4, "ELF Header", `e_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// `ET_EXEC`: an executable that runs at the addresses it names.
    Executable,
    /// `ET_DYN`: a shared object, which is also how a position-independent
    /// executable is stored.
    SharedObject,
}

/// What a segment's memory may be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    /// The memory may be read.
    pub read: bool,
    /// The memory may be written.
    pub write: bool,
    /// The memory may be run as code.
    pub execute: bool,
}

/// A loadable segment: bytes from the file to place in memory.
///
/// `offset + file_size` never overflows and is at most the file's length,
/// and `memory_size` is at least `file_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Where the contents start in the file.
    pub offset: u64,
    /// How many bytes of contents the file holds.
    pub file_size: u64,
    /// The virtual address the memory starts at.
    pub address: u64,
    /// How many bytes of memory the segment takes. The bytes past
    /// `file_size` are zero.
    pub memory_size: u64,
    /// The alignment of `address` and `offset`; 0 or 1 means none.
    pub align: u64,
    /// What the memory may be used for.
    pub flags: Flags,
}

impl Segment {
    /// True if `address` lies in this segment's memory.
    pub fn contains(&self, address: u64) -> bool {
        address
            .checked_sub(self.address)
            .is_some_and(|distance| distance < self.memory_size)
    }
}

/// A checked ELF file.
#[derive(Debug, Clone, Copy)]
pub struct Elf<'a> {
    bytes: &'a [u8],
    file_type: FileType,
    entry: u64,
    table: usize,
    count: u16,
}

impl<'a> Elf<'a> {
    /// Checks the file header and every loadable segment of `bytes`.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        // gABI ch. 4, "ELF Identification".
        let Some([m0, m1, m2, m3, class, data, version, ..]) = bytes.first_chunk::<16>() else {
            return Err(Error::Truncated);
        };
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if [*m0, *m1, *m2, *m3] != MAGIC {
            return Err(Error::BadSignature);
        }
        if *class != ELFCLASS64 {
            return Err(Error::NotElf64);
        }
        if *data != ELFDATA2LSB {
            return Err(Error::NotLittleEndian);
        }
        // gABI ch. 4, "ELF Header": the `Elf64_Ehdr` field offsets.
        if *version != EV_CURRENT || read_u32(bytes, 20)? != u32::from(EV_CURRENT) {
            return Err(Error::BadVersion);
        }
        if read_u16(bytes, 18)? != EM_X86_64 {
            return Err(Error::NotX86_64);
        }
        let file_type = match read_u16(bytes, 16)? {
            ET_EXEC => FileType::Executable,
            ET_DYN => FileType::SharedObject,
            other => return Err(Error::UnsupportedType(other)),
        };
        if usize::from(read_u16(bytes, 52)?) != HEADER_SIZE {
            return Err(Error::BadHeaderSize);
        }
        if usize::from(read_u16(bytes, 54)?) != PROGRAM_HEADER_SIZE {
            return Err(Error::BadProgramHeaderSize);
        }
        let count = read_u16(bytes, 56)?;
        if count == PN_XNUM {
            return Err(Error::TooManyProgramHeaders);
        }
        let table =
            usize::try_from(read_u64(bytes, 32)?).map_err(|_| Error::ProgramHeadersOutsideFile)?;
        let table_end = usize::from(count)
            .checked_mul(PROGRAM_HEADER_SIZE)
            .and_then(|length| length.checked_add(table))
            .ok_or(Error::ProgramHeadersOutsideFile)?;
        if table_end > bytes.len() {
            return Err(Error::ProgramHeadersOutsideFile);
        }
        let elf = Elf {
            bytes,
            file_type,
            entry: read_u64(bytes, 24)?,
            table,
            count,
        };
        for index in 0..count {
            elf.load_segment(index)?;
        }
        Ok(elf)
    }

    /// The virtual address execution starts at.
    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// The kind of program the file holds.
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    /// The loadable segments, in the order of the program header table.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + use<'a> {
        let elf = *self;
        (0..self.count).filter_map(move |index| elf.load_segment(index).ok().flatten())
    }

    /// The loadable segment whose memory holds the entry point, if any.
    pub fn entry_segment(&self) -> Option<Segment> {
        self.segments().find(|segment| segment.contains(self.entry))
    }

    /// Reads program header `index` and returns it if it is a loadable
    /// segment, after checking it.
    fn load_segment(&self, index: u16) -> Result<Option<Segment>, Error> {
        let at = usize::from(index)
            .checked_mul(PROGRAM_HEADER_SIZE)
            .and_then(|start| start.checked_add(self.table))
            .ok_or(Error::ProgramHeadersOutsideFile)?;
        let header = self
            .bytes
            .get(at..)
            .and_then(|rest| rest.first_chunk::<PROGRAM_HEADER_SIZE>())
            .ok_or(Error::ProgramHeadersOutsideFile)?;
        // gABI ch. 5, "Program Header": the `Elf64_Phdr` field offsets.
        if read_u32(header, 0)? != PT_LOAD {
            return Ok(None);
        }
        let flags = read_u32(header, 4)?;
        let segment = Segment {
            offset: read_u64(header, 8)?,
            address: read_u64(header, 16)?,
            file_size: read_u64(header, 32)?,
            memory_size: read_u64(header, 40)?,
            align: read_u64(header, 48)?,
            flags: Flags {
                read: flags & PF_R != 0,
                write: flags & PF_W != 0,
                execute: flags & PF_X != 0,
            },
        };
        let end = segment.offset.checked_add(segment.file_size);
        if !end.is_some_and(|end| usize::try_from(end).is_ok_and(|end| end <= self.bytes.len())) {
            return Err(Error::SegmentOutsideFile { index });
        }
        if segment.memory_size < segment.file_size {
            return Err(Error::MemorySmallerThanFile { index });
        }
        if segment.address.checked_add(segment.memory_size).is_none() {
            return Err(Error::AddressOverflow { index });
        }
        // gABI ch. 5, "Program Header", `p_align`: 0 and 1 mean no alignment;
        // otherwise a power of two, with `p_vaddr` equal to `p_offset`
        // modulo `p_align`.
        if segment.align > 1
            && (!segment.align.is_power_of_two()
                || segment.address.checked_rem(segment.align)
                    != segment.offset.checked_rem(segment.align))
        {
            return Err(Error::BadAlignment { index });
        }
        Ok(Some(segment))
    }
}

/// Reads a little-endian `u16` at `at`. gABI ch. 4, "Data Encoding",
/// `ELFDATA2LSB`, for this and the two below.
fn read_u16(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let chunk = bytes.get(at..).and_then(|rest| rest.first_chunk());
    chunk
        .map(|chunk| u16::from_le_bytes(*chunk))
        .ok_or(Error::Truncated)
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let chunk = bytes.get(at..).and_then(|rest| rest.first_chunk());
    chunk
        .map(|chunk| u32::from_le_bytes(*chunk))
        .ok_or(Error::Truncated)
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, Error> {
    let chunk = bytes.get(at..).and_then(|rest| rest.first_chunk());
    chunk
        .map(|chunk| u64::from_le_bytes(*chunk))
        .ok_or(Error::Truncated)
}

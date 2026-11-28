//! Reads the first ACPI tables: the root pointer, the header every other
//! table starts with, the table of tables (RSDT or XSDT) and the interrupt
//! controller table (MADT). Nothing else is parsed.
//!
//! The reader never touches memory itself. Every function takes bytes the
//! caller has already obtained and returns views that lie inside them. The
//! input is treated as hostile: any bytes give either an error or a correct
//! view. Nothing here allocates or can panic, and the lints below keep it
//! that way.
//!
//! The layouts come from the ACPI specification, version 6.5, chapter 5
//! "ACPI Software Programming Model", in the sections named at each item.
//! All fields are little-endian and need not be aligned, so every field is
//! read byte by byte.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result,
    clippy::string_slice,
    clippy::fallible_impl_from
)]
// No lint covers `assert!` outside a function returning `Result`,
// `split_at`, `copy_from_slice`, `chunks` or `windows`; the reader uses
// none of them.

#[cfg(test)]
mod tests;

// Section 5.2.5.3: the root pointer is 20 bytes in revision 0 and at least
// 36 from revision 2.
const RSDP_V1_SIZE: usize = 20;
const RSDP_V2_SIZE: usize = 36;
// Section 5.2.6: the header every other table starts with.
const HEADER_SIZE: usize = 36;
// Section 5.2.12: the interrupt controller structures start at offset 44,
// after the local interrupt controller address and the flags.
const MADT_ENTRIES_AT: usize = 44;

/// What is wrong with bytes the reader rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than the fixed part of the structure.
    Truncated,
    /// The structure does not start with the signature expected.
    BadSignature,
    /// The bytes covered by the checksum do not sum to zero modulo 256: the
    /// first 20 bytes of a root pointer, or the whole of a table.
    BadChecksum,
    /// The whole of a revision 2 root pointer does not sum to zero.
    BadExtendedChecksum,
    /// The stated length is shorter than the structure's fixed part.
    LengthTooShort,
    /// The stated length is longer than the bytes given.
    LengthPastEnd,
    /// The root pointer gives no table of tables: the address it says to
    /// use is zero.
    NoRootTable,
    /// The body of a table of tables is not a whole number of entries.
    PartialEntry,
    /// An interrupt controller structure is shorter than its type needs (a
    /// length of zero or one included). `offset` is where it starts in the
    /// table.
    EntryTooShort {
        /// Where the structure starts, counted from the start of the table.
        offset: usize,
    },
    /// An interrupt controller structure runs past the end of the table.
    EntryPastEnd {
        /// Where the structure starts, counted from the start of the table.
        offset: usize,
    },
}

/// Which table of tables to read, and its physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// The RSDT, with 32-bit entries.
    Rsdt(u32),
    /// The XSDT, with 64-bit entries.
    Xsdt(u64),
}

/// A checked root pointer (section 5.2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsdp {
    /// 0 for ACPI 1.0; 2 or more for later versions.
    pub revision: u8,
    /// Who made the firmware.
    pub oem_id: [u8; 6],
    /// The 32-bit physical address of the RSDT, possibly zero.
    pub rsdt_address: u32,
    /// The 64-bit physical address of the XSDT; zero below revision 2,
    /// where the field does not exist.
    pub xsdt_address: u64,
}

impl Rsdp {
    /// Checks a root pointer at the start of `bytes`. Bytes past its length
    /// are not read. A revision below 2 (only 0 is defined) is read as the
    /// 20-byte ACPI 1.0 form; 2 or more as the longer form, whose length
    /// and extended checksum must then be right as well.
    pub fn read(bytes: &[u8]) -> Result<Rsdp, Error> {
        let first = bytes
            .first_chunk::<RSDP_V1_SIZE>()
            .ok_or(Error::Truncated)?;
        // Section 5.2.5.3: signature at 0, checksum at 8 over the first 20
        // bytes, OEM ID at 9, revision at 15, RSDT address at 16.
        if first.first_chunk() != Some(b"RSD PTR ") {
            return Err(Error::BadSignature);
        }
        if !sums_to_zero(first) {
            return Err(Error::BadChecksum);
        }
        let mut rsdp = Rsdp {
            revision: get::<1>(first, 15)[0],
            oem_id: get(first, 9),
            rsdt_address: u32::from_le_bytes(get(first, 16)),
            xsdt_address: 0,
        };
        if rsdp.revision >= 2 {
            // Length at 20, XSDT address at 24, extended checksum at 32
            // over the whole length.
            let long = bytes
                .first_chunk::<RSDP_V2_SIZE>()
                .ok_or(Error::Truncated)?;
            let whole = sized(bytes, get_u32(long, 20), RSDP_V2_SIZE)?;
            if !sums_to_zero(whole) {
                return Err(Error::BadExtendedChecksum);
            }
            rsdp.xsdt_address = u64::from_le_bytes(get(long, 24));
        }
        match rsdp.root() {
            Root::Rsdt(0) => Err(Error::NoRootTable),
            _ => Ok(rsdp),
        }
    }

    /// The table of tables to read. The XSDT when there is one. Otherwise
    /// (revision 0, or revision 2 or later with a zero XSDT address, where
    /// the specification's guidance is to use the RSDT) the RSDT, whose
    /// address [`Rsdp::read`] has checked is not zero.
    pub fn root(&self) -> Root {
        match self.xsdt_address {
            0 => Root::Rsdt(self.rsdt_address),
            address => Root::Xsdt(address),
        }
    }
}

/// A table whose header and checksum have been checked (section 5.2.6).
#[derive(Debug, Clone, Copy)]
pub struct Table<'a> {
    /// Exactly the stated length, at least [`HEADER_SIZE`].
    bytes: &'a [u8],
}

impl<'a> Table<'a> {
    /// Checks the table at the start of `bytes`: its length must cover the
    /// header and fit in `bytes`, and its bytes must sum to zero modulo
    /// 256. The signature is not checked. Bytes past the length are not
    /// read.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        // Section 5.2.6: signature at 0, length at 4, revision at 8,
        // checksum at 9, OEM ID at 10, OEM table ID at 16, OEM revision at
        // 24, creator ID at 28, creator revision at 32.
        let header = bytes.first_chunk::<HEADER_SIZE>().ok_or(Error::Truncated)?;
        let bytes = sized(bytes, get_u32(header, 4), HEADER_SIZE)?;
        if !sums_to_zero(bytes) {
            return Err(Error::BadChecksum);
        }
        Ok(Table { bytes })
    }

    /// The whole table, exactly its stated length.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// What follows the header.
    pub fn body(&self) -> &'a [u8] {
        self.bytes.get(HEADER_SIZE..).unwrap_or(&[])
    }

    /// The four-character name of the table.
    pub fn signature(&self) -> [u8; 4] {
        get(self.bytes, 0)
    }

    /// The revision of the table's own layout.
    pub fn revision(&self) -> u8 {
        get::<1>(self.bytes, 8)[0]
    }

    /// Who made the firmware.
    pub fn oem_id(&self) -> [u8; 6] {
        get(self.bytes, 10)
    }

    /// The maker's name for this table.
    pub fn oem_table_id(&self) -> [u8; 8] {
        get(self.bytes, 16)
    }

    /// The maker's revision of this table.
    pub fn oem_revision(&self) -> u32 {
        get_u32(self.bytes, 24)
    }
}

/// A checked table of tables: the RSDT (section 5.2.7, signature `RSDT`,
/// 32-bit entries) or the XSDT (section 5.2.8, signature `XSDT`, 64-bit
/// entries). Its body is the physical addresses of the other tables.
#[derive(Debug, Clone, Copy)]
pub struct TableList<'a> {
    body: &'a [u8],
    extended: bool,
}

impl<'a> TableList<'a> {
    /// Checks an RSDT or XSDT at the start of `bytes`, telling them apart
    /// by signature. Its body must be a whole number of entries.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = Table::read(bytes)?;
        let (extended, size) = match &table.signature() {
            b"RSDT" => (false, 4),
            b"XSDT" => (true, 8),
            _ => return Err(Error::BadSignature),
        };
        let body = table.body();
        if body.len().checked_rem(size) != Some(0) {
            return Err(Error::PartialEntry);
        }
        Ok(TableList { body, extended })
    }

    /// True for an XSDT, false for an RSDT.
    pub fn is_extended(&self) -> bool {
        self.extended
    }

    /// The physical addresses of the other tables, in the order listed.
    pub fn addresses(&self) -> impl Iterator<Item = u64> + use<'a> {
        let (narrow, wide): (&[[u8; 4]], &[[u8; 8]]) = if self.extended {
            (&[], self.body.as_chunks().0)
        } else {
            (self.body.as_chunks().0, &[])
        };
        let narrow = narrow.iter().map(|e| u64::from(u32::from_le_bytes(*e)));
        narrow.chain(wide.iter().map(|e| u64::from_le_bytes(*e)))
    }
}

/// The flags of a processor's local interrupt controller (section
/// 5.2.12.2, "Local APIC Flags"; the x2APIC structure uses the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessorFlags {
    /// Bit 0: the processor is ready for use.
    pub enabled: bool,
    /// Bit 1: the processor is not enabled now but can be while the system
    /// runs.
    pub online_capable: bool,
}

impl ProcessorFlags {
    fn from_bits(bits: u32) -> Self {
        ProcessorFlags {
            enabled: bits & 1 != 0,
            online_capable: bits & 2 != 0,
        }
    }
}

/// The polarity of an interrupt line (section 5.2.12.5, "MPS INTI Flags",
/// bits 0 and 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// 00: as the bus specification says.
    Conforming,
    /// 01: active high.
    ActiveHigh,
    /// 10: reserved.
    Reserved,
    /// 11: active low.
    ActiveLow,
}

/// How an interrupt line is triggered (section 5.2.12.5, "MPS INTI Flags",
/// bits 2 and 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// 00: as the bus specification says.
    Conforming,
    /// 01: edge-triggered.
    Edge,
    /// 10: reserved.
    Reserved,
    /// 11: level-triggered.
    Level,
}

/// The polarity and trigger mode of an interrupt line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptFlags {
    /// Bits 0 and 1.
    pub polarity: Polarity,
    /// Bits 2 and 3.
    pub trigger: Trigger,
}

impl InterruptFlags {
    fn from_bits(bits: u16) -> Self {
        let polarity = match bits & 3 {
            0 => Polarity::Conforming,
            1 => Polarity::ActiveHigh,
            2 => Polarity::Reserved,
            _ => Polarity::ActiveLow,
        };
        let trigger = match (bits >> 2) & 3 {
            0 => Trigger::Conforming,
            1 => Trigger::Edge,
            2 => Trigger::Reserved,
            _ => Trigger::Level,
        };
        InterruptFlags { polarity, trigger }
    }
}

/// One interrupt controller structure of the MADT. A structure longer than
/// its type needs is read; the bytes past the fields known here are
/// skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry<'a> {
    /// Type 0, processor local APIC (section 5.2.12.2).
    LocalApic {
        /// The processor's ACPI processor UID.
        processor_uid: u8,
        /// Its local APIC's ID.
        apic_id: u8,
        /// Whether it can be used.
        flags: ProcessorFlags,
    },
    /// Type 1, I/O APIC (section 5.2.12.3).
    IoApic {
        /// Its ID.
        id: u8,
        /// The physical address of its registers.
        address: u32,
        /// The global system interrupt its first input stands for.
        interrupt_base: u32,
    },
    /// Type 2, interrupt source override (section 5.2.12.5): a bus
    /// interrupt that reaches a different global system interrupt.
    InterruptOverride {
        /// The bus, 0 for ISA.
        bus: u8,
        /// The interrupt line on that bus.
        source: u8,
        /// The global system interrupt it reaches.
        interrupt: u32,
        /// Its polarity and trigger mode.
        flags: InterruptFlags,
    },
    /// Type 4, local APIC NMI (section 5.2.12.7): which local interrupt
    /// input carries the non-maskable interrupt.
    LocalApicNmi {
        /// The processor's ACPI processor UID; 0xFF means all of them.
        processor_uid: u8,
        /// Its polarity and trigger mode.
        flags: InterruptFlags,
        /// The local APIC input, LINT0 or LINT1.
        lint: u8,
    },
    /// Type 5, local APIC address override (section 5.2.12.8): the 64-bit
    /// address to use in place of the table's 32-bit one.
    LocalApicAddress {
        /// The physical address of the local APICs' registers.
        address: u64,
    },
    /// Type 9, processor local x2APIC (section 5.2.12.12).
    LocalX2Apic {
        /// The processor's x2APIC ID.
        x2apic_id: u32,
        /// Whether it can be used.
        flags: ProcessorFlags,
        /// Its ACPI processor UID.
        processor_uid: u32,
    },
    /// Any other type, as given.
    Unknown {
        /// The type number.
        kind: u8,
        /// The whole structure, its type and length bytes included.
        bytes: &'a [u8],
    },
}

/// The length a structure of type `kind` needs at least: 2 (its type and
/// length) for one not known here.
fn needed(kind: u8) -> usize {
    match kind {
        0 => 8,
        1 => 12,
        2 => 10,
        4 => 6,
        5 => 12,
        9 => 16,
        _ => 2,
    }
}

/// Reads the structure at `at` in `entries`, the MADT from offset 44.
/// Returns `None` at the end, or the structure and where the next starts,
/// which is always further on.
fn entry_at(entries: &[u8], at: usize) -> Result<Option<(Entry<'_>, usize)>, Error> {
    let Some(rest) = entries.get(at..).filter(|rest| !rest.is_empty()) else {
        return Ok(None);
    };
    let offset = at.saturating_add(MADT_ENTRIES_AT);
    // Every structure starts with its type and its length, one byte each.
    let Some(&[kind, length]) = rest.first_chunk::<2>() else {
        return Err(Error::EntryPastEnd { offset });
    };
    let length = usize::from(length);
    if length < needed(kind) {
        return Err(Error::EntryTooShort { offset });
    }
    let e = rest.get(..length).ok_or(Error::EntryPastEnd { offset })?;
    let entry = match kind {
        0 => Entry::LocalApic {
            processor_uid: get::<1>(e, 2)[0],
            apic_id: get::<1>(e, 3)[0],
            flags: ProcessorFlags::from_bits(get_u32(e, 4)),
        },
        1 => Entry::IoApic {
            id: get::<1>(e, 2)[0],
            address: get_u32(e, 4),
            interrupt_base: get_u32(e, 8),
        },
        2 => Entry::InterruptOverride {
            bus: get::<1>(e, 2)[0],
            source: get::<1>(e, 3)[0],
            interrupt: get_u32(e, 4),
            flags: InterruptFlags::from_bits(u16::from_le_bytes(get(e, 8))),
        },
        4 => Entry::LocalApicNmi {
            processor_uid: get::<1>(e, 2)[0],
            flags: InterruptFlags::from_bits(u16::from_le_bytes(get(e, 3))),
            lint: get::<1>(e, 5)[0],
        },
        5 => Entry::LocalApicAddress {
            address: u64::from_le_bytes(get(e, 4)),
        },
        9 => Entry::LocalX2Apic {
            x2apic_id: get_u32(e, 4),
            flags: ProcessorFlags::from_bits(get_u32(e, 8)),
            processor_uid: get_u32(e, 12),
        },
        kind => Entry::Unknown { kind, bytes: e },
    };
    // `length` is at least 2 and `at + length` at most `entries.len()`.
    Ok(Some((entry, at.saturating_add(length))))
}

/// A checked interrupt controller table (section 5.2.12, signature
/// `APIC`).
#[derive(Debug, Clone, Copy)]
pub struct Madt<'a> {
    table: Table<'a>,
}

impl<'a> Madt<'a> {
    /// Checks the MADT at the start of `bytes`: the table, its signature,
    /// and every interrupt controller structure, so that [`Madt::entries`]
    /// has nothing left to refuse.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = Table::read(bytes)?;
        if &table.signature() != b"APIC" {
            return Err(Error::BadSignature);
        }
        let madt = Madt { table };
        let entries = madt.entries_bytes().ok_or(Error::LengthTooShort)?;
        // Each step moves on by at least two bytes, so this ends.
        let mut at = 0;
        while let Some((_, next)) = entry_at(entries, at)? {
            at = next;
        }
        Ok(madt)
    }

    fn entries_bytes(&self) -> Option<&'a [u8]> {
        self.table.bytes().get(MADT_ENTRIES_AT..)
    }

    /// The table itself.
    pub fn table(&self) -> Table<'a> {
        self.table
    }

    /// The 32-bit physical address of every processor's local APIC
    /// registers, unless a [`Entry::LocalApicAddress`] overrides it.
    pub fn local_apic_address(&self) -> u32 {
        get_u32(self.table.bytes(), 36)
    }

    /// Flag bit 0, `PCAT_COMPAT`: the machine also has the two legacy 8259
    /// interrupt controllers, which must be masked before the APICs are
    /// used.
    pub fn has_legacy_pics(&self) -> bool {
        get_u32(self.table.bytes(), 40) & 1 != 0
    }

    /// The interrupt controller structures, in table order.
    pub fn entries(&self) -> Entries<'a> {
        Entries {
            entries: self.entries_bytes().unwrap_or(&[]),
            at: 0,
        }
    }
}

/// The interrupt controller structures of a checked MADT.
#[derive(Debug, Clone)]
pub struct Entries<'a> {
    entries: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Entry<'a>> {
        // [`Madt::read`] checked every structure; should one still fail,
        // the iterator stops rather than loop.
        match entry_at(self.entries, self.at) {
            Ok(Some((entry, next))) => {
                self.at = next;
                Some(entry)
            }
            _ => {
                self.at = self.entries.len();
                None
            }
        }
    }
}

/// The first `length` bytes, if `length` is at least `minimum` and at most
/// the bytes given.
fn sized(bytes: &[u8], length: u32, minimum: usize) -> Result<&[u8], Error> {
    // A u32 always fits in a usize on the targets Bismuth builds for.
    let length = usize::try_from(length).map_err(|_| Error::LengthPastEnd)?;
    if length < minimum {
        return Err(Error::LengthTooShort);
    }
    bytes.get(..length).ok_or(Error::LengthPastEnd)
}

fn sums_to_zero(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) == 0
}

/// The `N` bytes at `at`, or zeros past the end; callers read only inside
/// a structure whose length they have checked.
fn get<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    bytes
        .get(at..)
        .and_then(|rest| rest.first_chunk::<N>())
        .copied()
        .unwrap_or([0; N])
}

fn get_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(get(bytes, at))
}

//! Reads PCI configuration data: where each function's configuration space
//! lives (the ACPI table `MCFG`), the header at its start, its address
//! registers and its capability list. Nothing else is parsed.
//!
//! The reader never touches memory or ports itself. Every function takes
//! bytes or values the caller has already read and returns views that lie
//! inside them. The input is treated as hostile: any bytes give either an
//! error or a correct view. Nothing here allocates or can panic, and the
//! lints below keep it that way.
//!
//! The layouts come from the PCI Local Bus Specification, Revision 3.0,
//! chapter 6 "Configuration Space"; the PCI Express Base Specification, for
//! the PCI Express capability and the enhanced configuration access
//! mechanism; and the PCI Firmware Specification, Revision 3.0, for `MCFG`;
//! in the sections named at each item. All fields are little-endian and are
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

use bismuth_acpi::Table;

// PCI Firmware Specification, "MCFG Table Description": after the 36-byte
// ACPI table header come 8 reserved bytes, then 16-byte allocations.
const MCFG_ENTRIES_AT: usize = 44;
const MCFG_ENTRY_SIZE: usize = 16;
// PCI Local Bus Specification, "Configuration Space Organization": a
// function's configuration space is 256 bytes, the first 64 the header.
// PCI Express extends it to 4096; the extended part is not read here.
const HEADER_SIZE: usize = 64;
const REGION_SIZE: usize = 256;
// PCI Local Bus Specification, "Capabilities List": the pointer to the
// first capability, the same in header types 0 and 1.
const CAPABILITIES_POINTER: usize = 0x34;

/// What is wrong with bytes or values the reader rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The ACPI table under the MCFG failed its own checks.
    Table(bismuth_acpi::Error),
    /// The table is not an MCFG.
    BadSignature,
    /// Fewer bytes than the structure's fixed part: an MCFG without its
    /// reserved bytes, or a header shorter than 64 bytes.
    Truncated,
    /// The MCFG's allocations are not a whole number of 16 bytes.
    PartialEntry,
    /// The bus is outside the allocation's start and end buses.
    BusOutOfRange,
    /// The device number is above 31.
    DeviceOutOfRange,
    /// The function number is above 7.
    FunctionOutOfRange,
    /// The address does not fit in 64 bits.
    AddressOverflow,
    /// The header type is neither 0 nor 1, so where its address registers
    /// and capabilities pointer are is not known.
    UnknownLayout,
    /// A 64-bit address register in the last slot, with no slot left for its
    /// high half.
    WideBarInLastSlot {
        /// The register's slot, 0 to 5.
        index: usize,
    },
    /// A memory register whose type is one of the reserved encodings, so its
    /// size cannot be worked out.
    ReservedWidth,
    /// The value read back from an address register does not describe a
    /// size that is a power of two.
    BadSize,
    /// A capability pointer below 0x40, into the header.
    CapabilityInHeader {
        /// The pointer, with its low two bits masked.
        offset: usize,
    },
    /// A capability pointer past the bytes given: there is no room for its
    /// id and next pointer.
    CapabilityPastEnd {
        /// The pointer, with its low two bits masked.
        offset: usize,
    },
    /// A capability whose fields run past the 256-byte region or the bytes
    /// given.
    CapabilityTooShort {
        /// Where the capability starts.
        offset: usize,
    },
    /// The list comes back to a capability it has already passed.
    CapabilityLoop {
        /// The capability reached a second time.
        offset: usize,
    },
}

/// A checked MCFG (PCI Firmware Specification, "MCFG Table Description",
/// signature `MCFG`).
#[derive(Debug, Clone, Copy)]
pub struct Mcfg<'a> {
    table: Table<'a>,
    /// The allocations: a whole number of [`MCFG_ENTRY_SIZE`] bytes.
    entries: &'a [u8],
}

impl<'a> Mcfg<'a> {
    /// Checks the MCFG at the start of `bytes`: the ACPI table, its
    /// signature, and that its allocations are whole.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = Table::read(bytes).map_err(Error::Table)?;
        if &table.signature() != b"MCFG" {
            return Err(Error::BadSignature);
        }
        let entries = table
            .bytes()
            .get(MCFG_ENTRIES_AT..)
            .ok_or(Error::Truncated)?;
        if entries.len().checked_rem(MCFG_ENTRY_SIZE) != Some(0) {
            return Err(Error::PartialEntry);
        }
        Ok(Mcfg { table, entries })
    }

    /// The table itself.
    pub fn table(&self) -> Table<'a> {
        self.table
    }

    /// The allocations, in table order.
    pub fn allocations(&self) -> impl Iterator<Item = Allocation> + use<'a> {
        // Base address at 0, segment group at 8, start bus at 10, end bus
        // at 11, then 4 reserved bytes.
        let entries: &[[u8; MCFG_ENTRY_SIZE]] = self.entries.as_chunks().0;
        entries.iter().map(|e| Allocation {
            base: u64::from_le_bytes(get(e, 0)),
            segment: u16::from_le_bytes(get(e, 8)),
            start_bus: byte(e, 10),
            end_bus: byte(e, 11),
        })
    }
}

/// One MCFG allocation: the configuration space of a range of buses in one
/// segment group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// The physical address of the configuration space of the start bus.
    pub base: u64,
    /// The PCI segment group.
    pub segment: u16,
    /// The first bus decoded.
    pub start_bus: u8,
    /// The last bus decoded.
    pub end_bus: u8,
}

impl Allocation {
    /// The physical address of the 4096-byte configuration space of a
    /// function (PCI Express Base Specification, "PCI Express Enhanced
    /// Configuration Access Mechanism (ECAM)"): the base plus the bus,
    /// counted from the start bus, in bits 27:20, the device in bits 19:15
    /// and the function in bits 14:12.
    pub fn address(&self, bus: u8, device: u8, function: u8) -> Result<u64, Error> {
        if bus > self.end_bus {
            return Err(Error::BusOutOfRange);
        }
        let bus = bus
            .checked_sub(self.start_bus)
            .ok_or(Error::BusOutOfRange)?;
        if device > 31 {
            return Err(Error::DeviceOutOfRange);
        }
        if function > 7 {
            return Err(Error::FunctionOutOfRange);
        }
        let offset = u64::from(bus) << 20 | u64::from(device) << 15 | u64::from(function) << 12;
        self.base.checked_add(offset).ok_or(Error::AddressOverflow)
    }
}

/// The command register at 0x04 (PCI Local Bus Specification, "Device
/// Control"), as read. The named bits are the ones a kernel sets first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command(pub u16);

impl Command {
    /// Bit 0: the function answers I/O space accesses.
    pub const IO_SPACE: u16 = 1 << 0;
    /// Bit 1: the function answers memory space accesses.
    pub const MEMORY_SPACE: u16 = 1 << 1;
    /// Bit 2: the function may start accesses itself, as for DMA and MSI.
    pub const BUS_MASTER: u16 = 1 << 2;
    /// Bit 10: the function may not raise its interrupt line (INTx).
    pub const INTERRUPT_DISABLE: u16 = 1 << 10;

    /// Whether every bit of `bits` is set.
    pub fn has(self, bits: u16) -> bool {
        self.0 & bits == bits
    }
}

/// The status register at 0x06 (PCI Local Bus Specification, "Device
/// Status"), as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status(pub u16);

impl Status {
    /// Bit 4: the capabilities pointer at 0x34 starts a list.
    pub const CAPABILITIES_LIST: u16 = 1 << 4;

    /// Whether every bit of `bits` is set.
    pub fn has(self, bits: u16) -> bool {
        self.0 & bits == bits
    }
}

/// What kind of function this is (the class code register).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Class {
    /// The base class, at 0x0B.
    pub class: u8,
    /// The subclass, at 0x0A.
    pub subclass: u8,
    /// The programming interface, at 0x09.
    pub interface: u8,
}

/// The fields of a header type 0, a general device (PCI Local Bus
/// Specification, "Configuration Space Organization").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct General {
    /// The six address registers, from 0x10, as read.
    pub bars: [u32; 6],
    /// At 0x2C.
    pub subsystem_vendor_id: u16,
    /// At 0x2E.
    pub subsystem_id: u16,
    /// At 0x34, as read; [`Header::capabilities`] masks it.
    pub capabilities_pointer: u8,
    /// At 0x3C: the interrupt line the firmware routed; on x86 0xFF means
    /// unknown or none.
    pub interrupt_line: u8,
    /// At 0x3D: 0 for none, 1 to 4 for INTA# to INTD#.
    pub interrupt_pin: u8,
}

/// The fields of a header type 1, a PCI-to-PCI bridge (PCI-to-PCI Bridge
/// Architecture Specification, Revision 1.2, which defines that header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bridge {
    /// The two address registers, at 0x10 and 0x14, as read.
    pub bars: [u32; 2],
    /// At 0x18: the bus the bridge is on.
    pub primary_bus: u8,
    /// At 0x19: the bus right behind it.
    pub secondary_bus: u8,
    /// At 0x1A: the highest bus behind it.
    pub subordinate_bus: u8,
    /// At 0x34, as read; [`Header::capabilities`] masks it.
    pub capabilities_pointer: u8,
}

/// The layout bits 6:0 of the header type register select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Type 0.
    General(General),
    /// Type 1.
    Bridge(Bridge),
    /// Any other type, as given.
    Unknown(u8),
}

/// The configuration header of a function that is present (PCI Local Bus
/// Specification, "Configuration Space Organization").
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    /// At least [`HEADER_SIZE`] bytes, at most [`REGION_SIZE`].
    bytes: &'a [u8],
}

impl<'a> Header<'a> {
    /// Reads the configuration space at the start of `bytes`: at least the
    /// 64-byte header, and the rest of the 256-byte region for the
    /// capability list when given; bytes past 256 are not read. A vendor ID
    /// of 0xFFFF, what a read of an absent function returns, gives `None`.
    pub fn read(bytes: &'a [u8]) -> Result<Option<Self>, Error> {
        let bytes = bytes.get(..REGION_SIZE).unwrap_or(bytes);
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        let header = Header { bytes };
        Ok((header.vendor_id() != 0xFFFF).then_some(header))
    }

    /// The bytes read: the header and as much of the region as was given.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// At 0x00.
    pub fn vendor_id(&self) -> u16 {
        u16::from_le_bytes(get(self.bytes, 0x00))
    }

    /// At 0x02.
    pub fn device_id(&self) -> u16 {
        u16::from_le_bytes(get(self.bytes, 0x02))
    }

    /// At 0x04.
    pub fn command(&self) -> Command {
        Command(u16::from_le_bytes(get(self.bytes, 0x04)))
    }

    /// At 0x06.
    pub fn status(&self) -> Status {
        Status(u16::from_le_bytes(get(self.bytes, 0x06)))
    }

    /// At 0x08.
    pub fn revision(&self) -> u8 {
        byte(self.bytes, 0x08)
    }

    /// At 0x09 to 0x0B.
    pub fn class(&self) -> Class {
        Class {
            interface: byte(self.bytes, 0x09),
            subclass: byte(self.bytes, 0x0A),
            class: byte(self.bytes, 0x0B),
        }
    }

    /// Bit 7 of the header type at 0x0E: the device has functions other
    /// than 0.
    pub fn multi_function(&self) -> bool {
        byte(self.bytes, 0x0E) & 0x80 != 0
    }

    /// Bits 6:0 of the header type at 0x0E, and the fields that type has.
    pub fn layout(&self) -> Layout {
        let b = self.bytes;
        match byte(b, 0x0E) & 0x7F {
            0 => Layout::General(General {
                bars: [0, 1, 2, 3, 4, 5].map(|i| bar_register(b, i)),
                subsystem_vendor_id: u16::from_le_bytes(get(b, 0x2C)),
                subsystem_id: u16::from_le_bytes(get(b, 0x2E)),
                capabilities_pointer: byte(b, CAPABILITIES_POINTER),
                interrupt_line: byte(b, 0x3C),
                interrupt_pin: byte(b, 0x3D),
            }),
            1 => Layout::Bridge(Bridge {
                bars: [0, 1].map(|i| bar_register(b, i)),
                primary_bus: byte(b, 0x18),
                secondary_bus: byte(b, 0x19),
                subordinate_bus: byte(b, 0x1A),
                capabilities_pointer: byte(b, CAPABILITIES_POINTER),
            }),
            kind => Layout::Unknown(kind),
        }
    }

    /// The address registers that read as other than zero, with their slot;
    /// a 64-bit one takes the next slot as its high half. One that reads as
    /// zero is either not implemented or not set up yet: [`bar_size`] tells
    /// which. Checks the pairing first, so iterating has nothing left to
    /// refuse.
    pub fn bars(&self) -> Result<Bars, Error> {
        let (registers, count) = match self.layout() {
            Layout::General(g) => (g.bars, 6),
            Layout::Bridge(b) => ([b.bars[0], b.bars[1], 0, 0, 0, 0], 2),
            Layout::Unknown(_) => return Err(Error::UnknownLayout),
        };
        let bars = Bars {
            registers,
            count,
            at: 0,
        };
        let mut check = bars.clone();
        while let Some(at) = check.next_slot()? {
            check.at = at;
        }
        Ok(bars)
    }

    /// The capability list (PCI Local Bus Specification, "Capabilities
    /// List"), empty unless the status register says there is one. The
    /// whole list is checked first, so iterating has nothing left to refuse.
    pub fn capabilities(&self) -> Result<Capabilities<'a>, Error> {
        let mut list = Capabilities {
            bytes: self.bytes,
            at: 0,
        };
        if !self.status().has(Status::CAPABILITIES_LIST) {
            return Ok(list);
        }
        if let Layout::Unknown(_) = self.layout() {
            return Err(Error::UnknownLayout);
        }
        // The low two bits of every pointer are reserved and masked.
        list.at = usize::from(byte(self.bytes, CAPABILITIES_POINTER) & 0xFC);
        // Each capability starts in a dword of its own from 0x40 to 0xFC,
        // so one bit per dword records which have been passed and a walk
        // takes at most 48 steps.
        let mut passed = 0u64;
        let mut at = list.at;
        while at != 0 {
            let (_, next) = capability_at(self.bytes, at)?;
            let dword = at.saturating_sub(HEADER_SIZE) / 4;
            let bit = 1u64.checked_shl(u32::try_from(dword).unwrap_or(u32::MAX));
            let bit = bit.unwrap_or(0);
            if passed & bit != 0 {
                return Err(Error::CapabilityLoop { offset: at });
            }
            passed |= bit;
            at = next;
        }
        Ok(list)
    }
}

/// The address register in slot `index` of a header.
fn bar_register(bytes: &[u8], index: usize) -> u32 {
    let at = index.saturating_mul(4).saturating_add(0x10);
    u32::from_le_bytes(get(bytes, at))
}

/// The width of a memory address register: bits 2:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// 00: anywhere in the low 4 GiB.
    Bits32,
    /// 10: anywhere in 64 bits; the next register holds the high half.
    Bits64,
    /// 01 (below 1 MiB in early revisions) or 11, as given.
    Reserved(u8),
}

/// A decoded address register (PCI Local Bus Specification, "Base
/// Addresses").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    /// Bit 0 clear: memory space.
    Memory {
        /// Bits 31:4, and for 64 bits the high half above them.
        base: u64,
        /// Bits 2:1.
        width: Width,
        /// Bit 3: reads have no side effects, so they may be merged or
        /// done ahead.
        prefetchable: bool,
    },
    /// Bit 0 set: I/O space.
    Io {
        /// Bits 31:2.
        base: u32,
    },
}

impl Bar {
    /// Decodes a register's value. `high`, the next register's value, is
    /// used only when `low` is a 64-bit memory register.
    pub fn decode(low: u32, high: u32) -> Bar {
        if low & 1 != 0 {
            return Bar::Io { base: low & !0x3 };
        }
        let width = match (low >> 1) & 3 {
            0 => Width::Bits32,
            2 => Width::Bits64,
            kind => Width::Reserved(kind as u8),
        };
        let high = if width == Width::Bits64 { high } else { 0 };
        Bar::Memory {
            base: u64::from(high) << 32 | u64::from(low & !0xF),
            width,
            prefetchable: low & 0x8 != 0,
        }
    }
}

/// The size in bytes an address register decodes, from the values read
/// back after writing all ones to it (`low`) and, for a 64-bit register, to
/// its high half (`high`, otherwise not read). The bits that stay zero are
/// the size. `None` when no address bit reads back as one: the register is
/// not implemented. An I/O register whose upper 16 bits read back as zero
/// decodes only 16 address bits.
pub fn bar_size(low: u32, high: u32) -> Result<Option<u64>, Error> {
    // The address bits that read back as one, with every bit the register
    // does not have taken as one too.
    let mask = match Bar::decode(low, high) {
        Bar::Io { base: 0 } => return Ok(None),
        Bar::Io { base } if base >> 16 == 0 => u64::from(base) | 0xFFFF_FFFF_FFFF_0000,
        Bar::Io { base } => u64::from(base) | 0xFFFF_FFFF_0000_0000,
        Bar::Memory {
            width: Width::Reserved(_),
            ..
        } => return Err(Error::ReservedWidth),
        Bar::Memory { base: 0, .. } => return Ok(None),
        Bar::Memory {
            base,
            width: Width::Bits64,
            ..
        } => base,
        Bar::Memory { base, .. } => base | 0xFFFF_FFFF_0000_0000,
    };
    // The mask is not zero, so its complement is not all ones.
    match (!mask).checked_add(1) {
        Some(size) if size.is_power_of_two() => Ok(Some(size)),
        _ => Err(Error::BadSize),
    }
}

/// The address registers of a header, from [`Header::bars`]: each with its
/// slot.
#[derive(Debug, Clone)]
pub struct Bars {
    registers: [u32; 6],
    count: usize,
    at: usize,
}

impl Bars {
    /// The first register at or after `self.at` that reads as other than
    /// zero, decoded, and the slot after it; `None` at the end.
    fn next_at(&self) -> Result<Option<(usize, Bar, usize)>, Error> {
        let mut at = self.at;
        while at < self.count {
            let low = self.registers.get(at).copied().unwrap_or(0);
            let next = at.saturating_add(1);
            if low == 0 {
                at = next;
                continue;
            }
            if low & 0x7 != 0x4 {
                return Ok(Some((at, Bar::decode(low, 0), next)));
            }
            if next >= self.count {
                return Err(Error::WideBarInLastSlot { index: at });
            }
            let high = self.registers.get(next).copied().unwrap_or(0);
            return Ok(Some((at, Bar::decode(low, high), next.saturating_add(1))));
        }
        Ok(None)
    }

    fn next_slot(&self) -> Result<Option<usize>, Error> {
        Ok(self.next_at()?.map(|(_, _, next)| next))
    }
}

impl Iterator for Bars {
    type Item = (usize, Bar);

    fn next(&mut self) -> Option<(usize, Bar)> {
        // [`Header::bars`] checked the pairing; should it still fail, the
        // iterator stops.
        match self.next_at() {
            Ok(Some((index, bar, next))) => {
                self.at = next;
                Some((index, bar))
            }
            _ => {
                self.at = self.count;
                None
            }
        }
    }
}

/// One capability of a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability<'a> {
    /// The capability ID.
    pub id: u8,
    /// Where it starts in the configuration space.
    pub offset: usize,
    /// The bytes from its start to the end of the 256-byte region, or of
    /// the bytes given if fewer: its ID and next pointer included.
    pub bytes: &'a [u8],
    /// Its fields, for the three IDs read here.
    pub decoded: Decoded,
}

/// The fields of a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// ID 0x05.
    Msi(Msi),
    /// ID 0x11.
    MsiX(MsiX),
    /// ID 0x10.
    Express(Express),
    /// Any other ID: see [`Capability::bytes`].
    Other,
}

/// Message signalled interrupts (PCI Local Bus Specification, "MSI
/// Capability Structure"). Offsets are in the configuration space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msi {
    /// The message control register at +2, as read.
    pub control: u16,
    /// Bit 0: MSI is on.
    pub enabled: bool,
    /// Bits 3:1: the function asks for 2 to this power vectors; 6 and 7 are
    /// reserved.
    pub multiple_message_capable: u8,
    /// Bits 6:4: it is given 2 to this power vectors.
    pub multiple_message_enable: u8,
    /// Bit 7: the message address may be 64 bits.
    pub is_64bit: bool,
    /// Bit 8: each vector can be masked; the mask and pending bits follow
    /// the data.
    pub per_vector_masking: bool,
    /// The message address, its low 32 bits: +4.
    pub address_offset: usize,
    /// Its high 32 bits, in the 64-bit form only: +8.
    pub upper_address_offset: Option<usize>,
    /// The 16-bit message data: +8, or +0x0C in the 64-bit form.
    pub data_offset: usize,
}

/// Where an MSI-X structure lives: one of the function's address
/// registers, and an offset into the space it decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// Bits 2:0, the register indicator: 0 to 5 for the register at 0x10
    /// to 0x24; 6 and 7 are reserved.
    pub bar: u8,
    /// Bits 31:3, a multiple of 8.
    pub offset: u32,
}

impl Location {
    fn from_bits(bits: u32) -> Self {
        Location {
            bar: (bits & 0x7) as u8,
            offset: bits & !0x7,
        }
    }
}

/// MSI-X (PCI Local Bus Specification, "MSI-X Capability and Table
/// Structure").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsiX {
    /// The message control register at +2, as read.
    pub control: u16,
    /// The number of table entries, 1 to 2048: bits 10:0 plus one.
    pub table_size: u16,
    /// Bit 15: MSI-X is on.
    pub enabled: bool,
    /// Bit 14: every vector is masked.
    pub function_mask: bool,
    /// The table, from +4.
    pub table: Location,
    /// The pending bit array, from +8.
    pub pending: Location,
}

/// A PCI Express function's capability (PCI Express Base Specification,
/// "PCI Express Capability Structure"): its capabilities register at +2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Express {
    /// Bits 3:0: the structure's version.
    pub version: u8,
    /// Bits 7:4.
    pub port_type: PortType,
}

/// The device/port type of a PCI Express function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    /// 0000: an endpoint.
    Endpoint,
    /// 0001: a legacy endpoint.
    LegacyEndpoint,
    /// 0100: a root port of a root complex.
    RootPort,
    /// 0101: the upstream port of a switch.
    UpstreamSwitchPort,
    /// 0110: a downstream port of a switch.
    DownstreamSwitchPort,
    /// 0111: a PCI Express to PCI or PCI-X bridge.
    ExpressToPciBridge,
    /// 1000: a PCI or PCI-X to PCI Express bridge.
    PciToExpressBridge,
    /// 1001: an endpoint integrated in the root complex.
    IntegratedEndpoint,
    /// 1010: a root complex event collector.
    EventCollector,
    /// Any other value, as given.
    Other(u8),
}

/// Reads the capability at `at`, a pointer with its low bits masked, and
/// returns it with the next pointer, masked; 0 ends the list.
fn capability_at(bytes: &[u8], at: usize) -> Result<(Capability<'_>, usize), Error> {
    if at < HEADER_SIZE {
        return Err(Error::CapabilityInHeader { offset: at });
    }
    let c = bytes
        .get(at..)
        .filter(|c| c.len() >= 2)
        .ok_or(Error::CapabilityPastEnd { offset: at })?;
    let control = u16::from_le_bytes(get(c, 2));
    let (decoded, needed) = match byte(c, 0) {
        0x05 => msi(at, control),
        0x11 => (
            Decoded::MsiX(MsiX {
                control,
                table_size: (control & 0x7FF).saturating_add(1),
                enabled: control & 0x8000 != 0,
                function_mask: control & 0x4000 != 0,
                table: Location::from_bits(u32::from_le_bytes(get(c, 4))),
                pending: Location::from_bits(u32::from_le_bytes(get(c, 8))),
            }),
            12,
        ),
        0x10 => (
            Decoded::Express(Express {
                version: (control & 0xF) as u8,
                port_type: port_type((control >> 4 & 0xF) as u8),
            }),
            4,
        ),
        _ => (Decoded::Other, 2),
    };
    if c.len() < needed {
        return Err(Error::CapabilityTooShort { offset: at });
    }
    let capability = Capability {
        id: byte(c, 0),
        offset: at,
        bytes: c,
        decoded,
    };
    Ok((capability, usize::from(byte(c, 1) & 0xFC)))
}

/// Decodes MSI's control word for the capability at `at`, and the bytes its
/// form takes.
fn msi(at: usize, control: u16) -> (Decoded, usize) {
    let is_64bit = control & 0x80 != 0;
    let per_vector_masking = control & 0x100 != 0;
    // Address at +4; the 32-bit form has data at +8, the 64-bit form the
    // high address at +8 and data at +0x0C; mask and pending bits take 8
    // more bytes, after reserved space that ends the data's dword.
    let (data, size) = match (is_64bit, per_vector_masking) {
        (false, false) => (0x08, 10),
        (true, false) => (0x0C, 14),
        (false, true) => (0x08, 20),
        (true, true) => (0x0C, 24),
    };
    let msi = Msi {
        control,
        enabled: control & 1 != 0,
        multiple_message_capable: (control >> 1 & 0x7) as u8,
        multiple_message_enable: (control >> 4 & 0x7) as u8,
        is_64bit,
        per_vector_masking,
        address_offset: at.saturating_add(4),
        upper_address_offset: is_64bit.then_some(at.saturating_add(8)),
        data_offset: at.saturating_add(data),
    };
    (Decoded::Msi(msi), size)
}

fn port_type(code: u8) -> PortType {
    match code {
        0x0 => PortType::Endpoint,
        0x1 => PortType::LegacyEndpoint,
        0x4 => PortType::RootPort,
        0x5 => PortType::UpstreamSwitchPort,
        0x6 => PortType::DownstreamSwitchPort,
        0x7 => PortType::ExpressToPciBridge,
        0x8 => PortType::PciToExpressBridge,
        0x9 => PortType::IntegratedEndpoint,
        0xA => PortType::EventCollector,
        other => PortType::Other(other),
    }
}

/// The capability list of a header, from [`Header::capabilities`].
#[derive(Debug, Clone)]
pub struct Capabilities<'a> {
    bytes: &'a [u8],
    /// The next capability, or 0 at the end.
    at: usize,
}

impl<'a> Iterator for Capabilities<'a> {
    type Item = Capability<'a>;

    fn next(&mut self) -> Option<Capability<'a>> {
        // [`Header::capabilities`] checked that the list ends. Offset 0, the
        // end, is refused as inside the header; should anything else fail,
        // the iterator stops too.
        let (capability, next) = capability_at(self.bytes, self.at).ok()?;
        self.at = next;
        Some(capability)
    }
}

/// The byte at `at`, or zero past the end.
fn byte(bytes: &[u8], at: usize) -> u8 {
    get::<1>(bytes, at)[0]
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

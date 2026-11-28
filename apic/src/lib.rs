//! The registers of the x86 interrupt controllers, as numbers: the local
//! APIC every processor has, in xAPIC mode, the I/O APIC that carries device
//! interrupt lines to processors, and the port writes that set up and
//! silence the two legacy 8259A controllers.
//!
//! Nothing here touches the hardware: it produces and reads the values a
//! kernel writes to the registers and reads back from them. Builders check
//! their input and return an [`Error`]; readers take any value and return
//! `None` for anything this library would not have built. Nothing allocates
//! or can panic.
//!
//! The local APIC layouts come from the Intel 64 and IA-32 Architectures
//! Software Developer's Manual (SDM), volume 3A, chapter "Advanced
//! Programmable Interrupt Controller (APIC)", in the sections named at each
//! item. The I/O APIC's come from the Intel 82093AA I/O Advanced
//! Programmable Interrupt Controller (IOAPIC) data sheet, "Register
//! Description"; later I/O APICs keep its layout. The 8259A's come from the
//! Intel 8259A Programmable Interrupt Controller data sheet.
//!
//! # Status bits
//!
//! The hardware sets two bits the kernel never writes: delivery status (bit
//! 12: the interrupt is still being sent) in local vector table entries, the
//! interrupt command register and redirection entries, and remote IRR (bit
//! 14: a level-triggered interrupt is being handled) in LINT entries and
//! redirection entries. Encoders leave them clear. Decoders ignore them, so
//! a value read back while they are set still reads as the entry written;
//! [`delivery_pending`] and [`remote_irr`] read them.
//!
//! # x2APIC
//!
//! Only xAPIC mode, with the local APIC's registers in memory, is covered.
//! In x2APIC mode (SDM vol. 3A, "Extended XAPIC (x2APIC)"), turned on by
//! bit 10 of `IA32_APIC_BASE` with bit 11, the registers are model-specific
//! registers instead, at 0x800 plus the xAPIC offset divided by 16 (0x80B
//! for end of interrupt, 0x830 for the command register), read and written
//! with `rdmsr` and `wrmsr`. Local APIC IDs are 32 bits, the whole ID
//! register, and the command register is one 64-bit register with the
//! destination in bits 63:32, written at once. The spurious-interrupt vector
//! register and the local vector table keep their layouts. A later task that
//! adds x2APIC mode starts from there.

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

#[cfg(test)]
mod tests;

use bismuth_acpi::{InterruptFlags, Polarity, Trigger};

/// Why a value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A vector below 32: 0 to 31 are the processor's exceptions.
    ExceptionVector,
    /// Vector 255, kept for spurious interrupts: see [`Vector::SPURIOUS`].
    SpuriousVector,
    /// An I/O APIC input past the last redirection entry, or one whose
    /// registers would lie past 0xFF.
    InputBeyondTable,
    /// A start-up page above 0xFF: the page must fit in 8 bits.
    StartUpPage,
    /// An 8259A vector base that is not a multiple of 8, is below 32, or
    /// leaves no room for the secondary's eight vectors.
    LegacyBase,
    /// A timer divisor other than 1, 2, 4, 8, 16, 32, 64 or 128.
    Divisor,
    /// ACPI flags with the reserved polarity or trigger value.
    ReservedFlags,
    /// A local interrupt input other than LINT0 and LINT1.
    NoSuchLint,
}

/// An interrupt vector the kernel assigns, 32 to 254.
///
/// 255 is kept, by Bismuth's convention, for the spurious interrupt: it is
/// [`Vector::SPURIOUS`], and [`Vector::new`] refuses it so no interrupt the
/// kernel assigns can share it. Two documents point there: the spurious
/// vector's bits 3:0 are fixed at 1s on P6 and Pentium processors (SDM vol.
/// 3A, "Spurious Interrupt"), and the 82093AA's redirection entries take
/// vectors up to 0xFE only. Decoders read 255 wherever it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vector(u8);

impl Vector {
    /// Vector 255, for the spurious-interrupt vector register.
    pub const SPURIOUS: Vector = Vector(0xFF);

    /// Vector `number`, if it is 32 to 254.
    pub fn new(number: u8) -> Result<Vector, Error> {
        match number {
            0..32 => Err(Error::ExceptionVector),
            0xFF => Err(Error::SpuriousVector),
            _ => Ok(Vector(number)),
        }
    }

    /// Its number.
    pub fn number(self) -> u8 {
        self.0
    }

    /// The vector in bits 7:0 of `value`, 32 to 255.
    fn read(value: u32) -> Option<Vector> {
        let number = value as u8;
        (number >= 32).then_some(Vector(number))
    }

    fn bits(self) -> u32 {
        u32::from(self.0)
    }
}

/// When an interrupt line is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPolarity {
    /// High: bit 13 clear.
    ActiveHigh,
    /// Low: bit 13 set.
    ActiveLow,
}

/// How an interrupt line signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// On a change: bit 15 clear.
    Edge,
    /// While it is held: bit 15 set.
    Level,
}

const DELIVERY_STATUS: u32 = 1 << 12;
const ACTIVE_LOW: u32 = 1 << 13;
const REMOTE_IRR: u32 = 1 << 14;
const LEVEL: u32 = 1 << 15;
const MASKED: u32 = 1 << 16;

/// Whether delivery status, bit 12, is set: the interrupt has not been
/// accepted yet. For a local vector table entry, the low half of the
/// interrupt command register or the low half of a redirection entry.
pub fn delivery_pending(value: u32) -> bool {
    value & DELIVERY_STATUS != 0
}

/// Whether remote IRR, bit 14, is set: a level-triggered interrupt has been
/// accepted and its end of interrupt not yet received. For a LINT entry or
/// the low half of a redirection entry; meaningless on an edge-triggered one.
pub fn remote_irr(value: u32) -> bool {
    value & REMOTE_IRR != 0
}

fn flag(set: bool, bit: u32) -> u32 {
    if set { bit } else { 0 }
}

fn line_bits(polarity: PinPolarity, trigger: TriggerMode) -> u32 {
    flag(polarity == PinPolarity::ActiveLow, ACTIVE_LOW)
        | flag(trigger == TriggerMode::Level, LEVEL)
}

fn read_line(value: u32) -> (PinPolarity, TriggerMode) {
    let polarity = match value & ACTIVE_LOW {
        0 => PinPolarity::ActiveHigh,
        _ => PinPolarity::ActiveLow,
    };
    let trigger = match value & LEVEL {
        0 => TriggerMode::Edge,
        _ => TriggerMode::Level,
    };
    (polarity, trigger)
}

/// ACPI flags as an ISA line has them. ACPI 6.5, section 5.2.12.5 ("MPS
/// INTI Flags"), leaves "conforms to the bus" to the bus; ISA lines are
/// active high and edge-triggered.
fn resolve(flags: InterruptFlags) -> Result<(PinPolarity, TriggerMode), Error> {
    let polarity = match flags.polarity {
        Polarity::Conforming | Polarity::ActiveHigh => PinPolarity::ActiveHigh,
        Polarity::ActiveLow => PinPolarity::ActiveLow,
        Polarity::Reserved => return Err(Error::ReservedFlags),
    };
    let trigger = match flags.trigger {
        Trigger::Conforming | Trigger::Edge => TriggerMode::Edge,
        Trigger::Level => TriggerMode::Level,
        Trigger::Reserved => return Err(Error::ReservedFlags),
    };
    Ok((polarity, trigger))
}

/// The local APIC's registers, as byte offsets from its base address (SDM
/// vol. 3A, "Local APIC Register Address Map" table). Each is 32 bits on a
/// 16-byte boundary and is read and written whole.
pub mod local {
    /// Local APIC ID, in bits 31:24.
    pub const ID: usize = 0x020;
    /// Local APIC version: [`crate::LocalVersion`].
    pub const VERSION: usize = 0x030;
    /// Task priority: interrupts at or below its class are held back; 0
    /// lets every one through.
    pub const TASK_PRIORITY: usize = 0x080;
    /// End of interrupt: writing 0 ends the interrupt being handled.
    pub const END_OF_INTERRUPT: usize = 0x0B0;
    /// Spurious-interrupt vector: [`crate::Spurious`].
    pub const SPURIOUS: usize = 0x0F0;
    /// Error status: write it, then read it, to see the errors since the
    /// last write.
    pub const ERROR_STATUS: usize = 0x280;
    /// Interrupt command, bits 31:0. Writing it sends the command, so the
    /// high half is written first.
    pub const COMMAND_LOW: usize = 0x300;
    /// Interrupt command, bits 63:32: the destination.
    pub const COMMAND_HIGH: usize = 0x310;
    /// Local vector table, timer: [`crate::Timer`].
    pub const LVT_TIMER: usize = 0x320;
    /// Local vector table, LINT0: [`crate::Lint`].
    pub const LVT_LINT0: usize = 0x350;
    /// Local vector table, LINT1: [`crate::Lint`].
    pub const LVT_LINT1: usize = 0x360;
    /// Local vector table, error: [`crate::ErrorEntry`].
    pub const LVT_ERROR: usize = 0x370;
    /// Timer initial count; writing it starts the timer.
    pub const TIMER_INITIAL_COUNT: usize = 0x380;
    /// Timer current count, read-only.
    pub const TIMER_CURRENT_COUNT: usize = 0x390;
    /// Timer divide configuration: [`crate::Divide`].
    pub const TIMER_DIVIDE: usize = 0x3E0;
}

/// The spurious-interrupt vector register (SDM vol. 3A, "Spurious
/// Interrupt"): bits 7:0 the vector, bit 8 the APIC software enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spurious {
    /// The vector a spurious interrupt arrives at.
    pub vector: Vector,
    /// Whether the local APIC is enabled.
    pub enabled: bool,
}

const SOFTWARE_ENABLE: u32 = 1 << 8;

impl Spurious {
    /// The register's value. Focus processor checking (bit 9) and
    /// EOI-broadcast suppression (bit 12) are left clear.
    pub fn encode(self) -> u32 {
        self.vector.bits() | flag(self.enabled, SOFTWARE_ENABLE)
    }

    /// The register `value` holds, or `None` if it is not one
    /// [`Spurious::encode`] gives.
    pub fn decode(value: u32) -> Option<Spurious> {
        let read = Spurious {
            vector: Vector::read(value)?,
            enabled: value & SOFTWARE_ENABLE != 0,
        };
        (read.encode() == value).then_some(read)
    }
}

/// How the local APIC timer counts (SDM vol. 3A, "APIC Timer").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerMode {
    /// Counts down once: bits 18:17 = 00.
    OneShot,
    /// Reloads the initial count at zero: bits 18:17 = 01.
    Periodic,
}

const PERIODIC: u32 = 1 << 17;

/// The timer's local vector table entry (SDM vol. 3A, "Local Vector
/// Table"): vector, delivery status, mask and timer mode. TSC-deadline mode
/// is not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    /// The vector the timer interrupt arrives at.
    pub vector: Vector,
    /// One-shot or periodic.
    pub mode: TimerMode,
    /// Whether the interrupt is held back.
    pub masked: bool,
}

impl Timer {
    /// The entry's value.
    pub fn encode(self) -> u32 {
        self.vector.bits()
            | flag(self.masked, MASKED)
            | flag(self.mode == TimerMode::Periodic, PERIODIC)
    }

    /// The entry `value` holds, delivery status ignored, or `None` if it is
    /// not one [`Timer::encode`] gives.
    pub fn decode(value: u32) -> Option<Timer> {
        let value = value & !DELIVERY_STATUS;
        let mode = match value >> 17 & 3 {
            0 => TimerMode::OneShot,
            1 => TimerMode::Periodic,
            _ => return None,
        };
        let read = Timer {
            vector: Vector::read(value)?,
            mode,
            masked: value & MASKED != 0,
        };
        (read.encode() == value).then_some(read)
    }
}

/// What a LINT input delivers (SDM vol. 3A, "Local Vector Table", delivery
/// mode in bits 10:8). SMI and INIT are not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintDelivery {
    /// 000: an interrupt at the vector.
    Fixed(Vector),
    /// 100: a non-maskable interrupt; the vector field is ignored and left 0.
    Nmi,
    /// 111: an interrupt whose vector an external 8259A supplies; the vector
    /// field is left 0.
    ExtInt,
}

/// A LINT0 or LINT1 local vector table entry: vector and delivery mode,
/// delivery status, polarity, remote IRR, trigger mode and mask. The
/// processor treats NMI as edge-triggered and ExtINT as level-triggered
/// whatever the trigger bit says, and LINT1 does not support level
/// triggering; the bit is kept as given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lint {
    /// What the input delivers.
    pub delivery: LintDelivery,
    /// When the input is active.
    pub polarity: PinPolarity,
    /// How it signals.
    pub trigger: TriggerMode,
    /// Whether the input is held back.
    pub masked: bool,
}

impl Lint {
    /// The entry for the NMI an ACPI local APIC NMI structure describes on
    /// input `lint`, with the offset of that input's register. The polarity
    /// is resolved as for an ISA line; the trigger is always edge, as the
    /// processor treats NMI, though reserved ACPI flags are still refused.
    pub fn from_acpi_nmi(lint: u8, flags: InterruptFlags) -> Result<(usize, Lint), Error> {
        let offset = match lint {
            0 => local::LVT_LINT0,
            1 => local::LVT_LINT1,
            _ => return Err(Error::NoSuchLint),
        };
        let (polarity, _) = resolve(flags)?;
        let entry = Lint {
            delivery: LintDelivery::Nmi,
            polarity,
            trigger: TriggerMode::Edge,
            masked: false,
        };
        Ok((offset, entry))
    }

    /// The entry's value.
    pub fn encode(self) -> u32 {
        let (mode, vector) = match self.delivery {
            LintDelivery::Fixed(vector) => (0b000, vector.bits()),
            LintDelivery::Nmi => (0b100, 0),
            LintDelivery::ExtInt => (0b111, 0),
        };
        vector | mode << 8 | line_bits(self.polarity, self.trigger) | flag(self.masked, MASKED)
    }

    /// The entry `value` holds, delivery status and remote IRR ignored, or
    /// `None` if it is not one [`Lint::encode`] gives.
    pub fn decode(value: u32) -> Option<Lint> {
        let value = value & !(DELIVERY_STATUS | REMOTE_IRR);
        let delivery = match value >> 8 & 7 {
            0b000 => LintDelivery::Fixed(Vector::read(value)?),
            0b100 => LintDelivery::Nmi,
            0b111 => LintDelivery::ExtInt,
            _ => return None,
        };
        let (polarity, trigger) = read_line(value);
        let read = Lint {
            delivery,
            polarity,
            trigger,
            masked: value & MASKED != 0,
        };
        (read.encode() == value).then_some(read)
    }
}

/// The error local vector table entry: vector, delivery status and mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorEntry {
    /// The vector the error interrupt arrives at.
    pub vector: Vector,
    /// Whether the interrupt is held back.
    pub masked: bool,
}

impl ErrorEntry {
    /// The entry's value.
    pub fn encode(self) -> u32 {
        self.vector.bits() | flag(self.masked, MASKED)
    }

    /// The entry `value` holds, delivery status ignored, or `None` if it is
    /// not one [`ErrorEntry::encode`] gives.
    pub fn decode(value: u32) -> Option<ErrorEntry> {
        let value = value & !DELIVERY_STATUS;
        let read = ErrorEntry {
            vector: Vector::read(value)?,
            masked: value & MASKED != 0,
        };
        (read.encode() == value).then_some(read)
    }
}

/// The timer divide configuration register (SDM vol. 3A, "APIC Timer"): the
/// bus clock is divided by 1, 2, 4, 8, 16, 32, 64 or 128 before it counts.
/// The divisor is coded in bits 3, 1 and 0, bit 2 being reserved: 000 is 2,
/// each step up doubles it to 110 for 128, and 111 is 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divide(u8);

impl Divide {
    /// Division by `divisor`, if it is one of the eight.
    pub fn new(divisor: u32) -> Result<Divide, Error> {
        if divisor.is_power_of_two() && divisor <= 128 {
            Ok(Divide(divisor.trailing_zeros() as u8))
        } else {
            Err(Error::Divisor)
        }
    }

    /// The divisor.
    pub fn divisor(self) -> u32 {
        1 << self.0
    }

    /// The register's value. The power of two `n` is coded as `n - 1`, so 1
    /// (`n` = 0) wraps round to 111.
    pub fn encode(self) -> u32 {
        let code = u32::from(self.0.wrapping_add(7) & 7);
        (code & 0b011) | (code & 0b100) << 1
    }

    /// The divisor `value` codes, or `None` if any bit but 3, 1 and 0 is set.
    pub fn decode(value: u32) -> Option<Divide> {
        if value & !0b1011 != 0 {
            return None;
        }
        let code = (value & 0b011) | (value >> 1 & 0b100);
        Some(Divide((code as u8).wrapping_add(1) & 7))
    }
}

/// A command for the interrupt command register (SDM vol. 3A, "Interrupt
/// Command Register (ICR)"), to one processor named by local APIC ID in
/// physical destination mode with no shorthand. The level bit is set, as
/// every delivery mode but INIT level de-assert requires; that one, which
/// processors since the Pentium 4 do not support, is not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Delivery mode 000: an interrupt at `vector`.
    Fixed {
        /// The vector.
        vector: Vector,
        /// The local APIC ID of the processor.
        destination: u8,
    },
    /// Delivery mode 101: INIT, which resets the processor to wait for a
    /// start-up.
    Init {
        /// The local APIC ID of the processor.
        destination: u8,
    },
    /// Delivery mode 110: start-up, which starts the processor in real mode
    /// at physical address `page` times 4096.
    StartUp {
        /// The 4 KiB page the processor starts at.
        page: u8,
        /// The local APIC ID of the processor.
        destination: u8,
    },
}

const ASSERT: u32 = 1 << 14;
const DELIVERY_MODE: u32 = 0b111 << 8;

impl Command {
    /// A start-up at page `page`, if it is below 0x100: the page goes in the
    /// 8-bit vector field, so the code starts below 1 MiB.
    pub fn start_up(page: u32, destination: u8) -> Result<Command, Error> {
        let page = u8::try_from(page).map_err(|_| Error::StartUpPage)?;
        Ok(Command::StartUp { page, destination })
    }

    /// The register's two halves, low first. The high half goes to
    /// [`local::COMMAND_HIGH`] before the low half goes to
    /// [`local::COMMAND_LOW`], which sends it.
    pub fn encode(self) -> [u32; 2] {
        let (mode, vector, destination) = match self {
            Command::Fixed {
                vector,
                destination,
            } => (0b000, vector.bits(), destination),
            Command::Init { destination } => (0b101, 0, destination),
            Command::StartUp { page, destination } => (0b110, u32::from(page), destination),
        };
        [vector | mode << 8 | ASSERT, u32::from(destination) << 24]
    }

    /// The command the two halves hold, delivery status ignored, or `None`
    /// if they are not ones [`Command::encode`] gives.
    pub fn decode(halves: [u32; 2]) -> Option<Command> {
        let [low, high] = halves;
        let low = low & !DELIVERY_STATUS;
        let destination = (high >> 24) as u8;
        let read = match (low & DELIVERY_MODE) >> 8 {
            0b000 => Command::Fixed {
                vector: Vector::read(low)?,
                destination,
            },
            0b101 => Command::Init { destination },
            0b110 => Command::StartUp {
                page: low as u8,
                destination,
            },
            _ => return None,
        };
        (read.encode() == [low, high]).then_some(read)
    }
}

/// The local APIC version register's fields (SDM vol. 3A, "Local APIC
/// Version Register"). The register is read-only, so any value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalVersion {
    /// Bits 7:0: 0x1X for an integrated local APIC.
    pub version: u8,
    /// Bits 23:16: the number of local vector table entries minus one.
    pub max_lvt_entry: u8,
}

impl LocalVersion {
    /// The fields of `value`; the other bits are not read.
    pub fn read(value: u32) -> LocalVersion {
        LocalVersion {
            version: value as u8,
            max_lvt_entry: (value >> 16) as u8,
        }
    }
}

/// The I/O APIC's registers (82093AA data sheet, "Register Description").
/// Two are in memory; the rest are reached by writing an index to
/// [`SELECT`] and then reading or writing [`WINDOW`].
pub mod io {
    use super::Error;

    /// Byte offset of the register select register, IOREGSEL.
    pub const SELECT: usize = 0x00;
    /// Byte offset of the window register, IOWIN.
    pub const WINDOW: usize = 0x10;
    /// Index of the identification register, IOAPICID: the ID in bits 27:24.
    pub const ID: u32 = 0x00;
    /// Index of the version register, IOAPICVER: [`crate::IoVersion`].
    pub const VERSION: u32 = 0x01;
    /// Index of the arbitration register, IOAPICARB.
    pub const ARBITRATION: u32 = 0x02;
    /// The index of input 0's redirection entry.
    const REDIRECTION: u32 = 0x10;
    /// The highest input whose pair of indices fits in IOREGSEL's 8 bits:
    /// 0x10 + 2 * 119 = 0xFE, and 0xFF for the high half.
    const LAST_INPUT: u32 = 119;

    /// The index of the low half of input `input`'s redirection entry,
    /// 0x10 + 2 * `input`; the high half is the next one. `max_entry` is the
    /// last input, from [`crate::IoVersion`].
    pub fn redirection_index(input: u32, max_entry: u8) -> Result<u32, Error> {
        if input > u32::from(max_entry) || input > LAST_INPUT {
            return Err(Error::InputBeyondTable);
        }
        // At most 0x10 + 2 * 119.
        Ok(REDIRECTION.wrapping_add(input.wrapping_mul(2)))
    }
}

/// The I/O APIC version register's fields (82093AA data sheet,
/// "IOAPICVER"). The register is read-only, so any value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoVersion {
    /// Bits 7:0: 0x11 for the 82093AA.
    pub version: u8,
    /// Bits 23:16: the number of inputs minus one.
    pub max_redirection_entry: u8,
}

impl IoVersion {
    /// The fields of `value`; the other bits are not read.
    pub fn read(value: u32) -> IoVersion {
        IoVersion {
            version: value as u8,
            max_redirection_entry: (value >> 16) as u8,
        }
    }
}

/// How a redirection entry picks among its destinations (82093AA data
/// sheet, "IOREDTBL", bits 10:8). SMI, NMI, INIT and ExtINT are not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoDelivery {
    /// 000: every processor the destination names.
    Fixed,
    /// 001: the one running at the lowest priority.
    LowestPriority,
}

/// How a redirection entry's destination is read (bit 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationMode {
    /// 0: a local APIC ID. The 82093AA reads bits 59:56 only; later I/O
    /// APICs, and QEMU's, all eight.
    Physical,
    /// 1: a set of processors, by their logical destination registers.
    Logical,
}

/// A 64-bit redirection entry (82093AA data sheet, "IOREDTBL"): vector
/// 7:0, delivery mode 10:8, destination mode 11, delivery status 12,
/// polarity 13, remote IRR 14, trigger mode 15, mask 16, destination 63:56.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Redirection {
    /// The vector the interrupt arrives at.
    pub vector: Vector,
    /// Which of the destinations takes it.
    pub delivery: IoDelivery,
    /// How [`Redirection::destination`] is read.
    pub destination_mode: DestinationMode,
    /// When the input is active.
    pub polarity: PinPolarity,
    /// How it signals.
    pub trigger: TriggerMode,
    /// Whether the input is held back.
    pub masked: bool,
    /// A local APIC ID or a set of processors.
    pub destination: u8,
}

impl Redirection {
    /// An entry with only the mask set, for every input until it is given
    /// an interrupt. It names vector 0, so [`Redirection::decode`] does not
    /// read it.
    pub const MASKED: [u32; 2] = [MASKED, 0];

    /// The entry for an ISA line with ACPI `flags` (from its interrupt
    /// source override, or conforming if it has none), delivered fixed to
    /// local APIC `destination` at `vector`, not masked. "Conforms to the
    /// bus" is active high and edge-triggered, as for ISA.
    pub fn from_acpi(
        vector: Vector,
        destination: u8,
        flags: InterruptFlags,
    ) -> Result<Redirection, Error> {
        let (polarity, trigger) = resolve(flags)?;
        Ok(Redirection {
            vector,
            delivery: IoDelivery::Fixed,
            destination_mode: DestinationMode::Physical,
            polarity,
            trigger,
            masked: false,
            destination,
        })
    }

    /// The two halves, low (index 0x10 + 2n) first.
    pub fn encode(self) -> [u32; 2] {
        let delivery = match self.delivery {
            IoDelivery::Fixed => 0b000,
            IoDelivery::LowestPriority => 0b001,
        };
        let logical = self.destination_mode == DestinationMode::Logical;
        let low = self.vector.bits()
            | delivery << 8
            | flag(logical, 1 << 11)
            | line_bits(self.polarity, self.trigger)
            | flag(self.masked, MASKED);
        [low, u32::from(self.destination) << 24]
    }

    /// The entry the two halves hold, delivery status and remote IRR
    /// ignored, or `None` if they are not ones [`Redirection::encode`]
    /// gives.
    pub fn decode(halves: [u32; 2]) -> Option<Redirection> {
        let [low, high] = halves;
        let low = low & !(DELIVERY_STATUS | REMOTE_IRR);
        let delivery = match low >> 8 & 7 {
            0b000 => IoDelivery::Fixed,
            0b001 => IoDelivery::LowestPriority,
            _ => return None,
        };
        let destination_mode = match low & 1 << 11 {
            0 => DestinationMode::Physical,
            _ => DestinationMode::Logical,
        };
        let (polarity, trigger) = read_line(low);
        let read = Redirection {
            vector: Vector::read(low)?,
            delivery,
            destination_mode,
            polarity,
            trigger,
            masked: low & MASKED != 0,
            destination: (high >> 24) as u8,
        };
        (read.encode() == [low, high]).then_some(read)
    }
}

/// The two legacy 8259A controllers of a PC, cascaded with the secondary on
/// the primary's line 2, at the ports every PC puts them. [`setup`] gives
/// the writes that move their vectors and mask them; a kernel that uses the
/// APICs does that and leaves them masked. Each write goes to the port in
/// the order given; old hardware wants a short wait between writes.
pub mod legacy {
    use super::Error;

    /// The primary's command port (A0 = 0).
    pub const PRIMARY_COMMAND: u16 = 0x20;
    /// The primary's data port (A0 = 1).
    pub const PRIMARY_DATA: u16 = 0x21;
    /// The secondary's command port.
    pub const SECONDARY_COMMAND: u16 = 0xA0;
    /// The secondary's data port.
    pub const SECONDARY_DATA: u16 = 0xA1;
    /// The number of writes [`setup`] gives.
    pub const STEPS: usize = 10;

    /// Which control word a write is (8259A data sheet, "Initialization
    /// Command Words" and "Operation Command Words").
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Word {
        /// Starts initialisation: edge triggered, cascaded, ICW4 to follow.
        Icw1,
        /// The vector of line 0, bits 7:3; the lines follow it.
        Icw2,
        /// The cascade: on the primary, which lines have a secondary; on the
        /// secondary, which primary line it is on.
        Icw3,
        /// 8086 mode, ordinary end of interrupt.
        Icw4,
        /// The interrupt mask, one bit per line.
        Ocw1,
    }

    /// One port write.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Step {
        /// The I/O port.
        pub port: u16,
        /// The byte written to it.
        pub value: u8,
        /// Which word it is.
        pub word: Word,
    }

    /// ICW1: bit 4 marks it, bit 0 (IC4) says ICW4 follows; SNGL (bit 1)
    /// and LTIM (bit 3) clear for a cascade of edge-triggered lines.
    const ICW1: u8 = 0x11;
    /// ICW3 on the primary: the secondary is on line 2.
    const CASCADE_LINES: u8 = 1 << 2;
    /// ICW3 on the secondary: its identity, the primary's line 2.
    const CASCADE_IDENTITY: u8 = 2;
    /// ICW4: bit 0 (uPM) for 8086 mode; no automatic end of interrupt, not
    /// buffered, not special fully nested.
    const ICW4: u8 = 0x01;
    /// OCW1: every line masked.
    const ALL_MASKED: u8 = 0xFF;

    /// The writes that initialise both controllers with the primary's
    /// vectors at `base` to `base` + 7 and the secondary's at `base` + 8 to
    /// `base` + 15, then mask every line. `base` must be a multiple of 8
    /// from 32 to 0xF0.
    pub fn setup(base: u8) -> Result<[Step; STEPS], Error> {
        if base & 7 != 0 || !(32..=0xF0).contains(&base) {
            return Err(Error::LegacyBase);
        }
        // At most 0xF0 + 8.
        let secondary = base.wrapping_add(8);
        let step = |port, value, word| Step { port, value, word };
        Ok([
            step(PRIMARY_COMMAND, ICW1, Word::Icw1),
            step(SECONDARY_COMMAND, ICW1, Word::Icw1),
            step(PRIMARY_DATA, base, Word::Icw2),
            step(SECONDARY_DATA, secondary, Word::Icw2),
            step(PRIMARY_DATA, CASCADE_LINES, Word::Icw3),
            step(SECONDARY_DATA, CASCADE_IDENTITY, Word::Icw3),
            step(PRIMARY_DATA, ICW4, Word::Icw4),
            step(SECONDARY_DATA, ICW4, Word::Icw4),
            step(PRIMARY_DATA, ALL_MASKED, Word::Ocw1),
            step(SECONDARY_DATA, ALL_MASKED, Word::Ocw1),
        ])
    }

    /// The base `steps` move the vectors to, or `None` if they are not
    /// exactly what [`setup`] gives for some base.
    pub fn base_of(steps: &[Step; STEPS]) -> Option<u8> {
        let [_, _, icw2, ..] = steps;
        let base = icw2.value;
        (setup(base).ok()? == *steps).then_some(base)
    }
}

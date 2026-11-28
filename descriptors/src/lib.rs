//! The entries of the x86-64 descriptor tables, as numbers and bytes: segment
//! selectors, the segment table (GDT) entries Bismuth uses, the task state
//! segment (TSS) and its descriptor, interrupt table (IDT) gates, the
//! pseudo-descriptor `lgdt` and `lidt` take, and the processor's exceptions.
//!
//! Nothing here touches the hardware. Builders check their input and return
//! an [`Error`]; readers take any value and return `None` for anything this
//! library would not have built. Nothing allocates or can panic.
//!
//! The layouts come from the Intel 64 and IA-32 Architectures Software
//! Developer's Manual (SDM), volume 3A: chapter "Protected-Mode Memory
//! Management", sections "Segment Selectors", "Segment Descriptors",
//! "Code- and Data-Segment Descriptor Types", "System Descriptor Types" and
//! "Segment Descriptor Tables"; chapter "Protection", sections "Code-Segment
//! Descriptor in 64-bit Mode" and "Fast System Calls in 64-Bit Mode";
//! chapter "Interrupt and Exception Handling", sections "Exception and
//! Interrupt Vectors", "Error Code", "64-Bit Mode IDT", "Interrupt Stack
//! Table" and "Exception and Interrupt Reference"; chapter "Task
//! Management", sections "TSS Descriptor in 64-bit mode" and "Task
//! Management in 64-bit Mode". Vectors 28 to 30 come from the AMD64
//! Architecture Programmer's Manual volume 2, chapter "Exceptions and
//! Interrupts".
//!
//! Addresses are canonical for 4-level paging, which Bismuth uses: bits 63:47
//! all equal. A canonical address is required where the processor faults on
//! a non-canonical one: a gate's handler (delivery raises #GP), the TSS
//! descriptor's base (`ltr` raises #GP), the stack pointers in the TSS
//! (loading one on a stack switch faults), and a table's base (every read of
//! the table through it faults; `lgdt` and `lidt` themselves do not check).

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

/// Why a value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A privilege level above 3.
    PrivilegeTooHigh,
    /// A selector index that does not fit in 13 bits: above 8191.
    SelectorIndexTooLarge,
    /// An interrupt-stack-table index of 0 or above 7. A gate that uses no
    /// interrupt stack says so with `None`.
    InterruptStackIndex,
    /// Ring 3 has no stack pointer in the TSS: the processor only switches
    /// stacks on the way to a more privileged ring.
    NoPrivilegeStack,
    /// A table with no entries.
    EmptyTable,
    /// A table with more entries than a 16-bit limit allows (8192 for the
    /// GDT) or than there are vectors (256 for the IDT).
    TableTooLarge,
    /// An address that is not canonical where one must be.
    NonCanonical,
}

fn canonical(address: u64) -> Result<u64, Error> {
    match address >> 47 {
        0 | 0x1_FFFF => Ok(address),
        _ => Err(Error::NonCanonical),
    }
}

/// A privilege level, 0 (most privileged) to 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Privilege(u8);

impl Privilege {
    /// Ring 0, the kernel's.
    pub const KERNEL: Privilege = Privilege(0);
    /// Ring 3, user programs'.
    pub const USER: Privilege = Privilege(3);

    /// The privilege level `level`, if it is at most 3.
    pub fn new(level: u8) -> Result<Privilege, Error> {
        if level <= 3 {
            Ok(Privilege(level))
        } else {
            Err(Error::PrivilegeTooHigh)
        }
    }

    /// The level, 0 to 3.
    pub fn level(self) -> u8 {
        self.0
    }

    /// The two-bit field at bit `shift` of an entry.
    fn field(self, shift: u32) -> u64 {
        u64::from(self.0) << shift
    }

    fn read(entry: u64, shift: u32) -> Privilege {
        Privilege(((entry >> shift) & 3) as u8)
    }
}

/// A segment selector into the GDT: bits 15:3 the index, bit 2 the table
/// indicator (always 0, the GDT), bits 1:0 the requested privilege level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selector {
    index: u16,
    privilege: Privilege,
}

/// The table indicator: set for the local descriptor table.
const LOCAL_TABLE: u16 = 1 << 2;

impl Selector {
    /// The selector for GDT entry `index` with requested privilege
    /// `privilege`.
    pub fn new(index: u16, privilege: Privilege) -> Result<Selector, Error> {
        if index < 1 << 13 {
            Ok(Selector { index, privilege })
        } else {
            Err(Error::SelectorIndexTooLarge)
        }
    }

    /// The index into the GDT.
    pub fn index(self) -> u16 {
        self.index
    }

    /// The requested privilege level.
    pub fn privilege(self) -> Privilege {
        self.privilege
    }

    /// The 16-bit value segment registers and gates hold.
    pub fn value(self) -> u16 {
        self.index << 3 | u16::from(self.privilege.0)
    }

    /// The selector `value` stands for, or `None` if it names the local
    /// descriptor table.
    pub fn from_value(value: u16) -> Option<Selector> {
        (value & LOCAL_TABLE == 0).then_some(Selector {
            index: value >> 3,
            privilege: Privilege(value as u8 & 3),
        })
    }
}

// SDM vol. 3A, "Segment Descriptors" and "Code- and Data-Segment Descriptor
// Types"; "Code-Segment Descriptor in 64-bit Mode" for L and D.
/// Type bit 0, accessed. Set in advance so the processor never writes it.
const ACCESSED: u64 = 1 << 40;
/// Type bit 1: readable for a code segment, writable for a data segment.
const READ_WRITE: u64 = 1 << 41;
/// Type bit 3: a code segment.
const EXECUTABLE: u64 = 1 << 43;
/// S: a code or data segment, not a system descriptor.
const CODE_OR_DATA: u64 = 1 << 44;
/// Where the descriptor privilege level starts, in segment descriptors and
/// gates alike.
const DPL: u32 = 45;
/// P: the entry is in use.
const PRESENT: u64 = 1 << 47;
/// L: 64-bit code.
const LONG_MODE: u64 = 1 << 53;
/// D/B: 32-bit default operand size, or for a stack segment a 32-bit stack
/// pointer.
const BIG: u64 = 1 << 54;
/// G: the limit counts 4 KiB units.
const GRANULARITY: u64 = 1 << 55;
/// Limit 0xFFFFF: bits 15:0 of the limit in bits 15:0, bits 19:16 in 51:48.
const FLAT_LIMIT: u64 = 0x000F_0000_0000_FFFF;

/// The entries of Bismuth's GDT, in table order.
///
/// The order follows `syscall` and `sysret` (SDM vol. 3A, "Fast System Calls
/// in 64-Bit Mode"), which take selectors from one register, `IA32_STAR`:
/// `syscall` loads CS from its bits 47:32 and SS from 8 past that, so kernel
/// data follows kernel code; `sysret` to 64-bit code loads SS from 8 past
/// its bits 63:48 and CS from 16 past, so user code follows user data. With
/// 0x08 and 0x10 there, both fit without a 32-bit user code segment. The TSS
/// descriptor follows, at [`TSS_INDEX`].
///
/// Base and limit are ignored in 64-bit mode; each entry holds the flat
/// segment `syscall` and `sysret` load into the processor's hidden copy
/// (base 0, limit 4 GiB, accessed), so a selector means the same whichever
/// way it was loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// Entry 0, which the processor never reads.
    Null = 0,
    /// 64-bit code for ring 0.
    KernelCode = 1,
    /// Data and stack for ring 0.
    KernelData = 2,
    /// Data and stack for ring 3.
    UserData = 3,
    /// 64-bit code for ring 3.
    UserCode = 4,
}

/// The index of the TSS descriptor, which takes two entries.
pub const TSS_INDEX: u16 = 5;
/// The number of 8-byte entries in Bismuth's GDT.
pub const GDT_ENTRIES: usize = 7;
/// The selector `ltr` takes.
pub const TSS_SELECTOR: Selector = Selector {
    index: TSS_INDEX,
    privilege: Privilege::KERNEL,
};

impl Segment {
    const ALL: [Segment; 5] = [
        Segment::Null,
        Segment::KernelCode,
        Segment::KernelData,
        Segment::UserData,
        Segment::UserCode,
    ];

    /// Its index in the GDT.
    pub fn index(self) -> u16 {
        self as u16
    }

    /// Its selector, with the requested privilege level of the ring it is
    /// for.
    pub fn selector(self) -> Selector {
        let privilege = match self {
            Segment::UserData | Segment::UserCode => Privilege::USER,
            _ => Privilege::KERNEL,
        };
        Selector {
            index: self.index(),
            privilege,
        }
    }

    /// Its 64-bit GDT entry.
    pub fn encode(self) -> u64 {
        let (code, privilege) = match self {
            Segment::Null => return 0,
            Segment::KernelCode => (true, Privilege::KERNEL),
            Segment::KernelData => (false, Privilege::KERNEL),
            Segment::UserData => (false, Privilege::USER),
            Segment::UserCode => (true, Privilege::USER),
        };
        let kind = if code { EXECUTABLE | LONG_MODE } else { BIG };
        FLAT_LIMIT
            | ACCESSED
            | READ_WRITE
            | CODE_OR_DATA
            | PRESENT
            | GRANULARITY
            | kind
            | privilege.field(DPL)
    }

    /// Which entry `entry` is, or `None` if it is none of them exactly.
    pub fn decode(entry: u64) -> Option<Segment> {
        Segment::ALL
            .into_iter()
            .find(|segment| segment.encode() == entry)
    }
}

/// The size of a 64-bit TSS in bytes.
pub const TSS_SIZE: usize = 104;
/// Its limit: its size minus one.
const TSS_LIMIT: u64 = 103;
/// System descriptor type 0b1001: an available 64-bit TSS (SDM vol. 3A,
/// "System Descriptor Types").
const TSS_AVAILABLE: u64 = 0x9 << 40;
/// Type bit 1 in a TSS descriptor: busy, set by `ltr`.
const TSS_BUSY: u64 = 1 << 41;

/// The 16-byte TSS descriptor of long mode, which takes two GDT entries:
/// base 63:0, limit 103, present, DPL 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TssDescriptor {
    base: u64,
    busy: bool,
}

impl TssDescriptor {
    /// The descriptor of an available TSS at the canonical address `base`.
    pub fn new(base: u64) -> Result<TssDescriptor, Error> {
        Ok(TssDescriptor {
            base: canonical(base)?,
            busy: false,
        })
    }

    /// The address of the TSS.
    pub fn base(self) -> u64 {
        self.base
    }

    /// Whether the type says busy, as `ltr` leaves it.
    pub fn is_busy(self) -> bool {
        self.busy
    }

    /// Its two GDT entries, low first: the low one holds the limit, base
    /// 31:0, type and P; the high one base 63:32.
    pub fn encode(self) -> [u64; 2] {
        let busy = if self.busy { TSS_BUSY } else { 0 };
        let low = TSS_LIMIT
            | (self.base & 0xFF_FFFF) << 16
            | TSS_AVAILABLE
            | busy
            | PRESENT
            | (self.base >> 24 & 0xFF) << 56;
        [low, self.base >> 32]
    }

    /// The descriptor `entry` holds, available or busy, or `None` if it is
    /// not exactly one [`TssDescriptor::encode`] gives.
    pub fn decode(entry: [u64; 2]) -> Option<TssDescriptor> {
        let [low, high] = entry;
        let base = (low >> 16 & 0xFF_FFFF) | (low >> 56) << 24 | high << 32;
        let descriptor = TssDescriptor {
            base: canonical(base).ok()?,
            busy: low & TSS_BUSY != 0,
        };
        (descriptor.encode() == entry).then_some(descriptor)
    }
}

/// An interrupt-stack-table slot, 1 to 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptStack(u8);

impl InterruptStack {
    /// Slot `index`, if it is 1 to 7.
    pub fn new(index: u8) -> Result<InterruptStack, Error> {
        if (1..=7).contains(&index) {
            Ok(InterruptStack(index))
        } else {
            Err(Error::InterruptStackIndex)
        }
    }

    /// The slot number, 1 to 7.
    pub fn index(self) -> u8 {
        self.0
    }
}

// Offsets in the 64-bit TSS (SDM vol. 3A, "Task Management in 64-bit
// Mode"). Everything else is reserved and left zero.
/// RSP0; RSP1 is at 12 and RSP2 at 20.
const RSP0_AT: usize = 4;
/// IST1 is at 36, IST n at `IST_BEFORE + 8 * n`.
const IST_BEFORE: usize = 28;
/// The I/O map base, 16 bits.
const IO_MAP_AT: usize = 102;

/// A 64-bit TSS as the 104 bytes the processor reads. Its 64-bit fields are
/// not 8-byte aligned, so it is bytes with accessors, not a Rust struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tss([u8; TSS_SIZE]);

impl Default for Tss {
    fn default() -> Tss {
        Tss::new()
    }
}

impl Tss {
    /// A TSS with no stacks and an I/O map base of 104, past the limit, so
    /// there is no I/O permission bitmap and ring 3 may use no port.
    pub fn new() -> Tss {
        let mut tss = Tss([0; TSS_SIZE]);
        tss.set_io_map_base(TSS_SIZE as u16);
        tss
    }

    /// The TSS these bytes hold. Any bytes are one.
    pub fn from_bytes(bytes: [u8; TSS_SIZE]) -> Tss {
        Tss(bytes)
    }

    /// Its bytes, as the processor reads them.
    pub fn as_bytes(&self) -> &[u8; TSS_SIZE] {
        &self.0
    }

    fn ring_at(ring: Privilege) -> Result<usize, Error> {
        match ring.0 {
            0 => Ok(RSP0_AT),
            1 => Ok(12),
            2 => Ok(20),
            _ => Err(Error::NoPrivilegeStack),
        }
    }

    /// The stack pointer loaded on a change to ring 0, 1 or 2.
    pub fn privilege_stack(&self, ring: Privilege) -> Result<u64, Error> {
        Ok(self.read(Tss::ring_at(ring)?))
    }

    /// Sets the stack pointer for ring 0, 1 or 2 to the canonical `top`.
    pub fn set_privilege_stack(&mut self, ring: Privilege, top: u64) -> Result<(), Error> {
        let at = Tss::ring_at(ring)?;
        self.write(at, canonical(top)?);
        Ok(())
    }

    /// The stack pointer loaded for a gate naming `stack`.
    pub fn interrupt_stack(&self, stack: InterruptStack) -> u64 {
        self.read(Tss::stack_at(stack))
    }

    /// Sets the stack pointer for `stack` to the canonical `top`.
    pub fn set_interrupt_stack(&mut self, stack: InterruptStack, top: u64) -> Result<(), Error> {
        self.write(Tss::stack_at(stack), canonical(top)?);
        Ok(())
    }

    /// The offset of the I/O permission bitmap from the TSS's base.
    pub fn io_map_base(&self) -> u16 {
        let [a, b] = self.get::<2>(IO_MAP_AT);
        u16::from_le_bytes([a, b])
    }

    /// Sets the offset of the I/O permission bitmap.
    pub fn set_io_map_base(&mut self, base: u16) {
        self.put(IO_MAP_AT, base.to_le_bytes());
    }

    /// The slot index is 1 to 7, so the sum is 36 to 84.
    fn stack_at(stack: InterruptStack) -> usize {
        usize::from(stack.0)
            .wrapping_mul(8)
            .wrapping_add(IST_BEFORE)
    }

    fn read(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.get::<8>(at))
    }

    fn write(&mut self, at: usize, value: u64) {
        self.put(at, value.to_le_bytes());
    }

    /// The `N` bytes at `at`; every caller's field lies inside the TSS.
    fn get<const N: usize>(&self, at: usize) -> [u8; N] {
        self.0
            .get(at..)
            .and_then(|rest| rest.first_chunk::<N>())
            .copied()
            .unwrap_or([0; N])
    }

    fn put<const N: usize>(&mut self, at: usize, value: [u8; N]) {
        if let Some(field) = self
            .0
            .get_mut(at..)
            .and_then(|rest| rest.first_chunk_mut::<N>())
        {
            *field = value;
        }
    }
}

/// What kind of gate an IDT entry is (SDM vol. 3A, "64-Bit Mode IDT").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// Type 0xE: interrupts are turned off on entry.
    Interrupt,
    /// Type 0xF: interrupts are left as they were.
    Trap,
}

const INTERRUPT_GATE: u64 = 0xE << 40;
const TRAP_GATE: u64 = 0xF << 40;

/// A 16-byte IDT gate of long mode, present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    handler: u64,
    selector: Selector,
    stack: Option<InterruptStack>,
    kind: GateKind,
    privilege: Privilege,
}

impl Gate {
    /// The entry for a vector with no handler: not present.
    pub const MISSING: [u64; 2] = [0, 0];

    /// A gate to the canonical address `handler` in the code segment
    /// `selector`, switching to `stack` if one is given, which software may
    /// invoke with `int` from rings up to `privilege`.
    pub fn new(
        handler: u64,
        selector: Selector,
        stack: Option<InterruptStack>,
        kind: GateKind,
        privilege: Privilege,
    ) -> Result<Gate, Error> {
        Ok(Gate {
            handler: canonical(handler)?,
            selector,
            stack,
            kind,
            privilege,
        })
    }

    /// The handler's address.
    pub fn handler(self) -> u64 {
        self.handler
    }

    /// The handler's code segment.
    pub fn selector(self) -> Selector {
        self.selector
    }

    /// The interrupt stack it switches to, if any.
    pub fn stack(self) -> Option<InterruptStack> {
        self.stack
    }

    /// Interrupt or trap gate.
    pub fn kind(self) -> GateKind {
        self.kind
    }

    /// The least privileged ring that may invoke it with `int`.
    pub fn privilege(self) -> Privilege {
        self.privilege
    }

    /// Its two 8-byte halves, low first: handler 15:0, selector, IST, type,
    /// DPL, P and handler 31:16 in the low one, handler 63:32 in the high.
    pub fn encode(self) -> [u64; 2] {
        let kind = match self.kind {
            GateKind::Interrupt => INTERRUPT_GATE,
            GateKind::Trap => TRAP_GATE,
        };
        let stack = self.stack.map_or(0, |stack| u64::from(stack.0));
        let low = (self.handler & 0xFFFF)
            | u64::from(self.selector.value()) << 16
            | stack << 32
            | kind
            | self.privilege.field(DPL)
            | PRESENT
            | (self.handler >> 16 & 0xFFFF) << 48;
        [low, self.handler >> 32]
    }

    /// The gate `entry` holds, or `None` if it is not exactly one
    /// [`Gate::encode`] gives: not present, another type, reserved bits set,
    /// a local-table selector or a non-canonical handler.
    pub fn decode(entry: [u64; 2]) -> Option<Gate> {
        let [low, high] = entry;
        // Type bit 0 tells the two apart; the comparison below refuses any
        // other type.
        let kind = if low & 1 << 40 == 0 {
            GateKind::Interrupt
        } else {
            GateKind::Trap
        };
        let stack = InterruptStack::new((low >> 32 & 7) as u8).ok();
        let gate = Gate::new(
            (low & 0xFFFF) | (low >> 48) << 16 | high << 32,
            Selector::from_value((low >> 16) as u16)?,
            stack,
            kind,
            Privilege::read(low, DPL),
        )
        .ok()?;
        (gate.encode() == entry).then_some(gate)
    }
}

/// The operand of `lgdt` and `lidt`: a table's limit (its size in bytes
/// minus one) and its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PseudoDescriptor {
    limit: u16,
    base: u64,
}

impl PseudoDescriptor {
    /// For a GDT of 1 to 8192 8-byte entries at the canonical address `base`.
    pub fn gdt(base: u64, entries: usize) -> Result<PseudoDescriptor, Error> {
        PseudoDescriptor::new(base, entries, 8)
    }

    /// For an IDT of 1 to 256 16-byte gates at the canonical address `base`:
    /// a larger limit fits in 16 bits but there are only 256 vectors.
    pub fn idt(base: u64, entries: usize) -> Result<PseudoDescriptor, Error> {
        if entries > 256 {
            return Err(Error::TableTooLarge);
        }
        PseudoDescriptor::new(base, entries, 16)
    }

    fn new(base: u64, entries: usize, size: usize) -> Result<Self, Error> {
        let limit = entries
            .checked_mul(size)
            .ok_or(Error::TableTooLarge)?
            .checked_sub(1)
            .ok_or(Error::EmptyTable)?;
        Ok(PseudoDescriptor {
            limit: u16::try_from(limit).map_err(|_| Error::TableTooLarge)?,
            base: canonical(base)?,
        })
    }

    /// The table's size in bytes minus one.
    pub fn limit(self) -> u16 {
        self.limit
    }

    /// The table's address.
    pub fn base(self) -> u64 {
        self.base
    }

    /// The 10 bytes the instructions read: the limit, then the base, both
    /// little-endian.
    pub fn to_bytes(self) -> [u8; 10] {
        let [a, b] = self.limit.to_le_bytes();
        let [c, d, e, f, g, h, i, j] = self.base.to_le_bytes();
        [a, b, c, d, e, f, g, h, i, j]
    }

    /// The pseudo-descriptor in `bytes`, any limit, or `None` if the base is
    /// not canonical.
    pub fn from_bytes(bytes: [u8; 10]) -> Option<PseudoDescriptor> {
        let [a, b, c, d, e, f, g, h, i, j] = bytes;
        Some(PseudoDescriptor {
            limit: u16::from_le_bytes([a, b]),
            base: canonical(u64::from_le_bytes([c, d, e, f, g, h, i, j])).ok()?,
        })
    }
}

/// How an exception relates to the instruction it came from (SDM vol. 3A,
/// "Exception Classifications").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Reported before the instruction, which can be run again.
    Fault,
    /// Reported after the instruction.
    Trap,
    /// A fault or a trap, depending on its cause.
    FaultOrTrap,
    /// No reliable place to return to.
    Abort,
    /// Not caused by an instruction.
    Interrupt,
    /// A reserved vector.
    Reserved,
}

/// One of the vectors 0 to 31, which the processor keeps for exceptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exception {
    /// Its vector.
    pub vector: u8,
    /// The manual's mnemonic, if it gives one.
    pub mnemonic: Option<&'static str>,
    /// The manual's name, or "Reserved".
    pub name: &'static str,
    /// Whether the processor pushes an error code after the return address.
    pub error_code: bool,
    /// Its class.
    pub class: Class,
}

const fn defined(
    vector: u8,
    mnemonic: &'static str,
    name: &'static str,
    error_code: bool,
    class: Class,
) -> Exception {
    Exception {
        vector,
        mnemonic: Some(mnemonic),
        name,
        error_code,
        class,
    }
}

const fn reserved(vector: u8) -> Exception {
    Exception {
        vector,
        mnemonic: None,
        name: "Reserved",
        error_code: false,
        class: Class::Reserved,
    }
}

/// Vectors 0 to 31, each at its own index, from the SDM vol. 3A table of
/// protected-mode exceptions and interrupts ("Exception and Interrupt
/// Vectors"). Intel reserves 28 to 30; AMD processors use them as below.
pub const EXCEPTIONS: [Exception; 32] = [
    defined(0, "#DE", "Divide Error", false, Class::Fault),
    defined(1, "#DB", "Debug Exception", false, Class::FaultOrTrap),
    Exception {
        vector: 2,
        mnemonic: None,
        name: "NMI Interrupt",
        error_code: false,
        class: Class::Interrupt,
    },
    defined(3, "#BP", "Breakpoint", false, Class::Trap),
    defined(4, "#OF", "Overflow", false, Class::Trap),
    defined(5, "#BR", "BOUND Range Exceeded", false, Class::Fault),
    defined(6, "#UD", "Invalid Opcode", false, Class::Fault),
    defined(7, "#NM", "Device Not Available", false, Class::Fault),
    defined(8, "#DF", "Double Fault", true, Class::Abort),
    // Not raised by any processor after the Intel386.
    Exception {
        vector: 9,
        mnemonic: None,
        name: "Coprocessor Segment Overrun",
        error_code: false,
        class: Class::Fault,
    },
    defined(10, "#TS", "Invalid TSS", true, Class::Fault),
    defined(11, "#NP", "Segment Not Present", true, Class::Fault),
    defined(12, "#SS", "Stack-Segment Fault", true, Class::Fault),
    defined(13, "#GP", "General Protection", true, Class::Fault),
    defined(14, "#PF", "Page Fault", true, Class::Fault),
    reserved(15),
    defined(
        16,
        "#MF",
        "x87 FPU Floating-Point Error",
        false,
        Class::Fault,
    ),
    defined(17, "#AC", "Alignment Check", true, Class::Fault),
    defined(18, "#MC", "Machine Check", false, Class::Abort),
    defined(
        19,
        "#XM",
        "SIMD Floating-Point Exception",
        false,
        Class::Fault,
    ),
    defined(20, "#VE", "Virtualization Exception", false, Class::Fault),
    defined(
        21,
        "#CP",
        "Control Protection Exception",
        true,
        Class::Fault,
    ),
    reserved(22),
    reserved(23),
    reserved(24),
    reserved(25),
    reserved(26),
    reserved(27),
    // AMD only: injected by a hypervisor.
    defined(
        28,
        "#HV",
        "Hypervisor Injection Exception",
        false,
        Class::Interrupt,
    ),
    // AMD only: raised in an encrypted guest.
    defined(29, "#VC", "VMM Communication Exception", true, Class::Fault),
    // AMD only: raised on a security-sensitive event such as INIT.
    defined(30, "#SX", "Security Exception", true, Class::Interrupt),
    reserved(31),
];

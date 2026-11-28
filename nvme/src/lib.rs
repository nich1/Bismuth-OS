//! The values an NVMe driver reads and writes, as numbers and bytes: the
//! controller's registers, the 64-byte submission and 16-byte completion
//! queue entries, the admin commands that set up I/O queues, the Read and
//! Write commands, and the identify data a controller returns.
//!
//! Nothing here touches the hardware. Constructors check their input and
//! return an [`Error`]; register readers take any value; decoders take any
//! value or bytes and return `None` for anything this library would not
//! have built; identify readers take any 4096 bytes and return values that
//! stay inside them. Nothing allocates or can panic.
//!
//! Everything comes from the NVM Express Base Specification, revision 1.4,
//! in the sections named at each item; the Read and Write commands from its
//! "NVM Command Set" chapter and the namespace data from the Identify
//! command in its "Admin Command Set" chapter. Later revisions move these
//! to the NVM Command Set Specification and allow more than the 16 block
//! formats read here. Only physical
//! region page (PRP) data pointers are built: no scatter-gather lists.

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
    /// Queue id 0 for an I/O queue: 0 is the admin queue pair.
    QueueId,
    /// A queue of 0 or 1 entries, or more than the controller takes (CAP's
    /// MQES for an I/O queue, 4096 for an admin queue).
    QueueSize,
    /// A requested number of I/O queues of 0 or more than 65,535.
    QueueCount,
    /// A transfer of 0 blocks or more than 65,536.
    BlockCount,
    /// A memory page size that is not a power of two inside CAP's range.
    PageSize,
    /// A queue entry size shift past the 4-bit field, above 15.
    EntrySize,
    /// An identify buffer shorter than 4096 bytes.
    Truncated,
    /// A block size shift below 9 (512 bytes, the least the specification
    /// allows) or above 16 (64 KiB, past any format in use).
    BlockShift,
    /// A doorbell offset that does not fit in 32 bits.
    DoorbellOverflow,
}

/// The size of an identify data buffer.
pub const IDENTIFY_SIZE: usize = 4096;

fn flag(set: bool, bit: u32) -> u32 {
    if set { bit } else { 0 }
}

/// Bits `low` to `low` + 3 of `value`.
fn nibble(value: u64, low: u32) -> u8 {
    (value >> low) as u8 & 0xF
}

/// Which queue of a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    /// A submission queue, whose doorbell takes the tail.
    Submission,
    /// A completion queue, whose doorbell takes the head.
    Completion,
}

/// The controller's registers, as byte offsets in the memory its first
/// address register (BAR0) names (Base 1.4, "Register Definition").
pub mod register {
    use super::{Error, Queue};

    /// Controller Capabilities, 64 bits: [`crate::Capabilities`].
    pub const CAP: usize = 0x00;
    /// Version: [`crate::Version`].
    pub const VS: usize = 0x08;
    /// Interrupt Mask Set.
    pub const INTMS: usize = 0x0C;
    /// Interrupt Mask Clear.
    pub const INTMC: usize = 0x10;
    /// Controller Configuration: [`crate::Configuration`].
    pub const CC: usize = 0x14;
    /// Controller Status: [`crate::ControllerStatus`].
    pub const CSTS: usize = 0x1C;
    /// Admin Queue Attributes: [`crate::AdminQueues`].
    pub const AQA: usize = 0x24;
    /// Admin Submission Queue base address, 64 bits.
    pub const ASQ: usize = 0x28;
    /// Admin Completion Queue base address, 64 bits.
    pub const ACQ: usize = 0x30;

    /// The offset of queue `queue`'s doorbell (Base 1.4, "Offset (1000h +
    /// ((2y) * (4 << CAP.DSTRD))): SQyTDBL – Submission Queue y Tail
    /// Doorbell" and the completion queue head doorbell after it):
    /// 0x1000 + (2 * `queue` + 1 for a completion queue) * (4 << `stride`),
    /// `stride` being CAP's DSTRD. Refused if it does not fit in 32 bits.
    pub fn doorbell(queue: u16, kind: Queue, stride: u8) -> Result<usize, Error> {
        let index = u32::from(queue) << 1 | u32::from(kind == Queue::Completion);
        // 4 << stride is 1 << (stride + 2), at most 257 here.
        let product = 1u32
            .checked_shl(u32::from(stride).wrapping_add(2))
            .and_then(|bytes| index.checked_mul(bytes))
            .ok_or(Error::DoorbellOverflow)?;
        // Adding 0x1000 to a product that fits cannot overflow: the index is
        // below 2 ^ 17, so with 4 << stride at most 2 ^ 14 the product is
        // below 2 ^ 31, and with more it is a multiple of 2 ^ 15, so at most
        // 2 ^ 32 - 2 ^ 15. A u32 always fits in a usize on x86-64.
        Ok(product.wrapping_add(0x1000) as usize)
    }
}

/// The fields of CAP this library uses (Base 1.4, "Offset 0h: CAP –
/// Controller Capabilities"). The register is read-only, so any value is
/// read; the other bits are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// MQES, bits 15:0: the most entries an I/O queue may have, zero-based.
    pub max_entries: u16,
    /// CQR, bit 16: queues must be physically contiguous.
    pub contiguous_required: bool,
    /// TO, bits 31:24: how long to wait for ready to change, in 500 ms.
    pub timeout: u8,
    /// DSTRD, bits 35:32: doorbells are 4 << this many bytes apart.
    pub doorbell_stride: u8,
    /// CSS bit 37: the NVM command set is supported.
    pub nvm_command_set: bool,
    /// 12 + MPSMIN (bits 51:48): the smallest memory page is 2 ^ this.
    pub min_page_shift: u8,
    /// 12 + MPSMAX (bits 55:52): the largest memory page is 2 ^ this.
    pub max_page_shift: u8,
}

impl Capabilities {
    /// The fields of `value`.
    pub fn read(value: u64) -> Capabilities {
        // At most 12 + 15.
        let page = |low| PAGE_SHIFT.wrapping_add(nibble(value, low));
        Capabilities {
            max_entries: value as u16,
            contiguous_required: value & 1 << 16 != 0,
            timeout: (value >> 24) as u8,
            doorbell_stride: nibble(value, 32),
            nvm_command_set: value & 1 << 37 != 0,
            min_page_shift: page(48),
            max_page_shift: page(52),
        }
    }
}

/// The smallest memory page, 4 KiB, as a power of two.
const PAGE_SHIFT: u8 = 12;
/// The largest, 128 MiB: 12 + 15.
const MAX_PAGE_SHIFT: u8 = 27;

/// The Version register (Base 1.4, "Offset 8h: VS – Version"), read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    /// MJR, bits 31:16.
    pub major: u16,
    /// MNR, bits 15:8.
    pub minor: u8,
    /// TER, bits 7:0.
    pub tertiary: u8,
}

impl Version {
    /// The fields of `value`.
    pub fn read(value: u32) -> Version {
        Version {
            major: (value >> 16) as u16,
            minor: (value >> 8) as u8,
            tertiary: value as u8,
        }
    }
}

/// CSTS.SHST, bits 3:2: how far a shutdown has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownStatus {
    /// 00: no shutdown asked for.
    Normal,
    /// 01: shutting down.
    Processing,
    /// 10: shut down.
    Complete,
    /// 11: reserved.
    Reserved,
}

/// The fields of CSTS this library uses (Base 1.4, "Offset 1Ch: CSTS –
/// Controller Status"). The register is read-only, so any value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerStatus {
    /// RDY, bit 0: the controller takes commands.
    pub ready: bool,
    /// CFS, bit 1: the controller has had a fatal error.
    pub fatal: bool,
    /// SHST, bits 3:2.
    pub shutdown: ShutdownStatus,
}

impl ControllerStatus {
    /// The fields of `value`.
    pub fn read(value: u32) -> ControllerStatus {
        let shutdown = match value >> 2 & 3 {
            0 => ShutdownStatus::Normal,
            1 => ShutdownStatus::Processing,
            2 => ShutdownStatus::Complete,
            _ => ShutdownStatus::Reserved,
        };
        ControllerStatus {
            ready: value & 1 != 0,
            fatal: value & 2 != 0,
            shutdown,
        }
    }
}

/// A memory page size, 2 ^ (12 + n) bytes, n held here (CC.MPS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSize(u8);

impl PageSize {
    /// Pages of `bytes`, if it is a power of two from CAP's smallest to its
    /// largest page.
    pub fn new(bytes: u64, cap: &Capabilities) -> Result<PageSize, Error> {
        let shift = bytes.trailing_zeros();
        let lowest = u32::from(cap.min_page_shift.max(PAGE_SHIFT));
        let highest = u32::from(cap.max_page_shift.min(MAX_PAGE_SHIFT));
        if !bytes.is_power_of_two() || shift < lowest || shift > highest {
            return Err(Error::PageSize);
        }
        // 0 to 15, from the bounds above.
        Ok(PageSize(shift.wrapping_sub(12) as u8))
    }

    /// The page size in bytes.
    pub fn bytes(self) -> u64 {
        0x1000 << self.0
    }
}

/// A queue entry size, 2 ^ n bytes, n from 0 to 15 (CC.IOSQES and
/// CC.IOCQES; the Identify Controller fields SQES and CQES).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntrySize(u8);

impl EntrySize {
    /// 64 bytes, the NVM command set's submission entry.
    pub const SUBMISSION: EntrySize = EntrySize(6);
    /// 16 bytes, the NVM command set's completion entry.
    pub const COMPLETION: EntrySize = EntrySize(4);

    /// Entries of 2 ^ `shift` bytes, if `shift` fits the 4-bit field.
    pub fn new(shift: u8) -> Result<EntrySize, Error> {
        if shift > 15 {
            return Err(Error::EntrySize);
        }
        Ok(EntrySize(shift))
    }

    /// The power of two.
    pub fn shift(self) -> u8 {
        self.0
    }

    /// The size in bytes.
    pub fn bytes(self) -> u32 {
        1 << self.0
    }
}

/// CC.CSS, bits 6:4: the command set the controller runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSet {
    /// 000: the NVM command set.
    Nvm,
    /// 111: admin commands only.
    AdminOnly,
}

/// CC.AMS, bits 13:11: how the controller picks among submission queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arbitration {
    /// 000: round robin, the one every controller has.
    RoundRobin,
    /// 001: weighted round robin with an urgent class.
    WeightedRoundRobin,
    /// 111: the maker's own.
    VendorSpecific,
}

/// CC.SHN, bits 15:14: a shutdown asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// 00: none.
    None,
    /// 01: an orderly one.
    Normal,
    /// 10: an abrupt one.
    Abrupt,
}

/// The Controller Configuration register (Base 1.4, "Offset 14h: CC –
/// Controller Configuration").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Configuration {
    /// EN, bit 0: turns the controller on; clearing it resets it.
    pub enable: bool,
    /// CSS, bits 6:4.
    pub command_set: CommandSet,
    /// MPS, bits 10:7.
    pub page_size: PageSize,
    /// AMS, bits 13:11.
    pub arbitration: Arbitration,
    /// SHN, bits 15:14.
    pub shutdown: Shutdown,
    /// IOSQES, bits 19:16.
    pub submission_entry: EntrySize,
    /// IOCQES, bits 23:20.
    pub completion_entry: EntrySize,
}

impl Configuration {
    /// The usual configuration: enabled, the NVM command set, pages of
    /// `page_size`, round robin, no shutdown, 64-byte and 16-byte entries.
    pub fn nvm(page_size: PageSize) -> Configuration {
        Configuration {
            enable: true,
            command_set: CommandSet::Nvm,
            page_size,
            arbitration: Arbitration::RoundRobin,
            shutdown: Shutdown::None,
            submission_entry: EntrySize::SUBMISSION,
            completion_entry: EntrySize::COMPLETION,
        }
    }

    /// The register's value.
    pub fn encode(self) -> u32 {
        let command_set = match self.command_set {
            CommandSet::Nvm => 0b000,
            CommandSet::AdminOnly => 0b111,
        };
        let arbitration = match self.arbitration {
            Arbitration::RoundRobin => 0b000,
            Arbitration::WeightedRoundRobin => 0b001,
            Arbitration::VendorSpecific => 0b111,
        };
        let shutdown = match self.shutdown {
            Shutdown::None => 0b00,
            Shutdown::Normal => 0b01,
            Shutdown::Abrupt => 0b10,
        };
        flag(self.enable, 1)
            | command_set << 4
            | u32::from(self.page_size.0) << 7
            | arbitration << 11
            | shutdown << 14
            | u32::from(self.submission_entry.0) << 16
            | u32::from(self.completion_entry.0) << 20
    }

    /// The configuration `value` holds, or `None` if it is not one
    /// [`Configuration::encode`] gives.
    pub fn decode(value: u32) -> Option<Configuration> {
        let command_set = match value >> 4 & 7 {
            0b000 => CommandSet::Nvm,
            0b111 => CommandSet::AdminOnly,
            _ => return None,
        };
        let arbitration = match value >> 11 & 7 {
            0b000 => Arbitration::RoundRobin,
            0b001 => Arbitration::WeightedRoundRobin,
            0b111 => Arbitration::VendorSpecific,
            _ => return None,
        };
        let shutdown = match value >> 14 & 3 {
            0b00 => Shutdown::None,
            0b01 => Shutdown::Normal,
            0b10 => Shutdown::Abrupt,
            _ => return None,
        };
        let value64 = u64::from(value);
        let read = Configuration {
            enable: value & 1 != 0,
            command_set,
            page_size: PageSize(nibble(value64, 7)),
            arbitration,
            shutdown,
            submission_entry: EntrySize(nibble(value64, 16)),
            completion_entry: EntrySize(nibble(value64, 20)),
        };
        (read.encode() == value).then_some(read)
    }
}

/// The Admin Queue Attributes register (Base 1.4, "Offset 24h: AQA – Admin
/// Queue Attributes"): the sizes of the admin queues, zero-based, in bits
/// 11:0 (submission) and 27:16 (completion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminQueues {
    submission: u16,
    completion: u16,
}

impl AdminQueues {
    /// Admin queues of `submission` and `completion` entries, each 2 to
    /// 4096.
    pub fn new(submission: u32, completion: u32) -> Result<AdminQueues, Error> {
        let size = |entries: u32| match entries {
            // 1 to 4095 once zero-based.
            2..=4096 => Ok(entries.wrapping_sub(1) as u16),
            _ => Err(Error::QueueSize),
        };
        Ok(AdminQueues {
            submission: size(submission)?,
            completion: size(completion)?,
        })
    }

    /// The submission queue's entries.
    pub fn submission_entries(self) -> u32 {
        u32::from(self.submission).wrapping_add(1)
    }

    /// The completion queue's entries.
    pub fn completion_entries(self) -> u32 {
        u32::from(self.completion).wrapping_add(1)
    }

    /// The register's value.
    pub fn encode(self) -> u32 {
        u32::from(self.submission) | u32::from(self.completion) << 16
    }

    /// The sizes `value` holds, or `None` if it is not one
    /// [`AdminQueues::encode`] gives.
    pub fn decode(value: u32) -> Option<AdminQueues> {
        let read = AdminQueues {
            submission: (value & 0xFFF) as u16,
            completion: (value >> 16 & 0xFFF) as u16,
        };
        let sizes = read.submission != 0 && read.completion != 0;
        (sizes && read.encode() == value).then_some(read)
    }
}

/// The id of an I/O queue, 1 to 65,535.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoQueueId(u16);

impl IoQueueId {
    /// Queue `number`, if it is not 0, the admin queues'.
    pub fn new(number: u16) -> Result<IoQueueId, Error> {
        match number {
            0 => Err(Error::QueueId),
            _ => Ok(IoQueueId(number)),
        }
    }

    /// Its number.
    pub fn number(self) -> u16 {
        self.0
    }
}

/// The size of an I/O queue, 2 to CAP's MQES + 1 entries, held zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueSize(u16);

impl QueueSize {
    /// A queue of `entries`, if it is at least 2 and no more than `cap`
    /// allows.
    pub fn new(entries: u32, cap: &Capabilities) -> Result<QueueSize, Error> {
        match entries.checked_sub(1) {
            Some(zero_based) if zero_based != 0 && zero_based <= u32::from(cap.max_entries) => {
                Ok(QueueSize(zero_based as u16))
            }
            _ => Err(Error::QueueSize),
        }
    }

    /// The number of entries.
    pub fn entries(self) -> u32 {
        u32::from(self.0).wrapping_add(1)
    }
}

/// A number of I/O queues to ask for, 1 to 65,535, held zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueCount(u16);

impl QueueCount {
    /// `count` queues, if it is 1 to 65,535: zero-based 0xFFFF is invalid.
    pub fn new(count: u32) -> Result<QueueCount, Error> {
        match count {
            1..=0xFFFF => Ok(QueueCount(count.wrapping_sub(1) as u16)),
            _ => Err(Error::QueueCount),
        }
    }

    /// The number of queues.
    pub fn count(self) -> u32 {
        u32::from(self.0).wrapping_add(1)
    }

    fn zero_based(value: u16) -> Option<QueueCount> {
        (value != 0xFFFF).then_some(QueueCount(value))
    }
}

/// The number of blocks a Read or Write moves, 1 to 65,536, held zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCount(u16);

impl BlockCount {
    /// `count` blocks, if it is 1 to 65,536.
    pub fn new(count: u32) -> Result<BlockCount, Error> {
        match count {
            1..=0x1_0000 => Ok(BlockCount(count.wrapping_sub(1) as u16)),
            _ => Err(Error::BlockCount),
        }
    }

    /// The number of blocks.
    pub fn count(self) -> u32 {
        u32::from(self.0).wrapping_add(1)
    }
}

/// Reads `N` bytes at `at`, or zeros past the end; callers read only inside
/// an entry or buffer they have the whole of.
fn take<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    bytes
        .get(at..)
        .and_then(|rest| rest.first_chunk::<N>())
        .copied()
        .unwrap_or([0; N])
}

/// Copies `value` to `at` in `out`, if it fits; callers write only inside
/// an entry, where it always does.
fn put(out: &mut [u8], at: usize, value: &[u8]) {
    if let Some(target) = out
        .get_mut(at..)
        .and_then(|rest| rest.get_mut(..value.len()))
    {
        target.copy_from_slice(value);
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(take(bytes, at))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(take(bytes, at))
}

/// A 64-byte submission queue entry (Base 1.4, "Submission Queue Entry –
/// Command Format"): opcode in byte 0, FUSE and PSDT in byte 1 (left 0:
/// not fused, PRPs), the command identifier at 2, the namespace id at 4,
/// MPTR at 16 (left 0), PRP1 at 24, PRP2 at 32, CDW10 to CDW15 at 40.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command([u8; 64]);

const AT_ID: usize = 2;
const AT_NAMESPACE: usize = 4;
const AT_PRP1: usize = 24;
const AT_PRP2: usize = 32;
const AT_CDW10: usize = 40;

impl Command {
    /// An entry with `opcode` and every other byte zero.
    pub fn new(opcode: u8) -> Command {
        let mut bytes = [0; 64];
        put(&mut bytes, 0, &[opcode]);
        Command(bytes)
    }

    /// The entry `bytes` hold.
    pub fn from_bytes(bytes: [u8; 64]) -> Command {
        Command(bytes)
    }

    /// Its bytes, as they go in the queue.
    pub fn bytes(&self) -> &[u8; 64] {
        &self.0
    }

    /// The opcode, byte 0.
    pub fn opcode(&self) -> u8 {
        take::<1>(&self.0, 0)[0]
    }

    /// The command identifier, CDW0 bits 31:16, which the completion
    /// carries back.
    pub fn command_id(&self) -> u16 {
        u16::from_le_bytes(take(&self.0, AT_ID))
    }

    /// Sets the command identifier.
    pub fn set_command_id(&mut self, id: u16) {
        put(&mut self.0, AT_ID, &id.to_le_bytes());
    }

    /// The namespace id, CDW1.
    pub fn namespace(&self) -> u32 {
        u32_at(&self.0, AT_NAMESPACE)
    }

    /// Sets the namespace id.
    pub fn set_namespace(&mut self, namespace: u32) {
        put(&mut self.0, AT_NAMESPACE, &namespace.to_le_bytes());
    }

    /// The first data pointer, PRP1.
    pub fn prp1(&self) -> u64 {
        u64_at(&self.0, AT_PRP1)
    }

    /// Sets PRP1.
    pub fn set_prp1(&mut self, address: u64) {
        put(&mut self.0, AT_PRP1, &address.to_le_bytes());
    }

    /// The second data pointer, PRP2.
    pub fn prp2(&self) -> u64 {
        u64_at(&self.0, AT_PRP2)
    }

    /// Sets PRP2.
    pub fn set_prp2(&mut self, address: u64) {
        put(&mut self.0, AT_PRP2, &address.to_le_bytes());
    }

    /// Command dwords 10 to 15.
    pub fn dwords(&self) -> [u32; 6] {
        let (dwords, _) = self.0.as_chunks::<4>();
        let mut out = [0; 6];
        for (slot, dword) in out.iter_mut().zip(dwords.iter().skip(AT_CDW10 / 4)) {
            *slot = u32::from_le_bytes(*dword);
        }
        out
    }

    /// Sets command dwords 10 to 15.
    pub fn set_dwords(&mut self, dwords: [u32; 6]) {
        let (slots, _) = self.0.as_chunks_mut::<4>();
        for (slot, dword) in slots.iter_mut().skip(AT_CDW10 / 4).zip(dwords) {
            *slot = dword.to_le_bytes();
        }
    }

    /// An entry with each field given and the rest zero.
    fn build(opcode: u8, namespace: u32, data: DataPointer, dwords: [u32; 6]) -> Command {
        let mut command = Command::new(opcode);
        command.set_namespace(namespace);
        command.set_prp1(data.prp1);
        command.set_prp2(data.prp2);
        command.set_dwords(dwords);
        command
    }

    /// Whether `built` is this entry but for the command identifier.
    fn is(&self, mut built: Command) -> bool {
        built.set_command_id(self.command_id());
        built == *self
    }
}

/// How a completion went: the status field of its last dword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    /// Status code type 0, status code 0.
    Success,
    /// Anything else.
    Failure {
        /// SCT: 0 generic, 1 command specific, 2 media and data integrity,
        /// 3 path related, 7 the maker's own.
        code_type: u8,
        /// SC, whose meaning depends on the type.
        code: u8,
    },
}

/// A 16-byte completion queue entry (Base 1.4, "Completion Queue Entry"):
/// DW0 command specific, DW1 reserved, DW2 the submission queue head (15:0)
/// and id (31:16), DW3 the command identifier (15:0), the phase tag (16)
/// and the status field (31:17: SC 24:17, SCT 27:25, CRD 29:28, M 30, DNR
/// 31). Any bytes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion([u8; 16]);

impl Completion {
    /// The entry `bytes` hold.
    pub fn from_bytes(bytes: [u8; 16]) -> Completion {
        Completion(bytes)
    }

    /// DW0, which some commands fill: see [`Completion::granted_queues`].
    pub fn specific(&self) -> u32 {
        u32_at(&self.0, 0)
    }

    /// SQHD: how far the controller has read the submission queue.
    pub fn submission_head(&self) -> u16 {
        u16::from_le_bytes(take(&self.0, 8))
    }

    /// SQID: the submission queue the command came from.
    pub fn submission_queue(&self) -> u16 {
        u16::from_le_bytes(take(&self.0, 10))
    }

    /// The command identifier of the command this completes.
    pub fn command_id(&self) -> u16 {
        u16::from_le_bytes(take(&self.0, 12))
    }

    /// Bits 31:16 of DW3: the phase tag and the status field.
    fn status_field(&self) -> u16 {
        u16::from_le_bytes(take(&self.0, 14))
    }

    /// The phase tag, which flips each time the queue wraps, so a new entry
    /// can be told from an old one.
    pub fn phase(&self) -> bool {
        self.status_field() & 1 != 0
    }

    /// The status code type and status code.
    pub fn status(&self) -> CommandStatus {
        let field = self.status_field();
        let code = (field >> 1) as u8;
        let code_type = (field >> 9 & 7) as u8;
        match (code_type, code) {
            (0, 0) => CommandStatus::Success,
            _ => CommandStatus::Failure { code_type, code },
        }
    }

    /// M: more status is in the error information log.
    pub fn more(&self) -> bool {
        self.status_field() & 1 << 14 != 0
    }

    /// DNR: the same command would fail again.
    pub fn do_not_retry(&self) -> bool {
        self.status_field() & 1 << 15 != 0
    }

    /// The numbers of I/O submission and completion queues a Set Features
    /// for Number of Queues was granted: DW0 NSQA (15:0) and NCQA (31:16),
    /// zero-based.
    pub fn granted_queues(&self) -> (u32, u32) {
        let dw0 = self.specific();
        let count = |value: u32| (value & 0xFFFF).wrapping_add(1);
        (count(dw0), count(dw0 >> 16))
    }
}

/// The two data pointers of a command: PRP1, and PRP2 for data that runs
/// into a second page (or, past two pages, the address of a PRP list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataPointer {
    /// PRP1.
    pub prp1: u64,
    /// PRP2.
    pub prp2: u64,
}

const NO_DATA: DataPointer = DataPointer { prp1: 0, prp2: 0 };

/// What an Identify command returns, by its CNS value (Base 1.4, "Identify
/// command").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identify {
    /// CNS 00h: the namespace with this id: [`IdentifyNamespace`].
    Namespace(u32),
    /// CNS 01h: the controller: [`IdentifyController`].
    Controller,
    /// CNS 02h: the active namespace ids above `after`:
    /// [`ActiveNamespaces`].
    ActiveNamespaces {
        /// 0 for the list from its start.
        after: u32,
    },
}

/// An admin command (Base 1.4, "Admin Command Set"). Admin and I/O opcodes
/// overlap (01h is Create I/O Submission Queue on the admin queue and Write
/// on an I/O queue), so the queue an entry is on says which decoder to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admin {
    /// Identify (06h), into a 4096-byte buffer.
    Identify {
        /// What to identify.
        target: Identify,
        /// The buffer.
        data: DataPointer,
    },
    /// Create I/O Completion Queue (05h).
    CreateCompletionQueue {
        /// The queue's id.
        id: IoQueueId,
        /// Its size.
        size: QueueSize,
        /// Its physical address (PRP1), or its PRP list's if not
        /// contiguous.
        base: u64,
        /// PC, CDW11 bit 0: the queue is one physically contiguous block.
        contiguous: bool,
        /// IEN and IV, CDW11 bits 1 and 31:16: the interrupt vector, or
        /// `None` for a polled queue.
        interrupt: Option<u16>,
    },
    /// Create I/O Submission Queue (01h). The queue priority field is left
    /// at 00b, which the specification calls urgent; it only matters if the
    /// controller is configured for weighted round robin arbitration.
    CreateSubmissionQueue {
        /// The queue's id.
        id: IoQueueId,
        /// Its size.
        size: QueueSize,
        /// Its physical address (PRP1), or its PRP list's if not
        /// contiguous.
        base: u64,
        /// PC, CDW11 bit 0.
        contiguous: bool,
        /// CQID, CDW11 bits 31:16: where its completions go.
        completion_queue: IoQueueId,
    },
    /// Delete I/O Submission Queue (00h).
    DeleteSubmissionQueue(IoQueueId),
    /// Delete I/O Completion Queue (04h).
    DeleteCompletionQueue(IoQueueId),
    /// Set Features (09h), Number of Queues (07h; Base 1.4, "Number of
    /// Queues"), not saved; the grant is in [`Completion::granted_queues`].
    SetNumberOfQueues {
        /// I/O submission queues asked for.
        submission: QueueCount,
        /// I/O completion queues asked for.
        completion: QueueCount,
    },
}

const DELETE_SQ: u8 = 0x00;
const CREATE_SQ: u8 = 0x01;
const DELETE_CQ: u8 = 0x04;
const CREATE_CQ: u8 = 0x05;
const IDENTIFY: u8 = 0x06;
const SET_FEATURES: u8 = 0x09;
const NUMBER_OF_QUEUES: u32 = 0x07;

/// CDW10 of a queue creation: QID 15:0, QSIZE 31:16.
fn queue_dword(id: IoQueueId, size: QueueSize) -> u32 {
    u32::from(id.0) | u32::from(size.0) << 16
}

impl Admin {
    /// The submission entry, with command identifier 0.
    pub fn encode(self) -> Command {
        let (opcode, namespace, data, dwords) = match self {
            Admin::Identify { target, data } => {
                let (cns, namespace) = match target {
                    Identify::Namespace(namespace) => (0x00, namespace),
                    Identify::Controller => (0x01, 0),
                    Identify::ActiveNamespaces { after } => (0x02, after),
                };
                (IDENTIFY, namespace, data, [cns, 0, 0, 0, 0, 0])
            }
            Admin::CreateCompletionQueue {
                id,
                size,
                base,
                contiguous,
                interrupt,
            } => {
                let vector = interrupt.map_or(0, |vector| 2 | u32::from(vector) << 16);
                let flags = flag(contiguous, 1) | vector;
                let data = DataPointer {
                    prp1: base,
                    prp2: 0,
                };
                (
                    CREATE_CQ,
                    0,
                    data,
                    [queue_dword(id, size), flags, 0, 0, 0, 0],
                )
            }
            Admin::CreateSubmissionQueue {
                id,
                size,
                base,
                contiguous,
                completion_queue,
            } => {
                let flags = flag(contiguous, 1) | u32::from(completion_queue.0) << 16;
                let data = DataPointer {
                    prp1: base,
                    prp2: 0,
                };
                (
                    CREATE_SQ,
                    0,
                    data,
                    [queue_dword(id, size), flags, 0, 0, 0, 0],
                )
            }
            Admin::DeleteSubmissionQueue(id) => {
                (DELETE_SQ, 0, NO_DATA, [u32::from(id.0), 0, 0, 0, 0, 0])
            }
            Admin::DeleteCompletionQueue(id) => {
                (DELETE_CQ, 0, NO_DATA, [u32::from(id.0), 0, 0, 0, 0, 0])
            }
            Admin::SetNumberOfQueues {
                submission,
                completion,
            } => {
                let counts = u32::from(submission.0) | u32::from(completion.0) << 16;
                (
                    SET_FEATURES,
                    0,
                    NO_DATA,
                    [NUMBER_OF_QUEUES, counts, 0, 0, 0, 0],
                )
            }
        };
        Command::build(opcode, namespace, data, dwords)
    }

    /// The command `command` holds, whatever its identifier, or `None` if
    /// it is not one [`Admin::encode`] gives.
    pub fn decode(command: &Command) -> Option<Admin> {
        let [cdw10, cdw11, ..] = command.dwords();
        let id = |value: u32| IoQueueId::new(value as u16).ok();
        let size = cdw10 >> 16;
        let size = (size != 0).then_some(QueueSize(size as u16));
        let read = match command.opcode() {
            IDENTIFY => {
                let namespace = command.namespace();
                let target = match cdw10 {
                    0x00 => Identify::Namespace(namespace),
                    0x01 => Identify::Controller,
                    0x02 => Identify::ActiveNamespaces { after: namespace },
                    _ => return None,
                };
                let data = DataPointer {
                    prp1: command.prp1(),
                    prp2: command.prp2(),
                };
                Admin::Identify { target, data }
            }
            CREATE_CQ => Admin::CreateCompletionQueue {
                id: id(cdw10)?,
                size: size?,
                base: command.prp1(),
                contiguous: cdw11 & 1 != 0,
                interrupt: (cdw11 & 2 != 0).then_some((cdw11 >> 16) as u16),
            },
            CREATE_SQ => Admin::CreateSubmissionQueue {
                id: id(cdw10)?,
                size: size?,
                base: command.prp1(),
                contiguous: cdw11 & 1 != 0,
                completion_queue: id(cdw11 >> 16)?,
            },
            DELETE_SQ => Admin::DeleteSubmissionQueue(id(cdw10)?),
            DELETE_CQ => Admin::DeleteCompletionQueue(id(cdw10)?),
            // Another feature differs from the encoding, checked below.
            SET_FEATURES => Admin::SetNumberOfQueues {
                submission: QueueCount::zero_based(cdw11 as u16)?,
                completion: QueueCount::zero_based((cdw11 >> 16) as u16)?,
            },
            _ => return None,
        };
        command.is(read.encode()).then_some(read)
    }
}

/// What a Read or Write moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    /// The namespace id.
    pub namespace: u32,
    /// The first logical block, SLBA (CDW11:CDW10).
    pub start: u64,
    /// How many blocks, NLB (CDW12 bits 15:0).
    pub blocks: BlockCount,
    /// Where the data is.
    pub data: DataPointer,
}

/// An I/O command of the NVM command set (Base 1.4, "Read command" and
/// "Write command"), with no protection information, no limited retry, no
/// forced unit access and no dataset hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// Read (02h): blocks into memory.
    Read(Transfer),
    /// Write (01h): memory into blocks.
    Write(Transfer),
}

const WRITE: u8 = 0x01;
const READ: u8 = 0x02;

impl Io {
    /// The submission entry, with command identifier 0.
    pub fn encode(self) -> Command {
        let (opcode, t) = match self {
            Io::Read(transfer) => (READ, transfer),
            Io::Write(transfer) => (WRITE, transfer),
        };
        let dwords = [
            t.start as u32,
            (t.start >> 32) as u32,
            u32::from(t.blocks.0),
            0,
            0,
            0,
        ];
        Command::build(opcode, t.namespace, t.data, dwords)
    }

    /// The command `command` holds, whatever its identifier, or `None` if
    /// it is not one [`Io::encode`] gives.
    pub fn decode(command: &Command) -> Option<Io> {
        let [cdw10, cdw11, cdw12, ..] = command.dwords();
        let transfer = Transfer {
            namespace: command.namespace(),
            start: u64::from(cdw10) | u64::from(cdw11) << 32,
            blocks: BlockCount(cdw12 as u16),
            data: DataPointer {
                prp1: command.prp1(),
                prp2: command.prp2(),
            },
        };
        let read = match command.opcode() {
            READ => Io::Read(transfer),
            WRITE => Io::Write(transfer),
            _ => return None,
        };
        command.is(read.encode()).then_some(read)
    }
}

/// The first 4096 bytes of `bytes`, which an identify reader needs.
fn identify(bytes: &[u8]) -> Result<&[u8; IDENTIFY_SIZE], Error> {
    bytes.first_chunk().ok_or(Error::Truncated)
}

/// `text` without the spaces at its end, as identify strings are padded.
pub fn trim_spaces(text: &[u8]) -> &[u8] {
    let mut text = text;
    while let [rest @ .., b' '] = text {
        text = rest;
    }
    text
}

/// The Identify Controller data (Base 1.4, "Identify Controller Data
/// Structure"), in a buffer of at least 4096 bytes.
#[derive(Debug, Clone, Copy)]
pub struct IdentifyController<'a>(&'a [u8; IDENTIFY_SIZE]);

impl<'a> IdentifyController<'a> {
    /// The data at the start of `bytes`.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        identify(bytes).map(IdentifyController)
    }

    /// VID, bytes 1:0: the PCI vendor id.
    pub fn vendor_id(&self) -> u16 {
        u16::from_le_bytes(take(self.0, 0))
    }

    /// SN, bytes 23:4, ASCII padded with spaces.
    pub fn serial_number(&self) -> &'a [u8] {
        self.0.get(4..24).unwrap_or(&[])
    }

    /// MN, bytes 63:24, ASCII padded with spaces.
    pub fn model_number(&self) -> &'a [u8] {
        self.0.get(24..64).unwrap_or(&[])
    }

    /// FR, bytes 71:64, ASCII padded with spaces.
    pub fn firmware_revision(&self) -> &'a [u8] {
        self.0.get(64..72).unwrap_or(&[])
    }

    /// MDTS, byte 77: the largest transfer is 2 ^ this many pages of CAP's
    /// smallest page size; `None` for no limit.
    pub fn max_transfer_shift(&self) -> Option<u8> {
        let [shift] = take(self.0, 77);
        (shift != 0).then_some(shift)
    }

    /// SQES bits 3:0, byte 512: the submission entry size required.
    pub fn submission_entry_size(&self) -> EntrySize {
        EntrySize(take::<1>(self.0, 512)[0] & 0xF)
    }

    /// CQES bits 3:0, byte 513: the completion entry size required.
    pub fn completion_entry_size(&self) -> EntrySize {
        EntrySize(take::<1>(self.0, 513)[0] & 0xF)
    }

    /// NN, bytes 519:516: the highest namespace id the controller has.
    pub fn namespaces(&self) -> u32 {
        u32_at(self.0, 516)
    }
}

/// The block size and metadata of one LBA format (Base 1.4, "LBA Format
/// Data Structure, NVM Command Set Specific").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockFormat {
    shift: u8,
    metadata: u16,
}

impl BlockFormat {
    /// LBADS: blocks are 2 ^ this many bytes, 9 to 16.
    pub fn shift(self) -> u8 {
        self.shift
    }

    /// The block size in bytes.
    pub fn block_size(self) -> u32 {
        1 << self.shift
    }

    /// MS: the bytes of metadata with each block.
    pub fn metadata(self) -> u16 {
        self.metadata
    }
}

/// The Identify Namespace data of the NVM command set (Base 1.4, "Identify
/// Namespace Data Structure, NVM Command Set Specific"), in a buffer of at
/// least 4096 bytes.
#[derive(Debug, Clone, Copy)]
pub struct IdentifyNamespace<'a>(&'a [u8; IDENTIFY_SIZE]);

impl<'a> IdentifyNamespace<'a> {
    /// The data at the start of `bytes`.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        identify(bytes).map(IdentifyNamespace)
    }

    /// NSZE, bytes 7:0: the namespace's size in blocks.
    pub fn size(&self) -> u64 {
        u64_at(self.0, 0)
    }

    /// NCAP, bytes 15:8: how many blocks may be allocated at once.
    pub fn capacity(&self) -> u64 {
        u64_at(self.0, 8)
    }

    /// FLBAS bits 3:0, byte 26: which of the 16 LBA formats is in use.
    pub fn format_index(&self) -> u8 {
        take::<1>(self.0, 26)[0] & 0xF
    }

    /// The LBA format in use, from the table at byte 128, four bytes each:
    /// MS 15:0, LBADS 23:16. Refused if LBADS is outside 9 to 16.
    pub fn block_format(&self) -> Result<BlockFormat, Error> {
        // 128 + 4 * an index of at most 15.
        let at = 128 | usize::from(self.format_index()) << 2;
        let [low, high, shift, _] = take::<4>(self.0, at);
        if !(9..=16).contains(&shift) {
            return Err(Error::BlockShift);
        }
        Ok(BlockFormat {
            shift,
            metadata: u16::from_le_bytes([low, high]),
        })
    }
}

/// The active namespace id list (Base 1.4, "Identify command", CNS 02h): up
/// to 1024 ids in increasing order, the unused ones 0. Iterates over the
/// ids up to the first 0.
#[derive(Debug, Clone)]
pub struct ActiveNamespaces<'a>(core::slice::Iter<'a, [u8; 4]>);

impl<'a> ActiveNamespaces<'a> {
    /// The list at the start of `bytes`.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        let (ids, _) = identify(bytes)?.as_chunks::<4>();
        Ok(ActiveNamespaces(ids.iter()))
    }
}

impl Iterator for ActiveNamespaces<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let id = u32::from_le_bytes(*self.0.next()?);
        if id == 0 {
            self.0 = [].iter();
            return None;
        }
        Some(id)
    }
}

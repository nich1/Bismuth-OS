//! Tests for the NVMe structures. Every expected value is a literal worked
//! out by hand from the NVM Express Base Specification, revision 1.4, with
//! the working in a comment beside it, so a test failing means the library
//! and the specification disagree, not that two copies of the same code do.
//! Bit `n` of a register or dword is written as `1 << n` in the working,
//! and "CDWn" is command dword `n`, at byte 4n of a submission entry.

// Tests index and add freely; the crate's lints are for the library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

/// CAP with every page size from 4 KiB (MPSMIN 0) to 128 MiB (MPSMAX 15,
/// bits 55:52) and 65,536 entries per queue (MQES 0xFFFF).
fn wide() -> Capabilities {
    Capabilities::read(0x00F0_0000_0000_FFFF)
}

fn page(bytes: u64) -> PageSize {
    PageSize::new(bytes, &wide()).unwrap()
}

fn id(number: u16) -> IoQueueId {
    IoQueueId::new(number).unwrap()
}

fn size(entries: u32) -> QueueSize {
    QueueSize::new(entries, &wide()).unwrap()
}

fn count(queues: u32) -> QueueCount {
    QueueCount::new(queues).unwrap()
}

fn blocks(number: u32) -> BlockCount {
    BlockCount::new(number).unwrap()
}

fn entry(shift: u8) -> EntrySize {
    EntrySize::new(shift).unwrap()
}

fn data(prp1: u64, prp2: u64) -> DataPointer {
    DataPointer { prp1, prp2 }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

// --- Controller registers ---

#[test]
fn register_offsets_match_the_specification() {
    // Base 1.4, "Register Definition": CAP 00h (8 bytes), VS 08h, INTMS 0Ch,
    // INTMC 10h, CC 14h, 18h reserved, CSTS 1Ch, NSSR 20h, AQA 24h, ASQ 28h
    // (8 bytes), ACQ 30h (8 bytes).
    assert_eq!(register::CAP, 0x00);
    assert_eq!(register::VS, 0x08);
    assert_eq!(register::INTMS, 0x0C);
    assert_eq!(register::INTMC, 0x10);
    assert_eq!(register::CC, 0x14);
    assert_eq!(register::CSTS, 0x1C);
    assert_eq!(register::AQA, 0x24);
    assert_eq!(register::ASQ, 0x28);
    assert_eq!(register::ACQ, 0x30);
}

#[test]
fn doorbells_match_hand_worked_offsets() {
    use Queue::*;
    // Base 1.4: submission queue y's tail doorbell is at 1000h + (2y *
    // (4 << DSTRD)), completion queue y's head doorbell at 1000h + ((2y + 1)
    // * (4 << DSTRD)).
    // Stride 0 (4 bytes): admin SQ 0x1000, admin CQ 0x1000 + 4 = 0x1004,
    // queue 1's SQ 0x1000 + 2 * 4 = 0x1008 and CQ 0x1000 + 3 * 4 = 0x100C.
    assert_eq!(register::doorbell(0, Submission, 0), Ok(0x1000));
    assert_eq!(register::doorbell(0, Completion, 0), Ok(0x1004));
    assert_eq!(register::doorbell(1, Submission, 0), Ok(0x1008));
    assert_eq!(register::doorbell(1, Completion, 0), Ok(0x100C));
    // Stride 1 (8 bytes), queue 3's CQ: 0x1000 + 7 * 8 = 0x1038.
    assert_eq!(register::doorbell(3, Completion, 1), Ok(0x1038));
    // The last queue id at stride 0: 0x1000 + (2 * 65535 + 1) * 4 =
    // 0x1000 + 0x7FFFC = 0x80FFC.
    assert_eq!(register::doorbell(0xFFFF, Completion, 0), Ok(0x8_0FFC));
    // Stride 15, the largest CAP can state (4 << 15 = 0x2_0000 bytes):
    // queue 16383's CQ is entry 32767, 0x1000 + 32767 * 0x2_0000 =
    // 0x1000 + 0xFFFE_0000 = 0xFFFE_1000, the last that fits 32 bits.
    assert_eq!(register::doorbell(16383, Completion, 15), Ok(0xFFFE_1000));
    // Stride 29 (1 << 31 bytes): queue 0's CQ at 0x1000 + 0x8000_0000.
    assert_eq!(register::doorbell(0, Completion, 29), Ok(0x8000_1000));
}

#[test]
fn refuses_doorbell_arithmetic_that_overflows() {
    use Queue::*;
    for (queue, kind, stride) in [
        // Entry 32768 at stride 15: 0x1000 + 0x1_0000_0000.
        (16384, Submission, 15),
        (0xFFFF, Completion, 15),
        // Entry 2 at stride 29: 2 * (1 << 31) = 1 << 32.
        (1, Submission, 29),
        // Strides whose 4 << stride does not fit 32 bits at all.
        (0, Submission, 30),
        (0, Completion, 31),
        (0, Submission, 255),
    ] {
        assert_eq!(
            register::doorbell(queue, kind, stride),
            Err(Error::DoorbellOverflow),
            "{queue} {kind:?} {stride}"
        );
    }
}

#[test]
fn capabilities_match_hand_worked_values() {
    // Base 1.4, "Offset 0h: CAP – Controller Capabilities": MQES 15:0, CQR
    // 16, TO 31:24, DSTRD 35:32, CSS 44:37 (bit 37 the NVM command set),
    // MPSMIN 51:48, MPSMAX 55:52, page sizes 2 ^ (12 + n).
    // Every field at its top: 0xFFFF | 1 << 16 | 0xFF << 24 = 0xFF01_FFFF;
    // high dword 0xF (DSTRD) | 1 << 5 (bit 37) | 0xF << 16 | 0xF << 20 =
    // 0x00FF_002F.
    let top = Capabilities::read(0x00FF_002F_FF01_FFFF);
    assert_eq!(
        top,
        Capabilities {
            max_entries: 0xFFFF,
            contiguous_required: true,
            timeout: 0xFF,
            doorbell_stride: 15,
            nvm_command_set: true,
            min_page_shift: 27,
            max_page_shift: 27,
        }
    );
    // Zero: one entry (a reserved value, still read), 4 KiB pages only.
    let zero = Capabilities::read(0);
    assert_eq!(
        zero,
        Capabilities {
            max_entries: 0,
            contiguous_required: false,
            timeout: 0,
            doorbell_stride: 0,
            nvm_command_set: false,
            min_page_shift: 12,
            max_page_shift: 12,
        }
    );
    // The bits no field takes (AMS 18:17, NSSRS 36, the other command set
    // bits 44:38, BPS 45, and 63:56) are ignored: 0x0006_0000 | 1 << 36 |
    // 0xFF << 38 | 1 << 45 | 0xFF << 56 reads as zero does.
    let rest = 0x0006_0000 | 1 << 36 | 0xFF << 38 | 1 << 45 | 0xFF << 56;
    assert_eq!(Capabilities::read(rest), zero);
}

#[test]
fn version_and_status_match_hand_worked_values() {
    // Base 1.4, "Offset 8h: VS – Version": MJR 31:16, MNR 15:8, TER 7:0.
    // 0x0001_0400: 1.4.0. 0x0002_0001: 2.0.1.
    let v = |major, minor, tertiary| Version {
        major,
        minor,
        tertiary,
    };
    assert_eq!(Version::read(0x0001_0400), v(1, 4, 0));
    assert_eq!(Version::read(0x0002_0001), v(2, 0, 1));
    assert_eq!(Version::read(u32::MAX), v(0xFFFF, 0xFF, 0xFF));
    // Base 1.4, "Offset 1Ch: CSTS – Controller Status": RDY 0, CFS 1, SHST
    // 3:2 (00 normal, 01 shutdown processing, 10 shutdown complete, 11
    // reserved); the rest is ignored.
    let s = |ready, fatal, shutdown| ControllerStatus {
        ready,
        fatal,
        shutdown,
    };
    use ShutdownStatus::*;
    assert_eq!(ControllerStatus::read(0x0), s(false, false, Normal));
    assert_eq!(ControllerStatus::read(0x1), s(true, false, Normal));
    assert_eq!(ControllerStatus::read(0x2), s(false, true, Normal));
    // 0x4 = 01 << 2, 0x8 = 10 << 2, 0xC = 11 << 2.
    assert_eq!(ControllerStatus::read(0x4), s(false, false, Processing));
    assert_eq!(ControllerStatus::read(0x9), s(true, false, Complete));
    assert_eq!(ControllerStatus::read(0xC), s(false, false, Reserved));
    assert_eq!(ControllerStatus::read(!0xF), s(false, false, Normal));
}

#[test]
fn page_sizes_inside_the_controllers_range_are_accepted() {
    // MPSMIN 2 (16 KiB) and MPSMAX 4 (64 KiB): bits 51:48 = 2, 55:52 = 4.
    let cap = Capabilities::read(0x0042_0000_0000_0000);
    for bytes in [0x4000, 0x8000, 0x1_0000] {
        assert_eq!(PageSize::new(bytes, &cap).map(PageSize::bytes), Ok(bytes));
    }
    assert_eq!(page(4096).bytes(), 4096);
    assert_eq!(page(1 << 27).bytes(), 1 << 27);
}

#[test]
fn refuses_a_page_size_outside_the_controllers_range() {
    let cap = Capabilities::read(0x0042_0000_0000_0000);
    // Below 16 KiB, above 64 KiB, not a power of two, zero.
    for bytes in [0x1000, 0x2000, 0x2_0000, 0x5000, 0xC000, 0, u64::MAX] {
        assert_eq!(
            PageSize::new(bytes, &cap),
            Err(Error::PageSize),
            "{bytes:#x}"
        );
    }
    // Past 2 ^ 27, which no CAP can state.
    assert_eq!(PageSize::new(1 << 28, &wide()), Err(Error::PageSize));
}

#[test]
fn refuses_an_entry_size_past_the_four_bit_field() {
    assert_eq!(entry(15).bytes(), 1 << 15);
    for shift in [16, 17, 0xFF] {
        assert_eq!(EntrySize::new(shift), Err(Error::EntrySize), "{shift}");
    }
}

#[test]
fn configuration_matches_hand_worked_values() {
    // Base 1.4, "Offset 14h: CC – Controller Configuration": EN 0, CSS 6:4
    // (000 NVM, 111 admin only), MPS 10:7 (2 ^ (12 + n)), AMS 13:11 (000
    // round robin, 001 weighted round robin with urgent, 111 vendor
    // specific), SHN 15:14 (01 normal, 10 abrupt), IOSQES 19:16, IOCQES
    // 23:20.
    // The usual one, 4 KiB pages: 1 | 6 << 16 (0x6_0000) | 4 << 20
    // (0x40_0000) = 0x0046_0001.
    let usual = Configuration::nvm(page(4096));
    assert_eq!(usual.encode(), 0x0046_0001);
    assert_eq!(usual.submission_entry, EntrySize::SUBMISSION);
    assert_eq!(usual.completion_entry, EntrySize::COMPLETION);
    assert_eq!(EntrySize::SUBMISSION.bytes(), 64);
    assert_eq!(EntrySize::COMPLETION.bytes(), 16);
    // Asking for a normal shutdown: | 01 << 14 = 0x4000.
    let shutdown = Configuration {
        shutdown: Shutdown::Normal,
        ..usual
    };
    assert_eq!(shutdown.encode(), 0x0046_4001);
    // Every field at its top: 1 | 7 << 4 (0x70) | 15 << 7 (0x780) | 7 << 11
    // (0x3800) | 10 << 14 (0x8000) | 15 << 16 | 15 << 20 = 0x00FF_BFF1.
    let top = Configuration {
        enable: true,
        command_set: CommandSet::AdminOnly,
        page_size: page(1 << 27),
        arbitration: Arbitration::VendorSpecific,
        shutdown: Shutdown::Abrupt,
        submission_entry: entry(15),
        completion_entry: entry(15),
    };
    assert_eq!(top.encode(), 0x00FF_BFF1);
    // Weighted round robin, disabled, 8 KiB pages, entries 1 byte: 1 << 11
    // (0x800) | 1 << 7 (0x80) = 0x880.
    let wrr = Configuration {
        enable: false,
        command_set: CommandSet::Nvm,
        page_size: page(8192),
        arbitration: Arbitration::WeightedRoundRobin,
        shutdown: Shutdown::None,
        submission_entry: entry(0),
        completion_entry: entry(0),
    };
    assert_eq!(wrr.encode(), 0x0000_0880);
    assert_eq!(Configuration::decode(0x0046_0001), Some(usual));
    assert_eq!(Configuration::decode(0x00FF_BFF1), Some(top));
}

#[test]
fn configuration_round_trips_at_each_extreme() {
    for enable in [false, true] {
        for command_set in [CommandSet::Nvm, CommandSet::AdminOnly] {
            for bytes in [4096, 1 << 27] {
                for arbitration in [
                    Arbitration::RoundRobin,
                    Arbitration::WeightedRoundRobin,
                    Arbitration::VendorSpecific,
                ] {
                    for shutdown in [Shutdown::None, Shutdown::Normal, Shutdown::Abrupt] {
                        for shift in [0, 15] {
                            let value = Configuration {
                                enable,
                                command_set,
                                page_size: page(bytes),
                                arbitration,
                                shutdown,
                                submission_entry: entry(shift),
                                completion_entry: entry(15 - shift),
                            };
                            assert_eq!(
                                Configuration::decode(value.encode()),
                                Some(value),
                                "{value:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn other_configurations_are_not_read() {
    for value in [
        // Reserved bits 3:1, 31:24.
        0x0046_0003,
        0x0046_0009,
        0x0146_0001,
        0x8046_0001,
        // CSS 001 and 110, AMS 010, SHN 11.
        0x0046_0011,
        0x0046_0061,
        0x0046_1001,
        0x0046_C001,
    ] {
        assert_eq!(Configuration::decode(value), None, "{value:#x}");
    }
}

#[test]
fn admin_queues_match_hand_worked_values() {
    // Base 1.4, "Offset 24h: AQA – Admin Queue Attributes": ASQS 11:0, ACQS
    // 27:16, both zero-based.
    // Two entries each, as QEMU's firmware sets: 1 | 1 << 16 = 0x0001_0001.
    let small = AdminQueues::new(2, 2).unwrap();
    assert_eq!(small.encode(), 0x0001_0001);
    // 32 and 64: 31 (0x1F) | 63 << 16 (0x3F_0000).
    let mixed = AdminQueues::new(32, 64).unwrap();
    assert_eq!(mixed.encode(), 0x003F_001F);
    assert_eq!(
        (mixed.submission_entries(), mixed.completion_entries()),
        (32, 64)
    );
    // 4096 each: 0xFFF | 0xFFF << 16.
    let large = AdminQueues::new(4096, 4096).unwrap();
    assert_eq!(large.encode(), 0x0FFF_0FFF);
    for value in [small, mixed, large, AdminQueues::new(2, 4096).unwrap()] {
        assert_eq!(AdminQueues::decode(value.encode()), Some(value));
    }
    // One entry (zero-based 0) and the reserved bits 15:12 and 31:28.
    for value in [0x0001_0000, 0x0000_0001, 0x0001_1001, 0x1001_0001] {
        assert_eq!(AdminQueues::decode(value), None, "{value:#x}");
    }
}

#[test]
fn refuses_an_admin_queue_of_0_1_or_more_than_4096_entries() {
    for (sq, cq) in [
        (0, 2),
        (1, 2),
        (2, 0),
        (2, 1),
        (4097, 2),
        (2, 4097),
        (2, u32::MAX),
    ] {
        assert_eq!(AdminQueues::new(sq, cq), Err(Error::QueueSize), "{sq} {cq}");
    }
}

// --- Queue values ---

#[test]
fn refuses_queue_id_0_for_an_io_queue() {
    // Queue 0 is the admin queue pair.
    assert_eq!(IoQueueId::new(0), Err(Error::QueueId));
    assert_eq!(id(1).number(), 1);
    assert_eq!(id(0xFFFF).number(), 0xFFFF);
}

#[test]
fn refuses_a_queue_of_0_1_or_more_entries_than_the_controller_takes() {
    // MQES 0x07FF, as QEMU's controller states: 2048 entries at most.
    let cap = Capabilities::read(0x07FF);
    assert_eq!(QueueSize::new(2, &cap).map(QueueSize::entries), Ok(2));
    assert_eq!(QueueSize::new(2048, &cap).map(QueueSize::entries), Ok(2048));
    assert_eq!(size(65536).entries(), 65536);
    for entries in [0, 1, 2049, 65536, u32::MAX] {
        assert_eq!(
            QueueSize::new(entries, &cap),
            Err(Error::QueueSize),
            "{entries}"
        );
    }
    assert_eq!(QueueSize::new(65537, &wide()), Err(Error::QueueSize));
}

#[test]
fn refuses_a_queue_count_of_0_or_more_than_65535() {
    assert_eq!(count(1).count(), 1);
    assert_eq!(count(65535).count(), 65535);
    // 65,536 would be the zero-based value 0xFFFF, which Base 1.4 calls
    // invalid in "Number of Queues".
    for queues in [0, 65536, u32::MAX] {
        assert_eq!(QueueCount::new(queues), Err(Error::QueueCount), "{queues}");
    }
}

#[test]
fn refuses_a_block_count_of_0_or_more_than_65536() {
    assert_eq!(blocks(1).count(), 1);
    assert_eq!(blocks(65536).count(), 65536);
    for number in [0, 65537, u32::MAX] {
        assert_eq!(BlockCount::new(number), Err(Error::BlockCount), "{number}");
    }
}

// --- Submission and completion entries ---

#[test]
fn submission_entry_fields_sit_where_the_specification_puts_them() {
    // Base 1.4, "Submission Queue Entry – Command Format": CDW0 opcode 7:0,
    // FUSE 9:8, PSDT 15:14, CID 31:16; NSID at byte 4; MPTR at 16; PRP1 at
    // 24; PRP2 at 32; CDW10 to CDW15 at 40 to 63.
    let mut command = Command::new(0x06);
    command.set_command_id(0xBEEF);
    command.set_namespace(0x0102_0304);
    command.set_prp1(0x1122_3344_5566_7788);
    command.set_prp2(0x99AA_BBCC_DDEE_FF00);
    command.set_dwords([10, 11, 12, 13, 14, 0xFFFF_FFFF]);
    let bytes = command.bytes();
    assert_eq!(bytes[..4], [0x06, 0x00, 0xEF, 0xBE]);
    assert_eq!(bytes[4..8], [0x04, 0x03, 0x02, 0x01]);
    assert_eq!(bytes[8..24], [0; 16]);
    assert_eq!(u64_at(bytes, 24), 0x1122_3344_5566_7788);
    assert_eq!(u64_at(bytes, 32), 0x99AA_BBCC_DDEE_FF00);
    for (n, at) in (10..15).zip((40..60).step_by(4)) {
        assert_eq!(u32_at(bytes, at), n, "CDW{n}");
    }
    assert_eq!(bytes[60..], [0xFF; 4]);
    // Read back through the accessors, and from the bytes.
    let again = Command::from_bytes(*bytes);
    assert_eq!(again, command);
    assert_eq!(again.opcode(), 0x06);
    assert_eq!(again.command_id(), 0xBEEF);
    assert_eq!(again.namespace(), 0x0102_0304);
    assert_eq!(again.prp1(), 0x1122_3344_5566_7788);
    assert_eq!(again.prp2(), 0x99AA_BBCC_DDEE_FF00);
    assert_eq!(again.dwords(), [10, 11, 12, 13, 14, 0xFFFF_FFFF]);
}

/// A completion entry from its four dwords.
fn completion(dwords: [u32; 4]) -> Completion {
    let mut bytes = [0; 16];
    for (at, dword) in dwords.iter().enumerate() {
        bytes[at * 4..at * 4 + 4].copy_from_slice(&dword.to_le_bytes());
    }
    Completion::from_bytes(bytes)
}

#[test]
fn completion_entries_match_hand_worked_values() {
    // Base 1.4, "Completion Queue Entry": DW0 command specific; DW2 SQHD
    // 15:0, SQID 31:16; DW3 CID 15:0, phase tag 16, status field 31:17: SC
    // 24:17, SCT 27:25, CRD 29:28, M 30, DNR 31.
    // A success: SQ head 5 of queue 3 (5 | 3 << 16), command 0x1234 with the
    // phase set (0x1234 | 1 << 16).
    let done = completion([0x0003_0007, 0, 0x0003_0005, 0x0001_1234]);
    assert_eq!(done.specific(), 0x0003_0007);
    assert_eq!(done.submission_head(), 5);
    assert_eq!(done.submission_queue(), 3);
    assert_eq!(done.command_id(), 0x1234);
    assert!(done.phase());
    assert_eq!(done.status(), CommandStatus::Success);
    assert!(!done.more() && !done.do_not_retry());
    // Command specific (SCT 1) status 01h, "Invalid Queue Identifier" for a
    // queue creation, do not retry, phase clear: 0x1234 | 0x01 << 17
    // (0x2_0000) | 1 << 25 (0x200_0000) | 1 << 31 = 0x8202_1234.
    let refused = completion([0, 0, 0, 0x8202_1234]);
    assert!(!refused.phase());
    assert_eq!(
        refused.status(),
        CommandStatus::Failure {
            code_type: 1,
            code: 0x01
        }
    );
    assert!(refused.do_not_retry() && !refused.more());
    // Generic (SCT 0) 02h, "Invalid Field in Command", with more: 0x02 << 17
    // (0x4_0000) | 1 << 30.
    let more = completion([0, 0, 0, 0x4004_0000]);
    assert_eq!(
        more.status(),
        CommandStatus::Failure {
            code_type: 0,
            code: 0x02
        }
    );
    assert!(more.more() && !more.do_not_retry());
    // Every status bit: SC 0xFF, SCT 7; CRD (29:28) is not read.
    let all = completion([0, 0, 0, 0xFFFE_0000]);
    assert_eq!(
        all.status(),
        CommandStatus::Failure {
            code_type: 7,
            code: 0xFF
        }
    );
    // CRD alone, with a zero status code and type: still a success.
    assert_eq!(
        completion([0, 0, 0, 0x3000_0000]).status(),
        CommandStatus::Success
    );
    assert_eq!(
        completion([0, 0, 0xFFFF_FFFF, 0]).submission_queue(),
        0xFFFF
    );
}

// --- Admin commands ---

/// The opcode, namespace, PRP1, PRP2 and CDW10 to CDW15 of an entry, and
/// that the rest is zero.
fn fields(command: &Command) -> (u8, u32, u64, u64, [u32; 6]) {
    let bytes = command.bytes();
    assert_eq!(bytes[1], 0, "FUSE and PSDT");
    assert_eq!(bytes[8..24], [0; 16], "CDW2, CDW3 and MPTR");
    (
        bytes[0],
        u32_at(bytes, 4),
        u64_at(bytes, 24),
        u64_at(bytes, 32),
        command.dwords(),
    )
}

#[test]
fn identify_commands_match_hand_worked_entries() {
    // Base 1.4, "Identify command": opcode 06h, CDW10 CNS 7:0 (00h a
    // namespace, 01h the controller, 02h the active namespace IDs above
    // NSID), the data at PRP1 and PRP2.
    let buffer = data(0x0012_3000, 0);
    let controller = Admin::Identify {
        target: Identify::Controller,
        data: buffer,
    };
    assert_eq!(
        fields(&controller.encode()),
        (0x06, 0, 0x0012_3000, 0, [1, 0, 0, 0, 0, 0])
    );
    let namespace = Admin::Identify {
        target: Identify::Namespace(1),
        data: data(0x0012_3800, 0x0012_4000),
    };
    assert_eq!(
        fields(&namespace.encode()),
        (0x06, 1, 0x0012_3800, 0x0012_4000, [0; 6])
    );
    let list = Admin::Identify {
        target: Identify::ActiveNamespaces { after: 0x10 },
        data: buffer,
    };
    assert_eq!(
        fields(&list.encode()),
        (0x06, 0x10, 0x0012_3000, 0, [2, 0, 0, 0, 0, 0])
    );
}

#[test]
fn queue_commands_match_hand_worked_entries() {
    // Base 1.4, "Create I/O Completion Queue command": opcode 05h, PRP1 the
    // queue, CDW10 QID 15:0 and QSIZE 31:16 (zero-based), CDW11 PC 0, IEN 1,
    // IV 31:16.
    // Queue 1, 256 entries, contiguous, vector 3: CDW10 1 | 255 << 16 =
    // 0x00FF_0001; CDW11 1 | 1 << 1 | 3 << 16 = 0x0003_0003.
    let cq = Admin::CreateCompletionQueue {
        id: id(1),
        size: size(256),
        base: 0x0020_0000,
        contiguous: true,
        interrupt: Some(3),
    };
    assert_eq!(
        fields(&cq.encode()),
        (
            0x05,
            0,
            0x0020_0000,
            0,
            [0x00FF_0001, 0x0003_0003, 0, 0, 0, 0]
        )
    );
    // Queue 0xFFFF, 65,536 entries, not contiguous, no interrupts: CDW10
    // 0xFFFF | 0xFFFF << 16; CDW11 0.
    let polled = Admin::CreateCompletionQueue {
        id: id(0xFFFF),
        size: size(65536),
        base: 0xFFFF_FFFF_FFFF_F000,
        contiguous: false,
        interrupt: None,
    };
    assert_eq!(
        fields(&polled.encode()),
        (
            0x05,
            0,
            0xFFFF_FFFF_FFFF_F000,
            0,
            [0xFFFF_FFFF, 0, 0, 0, 0, 0]
        )
    );
    // Base 1.4, "Create I/O Submission Queue command": opcode 01h, CDW10 as
    // above, CDW11 PC 0, QPRIO 2:1 (left 00), CQID 31:16.
    // Queue 2, 2 entries, contiguous, on CQ 1: CDW10 2 | 1 << 16 =
    // 0x0001_0002; CDW11 1 | 1 << 16 = 0x0001_0001.
    let sq = Admin::CreateSubmissionQueue {
        id: id(2),
        size: size(2),
        base: 0x0020_1000,
        contiguous: true,
        completion_queue: id(1),
    };
    assert_eq!(
        fields(&sq.encode()),
        (
            0x01,
            0,
            0x0020_1000,
            0,
            [0x0001_0002, 0x0001_0001, 0, 0, 0, 0]
        )
    );
    // "Delete I/O Submission Queue command" (00h) and "Delete I/O Completion
    // Queue command" (04h): CDW10 QID 15:0.
    let delete_sq = Admin::DeleteSubmissionQueue(id(1));
    assert_eq!(
        fields(&delete_sq.encode()),
        (0x00, 0, 0, 0, [1, 0, 0, 0, 0, 0])
    );
    let delete_cq = Admin::DeleteCompletionQueue(id(0xFFFF));
    assert_eq!(
        fields(&delete_cq.encode()),
        (0x04, 0, 0, 0, [0xFFFF, 0, 0, 0, 0, 0])
    );
}

#[test]
fn number_of_queues_matches_hand_worked_entries() {
    // Base 1.4, "Set Features command": opcode 09h, CDW10 FID 7:0 (SV, bit
    // 31, clear); "Number of Queues", FID 07h: CDW11 NSQR 15:0 and NCQR
    // 31:16, zero-based; the completion's DW0 has NSQA 15:0 and NCQA 31:16,
    // zero-based.
    // Four submission and two completion queues: 3 | 1 << 16 = 0x0001_0003.
    let ask = Admin::SetNumberOfQueues {
        submission: count(4),
        completion: count(2),
    };
    assert_eq!(
        fields(&ask.encode()),
        (0x09, 0, 0, 0, [0x07, 0x0001_0003, 0, 0, 0, 0])
    );
    // The most: 65,535 each, 0xFFFE | 0xFFFE << 16.
    let most = Admin::SetNumberOfQueues {
        submission: count(65535),
        completion: count(65535),
    };
    assert_eq!(most.encode().dwords()[1], 0xFFFE_FFFE);
    // Granted 0x0003_0007: 8 submission queues and 4 completion queues.
    let granted = completion([0x0003_0007, 0, 0, 0]);
    assert_eq!(granted.granted_queues(), (8, 4));
    assert_eq!(
        completion([u32::MAX, 0, 0, 0]).granted_queues(),
        (65536, 65536)
    );
    assert_eq!(completion([0; 4]).granted_queues(), (1, 1));
}

/// Admin commands with every field at its extremes.
fn admin_extremes() -> Vec<Admin> {
    let mut all = vec![];
    for buffer in [data(0, 0), data(u64::MAX, u64::MAX), data(0x1000, 0)] {
        for target in [
            Identify::Controller,
            Identify::Namespace(1),
            Identify::Namespace(u32::MAX),
            Identify::ActiveNamespaces { after: 0 },
            Identify::ActiveNamespaces { after: u32::MAX },
        ] {
            all.push(Admin::Identify {
                target,
                data: buffer,
            });
        }
    }
    for queue in [id(1), id(0xFFFF)] {
        for entries in [size(2), size(65536)] {
            for base in [0, u64::MAX] {
                for contiguous in [false, true] {
                    for interrupt in [None, Some(0), Some(0xFFFF)] {
                        all.push(Admin::CreateCompletionQueue {
                            id: queue,
                            size: entries,
                            base,
                            contiguous,
                            interrupt,
                        });
                    }
                    for completion_queue in [id(1), id(0xFFFF)] {
                        all.push(Admin::CreateSubmissionQueue {
                            id: queue,
                            size: entries,
                            base,
                            contiguous,
                            completion_queue,
                        });
                    }
                }
            }
        }
        all.push(Admin::DeleteSubmissionQueue(queue));
        all.push(Admin::DeleteCompletionQueue(queue));
    }
    for submission in [count(1), count(65535)] {
        for completion in [count(1), count(65535)] {
            all.push(Admin::SetNumberOfQueues {
                submission,
                completion,
            });
        }
    }
    all
}

#[test]
fn admin_commands_round_trip_at_each_extreme() {
    for command in admin_extremes() {
        let mut entry = command.encode();
        assert_eq!(Admin::decode(&entry), Some(command), "{command:?}");
        // The command identifier is the caller's, and not part of it.
        entry.set_command_id(0xFFFF);
        assert_eq!(Admin::decode(&entry), Some(command), "{command:?}");
    }
}

#[test]
fn other_admin_entries_are_not_read() {
    let identify = Admin::Identify {
        target: Identify::Controller,
        data: data(0x1000, 0),
    }
    .encode();
    let cq = Admin::CreateCompletionQueue {
        id: id(1),
        size: size(2),
        base: 0x1000,
        contiguous: true,
        interrupt: None,
    }
    .encode();
    let mut cases = vec![];
    let mut change = |base: &Command, edit: &dyn Fn(&mut [u8; 64])| {
        let mut bytes = *base.bytes();
        edit(&mut bytes);
        cases.push(Command::from_bytes(bytes));
    };
    // Another opcode: Get Log Page (02h), Abort (08h), Get Features (0Ah).
    for opcode in [0x02, 0x08, 0x0A, 0xFF] {
        change(&identify, &|b| b[0] = opcode);
    }
    // FUSE (bits 9:8) and PSDT (15:14).
    change(&identify, &|b| b[1] = 0x01);
    change(&identify, &|b| b[1] = 0x40);
    // The controller with a namespace; CNS 03h; a controller ID in CDW10.
    change(&identify, &|b| b[4] = 1);
    change(&identify, &|b| b[40] = 3);
    change(&identify, &|b| b[42] = 1);
    // MPTR, and CDW15.
    change(&identify, &|b| b[16] = 1);
    change(&identify, &|b| b[63] = 1);
    // Queue 0, one entry (QSIZE 0), PRP2, a vector with interrupts off,
    // CDW11 bit 2.
    change(&cq, &|b| b[40] = 0);
    change(&cq, &|b| b[42] = 0);
    change(&cq, &|b| b[32] = 1);
    change(&cq, &|b| b[46] = 1);
    change(&cq, &|b| b[44] |= 4);
    // A submission queue on completion queue 0, or with a priority.
    let sq = Admin::CreateSubmissionQueue {
        id: id(1),
        size: size(2),
        base: 0x1000,
        contiguous: true,
        completion_queue: id(1),
    }
    .encode();
    change(&sq, &|b| b[46] = 0);
    change(&sq, &|b| b[44] |= 2);
    // Delete queue 0; a QSIZE on a delete.
    let delete = Admin::DeleteCompletionQueue(id(1)).encode();
    change(&delete, &|b| b[40] = 0);
    change(&delete, &|b| b[42] = 1);
    // Another feature (Arbitration, 01h); the save bit; 0xFFFF requested.
    let queues = Admin::SetNumberOfQueues {
        submission: count(1),
        completion: count(1),
    }
    .encode();
    change(&queues, &|b| b[40] = 0x01);
    change(&queues, &|b| b[43] = 0x80);
    change(&queues, &|b| b[44..46].copy_from_slice(&[0xFF, 0xFF]));
    for entry in cases {
        assert_eq!(Admin::decode(&entry), None, "{:x?}", entry.bytes());
    }
}

// --- I/O commands ---

#[test]
fn read_and_write_match_hand_worked_entries() {
    // NVM command set, "Read command" (02h) and "Write command" (01h): the
    // namespace in NSID, data at PRP1 and PRP2, CDW10 and CDW11 the
    // starting LBA (low, high), CDW12 NLB 15:0 zero-based; LR, FUA, PRINFO
    // and the rest left 0.
    // Namespace 1, block 0x1_2345_6789, 8 blocks: CDW10 0x2345_6789, CDW11
    // 1, CDW12 7.
    let read = Io::Read(Transfer {
        namespace: 1,
        start: 0x1_2345_6789,
        blocks: blocks(8),
        data: data(0x0030_0000, 0),
    });
    assert_eq!(
        fields(&read.encode()),
        (0x02, 1, 0x0030_0000, 0, [0x2345_6789, 1, 7, 0, 0, 0])
    );
    // 65,536 blocks to the last block, written: NLB 0xFFFF.
    let write = Io::Write(Transfer {
        namespace: 0xFFFF_FFFE,
        start: u64::MAX,
        blocks: blocks(65536),
        data: data(0x0030_0000, 0x0030_1000),
    });
    assert_eq!(
        fields(&write.encode()),
        (
            0x01,
            0xFFFF_FFFE,
            0x0030_0000,
            0x0030_1000,
            [0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF, 0, 0, 0]
        )
    );
}

#[test]
fn io_commands_round_trip_at_each_extreme() {
    for namespace in [0, 1, u32::MAX] {
        for start in [0, 0xFFFF_FFFF, u64::MAX] {
            for number in [1, 2, 65536] {
                for buffer in [data(0, 0), data(u64::MAX, u64::MAX)] {
                    let transfer = Transfer {
                        namespace,
                        start,
                        blocks: blocks(number),
                        data: buffer,
                    };
                    for command in [Io::Read(transfer), Io::Write(transfer)] {
                        let mut entry = command.encode();
                        entry.set_command_id(0x8001);
                        assert_eq!(Io::decode(&entry), Some(command), "{command:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn other_io_entries_are_not_read() {
    let base = Io::Read(Transfer {
        namespace: 1,
        start: 0,
        blocks: blocks(1),
        data: data(0x1000, 0),
    })
    .encode();
    // Flush (00h) and Compare (05h); FUSE; MPTR; FUA (CDW12 bit 30);
    // limited retry (bit 31); CDW13; CDW15.
    for (at, value) in [
        (0, 0x00),
        (0, 0x05),
        (1, 0x01),
        (20, 0x01),
        (51, 0x40),
        (51, 0x80),
        (52, 0x01),
        (60, 0x01),
    ] {
        let mut bytes = *base.bytes();
        bytes[at] = value;
        let entry = Command::from_bytes(bytes);
        assert_eq!(Io::decode(&entry), None, "byte {at} = {value:#x}");
    }
}

// --- Identify data ---

/// An Identify Controller buffer as QEMU's controller fills one, by hand.
fn controller_data() -> Vec<u8> {
    let mut b = vec![0; 4096];
    // VID at 1:0, SN at 23:4, MN at 63:24, FR at 71:64, all ASCII padded
    // with spaces.
    b[0..2].copy_from_slice(&0x1B36u16.to_le_bytes());
    b[4..24].copy_from_slice(b"bismuth0            ");
    b[24..64].copy_from_slice(b"QEMU NVMe Ctrl                          ");
    b[64..72].copy_from_slice(b"1.0     ");
    // MDTS at 77: 7. SQES at 512: required 6 (3:0), maximum 6 (7:4). CQES
    // at 513: 4 and 4. NN at 519:516: 256.
    b[77] = 7;
    b[512] = 0x66;
    b[513] = 0x44;
    b[516..520].copy_from_slice(&256u32.to_le_bytes());
    b
}

#[test]
fn identify_controller_fields_are_read() {
    // Base 1.4, "Identify Controller Data Structure".
    let bytes = controller_data();
    let ctrl = IdentifyController::read(&bytes).unwrap();
    assert_eq!(ctrl.vendor_id(), 0x1B36);
    assert_eq!(ctrl.serial_number(), b"bismuth0            ");
    assert_eq!(trim_spaces(ctrl.serial_number()), b"bismuth0");
    assert_eq!(trim_spaces(ctrl.model_number()), b"QEMU NVMe Ctrl");
    assert_eq!(ctrl.model_number().len(), 40);
    assert_eq!(trim_spaces(ctrl.firmware_revision()), b"1.0");
    // 2 ^ 7 pages of the minimum page size: 512 KiB with 4 KiB pages.
    assert_eq!(ctrl.max_transfer_shift(), Some(7));
    assert_eq!(ctrl.submission_entry_size(), EntrySize::SUBMISSION);
    assert_eq!(ctrl.completion_entry_size(), EntrySize::COMPLETION);
    assert_eq!(ctrl.namespaces(), 256);
    // MDTS 0 is no limit; the required size is the low half of the byte.
    let mut bytes = bytes;
    bytes[77] = 0;
    bytes[512] = 0xF5;
    bytes[513] = 0x0F;
    let ctrl = IdentifyController::read(&bytes).unwrap();
    assert_eq!(ctrl.max_transfer_shift(), None);
    assert_eq!(ctrl.submission_entry_size(), entry(5));
    assert_eq!(ctrl.completion_entry_size(), entry(15));
    // A longer buffer is read by its first 4096 bytes.
    bytes.push(0xFF);
    assert_eq!(IdentifyController::read(&bytes).unwrap().namespaces(), 256);
}

#[test]
fn trim_spaces_removes_only_trailing_spaces() {
    assert_eq!(trim_spaces(b"  a b  "), b"  a b");
    assert_eq!(trim_spaces(b"    "), b"");
    assert_eq!(trim_spaces(b""), b"");
    assert_eq!(trim_spaces(b"a\0"), b"a\0");
}

/// An Identify Namespace buffer: 0x800 blocks, two formats, the second in
/// use.
fn namespace_data() -> Vec<u8> {
    let mut b = vec![0; 4096];
    // NSZE at 7:0, NCAP at 15:8, NLBAF at 25 (zero-based), FLBAS at 26
    // (format index 3:0).
    b[0..8].copy_from_slice(&0x800u64.to_le_bytes());
    b[8..16].copy_from_slice(&0x7FFu64.to_le_bytes());
    b[25] = 1;
    b[26] = 0x01;
    // LBAF0 at 131:128 and LBAF1 at 135:132: MS 15:0, LBADS 23:16. 512-byte
    // blocks without metadata: 00 00 09 00. 4096-byte blocks with 8 bytes
    // of metadata: 08 00 0C 00.
    b[128..132].copy_from_slice(&[0x00, 0x00, 0x09, 0x00]);
    b[132..136].copy_from_slice(&[0x08, 0x00, 0x0C, 0x00]);
    b
}

#[test]
fn identify_namespace_fields_are_read() {
    // Base 1.4, "Identify Namespace Data Structure, NVM Command Set
    // Specific" and its "LBA Format Data Structure".
    let bytes = namespace_data();
    let ns = IdentifyNamespace::read(&bytes).unwrap();
    assert_eq!(ns.size(), 0x800);
    assert_eq!(ns.capacity(), 0x7FF);
    assert_eq!(ns.format_index(), 1);
    let format = ns.block_format().unwrap();
    assert_eq!(
        format,
        BlockFormat {
            shift: 12,
            metadata: 8
        }
    );
    assert_eq!(format.block_size(), 4096);
    // Bit 4 of FLBAS (metadata at the end of the block) is not the index.
    let mut bytes = bytes;
    bytes[26] = 0x10;
    let ns = IdentifyNamespace::read(&bytes).unwrap();
    assert_eq!(ns.format_index(), 0);
    assert_eq!(ns.block_format().unwrap().block_size(), 512);
    // Format 15, the last, at 128 + 15 * 4 = 188: 64 KiB blocks, metadata
    // 0xFFFF.
    bytes[26] = 0x0F;
    bytes[188..192].copy_from_slice(&[0xFF, 0xFF, 0x10, 0x00]);
    let ns = IdentifyNamespace::read(&bytes).unwrap();
    assert_eq!(
        ns.block_format(),
        Ok(BlockFormat {
            shift: 16,
            metadata: 0xFFFF
        })
    );
    assert_eq!(ns.block_format().unwrap().block_size(), 0x1_0000);
}

#[test]
fn refuses_a_block_size_shift_outside_512_bytes_to_64_kib() {
    let mut bytes = namespace_data();
    for shift in [0, 8, 17, 0xFF] {
        bytes[134] = shift;
        let ns = IdentifyNamespace::read(&bytes).unwrap();
        assert_eq!(ns.block_format(), Err(Error::BlockShift), "{shift}");
    }
}

#[test]
fn active_namespace_ids_stop_at_the_first_zero() {
    // Base 1.4, "Identify command", CNS 02h: up to 1024 IDs, in increasing
    // order, the unused ones zero.
    let mut bytes = vec![0; 4096];
    for (at, nsid) in [1u32, 2, 5, 0, 7].iter().enumerate() {
        bytes[at * 4..at * 4 + 4].copy_from_slice(&nsid.to_le_bytes());
    }
    let ids: Vec<u32> = ActiveNamespaces::read(&bytes).unwrap().collect();
    assert_eq!(ids, [1, 2, 5]);
    let none = vec![0; 4096];
    assert_eq!(ActiveNamespaces::read(&none).unwrap().count(), 0);
    // A full list: all 1024, and nothing past the buffer's 4096 bytes.
    let mut full = vec![0xFF; 4100];
    full[4..8].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    let ids: Vec<u32> = ActiveNamespaces::read(&full).unwrap().collect();
    assert_eq!(ids.len(), 1024);
    assert_eq!(ids[..2], [0xFFFF_FFFF, 0xFFFF_FFFE]);
}

#[test]
fn refuses_an_identify_buffer_shorter_than_4096_bytes() {
    for length in [0, 1, 4095] {
        let bytes = vec![0; length];
        assert_eq!(
            IdentifyController::read(&bytes).err(),
            Some(Error::Truncated)
        );
        assert_eq!(
            IdentifyNamespace::read(&bytes).err(),
            Some(Error::Truncated)
        );
        assert_eq!(ActiveNamespaces::read(&bytes).err(), Some(Error::Truncated));
    }
}

// --- QEMU's controller ---

#[test]
fn reads_the_registers_of_qemus_controller() {
    // Bytes 00h to 3Fh of the registers of QEMU 11.1's NVMe controller once
    // its firmware has set it up, captured as `tests/data/qemu-registers.md`
    // says.
    let regs: &[u8; 64] = include_bytes!("../tests/data/qemu-registers.bin");
    // CAP = 0x0040_0820_0F01_07FF: MQES 0x7FF (2048 entries), CQR set, TO
    // 0x0F (7.5 seconds), DSTRD 0; high dword 0x0040_0820: bit 37 (0x20,
    // NVM command set) and bit 43 (0x800, from a later revision, ignored);
    // MPSMIN 0 (4 KiB), MPSMAX 4 (64 KiB).
    assert_eq!(u64_at(regs, register::CAP), 0x0040_0820_0F01_07FF);
    let cap = Capabilities::read(u64_at(regs, register::CAP));
    assert_eq!(
        cap,
        Capabilities {
            max_entries: 0x07FF,
            contiguous_required: true,
            timeout: 0x0F,
            doorbell_stride: 0,
            nvm_command_set: true,
            min_page_shift: 12,
            max_page_shift: 16,
        }
    );
    // VS = 0x0001_0400: 1.4.0.
    assert_eq!(
        Version::read(u32_at(regs, register::VS)),
        Version {
            major: 1,
            minor: 4,
            tertiary: 0
        }
    );
    // INTMS and INTMC read as 0.
    assert_eq!(u32_at(regs, register::INTMS), 0);
    assert_eq!(u32_at(regs, register::INTMC), 0);
    // CC = 0x0046_0001: the firmware enabled it with exactly the usual
    // configuration for 4 KiB pages.
    assert_eq!(u32_at(regs, register::CC), 0x0046_0001);
    let page = PageSize::new(4096, &cap).unwrap();
    assert_eq!(
        Configuration::decode(u32_at(regs, register::CC)),
        Some(Configuration::nvm(page))
    );
    // CSTS = 1: ready, no fatal status, operating normally.
    assert_eq!(
        ControllerStatus::read(u32_at(regs, register::CSTS)),
        ControllerStatus {
            ready: true,
            fatal: false,
            shutdown: ShutdownStatus::Normal
        }
    );
    // AQA = 0x0001_0001: two-entry admin queues; ASQ and ACQ the pages at
    // 0x0E4D_0000 and 0x0E4D_1000.
    let aqa = AdminQueues::decode(u32_at(regs, register::AQA)).unwrap();
    assert_eq!((aqa.submission_entries(), aqa.completion_entries()), (2, 2));
    assert_eq!(u64_at(regs, register::ASQ), 0x0E4D_0000);
    assert_eq!(u64_at(regs, register::ACQ), 0x0E4D_1000);
    // Its doorbells: stride 0, so queue 1's pair at 0x1008 and 0x100C.
    let stride = cap.doorbell_stride;
    assert_eq!(register::doorbell(1, Queue::Submission, stride), Ok(0x1008));
    assert_eq!(register::doorbell(1, Queue::Completion, stride), Ok(0x100C));
    // Its largest I/O queue, and one past it.
    assert_eq!(QueueSize::new(2048, &cap).map(QueueSize::entries), Ok(2048));
    assert_eq!(QueueSize::new(2049, &cap), Err(Error::QueueSize));
}

// --- Random values ---

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

    fn bits(&mut self) -> u32 {
        self.next() as u32
    }

    fn flag(&mut self) -> bool {
        self.next() & 1 != 0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }

    fn id(&mut self) -> IoQueueId {
        id(1 + self.below(0xFFFF) as u16)
    }

    fn size(&mut self) -> QueueSize {
        size(2 + self.below(65535) as u32)
    }

    fn count(&mut self) -> QueueCount {
        count(1 + self.below(65535) as u32)
    }

    fn data(&mut self) -> DataPointer {
        data(self.next(), self.next())
    }

    fn fill(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            *byte = self.next() as u8;
        }
    }

    fn admin(&mut self) -> Admin {
        match self.below(6) {
            0 => Admin::Identify {
                target: match self.below(3) {
                    0 => Identify::Controller,
                    1 => Identify::Namespace(self.bits()),
                    _ => Identify::ActiveNamespaces { after: self.bits() },
                },
                data: self.data(),
            },
            1 => Admin::CreateCompletionQueue {
                id: self.id(),
                size: self.size(),
                base: self.next(),
                contiguous: self.flag(),
                interrupt: self.flag().then(|| self.next() as u16),
            },
            2 => Admin::CreateSubmissionQueue {
                id: self.id(),
                size: self.size(),
                base: self.next(),
                contiguous: self.flag(),
                completion_queue: self.id(),
            },
            3 => Admin::DeleteSubmissionQueue(self.id()),
            4 => Admin::DeleteCompletionQueue(self.id()),
            _ => Admin::SetNumberOfQueues {
                submission: self.count(),
                completion: self.count(),
            },
        }
    }

    fn io(&mut self) -> Io {
        let transfer = Transfer {
            namespace: self.bits(),
            start: self.next(),
            blocks: blocks(1 + self.below(65536) as u32),
            data: self.data(),
        };
        if self.flag() {
            Io::Read(transfer)
        } else {
            Io::Write(transfer)
        }
    }
}

#[test]
fn random_values_round_trip_and_random_bytes_do_not_panic() {
    let mut random = Random(0x0E6D_E5EE_D000_0065);
    let mut recognised = [0u32; 4];
    let mut buffer = vec![0u8; 4096];
    for round in 0..20_000 {
        // Random valid values come back as they went in.
        let page = page(1 << (12 + random.below(16)));
        let configuration = Configuration {
            enable: random.flag(),
            command_set: [CommandSet::Nvm, CommandSet::AdminOnly][usize::from(random.flag())],
            page_size: page,
            arbitration: [
                Arbitration::RoundRobin,
                Arbitration::WeightedRoundRobin,
                Arbitration::VendorSpecific,
            ][random.below(3) as usize],
            shutdown: [Shutdown::None, Shutdown::Normal, Shutdown::Abrupt]
                [random.below(3) as usize],
            submission_entry: entry(random.below(16) as u8),
            completion_entry: entry(random.below(16) as u8),
        };
        assert_eq!(
            Configuration::decode(configuration.encode()),
            Some(configuration)
        );
        let sq = 2 + random.below(4095) as u32;
        let cq = 2 + random.below(4095) as u32;
        let aqa = AdminQueues::new(sq, cq).unwrap();
        assert_eq!(AdminQueues::decode(aqa.encode()), Some(aqa));
        let admin = random.admin();
        let mut entry = admin.encode();
        entry.set_command_id(random.next() as u16);
        assert_eq!(Admin::decode(&entry), Some(admin));
        let io = random.io();
        let mut entry = io.encode();
        entry.set_command_id(random.next() as u16);
        assert_eq!(Io::decode(&entry), Some(io));

        // Random bits and bytes give no panic, and whatever a decoder
        // accepts encodes back to the same bits. The decoders are fed fully
        // random values and nearly-right ones: a valid entry with one random
        // byte, or a register with the bits no field takes cleared.
        let (wild, other) = (random.bits(), random.next());
        let _ = Capabilities::read(other);
        let _ = Version::read(wild);
        let _ = ControllerStatus::read(wild);
        for value in [wild, wild & 0x00FF_FFF1] {
            if let Some(read) = Configuration::decode(value) {
                assert_eq!(read.encode(), value);
                recognised[0] += 1;
            }
        }
        for value in [wild, wild & 0x0FFF_0FFF] {
            if let Some(read) = AdminQueues::decode(value) {
                assert_eq!(read.encode(), value);
                recognised[1] += 1;
            }
        }
        let kind = [Queue::Submission, Queue::Completion][usize::from(random.flag())];
        let _ = register::doorbell(wild as u16, kind, other as u8);
        let _ = PageSize::new(other, &Capabilities::read(random.next()));
        let _ = QueueSize::new(wild, &Capabilities::read(other));
        let _ = QueueCount::new(wild);
        let _ = BlockCount::new(wild);
        let _ = EntrySize::new(wild as u8);
        let _ = AdminQueues::new(wild, other as u32);
        let _ = IoQueueId::new(wild as u16);

        let mut bytes = [0u8; 64];
        random.fill(&mut bytes);
        let mut near = if round % 2 == 0 {
            *random.admin().encode().bytes()
        } else {
            *random.io().encode().bytes()
        };
        near[random.below(64) as usize] = random.next() as u8;
        for candidate in [bytes, near] {
            let command = Command::from_bytes(candidate);
            let _ = (command.opcode(), command.namespace(), command.prp1());
            let _ = (command.prp2(), command.dwords(), command.command_id());
            if let Some(read) = Admin::decode(&command) {
                let mut again = read.encode();
                again.set_command_id(command.command_id());
                assert_eq!(again, command);
                recognised[2] += 1;
            }
            if let Some(read) = Io::decode(&command) {
                let mut again = read.encode();
                again.set_command_id(command.command_id());
                assert_eq!(again, command);
                recognised[3] += 1;
            }
        }
        let mut cqe = [0u8; 16];
        random.fill(&mut cqe);
        let cqe = Completion::from_bytes(cqe);
        let _ = (
            cqe.specific(),
            cqe.submission_head(),
            cqe.submission_queue(),
        );
        let _ = (cqe.command_id(), cqe.phase(), cqe.status(), cqe.more());
        let _ = (cqe.do_not_retry(), cqe.granted_queues());

        // Identify readers over random buffers, every hundredth round, and
        // random lengths below 4096.
        if round % 100 == 0 {
            random.fill(&mut buffer);
            let ctrl = IdentifyController::read(&buffer).unwrap();
            let _ = (ctrl.vendor_id(), trim_spaces(ctrl.serial_number()));
            let _ = (trim_spaces(ctrl.model_number()), ctrl.firmware_revision());
            let _ = (ctrl.max_transfer_shift(), ctrl.namespaces());
            let _ = (ctrl.submission_entry_size(), ctrl.completion_entry_size());
            let ns = IdentifyNamespace::read(&buffer).unwrap();
            let _ = (ns.size(), ns.capacity(), ns.format_index());
            if let Ok(format) = ns.block_format() {
                assert!((9..=16).contains(&format.shift));
                let _ = format.block_size();
            }
            assert!(ActiveNamespaces::read(&buffer).unwrap().count() <= 1024);
        }
        let short = &buffer[..random.below(4096) as usize];
        assert!(IdentifyController::read(short).is_err());
        assert!(IdentifyNamespace::read(short).is_err());
        assert!(ActiveNamespaces::read(short).is_err());
    }
    // Each decoder accepted a good share, so the checks above ran.
    assert!(recognised.iter().all(|&n| n > 1000), "{recognised:?}");
}

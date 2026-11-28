//! Tests for the interrupt controller encodings. Every expected value is a
//! literal worked out by hand from the Intel SDM volume 3A chapter
//! "Advanced Programmable Interrupt Controller (APIC)", the Intel 82093AA
//! I/O APIC data sheet and the Intel 8259A data sheet, with the working in a
//! comment beside it, so a test failing means the library and the documents
//! disagree, not that two copies of the same code do. Bit `n` of a register
//! is written as `1 << n` in the working.

// Tests index and add freely; the crate's lints are for the library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use bismuth_acpi::{InterruptFlags, Polarity, Trigger};

fn vector(number: u8) -> Vector {
    Vector::new(number).unwrap()
}

/// The lowest vector the kernel may assign, the highest, and one between.
fn vector_extremes() -> [Vector; 4] {
    [vector(32), vector(0x80), vector(254), Vector::SPURIOUS]
}

const POLARITIES: [PinPolarity; 2] = [PinPolarity::ActiveHigh, PinPolarity::ActiveLow];
const TRIGGERS: [TriggerMode; 2] = [TriggerMode::Edge, TriggerMode::Level];

fn flags(polarity: Polarity, trigger: Trigger) -> InterruptFlags {
    InterruptFlags { polarity, trigger }
}

// --- Vectors ---

#[test]
fn vectors_32_to_254_are_accepted() {
    for number in [32, 33, 0x80, 0xFE] {
        assert_eq!(Vector::new(number).map(Vector::number), Ok(number));
    }
    assert_eq!(Vector::SPURIOUS.number(), 0xFF);
}

#[test]
fn refuses_an_exception_vector() {
    // SDM vol. 3A, "Exception and Interrupt Vectors": 0 to 31 are the
    // processor's.
    for number in [0, 1, 2, 14, 31] {
        assert_eq!(Vector::new(number), Err(Error::ExceptionVector), "{number}");
    }
}

#[test]
fn refuses_the_spurious_vector_as_an_assigned_one() {
    assert_eq!(Vector::new(0xFF), Err(Error::SpuriousVector));
}

#[test]
fn status_bits_are_read_where_the_hardware_puts_them() {
    // Delivery status is bit 12 (0x1000) and remote IRR bit 14 (0x4000) in
    // the local vector table, the interrupt command register's low half and
    // a redirection entry's low half.
    assert!(delivery_pending(0x0000_1000));
    assert!(!delivery_pending(0xFFFF_EFFF));
    assert!(remote_irr(0x0000_4000));
    assert!(!remote_irr(0xFFFF_BFFF));
}

// --- Local APIC registers ---

#[test]
fn local_register_offsets_match_the_manuals_address_map() {
    // SDM vol. 3A, "Local APIC Register Address Map" table, offsets from
    // the base (0xFEE0_0000 by default).
    assert_eq!(local::ID, 0x020);
    assert_eq!(local::VERSION, 0x030);
    assert_eq!(local::TASK_PRIORITY, 0x080);
    assert_eq!(local::END_OF_INTERRUPT, 0x0B0);
    assert_eq!(local::SPURIOUS, 0x0F0);
    assert_eq!(local::ERROR_STATUS, 0x280);
    assert_eq!(local::COMMAND_LOW, 0x300);
    assert_eq!(local::COMMAND_HIGH, 0x310);
    assert_eq!(local::LVT_TIMER, 0x320);
    assert_eq!(local::LVT_LINT0, 0x350);
    assert_eq!(local::LVT_LINT1, 0x360);
    assert_eq!(local::LVT_ERROR, 0x370);
    assert_eq!(local::TIMER_INITIAL_COUNT, 0x380);
    assert_eq!(local::TIMER_CURRENT_COUNT, 0x390);
    assert_eq!(local::TIMER_DIVIDE, 0x3E0);
}

#[test]
fn spurious_register_matches_hand_worked_values() {
    // SDM vol. 3A, "Spurious Interrupt": vector in bits 7:0, APIC software
    // enable in bit 8.
    // Enabled, vector 0xFF: 1 << 8 | 0xFF = 0x100 | 0xFF = 0x1FF.
    let on = Spurious {
        vector: Vector::SPURIOUS,
        enabled: true,
    };
    assert_eq!(on.encode(), 0x0000_01FF);
    // Disabled, vector 0x2F: just the vector.
    let off = Spurious {
        vector: vector(0x2F),
        enabled: false,
    };
    assert_eq!(off.encode(), 0x0000_002F);
    // The value after reset, 0xFF: disabled, vector 0xFF.
    assert_eq!(
        Spurious::decode(0x0000_00FF),
        Some(Spurious {
            vector: Vector::SPURIOUS,
            enabled: false
        })
    );
}

#[test]
fn spurious_register_round_trips_at_each_extreme() {
    for vector in vector_extremes() {
        for enabled in [false, true] {
            let value = Spurious { vector, enabled };
            assert_eq!(Spurious::decode(value.encode()), Some(value));
        }
    }
}

#[test]
fn other_spurious_values_are_not_read() {
    for value in [
        // Vector 0x0F, an exception's.
        0x0000_010F,
        // Focus processor checking off (bit 9).
        0x0000_02FF,
        // EOI-broadcast suppression (bit 12).
        0x0000_10FF,
        // A reserved bit at the top.
        0x8000_01FF,
    ] {
        assert_eq!(Spurious::decode(value), None, "{value:#x}");
    }
}

#[test]
fn timer_entry_matches_hand_worked_values() {
    // SDM vol. 3A, "Local Vector Table": vector in bits 7:0, delivery status
    // in 12, mask in 16, timer mode in 18:17 (00 one-shot, 01 periodic).
    // Vector 0x20, periodic, not masked: 1 << 17 = 0x2_0000, | 0x20.
    let periodic = Timer {
        vector: vector(0x20),
        mode: TimerMode::Periodic,
        masked: false,
    };
    assert_eq!(periodic.encode(), 0x0002_0020);
    // Vector 0x30, one-shot, masked: 1 << 16 = 0x1_0000, | 0x30.
    let masked = Timer {
        vector: vector(0x30),
        mode: TimerMode::OneShot,
        masked: true,
    };
    assert_eq!(masked.encode(), 0x0001_0030);
    // Read back with delivery status set (0x1000): the same entry.
    assert_eq!(Timer::decode(0x0002_1020), Some(periodic));
}

#[test]
fn timer_entry_round_trips_at_each_extreme() {
    for vector in vector_extremes() {
        for mode in [TimerMode::OneShot, TimerMode::Periodic] {
            for masked in [false, true] {
                let entry = Timer {
                    vector,
                    mode,
                    masked,
                };
                assert_eq!(Timer::decode(entry.encode()), Some(entry));
            }
        }
    }
}

#[test]
fn other_timer_entries_are_not_read() {
    for value in [
        // TSC-deadline mode (18:17 = 10), not supported here.
        0x0004_0020,
        // Mode 11, reserved.
        0x0006_0020,
        // Vector 0x10, below 32.
        0x0000_0010,
        // The reset value: masked, vector 0.
        0x0001_0000,
        // A delivery mode (bits 10:8), which the timer entry does not have.
        0x0000_0420,
        // A reserved bit at the top.
        0x8000_0020,
    ] {
        assert_eq!(Timer::decode(value), None, "{value:#x}");
    }
}

#[test]
fn lint_entries_match_hand_worked_values() {
    // SDM vol. 3A, "Local Vector Table": vector 7:0, delivery mode 10:8
    // (000 fixed, 100 NMI, 111 ExtINT), delivery status 12, polarity 13
    // (1 active low), remote IRR 14, trigger 15 (1 level), mask 16.
    // NMI, active high, edge, not masked: 0b100 << 8 = 0x400.
    let nmi = Lint {
        delivery: LintDelivery::Nmi,
        polarity: PinPolarity::ActiveHigh,
        trigger: TriggerMode::Edge,
        masked: false,
    };
    assert_eq!(nmi.encode(), 0x0000_0400);
    // ExtINT, masked: 0b111 << 8 = 0x700, | 1 << 16 = 0x1_0700.
    let external = Lint {
        delivery: LintDelivery::ExtInt,
        polarity: PinPolarity::ActiveHigh,
        trigger: TriggerMode::Edge,
        masked: true,
    };
    assert_eq!(external.encode(), 0x0001_0700);
    // Fixed vector 0x41, active low, level: 0x41 | 1 << 13 (0x2000) |
    // 1 << 15 (0x8000) = 0xA041.
    let fixed = Lint {
        delivery: LintDelivery::Fixed(vector(0x41)),
        polarity: PinPolarity::ActiveLow,
        trigger: TriggerMode::Level,
        masked: false,
    };
    assert_eq!(fixed.encode(), 0x0000_A041);
    // Read back with delivery status and remote IRR set (0x1000 | 0x4000):
    // the same entry.
    assert_eq!(Lint::decode(0x0000_F041), Some(fixed));
}

#[test]
fn lint_entries_round_trip_at_each_extreme() {
    let mut deliveries = vec![LintDelivery::Nmi, LintDelivery::ExtInt];
    deliveries.extend(vector_extremes().map(LintDelivery::Fixed));
    for delivery in deliveries {
        for polarity in POLARITIES {
            for trigger in TRIGGERS {
                for masked in [false, true] {
                    let entry = Lint {
                        delivery,
                        polarity,
                        trigger,
                        masked,
                    };
                    assert_eq!(Lint::decode(entry.encode()), Some(entry), "{entry:?}");
                }
            }
        }
    }
}

#[test]
fn other_lint_entries_are_not_read() {
    for value in [
        // SMI (010), INIT (101), start-up (110) and 011, 001: not built here.
        0x0000_0200,
        0x0000_0500,
        0x0000_0600,
        0x0000_0300,
        0x0000_0100,
        // NMI with a vector, which the encoder leaves zero.
        0x0000_0402,
        // ExtINT with a vector.
        0x0000_0720,
        // Fixed with vector 0x1F.
        0x0000_001F,
        // Bit 11, reserved.
        0x0000_0C00,
        // A timer mode bit.
        0x0002_0400,
    ] {
        assert_eq!(Lint::decode(value), None, "{value:#x}");
    }
}

#[test]
fn the_nmi_qemu_describes_gives_the_usual_lint1_entry() {
    // QEMU's MADT has one local APIC NMI entry: every processor, LINT1,
    // flags conforming (see `reads_the_tables_qemu_generates` in
    // `acpi/src/tests.rs`). Conforming is active high for ISA, NMI is edge:
    // 0b100 << 8 = 0x400 at offset 0x360.
    let conforming = flags(Polarity::Conforming, Trigger::Conforming);
    let (offset, entry) = Lint::from_acpi_nmi(1, conforming).unwrap();
    assert_eq!(offset, 0x360);
    assert_eq!(entry.encode(), 0x0000_0400);
    // LINT0, active low: 0x400 | 1 << 13 = 0x2400 at 0x350. A level trigger
    // is not used: the processor treats NMI as edge-triggered whatever the
    // bit says, and the manual says to leave it clear.
    let low = flags(Polarity::ActiveLow, Trigger::Level);
    let (offset, entry) = Lint::from_acpi_nmi(0, low).unwrap();
    assert_eq!(offset, 0x350);
    assert_eq!(entry.encode(), 0x0000_2400);
    // Edge and active high said outright: the same as conforming.
    let high = flags(Polarity::ActiveHigh, Trigger::Edge);
    assert_eq!(Lint::from_acpi_nmi(1, high).unwrap().1.encode(), 0x400);
}

#[test]
fn refuses_an_nmi_on_a_lint_that_does_not_exist() {
    let conforming = flags(Polarity::Conforming, Trigger::Conforming);
    for lint in [2, 3, 0xFF] {
        assert_eq!(
            Lint::from_acpi_nmi(lint, conforming),
            Err(Error::NoSuchLint),
            "{lint}"
        );
    }
}

#[test]
fn refuses_reserved_acpi_flags_for_an_nmi() {
    for reserved in [
        flags(Polarity::Reserved, Trigger::Conforming),
        flags(Polarity::ActiveHigh, Trigger::Reserved),
    ] {
        assert_eq!(
            Lint::from_acpi_nmi(1, reserved),
            Err(Error::ReservedFlags),
            "{reserved:?}"
        );
    }
}

#[test]
fn error_entry_matches_hand_worked_values() {
    // SDM vol. 3A, "Local Vector Table": the error entry has a vector, the
    // delivery status and the mask.
    // Vector 0xFE, not masked: 0xFE.
    let entry = ErrorEntry {
        vector: vector(0xFE),
        masked: false,
    };
    assert_eq!(entry.encode(), 0x0000_00FE);
    // Masked: 1 << 16 | 0xFE = 0x1_00FE.
    let masked = ErrorEntry {
        vector: vector(0xFE),
        masked: true,
    };
    assert_eq!(masked.encode(), 0x0001_00FE);
    // Delivery status set: the same entry.
    assert_eq!(ErrorEntry::decode(0x0001_10FE), Some(masked));
}

#[test]
fn error_entry_round_trips_at_each_extreme() {
    for vector in vector_extremes() {
        for masked in [false, true] {
            let entry = ErrorEntry { vector, masked };
            assert_eq!(ErrorEntry::decode(entry.encode()), Some(entry));
        }
    }
}

#[test]
fn other_error_entries_are_not_read() {
    // Vector 0x08; a delivery mode (NMI); periodic mode; polarity bit 13.
    for value in [0x0000_0008, 0x0000_04FE, 0x0002_00FE, 0x0000_20FE] {
        assert_eq!(ErrorEntry::decode(value), None, "{value:#x}");
    }
}

#[test]
fn divide_configuration_matches_the_manuals_table() {
    // SDM vol. 3A, "APIC Timer", the divide configuration register: the
    // divisor is coded in bits 3, 1 and 0 (bit 2 reserved):
    // 000 = 2, 001 = 4, 010 = 8, 011 = 16, 100 = 32, 101 = 64, 110 = 128,
    // 111 = 1. Bit 3 of the code goes in bit 3 of the register, bits 1:0 in
    // bits 1:0: code 100 is 0b1000 = 0x8, code 111 is 0b1011 = 0xB.
    let table = [
        (1, 0xB),
        (2, 0x0),
        (4, 0x1),
        (8, 0x2),
        (16, 0x3),
        (32, 0x8),
        (64, 0x9),
        (128, 0xA),
    ];
    for (divisor, value) in table {
        let divide = Divide::new(divisor).unwrap();
        assert_eq!(divide.divisor(), divisor);
        assert_eq!(divide.encode(), value, "divide by {divisor}");
        assert_eq!(Divide::decode(value), Some(divide), "{value:#x}");
    }
}

#[test]
fn other_divide_values_are_not_read() {
    // Bit 2 set; bits above 3.
    for value in [0x4, 0xF, 0x10, 0x1B, u32::MAX] {
        assert_eq!(Divide::decode(value), None, "{value:#x}");
    }
}

#[test]
fn refuses_a_divisor_that_is_not_one_of_the_eight() {
    for divisor in [0, 3, 5, 6, 12, 256, 512, u32::MAX] {
        assert_eq!(Divide::new(divisor), Err(Error::Divisor), "{divisor}");
    }
}

#[test]
fn commands_match_hand_worked_values() {
    // SDM vol. 3A, "Interrupt Command Register (ICR)": low half: vector
    // 7:0, delivery mode 10:8 (000 fixed, 101 INIT, 110 start-up),
    // destination mode 11 (0 physical), delivery status 12, level 14 (1
    // assert, required for all but INIT de-assert), trigger 15 (0 edge),
    // shorthand 19:18 (00 none). High half: destination in bits 31:24.
    // Fixed vector 0x40 to local APIC 3: 1 << 14 = 0x4000, | 0x40; 3 << 24.
    let fixed = Command::Fixed {
        vector: vector(0x40),
        destination: 3,
    };
    assert_eq!(fixed.encode(), [0x0000_4040, 0x0300_0000]);
    // INIT to local APIC 1: 0x4000 | 0b101 << 8 (0x500) = 0x4500.
    let init = Command::Init { destination: 1 };
    assert_eq!(init.encode(), [0x0000_4500, 0x0100_0000]);
    // Start-up at page 8 (address 0x8000) to local APIC 1: 0x4000 |
    // 0b110 << 8 (0x600) | 0x08 = 0x4608.
    let start = Command::start_up(8, 1).unwrap();
    assert_eq!(start.encode(), [0x0000_4608, 0x0100_0000]);
    // The last page and the last destination: 0x46FF, 0xFF << 24.
    let last = Command::start_up(0xFF, 0xFF).unwrap();
    assert_eq!(last.encode(), [0x0000_46FF, 0xFF00_0000]);
    // Read back while it is still being sent (0x1000): the same command.
    assert_eq!(Command::decode([0x0000_5040, 0x0300_0000]), Some(fixed));
    assert!(delivery_pending(0x0000_5040));
}

#[test]
fn commands_round_trip_at_each_extreme() {
    for destination in [0, 1, 0xFE, 0xFF] {
        let mut commands = vec![
            Command::Init { destination },
            Command::start_up(0, destination).unwrap(),
            Command::start_up(0xFF, destination).unwrap(),
        ];
        for vector in vector_extremes() {
            commands.push(Command::Fixed {
                vector,
                destination,
            });
        }
        for command in commands {
            assert_eq!(Command::decode(command.encode()), Some(command));
        }
    }
}

#[test]
fn other_commands_are_not_read() {
    let high = 0x0100_0000;
    for (low, high) in [
        // INIT level de-assert: level 0, trigger level.
        (0x0000_8500, high),
        // A broadcast to all but self (shorthand 11).
        (0x000C_4500, high),
        // Logical destination mode.
        (0x0000_4840, high),
        // Level clear on a fixed interrupt.
        (0x0000_0040, high),
        // Lowest priority (001), NMI (100), SMI (010).
        (0x0000_4140, high),
        (0x0000_4400, high),
        (0x0000_4200, high),
        // INIT with a vector.
        (0x0000_4520, high),
        // Fixed with vector 0x10.
        (0x0000_4010, high),
        // A reserved bit in the high half.
        (0x0000_4500, 0x0100_0001),
    ] {
        assert_eq!(Command::decode([low, high]), None, "{low:#x} {high:#x}");
    }
}

#[test]
fn refuses_a_start_up_page_that_does_not_fit() {
    // The page goes in the 8-bit vector field: 0 to 0xFF, below 1 MiB.
    for page in [0x100, 0x1000, u32::MAX] {
        assert_eq!(
            Command::start_up(page, 0),
            Err(Error::StartUpPage),
            "{page:#x}"
        );
    }
}

#[test]
fn local_version_fields_are_read() {
    // SDM vol. 3A, "Local APIC Version Register": version in bits 7:0,
    // maximum LVT entry (entries minus one) in bits 23:16.
    // 0x0005_0014: version 0x14, maximum entry 5 (six entries).
    let read = LocalVersion::read(0x0005_0014);
    assert_eq!((read.version, read.max_lvt_entry), (0x14, 5));
    // Bit 24 (EOI-broadcast suppression supported) and the rest are not
    // fields here.
    let read = LocalVersion::read(0xFFFF_FFFF);
    assert_eq!((read.version, read.max_lvt_entry), (0xFF, 0xFF));
    let read = LocalVersion::read(0);
    assert_eq!((read.version, read.max_lvt_entry), (0, 0));
}

// --- I/O APIC ---

#[test]
fn io_registers_match_the_data_sheet() {
    // 82093AA data sheet, "Register Description": IOREGSEL at offset 0x00,
    // IOWIN at 0x10; IOAPICID 0x00, IOAPICVER 0x01, IOAPICARB 0x02,
    // IOREDTBL from 0x10.
    assert_eq!(io::SELECT, 0x00);
    assert_eq!(io::WINDOW, 0x10);
    assert_eq!(io::ID, 0x00);
    assert_eq!(io::VERSION, 0x01);
    assert_eq!(io::ARBITRATION, 0x02);
}

#[test]
fn redirection_indices_are_0x10_plus_twice_the_input() {
    // The 82093AA's 24 inputs: maximum entry 0x17.
    // Input 0: 0x10. Input 2: 0x10 + 4 = 0x14. Input 23: 0x10 + 46 = 0x3E.
    assert_eq!(io::redirection_index(0, 0x17), Ok(0x10));
    assert_eq!(io::redirection_index(2, 0x17), Ok(0x14));
    assert_eq!(io::redirection_index(23, 0x17), Ok(0x3E));
    // The last input whose pair fits in 8-bit register indices: 0x10 +
    // 2 * 119 = 0xFE, and 0xFF for its high half.
    assert_eq!(io::redirection_index(119, 0xFF), Ok(0xFE));
}

#[test]
fn refuses_an_input_beyond_the_redirection_table() {
    for (input, max_entry) in [(24, 0x17), (1, 0), (120, 0xFF), (u32::MAX, 0xFF)] {
        assert_eq!(
            io::redirection_index(input, max_entry),
            Err(Error::InputBeyondTable),
            "{input} {max_entry}"
        );
    }
}

#[test]
fn io_version_fields_are_read() {
    // 82093AA data sheet, "IOAPICVER": version in bits 7:0 (0x11 for the
    // 82093AA), maximum redirection entry in bits 23:16 (0x17, 24 inputs).
    let read = IoVersion::read(0x0017_0011);
    assert_eq!((read.version, read.max_redirection_entry), (0x11, 0x17));
    let read = IoVersion::read(0xFFFF_FFFF);
    assert_eq!((read.version, read.max_redirection_entry), (0xFF, 0xFF));
}

#[test]
fn redirection_entries_match_hand_worked_values() {
    // 82093AA data sheet, "IOREDTBL": vector 7:0, delivery mode 10:8 (000
    // fixed, 001 lowest priority), destination mode 11 (1 logical),
    // delivery status 12, polarity 13 (1 low active), remote IRR 14, trigger
    // 15 (1 level), mask 16, destination 63:56 (bits 31:24 of the high
    // half).
    // Every field at its top: 0xFE | 1 << 8 (0x100) | 1 << 11 (0x800) |
    // 1 << 13 (0x2000) | 1 << 15 (0x8000) | 1 << 16 (0x1_0000) = 0x1_A9FE;
    // destination 0xFF << 24.
    let full = Redirection {
        vector: vector(0xFE),
        delivery: IoDelivery::LowestPriority,
        destination_mode: DestinationMode::Logical,
        polarity: PinPolarity::ActiveLow,
        trigger: TriggerMode::Level,
        masked: true,
        destination: 0xFF,
    };
    assert_eq!(full.encode(), [0x0001_A9FE, 0xFF00_0000]);
    // Fixed, physical, active high, level, not masked, vector 0x31 to local
    // APIC 2: 0x31 | 1 << 15 (0x8000) = 0x8031; 2 << 24.
    let plain = Redirection {
        vector: vector(0x31),
        delivery: IoDelivery::Fixed,
        destination_mode: DestinationMode::Physical,
        polarity: PinPolarity::ActiveHigh,
        trigger: TriggerMode::Level,
        masked: false,
        destination: 2,
    };
    assert_eq!(plain.encode(), [0x0000_8031, 0x0200_0000]);
    // Read back with delivery status and remote IRR set (0x5000): the same.
    assert_eq!(Redirection::decode([0x0000_D031, 0x0200_0000]), Some(plain));
    // The masked default: only the mask, 1 << 16.
    assert_eq!(Redirection::MASKED, [0x0001_0000, 0]);
}

#[test]
fn redirection_entries_round_trip_at_each_extreme() {
    for vector in vector_extremes() {
        for delivery in [IoDelivery::Fixed, IoDelivery::LowestPriority] {
            for destination_mode in [DestinationMode::Physical, DestinationMode::Logical] {
                for polarity in POLARITIES {
                    for trigger in TRIGGERS {
                        for masked in [false, true] {
                            for destination in [0, 0x0F, 0xFF] {
                                let entry = Redirection {
                                    vector,
                                    delivery,
                                    destination_mode,
                                    polarity,
                                    trigger,
                                    masked,
                                    destination,
                                };
                                assert_eq!(
                                    Redirection::decode(entry.encode()),
                                    Some(entry),
                                    "{entry:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn other_redirection_entries_are_not_read() {
    for (low, high) in [
        // The masked default: vector 0.
        (Redirection::MASKED[0], Redirection::MASKED[1]),
        // SMI (010), NMI (100), INIT (101), ExtINT (111), 011, 110.
        (0x0000_0231, 0),
        (0x0000_0431, 0),
        (0x0000_0531, 0),
        (0x0000_0731, 0),
        (0x0000_0331, 0),
        (0x0000_0631, 0),
        // Vector 0x1F.
        (0x0000_001F, 0),
        // Bit 17, reserved.
        (0x0002_0031, 0),
        // Bits 55:32 of the entry, reserved.
        (0x0000_0031, 0x0000_0001),
        (0x0000_0031, 0x00FF_FFFF),
    ] {
        assert_eq!(Redirection::decode([low, high]), None, "{low:#x} {high:#x}");
    }
}

#[test]
fn qemus_overrides_give_hand_worked_entries() {
    // QEMU's MADT (see `reads_the_tables_qemu_generates` in
    // `acpi/src/tests.rs`): ISA line 0 goes to input 2 with conforming
    // flags; lines 5, 9, 10 and 11 are active high and level-triggered.
    // ISA conforms to edge and active high, so line 0 (the timer), given
    // vector 0x20 and local APIC 0: 0x20, nothing else; high 0.
    let conforming = flags(Polarity::Conforming, Trigger::Conforming);
    let timer = Redirection::from_acpi(vector(0x20), 0, conforming).unwrap();
    assert_eq!(timer.encode(), [0x0000_0020, 0]);
    // Lines 5, 9, 10 and 11 at vectors 0x25, 0x29, 0x2A and 0x2B: level is
    // 1 << 15 = 0x8000, active high is 0; destination local APIC 0.
    let level_high = flags(Polarity::ActiveHigh, Trigger::Level);
    for (number, expected) in [
        (0x25, 0x0000_8025),
        (0x29, 0x0000_8029),
        (0x2A, 0x0000_802A),
        (0x2B, 0x0000_802B),
    ] {
        let entry = Redirection::from_acpi(vector(number), 0, level_high).unwrap();
        assert_eq!(entry.encode(), [expected, 0], "{number:#x}");
    }
    // Lines with no override conform too, like line 0: line 1 (keyboard),
    // vector 0x21, to local APIC 1: 0x21; 1 << 24.
    let keyboard = Redirection::from_acpi(vector(0x21), 1, conforming).unwrap();
    assert_eq!(keyboard.encode(), [0x0000_0021, 0x0100_0000]);
    // Active low and edge said outright: 0x21 | 1 << 13 = 0x2021.
    let low_edge = flags(Polarity::ActiveLow, Trigger::Edge);
    let entry = Redirection::from_acpi(vector(0x21), 0, low_edge).unwrap();
    assert_eq!(entry.encode(), [0x0000_2021, 0]);
}

#[test]
fn refuses_reserved_acpi_flags_for_an_entry() {
    for reserved in [
        flags(Polarity::Reserved, Trigger::Edge),
        flags(Polarity::Conforming, Trigger::Reserved),
    ] {
        assert_eq!(
            Redirection::from_acpi(vector(0x20), 0, reserved),
            Err(Error::ReservedFlags),
            "{reserved:?}"
        );
    }
}

// --- The legacy 8259A pair ---

#[test]
fn legacy_setup_matches_hand_worked_writes() {
    use legacy::Word::*;
    // 8259A data sheet, "Initialization Command Words" and "Operation
    // Command Words". Ports: primary command 0x20 (A0 = 0) and data 0x21
    // (A0 = 1), secondary 0xA0 and 0xA1.
    // ICW1 = 0x11: bit 4 marks ICW1, bit 0 (IC4) says ICW4 follows; bit 1
    // (SNGL) 0 for a cascade, bit 3 (LTIM) 0 for edge triggering.
    // ICW2 = the vector base: 0x20 for the primary, 0x28 for the secondary.
    // ICW3 = 0x04 on the primary (bit 2: a secondary on line 2), 0x02 on the
    // secondary (its identity, 2).
    // ICW4 = 0x01: bit 0 (uPM) for 8086 mode; no automatic end of interrupt,
    // not buffered, not special fully nested.
    // OCW1 = 0xFF: every line masked.
    let expected = [
        (0x20, 0x11, Icw1),
        (0xA0, 0x11, Icw1),
        (0x21, 0x20, Icw2),
        (0xA1, 0x28, Icw2),
        (0x21, 0x04, Icw3),
        (0xA1, 0x02, Icw3),
        (0x21, 0x01, Icw4),
        (0xA1, 0x01, Icw4),
        (0x21, 0xFF, Ocw1),
        (0xA1, 0xFF, Ocw1),
    ];
    let steps = legacy::setup(0x20).unwrap();
    let got = steps.map(|step| (step.port, step.value, step.word));
    assert_eq!(got, expected);
    assert_eq!(legacy::base_of(&steps), Some(0x20));
    // The highest base that leaves room for the secondary: 0xF0, which
    // gives 0xF0 to 0xFF; ICW2 0xF0 and 0xF8.
    let top = legacy::setup(0xF0).unwrap();
    assert_eq!((top[2].value, top[3].value), (0xF0, 0xF8));
    assert_eq!(legacy::base_of(&top), Some(0xF0));
}

#[test]
fn legacy_setup_round_trips_for_every_base() {
    for base in (32..=0xF0).step_by(8) {
        let steps = legacy::setup(base).unwrap();
        assert_eq!(legacy::base_of(&steps), Some(base), "{base:#x}");
    }
}

#[test]
fn other_legacy_sequences_are_not_read() {
    let good = legacy::setup(0x40).unwrap();
    let mut changed = good;
    changed[3].value = 0x40;
    let mut port = good;
    port[0].port = 0x21;
    let mut word = good;
    word[9].word = legacy::Word::Icw4;
    let mut low = good;
    low[2].value = 0x08;
    low[3].value = 0x10;
    for steps in [changed, port, word, low] {
        assert_eq!(legacy::base_of(&steps), None);
    }
}

#[test]
fn refuses_a_legacy_base_that_is_unaligned_low_or_too_high() {
    // Not a multiple of 8; below 32; 0xF8 would put the secondary at 0x100.
    for base in [0x21, 0x27, 0x00, 0x08, 0x18, 0xF8, 0xFF] {
        assert_eq!(legacy::setup(base), Err(Error::LegacyBase), "{base:#x}");
    }
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

    fn vector(&mut self) -> Vector {
        match 32 + self.next() % 224 {
            255 => Vector::SPURIOUS,
            number => vector(number as u8),
        }
    }

    fn polarity(&mut self) -> PinPolarity {
        POLARITIES[usize::from(self.flag())]
    }

    fn trigger(&mut self) -> TriggerMode {
        TRIGGERS[usize::from(self.flag())]
    }
}

#[test]
fn random_values_round_trip_and_random_bits_do_not_panic() {
    let mut random = Random(0x0A91_C5EE_D000_0001);
    let mut recognised = [0u32; 7];
    for _ in 0..20_000 {
        // Random valid values come back as they went in.
        let spurious = Spurious {
            vector: random.vector(),
            enabled: random.flag(),
        };
        assert_eq!(Spurious::decode(spurious.encode()), Some(spurious));
        let mode = [TimerMode::OneShot, TimerMode::Periodic][usize::from(random.flag())];
        let timer = Timer {
            vector: random.vector(),
            mode,
            masked: random.flag(),
        };
        assert_eq!(Timer::decode(timer.encode()), Some(timer));
        let delivery = match random.next() % 3 {
            0 => LintDelivery::Nmi,
            1 => LintDelivery::ExtInt,
            _ => LintDelivery::Fixed(random.vector()),
        };
        let lint = Lint {
            delivery,
            polarity: random.polarity(),
            trigger: random.trigger(),
            masked: random.flag(),
        };
        assert_eq!(Lint::decode(lint.encode()), Some(lint));
        let error = ErrorEntry {
            vector: random.vector(),
            masked: random.flag(),
        };
        assert_eq!(ErrorEntry::decode(error.encode()), Some(error));
        let divide = Divide::new(1 << (random.next() % 8)).unwrap();
        assert_eq!(Divide::decode(divide.encode()), Some(divide));
        let destination = random.next() as u8;
        let command = match random.next() % 3 {
            0 => Command::Init { destination },
            1 => Command::start_up(random.bits() & 0xFF, destination).unwrap(),
            _ => Command::Fixed {
                vector: random.vector(),
                destination,
            },
        };
        assert_eq!(Command::decode(command.encode()), Some(command));
        let entry = Redirection {
            vector: random.vector(),
            delivery: [IoDelivery::Fixed, IoDelivery::LowestPriority][usize::from(random.flag())],
            destination_mode: [DestinationMode::Physical, DestinationMode::Logical]
                [usize::from(random.flag())],
            polarity: random.polarity(),
            trigger: random.trigger(),
            masked: random.flag(),
            destination: random.next() as u8,
        };
        assert_eq!(Redirection::decode(entry.encode()), Some(entry));
        // The 27 bases from 32 to 0xF0.
        let base = 32 + 8 * (random.next() % 27) as u8;
        let steps = legacy::setup(base).unwrap();
        assert_eq!(legacy::base_of(&steps), Some(base));

        // Random bits give no panic, and whatever a decoder accepts encodes
        // back to the same bits less the status bits it ignores. Each is fed
        // fully random bits and nearly-right ones, with the bits no encoder
        // sets cleared, so the decoders accept some.
        let (wild, other) = (random.bits(), random.bits());
        for value in [wild, wild & 0x0000_01FF] {
            if let Some(read) = Spurious::decode(value) {
                assert_eq!(read.encode(), value);
                recognised[0] += 1;
            }
        }
        for value in [wild, wild & 0x0003_10FF] {
            if let Some(read) = Timer::decode(value) {
                assert_eq!(read.encode(), value & !0x1000);
                recognised[1] += 1;
            }
        }
        for value in [wild, wild & 0x0001_F7FF] {
            if let Some(read) = Lint::decode(value) {
                assert_eq!(read.encode(), value & !0x5000);
                recognised[2] += 1;
            }
        }
        for value in [wild, wild & 0x0001_10FF] {
            if let Some(read) = ErrorEntry::decode(value) {
                assert_eq!(read.encode(), value & !0x1000);
                recognised[3] += 1;
            }
        }
        for value in [wild, wild & 0xB] {
            if let Some(read) = Divide::decode(value) {
                assert_eq!(read.encode(), value);
            }
        }
        // Clears bits 31:15, 13 and 11 and sets the level bit, 14.
        let near = (wild & 0x0000_17FF) | 0x4000;
        for halves in [[wild, other], [near, other & 0xFF00_0000]] {
            if let Some(read) = Command::decode(halves) {
                assert_eq!(read.encode(), [halves[0] & !0x1000, halves[1]]);
                recognised[4] += 1;
            }
        }
        for halves in [[wild, other], [wild & 0x0001_F9FF, other & 0xFF00_0000]] {
            if let Some(read) = Redirection::decode(halves) {
                assert_eq!(read.encode(), [halves[0] & !0x5000, halves[1]]);
                recognised[5] += 1;
            }
        }
        let _ = LocalVersion::read(wild);
        let _ = IoVersion::read(wild);
        let _ = delivery_pending(wild);
        let _ = remote_irr(wild);
        let _ = io::redirection_index(wild, other as u8);
        let _ = Command::start_up(wild, other as u8);
        let _ = Divide::new(wild);
        let _ = Vector::new(wild as u8);
        let _ = legacy::setup(wild as u8);
        let mut steps = legacy::setup(0x20).unwrap();
        let at = (other % 10) as usize;
        steps[at].value = wild as u8;
        if legacy::base_of(&steps).is_some() {
            recognised[6] += 1;
        }
    }
    // Each decoder accepted a good share, so the checks above ran; the
    // legacy reader accepted only the few sequences a random byte left
    // unchanged or turned into another valid base.
    assert!(
        recognised[..6].iter().all(|&count| count > 1000),
        "{recognised:?}"
    );
    assert!(recognised[6] < 2_000, "{recognised:?}");
}

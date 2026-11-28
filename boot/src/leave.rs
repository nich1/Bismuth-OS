//! Leaving the firmware: the order of the calls that read the memory map
//! and end boot services, and the count of what the map holds.
//!
//! This file has no firmware or hardware code: the calls are made through
//! the [`Firmware`] trait, which `uefi/boot_services.rs` implements with the
//! real ones. So `xtask` also compiles it on the development machine (with
//! `#[path]`) and `cargo test --package xtask` runs the tests at the bottom,
//! with stand-ins that fail in each way the specification allows. QEMU's
//! firmware never fails here, so those tests are the only guard.
//!
//! UEFI specification 2.10: section 7.2, `GetMemoryMap()` and
//! `EFI_MEMORY_DESCRIPTOR`; section 7.4, `ExitBootServices()`; the status
//! values are in appendix D.

/// A firmware status code (section 2.3.1, `EFI_STATUS`).
pub type Status = usize;

/// `EFI_SUCCESS`, appendix D.
pub const SUCCESS: Status = 0;
/// `EFI_INVALID_PARAMETER`, appendix D: from `ExitBootServices()`, the map
/// key is not the current one, because the map changed since it was read.
pub const INVALID_PARAMETER: Status = ERROR | 2;
/// `EFI_BUFFER_TOO_SMALL`, appendix D: from `GetMemoryMap()`, the map does
/// not fit, and the size it needs has been written back.
pub const BUFFER_TOO_SMALL: Status = ERROR | 5;
/// The bit that marks a status as an error on a 64-bit machine (appendix D).
const ERROR: Status = 1 << 63;

/// The size of `EFI_MEMORY_DESCRIPTOR` as section 7.2 lays it out: the
/// type (4 bytes, then 4 of padding to align what follows), the physical
/// start, the virtual start, the number of pages and the attributes (8
/// each). The firmware may use a larger stride, which it states, so that
/// it can add fields; never a smaller one.
pub const DESCRIPTOR_SIZE: usize = 40;
/// Where the type, the physical start, the number of 4 KiB pages and the
/// attributes are in a descriptor.
const AT_TYPE: usize = 0;
const AT_START: usize = 8;
const AT_PAGES: usize = 24;
const AT_ATTRIBUTE: usize = 32;

/// `EFI_MEMORY_SP`, an attribute bit of a descriptor (section 7.2,
/// `GetMemoryMap()`, "Memory Attribute Definitions"): the memory is set
/// aside for a specific purpose, such as a device's or an application's,
/// and is not for general use even when its type is conventional memory.
pub const MEMORY_SP: u64 = 0x0000_0000_0004_0000;
/// `EfiReservedMemoryType` (section 7.2): what a descriptor marked
/// [`MEMORY_SP`] is treated as, so it is never counted or handed on as
/// usable.
pub const RESERVED_TYPE: u32 = 0;

/// How many descriptors more than the firmware asks for the map's buffer
/// has room for. Allocating the buffer itself changes the map: the pool
/// allocation may take pages from a free range, splitting it into at most
/// three (two descriptors more), and the firmware may allocate for itself
/// between the calls. Eight leaves room for that several times over, at a
/// cost of a few hundred bytes. A map that still outgrows it is an error.
pub const SPARE_DESCRIPTORS: usize = 8;

/// What one `GetMemoryMap()` call returned: its status, and the values it
/// wrote back. On [`BUFFER_TOO_SMALL`], `size` is the size the map needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapRead {
    /// The status.
    pub status: Status,
    /// The size of the map in bytes, or the size it needs.
    pub size: usize,
    /// The key that names this version of the map.
    pub key: usize,
    /// The distance between descriptors, in bytes.
    pub descriptor_size: usize,
    /// The version of the descriptor layout.
    pub version: u32,
}

/// The firmware calls the sequence makes.
pub trait Firmware {
    /// Section 7.2, `GetMemoryMap()`: into the buffer [`Firmware::allocate`]
    /// gave, its whole size offered, or, before there is one, with a size of
    /// zero and no buffer, to learn the size.
    fn memory_map(&mut self) -> MapRead;
    /// Section 7.2, `AllocatePool()`: `size` bytes of loader data, 8-byte
    /// aligned, never freed, kept as the buffer for the map. Returns its
    /// address, or the status, `None` when there is no service.
    fn allocate(&mut self, size: usize) -> Result<u64, Option<Status>>;
    /// Section 7.4, `ExitBootServices()` with this program's image handle
    /// and `key`.
    fn exit(&mut self, key: usize) -> Status;
    /// Section 7.2, `AllocatePages()`: working space for the hand-off
    /// builder, for a map of at most `descriptors` descriptors, as loader
    /// data in whole pages, kept. Returns the status, `None` when there is
    /// no service.
    fn allocate_space(&mut self, descriptors: usize) -> Result<(), Option<Status>>;
}

/// The most descriptors a map read into a buffer of `capacity` bytes can
/// hold: [`read_map`] refuses a stride below [`DESCRIPTOR_SIZE`], whatever
/// the size query said.
pub fn most_descriptors(capacity: usize) -> usize {
    capacity / DESCRIPTOR_SIZE
}

/// The memory map as it was when boot services ended, in memory that
/// stays this program's and is never freed.
// Comparing is for the tests only, where it would count as reading the
// fields.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the descriptor version is kept for a later hand-off of the runtime services"
    )
)]
pub struct Map {
    /// The buffer's physical address, which is also its address while the
    /// firmware's lower mappings are in use.
    pub address: u64,
    /// The size of the map in bytes.
    pub size: usize,
    /// The distance between descriptors, at least [`DESCRIPTOR_SIZE`].
    pub descriptor_size: usize,
    /// The version of the descriptor layout.
    pub version: u32,
}

/// Why boot services could not be ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitFailure {
    /// The firmware has no `GetMemoryMap()` or no `ExitBootServices()`.
    NoService,
    /// Asking for the map's size did not answer [`BUFFER_TOO_SMALL`].
    SizeQuery(Status),
    /// No buffer for the map: the status, `None` when there is no service.
    Allocate(Option<Status>),
    /// No working space for the hand-off builder: the status, `None` when
    /// there is no service.
    Space(Option<Status>),
    /// Reading the map into the buffer failed with this status.
    ReadMap(Status),
    /// The map needs more than the buffer holds.
    MapTooLarge {
        /// The size the map needs, in bytes.
        size: usize,
        /// The buffer's size.
        capacity: usize,
    },
    /// The firmware states a descriptor size below [`DESCRIPTOR_SIZE`].
    DescriptorSize(usize),
    /// `ExitBootServices()` failed with this status.
    Exit(Status),
    /// `ExitBootServices()` found the map key stale again after the map was
    /// read a second time.
    StaleTwice,
}

/// Reads the memory map and ends boot services with its key.
///
/// First the size the map needs, then a buffer of that size with room for
/// [`SPARE_DESCRIPTORS`] more, then the hand-off builder's working space
/// for as many descriptors as that buffer can hold, then the map into the
/// buffer, then `ExitBootServices()`. Both allocations come before the
/// read, since any allocation changes the map. If `ExitBootServices()` says
/// the key is stale, the map changed in between: it is read again into the
/// same buffer, without allocating, and `ExitBootServices()` is tried once
/// more. After a failed `ExitBootServices()` only `GetMemoryMap()` is
/// called, which the specification allows (section 7.4).
pub fn leave(firmware: &mut impl Firmware) -> Result<Map, ExitFailure> {
    let query = firmware.memory_map();
    if query.status != BUFFER_TOO_SMALL {
        return Err(ExitFailure::SizeQuery(query.status));
    }
    // The specification promises only the size with this answer, so a
    // descriptor size left at zero still gets room for the spare
    // descriptors. A size too large to allocate makes the allocation fail.
    let stride = query.descriptor_size.max(DESCRIPTOR_SIZE);
    let capacity = query
        .size
        .saturating_add(SPARE_DESCRIPTORS.saturating_mul(stride));
    let address = firmware.allocate(capacity).map_err(ExitFailure::Allocate)?;
    firmware
        .allocate_space(most_descriptors(capacity))
        .map_err(ExitFailure::Space)?;
    let mut map = read_map(firmware, capacity)?;
    let mut status = firmware.exit(map.key);
    if status == INVALID_PARAMETER {
        map = read_map(firmware, capacity)?;
        status = firmware.exit(map.key);
        if status == INVALID_PARAMETER {
            return Err(ExitFailure::StaleTwice);
        }
    }
    if status != SUCCESS {
        return Err(ExitFailure::Exit(status));
    }
    Ok(Map {
        address,
        size: map.size,
        descriptor_size: map.descriptor_size,
        version: map.version,
    })
}

/// Reads the map into the buffer of `capacity` bytes and checks that it
/// fits and that its stride is at least a whole descriptor.
fn read_map(firmware: &mut impl Firmware, capacity: usize) -> Result<MapRead, ExitFailure> {
    let map = firmware.memory_map();
    if map.status == BUFFER_TOO_SMALL || (map.status == SUCCESS && map.size > capacity) {
        return Err(ExitFailure::MapTooLarge {
            size: map.size,
            capacity,
        });
    }
    if map.status != SUCCESS {
        return Err(ExitFailure::ReadMap(map.status));
    }
    if map.descriptor_size < DESCRIPTOR_SIZE {
        return Err(ExitFailure::DescriptorSize(map.descriptor_size));
    }
    Ok(map)
}

/// One descriptor of the map, as the boot stage hands it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    /// Its `EFI_MEMORY_TYPE`, or [`RESERVED_TYPE`] if it is marked
    /// [`MEMORY_SP`].
    pub firmware_type: u32,
    /// Its physical start.
    pub start: u64,
    /// Its number of 4 KiB pages.
    pub pages: u64,
}

/// The descriptors in `map`, stepping by `descriptor_size`. A stride below
/// [`DESCRIPTOR_SIZE`] gives none, and a trailing piece shorter than a
/// descriptor is not one.
pub fn descriptors(map: &[u8], descriptor_size: usize) -> impl Iterator<Item = Descriptor> + '_ {
    let stride = if descriptor_size < DESCRIPTOR_SIZE {
        usize::MAX
    } else {
        descriptor_size
    };
    map.chunks_exact(stride).map(|descriptor| {
        let field = |at: usize| {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&descriptor[at..at + 8]);
            u64::from_le_bytes(bytes)
        };
        let firmware_type = field(AT_TYPE) as u32;
        let special = field(AT_ATTRIBUTE) & MEMORY_SP != 0;
        Descriptor {
            firmware_type: if special {
                RESERVED_TYPE
            } else {
                firmware_type
            },
            start: field(AT_START),
            pages: field(AT_PAGES),
        }
    })
}

/// The number of descriptors in `map`, stepping by `descriptor_size`, and
/// the total pages of those whose type `usable` accepts, as
/// [`descriptors`] gives them. A stride below [`DESCRIPTOR_SIZE`] counts
/// nothing.
pub fn count(map: &[u8], descriptor_size: usize, usable: impl Fn(u32) -> bool) -> (u64, u64) {
    let mut count: u64 = 0;
    let mut pages: u64 = 0;
    for descriptor in descriptors(map, descriptor_size) {
        count += 1;
        if usable(descriptor.firmware_type) {
            pages = pages.saturating_add(descriptor.pages);
        }
    }
    (count, pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Map { buffer: bool },
        Allocate(usize),
        Space(usize),
        Exit(usize),
    }

    /// A firmware that answers from a script and records every call.
    struct Script {
        maps: Vec<MapRead>,
        allocation: Result<u64, Option<Status>>,
        space: Result<(), Option<Status>>,
        exits: Vec<Status>,
        calls: Vec<Call>,
        buffer: bool,
    }

    impl Firmware for Script {
        fn memory_map(&mut self) -> MapRead {
            self.calls.push(Call::Map {
                buffer: self.buffer,
            });
            assert!(!self.maps.is_empty(), "more map reads than scripted");
            self.maps.remove(0)
        }
        fn allocate(&mut self, size: usize) -> Result<u64, Option<Status>> {
            self.calls.push(Call::Allocate(size));
            self.buffer = self.allocation.is_ok();
            self.allocation
        }
        fn exit(&mut self, key: usize) -> Status {
            self.calls.push(Call::Exit(key));
            assert!(!self.exits.is_empty(), "more exits than scripted");
            self.exits.remove(0)
        }
        fn allocate_space(&mut self, descriptors: usize) -> Result<(), Option<Status>> {
            self.calls.push(Call::Space(descriptors));
            self.space
        }
    }

    const OUT_OF_RESOURCES: Status = ERROR | 9;
    const DEVICE_ERROR: Status = ERROR | 7;
    /// A stride larger than the specification's, as OVMF uses.
    const STRIDE: usize = 48;
    const NEEDED: usize = 10 * STRIDE;
    const CAPACITY: usize = NEEDED + SPARE_DESCRIPTORS * STRIDE;
    const ADDRESS: u64 = 0x7_0000;

    fn read(status: Status, size: usize, key: usize) -> MapRead {
        MapRead {
            status,
            size,
            key,
            descriptor_size: STRIDE,
            version: 1,
        }
    }

    /// The size query, then the map reads and exits given.
    fn script(maps: &[MapRead], exits: &[Status]) -> Script {
        let mut all = vec![read(BUFFER_TOO_SMALL, NEEDED, 0)];
        all.extend_from_slice(maps);
        Script {
            maps: all,
            allocation: Ok(ADDRESS),
            space: Ok(()),
            exits: exits.to_vec(),
            calls: Vec::new(),
            buffer: false,
        }
    }

    fn run(mut script: Script) -> (Result<Map, ExitFailure>, Vec<Call>) {
        let result = leave(&mut script);
        assert!(script.maps.is_empty(), "map reads left: {:?}", script.maps);
        assert!(script.exits.is_empty(), "exits left: {:?}", script.exits);
        (result, script.calls)
    }

    /// The calls up to and including the first read into the buffer: the
    /// working space is for as many descriptors of the specification's 40
    /// bytes as the buffer holds.
    const START: [Call; 4] = [
        Call::Map { buffer: false },
        Call::Allocate(CAPACITY),
        Call::Space(CAPACITY / 40),
        Call::Map { buffer: true },
    ];

    fn calls(rest: &[Call]) -> Vec<Call> {
        let mut all = START.to_vec();
        all.extend_from_slice(rest);
        all
    }

    fn map(size: usize) -> Map {
        Map {
            address: ADDRESS,
            size,
            descriptor_size: STRIDE,
            version: 1,
        }
    }

    #[test]
    fn the_first_key_ends_boot_services() {
        let (result, made) = run(script(&[read(SUCCESS, NEEDED + STRIDE, 7)], &[SUCCESS]));
        assert_eq!(result, Ok(map(NEEDED + STRIDE)));
        assert_eq!(made, calls(&[Call::Exit(7)]));
    }

    #[test]
    fn a_stale_key_reads_the_map_again_into_the_same_buffer() {
        let maps = [read(SUCCESS, NEEDED, 7), read(SUCCESS, NEEDED + STRIDE, 8)];
        let (result, made) = run(script(&maps, &[INVALID_PARAMETER, SUCCESS]));
        // The map as read the second time.
        assert_eq!(result, Ok(map(NEEDED + STRIDE)));
        assert_eq!(
            made,
            calls(&[Call::Exit(7), Call::Map { buffer: true }, Call::Exit(8)])
        );
    }

    #[test]
    fn a_key_stale_twice_is_an_error_and_not_tried_a_third_time() {
        let maps = [read(SUCCESS, NEEDED, 7), read(SUCCESS, NEEDED, 8)];
        let (result, made) = run(script(&maps, &[INVALID_PARAMETER, INVALID_PARAMETER]));
        assert_eq!(result, Err(ExitFailure::StaleTwice));
        assert_eq!(
            made,
            calls(&[Call::Exit(7), Call::Map { buffer: true }, Call::Exit(8)])
        );
    }

    #[test]
    fn a_map_that_outgrows_the_buffer_on_the_retry_is_an_error() {
        let grown = CAPACITY + STRIDE;
        let maps = [read(SUCCESS, NEEDED, 7), read(BUFFER_TOO_SMALL, grown, 0)];
        let (result, made) = run(script(&maps, &[INVALID_PARAMETER]));
        assert_eq!(
            result,
            Err(ExitFailure::MapTooLarge {
                size: grown,
                capacity: CAPACITY
            })
        );
        assert_eq!(made, calls(&[Call::Exit(7), Call::Map { buffer: true }]));
    }

    #[test]
    fn a_failed_read_on_the_retry_is_an_error() {
        let maps = [read(SUCCESS, NEEDED, 7), read(DEVICE_ERROR, NEEDED, 0)];
        let (result, made) = run(script(&maps, &[INVALID_PARAMETER]));
        assert_eq!(result, Err(ExitFailure::ReadMap(DEVICE_ERROR)));
        assert_eq!(made, calls(&[Call::Exit(7), Call::Map { buffer: true }]));
    }

    #[test]
    fn another_error_from_exit_is_not_retried() {
        let (result, made) = run(script(&[read(SUCCESS, NEEDED, 7)], &[DEVICE_ERROR]));
        assert_eq!(result, Err(ExitFailure::Exit(DEVICE_ERROR)));
        assert_eq!(made, calls(&[Call::Exit(7)]));
        // Nor after a retry.
        let maps = [read(SUCCESS, NEEDED, 7), read(SUCCESS, NEEDED, 8)];
        let (result, made) = run(script(&maps, &[INVALID_PARAMETER, DEVICE_ERROR]));
        assert_eq!(result, Err(ExitFailure::Exit(DEVICE_ERROR)));
        assert_eq!(
            made,
            calls(&[Call::Exit(7), Call::Map { buffer: true }, Call::Exit(8)])
        );
    }

    #[test]
    fn a_size_query_that_does_not_ask_for_a_buffer_is_an_error() {
        for status in [SUCCESS, INVALID_PARAMETER] {
            let mut script = script(&[], &[]);
            script.maps[0].status = status;
            let (result, made) = run(script);
            assert_eq!(result, Err(ExitFailure::SizeQuery(status)));
            assert_eq!(made, [Call::Map { buffer: false }]);
        }
    }

    #[test]
    fn no_buffer_is_an_error() {
        for refusal in [Some(OUT_OF_RESOURCES), None] {
            let mut script = script(&[], &[]);
            script.allocation = Err(refusal);
            let (result, made) = run(script);
            assert_eq!(result, Err(ExitFailure::Allocate(refusal)));
            assert_eq!(made, START[..2]);
        }
    }

    #[test]
    fn a_failed_first_read_is_an_error_and_does_not_exit() {
        let (result, made) = run(script(&[read(DEVICE_ERROR, 0, 0)], &[]));
        assert_eq!(result, Err(ExitFailure::ReadMap(DEVICE_ERROR)));
        assert_eq!(made, START);
        let grown = CAPACITY + 1;
        let (result, made) = run(script(&[read(BUFFER_TOO_SMALL, grown, 0)], &[]));
        assert_eq!(
            result,
            Err(ExitFailure::MapTooLarge {
                size: grown,
                capacity: CAPACITY
            })
        );
        assert_eq!(made, START);
    }

    #[test]
    fn a_read_claiming_more_than_the_buffer_or_a_short_stride_is_an_error() {
        let (result, made) = run(script(&[read(SUCCESS, CAPACITY + 1, 7)], &[]));
        assert_eq!(
            result,
            Err(ExitFailure::MapTooLarge {
                size: CAPACITY + 1,
                capacity: CAPACITY
            })
        );
        assert_eq!(made, START);
        let mut short = read(SUCCESS, NEEDED, 7);
        short.descriptor_size = DESCRIPTOR_SIZE - 8;
        let (result, made) = run(script(&[short], &[]));
        assert_eq!(
            result,
            Err(ExitFailure::DescriptorSize(DESCRIPTOR_SIZE - 8))
        );
        assert_eq!(made, START);
        // The specification's own size is accepted.
        let mut exact = read(SUCCESS, NEEDED, 7);
        exact.descriptor_size = DESCRIPTOR_SIZE;
        let (result, _) = run(script(&[exact], &[SUCCESS]));
        assert_eq!(result.map(|map| map.descriptor_size), Ok(DESCRIPTOR_SIZE));
    }

    /// A descriptor of `stride` bytes with the attributes `attribute`:
    /// type, start, pages and attributes where section 7.2 puts them (at
    /// 0, 8, 24 and 32, written here as numbers, not the constants under
    /// test), every other byte 0xA5, so a wrong stride or offset reads
    /// something else.
    fn marked(stride: usize, kind: u32, start: u64, pages: u64, attribute: u64) -> Vec<u8> {
        let mut bytes = vec![0xA5; stride];
        bytes[0..4].copy_from_slice(&kind.to_le_bytes());
        bytes[8..16].copy_from_slice(&start.to_le_bytes());
        bytes[24..32].copy_from_slice(&pages.to_le_bytes());
        bytes[32..40].copy_from_slice(&attribute.to_le_bytes());
        bytes
    }

    /// As [`marked`], with no attributes and a start of its own.
    fn descriptor(stride: usize, kind: u32, pages: u64) -> Vec<u8> {
        marked(stride, kind, 0x1000 * pages, pages, 0)
    }

    #[test]
    fn the_descriptors_are_read_at_their_offsets() {
        let mut map = marked(STRIDE, 7, 0x10_0000, 16, 0xF);
        map.extend(marked(STRIDE, 4, 0x20_0000, 3, 0));
        let read: Vec<_> = descriptors(&map, STRIDE).collect();
        assert_eq!(
            read,
            [
                Descriptor {
                    firmware_type: 7,
                    start: 0x10_0000,
                    pages: 16
                },
                Descriptor {
                    firmware_type: 4,
                    start: 0x20_0000,
                    pages: 3
                },
            ]
        );
    }

    #[test]
    fn specific_purpose_memory_is_reserved_and_not_usable() {
        // EFI_MEMORY_SP is bit 18 of the attributes (section 7.2).
        let special = 1 << 18;
        let mut map = marked(STRIDE, 7, 0x10_0000, 16, special);
        map.extend(marked(STRIDE, 7, 0x20_0000, 5, special | 0xF));
        map.extend(marked(STRIDE, 7, 0x30_0000, 2, !special));
        let types: Vec<_> = descriptors(&map, STRIDE).map(|d| d.firmware_type).collect();
        assert_eq!(types, [0, 0, 7]);
        assert_eq!(count(&map, STRIDE, |kind| kind == 7), (3, 2));
        // Another type marked so stays reserved too.
        let map = marked(STRIDE, 4, 0x10_0000, 1, special);
        assert_eq!(
            descriptors(&map, STRIDE).next().map(|d| d.firmware_type),
            Some(0)
        );
    }

    #[test]
    fn a_size_query_without_a_descriptor_size_still_leaves_room_to_spare() {
        let mut query = read(BUFFER_TOO_SMALL, NEEDED, 0);
        query.descriptor_size = 0;
        let mut script = script(&[read(SUCCESS, NEEDED, 7)], &[SUCCESS]);
        script.maps[0] = query;
        let (result, made) = run(script);
        assert!(result.is_ok(), "{result:?}");
        // Room for eight descriptors of the specification's 40 bytes.
        let capacity = NEEDED + SPARE_DESCRIPTORS * 40;
        assert_eq!(made[1], Call::Allocate(capacity));
        assert_eq!(made[2], Call::Space(capacity / 40));
    }

    #[test]
    fn no_working_space_is_an_error_and_reads_nothing() {
        for refusal in [Some(OUT_OF_RESOURCES), None] {
            let mut script = script(&[], &[]);
            script.space = Err(refusal);
            let (result, made) = run(script);
            assert_eq!(result, Err(ExitFailure::Space(refusal)));
            assert_eq!(made, START[..3]);
        }
    }

    #[test]
    fn the_count_steps_by_the_stated_stride_and_sums_usable_pages() {
        let conventional = |kind| kind == 7;
        let mut map = Vec::new();
        for (kind, pages) in [(7, 100), (4, 30), (7, 5), (11, 1000), (2, 9)] {
            map.extend(descriptor(STRIDE, kind, pages));
        }
        assert_eq!(count(&map, STRIDE, conventional), (5, 105));
        // A trailing piece shorter than a descriptor is not one.
        map.extend([0; STRIDE - 1]);
        assert_eq!(count(&map, STRIDE, conventional), (5, 105));
        // The specification's size is a stride like any other.
        let map: Vec<u8> = [(7, 3), (7, 4)]
            .iter()
            .flat_map(|&(kind, pages)| descriptor(40, kind, pages))
            .collect();
        assert_eq!(count(&map, 40, conventional), (2, 7));
        assert_eq!(count(&[], STRIDE, conventional), (0, 0));
    }

    #[test]
    fn a_stride_below_the_descriptor_size_counts_nothing() {
        let map = descriptor(STRIDE, 7, 100);
        assert_eq!(count(&map, 0, |_| true), (0, 0));
        assert_eq!(count(&map, DESCRIPTOR_SIZE - 1, |_| true), (0, 0));
    }
}

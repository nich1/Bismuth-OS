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
/// Where the type and the number of 4 KiB pages are in a descriptor.
const AT_TYPE: usize = 0;
const AT_PAGES: usize = 24;

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
        reason = "the hand-off to the kernel (issue 22) reads the descriptor version"
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
/// [`SPARE_DESCRIPTORS`] more, then the map into it, then
/// `ExitBootServices()`. If that says the key is stale, the map changed in
/// between: it is read again into the same buffer, without allocating, and
/// `ExitBootServices()` is tried once more. After a failed
/// `ExitBootServices()` only `GetMemoryMap()` is called, which the
/// specification allows (section 7.4).
pub fn leave(firmware: &mut impl Firmware) -> Result<Map, ExitFailure> {
    let query = firmware.memory_map();
    if query.status != BUFFER_TOO_SMALL {
        return Err(ExitFailure::SizeQuery(query.status));
    }
    // A size too large to allocate makes the allocation fail.
    let capacity = query
        .size
        .saturating_add(SPARE_DESCRIPTORS.saturating_mul(query.descriptor_size));
    let address = firmware.allocate(capacity).map_err(ExitFailure::Allocate)?;
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

/// The number of descriptors in `map`, stepping by `descriptor_size`, and
/// the total pages of those whose type `usable` accepts. A stride below
/// [`DESCRIPTOR_SIZE`] counts nothing.
pub fn count(map: &[u8], descriptor_size: usize, usable: impl Fn(u32) -> bool) -> (u64, u64) {
    if descriptor_size < DESCRIPTOR_SIZE {
        return (0, 0);
    }
    let mut descriptors: u64 = 0;
    let mut pages: u64 = 0;
    for descriptor in map.chunks_exact(descriptor_size) {
        descriptors += 1;
        let field = |at: usize, bytes: &mut [u8]| {
            let len = bytes.len();
            bytes.copy_from_slice(&descriptor[at..at + len]);
        };
        let mut kind = [0; 4];
        field(AT_TYPE, &mut kind);
        if usable(u32::from_le_bytes(kind)) {
            let mut count = [0; 8];
            field(AT_PAGES, &mut count);
            pages = pages.saturating_add(u64::from_le_bytes(count));
        }
    }
    (descriptors, pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Map { buffer: bool },
        Allocate(usize),
        Exit(usize),
    }

    /// A firmware that answers from a script and records every call.
    struct Script {
        maps: Vec<MapRead>,
        allocation: Result<u64, Option<Status>>,
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

    /// The calls up to and including the first read into the buffer.
    const START: [Call; 3] = [
        Call::Map { buffer: false },
        Call::Allocate(CAPACITY),
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

    /// A descriptor of `stride` bytes: type and pages where section 7.2
    /// puts them, every other byte 0xA5, so a wrong stride or offset reads
    /// something else.
    fn descriptor(stride: usize, kind: u32, pages: u64) -> Vec<u8> {
        let mut bytes = vec![0xA5; stride];
        bytes[AT_TYPE..AT_TYPE + 4].copy_from_slice(&kind.to_le_bytes());
        bytes[AT_PAGES..AT_PAGES + 8].copy_from_slice(&pages.to_le_bytes());
        bytes
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
            .flat_map(|&(kind, pages)| descriptor(DESCRIPTOR_SIZE, kind, pages))
            .collect();
        assert_eq!(count(&map, DESCRIPTOR_SIZE, conventional), (2, 7));
        assert_eq!(count(&[], STRIDE, conventional), (0, 0));
    }

    #[test]
    fn a_stride_below_the_descriptor_size_counts_nothing() {
        let map = descriptor(STRIDE, 7, 100);
        assert_eq!(count(&map, 0, |_| true), (0, 0));
        assert_eq!(count(&map, DESCRIPTOR_SIZE - 1, |_| true), (0, 0));
    }
}

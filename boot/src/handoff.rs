//! Building the hand-off structure for the kernel, once boot services have
//! ended, from the memory map they left and the ranges the boot stage gave
//! the kernel. `docs/handoff.md` describes the structure.
//!
//! This file has no firmware or hardware code, so `xtask` also compiles it
//! on the development machine (with `#[path]`) and `cargo test --package
//! xtask` runs the tests at the bottom.

use bismuth_elf::place::MAX_SEGMENTS;
use bismuth_handoff::{BuildError, Builder, Region};

use crate::leave;

/// How many ranges the boot stage sets aside for the kernel at most: one
/// per segment, the stack, and the block of table frames.
pub const KERNEL_RANGES: usize = MAX_SEGMENTS + 2;

/// How many regions the builder needs room for, for a map of at most
/// `descriptors` descriptors: one each, and two more for each range set
/// aside, which may split a region of loader memory into three. The builder
/// counts its space before merging neighbours, so nothing is saved by them.
pub fn space_for(descriptors: usize) -> usize {
    descriptors.saturating_add(2 * KERNEL_RANGES)
}

/// Why the structure could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffFailure {
    /// The builder refused the firmware descriptor starting here.
    Descriptor {
        /// Its physical start.
        start: u64,
        /// What the builder returned.
        error: BuildError,
    },
    /// The builder refused to set aside the kernel's range starting here.
    SetAside {
        /// Its physical start.
        start: u64,
        /// What the builder returned.
        error: BuildError,
    },
    /// The builder could not write the structure.
    Write(BuildError),
}

/// Builds the structure into `out` from the firmware's `map`, stepping by
/// `descriptor_size`, with each range of `kernel` (a physical start and a
/// number of pages) set aside as the kernel's, using `space` as the
/// builder's working space. Returns the structure's length.
///
/// The map's descriptors go in as [`leave::descriptors`] reads them, so
/// memory the firmware marks for a specific purpose goes in as reserved.
pub fn build(
    map: &[u8],
    descriptor_size: usize,
    kernel: impl IntoIterator<Item = (u64, u64)>,
    space: &mut [Region],
    out: &mut [u8],
) -> Result<usize, HandoffFailure> {
    let mut builder = Builder::new(space);
    for descriptor in leave::descriptors(map, descriptor_size) {
        builder
            .add_firmware(descriptor.firmware_type, descriptor.start, descriptor.pages)
            .map_err(|error| HandoffFailure::Descriptor {
                start: descriptor.start,
                error,
            })?;
    }
    for (start, pages) in kernel {
        builder
            .set_aside_for_kernel(start, pages)
            .map_err(|error| HandoffFailure::SetAside { start, error })?;
    }
    builder.write(out).map_err(HandoffFailure::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bismuth_handoff::{Handoff, Kind};

    const STRIDE: usize = 48;

    /// A descriptor as section 7.2 lays it out, at the offsets written as
    /// numbers: type at 0, start at 8, pages at 24, attributes at 32.
    fn descriptor(kind: u32, start: u64, pages: u64, attribute: u64) -> Vec<u8> {
        let mut bytes = vec![0; STRIDE];
        bytes[0..4].copy_from_slice(&kind.to_le_bytes());
        bytes[8..16].copy_from_slice(&start.to_le_bytes());
        bytes[24..32].copy_from_slice(&pages.to_le_bytes());
        bytes[32..40].copy_from_slice(&attribute.to_le_bytes());
        bytes
    }

    fn map(descriptors: &[(u32, u64, u64)]) -> Vec<u8> {
        descriptors
            .iter()
            .flat_map(|&(kind, start, pages)| descriptor(kind, start, pages, 0))
            .collect()
    }

    fn regions(out: &[u8]) -> Vec<Region> {
        Handoff::read(out).expect("readable").regions().collect()
    }

    #[test]
    fn the_kernels_ranges_are_set_aside_from_loader_memory() {
        // Free memory, then loader data holding the kernel's pages, then
        // boot-services data.
        let map = map(&[(7, 0, 16), (2, 0x10_000, 16), (4, 0x20_000, 4)]);
        let mut space = [Region::default(); 3 + 2 * 2];
        let mut out = [0u8; 4096];
        let kernel = [(0x11_000, 2), (0x18_000, 1)];
        let length = build(&map, STRIDE, kernel, &mut space, &mut out).expect("built");
        let region = |start, pages, kind| Region { start, pages, kind };
        assert_eq!(
            regions(&out[..length]),
            [
                region(0, 16, Kind::Usable),
                region(0x10_000, 1, Kind::Loader),
                region(0x11_000, 2, Kind::Kernel),
                region(0x13_000, 5, Kind::Loader),
                region(0x18_000, 1, Kind::Kernel),
                region(0x19_000, 7, Kind::Loader),
                region(0x20_000, 4, Kind::BootServices),
            ]
        );
    }

    #[test]
    fn specific_purpose_memory_goes_in_as_reserved() {
        let mut map = descriptor(7, 0, 16, 1 << 18);
        map.extend(descriptor(7, 0x10_000, 4, 0));
        let mut space = [Region::default(); 2];
        let mut out = [0u8; 4096];
        let length = build(&map, STRIDE, [], &mut space, &mut out).expect("built");
        let read = Handoff::read(&out[..length]).expect("readable");
        assert_eq!(read.usable_pages(), 4);
        assert_eq!(read.regions().next().map(|r| r.kind), Some(Kind::Reserved));
    }

    #[test]
    fn the_space_is_exactly_enough_for_the_worst_case() {
        // Every range in the middle of a loader descriptor of its own, so
        // each splits it into three.
        let descriptors: Vec<_> = (0..KERNEL_RANGES as u64 + 5)
            .map(|i| (2, i * 0x10_000, 16))
            .collect();
        let map = map(&descriptors);
        let kernel: Vec<_> = (0..KERNEL_RANGES as u64)
            .map(|i| (i * 0x10_000 + 0x4_000, 2))
            .collect();
        let needed = space_for(descriptors.len());
        let mut out = [0u8; 4096];
        let mut space = vec![Region::default(); needed];
        assert!(build(&map, STRIDE, kernel.clone(), &mut space, &mut out).is_ok());
        let mut space = vec![Region::default(); needed - 1];
        assert!(matches!(
            build(&map, STRIDE, kernel, &mut space, &mut out),
            Err(HandoffFailure::SetAside {
                error: BuildError::Full,
                ..
            })
        ));
    }

    #[test]
    fn each_refusal_names_where_it_happened() {
        let mut space = [Region::default(); 8];
        let mut out = [0u8; 4096];
        // Overlapping descriptors.
        let overlap = map(&[(7, 0, 16), (4, 0x8_000, 16)]);
        assert_eq!(
            build(&overlap, STRIDE, [], &mut space, &mut out),
            Err(HandoffFailure::Descriptor {
                start: 0x8_000,
                error: BuildError::Overlap
            })
        );
        // A kernel range in free memory.
        let free = map(&[(7, 0, 16)]);
        assert_eq!(
            build(&free, STRIDE, [(0x1_000, 1)], &mut space, &mut out),
            Err(HandoffFailure::SetAside {
                start: 0x1_000,
                error: BuildError::NotLoaderMemory
            })
        );
        // Too little room for the structure.
        assert_eq!(
            build(&free, STRIDE, [], &mut space, &mut out[..30]),
            Err(HandoffFailure::Write(BuildError::OutputTooSmall {
                needed: 48
            }))
        );
    }

    #[test]
    fn the_stated_stride_is_followed() {
        // Read with the specification's 40 bytes, the second descriptor's
        // fields come from the wrong place.
        let map = map(&[(7, 0, 16), (7, 0x10_000, 4)]);
        let mut space = [Region::default(); 8];
        let mut out = [0u8; 4096];
        let length = build(&map, STRIDE, [], &mut space, &mut out).expect("built");
        assert_eq!(
            Handoff::read(&out[..length]).map(|h| h.usable_pages()),
            Ok(20)
        );
        assert_ne!(
            build(&map, 40, [], &mut space, &mut out)
                .map(|length| Handoff::read(&out[..length]).map(|h| h.usable_pages())),
            Ok(Ok(20))
        );
    }
}

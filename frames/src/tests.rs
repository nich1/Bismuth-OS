//! Tests for the frame tracker. Each test checks the running counts against
//! the recount from the bitmap after every step, and the randomised test
//! checks every result against [`Model`], which keeps the free and handed-out
//! frames in two sets and shares no code with the library.

// Tests index and add freely; the crate's lints are for the library itself.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// The base of most test trackers. Its high bits are set so that an address
/// computed without the base shows.
const BASE: u64 = 0x000A_BCD0_0000_0000;
const PAGE: u64 = 0x1000;

/// The address of frame `frame` of a tracker at [`BASE`].
fn at(frame: u64) -> u64 {
    BASE + frame * PAGE
}

/// A bitmap for `frames` frames, full of set bits so that a tracker that
/// forgot to clear it shows.
fn bitmap(frames: u64) -> Vec<u64> {
    vec![u64::MAX; bitmap_words(frames) as usize]
}

/// Checks the counts, and that the running totals match the recount.
fn assert_counts(tracker: &Tracker, usable: u64, free: u64, in_use: u64) {
    let counts = tracker.counts();
    assert_eq!(counts, tracker.recount(), "running totals and recount");
    assert_eq!(
        counts,
        Counts {
            covered: tracker.frames,
            usable,
            free,
            in_use,
        }
    );
}

/// Allocates until nothing is free, and returns the addresses in order.
fn drain(tracker: &mut Tracker) -> Vec<u64> {
    let mut out = Vec::new();
    while let Some(address) = tracker.allocate() {
        out.push(address);
        assert!(
            out.len() <= tracker.frames as usize,
            "more frames than covered"
        );
    }
    out
}

/// The addresses of frames `first..end` of a tracker at [`BASE`].
fn frames(first: u64, end: u64) -> Vec<u64> {
    (first..end).map(at).collect()
}

/// Runs `operation`, expecting an error, and checks that it changed nothing.
fn refused(
    tracker: &mut Tracker,
    operation: impl FnOnce(&mut Tracker) -> Result<(), Error>,
) -> Error {
    let before = (tracker.bitmap.to_vec(), tracker.counts(), tracker.next);
    let error = operation(tracker).expect_err("the operation is refused");
    let after = (tracker.bitmap.to_vec(), tracker.counts(), tracker.next);
    assert_eq!(before, after, "a refused operation changed the tracker");
    error
}

#[test]
fn bitmap_words_are_two_per_64_frames_rounded_up() {
    for (frames, words) in [
        (0, 0),
        (1, 2),
        (63, 2),
        (64, 2),
        (65, 4),
        (128, 4),
        (129, 6),
    ] {
        assert_eq!(bitmap_words(frames), words, "{frames} frames");
    }
    assert_eq!(bitmap_words(u64::MAX), 2 * (1 << 58));
}

#[test]
fn a_new_tracker_has_nothing_free() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    assert_counts(&tracker, 0, 0, 0);
    assert_eq!(tracker.counts().covered, 100);
    assert_eq!(tracker.allocate(), None);
    assert_eq!(tracker.free(at(0)), Err(Error::NotInUse));
    assert_counts(&tracker, 0, 0, 0);
    // The garbage the bitmap held is gone.
    assert!(tracker.bitmap.iter().all(|&word| word == 0));
}

#[test]
fn a_tracker_may_cover_nothing() {
    let mut words = [u64::MAX; 0];
    let mut tracker = Tracker::new(&mut words, BASE, 0).unwrap();
    assert_counts(&tracker, 0, 0, 0);
    assert_eq!(tracker.allocate(), None);
    assert_eq!(tracker.add_usable(BASE, 1), Err(Error::OutOfRange));
    assert_eq!(tracker.free(BASE), Err(Error::OutOfRange));
}

#[test]
fn a_longer_bitmap_than_needed_is_cleared_and_its_tail_never_used() {
    let mut words = vec![u64::MAX; 10];
    let mut tracker = Tracker::new(&mut words, BASE, 70).unwrap();
    tracker.add_usable(BASE, 70).unwrap();
    assert_eq!(drain(&mut tracker), frames(0, 70));
    assert_counts(&tracker, 70, 0, 70);
    assert!(tracker.bitmap[4..].iter().all(|&word| word == 0));
}

#[test]
fn new_refuses_an_unaligned_base() {
    let mut words = bitmap(10);
    for base in [BASE + 1, BASE + 0x800, BASE + 0xFFF] {
        assert_eq!(
            Tracker::new(&mut words, base, 10).err(),
            Some(Error::Unaligned)
        );
    }
}

#[test]
fn new_refuses_coverage_past_the_top_of_the_address_range() {
    let top = 0xFFFF_FFFF_FFFF_F000;
    let mut words = bitmap(2);
    // The last page whose end still fits in 64 bits can be covered...
    assert!(Tracker::new(&mut words, top - PAGE, 1).is_ok());
    // ...but not the page after it, nor anything whose size overflows.
    assert_eq!(
        Tracker::new(&mut words, top - PAGE, 2).err(),
        Some(Error::Overflow)
    );
    assert_eq!(
        Tracker::new(&mut words, top, 1).err(),
        Some(Error::Overflow)
    );
    // Checked before the bitmap's size, which is far too small here.
    assert_eq!(
        Tracker::new(&mut words, 0, 1 << 52).err(),
        Some(Error::Overflow)
    );
}

#[test]
fn new_refuses_a_bitmap_that_is_too_small() {
    for frames in [1, 64, 65, 200] {
        let mut words = vec![0; bitmap_words(frames) as usize - 1];
        assert_eq!(
            Tracker::new(&mut words, BASE, frames).err(),
            Some(Error::BitmapTooSmall),
            "{frames} frames"
        );
    }
}

#[test]
fn one_frame() {
    let mut words = bitmap(1);
    let mut tracker = Tracker::new(&mut words, BASE, 1).unwrap();
    tracker.add_usable(BASE, 1).unwrap();
    assert_counts(&tracker, 1, 1, 0);
    assert_eq!(tracker.allocate(), Some(BASE));
    assert_counts(&tracker, 1, 0, 1);
    assert_eq!(tracker.allocate(), None);
    assert_counts(&tracker, 1, 0, 1);
    tracker.free(BASE).unwrap();
    assert_counts(&tracker, 1, 1, 0);
    assert_eq!(tracker.allocate(), Some(BASE));
    assert_counts(&tracker, 1, 0, 1);
}

#[test]
fn ranges_inside_words_and_across_their_boundaries() {
    // (first frame, pages) in a tracker of 200 frames: whole words, single
    // frames at either end of a word, ranges starting or ending mid-word, and
    // ranges spanning several words.
    let cases = [
        (0, 64),
        (0, 1),
        (63, 1),
        (64, 1),
        (64, 64),
        (5, 10),
        (60, 10),
        (63, 66),
        (127, 2),
        (1, 190),
        (128, 72),
        (199, 1),
        (0, 200),
    ];
    for (first, pages) in cases {
        let mut words = bitmap(200);
        let mut tracker = Tracker::new(&mut words, BASE, 200).unwrap();
        tracker.add_usable(at(first), pages).unwrap();
        assert_counts(&tracker, pages, pages, 0);
        assert_eq!(
            drain(&mut tracker),
            frames(first, first + pages),
            "{first} + {pages}"
        );
        assert_counts(&tracker, pages, 0, pages);
    }
}

#[test]
fn a_bitmap_whose_last_word_is_partly_covered() {
    // 70 frames: the second word pair covers frames 64 to 69 only.
    let mut words = bitmap(70);
    let mut tracker = Tracker::new(&mut words, BASE, 70).unwrap();
    assert_eq!(tracker.add_usable(at(69), 2), Err(Error::OutOfRange));
    assert_eq!(tracker.add_usable(at(70), 1), Err(Error::OutOfRange));
    tracker.add_usable(at(60), 10).unwrap();
    assert_eq!(tracker.bitmap[2], 0b11_1111, "free bits of frames 64 to 69");
    tracker.add_usable(BASE, 60).unwrap();
    assert_counts(&tracker, 70, 70, 0);
    assert_eq!(drain(&mut tracker), frames(0, 70));
    assert_eq!(
        tracker.bitmap[3], 0b11_1111,
        "in-use bits of frames 64 to 69"
    );
    assert_counts(&tracker, 70, 0, 70);
}

#[test]
fn handing_out_every_frame_then_failing_cleanly() {
    let mut words = bitmap(130);
    let mut tracker = Tracker::new(&mut words, BASE, 130).unwrap();
    tracker.add_usable(BASE, 130).unwrap();
    for frame in 0..130 {
        assert_eq!(tracker.allocate(), Some(at(frame)));
        assert_counts(&tracker, 130, 129 - frame, frame + 1);
    }
    for _ in 0..3 {
        assert_eq!(tracker.allocate(), None);
        assert_counts(&tracker, 130, 0, 130);
    }
}

#[test]
fn freed_frames_come_back_lowest_first() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    tracker.add_usable(BASE, 100).unwrap();
    assert_eq!(drain(&mut tracker).len(), 100);
    for frame in [70, 3, 64] {
        tracker.free(at(frame)).unwrap();
    }
    assert_counts(&tracker, 100, 3, 97);
    assert_eq!(drain(&mut tracker), [at(3), at(64), at(70)]);
    // The same frame again, alone.
    tracker.free(at(99)).unwrap();
    assert_eq!(tracker.allocate(), Some(at(99)));
    tracker.free(at(0)).unwrap();
    assert_eq!(tracker.allocate(), Some(at(0)));
    assert_counts(&tracker, 100, 0, 100);
}

#[test]
fn a_range_added_below_the_search_position_is_handed_out_first() {
    let mut words = bitmap(200);
    let mut tracker = Tracker::new(&mut words, BASE, 200).unwrap();
    tracker.add_usable(at(130), 5).unwrap();
    assert_eq!(tracker.allocate(), Some(at(130)));
    tracker.add_usable(at(10), 2).unwrap();
    assert_eq!(
        drain(&mut tracker),
        [at(10), at(11), at(131), at(132), at(133), at(134)]
    );
    assert_counts(&tracker, 7, 0, 7);
}

#[test]
fn reserving_free_and_unavailable_frames() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    tracker.add_usable(BASE, 20).unwrap();
    // Free frames become unavailable.
    tracker.reserve(at(5), 5).unwrap();
    assert_counts(&tracker, 15, 15, 0);
    // Frames that are already unavailable stay so, and it is no error.
    tracker.reserve(at(5), 5).unwrap();
    tracker.reserve(at(30), 70).unwrap();
    assert_counts(&tracker, 15, 15, 0);
    // A range of both: frames 15 to 19 are free, 20 to 24 never were.
    tracker.reserve(at(15), 10).unwrap();
    assert_counts(&tracker, 10, 10, 0);
    let mut handed_out = frames(0, 5);
    handed_out.extend(frames(10, 15));
    assert_eq!(drain(&mut tracker), handed_out);
    assert_counts(&tracker, 10, 0, 10);
}

#[test]
fn reserved_frames_can_be_made_usable_again() {
    let mut words = bitmap(64);
    let mut tracker = Tracker::new(&mut words, BASE, 64).unwrap();
    tracker.add_usable(BASE, 10).unwrap();
    tracker.reserve(BASE, 10).unwrap();
    assert_counts(&tracker, 0, 0, 0);
    tracker.add_usable(at(4), 2).unwrap();
    assert_counts(&tracker, 2, 2, 0);
    assert_eq!(drain(&mut tracker), [at(4), at(5)]);
}

#[test]
fn reserving_a_frame_in_use_is_refused() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    tracker.add_usable(BASE, 10).unwrap();
    assert_eq!(tracker.allocate(), Some(at(0)));
    assert_eq!(tracker.allocate(), Some(at(1)));
    assert_eq!(refused(&mut tracker, |t| t.reserve(at(1), 1)), Error::InUse);
    // The frame in use is the last one of the range: the free ones before
    // it stay free.
    assert_eq!(refused(&mut tracker, |t| t.reserve(BASE, 2)), Error::InUse);
    assert_eq!(refused(&mut tracker, |t| t.reserve(BASE, 50)), Error::InUse);
    assert_counts(&tracker, 10, 8, 2);
}

#[test]
fn adding_refuses_bad_ranges() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    for start in [BASE + 1, BASE + 0x800, at(99) + 0xFFF] {
        assert_eq!(
            refused(&mut tracker, |t| t.add_usable(start, 1)),
            Error::Unaligned
        );
    }
    assert_eq!(
        refused(&mut tracker, |t| t.add_usable(BASE, 0)),
        Error::Empty
    );
    // Unaligned is checked before empty.
    assert_eq!(
        refused(&mut tracker, |t| t.add_usable(BASE + 1, 0)),
        Error::Unaligned
    );
    assert_eq!(
        refused(&mut tracker, |t| t.add_usable(0xFFFF_FFFF_FFFF_F000, 1)),
        Error::Overflow
    );
    assert_eq!(
        refused(&mut tracker, |t| t.add_usable(BASE, u64::MAX)),
        Error::Overflow
    );
    assert_eq!(
        refused(&mut tracker, |t| t.add_usable(BASE, 1 << 52)),
        Error::Overflow
    );
    for (start, pages) in [
        (BASE - PAGE, 1),
        (BASE - PAGE, 2),
        (0, 1),
        (at(100), 1),
        (at(99), 2),
        (BASE, 101),
        (at(200), 5),
    ] {
        assert_eq!(
            refused(&mut tracker, |t| t.add_usable(start, pages)),
            Error::OutOfRange,
            "{start:#x} + {pages}"
        );
    }
}

#[test]
fn adding_refuses_a_range_over_usable_frames() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    tracker.add_usable(at(10), 10).unwrap();
    assert_eq!(tracker.allocate(), Some(at(10)));
    // Over a frame in use, a free frame, partly before and partly after.
    for (first, pages) in [(10, 1), (11, 1), (5, 6), (19, 5), (0, 100)] {
        assert_eq!(
            refused(&mut tracker, |t| t.add_usable(at(first), pages)),
            Error::Overlap,
            "{first} + {pages}"
        );
    }
    // Next to it on either side is fine.
    tracker.add_usable(at(9), 1).unwrap();
    tracker.add_usable(at(20), 1).unwrap();
    assert_counts(&tracker, 12, 11, 1);
}

#[test]
fn reserving_refuses_bad_ranges() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    tracker.add_usable(BASE, 100).unwrap();
    assert_eq!(
        refused(&mut tracker, |t| t.reserve(BASE + 0x10, 1)),
        Error::Unaligned
    );
    assert_eq!(refused(&mut tracker, |t| t.reserve(BASE, 0)), Error::Empty);
    assert_eq!(
        refused(&mut tracker, |t| t.reserve(at(1), u64::MAX / PAGE)),
        Error::Overflow
    );
    for (start, pages) in [(BASE - PAGE, 2), (at(100), 1), (at(50), 51)] {
        assert_eq!(
            refused(&mut tracker, |t| t.reserve(start, pages)),
            Error::OutOfRange,
            "{start:#x} + {pages}"
        );
    }
    assert_counts(&tracker, 100, 100, 0);
}

#[test]
fn freeing_refuses_bad_addresses() {
    let mut words = bitmap(100);
    let mut tracker = Tracker::new(&mut words, BASE, 100).unwrap();
    tracker.add_usable(BASE, 50).unwrap();
    tracker.reserve(at(40), 10).unwrap();
    assert_eq!(tracker.allocate(), Some(at(0)));
    for address in [BASE + 1, at(0) + 0xFFF, u64::MAX] {
        assert_eq!(
            refused(&mut tracker, |t| t.free(address)),
            Error::Unaligned,
            "{address:#x}"
        );
    }
    for address in [0, BASE - PAGE, at(100), at(1000), 0xFFFF_FFFF_FFFF_F000] {
        assert_eq!(
            refused(&mut tracker, |t| t.free(address)),
            Error::OutOfRange,
            "{address:#x}"
        );
    }
    // Free (a double free), reserved, never usable.
    for frame in [1, 39, 40, 49, 50, 99] {
        assert_eq!(
            refused(&mut tracker, |t| t.free(at(frame))),
            Error::NotInUse,
            "frame {frame}"
        );
    }
    tracker.free(at(0)).unwrap();
    assert_eq!(refused(&mut tracker, |t| t.free(at(0))), Error::NotInUse);
    assert_counts(&tracker, 40, 40, 0);
}

/// A run of allocations examines each bitmap word about once, not the whole
/// bitmap from the start every time.
#[test]
fn a_run_of_allocations_is_linear_overall() {
    // 100 word pairs.
    let mut words = bitmap(6400);
    let mut tracker = Tracker::new(&mut words, BASE, 6400).unwrap();
    // Nothing free: nothing to examine.
    assert_eq!(tracker.allocate(), None);
    assert_eq!(tracker.examined, 0);
    tracker.add_usable(BASE, 6400).unwrap();
    assert_eq!(drain(&mut tracker).len(), 6400);
    // One word per allocation, plus one each time the search moves on to the
    // next word, plus the last word once more for the failed allocation that
    // ends the drain. Scanning from the start would examine about 320000.
    assert_eq!(tracker.examined, 6400 + 99 + 1);
    // Once everything is handed out, a failed allocation examines nothing.
    let before = tracker.examined;
    assert_eq!(tracker.allocate(), None);
    assert_eq!(tracker.examined, before);
}

#[test]
fn a_freed_frame_is_found_without_a_search() {
    let mut words = bitmap(6400);
    let mut tracker = Tracker::new(&mut words, BASE, 6400).unwrap();
    tracker.add_usable(BASE, 6400).unwrap();
    assert_eq!(drain(&mut tracker).len(), 6400);
    for frame in [6399, 3200, 0] {
        tracker.free(at(frame)).unwrap();
        let before = tracker.examined;
        assert_eq!(tracker.allocate(), Some(at(frame)));
        assert_eq!(tracker.examined - before, 1, "frame {frame}");
    }
}

#[test]
fn the_worst_case_examines_every_word_once() {
    // Frame 0 and the last frame free: after frame 0 goes, the search walks
    // all 100 words to the last one.
    let mut words = bitmap(6400);
    let mut tracker = Tracker::new(&mut words, BASE, 6400).unwrap();
    tracker.add_usable(at(6399), 1).unwrap();
    tracker.add_usable(BASE, 1).unwrap();
    assert_eq!(tracker.allocate(), Some(at(0)));
    assert_eq!(tracker.examined, 1);
    assert_eq!(tracker.allocate(), Some(at(6399)));
    assert_eq!(tracker.examined, 1 + 100);
}

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

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }
}

/// What the tracker should hold: the free and the handed-out frames, by
/// number from the base. Addresses are worked out in 128 bits, so nothing
/// overflows.
struct Model {
    base: u64,
    frames: u64,
    free: BTreeSet<u64>,
    in_use: BTreeSet<u64>,
}

impl Model {
    /// The frames of a range, checked in the documented order.
    fn range(&self, start: u64, pages: u64) -> Result<(u64, u64), Error> {
        if !start.is_multiple_of(PAGE) {
            return Err(Error::Unaligned);
        }
        if pages == 0 {
            return Err(Error::Empty);
        }
        let end = u128::from(start) + u128::from(pages) * u128::from(PAGE);
        if end > u128::from(u64::MAX) {
            return Err(Error::Overflow);
        }
        let covered_end = u128::from(self.base) + u128::from(self.frames) * u128::from(PAGE);
        if start < self.base || end > covered_end {
            return Err(Error::OutOfRange);
        }
        let first = (start - self.base) / PAGE;
        Ok((first, first + pages))
    }

    fn add_usable(&mut self, start: u64, pages: u64) -> Result<(), Error> {
        let (first, end) = self.range(start, pages)?;
        if self.free.range(first..end).next().is_some()
            || self.in_use.range(first..end).next().is_some()
        {
            return Err(Error::Overlap);
        }
        self.free.extend(first..end);
        Ok(())
    }

    fn reserve(&mut self, start: u64, pages: u64) -> Result<(), Error> {
        let (first, end) = self.range(start, pages)?;
        if self.in_use.range(first..end).next().is_some() {
            return Err(Error::InUse);
        }
        let reserved: Vec<u64> = self.free.range(first..end).copied().collect();
        for frame in reserved {
            self.free.remove(&frame);
        }
        Ok(())
    }

    fn allocate(&mut self) -> Option<u64> {
        let frame = self.free.pop_first()?;
        self.in_use.insert(frame);
        Some(self.base + frame * PAGE)
    }

    fn free(&mut self, address: u64) -> Result<(), Error> {
        if !address.is_multiple_of(PAGE) {
            return Err(Error::Unaligned);
        }
        if address < self.base || (address - self.base) / PAGE >= self.frames {
            return Err(Error::OutOfRange);
        }
        let frame = (address - self.base) / PAGE;
        if !self.in_use.remove(&frame) {
            return Err(Error::NotInUse);
        }
        self.free.insert(frame);
        Ok(())
    }

    fn counts(&self) -> Counts {
        Counts {
            covered: self.frames,
            usable: (self.free.len() + self.in_use.len()) as u64,
            free: self.free.len() as u64,
            in_use: self.in_use.len() as u64,
        }
    }
}

/// Long mixed runs of adding, reserving, allocating and freeing, with good
/// and bad arguments, checked call by call against [`Model`].
#[test]
fn random_operations_match_the_model() {
    let mut random = Random(0xF4A3_E5D1_7C0B_2961);
    let mut outcomes: BTreeMap<&str, u32> = BTreeMap::new();
    for _ in 0..60 {
        let frames = 1 + random.below(300);
        let base = match random.below(3) {
            0 => 0,
            1 => random.below(1 << 40) * PAGE,
            // Ending at the last page whose end fits in 64 bits.
            _ => 0xFFFF_FFFF_FFFF_F000 - frames * PAGE,
        };
        let mut words = vec![random.next(); bitmap_words(frames) as usize];
        let mut tracker = Tracker::new(&mut words, base, frames).unwrap();
        let mut model = Model {
            base,
            frames,
            free: BTreeSet::new(),
            in_use: BTreeSet::new(),
        };
        // A frame number near the covered range, as an address.
        let near = |random: &mut Random| {
            base.wrapping_add(
                (random.below(frames + 16))
                    .wrapping_sub(8)
                    .wrapping_mul(PAGE),
            )
        };
        for _ in 0..400 {
            let operation = random.below(10);
            let (name, actual, expected) = match operation {
                0..=2 => {
                    let mut start = near(&mut random);
                    let mut pages = match random.below(12) {
                        0 => 0,
                        1 => random.next(),
                        _ => 1 + random.below(40),
                    };
                    match random.below(25) {
                        0 => start = start.wrapping_add(1 + random.below(0xFFF)),
                        1 => start = random.next() & !0xFFF,
                        2 => pages = u64::MAX / PAGE - random.below(4),
                        _ => {}
                    }
                    let (actual, expected) = if operation == 2 {
                        (tracker.reserve(start, pages), model.reserve(start, pages))
                    } else {
                        (
                            tracker.add_usable(start, pages),
                            model.add_usable(start, pages),
                        )
                    };
                    assert_eq!(actual, expected, "op {operation}: {start:#x} + {pages}");
                    let name = match (operation, actual) {
                        (2, Ok(())) => "reserved",
                        (_, Ok(())) => "added",
                        _ => "",
                    };
                    (name, actual, expected)
                }
                3..=6 => {
                    let actual = tracker.allocate();
                    let expected = model.allocate();
                    assert_eq!(actual, expected, "allocate");
                    let name = if actual.is_some() {
                        "allocated"
                    } else {
                        "none free"
                    };
                    (name, Ok(()), Ok(()))
                }
                _ => {
                    let address = match random.below(10) {
                        0..=4 if !model.in_use.is_empty() => {
                            let index = random.below(model.in_use.len() as u64) as usize;
                            base + model.in_use.iter().nth(index).unwrap() * PAGE
                        }
                        5 if !model.free.is_empty() => {
                            let index = random.below(model.free.len() as u64) as usize;
                            base + model.free.iter().nth(index).unwrap() * PAGE
                        }
                        6 => near(&mut random).wrapping_add(1 + random.below(0xFFF)),
                        7 => random.next(),
                        _ => near(&mut random),
                    };
                    let actual = tracker.free(address);
                    let expected = model.free(address);
                    assert_eq!(actual, expected, "free {address:#x}");
                    (if actual.is_ok() { "freed" } else { "" }, actual, expected)
                }
            };
            assert_eq!(actual, expected);
            let name = match actual {
                Ok(()) => name,
                Err(Error::Unaligned) => "unaligned",
                Err(Error::Empty) => "empty",
                Err(Error::Overflow) => "overflow",
                Err(Error::BitmapTooSmall) => "bitmap too small",
                Err(Error::OutOfRange) => "out of range",
                Err(Error::Overlap) => "overlap",
                Err(Error::InUse) => "in use",
                Err(Error::NotInUse) => "not in use",
            };
            *outcomes.entry(name).or_default() += 1;
            assert_eq!(tracker.counts(), model.counts(), "counts");
            assert_eq!(tracker.recount(), model.counts(), "recount");
        }
    }
    // Every outcome but a bitmap too small, which only `new` returns, must
    // be common, or the test explores too little.
    for name in [
        "added",
        "reserved",
        "allocated",
        "none free",
        "freed",
        "unaligned",
        "empty",
        "overflow",
        "out of range",
        "overlap",
        "in use",
        "not in use",
    ] {
        let count = outcomes.get(name).copied().unwrap_or(0);
        assert!(count >= 300, "{name}: {outcomes:?}");
    }
}

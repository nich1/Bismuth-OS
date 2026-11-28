//! Keeps track of which 4 KiB frames of physical memory are free.
//!
//! A [`Tracker`] covers a contiguous range of physical memory: a number of
//! frames from a page-aligned base. It holds no memory of its own. The caller
//! lends it a bitmap of [`bitmap_words`] words, which [`Tracker::new`]
//! clears, so every frame starts unavailable. The caller then adds the
//! ranges it knows are usable and reserves the ones it learns are taken.
//!
//! # The bitmap
//!
//! Each group of 64 frames has two words. The first has a bit set for each
//! free frame, the second for each frame handed out by [`Tracker::allocate`]
//! and not yet given back. A frame with neither bit set is unavailable: the
//! tracker was never told it is usable, or it was reserved since. No frame
//! has both.
//!
//! A set bit means free, not in use, so that a frame the tracker was never
//! told about can never be handed out: allocation only takes a frame whose
//! free bit is set, and a cleared bitmap has none. A lost bit loses a frame;
//! it never hands out memory nobody said was usable. The second word is what
//! tells a frame handed out from one that never was, so that freeing a frame
//! that was never allocated, and adding a range over frames in use, are
//! refused instead of making those frames free twice.
//!
//! # Rules
//!
//! - [`Tracker::allocate`] always returns the lowest free address, so the
//!   same calls return the same frames.
//! - A range of zero pages is an error, [`Error::Empty`].
//! - Reserving makes free frames unavailable and leaves unavailable frames
//!   as they are, without an error. A range holding a frame in use is
//!   refused. A reserved frame is again one the tracker was never told
//!   about, so adding it later makes it free.
//! - Arguments are checked in the order of the [`Error`] variants, and an
//!   error changes nothing.
//!
//! # Cost
//!
//! The tracker remembers a search position: a group of 64 frames below which
//! no group has a free frame. Allocation searches upward from it, one free
//! word per group, and stays at the group it found a frame in, even when it
//! took that group's last free frame. Freeing a frame or adding a range
//! below the position moves it down to that group; freeing a frame above it
//! leaves it where it is.
//!
//! - One allocation examines each group's free word at most once: at most
//!   the covered frames divided by 64, rounded up.
//! - A run of allocations with no frees or additions in between examines at
//!   most the number of groups plus one word per allocation in all.
//! - An allocation right after a free examines one word if the group at the
//!   search position then has a free frame, which is always so when the
//!   freed frame is in that group or below it. Otherwise the search walks up
//!   from the position, at most to the freed frame's group, which can be the
//!   whole bitmap. So with memory nearly full, frees and
//!   allocations in turn cost time in proportion to the bitmap's size per
//!   allocation.
//! - Freeing is constant time; adding and reserving go over the range
//!   twice, a frame at a time.

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

/// The size of a frame, in bytes.
pub const PAGE_SIZE: u64 = 4096;

/// Frames per bitmap word.
const BITS: u64 = 64;

/// How many words of bitmap a tracker of `frames` frames needs: two for
/// each 64 frames, rounded up.
pub const fn bitmap_words(frames: u64) -> u64 {
    // At most 2^59: it cannot overflow.
    frames.div_ceil(BITS).wrapping_mul(2)
}

/// Why the tracker refused a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An address is not a multiple of 4 KiB.
    Unaligned,
    /// A range has zero pages.
    Empty,
    /// The end of a range, or of the frames to cover, is past the last
    /// address 64 bits hold, so the last page of the address range can never
    /// be covered.
    Overflow,
    /// The bitmap has fewer words than [`bitmap_words`] asks for.
    BitmapTooSmall,
    /// An address, or part of a range, is outside the frames the tracker
    /// covers.
    OutOfRange,
    /// A range to add holds a frame that is already usable: free or in use.
    Overlap,
    /// A range to reserve holds a frame in use.
    InUse,
    /// The frame to free is not in use: it is free already, reserved, or was
    /// never usable.
    NotInUse,
}

/// The tracker's counts of frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Frames the tracker covers, whatever their state.
    pub covered: u64,
    /// Frames made usable and not reserved since: free or in use.
    pub usable: u64,
    /// Frames free now.
    pub free: u64,
    /// Frames handed out and not given back.
    pub in_use: u64,
}

/// What one frame is, as its two bits say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Unavailable,
    Free,
    InUse,
}

/// Tracks which frames of a range of physical memory are free, in a bitmap
/// the caller lends it.
pub struct Tracker<'a> {
    bitmap: &'a mut [u64],
    base: u64,
    frames: u64,
    free: u64,
    in_use: u64,
    /// The group to search from: no group below it has a free frame.
    next: u64,
    /// Free words examined by [`Tracker::allocate`], for the cost tests.
    #[cfg(test)]
    examined: u64,
}

impl<'a> Tracker<'a> {
    /// A tracker of `frames` frames from physical address `base`, every one
    /// unavailable. Clears the whole of `bitmap`, which must have at least
    /// [`bitmap_words`] words. The end of the covered range must fit in 64
    /// bits.
    pub fn new(bitmap: &'a mut [u64], base: u64, frames: u64) -> Result<Self, Error> {
        if !base.is_multiple_of(PAGE_SIZE) {
            return Err(Error::Unaligned);
        }
        end(base, frames)?;
        if (bitmap.len() as u64) < bitmap_words(frames) {
            return Err(Error::BitmapTooSmall);
        }
        bitmap.fill(0);
        Ok(Tracker {
            bitmap,
            base,
            frames,
            free: 0,
            in_use: 0,
            next: frames.div_ceil(BITS),
            #[cfg(test)]
            examined: 0,
        })
    }

    /// Makes the `pages` frames from physical address `start` free. Refuses
    /// the range if any of them is already free or in use.
    pub fn add_usable(&mut self, start: u64, pages: u64) -> Result<(), Error> {
        let (first, end) = self.range(start, pages)?;
        if (first..end).any(|frame| self.state(frame) != State::Unavailable) {
            return Err(Error::Overlap);
        }
        for frame in first..end {
            self.put(frame, State::Free);
        }
        // Every frame was unavailable, so the total stays at most `frames`.
        self.free = self.free.wrapping_add(pages);
        self.next = self.next.min(first / BITS);
        Ok(())
    }

    /// Makes the `pages` frames from physical address `start` unavailable.
    /// Frames already unavailable stay so; a range holding a frame in use is
    /// refused.
    pub fn reserve(&mut self, start: u64, pages: u64) -> Result<(), Error> {
        let (first, end) = self.range(start, pages)?;
        if (first..end).any(|frame| self.state(frame) == State::InUse) {
            return Err(Error::InUse);
        }
        for frame in first..end {
            if self.state(frame) == State::Free {
                self.put(frame, State::Unavailable);
                self.free = self.free.wrapping_sub(1);
            }
        }
        Ok(())
    }

    /// Hands out the free frame with the lowest address, or `None` if no
    /// frame is free.
    pub fn allocate(&mut self) -> Option<u64> {
        while self.next < self.frames.div_ceil(BITS) {
            #[cfg(test)]
            {
                self.examined = self.examined.wrapping_add(1);
            }
            // Below `groups`, so twice it is below `bitmap_words(frames)`.
            let word = self.bitmap.get(self.next.wrapping_mul(2) as usize);
            let word = word.copied().unwrap_or(0);
            if word != 0 {
                // Bits past the last covered frame are never set, so this
                // frame is covered and its address does not overflow.
                let frame = self
                    .next
                    .wrapping_mul(BITS)
                    .wrapping_add(u64::from(word.trailing_zeros()));
                self.put(frame, State::InUse);
                self.free = self.free.wrapping_sub(1);
                self.in_use = self.in_use.wrapping_add(1);
                return Some(self.base.wrapping_add(frame.wrapping_mul(PAGE_SIZE)));
            }
            self.next = self.next.wrapping_add(1);
        }
        None
    }

    /// Gives back the frame at physical address `address`, which must be in
    /// use.
    pub fn free(&mut self, address: u64) -> Result<(), Error> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(Error::Unaligned);
        }
        let frame = match address.checked_sub(self.base) {
            Some(offset) if offset / PAGE_SIZE < self.frames => offset / PAGE_SIZE,
            _ => return Err(Error::OutOfRange),
        };
        if self.state(frame) != State::InUse {
            return Err(Error::NotInUse);
        }
        self.put(frame, State::Free);
        self.in_use = self.in_use.wrapping_sub(1);
        self.free = self.free.wrapping_add(1);
        self.next = self.next.min(frame / BITS);
        Ok(())
    }

    /// The counts, from running totals.
    pub fn counts(&self) -> Counts {
        Counts {
            covered: self.frames,
            usable: self.free.wrapping_add(self.in_use),
            free: self.free,
            in_use: self.in_use,
        }
    }

    /// The counts, recounted from every bit of the bitmap, for tests to
    /// compare with [`Tracker::counts`]. Takes time in proportion to the
    /// bitmap's length.
    pub fn recount(&self) -> Counts {
        let (mut free, mut in_use) = (0_u64, 0_u64);
        for pair in self.bitmap.chunks(2) {
            let mut bits = pair.iter().map(|word| u64::from(word.count_ones()));
            // At most 64 bits per word: neither total can overflow.
            free = free.wrapping_add(bits.next().unwrap_or(0));
            in_use = in_use.wrapping_add(bits.next().unwrap_or(0));
        }
        Counts {
            covered: self.frames,
            usable: free.wrapping_add(in_use),
            free,
            in_use,
        }
    }

    /// The frames of the `pages` pages from `start`, as the first and the one
    /// after the last.
    fn range(&self, start: u64, pages: u64) -> Result<(u64, u64), Error> {
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(Error::Unaligned);
        }
        if pages == 0 {
            return Err(Error::Empty);
        }
        end(start, pages)?;
        let first = start.checked_sub(self.base).ok_or(Error::OutOfRange)? / PAGE_SIZE;
        match first.checked_add(pages) {
            Some(end) if end <= self.frames => Ok((first, end)),
            _ => Err(Error::OutOfRange),
        }
    }

    /// The state of covered frame `frame`.
    fn state(&self, frame: u64) -> State {
        let (index, bit) = position(frame);
        // Covered frames are inside the bitmap; anything else reads as
        // unavailable.
        let set = |index: usize| self.bitmap.get(index).is_some_and(|word| word & bit != 0);
        match (set(index), set(index.wrapping_add(1))) {
            (true, _) => State::Free,
            (false, true) => State::InUse,
            (false, false) => State::Unavailable,
        }
    }

    /// Sets the two bits of covered frame `frame` to say `state`.
    fn put(&mut self, frame: u64, state: State) {
        let (index, bit) = position(frame);
        for (index, set) in [
            (index, state == State::Free),
            (index.wrapping_add(1), state == State::InUse),
        ] {
            if let Some(word) = self.bitmap.get_mut(index) {
                if set {
                    *word |= bit;
                } else {
                    *word &= !bit;
                }
            }
        }
    }
}

/// The address after the `pages` pages from `start`, if it fits in 64 bits.
fn end(start: u64, pages: u64) -> Result<u64, Error> {
    pages
        .checked_mul(PAGE_SIZE)
        .and_then(|bytes| start.checked_add(bytes))
        .ok_or(Error::Overflow)
}

/// The index of the free word that holds frame `frame`'s bit, and the bit.
/// Its in-use bit is the same bit of the next word.
fn position(frame: u64) -> (usize, u64) {
    // A frame number over 64, times 2, cannot overflow.
    let index = (frame / BITS).wrapping_mul(2) as usize;
    (index, 1_u64.wrapping_shl((frame % BITS) as u32))
}

//! The job host's counting allocator (design/phase4-helper.md P4.4.3): the system allocator, with
//! each run's live and peak bytes counted apart, so that `job_progress` and `job_result` carry a
//! run's real peak (`peak_mb`) and the server's cost table learns what a job needs (P4.5.4).
//!
//! **How a run is counted.** The thread that plays a run claims a [`Tally`]: one of [`SLOTS`]
//! slots, named by a thread-local index. Every allocation made on that thread adds its size to the
//! slot's live bytes, every free made there takes it away, and the slot keeps the highest live
//! figure it reached. A thread with no tally (the host's reader, its writer, a server's request
//! thread) pays one thread-local read per call and counts nothing. Each slot has one writer, the
//! thread that claimed it, so a plain load and store keep it (no read-modify-write between
//! threads), and slots sit on cache lines of their own so two runs never share one.
//!
//! **What it does not count:** memory a run's thread frees that another thread allocated (the
//! spec handed to it) makes its live figure dip, and memory another thread frees for it (its
//! result, written out by the host's writer) is never taken away; neither lowers a peak. A run's
//! game is made, played and dropped on its own thread, so its peak is its game's.
//!
//! The program that runs a job host declares it: `#[global_allocator] static A: Counting =
//! Counting;` (the helper's binary, the Python extension, and the jobs crate's host test).
//! [`installed`] says whether this process's allocator is it.
//!
//! The thread-local index is a `Cell` made by a `const` initialiser with no destructor, so reading
//! it never allocates and works on any thread at any time, which an allocator needs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

/// How many runs can be counted at once: a host runs at most `threads` runs (at most 1,024 by
/// the protocol's bounds).
pub const SLOTS: usize = 1024;

/// No slot: this thread counts nothing.
const NONE: usize = usize::MAX;

/// One run's tally, on a cache line of its own.
#[repr(align(64))]
struct Slot {
    live: AtomicIsize,
    peak: AtomicIsize,
    taken: AtomicBool,
}

impl Slot {
    const fn new() -> Self {
        Self { live: AtomicIsize::new(0), peak: AtomicIsize::new(0), taken: AtomicBool::new(false) }
    }
}

static TABLE: [Slot; SLOTS] = [const { Slot::new() }; SLOTS];

thread_local! {
    /// The slot this thread counts into, or [`NONE`].
    static MINE: Cell<usize> = const { Cell::new(NONE) };
}

fn mine() -> usize {
    MINE.try_with(Cell::get).unwrap_or(NONE)
}

/// This thread allocated `n` more bytes.
fn grew(n: usize) {
    if let Some(s) = TABLE.get(mine()) {
        let live = s.live.load(Ordering::Relaxed).saturating_add(n.cast_signed());
        s.live.store(live, Ordering::Relaxed);
        if live > s.peak.load(Ordering::Relaxed) {
            s.peak.store(live, Ordering::Relaxed);
        }
    }
}

/// This thread freed `n` bytes.
fn shrank(n: usize) {
    if let Some(s) = TABLE.get(mine()) {
        let live = s.live.load(Ordering::Relaxed).saturating_sub(n.cast_signed());
        s.live.store(live, Ordering::Relaxed);
    }
}

/// The system allocator, counting each run's bytes apart.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counting;

// SAFETY: every call is passed to `System` with the caller's own arguments, which meet the
// trait's contract because the caller's do; the counting around it touches only atomics and a
// const-initialised thread-local cell, and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, which the trait requires to be of non-zero size.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's block, allocated by this allocator (so by `System`) with `layout`.
        unsafe { System.dealloc(ptr, layout) };
        shrank(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's block, layout and new size, passed on.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                shrank(layout.size() - new_size);
            }
        }
        p
    }
}

/// A run's tally, held by the thread that plays it: what that thread allocates from now on is
/// counted here until it is dropped. Not `Send`: it names this thread's slot.
#[derive(Debug)]
pub struct Tally {
    slot: usize,
    _here: PhantomData<*const ()>,
}

/// Reads a tally from another thread (the host's progress reports).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reader {
    slot: usize,
}

fn bytes(n: isize) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

impl Tally {
    /// Claims a free slot for this thread, counting from zero. `None` when this thread already
    /// holds one or every slot is taken.
    #[must_use]
    pub fn claim() -> Option<Self> {
        if mine() != NONE {
            return None;
        }
        let slot = TABLE.iter().position(|s| {
            s.taken.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
        })?;
        TABLE[slot].live.store(0, Ordering::Relaxed);
        TABLE[slot].peak.store(0, Ordering::Relaxed);
        if MINE.try_with(|m| m.set(slot)).is_err() {
            TABLE[slot].taken.store(false, Ordering::Release);
            return None;
        }
        Some(Self { slot, _here: PhantomData })
    }

    /// The highest live figure since the claim, in bytes.
    #[must_use]
    pub fn peak_bytes(&self) -> u64 {
        self.reader().peak_bytes()
    }

    /// A handle another thread reads this tally through.
    #[must_use]
    pub const fn reader(&self) -> Reader {
        Reader { slot: self.slot }
    }
}

impl Reader {
    /// The highest live figure since the tally was claimed, in bytes.
    #[must_use]
    pub fn peak_bytes(&self) -> u64 {
        TABLE.get(self.slot).map_or(0, |s| bytes(s.peak.load(Ordering::Relaxed)))
    }

    /// The live figure now, in bytes (0 when the thread freed more than it allocated).
    #[must_use]
    pub fn live_bytes(&self) -> u64 {
        TABLE.get(self.slot).map_or(0, |s| bytes(s.live.load(Ordering::Relaxed)))
    }
}

impl Drop for Tally {
    fn drop(&mut self) {
        let _cleared = MINE.try_with(|m| m.set(NONE));
        TABLE[self.slot].taken.store(false, Ordering::Release);
    }
}

/// Whether this process allocates through [`Counting`]: a block allocated under a fresh tally
/// shows in it.
#[must_use]
pub fn installed() -> bool {
    let Some(t) = Tally::claim() else { return false };
    let probe: Vec<u8> = black_box(Vec::with_capacity(1 << 16));
    let seen = t.peak_bytes() >= 1 << 16;
    drop(black_box(probe));
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1 << 20;

    /// The unit tests run under this allocator (lib.rs): a tally counts what its thread holds at
    /// most, frees included, apart from every other thread's.
    #[test]
    fn a_tally_keeps_its_threads_peak_with_frees_counted() {
        assert!(installed());
        let t = Tally::claim().expect("a free slot");
        assert!(Tally::claim().is_none(), "one tally a thread");
        for _ in 0..3 {
            let block = black_box(vec![1u8; 10 * MB]);
            drop(black_box(block));
        }
        let peak = t.peak_bytes();
        assert!((10 * MB..11 * MB).contains(&usize::try_from(peak).unwrap_or(0)), "{peak}");
        assert!(t.reader().live_bytes() < MB as u64, "freed: {}", t.reader().live_bytes());
        // Another thread's allocations are not this tally's.
        let reader = t.reader();
        std::thread::spawn(|| drop(black_box(vec![1u8; 30 * MB]))).join().expect("joined");
        assert_eq!(reader.peak_bytes(), peak);
        drop(t);
        assert!(Tally::claim().is_some(), "the slot and the thread are free again");
    }
}

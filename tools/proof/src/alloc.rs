//! Owns the process-wide allocator statistic: how many allocations an interval served, how
//! many bytes they carried, and the high-water mark of what was outstanding.
//!
//! Every allocation this process makes passes through here, including every allocation the
//! codec makes, which is what turns "the codec holds no whole input" from a claim about the
//! source into a measurement of the run.
//!
//! The counters are global because an allocator is. They are statistics: no measurement
//! depends on two of them being consistent with each other at an instant, so the weakest
//! ordering is the correct one.
//!
//! This module does not own what an interval means. It counts; the caller decides what the
//! interval covers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static OUTSTANDING: AtomicI64 = AtomicI64::new(0);
static PEAK_OUTSTANDING: AtomicI64 = AtomicI64::new(0);

/// The allocator every allocation in this process passes through.
pub struct Counting;

#[global_allocator]
static GLOBAL: Counting = Counting;

// A counting allocator is the only way to observe allocation from inside the process that
// performs it, and `GlobalAlloc` is an unsafe trait. The unsafe surface is this file. No
// codec path reaches it: the codec links no part of this crate.
#[allow(unsafe_code)]
// SAFETY: every method forwards its arguments unchanged to the system allocator, which
// implements the same contract, and adds only relaxed atomic counting around it.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller guarantees the layout contract, and it is forwarded unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            took(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        gave(layout.size());
        // SAFETY: the caller guarantees the pointer came from this allocator with this
        // layout, and it is forwarded unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn took(size: usize) {
    let bytes = i64::try_from(size).unwrap_or(i64::MAX);
    let _ = ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    let _ = ALLOCATED_BYTES.fetch_add(u64::try_from(size).unwrap_or(u64::MAX), Ordering::Relaxed);
    let outstanding = OUTSTANDING
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);
    let _ = PEAK_OUTSTANDING.fetch_max(outstanding, Ordering::Relaxed);
}

fn gave(size: usize) {
    let bytes = i64::try_from(size).unwrap_or(i64::MAX);
    let _ = OUTSTANDING.fetch_sub(bytes, Ordering::Relaxed);
}

/// What the allocator has served since the process started.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    pub allocations: u64,
    pub allocated_bytes: u64,
    pub outstanding_bytes: u64,
    pub peak_outstanding_bytes: u64,
}

/// Reads the counters.
pub fn counts() -> Counts {
    Counts {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        outstanding_bytes: u64::try_from(OUTSTANDING.load(Ordering::Relaxed)).unwrap_or(0),
        peak_outstanding_bytes: u64::try_from(PEAK_OUTSTANDING.load(Ordering::Relaxed))
            .unwrap_or(0),
    }
}

/// Restarts the high-water mark at what is outstanding now.
///
/// A measurement that runs after setup wants the peak of its own interval, not the peak of
/// everything the process did before it.
pub fn restart_peak() {
    let outstanding = OUTSTANDING.load(Ordering::Relaxed);
    PEAK_OUTSTANDING.store(outstanding, Ordering::Relaxed);
}

/// What one interval served, which is the difference between two readings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Interval {
    pub allocations: u64,
    pub allocated_bytes: u64,
    pub peak_outstanding_bytes: u64,
}

impl Counts {
    /// The interval that starts at `self` and ends at `end`.
    #[must_use]
    pub const fn until(self, end: Self) -> Interval {
        Interval {
            allocations: end.allocations.saturating_sub(self.allocations),
            allocated_bytes: end.allocated_bytes.saturating_sub(self.allocated_bytes),
            peak_outstanding_bytes: end.peak_outstanding_bytes,
        }
    }
}

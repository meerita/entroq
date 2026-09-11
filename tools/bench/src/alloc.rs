//! Owns the process-wide allocator statistic: how many allocations an interval served, how
//! many bytes they carried, and the high-water mark of what was outstanding.
//!
//! Every allocation the harness makes passes through here, and so does every allocation a
//! competitor library makes, because each competitor that publishes an allocator hook is
//! handed the two functions below instead of the C runtime's. That is what makes one counter
//! cover both sides of the boundary. A competitor that publishes no hook allocates through
//! the C runtime, this counter never sees it, and the result says so rather than reporting a
//! zero.
//!
//! The counters are global because an allocator is. They are statistics: no measurement
//! depends on them being consistent with each other at an instant, so the weakest ordering
//! is the correct one.
//!
//! This module does not own what an interval means. It counts; the caller decides what the
//! interval covers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::{c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// The alignment every allocation served through the C hook takes.
///
/// It matches what a C runtime's `malloc` guarantees, which is what a library handed an
/// allocator hook is entitled to assume.
const C_ALIGN: usize = 16;

/// The header that lets the C hook return an allocation the Rust allocator owns.
///
/// The Rust allocator needs the layout back at release, and a C `free` is given only an
/// address, so the size travels in front of the block the library sees.
const C_HEADER: usize = C_ALIGN;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static OUTSTANDING: AtomicI64 = AtomicI64::new(0);
static PEAK_OUTSTANDING: AtomicI64 = AtomicI64::new(0);

/// The allocator every allocation in this process passes through.
pub struct Counting;

#[global_allocator]
static GLOBAL: Counting = Counting;

// The harness has no safe way to observe allocation, and the C libraries it measures have no
// safe way to be handed one. The unsafe surface is this module and the extern declarations
// of the competitor modules. No Entroq codec path reaches either.
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

/// Serves one allocation for a library that takes a size, which is what Zstandard and Brotli
/// publish.
///
/// # Safety
///
/// The returned block is released only by `release`, which reads the header this wrote.
#[allow(unsafe_code)]
pub unsafe extern "C" fn reserve(_opaque: *mut c_void, size: usize) -> *mut c_void {
    reserve_bytes(size)
}

/// Serves one allocation for a library that takes a count and an element size, which is what
/// zlib publishes.
///
/// # Safety
///
/// The returned block is released only by `release`, which reads the header this wrote.
#[allow(unsafe_code)]
pub unsafe extern "C" fn reserve_items(
    _opaque: *mut c_void,
    items: c_uint,
    size: c_uint,
) -> *mut c_void {
    let bytes = usize::try_from(items)
        .ok()
        .zip(usize::try_from(size).ok())
        .and_then(|(items, size)| items.checked_mul(size));
    bytes.map_or_else(ptr::null_mut, reserve_bytes)
}

/// Releases a block that `reserve` or `reserve_items` produced.
///
/// # Safety
///
/// `address` is null, or a pointer this module returned and has not released.
// The header sits at `C_ALIGN`, which is what the cast requires.
#[allow(unsafe_code, clippy::cast_ptr_alignment)]
pub unsafe extern "C" fn release(_opaque: *mut c_void, address: *mut c_void) {
    if address.is_null() {
        return;
    }
    // SAFETY: the caller guarantees this address came from `reserve_bytes`, which placed the
    // header immediately before it inside the same allocation.
    let base = unsafe { address.cast::<u8>().sub(C_HEADER) };
    // SAFETY: `base` points at the header `reserve_bytes` wrote, aligned to `C_ALIGN`.
    let total = unsafe { base.cast::<usize>().read() };
    let Ok(layout) = Layout::from_size_align(total, C_ALIGN) else {
        return;
    };
    // SAFETY: the block came from `std::alloc::alloc` with exactly this layout.
    unsafe { std::alloc::dealloc(base, layout) }
}

// The block is allocated at `C_ALIGN`, which is what the header cast requires. A byte
// pointer is what the allocator hands back, so the cast is the only way to read it.
#[allow(unsafe_code, clippy::cast_ptr_alignment)]
fn reserve_bytes(size: usize) -> *mut c_void {
    let Some(total) = size.checked_add(C_HEADER) else {
        return ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, C_ALIGN) else {
        return ptr::null_mut();
    };
    // SAFETY: the layout carries the header, so its size is never zero.
    let base = unsafe { std::alloc::alloc(layout) };
    if base.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `base` owns at least `C_HEADER` bytes at `C_ALIGN`, which holds one `usize`.
    unsafe { base.cast::<usize>().write(total) };
    // SAFETY: `C_HEADER` is within the allocation, so the offset stays in bounds.
    unsafe { base.add(C_HEADER) }.cast::<c_void>()
}

fn took(size: usize) {
    let bytes = u64::try_from(size).unwrap_or(u64::MAX);
    let signed = i64::try_from(size).unwrap_or(i64::MAX);
    let _ = ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    let _ = ALLOCATED_BYTES.fetch_add(bytes, Ordering::Relaxed);
    let outstanding = OUTSTANDING
        .fetch_add(signed, Ordering::Relaxed)
        .saturating_add(signed);
    let _ = PEAK_OUTSTANDING.fetch_max(outstanding, Ordering::Relaxed);
}

fn gave(size: usize) {
    let signed = i64::try_from(size).unwrap_or(i64::MAX);
    let _ = OUTSTANDING.fetch_sub(signed, Ordering::Relaxed);
}

/// The counter state an interval starts from.
pub struct Interval {
    allocations: u64,
    bytes: u64,
    outstanding: i64,
}

/// What an interval cost the allocator.
pub struct Usage {
    pub allocations: u64,
    pub bytes: u64,
    /// The largest amount outstanding above the interval's starting point.
    ///
    /// For an operation that creates a context, runs, and releases it, this is what the
    /// operation held at once.
    pub peak_bytes: u64,
}

/// Opens an interval, and arms the high-water mark at the current outstanding total.
pub fn open() -> Interval {
    let outstanding = OUTSTANDING.load(Ordering::Relaxed);
    PEAK_OUTSTANDING.store(outstanding, Ordering::Relaxed);
    Interval {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        outstanding,
    }
}

/// Closes an interval and reports what it served.
pub fn close(interval: &Interval) -> Usage {
    let peak = PEAK_OUTSTANDING.load(Ordering::Relaxed);
    Usage {
        allocations: ALLOCATIONS
            .load(Ordering::Relaxed)
            .saturating_sub(interval.allocations),
        bytes: ALLOCATED_BYTES
            .load(Ordering::Relaxed)
            .saturating_sub(interval.bytes),
        peak_bytes: u64::try_from(peak.saturating_sub(interval.outstanding)).unwrap_or(0),
    }
}

// The tests drive the C hook directly, which is the only way to prove the header round
// trips and that a release of nothing is not a fault.
#[allow(unsafe_code)]
#[cfg(test)]
mod tests {
    //! The counters are process wide, and a test binary runs its tests on several threads,
    //! so an interval here also sees what other tests allocate. Every assertion is therefore
    //! a lower bound or a direction, which holds whatever else the process is doing. The
    //! measurement these counters serve runs one operation at a time in one process.

    use super::{Usage, close, open, release, reserve, reserve_bytes, reserve_items};
    use std::ffi::c_void;
    use std::ptr;

    fn measure<T>(work: impl FnOnce() -> T) -> Usage {
        let interval = open();
        let held = work();
        let usage = close(&interval);
        drop(held);
        usage
    }

    #[test]
    fn an_interval_counts_the_allocations_it_served() {
        let usage = measure(|| vec![0_u8; 4096]);
        assert!(usage.allocations >= 1, "{}", usage.allocations);
        assert!(usage.bytes >= 4096, "{}", usage.bytes);
    }

    #[test]
    fn a_block_released_inside_the_interval_still_counts_as_an_allocation() {
        let usage = measure(|| {
            drop(vec![0_u8; 8192]);
        });
        assert!(usage.allocations >= 1);
        assert!(usage.peak_bytes >= 8192, "{}", usage.peak_bytes);
    }

    #[test]
    fn the_bytes_an_interval_served_grow_with_what_it_asked_for() {
        let small = measure(|| vec![0_u8; 4096]);
        let large = measure(|| vec![0_u8; 4 * 1024 * 1024]);
        assert!(large.bytes > small.bytes, "{} {}", large.bytes, small.bytes);
        // The high-water mark is relative to what was outstanding when the interval opened,
        // and another thread releasing a block just after that lowers it slightly.
        assert!(large.peak_bytes >= 3 * 1024 * 1024, "{}", large.peak_bytes);
    }

    #[test]
    fn a_c_allocation_is_counted_and_round_trips_through_the_hook() {
        let interval = open();
        // SAFETY: the block is released once, below, through the matching hook.
        let block = unsafe { reserve(ptr::null_mut(), 1024) };
        assert!(!block.is_null());
        // SAFETY: `block` came from `reserve` and has not been released.
        unsafe { release(ptr::null_mut(), block) };
        let usage = close(&interval);
        assert!(usage.allocations >= 1, "{}", usage.allocations);
        assert!(usage.bytes >= 1024, "{}", usage.bytes);
    }

    #[test]
    fn a_c_allocation_by_count_multiplies_its_two_arguments() {
        let interval = open();
        // SAFETY: the block is released once, below, through the matching hook.
        let block = unsafe { reserve_items(ptr::null_mut(), 64, 32) };
        assert!(!block.is_null());
        // SAFETY: `block` came from `reserve_items` and has not been released.
        unsafe { release(ptr::null_mut(), block) };
        let usage = close(&interval);
        assert!(usage.bytes >= 2048, "{}", usage.bytes);
    }

    #[test]
    fn a_c_allocation_holds_the_bytes_it_was_asked_for() {
        // SAFETY: the block is released once, below, through the matching hook.
        let block = unsafe { reserve(ptr::null_mut(), 4096) };
        assert!(!block.is_null());
        // SAFETY: the hook returned a block of at least 4096 usable bytes.
        unsafe { block.cast::<u8>().write_bytes(0xAB, 4096) };
        // SAFETY: `block` came from `reserve` and has not been released.
        unsafe { release(ptr::null_mut(), block) };
    }

    #[test]
    fn releasing_nothing_is_not_a_fault() {
        // SAFETY: a null address is the documented no-op, which is what C `free` promises.
        unsafe { release(ptr::null_mut(), ptr::null_mut()) };
    }

    #[test]
    fn an_allocation_that_would_overflow_its_header_serves_nothing() {
        assert!(reserve_bytes(usize::MAX).is_null());
    }

    #[test]
    fn a_c_allocation_hands_back_an_aligned_block() {
        // SAFETY: the block is released once, below, through the matching hook.
        let block = unsafe { reserve(ptr::null_mut(), 1) };
        assert_eq!(block.cast::<u8>() as usize % super::C_ALIGN, 0);
        // SAFETY: `block` came from `reserve` and has not been released.
        unsafe { release(ptr::null_mut(), block) };
    }

    #[test]
    fn a_zero_length_c_allocation_still_returns_a_block() {
        // SAFETY: the block is released once, below, through the matching hook.
        let block: *mut c_void = unsafe { reserve(ptr::null_mut(), 0) };
        assert!(!block.is_null());
        // SAFETY: `block` came from `reserve` and has not been released.
        unsafe { release(ptr::null_mut(), block) };
    }
}

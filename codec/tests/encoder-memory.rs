//! What the encoder's parse path allocates and holds, measured from the allocator rather than
//! stated.
//!
//! An allocator is process wide, so this file is its own test binary and its one test runs the
//! whole measurement in order. The counters it keeps are per thread, so the figure is what the
//! encoder held and not what the process held around it.
//!
//! The figure is parser-owned allocated bytes. The input fragment is the caller's and is
//! allocated before the interval begins, so it is not in the figure. Nothing here reads a
//! resident set, and no claim about resident pages is made from these numbers.
//!
//! This file owns no codec behavior. It drives the public API and the allocator and nothing
//! else.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use codec::format::Error;
use codec::parser::{MAX_PARSE_BYTES, Parser};

// The counters are per thread, not per process. A process-wide figure would carry whatever the
// test harness allocated on its own thread while the interval ran, and that showed up as a
// intermittent excess of about a kibibyte over the declared bound. The encoder allocates on the
// thread that called it and on no other, so the thread's own figure is the encoder's figure
// exactly, and the measurement stops reporting another thread's work as the codec's.
//
// Each is const-initialized and holds no destructor, so reading one allocates nothing and the
// allocator cannot recurse into itself through it.
thread_local! {
    static OUTSTANDING: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

#[global_allocator]
static GLOBAL: Counting = Counting;

// A counting allocator is the only way to observe allocation from inside the process that
// performs it, and the trait is unsafe. The unsafe surface is this one impl, and no codec path
// reaches it: the codec links no part of this file.
#[allow(unsafe_code)]
// SAFETY: every method forwards its arguments unchanged to the system allocator, which
// implements the same contract, and adds only thread-local counting around it.
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
        // SAFETY: the caller guarantees the pointer came from this allocator with this layout,
        // and it is forwarded unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn took(size: usize) {
    let bytes = i64::try_from(size).unwrap_or(i64::MAX);
    let _ = ALLOCATIONS.try_with(|count| count.set(count.get().saturating_add(1)));
    let _ = OUTSTANDING.try_with(|outstanding| {
        let now = outstanding.get().saturating_add(bytes);
        outstanding.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

fn gave(size: usize) {
    let bytes = i64::try_from(size).unwrap_or(i64::MAX);
    let _ = OUTSTANDING
        .try_with(|outstanding| outstanding.set(outstanding.get().saturating_sub(bytes)));
}

/// Starts an interval and reports the outstanding bytes it begins from.
fn begin() -> i64 {
    let outstanding = OUTSTANDING.with(Cell::get);
    PEAK.with(|peak| peak.set(outstanding));
    ALLOCATIONS.with(|count| count.set(0));
    outstanding
}

/// The bytes the interval held above what it began from.
fn peak_above(baseline: i64) -> u64 {
    u64::try_from(PEAK.with(Cell::get).saturating_sub(baseline)).unwrap_or(0)
}

/// The allocations the interval made.
fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// The input classes the bound is measured on.
const CLASSES: [&str; 6] = [
    "zeros",
    "one-byte",
    "random",
    "repetitive",
    "near-window",
    "incompressible",
];

/// Fills `block` with the bytes of one input class at `offset`.
///
/// The content is generated rather than stored, so a sixteen-mebibyte run costs no disk and
/// no buffer the measurement would have to subtract.
fn fill(class: &str, offset: u64, block: &mut [u8]) {
    for (step, slot) in block.iter_mut().enumerate() {
        let at = offset.saturating_add(u64::try_from(step).unwrap_or(0));
        *slot = match class {
            "zeros" => 0,
            "one-byte" => 0x5A,
            "repetitive" => {
                let phrase = b"the same thirty-seven bytes, again!!!";
                phrase
                    .get(usize::try_from(at.checked_rem(37).unwrap_or(0)).unwrap_or(0))
                    .copied()
                    .unwrap_or(b'?')
            }
            // The content repeats every window, so every position has an identical
            // predecessor at a distance of exactly one window, which is the farthest a match
            // may reach.
            "near-window" => scramble(
                at.checked_rem(u64::from(codec::sequence::WINDOW))
                    .unwrap_or(0),
            ),
            "incompressible" => scramble(at.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            _ => scramble(at),
        };
    }
}

fn scramble(value: u64) -> u8 {
    let mixed = value
        .wrapping_mul(0xBF58_476D_1CE4_E5B9)
        .rotate_left(31)
        .wrapping_mul(0x94D0_49BB_1331_11EB);
    u8::try_from((mixed >> 33) & 0xFF).unwrap_or(0)
}

/// Parses `total` bytes of one class in whole blocks, and reports what the interval held and
/// how many allocations it made.
fn parse_class(class: &str, total: u64) -> Result<(u64, u64), Error> {
    parse_with(class, total, Parser::new)
}

/// Parses `total` bytes of one class through the parser `construct` builds, and reports what
/// the interval held and how many allocations it made.
///
/// The parser is built inside the interval, so its setup allocations are in the figure and a
/// per-block allocation would show as a count that moves with the length.
fn parse_with(
    class: &str,
    total: u64,
    construct: impl Fn() -> Parser,
) -> Result<(u64, u64), Error> {
    let mut block = vec![0u8; MAX_PARSE_BYTES];
    let baseline = begin();
    let mut parser = construct();
    let mut done = 0u64;
    while done < total {
        let len = usize::try_from(total.saturating_sub(done))
            .unwrap_or(MAX_PARSE_BYTES)
            .min(MAX_PARSE_BYTES);
        let room = block.get_mut(..len).ok_or(Error::InvalidParameter)?;
        fill(class, done, room);
        let sequences = parser.parse(room)?;
        assert_eq!(
            sequences.decoded_len(),
            u64::try_from(len).unwrap_or(0),
            "{class}"
        );
        done = done.saturating_add(u64::try_from(len).unwrap_or(0));
    }
    let held = peak_above(baseline);
    let made = allocations();
    drop(parser);
    Ok((held, made))
}

const MIB: u64 = 1_048_576;

/// The allocations one parser makes, whatever the input is.
///
/// The single table, the window buffer, the literal storage and the step storage: four,
/// all at setup, and none per block. The FAST matcher dropped the chain's link array, which
/// is the one allocation this figure lost.
const SETUP_ALLOCATIONS: u64 = 4;

#[test]
fn the_parse_holds_no_more_than_it_declared_and_does_not_grow_with_the_input() -> Result<(), Error>
{
    let declared = u64::try_from(Parser::declared_bytes(MAX_PARSE_BYTES)).unwrap_or(u64::MAX);

    // The declared bound, on every input class, at one mebibyte each. The parser allocates
    // once and reuses its storage, so the allocation count is the same on every class and the
    // same as it is at every length below.
    for class in CLASSES {
        let (held, made) = parse_class(class, MIB)?;
        assert!(
            held <= declared,
            "{class} held {held} bytes against a declared {declared}"
        );
        assert!(
            held >= u64::try_from(Parser::state_bytes()).unwrap_or(0),
            "{class} held {held} bytes, which is less than the state the parser keeps"
        );
        assert_eq!(
            made, SETUP_ALLOCATIONS,
            "{class} allocated {made} times rather than once per buffer at setup"
        );
        println!("class {class:<16} held {held} of {declared} declared, in {made} allocations");
    }

    // The growth curve. The peak is the same figure at every length, which is what makes the
    // bound a property of the configuration rather than of the input.
    let mut curve = Vec::new();
    for length in [MIB, MIB.saturating_mul(4), MIB.saturating_mul(16)] {
        let (held, made) = parse_class("random", length)?;
        println!("length {length:>9} held {held} of {declared} declared, in {made} allocations");
        curve.push((length, held, made));
    }
    let first = curve.first().map(|point| point.1).unwrap_or_default();
    for (length, held, made) in &curve {
        assert_eq!(
            *held, first,
            "the peak moved with the input length at {length} bytes"
        );
        assert!(*held <= declared, "the peak passed the declared bound");
        assert_eq!(
            *made, SETUP_ALLOCATIONS,
            "the allocation count moved with the input length at {length} bytes"
        );
    }
    Ok(())
}

/// The allocations one BALANCED parser makes, whatever the input is.
///
/// The chain's head table and link array, the window buffer, the literal storage and the
/// step storage: five, all at setup, and none per block. One more than the FAST figure, and
/// the link array is the allocation.
const BALANCED_SETUP_ALLOCATIONS: u64 = 5;

#[test]
fn the_balanced_parse_holds_no_more_than_it_declared_and_does_not_grow_with_the_input()
-> Result<(), Error> {
    let declared =
        u64::try_from(Parser::balanced_declared_bytes(MAX_PARSE_BYTES)).unwrap_or(u64::MAX);
    let state = u64::try_from(Parser::balanced_state_bytes()).unwrap_or(0);

    // The declared BALANCED bound, on every input class, at one mebibyte each. The parser
    // allocates once and reuses its storage, so the allocation count is the same on every
    // class and the same as it is at every length below.
    for class in CLASSES {
        let (held, made) = parse_with(class, MIB, Parser::balanced)?;
        assert!(
            held <= declared,
            "{class} held {held} bytes against a declared {declared}"
        );
        assert!(
            held >= state,
            "{class} held {held} bytes, which is less than the state the parser keeps"
        );
        assert_eq!(
            made, BALANCED_SETUP_ALLOCATIONS,
            "{class} allocated {made} times rather than once per buffer at setup"
        );
        println!("class {class:<16} held {held} of {declared} declared, in {made} allocations");
    }

    // The growth curve. The peak is the same figure at every length, which is what makes the
    // bound a property of the configuration rather than of the input.
    let mut curve = Vec::new();
    for length in [MIB, MIB.saturating_mul(4), MIB.saturating_mul(16)] {
        let (held, made) = parse_with("random", length, Parser::balanced)?;
        println!("length {length:>9} held {held} of {declared} declared, in {made} allocations");
        curve.push((length, held, made));
    }
    let first = curve.first().map(|point| point.1).unwrap_or_default();
    for (length, held, made) in &curve {
        assert_eq!(
            *held, first,
            "the peak moved with the input length at {length} bytes"
        );
        assert!(*held <= declared, "the peak passed the declared bound");
        assert_eq!(
            *made, BALANCED_SETUP_ALLOCATIONS,
            "the allocation count moved with the input length at {length} bytes"
        );
    }
    Ok(())
}

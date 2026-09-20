//! What the streaming pair actually allocates, measured from the allocator rather than stated.
//!
//! An allocator is process wide, so this file is its own test binary and its tests run the
//! whole measurement in order. The counters it keeps are per thread, so each figure is what the
//! codec held and not what the process held around it.
//!
//! Two figures are kept apart, because they answer different questions.
//!
//! ```text
//! steady state   what a machine holds between calls, which the machine states itself
//! peak           the most it held at any instant, which includes everything one block
//!                allocates and frees inside a call
//! ```
//!
//! A bound that counts only the first is a bound the codec exceeds every time it codes a
//! block. The encoder declares both figures: the steady state from its construction
//! capacities under its mode, and the peak as that figure plus the per-site transient ceiling
//! its emitter states. The decoder declares its steady state and not its peak, so the peak on
//! the decode direction is measured and recorded and nothing asserts a figure for it. What is
//! asserted there is the property that matters and that a measurement can settle: the peak is
//! bounded by the configuration and does not move with the input.
//!
//! Nothing here reads a resident set, and no claim about resident pages is made from these
//! numbers.
//!
//! This file owns no codec behavior. It drives the public API and the allocator and nothing
//! else.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use codec::format::{
    DecoderPolicy, Error, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass,
};
use codec::stream::{
    DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Decoder, Encoder, Mode, StreamState,
};

// The counters are per thread, not per process. The codec allocates on the thread that called
// it and on no other, so the thread's own figure is the codec's figure exactly, and the
// measurement never reports the test harness's own work as the codec's.
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

/// The input classes both figures are measured on.
#[derive(Clone, Copy, Debug)]
enum Class {
    Zeros,
    OneByte,
    Incompressible,
    Repetitive,
    Text,
    NearWindow,
}

impl Class {
    const ALL: [Self; 6] = [
        Self::Zeros,
        Self::OneByte,
        Self::Incompressible,
        Self::Repetitive,
        Self::Text,
        Self::NearWindow,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Zeros => "zeros",
            Self::OneByte => "one-byte",
            Self::Incompressible => "incompressible",
            Self::Repetitive => "repetitive",
            Self::Text => "text",
            Self::NearWindow => "near-window",
        }
    }

    fn at(self, position: u64) -> u8 {
        match self {
            Self::Zeros => 0,
            Self::OneByte => 0xA5,
            Self::Incompressible => mix(position),
            Self::Repetitive => {
                let phrase = b"the quick brown fox jumps over the lazy dog. ";
                let at = usize::try_from(position.checked_rem(45).unwrap_or(0)).unwrap_or(0);
                phrase.get(at).copied().unwrap_or(b' ')
            }
            Self::Text => {
                let pick = mix(position.wrapping_mul(3)) & 0x0F;
                b"abcdefghijklmnop"
                    .get(usize::from(pick))
                    .copied()
                    .unwrap_or(b'a')
            }
            Self::NearWindow => mix(position.checked_rem(65_413).unwrap_or(0)),
        }
    }
}

/// A value that depends on its position and on nothing else.
fn mix(position: u64) -> u8 {
    let mut state = position.wrapping_add(0x9E37_79B9_7F4A_7C15);
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    u8::try_from((state ^ (state >> 31)).wrapping_shr(24) & 0xFF).unwrap_or(0)
}

/// What one measured interval held.
#[derive(Clone, Copy, Debug)]
struct Held {
    peak: u64,
    steady: u64,
    /// The peak the encoder declares for the configuration, when the direction declares one.
    declared_peak: Option<u64>,
    allocations: u64,
}

const CHUNK: usize = 16 * 1024;

const fn header() -> FrameHeader {
    FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    )
}

/// Encodes `total` generated bytes under `mode` and reports what the encoder held while it
/// did.
///
/// The generated content is produced in place into a buffer allocated before the interval
/// begins, so nothing the caller owns is in the figure.
fn encoded(class: Class, total: usize, sink: &mut Vec<u8>, mode: Mode) -> Result<Held, Error> {
    let mut input = vec![0_u8; CHUNK];
    let mut room = vec![0_u8; CHUNK];
    sink.clear();
    sink.reserve(total.saturating_add(CHUNK));

    let baseline = begin();
    let mut encoder = match mode {
        Mode::Fast => Encoder::new(header())?,
        Mode::Balanced => Encoder::balanced(header())?,
    };
    let steady = u64::try_from(encoder.steady_state_bytes()).unwrap_or(0);
    let peak = u64::try_from(encoder.peak_bytes()).unwrap_or(0);
    let mut at = 0_usize;
    while at < total {
        let span = CHUNK.min(total.saturating_sub(at));
        let chunk = input.get_mut(..span).ok_or(Error::InvalidParameter)?;
        for (lane, slot) in chunk.iter_mut().enumerate() {
            *slot = class.at(u64::try_from(at.saturating_add(lane)).unwrap_or(0));
        }
        let mut fed = 0_usize;
        while fed < span {
            let rest = chunk.get(fed..).ok_or(Error::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut room)?;
            sink.extend_from_slice(
                room.get(..progress.produced)
                    .ok_or(Error::InvalidParameter)?,
            );
            fed = fed.saturating_add(progress.consumed);
        }
        at = at.saturating_add(span);
    }
    loop {
        let progress = encoder.finish(&mut room)?;
        sink.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        if progress.state == StreamState::Finished {
            break;
        }
    }
    let held = Held {
        peak: peak_above(baseline),
        steady,
        declared_peak: Some(peak),
        allocations: allocations(),
    };
    drop(encoder);
    Ok(held)
}

/// Decodes a frame and reports what the decoder held while it did.
fn decoded(stream: &[u8], expected: usize) -> Result<Held, Error> {
    let mut room = vec![0_u8; CHUNK];

    let baseline = begin();
    let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
    let steady = u64::try_from(decoder.steady_state_bytes()).unwrap_or(0);
    let mut produced = 0_usize;
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, &mut room)?;
        at = at.saturating_add(progress.consumed);
        produced = produced.saturating_add(progress.produced);
        if progress.state == StreamState::Finished {
            break;
        }
        assert!(
            progress.consumed > 0 || progress.produced > 0,
            "the decoder stalled"
        );
    }
    decoder.finish()?;
    assert_eq!(produced, expected, "the decode did not produce its content");
    assert!(
        decoder.peak_table_bytes() <= DecoderPolicy::CONSERVATIVE.max_table_bytes(),
        "the decoder held {} table bytes against a ceiling of {}",
        decoder.peak_table_bytes(),
        DecoderPolicy::CONSERVATIVE.max_table_bytes()
    );
    let held = Held {
        peak: peak_above(baseline),
        steady,
        declared_peak: None,
        allocations: allocations(),
    };
    drop(decoder);
    Ok(held)
}

fn report(what: &str, label: &str, bytes: usize, held: Held) {
    if let Some(declared) = held.declared_peak {
        println!(
            "codec-memory: {what} {label} {bytes} bytes: peak {} steady {} declared peak {declared} allocations {}",
            held.peak, held.steady, held.allocations
        );
    } else {
        println!(
            "codec-memory: {what} {label} {bytes} bytes: peak {} steady {} allocations {}",
            held.peak, held.steady, held.allocations
        );
    }
}

/// The table memory the encoder and the decoder are both held to.
const TABLE_CEILING: u64 = 65_536;

/// The header scratch each machine keeps.
///
/// It lives inside the machine's own value rather than on the heap, so the allocator never
/// sees it and a figure taken from the allocator is below the stated one by at most this.
const HEADER_SCRATCH: u64 = 64;

/// The bytes a machine allocates at construction, out of what it states it holds.
///
/// The stated figure counts the tables in force at the ceiling that bounds them, and a run
/// that builds no table holds none of it. So the floor a measurement can be held to is the
/// stated figure less the ceiling and less the scratch that never reaches the allocator.
const fn heap_floor(steady: u64) -> u64 {
    steady
        .saturating_sub(TABLE_CEILING)
        .saturating_sub(HEADER_SCRATCH)
}

/// The transient allowance this measurement holds the peak to, over the buffers a machine
/// allocates at construction.
///
/// It is a recorded headroom and not a bound a mode declares. Coding or reading one block
/// allocates in proportion to that block, so the allowance is stated in block lengths; what it
/// catches is a buffer that turns out to be proportional to the object instead.
const TRANSIENT_ALLOWANCE: u64 = 8 * DEFAULT_BLOCK_BYTES as u64;

/// The peak is above the steady state on both directions, and the steady state is held.
///
/// A peak equal to the steady state would mean the per-block work allocates nothing, and it
/// does. A peak below it would mean the steady figure states memory the machine never holds.
#[test]
fn the_peak_is_above_the_buffers_each_machine_allocates_at_construction() -> Result<(), Error> {
    const BYTES: usize = 196_608;
    let mut stream = Vec::new();
    for class in Class::ALL {
        for (what, held) in [
            ("encode", encoded(class, BYTES, &mut stream, Mode::Fast)?),
            ("decode", decoded(&stream, BYTES)?),
        ] {
            report(what, class.name(), BYTES, held);
            let floor = heap_floor(held.steady);
            assert!(
                held.peak >= floor,
                "class {}: the {what} peaked at {} below the {floor} it allocates at \
                 construction",
                class.name(),
                held.peak
            );
            assert!(
                held.peak <= floor.saturating_add(TRANSIENT_ALLOWANCE),
                "class {}: the {what} peaked at {} above {floor} and its transient allowance",
                class.name(),
                held.peak
            );
        }
    }
    Ok(())
}

/// The peak is bounded by the configuration and not by the input.
///
/// This is the property a memory bound is for, and it is the one a measurement can settle
/// while no mode declares a figure. A peak that grew with the input would mean some buffer is
/// proportional to the object rather than to the window and the block.
#[test]
fn the_peak_does_not_grow_with_the_input_length() -> Result<(), Error> {
    let lengths = [262_144_usize, 1_048_576, 4_194_304];
    assert!(
        lengths.first().copied().unwrap_or(0) > DEFAULT_REGION_BYTES / 8,
        "the shortest run must fill more than one block"
    );
    let mut stream = Vec::new();
    let mut encode_peaks = Vec::new();
    let mut decode_peaks = Vec::new();
    for total in lengths {
        let encode = encoded(Class::Text, total, &mut stream, Mode::Fast)?;
        report("encode", "text", total, encode);
        encode_peaks.push(encode.peak);

        let decode = decoded(&stream, total)?;
        report("decode", "text", total, decode);
        decode_peaks.push(decode.peak);
    }
    // The spread is what a longer run adds, and it has to be smaller than one block. Every
    // block allocates in proportion to itself and frees it, so the peak is the widest of those
    // transients and a longer run only samples more of them. A peak that scaled with the
    // object would show a spread of the same order as the input, not of one block.
    for (what, peaks) in [("encoder", &encode_peaks), ("decoder", &decode_peaks)] {
        let low = peaks.iter().copied().min().unwrap_or(0);
        let high = peaks.iter().copied().max().unwrap_or(0);
        let spread = high.saturating_sub(low);
        println!("codec-memory: {what} peak spread over a sixteenfold input range: {spread}");
        assert!(
            spread < u64::from(DEFAULT_BLOCK_BYTES),
            "the {what}'s peak moved {spread} bytes across a sixteenfold input range"
        );
    }
    Ok(())
}

/// Each mode peaks inside the peak it declares, on every input class.
///
/// The declared figure is the steady state from construction capacities plus the per-site
/// transient ceiling, and the transient is mode independent: both modes assemble through the
/// same machinery. A class that peaked above its figure would name a site the formula misses.
#[test]
fn each_mode_peaks_inside_its_declared_peak() -> Result<(), Error> {
    const BYTES: usize = 196_608;
    let mut stream = Vec::new();
    for mode in [Mode::Fast, Mode::Balanced] {
        let label = match mode {
            Mode::Fast => "encode",
            Mode::Balanced => "balanced-encode",
        };
        for class in Class::ALL {
            let held = encoded(class, BYTES, &mut stream, mode)?;
            report(label, class.name(), BYTES, held);
            let declared = held.declared_peak.ok_or(Error::InvalidParameter)?;
            assert!(
                held.peak <= declared,
                "class {}: {label} peaked at {} above its declared {declared}",
                class.name(),
                held.peak
            );
            let floor = heap_floor(held.steady);
            assert!(
                held.peak >= floor,
                "class {}: {label} peaked at {} below the {floor} it allocates at construction",
                class.name(),
                held.peak
            );
        }
    }
    Ok(())
}

/// The BALANCED peak is bounded by the configuration and not by the input.
///
/// The same property the FAST direction holds, over the same sixteenfold range: a longer run
/// samples more block transients and holds no wider one.
#[test]
fn the_balanced_peak_does_not_grow_with_the_input_length() -> Result<(), Error> {
    let lengths = [262_144_usize, 1_048_576, 4_194_304];
    let mut stream = Vec::new();
    let mut peaks = Vec::new();
    for total in lengths {
        let held = encoded(Class::Text, total, &mut stream, Mode::Balanced)?;
        report("balanced-encode", "text", total, held);
        let declared = held.declared_peak.ok_or(Error::InvalidParameter)?;
        assert!(
            held.peak <= declared,
            "the BALANCED encoder peaked at {} above its declared {declared} over {total} bytes",
            held.peak
        );
        peaks.push(held.peak);
    }
    let low = peaks.iter().copied().min().unwrap_or(0);
    let high = peaks.iter().copied().max().unwrap_or(0);
    let spread = high.saturating_sub(low);
    println!("codec-memory: balanced encoder peak spread over a sixteenfold input range: {spread}");
    assert!(
        spread < u64::from(DEFAULT_BLOCK_BYTES),
        "the BALANCED encoder's peak moved {spread} bytes across a sixteenfold input range"
    );
    Ok(())
}

/// The allocations the streaming machines make at construction, whatever the input is.
///
/// FAST builds the single table, BALANCED the head table and the link array, and both stage
/// one region and its blocks beside the payload scratch: seven and eight, all at setup. An
/// incompressible run assembles nothing, so its count is the construction count exactly, and
/// a per-block allocation would show as a count above it. The decoder holds its two content
/// buffers and nothing else on such a run.
const FAST_ENCODER_SETUP_ALLOCATIONS: u64 = 7;
const BALANCED_ENCODER_SETUP_ALLOCATIONS: u64 = 8;
const DECODER_SETUP_ALLOCATIONS: u64 = 2;

#[test]
fn an_incompressible_run_allocates_only_at_construction() -> Result<(), Error> {
    const BYTES: usize = 1_048_576;
    let mut stream = Vec::new();
    for (mode, setup) in [
        (Mode::Fast, FAST_ENCODER_SETUP_ALLOCATIONS),
        (Mode::Balanced, BALANCED_ENCODER_SETUP_ALLOCATIONS),
    ] {
        let label = match mode {
            Mode::Fast => "encode",
            Mode::Balanced => "balanced-encode",
        };
        let held = encoded(Class::Incompressible, BYTES, &mut stream, mode)?;
        report(label, Class::Incompressible.name(), BYTES, held);
        assert_eq!(
            held.allocations, setup,
            "{label} allocated {} times over sixteen blocks rather than {setup} at construction",
            held.allocations
        );
        let back = decoded(&stream, BYTES)?;
        report("decode", Class::Incompressible.name(), BYTES, back);
        assert_eq!(
            back.allocations, DECODER_SETUP_ALLOCATIONS,
            "the decode allocated {} times rather than {DECODER_SETUP_ALLOCATIONS} at construction",
            back.allocations
        );
    }
    Ok(())
}

/// A BALANCED stream that declares more table memory than policy allows is refused before the
/// decoder allocates for it.
///
/// The decoder side is unchanged by the mode: admission precedes every allocation the block
/// would size, so the table counters stay at zero past the refusal.
#[test]
fn an_over_policy_balanced_stream_is_refused_before_allocation() -> Result<(), Error> {
    const BYTES: usize = 65_536;
    let mut stream = Vec::new();
    let _ = encoded(Class::Text, BYTES, &mut stream, Mode::Balanced)?;
    let narrow = DecoderPolicy::CONSERVATIVE.with_max_table_bytes(64);
    let mut decoder = Decoder::new(narrow);
    let mut room = vec![0_u8; CHUNK];
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        match decoder.decode(rest, &mut room) {
            Ok(progress) => {
                at = at.saturating_add(progress.consumed);
                assert_ne!(
                    progress.state,
                    StreamState::Finished,
                    "the over-policy stream decoded to a successful end"
                );
                assert!(
                    progress.consumed > 0 || progress.produced > 0,
                    "the decoder stalled instead of refusing"
                );
            }
            Err(error) => {
                assert!(
                    matches!(error, Error::LimitExceeded { allowed: 64, .. }),
                    "the refusal was {error:?} rather than the table ceiling"
                );
                assert_eq!(
                    decoder.tables_built(),
                    0,
                    "a table was built before the refusal"
                );
                assert_eq!(
                    decoder.peak_table_bytes(),
                    0,
                    "table memory was held before the refusal"
                );
                assert_eq!(
                    decoder.decode(&[], &mut room),
                    Err(error),
                    "a failed decoder answered a later call"
                );
                return Ok(());
            }
        }
    }
}

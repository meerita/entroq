//! The RAW side of the BALANCED byte contract.
//!
//! High-entropy and already-compressed inputs select RAW blocks. On those
//! inputs BALANCED must emit exactly the stream FAST emits, so the carried
//! skip schedule only recovers encode CPU and never moves a byte. The block
//! bytes are pinned at the values recorded when the skip was accepted:
//! 65 540 for a 64 KiB input and 1 048 640 for a 1 MiB input, in both
//! modes, with the stream carrying exactly the 34 bytes of frame, region,
//! and terminator overhead around them.
//!
//! The corpus lives outside the repository and is materialized by the corpus
//! tooling. A host that does not hold it reports so and measures nothing,
//! which is the one thing this file does conditionally.

use std::env;
use std::path::PathBuf;

use codec::format::{
    DecoderPolicy, Error, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass,
};
use codec::stream::{Decoder, Encoder, StreamState};

/// The frame/region/terminator overhead a RAW stream carries around its
/// block bytes.
const RAW_OVERHEAD: usize = 34;

/// One RAW case: a frozen file, the prefix it reads, and the recorded block
/// bytes and block count under both modes.
struct Case {
    file: &'static str,
    len: usize,
    block_bytes: usize,
    blocks: usize,
}

const CASES: [Case; 4] = [
    Case {
        file: "project-high-entropy-medium.bin",
        len: 65_536,
        block_bytes: 65_540,
        blocks: 1,
    },
    Case {
        file: "project-already-compressed-medium.bin",
        len: 65_536,
        block_bytes: 65_540,
        blocks: 1,
    },
    Case {
        file: "project-high-entropy-medium.bin",
        len: 1_048_576,
        block_bytes: 1_048_640,
        blocks: 16,
    },
    Case {
        file: "project-already-compressed-medium.bin",
        len: 1_048_576,
        block_bytes: 1_048_640,
        blocks: 16,
    },
];

/// Where the corpus cache sits, which the corpus tooling names the same way.
fn cache() -> PathBuf {
    env::var("CORPUS")
        .map_or_else(
            |_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join("corpus")
            },
            PathBuf::from,
        )
        .join("cache")
}

const fn header() -> FrameHeader {
    FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    )
}

/// One input encoded through one mode, with 64 KiB blocks and input chunks
/// of `in_chunk` bytes.
fn encode(data: &[u8], in_chunk: usize, balanced: bool) -> Result<Vec<u8>, Error> {
    let mut encoder = if balanced {
        Encoder::balanced_with_layout(header(), data.len(), 65_536)?
    } else {
        Encoder::with_layout(header(), data.len(), 65_536)?
    };
    let mut stream = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut at = 0_usize;
    while at < data.len() {
        let end = at.saturating_add(in_chunk).min(data.len());
        let mut fed = at;
        while fed < end {
            let chunk = data.get(fed..end).ok_or(Error::InvalidParameter)?;
            let progress = encoder.encode(chunk, &mut room)?;
            stream.extend_from_slice(
                room.get(..progress.produced)
                    .ok_or(Error::InvalidParameter)?,
            );
            assert!(
                progress.consumed > 0 || progress.produced > 0,
                "the encoder stalled"
            );
            fed = fed.saturating_add(progress.consumed);
        }
        at = end;
    }
    loop {
        let progress = encoder.finish(&mut room)?;
        stream.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        if progress.state == StreamState::Finished {
            break;
        }
        assert!(progress.produced > 0, "the encoder stalled while finishing");
    }
    Ok(stream)
}

fn decode_all(stream: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
    let mut out = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, &mut room)?;
        out.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            break;
        }
        assert!(
            progress.consumed > 0 || progress.produced > 0,
            "the decoder stalled"
        );
    }
    decoder.finish()?;
    Ok(out)
}

#[test]
fn balanced_raw_bytes_match_fast_and_the_recorded_blocks() -> Result<(), Error> {
    let cache = cache();
    if !cache.is_dir() {
        println!(
            "balanced-raw: {} does not hold the corpus, so nothing was measured",
            cache.display()
        );
        return Ok(());
    }
    for case in CASES {
        let path = cache.join(case.file);
        let Ok(whole) = std::fs::read(&path) else {
            println!(
                "balanced-raw: {} is not in the corpus, so nothing was measured",
                path.display()
            );
            return Ok(());
        };
        let data = whole
            .get(..case.len.min(whole.len()))
            .ok_or(Error::InvalidParameter)?;
        assert_eq!(
            data.len(),
            case.len,
            "{} holds {} bytes and the case reads {}",
            case.file,
            whole.len(),
            case.len
        );

        let fast = encode(data, data.len(), false)?;
        let balanced = encode(data, data.len(), true)?;
        assert_eq!(
            fast, balanced,
            "{} ({} bytes): BALANCED did not reproduce the FAST RAW stream",
            case.file, case.len
        );
        assert_eq!(
            balanced.len().saturating_sub(RAW_OVERHEAD),
            case.block_bytes,
            "{} ({} bytes): the RAW block bytes moved",
            case.file,
            case.len
        );
        // Every block is stored, and a RAW block carries a four-byte header,
        // so the pinned figure fixes the block count as well.
        assert_eq!(
            case.block_bytes,
            case.len.saturating_add(case.blocks.saturating_mul(4)),
            "{} ({} bytes): the recorded block bytes do not match {} RAW blocks",
            case.file,
            case.len,
            case.blocks
        );
        assert_eq!(
            decode_all(&balanced)?,
            data,
            "{}: the RAW stream did not decode to its input",
            case.file
        );
        assert_eq!(
            encode(data, 1_019, true)?,
            balanced,
            "{}: the BALANCED RAW bytes moved with the input chunking",
            case.file
        );
        println!(
            "balanced-raw: {} {} -> {} block bytes in {} blocks, identical to FAST",
            case.file, case.len, case.block_bytes, case.blocks
        );
    }
    Ok(())
}

/// The samples per arm the end-to-end figure reads.
const SAMPLES: usize = 7;

/// The median and the spread of one timing series, printed under its label.
///
/// The median is in picoseconds per byte, so a release build's
/// sub-nanosecond figure keeps its resolution. Spread is slowest minus
/// fastest over the median, the figure the repository compares against 0.15
/// before it trusts a timing.
#[allow(clippy::arithmetic_side_effects, clippy::cast_precision_loss)]
fn summarize(series: &mut [u128], label: &str) -> u128 {
    series.sort_unstable();
    let median = series.get(series.len() / 2).copied().unwrap_or(u128::MAX);
    let fastest = series.first().copied().unwrap_or(0);
    let slowest = series.last().copied().unwrap_or(0);
    let spread = slowest.saturating_sub(fastest) as f64 / median.max(1) as f64;
    println!(
        "balanced-raw cost {label}: median {median} ps/B, min {fastest}, max {slowest}, spread \
         {spread:.3}"
    );
    median
}

/// The second RAW gate term: BALANCED recovers FAST's order of magnitude on
/// the encode path.
///
/// The production encoders are measured end to end, on the same bytes, with
/// the arms alternating inside the sample loop so host drift lands on both.
/// The skip's whole point is cheapest on exactly this input, so a ratio above
/// a small multiple is a defect, not noise.
#[test]
fn balanced_raw_encode_cost_stays_inside_fast_order_of_magnitude() -> Result<(), Error> {
    let cache = cache();
    if !cache.is_dir() {
        println!(
            "balanced-raw: {} does not hold the corpus, so nothing was measured",
            cache.display()
        );
        return Ok(());
    }
    for case in CASES {
        let path = cache.join(case.file);
        let Ok(whole) = std::fs::read(&path) else {
            println!(
                "balanced-raw: {} is not in the corpus, so nothing was measured",
                path.display()
            );
            return Ok(());
        };
        let data = whole
            .get(..case.len.min(whole.len()))
            .ok_or(Error::InvalidParameter)?;
        assert_eq!(
            data.len(),
            case.len,
            "{} holds {} bytes",
            case.file,
            whole.len()
        );
        let mut fast_series = Vec::with_capacity(SAMPLES);
        let mut balanced_series = Vec::with_capacity(SAMPLES);
        let bytes = u128::try_from(data.len()).map_err(|_| Error::InvalidParameter)?;
        // One untimed pass per arm: the pages and the allocator caches warm
        // once, then the timed samples read steady state.
        for balanced in [false, true] {
            let _ = encode(data, data.len(), balanced)?;
        }
        for sample in 0..SAMPLES {
            let order = if sample % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for balanced in order {
                let started = std::time::Instant::now();
                let _ = encode(data, data.len(), balanced)?;
                // Picoseconds per byte, so a release build's sub-nanosecond
                // figure keeps its resolution instead of flooring to zero.
                let per_byte = started.elapsed().as_nanos().saturating_mul(1_000) / bytes;
                if balanced {
                    balanced_series.push(per_byte);
                } else {
                    fast_series.push(per_byte);
                }
            }
        }
        let fast = summarize(&mut fast_series, "FAST");
        let balanced = summarize(&mut balanced_series, "BALANCED");
        println!(
            "balanced-raw cost {} {}: FAST {fast} ps/B vs BALANCED {balanced} ps/B",
            case.file, case.len
        );
        assert!(
            balanced <= fast.saturating_mul(4),
            "{} ({} bytes): BALANCED costs {balanced} ps/B against FAST's {fast}, leaving its \
             order of magnitude",
            case.file,
            case.len
        );
    }
    Ok(())
}

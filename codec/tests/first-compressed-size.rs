//! The first compressed size this codec produces, against the figure it is read beside.
//!
//! The workload is eleven corpus entries, each loaded to the same 65 536-byte prefix and each
//! written as one version 1 frame at a block length of 65 536 input bytes. The figure beside
//! it was measured over the same entries, under the same representation and the same block
//! length, before this codec could produce one. Same content, same block length, same
//! representation: a codec figure far from it would mean a defect in one of the two.
//!
//! The two are not the same measurement and the test does not assert equality. The earlier
//! figure charged sixty-two parses from six parser families over these eleven entries; this
//! codec has one parse, the one it ships. What is comparable is the bytes a frame spends per
//! content byte, and the test pins the codec's own figure so that a later change has to say
//! why it moved.
//!
//! The corpus lives outside the repository and is materialized by the corpus tooling. A host
//! that does not hold it reports so and measures nothing, which is the one thing this file
//! does conditionally.

use std::env;
use std::path::PathBuf;

use codec::format::{
    DecoderPolicy, Error, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass,
};
use codec::stream::{Decoder, Encoder, StreamState};

/// The block length these figures are read at, which is also the prefix each entry holds.
const RUNG_BYTES: usize = 65_536;

/// The content bytes the eleven entries hold at this rung.
const CONTENT_BYTES: usize = 382_720;

/// The frame bytes this codec spends on them, recorded so that a later change states why it
/// moved. It is a pin and not a target.
///
/// The FAST operating point moved it from 42 603: the single-entry matcher with adaptive
/// skip finds nearer, shorter matches than the depth-8 chain, so structured entries spend
/// more bytes (source most, then json, logs, sparse) while high-entropy and RLE entries
/// spend what they spent. The per-entry deltas match the measured FAST trade. The FAST
/// table-log preference moved it from 49 256 to 49 179: capped tables spend fewer
/// description bytes on the structured entries.
const RECORDED_FRAME_BYTES: usize = 49_179;

/// The earlier figure at this rung: the frame bytes it spent and the content it spent them
/// over. Both are recorded figures, and neither is a target this codec is held to.
const EARLIER_FRAME_BYTES: u64 = 295_065;
const EARLIER_CONTENT_BYTES: u64 = 2_473_728;

/// The scale the two figures are compared on: frame bytes per hundred thousand content bytes,
/// which is exact in integers where a ratio is not.
const SCALE: u64 = 100_000;

/// The frame bytes `bytes` of content cost, per hundred thousand content bytes.
fn per_hundred_thousand(frame_bytes: usize, content: usize) -> u64 {
    let frame = u64::try_from(frame_bytes).unwrap_or(u64::MAX);
    let content = u64::try_from(content).unwrap_or(1).max(1);
    frame
        .saturating_mul(SCALE)
        .checked_div(content)
        .unwrap_or(0)
}

/// The entries, in the order the earlier measurement records them, with the bytes each holds
/// at this rung.
const ENTRIES: [(&str, usize); 11] = [
    ("project-high-entropy-tiny.bin", 256),
    ("project-zeros-tiny.bin", 512),
    ("project-json-small.json", 1_024),
    ("project-sparse-small.bin", 4_096),
    ("project-short-tokens-small.bin", 16_384),
    ("project-source-small.rs", 32_768),
    ("project-logs-medium.log", 65_536),
    ("project-json-medium.json", 65_536),
    ("project-source-medium.rs", 65_536),
    ("project-zeros-medium.bin", 65_536),
    ("project-long-repetitions-medium.bin", 65_536),
];

/// Where the corpus cache sits, which the corpus tooling names the same way.
///
/// `CORPUS` names it when it is set. Otherwise it is the default the repository's own tooling
/// takes, resolved from this package rather than from the working directory.
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
        ResourceClass::Minimal,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    )
}

/// One entry as one frame, at one region and one block of the rung, and the block types the
/// selection rule chose for it.
fn frame(data: &[u8]) -> Result<(Vec<u8>, [u64; 3]), Error> {
    let mut encoder = Encoder::with_layout(
        header(),
        RUNG_BYTES,
        u32::try_from(RUNG_BYTES).map_err(|_| Error::InvalidParameter)?,
    )?;
    let mut out = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut fed = 0_usize;
    while fed < data.len() {
        let rest = data.get(fed..).ok_or(Error::InvalidParameter)?;
        let progress = encoder.encode(rest, &mut room)?;
        out.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        fed = fed.saturating_add(progress.consumed);
        assert!(
            progress.consumed > 0 || progress.produced > 0,
            "the encoder stalled"
        );
    }
    loop {
        let progress = encoder.finish(&mut room)?;
        out.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        if progress.state == StreamState::Finished {
            break;
        }
    }
    let stats = encoder.statistics();
    Ok((
        out,
        [stats.raw_blocks, stats.rle_blocks, stats.compressed_blocks],
    ))
}

fn expand(stream: &[u8]) -> Result<Vec<u8>, Error> {
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
fn the_first_compressed_size_is_recorded_against_the_earlier_figure() -> Result<(), Error> {
    let cache = cache();
    if !cache.is_dir() {
        println!(
            "first-compressed-size: {} does not hold the corpus, so nothing was measured",
            cache.display()
        );
        return Ok(());
    }

    let mut content = 0_usize;
    let mut frame_bytes = 0_usize;
    let mut histogram = [0_u64; 3];
    for (name, bytes) in ENTRIES {
        let path = cache.join(name);
        let Ok(whole) = std::fs::read(&path) else {
            println!(
                "first-compressed-size: {} is not in the corpus, so nothing was measured",
                path.display()
            );
            return Ok(());
        };
        let data = whole
            .get(..bytes.min(whole.len()))
            .ok_or(Error::InvalidParameter)?;
        assert_eq!(
            data.len(),
            bytes,
            "{name} holds {} bytes and the workload reads {bytes}",
            whole.len()
        );
        let (stream, types) = frame(data)?;
        assert_eq!(
            expand(&stream)?,
            data,
            "{name} did not decode to its content"
        );
        let bound = header().raw_frame_bytes(
            u64::try_from(bytes).map_err(|_| Error::InvalidParameter)?,
            u64::try_from(RUNG_BYTES).map_err(|_| Error::InvalidParameter)?,
            u32::try_from(RUNG_BYTES).map_err(|_| Error::InvalidParameter)?,
        )?;
        assert!(
            u64::try_from(stream.len()).unwrap_or(u64::MAX) <= bound,
            "{name} spent {} bytes against a bound of {bound}",
            stream.len()
        );
        println!(
            "first-compressed-size: {name} {bytes} -> {} bytes, {} per hundred thousand, \
             {} raw {} rle {} compressed",
            stream.len(),
            per_hundred_thousand(stream.len(), bytes),
            types.first().copied().unwrap_or(0),
            types.get(1).copied().unwrap_or(0),
            types.get(2).copied().unwrap_or(0)
        );
        for (slot, count) in histogram.iter_mut().zip(types.iter()) {
            *slot = slot.saturating_add(*count);
        }
        content = content.saturating_add(bytes);
        frame_bytes = frame_bytes.saturating_add(stream.len());
    }

    let share = per_hundred_thousand(frame_bytes, content);
    let earlier = EARLIER_FRAME_BYTES
        .saturating_mul(SCALE)
        .checked_div(EARLIER_CONTENT_BYTES)
        .unwrap_or(0);
    println!(
        "first-compressed-size: {frame_bytes} frame bytes over {content} content bytes, \
         {share} per hundred thousand, against the earlier {earlier} over \
         {EARLIER_CONTENT_BYTES} bytes"
    );
    println!(
        "first-compressed-size: {} raw, {} rle, {} compressed",
        histogram.first().copied().unwrap_or(0),
        histogram.get(1).copied().unwrap_or(0),
        histogram.get(2).copied().unwrap_or(0)
    );
    assert_eq!(content, CONTENT_BYTES, "the workload changed");
    assert_eq!(
        frame_bytes, RECORDED_FRAME_BYTES,
        "the first compressed size moved from the figure this phase recorded"
    );
    Ok(())
}

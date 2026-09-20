//! The BALANCED byte oracle with the FAST skip.
//!
//! Pins the production chain32 + length-lazy1 + `FAST_SKIP` path to its recorded
//! oracle block bytes: the block bytes (headers + payloads) the frozen inputs
//! compress to, and the greedy control bytes on the same inputs. Every entry
//! is one region cut into 64 KiB blocks, so multi-block entries prove the
//! carried chain state, tables, and offset cache reproduce the oracle on
//! every block, not only in total.
//!
//! The no-skip values stay the mechanism-selection evidence for chain32 +
//! length-lazy1; they are not the production oracle. The accepted cost of the
//! skip on the frozen corpus is +3 B on logs, +59 B on serialized binary,
//! +67 B on long repetitions, 0 B on the remaining byte fixtures, with
//! database-rows sequence drift at byte identity.
//!
//! A mismatch stops here: the failure names the first divergent match
//! (position, expected vs produced length/distance) so the semantic
//! difference is classified before anything changes. Tuning around a
//! mismatch is forbidden. Sequence equality is always asserted, because byte
//! equality hides decision drift.
//!
//! The corpus lives outside the repository and is materialized by the corpus
//! tooling. A host that does not hold it reports so and measures nothing,
//! which is the one thing this file does conditionally.

use std::env;
use std::path::PathBuf;

use codec::format::{
    BlockHeader, BlockPrologue, BlockType, DecoderPolicy, Error, FrameHeader, IntegrityMode,
    Record, RegionIndependence, ResourceClass,
};
use codec::matchfinder::{BoundedHashChain, CHAIN_DEPTH_BALANCED};
use codec::parser::Parser;
use codec::sequence::{MIN_MATCH, Match, Sequences};
use codec::stream::{Decoder, Encoder, StreamState};

/// The input bytes one oracle block holds.
const BLOCK_BYTES: usize = 65_536;

/// The frame/region/terminator overhead a single-block input carries.
///
/// Block bytes are headers + payloads; the stream around one block adds
/// exactly this many bytes.
const SINGLE_BLOCK_OVERHEAD: usize = 34;

/// One frozen input with its recorded block bytes.
///
/// `greedy` is the chain32 + greedy control on the same bytes; `balanced`
/// is the chain32 + length-lazy1 oracle; `delta` is their difference.
struct Entry {
    file: &'static str,
    len: usize,
    greedy: usize,
    balanced: usize,
    delta: i64,
}

const ENTRIES: [Entry; 8] = [
    Entry {
        file: "project-source-medium.rs",
        len: 65_536,
        greedy: 2_938,
        balanced: 2_938,
        delta: 0,
    },
    Entry {
        file: "project-logs-medium.log",
        len: 65_536,
        greedy: 15_130,
        balanced: 14_169,
        delta: -961,
    },
    Entry {
        file: "project-json-medium.json",
        len: 65_536,
        greedy: 11_157,
        balanced: 10_523,
        delta: -634,
    },
    Entry {
        file: "project-database-rows-medium.tsv",
        len: 65_536,
        greedy: 26_216,
        balanced: 25_555,
        delta: -661,
    },
    Entry {
        file: "project-long-repetitions-medium.bin",
        len: 65_536,
        greedy: 4_405,
        balanced: 4_472,
        delta: 67,
    },
    Entry {
        file: "project-serialized-binary-medium.bin",
        len: 65_536,
        greedy: 48_407,
        balanced: 48_013,
        delta: -394,
    },
    Entry {
        file: "project-json-medium.json",
        len: 262_144,
        greedy: 42_573,
        balanced: 40_168,
        delta: -2_405,
    },
    Entry {
        file: "project-database-rows-medium.tsv",
        len: 262_144,
        greedy: 102_675,
        balanced: 100_135,
        delta: -2_540,
    },
];

/// Where the corpus cache sits, which the corpus tooling names the same way.
///
/// `CORPUS` names it when it is set. Otherwise it is the default the
/// repository's own tooling takes, resolved from this package rather than
/// from the working directory.
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

/// One entry encoded through the production BALANCED path.
///
/// One region, 64 KiB blocks: the same region/block state the oracle was
/// recorded under. `in_chunk` varies the input chunking; the bytes must not
/// move with it.
fn encode_balanced(data: &[u8], in_chunk: usize) -> Result<Vec<u8>, Error> {
    let block = u32::try_from(BLOCK_BYTES).map_err(|_| Error::InvalidParameter)?;
    let mut encoder = Encoder::balanced_with_layout(header(), data.len(), block)?;
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
                "the BALANCED encoder stalled"
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
        assert!(
            progress.produced > 0,
            "the BALANCED encoder stalled while finishing"
        );
    }
    Ok(stream)
}

fn decode_all(stream: &[u8], len: usize) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
    let mut out = Vec::with_capacity(len);
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

/// The block byte strings of a single-region stream: each header with the
/// payload it declares.
fn extract_blocks(stream: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let (frame, mut at) = FrameHeader::decode(stream)?;
    let (record, used) = Record::decode(
        stream.get(at..).ok_or(Error::InvalidParameter)?,
        frame.integrity,
    )?;
    at = at.saturating_add(used);
    let region = match record {
        Record::Region(region) => region,
        Record::Terminator => return Err(Error::InvalidParameter),
    };
    let physical = usize::try_from(region.physical_size).map_err(|_| Error::InvalidParameter)?;
    let region_bytes = stream
        .get(at..at.saturating_add(physical))
        .ok_or(Error::InvalidParameter)?;
    let mut blocks = Vec::new();
    let mut bat = 0_usize;
    let mut first = true;
    while bat < region_bytes.len() {
        let (block, hused) =
            BlockHeader::decode(region_bytes.get(bat..).ok_or(Error::InvalidParameter)?)?;
        let stored = match block.kind {
            BlockType::Raw | BlockType::Rle => {
                usize::try_from(block.stored_len().ok_or(Error::InvalidParameter)?)
                    .map_err(|_| Error::InvalidParameter)?
            }
            BlockType::Compressed => {
                let payload = region_bytes
                    .get(bat.saturating_add(hused)..)
                    .ok_or(Error::InvalidParameter)?;
                let (prologue, pused) = BlockPrologue::decode(
                    payload,
                    block.decoded_len(),
                    first,
                    &DecoderPolicy::CONSERVATIVE,
                )?;
                let body = usize::try_from(prologue.body_bytes().ok_or(Error::InvalidParameter)?)
                    .map_err(|_| Error::InvalidParameter)?;
                pused.saturating_add(body)
            }
        };
        let end = bat.saturating_add(hused).saturating_add(stored);
        blocks.push(
            region_bytes
                .get(bat..end)
                .ok_or(Error::InvalidParameter)?
                .to_vec(),
        );
        bat = end;
        first = false;
    }
    Ok(blocks)
}

/// The best chain32 candidate at `at`, capped at the block end.
///
/// A candidate the block end shortens below the minimum is no candidate:
/// the block holds no bytes past its end for the hash to read.
fn capped_peek(chain: &BoundedHashChain, data: &[u8], at: usize, end: usize) -> Option<Match> {
    let found = chain.peek(data, at).matched?;
    let room = u32::try_from(end.saturating_sub(at)).unwrap_or(u32::MAX);
    let length = found.length.min(room);
    if length >= MIN_MATCH {
        Some(Match {
            length,
            distance: found.distance,
        })
    } else {
        None
    }
}

/// The independent length-lazy depth-1 oracle with the FAST skip, over the
/// whole entry.
///
/// One chain32 carried across the entry's 64 KiB blocks inside a sliding
/// window staged exactly as the production parser stages it (three windows
/// of room, slide by one window when full), so chain coordinates, the match
/// cap at each block end, and the tail re-offer per the `insertable_end`
/// rule all agree with production. Consecutive searched misses advance by
/// `1 + (streak >> 6)` with the streak reset on a taken match and at each
/// block start; skipped positions are never searched nor inserted. Written
/// separately from the production parse so a byte mismatch has a second
/// implementation to diverge against, and so sequence drift that keeps the
/// bytes is still caught.
/// The shift ramps the skip one step per sixty-four consecutive misses.
// The division sizes one step per shortest match: the block holds at most
// 64 KiB, so the quotient and its successor cannot overflow. The length is the
// whole schedule: windowing, lazy lookahead, skip, and the block tail.
#[allow(clippy::arithmetic_side_effects, clippy::too_many_lines)]
fn oracle_blocks(data: &[u8]) -> Result<Vec<Sequences>, Error> {
    const WINDOW_BYTES: usize = 65_536;
    const CAPACITY: usize = WINDOW_BYTES * 3;
    let mut window = vec![0_u8; CAPACITY];
    let mut chain = BoundedHashChain::with_depth(CHAIN_DEPTH_BALANCED);
    let mut out = Vec::new();
    let mut filled = 0_usize;
    let mut inserted = 0_usize;
    let mut fed = 0_usize;
    while fed < data.len() {
        let len = data.len().saturating_sub(fed).min(BLOCK_BYTES);
        if filled.saturating_add(len) > CAPACITY {
            window.copy_within(WINDOW_BYTES..filled, 0);
            filled = filled.saturating_sub(WINDOW_BYTES);
            inserted = inserted.saturating_sub(WINDOW_BYTES);
            chain.slide();
        }
        let start = filled;
        window
            .get_mut(start..start.saturating_add(len))
            .ok_or(Error::InvalidParameter)?
            .copy_from_slice(
                data.get(fed..fed.saturating_add(len))
                    .ok_or(Error::InvalidParameter)?,
            );
        filled = filled.saturating_add(len);
        let end = filled;
        let view = window.get(..end).ok_or(Error::InvalidParameter)?;
        let mut sequences = Sequences::with_capacity(len, len / 4 + 1);
        let mut run_start = start;
        let mut at = start;
        let mut streak = 0_u32;
        let mut delayed = false;
        let mut pending: Option<Option<Match>> = None;
        while at < end {
            while inserted < at {
                chain.insert(view, inserted);
                inserted = inserted.saturating_add(1);
            }
            let current = pending
                .take()
                .unwrap_or_else(|| capped_peek(&chain, view, at, end));
            let Some(current) = current else {
                chain.insert(view, at);
                streak = streak.saturating_add(1);
                at = at
                    .saturating_add(
                        1_usize.saturating_add(usize::try_from(streak >> 6).unwrap_or(usize::MAX)),
                    )
                    .min(end);
                // Skipped positions stay out of the chain: the miss was searched and
                // inserted, the jumped-over positions are neither.
                inserted = at;
                delayed = false;
                continue;
            };
            if delayed {
                let run = view.get(run_start..at).ok_or(Error::InvalidParameter)?;
                sequences.push(run, Some(current))?;
                chain.insert(view, at);
                inserted = at.saturating_add(1);
                at = commit_covered(&mut chain, view, at, current, end, &mut inserted);
                run_start = at;
                streak = 0;
                delayed = false;
                continue;
            }
            chain.insert(view, at);
            inserted = at.saturating_add(1);
            let next = if at.saturating_add(1) < end {
                capped_peek(&chain, view, at.saturating_add(1), end)
            } else {
                None
            };
            let Some(next) = next else {
                let run = view.get(run_start..at).ok_or(Error::InvalidParameter)?;
                sequences.push(run, Some(current))?;
                at = commit_covered(&mut chain, view, at, current, end, &mut inserted);
                run_start = at;
                streak = 0;
                delayed = false;
                continue;
            };
            if next.length > current.length {
                at = at.saturating_add(1);
                pending = Some(Some(next));
                delayed = true;
                continue;
            }
            let run = view.get(run_start..at).ok_or(Error::InvalidParameter)?;
            sequences.push(run, Some(current))?;
            at = commit_covered(&mut chain, view, at, current, end, &mut inserted);
            run_start = at;
            streak = 0;
            delayed = false;
        }
        if run_start < end {
            let run = view.get(run_start..end).ok_or(Error::InvalidParameter)?;
            sequences.push(run, None)?;
        }
        inserted = inserted.min(BoundedHashChain::insertable_end(end));
        out.push(sequences);
        fed = fed.saturating_add(len);
    }
    Ok(out)
}

/// Inserts the taken match's covered positions and answers where it ends.
///
/// Positions before the end are inserted exactly once; the tail the hash
/// cannot read is left to the caller's re-offer rule.
fn commit_covered(
    chain: &mut BoundedHashChain,
    data: &[u8],
    at: usize,
    current: Match,
    end: usize,
    inserted: &mut usize,
) -> usize {
    let take_end = at
        .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
        .min(end);
    while *inserted < take_end {
        chain.insert(data, *inserted);
        *inserted = inserted.saturating_add(1);
    }
    take_end
}

/// The production BALANCED sequences over the entry's 64 KiB blocks.
///
/// One skip-carrying parser fed block by block, so its window history
/// carries exactly as the encoder's does.
fn production_blocks(data: &[u8]) -> Result<Vec<Sequences>, Error> {
    let mut parser = Parser::balanced();
    let mut out = Vec::new();
    let mut at = 0_usize;
    while at < data.len() {
        let end = at.saturating_add(BLOCK_BYTES).min(data.len());
        let chunk = data.get(at..end).ok_or(Error::InvalidParameter)?;
        out.push(parser.parse(chunk)?.clone());
        at = end;
    }
    Ok(out)
}

/// The first position where the oracle and production sequences part ways.
///
/// Answers `None` when every block agrees. Otherwise names the block, the
/// step, the input position, and the expected vs produced run and match, so
/// a byte mismatch classifies to a hash, tie-break, insertion, assembly, or
/// reuse difference before anything changes.
fn first_divergence(expected: &[Sequences], produced: &[Sequences]) -> Option<String> {
    if expected.len() != produced.len() {
        return Some(format!(
            "block count differs: oracle {} vs production {}",
            expected.len(),
            produced.len()
        ));
    }
    for (block, (want, got)) in expected.iter().zip(produced.iter()).enumerate() {
        if want.literals() != got.literals() {
            return Some(format!(
                "block {block}: literal bytes differ ({} vs {} bytes)",
                want.literals().len(),
                got.literals().len()
            ));
        }
        if want.steps().len() != got.steps().len() {
            return Some(format!(
                "block {block}: step count differs: oracle {} vs production {}",
                want.steps().len(),
                got.steps().len()
            ));
        }
        let mut position = block.saturating_mul(BLOCK_BYTES);
        for (step, (want, got)) in want.steps().iter().zip(got.steps().iter()).enumerate() {
            if want == got {
                position = position.saturating_add(usize::try_from(want.run).unwrap_or(usize::MAX));
                if let Some(matched) = want.matched {
                    position = position
                        .saturating_add(usize::try_from(matched.length).unwrap_or(usize::MAX));
                }
                continue;
            }
            return Some(format!(
                "block {block} step {step} at {position}: oracle run {} len {} dist {} \
                 vs production run {} len {} dist {}",
                want.run,
                want.matched.map_or(0, |matched| matched.length),
                want.matched.map_or(0, |matched| matched.distance),
                got.run,
                got.matched.map_or(0, |matched| matched.length),
                got.matched.map_or(0, |matched| matched.distance),
            ));
        }
    }
    None
}

#[test]
fn balanced_block_bytes_match_the_fast_skip_oracle() -> Result<(), Error> {
    let cache = cache();
    if !cache.is_dir() {
        println!(
            "byte-oracle: {} does not hold the corpus, so nothing was measured",
            cache.display()
        );
        return Ok(());
    }
    for entry in ENTRIES {
        let path = cache.join(entry.file);
        let Ok(whole) = std::fs::read(&path) else {
            println!(
                "byte-oracle: {} is not in the corpus, so nothing was measured",
                path.display()
            );
            return Ok(());
        };
        let data = whole
            .get(..entry.len.min(whole.len()))
            .ok_or(Error::InvalidParameter)?;
        assert_eq!(
            data.len(),
            entry.len,
            "{} holds {} bytes and the oracle reads {}",
            entry.file,
            whole.len(),
            entry.len
        );

        let stream = encode_balanced(data, data.len())?;
        let blocks = extract_blocks(&stream)?;
        let total: usize = blocks.iter().map(Vec::len).sum();
        // Sequence equality is asserted on every entry, not only when the
        // bytes move: database-rows keeps the bytes while drifting the
        // decisions, so bytes alone cannot pin the operating point.
        let sequences = oracle_blocks(data)?;
        let produced = production_blocks(data)?;
        let divergence = first_divergence(&sequences, &produced);
        assert!(
            divergence.is_none(),
            "{} ({} bytes): the FAST_SKIP sequences moved: {}",
            entry.file,
            entry.len,
            divergence.unwrap_or_default()
        );
        if total != entry.balanced {
            let oracle = oracle_blocks(data).map_or_else(
                |error| format!("the oracle itself failed: {error:?}"),
                |sequences| {
                    first_divergence(&sequences, &production_blocks(data).unwrap_or_default())
                        .unwrap_or_else(|| {
                            "sequences agree; the difference is in assembly".to_string()
                        })
                },
            );
            assert_eq!(
                total, entry.balanced,
                "{} ({} bytes): oracle {} vs production {total} block bytes; {oracle}",
                entry.file, entry.len, entry.balanced,
            );
        }
        let measured = i64::try_from(total)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(entry.greedy).unwrap_or(0));
        assert_eq!(
            measured, entry.delta,
            "{}: production {total} vs control {} is {measured}, oracle delta {}",
            entry.file, entry.greedy, entry.delta,
        );
        if entry.len == BLOCK_BYTES {
            assert_eq!(
                stream.len(),
                total.saturating_add(SINGLE_BLOCK_OVERHEAD),
                "{}: single-block overhead moved",
                entry.file,
            );
        }
        assert_eq!(
            decode_all(&stream, data.len())?,
            data,
            "{}: an oracle block did not decode through the production decoder",
            entry.file,
        );
        assert_eq!(
            encode_balanced(data, 1_019)?,
            stream,
            "{}: the BALANCED bytes moved with the input chunking",
            entry.file,
        );
        println!(
            "byte-oracle: {} {} -> {total} block bytes (control {}, delta {measured}) \
             in {} blocks",
            entry.file,
            entry.len,
            entry.greedy,
            blocks.len(),
        );
    }
    Ok(())
}

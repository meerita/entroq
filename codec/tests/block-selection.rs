//! The block selection rule, the type every block declares, and the version 1 expansion bound.
//!
//! Every property here is read through the public streaming API, because the rule is a
//! property of the frames a caller gets and not of the pieces that assembled them. The frame
//! walk is the exception in the other direction: it reads structure only, and proves that the
//! type of every block is knowable without decoding a payload.

use codec::format::{
    BLOCK_HEADER_BYTES, BlockHeader, BlockPrologue, BlockType, Corruption, DecoderPolicy, Error,
    FrameHeader, IntegrityMode, Record, RegionIndependence, ResourceClass,
};
use codec::stream::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Decoder, Encoder, StreamState};

/// The layout every figure below is taken at.
const REGION_BYTES: usize = 262_144;
const BLOCK_BYTES: u32 = 16_384;

/// The input classes the rule is read over.
#[derive(Clone, Copy, Debug)]
enum Class {
    Zeros,
    OneByte,
    Incompressible,
    Repetitive,
    Text,
    Sparse,
    NearWindow,
}

impl Class {
    const ALL: [Self; 7] = [
        Self::Zeros,
        Self::OneByte,
        Self::Incompressible,
        Self::Repetitive,
        Self::Text,
        Self::Sparse,
        Self::NearWindow,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Zeros => "zeros",
            Self::OneByte => "one-byte",
            Self::Incompressible => "incompressible",
            Self::Repetitive => "repetitive",
            Self::Text => "text",
            Self::Sparse => "sparse",
            Self::NearWindow => "near-window",
        }
    }

    fn content(self, len: usize) -> Vec<u8> {
        let mut out = vec![0_u8; len];
        for (at, slot) in out.iter_mut().enumerate() {
            let position = u64::try_from(at).unwrap_or(0);
            *slot = match self {
                Self::Zeros => 0,
                Self::OneByte => 0xA5,
                Self::Incompressible => mix(position),
                Self::Repetitive => {
                    let phrase = b"the quick brown fox jumps over the lazy dog. ";
                    phrase
                        .get(at.checked_rem(phrase.len()).unwrap_or(0))
                        .copied()
                        .unwrap_or(b' ')
                }
                Self::Text => {
                    let pick = mix(position.wrapping_mul(3)) & 0x0F;
                    b"abcdefghijklmnop"
                        .get(usize::from(pick))
                        .copied()
                        .unwrap_or(b'a')
                }
                Self::Sparse => {
                    if at.checked_rem(64) == Some(0) {
                        mix(position)
                    } else {
                        0
                    }
                }
                // A phrase that recurs just inside the window, which is the farthest a match
                // may reach and therefore the distance a region's history has to hold.
                Self::NearWindow => mix(position.checked_rem(65_413).unwrap_or(0)),
            };
        }
        out
    }
}

/// A value that depends on its position and on nothing else.
fn mix(position: u64) -> u8 {
    let mut state = position.wrapping_add(0x9E37_79B9_7F4A_7C15);
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    u8::try_from((state ^ (state >> 31)).wrapping_shr(24) & 0xFF).unwrap_or(0)
}

const fn permissive() -> DecoderPolicy {
    DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes())
}

const fn plain_header() -> FrameHeader {
    FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    )
}

/// What one encoded frame cost, and what the encoder recorded about it.
struct Encoded {
    stream: Vec<u8>,
    blocks: [u64; 3],
    table_limited: u64,
}

fn encode(
    header: FrameHeader,
    region_bytes: usize,
    block_bytes: u32,
    data: &[u8],
    in_chunk: usize,
    out_chunk: usize,
) -> Result<Encoded, Error> {
    let mut encoder = Encoder::with_layout(header, region_bytes, block_bytes)?;
    let mut stream = Vec::new();
    let mut room = vec![0_u8; out_chunk];
    let mut at = 0_usize;
    while at < data.len() {
        let end = at.saturating_add(in_chunk).min(data.len());
        let chunk = data.get(at..end).ok_or(Error::InvalidParameter)?;
        let mut fed = 0_usize;
        while fed < chunk.len() {
            let rest = chunk.get(fed..).ok_or(Error::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut room)?;
            keep(&mut stream, &room, progress.produced)?;
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
        keep(&mut stream, &room, progress.produced)?;
        if progress.state == StreamState::Finished {
            break;
        }
        assert!(progress.produced > 0, "the encoder stalled while finishing");
    }
    let stats = encoder.statistics();
    assert_eq!(
        stats.blocks,
        stats
            .raw_blocks
            .saturating_add(stats.rle_blocks)
            .saturating_add(stats.compressed_blocks),
        "a block was emitted under no type"
    );
    assert_eq!(
        stats.input_bytes,
        u64::try_from(data.len()).unwrap_or(u64::MAX),
        "the blocks did not cover the input"
    );
    Ok(Encoded {
        stream,
        blocks: [stats.raw_blocks, stats.rle_blocks, stats.compressed_blocks],
        table_limited: stats.table_limited_blocks,
    })
}

fn decode(
    policy: DecoderPolicy,
    stream: &[u8],
    in_chunk: usize,
    out_chunk: usize,
) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(policy);
    let mut out = Vec::new();
    let mut room = vec![0_u8; out_chunk];
    let mut at = 0_usize;
    while at < stream.len() {
        let end = at.saturating_add(in_chunk).min(stream.len());
        let chunk = stream.get(at..end).ok_or(Error::InvalidParameter)?;
        let progress = decoder.decode(chunk, &mut room)?;
        keep(&mut out, &room, progress.produced)?;
        assert!(
            progress.consumed > 0 || progress.produced > 0,
            "the decoder stalled"
        );
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            break;
        }
    }
    loop {
        let progress = decoder.decode(&[], &mut room)?;
        keep(&mut out, &room, progress.produced)?;
        if progress.produced == 0 {
            break;
        }
    }
    decoder.finish()?;
    // The tables in force cross the blocks of a region and the tables of one block are held
    // while it decodes, so the figure the ceiling has to cover is the widest set held at once
    // and not the set any one block declared.
    assert!(
        decoder.peak_table_bytes() <= policy.max_table_bytes(),
        "the decoder held {} table bytes against a ceiling of {}",
        decoder.peak_table_bytes(),
        policy.max_table_bytes()
    );
    Ok(out)
}

fn keep(sink: &mut Vec<u8>, room: &[u8], produced: usize) -> Result<(), Error> {
    let written = room.get(..produced).ok_or(Error::InvalidParameter)?;
    sink.extend_from_slice(written);
    Ok(())
}

/// Walks a frame's structure and reports how many blocks of each type it carries.
///
/// Nothing here expands a payload. A RAW block and an RLE block state their stored length in
/// the block header; a COMPRESSED block states it in the twelve extents of its own prologue,
/// which is structure and not content. So the type of every block is knowable to a reader that
/// decodes nothing, which is what makes the fallback visible.
fn walk(stream: &[u8]) -> Result<[u64; 3], Error> {
    let (header, mut at) = FrameHeader::decode(stream)?;
    let mut counts = [0_u64; 3];
    loop {
        let rest = stream.get(at..).ok_or(Error::InvalidParameter)?;
        let (record, used) = Record::decode(rest, header.integrity)?;
        at = at.saturating_add(used);
        let region = match record {
            Record::Terminator => break,
            Record::Region(region) => region,
        };
        let mut left = region.physical_size;
        let mut first_in_region = true;
        while left > 0 {
            let rest = stream.get(at..).ok_or(Error::InvalidParameter)?;
            let (block, used) = BlockHeader::decode(rest)?;
            at = at.saturating_add(used);
            left = left
                .checked_sub(u64::try_from(used).unwrap_or(u64::MAX))
                .ok_or(Error::InvalidParameter)?;
            let stored = match block.kind {
                BlockType::Raw => {
                    counts[0] = counts[0].saturating_add(1);
                    u64::from(block.decoded_len())
                }
                BlockType::Rle => {
                    counts[1] = counts[1].saturating_add(1);
                    1
                }
                BlockType::Compressed => {
                    counts[2] = counts[2].saturating_add(1);
                    let body = stream.get(at..).ok_or(Error::InvalidParameter)?;
                    let (prologue, prologue_bytes) = BlockPrologue::decode(
                        body,
                        block.decoded_len(),
                        first_in_region,
                        &permissive(),
                    )?;
                    u64::try_from(prologue_bytes)
                        .ok()
                        .and_then(|bytes| bytes.checked_add(prologue.body_bytes()?))
                        .ok_or(Error::InvalidParameter)?
                }
            };
            at = at.saturating_add(usize::try_from(stored).map_err(|_| Error::InvalidParameter)?);
            left = left.checked_sub(stored).ok_or(Error::InvalidParameter)?;
            first_in_region = false;
            if block.last {
                assert_eq!(left, 0, "the last block did not spend the region");
            }
        }
    }
    assert_eq!(at, stream.len(), "the walk did not reach the frame's end");
    Ok(counts)
}

#[test]
fn the_round_trip_is_exact_over_every_input_class_at_every_chunk_size() -> Result<(), Error> {
    let chunks = [
        (1_usize, 1_usize),
        (7, 3),
        (4_096, 512),
        (usize::MAX, usize::MAX),
    ];
    let mut spent = 0_usize;
    for class in Class::ALL {
        for len in [
            0_usize,
            1,
            2,
            BLOCK_BYTES as usize - 1,
            BLOCK_BYTES as usize + 1,
        ] {
            let data = class.content(len);
            for (in_chunk, out_chunk) in chunks {
                let in_chunk = in_chunk.min(data.len().max(1));
                let out_chunk = out_chunk.clamp(1, 4_096);
                let encoded = encode(
                    plain_header(),
                    REGION_BYTES,
                    BLOCK_BYTES,
                    &data,
                    in_chunk,
                    out_chunk,
                )?;
                let back = decode(permissive(), &encoded.stream, in_chunk, out_chunk)?;
                assert_eq!(
                    back,
                    data,
                    "class {} at {len} bytes, chunks {in_chunk}/{out_chunk}",
                    class.name()
                );
                spent = spent.saturating_add(len);
            }
        }
    }
    assert!(spent < 1_048_576, "the class set spent {spent} bytes");
    Ok(())
}

/// A region is more than one block, a match reaches back into the blocks before it, and every
/// byte still arrives.
#[test]
fn a_region_of_many_blocks_round_trips_with_matches_that_cross_them() -> Result<(), Error> {
    for class in Class::ALL {
        let data = class.content(200_000);
        let encoded = encode(
            plain_header(),
            REGION_BYTES,
            BLOCK_BYTES,
            &data,
            65_536,
            8_192,
        )?;
        assert!(
            encoded.blocks.iter().sum::<u64>() >= 12,
            "class {} produced too few blocks to cross one",
            class.name()
        );
        let back = decode(permissive(), &encoded.stream, 9_973, 8_192)?;
        assert_eq!(back, data, "class {}", class.name());
    }
    Ok(())
}

/// The rule reaches every type, and the type it chose is the one the frame declares.
#[test]
fn the_encoder_emits_each_type_and_declares_the_one_it_chose() -> Result<(), Error> {
    let expected = [
        (Class::Zeros, 1_usize),
        (Class::Incompressible, 0),
        (Class::Repetitive, 2),
    ];
    for (class, want) in expected {
        let data = class.content(100_000);
        let encoded = encode(
            plain_header(),
            REGION_BYTES,
            BLOCK_BYTES,
            &data,
            65_536,
            4_096,
        )?;
        let total: u64 = encoded.blocks.iter().sum();
        assert_eq!(
            encoded.blocks.get(want).copied().unwrap_or(0),
            total,
            "class {} did not emit every block under the type its content earns: {:?}",
            class.name(),
            encoded.blocks
        );
        assert_eq!(encoded.table_limited, 0);
        assert_eq!(
            walk(&encoded.stream)?,
            encoded.blocks,
            "the frame walk disagreed with what the encoder recorded, class {}",
            class.name()
        );
    }
    Ok(())
}

/// A frame walk reports the type of every block, and agrees with the encoder on every class.
#[test]
fn a_frame_walk_reports_every_block_type_without_decoding_a_payload() -> Result<(), Error> {
    for class in Class::ALL {
        for len in [1_usize, 5_000, 100_000, 300_000] {
            let data = class.content(len);
            let encoded = encode(
                plain_header(),
                REGION_BYTES,
                BLOCK_BYTES,
                &data,
                65_536,
                4_096,
            )?;
            assert_eq!(
                walk(&encoded.stream)?,
                encoded.blocks,
                "class {} at {len} bytes",
                class.name()
            );
        }
    }
    Ok(())
}

/// No frame this encoder emits is above the version 1 expansion bound, at the sizes that
/// stress it.
#[test]
fn no_emitted_frame_exceeds_the_version_one_expansion_bound() -> Result<(), Error> {
    let header = plain_header();
    let region = 4_096_usize;
    let block = 1_024_u32;
    let span = usize::try_from(block).map_err(|_| Error::InvalidParameter)?;
    let sizes = [
        0_usize,
        1,
        2,
        span.saturating_sub(1),
        span,
        span.saturating_add(1),
        region.saturating_sub(1),
        region,
        region.saturating_add(1),
        region.saturating_mul(2).saturating_add(3),
    ];
    for class in Class::ALL {
        for len in sizes {
            let data = class.content(len);
            let encoded = encode(header, region, block, &data, 4_096, 4_096)?;
            let bound = header.raw_frame_bytes(
                u64::try_from(len).map_err(|_| Error::InvalidParameter)?,
                u64::try_from(region).map_err(|_| Error::InvalidParameter)?,
                block,
            )?;
            assert!(
                u64::try_from(encoded.stream.len()).unwrap_or(u64::MAX) <= bound,
                "class {} at {len} bytes emitted {} bytes against a bound of {bound}",
                class.name(),
                encoded.stream.len()
            );
        }
    }
    Ok(())
}

/// A COMPRESSED block whose stored payload is not below its decoded size is refused, and the
/// refusal happens before the payload is read.
#[test]
fn a_compressed_block_that_stores_its_decoded_size_is_refused() -> Result<(), Error> {
    let data = Class::Repetitive.content(40_000);
    let encoded = encode(
        plain_header(),
        REGION_BYTES,
        BLOCK_BYTES,
        &data,
        65_536,
        4_096,
    )?;
    assert!(
        encoded.blocks.get(2).copied().unwrap_or(0) > 0,
        "the frame carries no COMPRESSED block to edit"
    );

    // The block header declares the decoded size for every type, so lowering it to the stored
    // length is what makes an otherwise well-formed block one the bound refuses.
    let (header, frame_bytes) = FrameHeader::decode(&encoded.stream)?;
    let (record, record_bytes) = Record::decode(
        encoded
            .stream
            .get(frame_bytes..)
            .ok_or(Error::InvalidParameter)?,
        header.integrity,
    )?;
    assert!(matches!(record, Record::Region(_)));
    let at = frame_bytes.saturating_add(record_bytes);
    let (block, _used) =
        BlockHeader::decode(encoded.stream.get(at..).ok_or(Error::InvalidParameter)?)?;
    assert_eq!(block.kind, BlockType::Compressed);

    // The decoded size a header declares is what the bound compares the stored length
    // against, so declaring exactly the stored length is the first size the bound refuses.
    let body = encoded
        .stream
        .get(at.saturating_add(BLOCK_HEADER_BYTES)..)
        .ok_or(Error::InvalidParameter)?;
    let (prologue, prologue_bytes) =
        BlockPrologue::decode(body, block.decoded_len(), true, &permissive())?;
    let stored = u64::try_from(prologue_bytes)
        .ok()
        .and_then(|bytes| bytes.checked_add(prologue.body_bytes()?))
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or(Error::InvalidParameter)?;
    for index in 0..4 {
        let declared = prologue.stream(index).ok_or(Error::InvalidParameter)?.count;
        assert!(
            declared <= u64::from(stored),
            "the edited size would be refused for its symbol counts before the bound is read"
        );
    }

    let mut edited = encoded.stream.clone();
    let shrunk = BlockHeader::compressed(block.last, stored)?;
    let bytes = shrunk.encode();
    let slot = edited
        .get_mut(at..at.saturating_add(BLOCK_HEADER_BYTES))
        .ok_or(Error::InvalidParameter)?;
    slot.copy_from_slice(bytes.as_bytes());

    let mut decoder = Decoder::new(permissive());
    let mut room = [0_u8; 4_096];
    let refused = decoder.decode(&edited, &mut room);
    assert_eq!(
        refused,
        Err(Error::CorruptData(Corruption::BlockExpansion)),
        "a block storing at least its decoded size was not refused"
    );
    Ok(())
}

/// A COMPRESSED block above the decoded size a policy admits is refused before it is read.
#[test]
fn a_compressed_block_above_the_policy_block_size_is_refused() -> Result<(), Error> {
    let data = Class::Repetitive.content(40_000);
    let encoded = encode(
        plain_header(),
        REGION_BYTES,
        BLOCK_BYTES,
        &data,
        65_536,
        4_096,
    )?;
    let strict = permissive().with_max_block_bytes(1_024);
    let mut decoder = Decoder::new(strict);
    let mut room = [0_u8; 4_096];
    assert_eq!(
        decoder.decode(&encoded.stream, &mut room),
        Err(Error::LimitExceeded {
            declared: 16_384,
            allowed: 1_024,
        })
    );
    Ok(())
}

/// The default layout carries the same properties as the small one the figures above use.
#[test]
fn the_default_layout_round_trips_and_declares_every_block_type() -> Result<(), Error> {
    let data = Class::Text.content(150_000);
    let encoded = encode(
        plain_header(),
        DEFAULT_REGION_BYTES,
        DEFAULT_BLOCK_BYTES,
        &data,
        32_768,
        16_384,
    )?;
    assert_eq!(walk(&encoded.stream)?, encoded.blocks);
    assert_eq!(decode(permissive(), &encoded.stream, 8_192, 8_192)?, data);
    assert!(
        encoded.stream.len() < data.len(),
        "the default layout stored {} bytes for {} of content",
        encoded.stream.len(),
        data.len()
    );
    Ok(())
}

/// A COMPRESSED block whose matches reach back into a RAW block of the same region.
///
/// The bytes a match may name are the bytes the region produced, whatever type carried them.
/// A decoder that kept only the content of the blocks it expanded would decode this frame to
/// something else.
#[test]
fn a_compressed_block_reaches_back_into_a_raw_block_of_its_region() -> Result<(), Error> {
    let span = usize::try_from(BLOCK_BYTES).map_err(|_| Error::InvalidParameter)?;
    let seed = Class::Incompressible.content(span);
    let mut data = Vec::with_capacity(span.saturating_mul(5));
    for _ in 0..5 {
        data.extend_from_slice(&seed);
    }

    let encoded = encode(
        plain_header(),
        REGION_BYTES,
        BLOCK_BYTES,
        &data,
        65_536,
        4_096,
    )?;
    assert_eq!(
        encoded.blocks,
        [1, 0, 4],
        "the region did not carry one RAW block and four COMPRESSED ones"
    );
    assert_eq!(walk(&encoded.stream)?, encoded.blocks);
    for (in_chunk, out_chunk) in [(1_usize, 1_usize), (37, 11), (65_536, 4_096)] {
        assert_eq!(
            decode(permissive(), &encoded.stream, in_chunk, out_chunk)?,
            data,
            "chunks {in_chunk}/{out_chunk}"
        );
    }
    Ok(())
}

/// A policy that admits no COMPRESSED block holds no buffers for one and refuses every one.
#[test]
fn a_policy_that_admits_no_compressed_block_holds_no_buffers_for_one() -> Result<(), Error> {
    let raw_only = permissive().with_max_block_bytes(0);
    let bare = Decoder::new(raw_only);
    let ready = Decoder::new(permissive());
    assert!(
        bare.steady_state_bytes() < ready.steady_state_bytes(),
        "a decoder that admits no compressed block held {} bytes against {}",
        bare.steady_state_bytes(),
        ready.steady_state_bytes()
    );

    let plain = Class::Incompressible.content(40_000);
    let encoded = encode(
        plain_header(),
        REGION_BYTES,
        BLOCK_BYTES,
        &plain,
        65_536,
        4_096,
    )?;
    assert_eq!(encoded.blocks.get(2).copied().unwrap_or(0), 0);
    assert_eq!(decode(raw_only, &encoded.stream, 4_096, 4_096)?, plain);

    let dense = Class::Repetitive.content(40_000);
    let compressed = encode(
        plain_header(),
        REGION_BYTES,
        BLOCK_BYTES,
        &dense,
        65_536,
        4_096,
    )?;
    assert!(compressed.blocks.get(2).copied().unwrap_or(0) > 0);
    let mut decoder = Decoder::new(raw_only);
    let mut room = [0_u8; 4_096];
    assert_eq!(
        decoder.decode(&compressed.stream, &mut room),
        Err(Error::LimitExceeded {
            declared: 16_384,
            allowed: 0,
        })
    );
    Ok(())
}

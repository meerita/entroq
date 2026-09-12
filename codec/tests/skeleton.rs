//! The gate-scale proofs of the format skeleton.
//!
//! Every test here is ignored by default. The dev tier proves the same properties on a small
//! input set from inside the crate; these run once, at the gate tier, one test per segment of
//! the closing campaign, over an input set sized to that tier's budget.
//!
//! This file owns no codec behavior. It drives the public API and nothing else.

use codec::format::{
    BlockHeader, DecoderPolicy, Error, FrameHeader, IntegrityMode, Record, RegionHeader,
    RegionIndependence, ResourceClass,
};
use codec::stream::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Decoder, Encoder, StreamState};

/// The layout the structural fixtures are built on.
///
/// The sizes are small so one fixture holds several regions and several blocks, which is what
/// puts every structural boundary inside a stream short enough to cut at every offset.
const FIXTURE_REGION_BYTES: usize = 64;
const FIXTURE_BLOCK_BYTES: u32 = 16;

/// The seed every pseudo-random chunking is drawn from.
///
/// It is a constant rather than a clock reading, so a failure reproduces from the test name
/// alone.
const CHUNK_SEED: u64 = 0x5EED_0000_0000_0001;

/// The content classes the round trips cover.
///
/// Each one stresses a different part of the skeleton: a constant run is what an RLE block
/// exists for, mixed content is what a RAW block costs its expansion bound on, and a run
/// length that divides no block size evenly starts a repeat at a different offset in every
/// block.
#[derive(Clone, Copy, Debug)]
enum Shape {
    Zeros,
    OneByte,
    Mixed,
    Runs,
    TextLike,
}

const SHAPES: [Shape; 5] = [
    Shape::Zeros,
    Shape::OneByte,
    Shape::Mixed,
    Shape::Runs,
    Shape::TextLike,
];

const INTEGRITY: [IntegrityMode; 2] = [IntegrityMode::Absent, IntegrityMode::PerRegion];

impl Shape {
    const fn name(self) -> &'static str {
        match self {
            Self::Zeros => "zeros",
            Self::OneByte => "one-byte",
            Self::Mixed => "mixed",
            Self::Runs => "runs",
            Self::TextLike => "text-like",
        }
    }

    /// The byte this shape holds at a position, which does not depend on how the content is
    /// cut into chunks.
    fn at(self, position: u64) -> u8 {
        match self {
            Self::Zeros => 0,
            Self::OneByte => 0xA5,
            Self::Mixed => mix(position),
            Self::Runs => mix(position.checked_div(97).unwrap_or(0)),
            Self::TextLike => {
                const ALPHABET: &[u8; 16] = b"the quick brown ";
                let index = usize::try_from(position & 0x0F).unwrap_or(0);
                ALPHABET.get(index).copied().unwrap_or(b' ')
            }
        }
    }

    fn content(self, len: usize) -> Vec<u8> {
        let mut data = vec![0_u8; len];
        for (offset, slot) in data.iter_mut().enumerate() {
            *slot = self.at(u64::try_from(offset).unwrap_or(0));
        }
        data
    }
}

fn mix(position: u64) -> u8 {
    let mixed = position.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    u8::try_from(mixed.wrapping_shr(56)).unwrap_or(0)
}

/// The member of a rotating set an index lands on.
fn pick<T: Copy>(items: &[T], index: usize) -> Option<T> {
    items.get(index.checked_rem(items.len())?).copied()
}

/// Whether an index is the first of every `step`.
fn every(index: usize, step: usize) -> bool {
    index.checked_rem(step) == Some(0)
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

fn encode_all(
    header: FrameHeader,
    region_bytes: usize,
    block_bytes: u32,
    data: &[u8],
    in_chunk: usize,
    out_chunk: usize,
) -> Result<Vec<u8>, Error> {
    let mut encoder = Encoder::with_layout(header, region_bytes, block_bytes)?;
    let mut stream = Vec::new();
    let mut room = vec![0_u8; out_chunk.max(1)];
    let mut at = 0_usize;
    while at < data.len() {
        let end = at.saturating_add(in_chunk.max(1)).min(data.len());
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
    Ok(stream)
}

fn decode_all(
    policy: DecoderPolicy,
    stream: &[u8],
    in_chunk: usize,
    out_chunk: usize,
) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(policy);
    let mut out = Vec::new();
    let mut room = vec![0_u8; out_chunk.max(1)];
    let mut at = 0_usize;
    while at < stream.len() {
        let end = at.saturating_add(in_chunk.max(1)).min(stream.len());
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
    Ok(out)
}

fn keep(sink: &mut Vec<u8>, room: &[u8], produced: usize) -> Result<(), Error> {
    let written = room.get(..produced).ok_or(Error::InvalidParameter)?;
    sink.extend_from_slice(written);
    Ok(())
}

/// A pseudo-random chunk size in `1..=limit`, drawn from a stated seed.
fn next_chunk(state: &mut u64, limit: usize) -> usize {
    *state = state
        .wrapping_mul(0x5851_F42D_4C95_7F2D)
        .wrapping_add(0x1405_7B7E_F767_814F);
    let drawn = usize::try_from(state.wrapping_shr(33)).unwrap_or(1);
    drawn
        .checked_rem(limit.max(1))
        .unwrap_or(0)
        .saturating_add(1)
}

/// Encodes `data` through a pseudo-random chunking of the input and the output.
fn encode_randomly(header: FrameHeader, data: &[u8], state: &mut u64) -> Result<Vec<u8>, Error> {
    let mut encoder = Encoder::new(header)?;
    let mut stream = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut fed = 0_usize;
    while fed < data.len() {
        let take = next_chunk(state, 8_192).min(data.len().saturating_sub(fed));
        let end = fed.saturating_add(take);
        let chunk = data.get(fed..end).ok_or(Error::InvalidParameter)?;
        let mut sent = 0_usize;
        while sent < chunk.len() {
            let rest = chunk.get(sent..).ok_or(Error::InvalidParameter)?;
            let width = next_chunk(state, room.len());
            let target = room.get_mut(..width).ok_or(Error::InvalidParameter)?;
            let progress = encoder.encode(rest, target)?;
            keep(&mut stream, target, progress.produced)?;
            sent = sent.saturating_add(progress.consumed);
        }
        fed = end;
    }
    loop {
        let progress = encoder.finish(&mut room)?;
        keep(&mut stream, &room, progress.produced)?;
        if progress.state == StreamState::Finished {
            break;
        }
    }
    Ok(stream)
}

/// Decodes `stream` through a pseudo-random chunking of the input and the output.
fn decode_randomly(stream: &[u8], state: &mut u64) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(permissive());
    let mut out = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut at = 0_usize;
    while at < stream.len() {
        let take = next_chunk(state, 8_192).min(stream.len().saturating_sub(at));
        let end = at.saturating_add(take);
        let chunk = stream.get(at..end).ok_or(Error::InvalidParameter)?;
        let mut consumed = 0_usize;
        while consumed < chunk.len() {
            let rest = chunk.get(consumed..).ok_or(Error::InvalidParameter)?;
            let width = next_chunk(state, room.len());
            let target = room.get_mut(..width).ok_or(Error::InvalidParameter)?;
            let progress = decoder.decode(rest, target)?;
            keep(&mut out, target, progress.produced)?;
            consumed = consumed.saturating_add(progress.consumed);
            if progress.consumed == 0 && progress.produced == 0 {
                break;
            }
        }
        at = end;
    }
    loop {
        let progress = decoder.decode(&[], &mut room)?;
        keep(&mut out, &room, progress.produced)?;
        if progress.produced == 0 {
            break;
        }
    }
    decoder.finish()?;
    Ok(out)
}

/// Round trips every length in the set, rotating the content shape and the integrity mode
/// across it, through each stated chunking and once through a pseudo-random one.
///
/// The attributes rotate rather than multiply. A cartesian product of shape, integrity, and
/// length over a size class costs ten times the tier's whole input budget and proves nothing
/// the rotation does not: every shape and every mode still meets every part of the length
/// range, because the rotation period divides no length step evenly.
fn round_trip_class(lengths: &[usize], chunks: &[(usize, usize)]) -> Result<u64, Error> {
    let mut bytes = 0_u64;
    let mut state = CHUNK_SEED;
    for (index, length) in lengths.iter().enumerate() {
        let shape = pick(&SHAPES, index).ok_or(Error::InvalidParameter)?;
        let integrity = pick(&INTEGRITY, index).ok_or(Error::InvalidParameter)?;
        let header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            integrity,
        );
        let data = shape.content(*length);
        let counted = u64::try_from(*length).unwrap_or(0);

        for (in_chunk, out_chunk) in chunks {
            let stream = encode_all(
                header,
                DEFAULT_REGION_BYTES,
                DEFAULT_BLOCK_BYTES,
                &data,
                *in_chunk,
                *out_chunk,
            )?;
            let decoded = decode_all(permissive(), &stream, *out_chunk, *in_chunk)?;
            assert_eq!(
                decoded,
                data,
                "{} at {length} bytes, chunks {in_chunk} and {out_chunk}",
                shape.name()
            );
            bytes = bytes.saturating_add(counted);
        }

        let stream = encode_randomly(header, &data, &mut state)?;
        let decoded = decode_randomly(&stream, &mut state)?;
        assert_eq!(
            decoded,
            data,
            "{} at {length} bytes, random chunking, seed {CHUNK_SEED}",
            shape.name()
        );
        bytes = bytes.saturating_add(counted);
    }
    Ok(bytes)
}

fn report(segment: &str, inputs: usize, bytes: u64) {
    let mib = bytes.wrapping_shr(20);
    println!("{segment}: {inputs} inputs, {bytes} logical bytes round tripped, {mib} MiB");
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_roundtrip_tiny() -> Result<(), Error> {
    let lengths: Vec<usize> = (0..1_024).collect();
    let bytes = round_trip_class(&lengths, &[(1, 1), (2, 3), (7, 5), (1_024, 1_024)])?;
    report("skeleton-roundtrip-tiny", lengths.len(), bytes);
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_roundtrip_small() -> Result<(), Error> {
    let mut lengths = Vec::new();
    for kib in 1_usize..=64 {
        let at = kib.saturating_mul(1_024);
        lengths.push(at.saturating_sub(1));
        lengths.push(at);
        lengths.push(at.saturating_add(1));
    }
    let bytes = round_trip_class(&lengths, &[(13, 7), (4_096, 4_096)])?;
    report("skeleton-roundtrip-small", lengths.len(), bytes);
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_roundtrip_medium() -> Result<(), Error> {
    // The region size is this class's interesting boundary: one below it the stream holds a
    // single region, one above it two, and the second holds one byte.
    let lengths = [
        65_535_usize,
        65_536,
        65_537,
        262_144,
        700_000,
        DEFAULT_REGION_BYTES.saturating_sub(1),
        DEFAULT_REGION_BYTES,
        DEFAULT_REGION_BYTES.saturating_add(1),
        4_194_303,
    ];
    let bytes = round_trip_class(&lengths, &[(65_536, 65_536)])?;
    report("skeleton-roundtrip-medium", lengths.len(), bytes);
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_roundtrip_large() -> Result<(), Error> {
    let lengths = [4_194_304_usize, 5_000_001, 8_388_608, 12_582_913];
    let bytes = round_trip_class(&lengths, &[(1_048_576, 262_144)])?;
    report("skeleton-roundtrip-large", lengths.len(), bytes);
    Ok(())
}

/// One structural fixture: a frame header, a layout, and the content a stream carries.
struct Fixture {
    name: &'static str,
    header: FrameHeader,
    region_bytes: usize,
    block_bytes: u32,
    shape: Shape,
    length: usize,
}

fn fixtures() -> Vec<Fixture> {
    let mut built = Vec::new();
    let lengths = [0_usize, 1, 16, 63, 64, 65, 128, 200, 300];
    for (index, length) in lengths.iter().enumerate() {
        let mut header = FrameHeader::new(
            ResourceClass::Small,
            if every(index, 2) {
                RegionIndependence::Independent
            } else {
                RegionIndependence::Dependent
            },
            pick(&INTEGRITY, index).unwrap_or(IntegrityMode::Absent),
        );
        if every(index, 3) {
            header.content_length = Some(u64::try_from(*length).unwrap_or(0));
        }
        if every(index, 4) {
            header.dictionary_id = Some(0x0A0B_0C0D);
        }
        built.push(Fixture {
            name: "fixture-layout",
            header,
            region_bytes: FIXTURE_REGION_BYTES,
            block_bytes: FIXTURE_BLOCK_BYTES,
            shape: pick(&SHAPES, index).unwrap_or(Shape::Mixed),
            length: *length,
        });
    }
    built.push(Fixture {
        name: "wide-layout",
        header: plain_header(),
        region_bytes: 4_096,
        block_bytes: 512,
        shape: Shape::Mixed,
        length: 10_000,
    });
    built
}

impl Fixture {
    fn content(&self) -> Vec<u8> {
        self.shape.content(self.length)
    }

    fn stream(&self, data: &[u8]) -> Result<Vec<u8>, Error> {
        encode_all(
            self.header,
            self.region_bytes,
            self.block_bytes,
            data,
            data.len().max(1),
            4_096,
        )
    }

    fn label(&self) -> String {
        format!(
            "{} {} at {} bytes",
            self.name,
            self.shape.name(),
            self.length
        )
    }
}

/// Decodes a prefix and reports whether it reached the terminator.
///
/// A prefix that stops inside a structure is not an error while it is being fed: the decoder
/// is entitled to wait for the rest. `finish` is what turns a stream that never ended into an
/// error, and that is what this checks.
fn decodes_to_an_end(prefix: &[u8]) -> Result<bool, Error> {
    let mut decoder = Decoder::new(permissive());
    let mut room = [0_u8; 64];
    let mut at = 0_usize;
    loop {
        let rest = prefix.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, &mut room)?;
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            return Ok(true);
        }
        if progress.consumed == 0 && progress.produced == 0 {
            break;
        }
    }
    match decoder.finish() {
        Ok(()) => Ok(true),
        Err(Error::TruncatedInput { needed }) => {
            assert!(needed > 0, "a truncated stream asked for nothing");
            Ok(false)
        }
        Err(other) => Err(other),
    }
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_truncation_matrix() -> Result<(), Error> {
    let mut cuts = 0_u64;
    for fixture in fixtures() {
        let data = fixture.content();
        let stream = fixture.stream(&data)?;
        assert_eq!(
            decode_all(permissive(), &stream, 7, 7)?,
            data,
            "{}",
            fixture.label()
        );
        assert!(
            decodes_to_an_end(&stream)?,
            "{} did not reach its terminator whole",
            fixture.label()
        );
        for length in 0..stream.len() {
            let cut = stream.get(..length).ok_or(Error::InvalidParameter)?;
            assert!(
                !decodes_to_an_end(cut)?,
                "{} cut at {length} decoded as a whole frame",
                fixture.label()
            );
            cuts = cuts.saturating_add(1);
        }
    }
    println!("skeleton-truncation-matrix: {cuts} cuts refused");
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_truncation_matrix_refuses_a_declared_size_the_stream_does_not_hold() -> Result<(), Error>
{
    // A region that declares more than it carries is not a truncation of a valid stream, so
    // cutting one never produces it. It is built here instead.
    let header = plain_header();
    let mut stream = Vec::from(header.encode().as_bytes());
    stream.extend_from_slice(
        Record::Region(RegionHeader::new(64, 64 + 4, IntegrityMode::Absent)?)
            .encode()
            .as_bytes(),
    );
    stream.extend_from_slice(BlockHeader::raw(true, 64)?.encode().as_bytes());
    stream.extend_from_slice(&[0_u8; 32]);

    let mut decoder = Decoder::new(permissive());
    let mut room = [0_u8; 64];
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, &mut room)?;
        at = at.saturating_add(progress.consumed);
        assert_ne!(
            progress.state,
            StreamState::Finished,
            "a region that declared more than it held decoded to an end"
        );
        if progress.consumed == 0 && progress.produced == 0 {
            break;
        }
    }
    assert!(
        matches!(decoder.finish(), Err(Error::TruncatedInput { needed }) if needed > 0),
        "a region that declared more than it held ended cleanly"
    );
    Ok(())
}

/// The chunk sizes both permutation segments drive their machines at.
const IN_CHUNKS: [usize; 7] = [1, 2, 3, 7, 13, 4_096, 65_536];
const OUT_CHUNKS: [usize; 5] = [1, 2, 7, 4_096, 65_536];

/// The inputs both permutation segments cover.
///
/// The largest crosses the default region size, so a permutation that moved a region boundary
/// would change the stream rather than only the call pattern.
const PERMUTATION_LENGTHS: [usize; 4] = [0, 1, 300_000, 1_100_000];

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_chunk_permutation_encode() -> Result<(), Error> {
    let mut permutations = 0_u64;
    let mut state = CHUNK_SEED;
    for (index, length) in PERMUTATION_LENGTHS.iter().enumerate() {
        let shape = pick(&SHAPES, index).ok_or(Error::InvalidParameter)?;
        let data = shape.content(*length);
        let header = plain_header();
        let whole = encode_all(
            header,
            DEFAULT_REGION_BYTES,
            DEFAULT_BLOCK_BYTES,
            &data,
            data.len().max(1),
            data.len().max(1),
        )?;

        for in_chunk in IN_CHUNKS {
            for out_chunk in OUT_CHUNKS {
                let stream = encode_all(
                    header,
                    DEFAULT_REGION_BYTES,
                    DEFAULT_BLOCK_BYTES,
                    &data,
                    in_chunk,
                    out_chunk,
                )?;
                assert_eq!(
                    stream,
                    whole,
                    "{} at {length} bytes, input {in_chunk}, output {out_chunk}",
                    shape.name()
                );
                permutations = permutations.saturating_add(1);
            }
        }
        for draw in 0..4 {
            let stream = encode_randomly(header, &data, &mut state)?;
            assert_eq!(
                stream,
                whole,
                "{} at {length} bytes, random draw {draw}, seed {CHUNK_SEED}",
                shape.name()
            );
            permutations = permutations.saturating_add(1);
        }
        assert_eq!(decode_all(permissive(), &whole, 4_096, 4_096)?, data);
    }
    println!("skeleton-chunk-permutation-encode: {permutations} permutations agreed");
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn skeleton_chunk_permutation_decode() -> Result<(), Error> {
    let mut permutations = 0_u64;
    let mut state = CHUNK_SEED;
    for (index, length) in PERMUTATION_LENGTHS.iter().enumerate() {
        let shape = pick(&SHAPES, index).ok_or(Error::InvalidParameter)?;
        let data = shape.content(*length);
        let stream = encode_all(
            plain_header(),
            DEFAULT_REGION_BYTES,
            DEFAULT_BLOCK_BYTES,
            &data,
            4_096,
            4_096,
        )?;

        for in_chunk in IN_CHUNKS {
            for out_chunk in OUT_CHUNKS {
                let decoded = decode_all(permissive(), &stream, in_chunk, out_chunk)?;
                assert_eq!(
                    decoded,
                    data,
                    "{} at {length} bytes, input {in_chunk}, output {out_chunk}",
                    shape.name()
                );
                permutations = permutations.saturating_add(1);
            }
        }
        // A decoder given exactly the room the content needs, and one given the whole stream
        // at once, are the two ends of the permutation range.
        assert_eq!(
            decode_all(
                permissive(),
                &stream,
                stream.len().max(1),
                data.len().max(1)
            )?,
            data
        );
        permutations = permutations.saturating_add(1);
        for draw in 0..4 {
            let decoded = decode_randomly(&stream, &mut state)?;
            assert_eq!(
                decoded,
                data,
                "{} at {length} bytes, random draw {draw}, seed {CHUNK_SEED}",
                shape.name()
            );
            permutations = permutations.saturating_add(1);
        }
    }
    println!("skeleton-chunk-permutation-decode: {permutations} permutations agreed");
    Ok(())
}

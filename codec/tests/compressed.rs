//! The gate-scale proofs of the compressed block.
//!
//! Every test here is ignored by default. The dev tier proves the same properties on a small
//! input set from inside the crate; these run once, at the gate tier, one test per segment of
//! the closing campaign, over an input set sized to that tier's budget.
//!
//! The skeleton's proofs cover a codec that stores. These cover one that compresses, so every
//! content class here is chosen for the block type it drives the selection rule to, and the
//! structural walk reads the type of every block from its header alone.
//!
//! This file owns no codec behavior. It drives the public API and nothing else.

use std::env;
use std::path::PathBuf;

use codec::format::{
    BlockHeader, BlockPrologue, BlockType, DecoderPolicy, Error, FrameHeader, IntegrityMode,
    Record, RegionIndependence, ResourceClass,
};
use codec::stream::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Decoder, Encoder, StreamState};

/// The layout the structural fixtures are built on.
///
/// The sizes are small so one fixture holds several regions and several blocks, which is what
/// puts every structural boundary inside a stream short enough to cut and corrupt at every
/// offset. A block of 256 bytes still compresses on a repetitive shape, so the fixture set
/// reaches the COMPRESSED type rather than only the two stored ones.
const FIXTURE_REGION_BYTES: usize = 1_024;
const FIXTURE_BLOCK_BYTES: u32 = 256;

/// The seed every pseudo-random chunking is drawn from.
///
/// It is a constant rather than a clock reading, so a failure reproduces from the test name
/// alone.
const CHUNK_SEED: u64 = 0x5EED_0000_0000_0003;

/// The content classes the round trips cover.
///
/// Each one drives the selection rule somewhere different: a constant run is what an RLE block
/// exists for, incompressible bytes are what keeps RAW in the rule, and the repetitive
/// shapes are what a COMPRESSED block is for. `Layered` changes class every kibibyte, so one
/// frame of it carries all three types.
#[derive(Clone, Copy, Debug)]
enum Shape {
    Zeros,
    OneByte,
    Mixed,
    Runs,
    TextLike,
    Tokens,
    Records,
    Sparse,
    Layered,
}

const SHAPES: [Shape; 9] = [
    Shape::Zeros,
    Shape::OneByte,
    Shape::Mixed,
    Shape::Runs,
    Shape::TextLike,
    Shape::Tokens,
    Shape::Records,
    Shape::Sparse,
    Shape::Layered,
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
            Self::Tokens => "tokens",
            Self::Records => "records",
            Self::Sparse => "sparse",
            Self::Layered => "layered",
        }
    }

    /// The byte this shape holds at a position, which does not depend on how the content is
    /// cut into chunks.
    fn at(self, position: u64) -> u8 {
        match self {
            Self::Zeros => 0,
            Self::OneByte => 0xA5,
            Self::Mixed => noise(position),
            Self::Runs => mix(position.checked_div(97).unwrap_or(0)),
            Self::TextLike => {
                const ALPHABET: &[u8; 16] = b"the quick brown ";
                let index = usize::try_from(position & 0x0F).unwrap_or(0);
                ALPHABET.get(index).copied().unwrap_or(b' ')
            }
            Self::Tokens => {
                const PATTERN: &[u8; 64] =
                    b"alpha,beta,gamma;delta,epsilon;zeta,eta,theta;iota,kappa,lambda\n";
                let index = usize::try_from(position & 0x3F).unwrap_or(0);
                PATTERN.get(index).copied().unwrap_or(b' ')
            }
            Self::Records => {
                const FRAME: &[u8; 32] = b"{\"id\":0000,\"tag\":\"entry\",\"n\":0}\n";
                let index = usize::try_from(position & 0x1F).unwrap_or(0);
                let byte = FRAME.get(index).copied().unwrap_or(b' ');
                if byte == b'0' {
                    // The digits vary per record, so a record differs from the one before it
                    // and the literal stream carries something the match stream cannot.
                    let record = position.wrapping_shr(5);
                    b'0'.wrapping_add(mix(record).wrapping_rem(10))
                } else {
                    byte
                }
            }
            Self::Sparse => {
                if position.trailing_zeros() >= 6 {
                    mix(position)
                } else {
                    0
                }
            }
            Self::Layered => {
                // One kibibyte per band, so a fixture block of 256 bytes sits wholly inside
                // one band and the frame carries one block of each type in turn.
                match position.wrapping_shr(10) & 0x03 {
                    0 => Self::Zeros.at(position),
                    1 => Self::Mixed.at(position),
                    2 => Self::TextLike.at(position),
                    _ => Self::Runs.at(position),
                }
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

/// A byte a codec cannot predict from the bytes around it.
///
/// The high byte of one multiply is not enough: the selection rule compressed it, because the
/// byte values it reaches are not uniform and a Huffman code over them pays for itself. This
/// is the splitmix64 finalizer, whose output byte is uniform, so a block of it is what keeps
/// RAW in the rule.
fn noise(position: u64) -> u8 {
    let mut z = position.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ z.wrapping_shr(30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ z.wrapping_shr(27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    u8::try_from((z ^ z.wrapping_shr(31)) & 0xFF).unwrap_or(0)
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
    encode_all_mode(
        false,
        header,
        region_bytes,
        block_bytes,
        data,
        in_chunk,
        out_chunk,
    )
}

/// Encodes `data` through one mode, one layout, and one input/output chunking.
///
/// `balanced` selects the production chain32 path; FAST stays the default a caller who states
/// no mode gets. The signature mirrors `encode_all` with the mode added, so the mode stays a
/// parameter rather than a second copy of the drive loop.
#[allow(clippy::too_many_arguments)]
fn encode_all_mode(
    balanced: bool,
    header: FrameHeader,
    region_bytes: usize,
    block_bytes: u32,
    data: &[u8],
    in_chunk: usize,
    out_chunk: usize,
) -> Result<Vec<u8>, Error> {
    let mut encoder = if balanced {
        Encoder::balanced_with_layout(header, region_bytes, block_bytes)?
    } else {
        Encoder::with_layout(header, region_bytes, block_bytes)?
    };
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

/// How many blocks of each type a set of frames carried.
#[derive(Clone, Copy, Debug, Default)]
struct Histogram {
    raw: u64,
    rle: u64,
    compressed: u64,
}

impl Histogram {
    const fn count(&mut self, kind: BlockType) {
        match kind {
            BlockType::Raw => self.raw = self.raw.saturating_add(1),
            BlockType::Rle => self.rle = self.rle.saturating_add(1),
            BlockType::Compressed => self.compressed = self.compressed.saturating_add(1),
        }
    }

    const fn absorb(&mut self, other: Self) {
        self.raw = self.raw.saturating_add(other.raw);
        self.rle = self.rle.saturating_add(other.rle);
        self.compressed = self.compressed.saturating_add(other.compressed);
    }

    const fn total(self) -> u64 {
        self.raw
            .saturating_add(self.rle)
            .saturating_add(self.compressed)
    }

    fn line(self) -> String {
        format!(
            "{} raw, {} rle, {} compressed",
            self.raw, self.rle, self.compressed
        )
    }
}

/// Round trips every length in the set, rotating the content shape and the integrity mode
/// across it, through each stated chunking and once through a pseudo-random one.
///
/// The attributes rotate rather than multiply. A cartesian product of shape, integrity, and
/// length over a size class costs ten times the tier's whole input budget and proves nothing
/// the rotation does not: every shape and every mode still meets every part of the length
/// range, because the rotation period divides no length step evenly.
///
/// `offset` turns the rotation by that many shapes. A class cheap enough to afford the
/// product runs the whole rotation, one pass per shape, and then every length has met every
/// shape. The tiny class is the one that affords it, and it is also the one where a length
/// boundary matters most.
fn round_trip_class(
    lengths: &[usize],
    chunks: &[(usize, usize)],
    offset: usize,
) -> Result<(u64, Histogram), Error> {
    let mut bytes = 0_u64;
    let mut histogram = Histogram::default();
    let mut state = CHUNK_SEED;
    for (index, length) in lengths.iter().enumerate() {
        let shape = pick(&SHAPES, index.saturating_add(offset)).ok_or(Error::InvalidParameter)?;
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
            histogram.absorb(walk(&stream)?.histogram());
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
    Ok((bytes, histogram))
}

fn report(segment: &str, inputs: usize, bytes: u64, histogram: Histogram) {
    let mib = bytes.wrapping_shr(20);
    println!(
        "{segment}: {inputs} inputs, {bytes} logical bytes round tripped, {mib} MiB, \
         {} over {} blocks",
        histogram.line(),
        histogram.total()
    );
}

/// A deterministic sequence no match finder can match.
///
/// The bytes are the top byte of a maximal-length 32-bit LFSR advanced one byte per output,
/// so every four-byte window is unique over any region shorter than the period. A pseudo-random
/// byte stream collides and is not the all-miss run the transition bound is stated for.
fn matchless(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x1234_5678;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        for _ in 0..8 {
            let feedback = ((state >> 31) ^ (state >> 21) ^ (state >> 1) ^ state) & 1;
            state = state.wrapping_shl(1) | feedback;
        }
        out.push(u8::try_from(state >> 24).unwrap_or(0));
    }
    out
}

/// The small layout the region-streak boundary tests cross.
const STREAK_REGION_BYTES: usize = 131_072;
const STREAK_BLOCK_BYTES: u32 = 16_384;

#[test]
fn region_streak_frames_are_invariant_to_input_chunking() -> Result<(), Error> {
    // The streak is carried across a region's blocks, so the only cut the encoder may respond
    // to is its layout, never the rhythm a caller feeds input. A streak that depended on the
    // call boundary would move the frames with the input chunk size.
    let fixtures = [
        ("noise", Shape::Mixed.content(300_000)),
        ("text", Shape::TextLike.content(300_000)),
        ("records", Shape::Records.content(300_000)),
        ("runs", Shape::Runs.content(300_000)),
    ];
    let chunks = [
        300_000,
        1,
        7,
        1_000,
        STREAK_BLOCK_BYTES as usize,
        STREAK_REGION_BYTES,
    ];
    for balanced in [false, true] {
        for (name, data) in &fixtures {
            let mut expected: Option<Vec<u8>> = None;
            for chunk in chunks {
                let stream = encode_all_mode(
                    balanced,
                    plain_header(),
                    STREAK_REGION_BYTES,
                    STREAK_BLOCK_BYTES,
                    data,
                    chunk,
                    8_192,
                )?;
                match &expected {
                    None => expected = Some(stream),
                    Some(first) => assert_eq!(
                        &stream, first,
                        "{name} moved with an input chunk of {chunk} bytes (balanced {balanced})"
                    ),
                }
            }
            let stream = expected.ok_or(Error::InvalidParameter)?;
            assert_eq!(
                decode_all(permissive(), &stream, 8_192, 8_192)?,
                *data,
                "{name} did not round trip"
            );
        }
    }
    Ok(())
}

#[test]
fn the_all_miss_transition_stays_within_its_bound() -> Result<(), Error> {
    // A long all-miss run immediately followed by a compressible run, inside one region and
    // across a region boundary. The carried streak can skip the first searches of the
    // compressible run, so the marginal cost of the run is compared against the same run
    // encoded alone. The extra is bounded by the carried jump the region's own end produces,
    // because the streak cannot exceed the misses of one region and the region boundary resets
    // it; the across-boundary case therefore does not carry the first region's streak into the
    // second. The measured figures are printed, not retyped.
    let jump_bound = 1 + (STREAK_REGION_BYTES >> 6);
    for (label, prefix_len) in [("one-region", 65_536usize), ("cross-region", 200_000)] {
        let prefix = matchless(prefix_len);
        let suffix = Shape::TextLike.content(400_000);
        let mut data = prefix.clone();
        data.extend_from_slice(&suffix);
        for balanced in [false, true] {
            let whole = encode_all_mode(
                balanced,
                plain_header(),
                STREAK_REGION_BYTES,
                STREAK_BLOCK_BYTES,
                &data,
                data.len(),
                8_192,
            )?;
            let prefix_only = encode_all_mode(
                balanced,
                plain_header(),
                STREAK_REGION_BYTES,
                STREAK_BLOCK_BYTES,
                &prefix,
                prefix.len(),
                8_192,
            )?;
            let suffix_only = encode_all_mode(
                balanced,
                plain_header(),
                STREAK_REGION_BYTES,
                STREAK_BLOCK_BYTES,
                &suffix,
                suffix.len(),
                8_192,
            )?;
            assert_eq!(
                decode_all(permissive(), &whole, 8_192, 8_192)?,
                data,
                "{label} balanced {balanced} did not round trip"
            );
            let transition = whole.len().saturating_sub(prefix_only.len());
            let extra = transition.saturating_sub(suffix_only.len());
            let bound = suffix_only.len().saturating_add(jump_bound);
            println!(
                "region-streak transition {label} balanced {balanced}: whole {} prefix {} \
                 suffix {} marginal {transition} extra {extra} bound {bound} \
                 ({jump_bound} over suffix alone)",
                whole.len(),
                prefix_only.len(),
                suffix_only.len(),
            );
            assert!(
                transition <= bound,
                "{label} balanced {balanced}: the transition cost {transition} exceeds the \
                 bound {bound}, {extra} bytes over the suffix alone ({}), above the carried \
                 jump {jump_bound}",
                suffix_only.len()
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_roundtrip_tiny() -> Result<(), Error> {
    let lengths: Vec<usize> = (0..1_024).collect();
    let chunks = [(1, 1), (2, 3), (7, 5), (1_024, 1_024)];
    let mut bytes = 0_u64;
    let mut histogram = Histogram::default();
    // One pass per shape, so every length below a kibibyte meets every content class.
    for offset in 0..SHAPES.len() {
        let (spent, types) = round_trip_class(&lengths, &chunks, offset)?;
        bytes = bytes.saturating_add(spent);
        histogram.absorb(types);
    }
    report(
        "compressed-roundtrip-tiny",
        lengths.len().saturating_mul(SHAPES.len()),
        bytes,
        histogram,
    );
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_roundtrip_small() -> Result<(), Error> {
    let mut lengths = Vec::new();
    for kib in 1_usize..=64 {
        let at = kib.saturating_mul(1_024);
        lengths.push(at.saturating_sub(1));
        lengths.push(at);
        lengths.push(at.saturating_add(1));
    }
    let (bytes, histogram) = round_trip_class(&lengths, &[(13, 7), (4_096, 4_096)], 0)?;
    report(
        "compressed-roundtrip-small",
        lengths.len(),
        bytes,
        histogram,
    );
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_roundtrip_medium() -> Result<(), Error> {
    // The block length and the region length are this class's interesting boundaries: the
    // tables in force and the offset cache cross the first and are discarded at the second.
    let lengths = [
        65_535_usize,
        65_536,
        65_537,
        131_072,
        262_144,
        700_000,
        DEFAULT_REGION_BYTES.saturating_sub(1),
        DEFAULT_REGION_BYTES,
        DEFAULT_REGION_BYTES.saturating_add(1),
        4_194_303,
    ];
    let (bytes, histogram) = round_trip_class(&lengths, &[(65_536, 65_536)], 0)?;
    report(
        "compressed-roundtrip-medium",
        lengths.len(),
        bytes,
        histogram,
    );
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_roundtrip_large() -> Result<(), Error> {
    let lengths = [4_194_304_usize, 5_000_001, 8_388_608, 12_582_913];
    let (bytes, histogram) = round_trip_class(&lengths, &[(1_048_576, 262_144)], 0)?;
    report(
        "compressed-roundtrip-large",
        lengths.len(),
        bytes,
        histogram,
    );
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

/// The fixture set both matrices work over.
///
/// Every stream is short enough to cut and to corrupt at every one of its bytes, and the set
/// covers every shape, both integrity modes, both independence flags, the optional header
/// fields, and a layout wide enough that one region holds several blocks of each type.
fn fixtures() -> Vec<Fixture> {
    let mut built = Vec::new();
    let lengths = [0_usize, 1, 64, 255, 256, 257, 512, 900, 1_024, 2_048, 4_096];
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
            shape: pick(&SHAPES, index).unwrap_or(Shape::Tokens),
            length: *length,
        });
    }
    built.push(Fixture {
        name: "layered-layout",
        header: plain_header(),
        region_bytes: 4_096,
        block_bytes: 1_024,
        shape: Shape::Layered,
        length: 8_192,
    });
    built.push(Fixture {
        name: "wide-layout",
        header: plain_header(),
        region_bytes: 4_096,
        block_bytes: 512,
        shape: Shape::Records,
        length: 6_000,
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
fn compressed_truncation_matrix() -> Result<(), Error> {
    let mut cuts = 0_u64;
    let mut histogram = Histogram::default();
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
        histogram.absorb(walk(&stream)?.histogram());
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
    assert!(
        histogram.compressed > 0,
        "the fixture set carried no COMPRESSED block, so the matrix cut none"
    );
    println!(
        "compressed-truncation-matrix: {cuts} cuts refused, over {}",
        histogram.line()
    );
    Ok(())
}

/// The byte classes a corruption run applies at every offset.
///
/// A mask that clears, a mask that sets, a mask that moves the low bit, and a mask that moves
/// the high bit. Between them every bit position of every byte is reached by at least one, and
/// no mutation is a no-op: the tool skips an offset whose byte the mask leaves unchanged.
const MUTATIONS: [(&str, u8); 4] = [
    ("clear", 0x00),
    ("set", 0xFF),
    ("low-bit", 0x01),
    ("high-bit", 0x80),
];

fn mutate(stream: &[u8], at: usize, mask: u8, name: &str) -> Option<Vec<u8>> {
    let mut copy = Vec::from(stream);
    let slot = copy.get_mut(at)?;
    let after = match name {
        "clear" => 0x00,
        "set" => 0xFF,
        _ => *slot ^ mask,
    };
    if after == *slot {
        return None;
    }
    *slot = after;
    Some(copy)
}

/// What a set of corrupted streams did.
#[derive(Clone, Copy, Debug, Default)]
struct Outcomes {
    /// The decoder refused with a typed error of a declared class.
    refused: u64,
    /// The decoder accepted, and the content is the one the fixture held.
    identical: u64,
    /// The decoder accepted, and the content differs.
    ///
    /// Version 1 defines the position and the width of the integrity field and computes
    /// nothing into it, so a corruption a structure does not contradict is accepted and the
    /// content it decodes to is whatever the corrupted bytes describe. The figure is recorded
    /// rather than asserted to be zero: it is the measure of what the reserved field does not
    /// yet do, and the integrity algorithm owns closing it.
    accepted_other: u64,
}

impl Outcomes {
    const fn total(self) -> u64 {
        self.refused
            .saturating_add(self.identical)
            .saturating_add(self.accepted_other)
    }

    const fn absorb(&mut self, other: Self) {
        self.refused = self.refused.saturating_add(other.refused);
        self.identical = self.identical.saturating_add(other.identical);
        self.accepted_other = self.accepted_other.saturating_add(other.accepted_other);
    }

    fn line(self) -> String {
        format!(
            "{} mutations, {} refused, {} accepted unchanged, {} accepted with other content",
            self.total(),
            self.refused,
            self.identical,
            self.accepted_other
        )
    }
}

/// Whether an error is one of the classes the format declares for a malformed stream.
///
/// A corrupted stream may produce any of them, and nothing else. `InvalidParameter` is the
/// class this list leaves out on purpose: it names a caller mistake, and a corrupted stream is
/// not one.
const fn is_declared(error: Error) -> bool {
    matches!(
        error,
        Error::InvalidFormat
            | Error::UnsupportedVersion { .. }
            | Error::UnsupportedFeature(_)
            | Error::CorruptData(_)
            | Error::TruncatedInput { .. }
            | Error::LimitExceeded { .. }
            | Error::OutputTooSmall { .. }
    )
}

/// Decodes a corrupted stream and classifies what happened.
///
/// A content length is the only verification version 1 carries, and it is read from the
/// corrupted stream rather than from the one it was made of: a mutation of the frame header's
/// flag byte produces a frame that declares no length, and that frame is not the one the
/// fixture wrote. What is asserted is the decoder's own contract: a frame it accepted that
/// declares a content length produced exactly that many bytes.
fn corrupted(stream: &[u8], original: &[u8], label: &str) -> Outcomes {
    let mut outcomes = Outcomes::default();
    match decode_all(permissive(), stream, 17, 61) {
        Ok(decoded) => {
            if let Ok((header, _)) = FrameHeader::decode(stream)
                && let Some(length) = header.content_length
            {
                assert_eq!(
                    u64::try_from(decoded.len()).unwrap_or(u64::MAX),
                    length,
                    "{label} was accepted against a declared content length it does not hold"
                );
            }
            if decoded == original {
                outcomes.identical = 1;
            } else {
                outcomes.accepted_other = 1;
            }
        }
        Err(error) => {
            assert!(is_declared(error), "{label} produced {error}");
            outcomes.refused = 1;
        }
    }
    outcomes
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_corruption_matrix() -> Result<(), Error> {
    let mut structural = Outcomes::default();
    let mut body = Outcomes::default();
    let mut histogram = Histogram::default();
    let mut lengths_checked = 0_u64;
    for fixture in fixtures() {
        let data = fixture.content();
        let stream = fixture.stream(&data)?;
        let frame = walk(&stream)?;
        histogram.absorb(frame.histogram());
        for at in 0..stream.len() {
            let inside = frame.is_structural(at);
            for (name, mask) in MUTATIONS {
                let Some(corrupt) = mutate(&stream, at, mask, name) else {
                    continue;
                };
                let label = format!("{} corrupted at {at} by {name}", fixture.label());
                let outcomes = corrupted(&corrupt, &data, &label);
                if FrameHeader::decode(&corrupt)
                    .is_ok_and(|(header, _)| header.content_length.is_some())
                {
                    lengths_checked = lengths_checked.saturating_add(1);
                }
                if inside {
                    structural.absorb(outcomes);
                } else {
                    body.absorb(outcomes);
                }
            }
        }
    }
    assert!(
        histogram.compressed > 0,
        "the fixture set carried no COMPRESSED block, so the matrix corrupted none"
    );
    assert!(
        structural.refused > 0 && body.refused > 0,
        "a matrix that refuses nothing is not a matrix"
    );
    println!(
        "compressed-corruption-matrix: structural bytes: {}",
        structural.line()
    );
    println!("compressed-corruption-matrix: body bytes: {}", body.line());
    println!(
        "compressed-corruption-matrix: {lengths_checked} mutations of a frame that declares its \
         content length, every accepted one producing exactly that length. Version 1 computes \
         no integrity field, so an accepted corruption the structures do not contradict \
         decodes to what the corrupted bytes describe, and the counts above are what that \
         costs. Over {}",
        histogram.line()
    );
    Ok(())
}

/// The chunk sizes both permutation segments drive their machines at.
const IN_CHUNKS: [usize; 7] = [1, 2, 3, 7, 13, 4_096, 65_536];
const OUT_CHUNKS: [usize; 5] = [1, 2, 7, 4_096, 65_536];

/// The inputs both permutation segments cover.
///
/// The largest crosses the default region size, so a permutation that moved a region boundary
/// would change the stream rather than only the call pattern. The shapes rotate across them,
/// so the set reaches every block type.
const PERMUTATION_LENGTHS: [usize; 5] = [0, 1, 70_000, 300_000, 1_100_000];

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_chunk_permutation_encode() -> Result<(), Error> {
    let mut permutations = 0_u64;
    let mut histogram = Histogram::default();
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
        histogram.absorb(walk(&whole)?.histogram());

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
    assert!(
        histogram.compressed > 0,
        "no permutation covered a COMPRESSED block"
    );
    println!(
        "compressed-chunk-permutation-encode: {permutations} permutations agreed, over {}",
        histogram.line()
    );
    Ok(())
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_chunk_permutation_decode() -> Result<(), Error> {
    let mut permutations = 0_u64;
    let mut histogram = Histogram::default();
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
        histogram.absorb(walk(&stream)?.histogram());

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
    assert!(
        histogram.compressed > 0,
        "no permutation covered a COMPRESSED block"
    );
    println!(
        "compressed-chunk-permutation-decode: {permutations} permutations agreed, over {}",
        histogram.line()
    );
    Ok(())
}

/// Where one structure sits inside a stream.
#[derive(Clone, Copy, Debug)]
struct Span {
    at: usize,
    len: usize,
}

impl Span {
    const fn holds(self, offset: usize) -> bool {
        offset >= self.at && offset < self.at.saturating_add(self.len)
    }
}

/// What one block of a frame declares, read without expanding a payload.
struct BlockFacts {
    kind: BlockType,
    decoded: u32,
    header: Span,
    /// The prologue a COMPRESSED block carries, and where it sits.
    prologue: Option<Span>,
}

/// One frame's structure: the header, the regions, and the blocks each holds.
struct Frame {
    header: Span,
    regions: Vec<Span>,
    blocks: Vec<BlockFacts>,
    content_bytes: u64,
}

/// Reads a frame's structure without expanding a payload.
///
/// Every field it reads is one a reader needs to reach the next structure, so a walk that
/// reaches the terminator is itself the evidence that the type of every block is knowable
/// without decoding one.
fn walk(stream: &[u8]) -> Result<Frame, Error> {
    let (header, mut at) = FrameHeader::decode(stream)?;
    let mut frame = Frame {
        header: Span { at: 0, len: at },
        regions: Vec::new(),
        blocks: Vec::new(),
        content_bytes: 0,
    };
    loop {
        let rest = stream.get(at..).ok_or(Error::InvalidParameter)?;
        let (record, used) = Record::decode(rest, header.integrity)?;
        frame.regions.push(Span { at, len: used });
        at = at.saturating_add(used);
        let region = match record {
            Record::Terminator => break,
            Record::Region(region) => region,
        };
        let mut left = region.physical_size;
        let mut first = true;
        while left > 0 {
            let rest = stream.get(at..).ok_or(Error::InvalidParameter)?;
            let (block, used) = BlockHeader::decode(rest)?;
            let header_span = Span { at, len: used };
            at = at.saturating_add(used);
            left = left
                .checked_sub(u64::try_from(used).unwrap_or(u64::MAX))
                .ok_or(Error::InvalidParameter)?;
            let mut prologue = None;
            let stored = match block.kind {
                BlockType::Raw => u64::from(block.decoded_len()),
                BlockType::Rle => 1,
                BlockType::Compressed => {
                    let body = stream.get(at..).ok_or(Error::InvalidParameter)?;
                    let (parsed, used) =
                        BlockPrologue::decode(body, block.decoded_len(), first, &permissive())?;
                    prologue = Some(Span { at, len: used });
                    u64::try_from(used)
                        .ok()
                        .and_then(|bytes| {
                            bytes.checked_add(parsed.body_bytes().unwrap_or(u64::MAX))
                        })
                        .ok_or(Error::InvalidParameter)?
                }
            };
            frame.blocks.push(BlockFacts {
                kind: block.kind,
                decoded: block.decoded_len(),
                header: header_span,
                prologue,
            });
            frame.content_bytes = frame
                .content_bytes
                .saturating_add(u64::from(block.decoded_len()));
            at = at.saturating_add(usize::try_from(stored).map_err(|_| Error::InvalidParameter)?);
            left = left.checked_sub(stored).ok_or(Error::InvalidParameter)?;
            first = false;
        }
    }
    assert_eq!(at, stream.len(), "the walk did not reach the frame's end");
    Ok(frame)
}

impl Frame {
    fn histogram(&self) -> Histogram {
        let mut histogram = Histogram::default();
        for block in &self.blocks {
            histogram.count(block.kind);
        }
        histogram
    }

    /// Whether the byte at this offset belongs to a structure a reader parses.
    ///
    /// The frame header, every record header, every block header, and the prologue of every
    /// COMPRESSED block. Everything else is a stored payload or a coded body.
    fn is_structural(&self, offset: usize) -> bool {
        if self.header.holds(offset) {
            return true;
        }
        if self.regions.iter().any(|span| span.holds(offset)) {
            return true;
        }
        self.blocks.iter().any(|block| {
            block.header.holds(offset) || block.prologue.is_some_and(|span| span.holds(offset))
        })
    }
}

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_block_types_visible() -> Result<(), Error> {
    let mut histogram = Histogram::default();
    let mut frames = 0_u64;
    for fixture in fixtures() {
        let data = fixture.content();
        let stream = fixture.stream(&data)?;
        let frame = walk(&stream)?;
        assert_eq!(
            frame.content_bytes,
            u64::try_from(data.len()).unwrap_or(0),
            "{} declares content the walk does not add up to",
            fixture.label()
        );
        for block in &frame.blocks {
            assert!(
                block.decoded > 0,
                "{} carries a block declaring no content",
                fixture.label()
            );
            assert_eq!(
                block.prologue.is_some(),
                block.kind == BlockType::Compressed,
                "{} carries a prologue on a block that is not COMPRESSED, or none on one that \
                 is",
                fixture.label()
            );
        }
        // The walk read every type from a header. The decode is what shows the walk read the
        // same frame the decoder does.
        assert_eq!(decode_all(permissive(), &stream, 4_096, 4_096)?, data);
        histogram.absorb(frame.histogram());
        frames = frames.saturating_add(1);
    }

    // The three lengths below drive the rule to each type on purpose, so the set proves the
    // property for all three rather than for whichever two the fixtures happened to reach.
    for (shape, kind) in [
        (Shape::Mixed, BlockType::Raw),
        (Shape::Zeros, BlockType::Rle),
        (Shape::TextLike, BlockType::Compressed),
    ] {
        let data = shape.content(4_096);
        let stream = encode_all(plain_header(), 4_096, 1_024, &data, 4_096, 4_096)?;
        let frame = walk(&stream)?;
        assert!(
            frame.blocks.iter().all(|block| block.kind == kind),
            "{} did not drive the selection rule to {kind:?}: {}",
            shape.name(),
            frame.histogram().line()
        );
        histogram.absorb(frame.histogram());
        frames = frames.saturating_add(1);
    }

    assert!(
        histogram.raw > 0 && histogram.rle > 0 && histogram.compressed > 0,
        "the set did not reach all three block types: {}",
        histogram.line()
    );
    println!(
        "compressed-block-types-visible: {frames} frames walked without expanding a payload, \
         {}",
        histogram.line()
    );
    Ok(())
}

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

/// The corpus entries this segment covers, and the bytes it reads of each.
///
/// One entry per registered content class, and the prefix is what keeps the segment inside
/// its wall clock. The tier's input budget is a budget and not an ambition: this set spends
/// about a fifth of it and covers every class, which finds more than one large entry would.
const COVERAGE: [(&str, usize); 14] = [
    ("project-high-entropy-tiny.bin", 4_096),
    ("project-zeros-tiny.bin", 4_096),
    ("project-json-small.json", 4_096),
    ("project-sparse-small.bin", 4_096),
    ("project-short-tokens-small.bin", 16_384),
    ("project-source-small.rs", 32_768),
    ("project-logs-medium.log", 65_536),
    ("project-json-medium.json", 262_144),
    ("project-database-rows-medium.tsv", 262_144),
    ("project-serialized-binary-medium.bin", 262_144),
    ("project-mixed-medium.mixed", 262_144),
    ("project-source-medium.rs", 1_048_576),
    ("project-long-repetitions-medium.bin", 1_048_576),
    ("project-already-compressed-medium.bin", 1_048_576),
];

/// The public corpora this segment adds, and the bytes it reads of each.
///
/// They are the entries nobody in this project generated, so they are what says the codec
/// meets content it was not shaped around.
const COVERAGE_PUBLIC: [(&str, usize); 4] = [
    ("gutenberg-pride-and-prejudice", 774_000),
    ("gutenberg-war-and-peace", 3_300_000),
    ("gutenberg-shakespeare", 5_600_000),
    ("enwik8", 8_388_608),
];

#[test]
#[ignore = "gate tier: the closing campaign runs this segment"]
fn compressed_corpus_coverage() -> Result<(), Error> {
    // A gate is a claim, and a segment that measured nothing is not one. The corpus lives
    // outside the repository and the corpus tooling materializes it; a host that has not run
    // that tooling fails here rather than passing on an empty set.
    let cache = cache();
    assert!(
        cache.is_dir(),
        "{} does not hold the corpus. Run `make corpus`, or set CORPUS to the cache root.",
        cache.display()
    );

    let mut read = 0_u64;
    let mut stored = 0_u64;
    let mut entries = 0_u64;
    let mut histogram = Histogram::default();
    for (name, bytes) in COVERAGE.into_iter().chain(COVERAGE_PUBLIC) {
        let path = cache.join(name);
        let read_back = std::fs::read(&path);
        assert!(
            read_back.is_ok(),
            "{name} is not in the corpus at {}. Run `make corpus`.",
            path.display()
        );
        let whole = read_back.unwrap_or_default();
        let data = whole
            .get(..bytes.min(whole.len()))
            .ok_or(Error::InvalidParameter)?;
        let stream = encode_all(
            plain_header(),
            DEFAULT_REGION_BYTES,
            DEFAULT_BLOCK_BYTES,
            data,
            65_536,
            65_536,
        )?;
        assert_eq!(
            decode_all(permissive(), &stream, 65_536, 65_536)?,
            data,
            "{name} did not round trip"
        );
        let frame = walk(&stream)?;
        let types = frame.histogram();
        println!(
            "compressed-corpus-coverage: {name} {} -> {} bytes, {}",
            data.len(),
            stream.len(),
            types.line()
        );
        histogram.absorb(types);
        read = read.saturating_add(u64::try_from(data.len()).unwrap_or(0));
        stored = stored.saturating_add(u64::try_from(stream.len()).unwrap_or(0));
        entries = entries.saturating_add(1);
    }

    assert_eq!(
        entries,
        u64::try_from(COVERAGE.len().saturating_add(COVERAGE_PUBLIC.len())).unwrap_or(0),
        "an entry was skipped"
    );
    assert!(
        histogram.compressed > 0,
        "no corpus entry produced a COMPRESSED block"
    );
    println!(
        "compressed-corpus-coverage: {entries} entries, {read} content bytes -> {stored} \
         stream bytes, {}",
        histogram.line()
    );
    Ok(())
}

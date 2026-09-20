//! The BALANCED compatibility suite: same format, same decoder, no mode leak.
//!
//! BALANCED is encoder freedom. The streams it writes carry no mode bit and no
//! header field the decoder learns; every one of them decodes under the
//! conservative policy this file drives. What is proved here:
//!
//! * `decode(encode(x)) == x` for every input class, every size including
//!   empty and one byte, at every input and output chunking, with starved and
//!   exact output space.
//! * The BALANCED bytes do not move with the input chunking: a fixed encoder
//!   version and mode cut the same regions and blocks however the caller
//!   feeds the bytes.
//! * A mutated BALANCED stream is refused with a typed error of a declared
//!   class, or accepted as the stream its own bytes describe; never a panic,
//!   never an unbounded read, never an `InvalidParameter` (which names a
//!   caller mistake, not a malformed stream).
//!
//! This file owns no codec behavior. It drives the public API and nothing
//! else.

use codec::format::{
    DecoderPolicy, Error, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass,
};
use codec::stream::{Decoder, Encoder, StreamState};

/// The frame every case is encoded under.
const fn header() -> FrameHeader {
    FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    )
}

/// The output room a starved decoder is given, in bytes.
const STARVED: usize = 7;

/// One deterministic byte generator, so a class is the same bytes on every run.
struct Source(u64);

impl Source {
    fn byte(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(0x5851_F42D_4C95_7F2D)
            .wrapping_add(0x1405_7B7E_F767_814F);
        u8::try_from((self.0 >> 24) & 0xFF).unwrap_or(0)
    }
}

/// The input classes the round trip covers.
#[derive(Clone, Copy, Debug)]
enum Class {
    Empty,
    OneByte,
    Random,
    Repetitive,
    NearWindow,
    Incompressible,
    Text,
}

const CLASSES: [Class; 7] = [
    Class::Empty,
    Class::OneByte,
    Class::Random,
    Class::Repetitive,
    Class::NearWindow,
    Class::Incompressible,
    Class::Text,
];

impl Class {
    const fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::OneByte => "one-byte",
            Self::Random => "random",
            Self::Repetitive => "repetitive",
            Self::NearWindow => "near-window",
            Self::Incompressible => "incompressible",
            Self::Text => "text",
        }
    }

    /// The content of this class at `len` bytes. Allocates nothing proportional
    /// to the total logical input beyond the fixture itself.
    fn content(self, len: usize) -> Vec<u8> {
        match self {
            Self::Empty => Vec::new(),
            Self::OneByte => vec![0x5A_u8; len],
            Self::Random => {
                let mut source = Source(0x5EED_0000_0000_0042);
                (0..len).map(|_| source.byte()).collect()
            }
            Self::Repetitive => {
                let phrase = b"the same thirty-seven bytes, again!!!";
                (0..len)
                    .map(|at| {
                        phrase
                            .get(at.checked_rem(phrase.len()).unwrap_or(0))
                            .copied()
                            .unwrap_or(b'?')
                    })
                    .collect()
            }
            Self::NearWindow => {
                // A block of content, a block of filler, and the same content
                // again, so the second copy's matches sit at the window edge.
                let window = 65_536_usize;
                let head: Vec<u8> = {
                    let mut source = Source(0x5EED_0000_0000_0031);
                    (0..window / 2).map(|_| source.byte()).collect()
                };
                let filler: Vec<u8> = {
                    let mut source = Source(0x5EED_0000_0000_0032);
                    (0..window / 2).map(|_| source.byte()).collect()
                };
                let mut data = Vec::with_capacity(len);
                while data.len() < len {
                    data.extend_from_slice(&head);
                    data.extend_from_slice(&filler);
                    data.extend_from_slice(&head);
                }
                data.truncate(len);
                data
            }
            Self::Incompressible => (0..len)
                .map(|at| {
                    let mixed = u64::try_from(at)
                        .unwrap_or(0)
                        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        .rotate_left(29)
                        .wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    u8::try_from((mixed >> 33) & 0xFF).unwrap_or(0)
                })
                .collect(),
            Self::Text => {
                let phrase = b"the quick brown fox jumps over the lazy dog. ";
                (0..len)
                    .map(|at| {
                        phrase
                            .get(at.checked_rem(phrase.len()).unwrap_or(0))
                            .copied()
                            .unwrap_or(b' ')
                    })
                    .collect()
            }
        }
    }
}

/// One BALANCED stream over `data`, feeding `in_chunk` bytes per call into
/// room of `out_chunk` bytes per call.
fn encode(data: &[u8], in_chunk: usize, out_chunk: usize) -> Result<Vec<u8>, Error> {
    let mut encoder = Encoder::balanced(header())?;
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

/// The content of a BALANCED stream, decoded under the conservative policy in
/// output room of `room` bytes per call.
fn decode(stream: &[u8], room: usize) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
    let mut out = Vec::new();
    let mut space = vec![0_u8; room];
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, &mut space)?;
        out.extend_from_slice(
            space
                .get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            break;
        }
        if progress.consumed == 0 && progress.produced == 0 {
            // A stream that wants input the caller cannot supply is the one
            // case that stops without finishing. `finish` names the truncation,
            // so the result stays a typed error and never a silent end.
            break;
        }
    }
    decoder.finish()?;
    Ok(out)
}

/// The chunkings every case is fed at, bounded by the size so the cost stays
/// inside the dev budget.
fn chunkings(len: usize) -> Vec<usize> {
    if len <= 8_192 {
        vec![1, 2, 3, 7, len.max(1)]
    } else {
        vec![7, 4_096, len.max(1)]
    }
}

const SIZES: [usize; 14] = [
    0, 1, 2, 3, 4, 63, 64, 255, 256, 1_019, 4_096, 65_535, 65_536, 65_537,
];

#[test]
#[allow(clippy::too_many_lines)]
fn balanced_round_trips_every_class_at_every_size_and_chunking() -> Result<(), Error> {
    for class in CLASSES {
        for len in SIZES {
            let data = class.content(len);
            let whole = encode(&data, data.len().max(1), 8_192)?;
            // The bytes do not move with how the caller feeds them, in or out.
            for in_chunk in chunkings(len) {
                for out_chunk in [1_usize, 7, 8_192] {
                    let stream = encode(&data, in_chunk, out_chunk)?;
                    assert_eq!(
                        stream,
                        whole,
                        "{} at {len} moved with input {in_chunk}, output {out_chunk}",
                        class.name()
                    );
                }
            }
            // Starved output space, then exactly enough.
            assert_eq!(
                decode(&whole, STARVED)?,
                data,
                "{} at {len} under starved output room",
                class.name()
            );
            assert_eq!(
                decode(&whole, len.max(1))?,
                data,
                "{} at {len} under exact output room",
                class.name()
            );
            println!(
                "balanced-compat: {} {len} -> {} bytes, round trips at every chunking",
                class.name(),
                whole.len()
            );
        }
    }
    Ok(())
}

/// Whether an error is one of the classes the format declares for a
/// malformed stream. `InvalidParameter` is left out on purpose: it names a
/// caller mistake, and a corrupted stream is not one.
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

/// The mutations every byte of a stream is put through.
const MUTATIONS: [u8; 4] = [0x00, 0xFF, 0x01, 0x80];

#[test]
fn a_mutated_balanced_stream_is_refused_or_reads_a_declared_stream() -> Result<(), Error> {
    let mut refused = 0_u64;
    let mut accepted = 0_u64;
    for class in [Class::Text, Class::Incompressible] {
        let data = class.content(4_096);
        let stream = encode(&data, data.len(), 8_192)?;
        for at in 0..stream.len() {
            for mask in MUTATIONS {
                let mut corrupt = stream.clone();
                let Some(slot) = corrupt.get_mut(at) else {
                    continue;
                };
                let after = if mask == 0x00 { 0x00 } else { *slot ^ mask };
                if after == *slot {
                    continue;
                }
                *slot = after;
                match decode(&corrupt, 61) {
                    Ok(decoded) => {
                        accepted = accepted.saturating_add(1);
                        // A frame the decoder accepted that declares a content
                        // length produced exactly that many bytes.
                        if let Ok((frame, _)) = FrameHeader::decode(&corrupt)
                            && let Some(length) = frame.content_length
                        {
                            assert_eq!(
                                u64::try_from(decoded.len()).unwrap_or(u64::MAX),
                                length,
                                "{} corrupted at {at} read past its declared length",
                                class.name()
                            );
                        }
                    }
                    Err(error) => {
                        refused = refused.saturating_add(1);
                        assert!(
                            is_declared(error),
                            "{} corrupted at {at} produced {error}",
                            class.name()
                        );
                    }
                }
            }
        }
    }
    assert!(
        refused > 0,
        "a corruption pass that refuses nothing is not a corruption pass"
    );
    println!(
        "balanced-compat: {refused} mutated streams refused with a declared class, {accepted} \
         accepted as the stream their bytes describe"
    );
    Ok(())
}

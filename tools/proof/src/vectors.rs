//! Owns the cross-architecture proof: a fixed set of streams, produced from a catalog that is
//! code rather than data, and the comparison that decides whether two architectures wrote the
//! same bytes, read each other's, and refused the same ones.
//!
//! The catalog is code so that two lanes running the same revision produce the same request
//! without exchanging a description of it. Only the streams travel between lanes.
//!
//! Byte order is architectural, not microarchitectural, so a lane that emulates a platform
//! still settles the question this module asks. No timing is read here and no performance
//! claim is made from a lane.
//!
//! This module does not own the format's byte order. It observes it.
//!
//! # What a vector states
//!
//! A vector states the content it decodes to, by a hash of that content, and the structure the
//! encoder was asked to reach. The structure is read by a walk that expands no payload, so a
//! claim about a block type, a table description or a coded offset is checked rather than
//! assumed. A vector that no claim covers is a vector that would still pass if the encoder
//! stopped reaching the edge it was written for.
//!
//! One vector is a refusal instead. It carries bytes no encoder writes, and both lanes must
//! refuse it with the same error.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use codec::entropy::bits::{BitBuf, BitReader, BitWriter};
use codec::entropy::rans;
use codec::format::{
    BLOCK_HEADER_BYTES, BLOCK_STREAMS, BlockHeader, BlockPrologue, BlockType, Corruption,
    DecoderPolicy, Error as FormatError, FrameHeader, IntegrityMode, MATCH_DISTANCE_STREAM,
    MATCH_LENGTH_STREAM, Record, RegionHeader, RegionIndependence, ResourceClass,
};
use codec::sequence::Alphabet;
use codec::sequence::cache::REPEAT_CODE;
use codec::stream::{Decoder, Encoder, StreamState};

use crate::content::{SHAPES, Shape};
use crate::error::{Error, Result};
use crate::host;

/// The name of the file that lists what a lane produced.
const MANIFEST: &str = "manifest.txt";

/// The period of the pattern every compressed vector is built from.
///
/// It divides every block length the catalog cuts at, so a block of the pattern opens on the
/// same phase as the block before it and a match reaches exactly one period back.
const PERIOD: u64 = 64;

/// The bytes of fresh content a vector ends with when it is written to end in a literal run.
const TAIL_BYTES: usize = 48;

/// The widest distance the repeat claim has to rule out by inspection.
///
/// The decomposition gives every coded value at or below this its own symbol and no suffix
/// bit, so a coded offset that spends no suffix bit is either one of these or the repeat code.
/// The claim rules the first out by reading the content, and what is left is the repeat.
const SUFFIXLESS_DISTANCE: usize = 6;

/// The bytes a match has to agree on before it exists at all.
const MIN_MATCH: usize = 4;

/// The content a vector carries.
///
/// The position-only shapes live beside the growth curve, which reads them too. These three do
/// not: two of them exist to make one block of the catalog's layout reach one edge, and the
/// third is a function of the block length as well as the position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Content {
    /// One of the shapes the curve and the first sixteen vectors share.
    Shaped(Shape),
    /// The pattern, repeated. Every match is one period back.
    Periodic,
    /// The pattern, repeated, ending in bytes that occur nowhere before them.
    PeriodicTail,
    /// The pattern, with the second block of the region replaced by content that matches
    /// nothing, except for the one period that ends it.
    Interrupted,
}

/// What a lane must find when it reads a vector's stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    /// The stream decodes to the content the catalog describes.
    Content,
    /// The stream is refused with this corruption, and produces no content.
    Refused(Corruption),
}

/// A structural property a vector was written to carry.
///
/// Every claim is read from the frame's structure alone, by a walk that expands no payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Claim {
    /// A block whose stored payload and its decoded size differ by a coder, and no block of
    /// the frame differs by a run.
    CodedNotRun,
    /// Two regions or more open with a COMPRESSED block, which carries a table description
    /// because the first block of a region names no table in force.
    TableAtRegionStart,
    /// A COMPRESSED block names the table in force for a stream and carries no description
    /// for it.
    TableInForce,
    /// One region holds a COMPRESSED block, a RAW block and a COMPRESSED block in that order,
    /// and every coded offset of the third is the repeat code.
    RawBetweenCompressed,
    /// A COMPRESSED block ends in the terminal symbol, which is a literal run no match
    /// follows.
    Terminal,
}

impl Claim {
    const fn name(self) -> &'static str {
        match self {
            Self::CodedNotRun => "coded-not-run",
            Self::TableAtRegionStart => "table-at-region-start",
            Self::TableInForce => "table-in-force",
            Self::RawBetweenCompressed => "raw-between-compressed",
            Self::Terminal => "terminal-symbol",
        }
    }
}

/// One vector: a frame the catalog asks both lanes to write.
struct Vector {
    name: &'static str,
    class: ResourceClass,
    independence: RegionIndependence,
    integrity: IntegrityMode,
    content_length: bool,
    dictionary: bool,
    region_bytes: usize,
    block_bytes: u32,
    content: Content,
    length: usize,
    outcome: Outcome,
    claims: &'static [Claim],
}

/// The lengths the catalog covers.
///
/// Each one sits on a structure the format defines: nothing, one byte, inside a block, on a
/// block edge, on a region edge of the narrow layout, and one byte past a region edge of the
/// wide one.
const LENGTHS: [usize; 8] = [0, 1, 255, 4_096, 16_384, 65_536, 262_144, 1_048_577];

const CLASSES: [ResourceClass; 5] = [
    ResourceClass::Minimal,
    ResourceClass::Small,
    ResourceClass::Medium,
    ResourceClass::Large,
    ResourceClass::Huge,
];

/// The two layouts the catalog writes under.
///
/// The narrow one puts a region boundary every few blocks, so a short vector still holds
/// several regions. The wide one is the encoder's own default.
const NARROW: (usize, u32) = (4_096, 512);
const WIDE: (usize, u32) = (codec::stream::DEFAULT_REGION_BYTES, 65_536);

/// The vectors both lanes write, in the order they write them.
fn catalog() -> Vec<Vector> {
    // The names are stable across revisions of this catalog: a lane compares by name, and a
    // renamed vector would read as a missing one rather than as a changed one.
    const NAMES: [&str; 16] = [
        "v00-empty",
        "v01-one-byte",
        "v02-short-mixed",
        "v03-block-edge",
        "v04-multi-block",
        "v05-region-edge",
        "v06-multi-region",
        "v07-past-region",
        "v08-empty-wide",
        "v09-one-byte-wide",
        "v10-short-wide",
        "v11-block-wide",
        "v12-multi-block-wide",
        "v13-region-wide",
        "v14-multi-region-wide",
        "v15-past-region-wide",
    ];

    let mut built = Vec::new();
    for (index, name) in NAMES.iter().enumerate() {
        let narrow = index < LENGTHS.len();
        let (region_bytes, block_bytes) = if narrow { NARROW } else { WIDE };
        let length = LENGTHS
            .get(index.checked_rem(LENGTHS.len()).unwrap_or(0))
            .copied()
            .unwrap_or(0);
        built.push(Vector {
            name,
            class: CLASSES
                .get(index.checked_rem(CLASSES.len()).unwrap_or(0))
                .copied()
                .unwrap_or(ResourceClass::Small),
            independence: if index.checked_rem(2) == Some(0) {
                RegionIndependence::Independent
            } else {
                RegionIndependence::Dependent
            },
            integrity: if index.checked_rem(2) == Some(0) {
                IntegrityMode::Absent
            } else {
                IntegrityMode::PerRegion
            },
            content_length: index.checked_rem(3) == Some(0),
            dictionary: index.checked_rem(4) == Some(0),
            region_bytes,
            block_bytes,
            content: Content::Shaped(
                SHAPES
                    .get(index.checked_rem(SHAPES.len()).unwrap_or(0))
                    .copied()
                    .unwrap_or(Shape::Mixed),
            ),
            length,
            outcome: Outcome::Content,
            claims: &[],
        });
    }
    built.extend(compressed_catalog());
    built
}

/// The vectors the COMPRESSED block introduced.
///
/// The first sixteen reach none of these edges. They were written against a version that
/// stored every content byte, so every block they hold is RAW or RLE and no table, no coded
/// offset and no terminal symbol occurs in any of them.
fn compressed_catalog() -> Vec<Vector> {
    vec![
        Vector {
            name: "v16-coded-block",
            class: ResourceClass::Small,
            independence: RegionIndependence::Independent,
            integrity: IntegrityMode::Absent,
            content_length: true,
            dictionary: false,
            region_bytes: 4_096,
            block_bytes: 512,
            content: Content::Periodic,
            length: 4_096,
            outcome: Outcome::Content,
            claims: &[Claim::CodedNotRun],
        },
        Vector {
            name: "v17-table-at-region-start",
            class: ResourceClass::Small,
            independence: RegionIndependence::Independent,
            integrity: IntegrityMode::PerRegion,
            content_length: false,
            dictionary: false,
            region_bytes: 2_048,
            block_bytes: 512,
            content: Content::Periodic,
            length: 8_192,
            outcome: Outcome::Content,
            claims: &[Claim::TableAtRegionStart],
        },
        Vector {
            name: "v18-table-in-force",
            class: ResourceClass::Small,
            independence: RegionIndependence::Dependent,
            integrity: IntegrityMode::Absent,
            content_length: false,
            dictionary: true,
            region_bytes: 65_536,
            block_bytes: 512,
            content: Content::Periodic,
            length: 8_192,
            outcome: Outcome::Content,
            claims: &[Claim::TableInForce],
        },
        Vector {
            name: "v19-raw-between-compressed",
            class: ResourceClass::Small,
            independence: RegionIndependence::Independent,
            integrity: IntegrityMode::Absent,
            content_length: true,
            dictionary: false,
            region_bytes: 4_096,
            block_bytes: 512,
            content: Content::Interrupted,
            length: 1_536,
            outcome: Outcome::Content,
            claims: &[Claim::RawBetweenCompressed],
        },
        Vector {
            name: "v20-terminal-symbol",
            class: ResourceClass::Small,
            independence: RegionIndependence::Independent,
            integrity: IntegrityMode::Absent,
            content_length: false,
            dictionary: false,
            region_bytes: 4_096,
            block_bytes: 1_024,
            content: Content::PeriodicTail,
            length: 1_024,
            outcome: Outcome::Content,
            claims: &[Claim::Terminal],
        },
        Vector {
            name: "v21-unset-repeat",
            class: ResourceClass::Small,
            independence: RegionIndependence::Independent,
            integrity: IntegrityMode::Absent,
            content_length: false,
            dictionary: false,
            region_bytes: 512,
            block_bytes: 512,
            content: Content::Periodic,
            length: 512,
            outcome: Outcome::Refused(Corruption::RepeatUnset),
            claims: &[],
        },
    ]
}

/// The byte the pattern holds at a position inside one period.
///
/// Integer arithmetic on the position and nothing else, so two hosts of different byte order
/// produce the same pattern. The avalanche is what keeps the pattern free of a short internal
/// period, which the repeat claim depends on.
fn pattern_byte(index: u64) -> u8 {
    let mut state = index.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    state ^= state.wrapping_shr(29);
    state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state ^= state.wrapping_shr(32);
    u8::try_from(state.wrapping_shr(24) & 0xFF).unwrap_or(0)
}

impl Vector {
    fn header(&self) -> FrameHeader {
        let mut header = FrameHeader::new(self.class, self.independence, self.integrity);
        if self.content_length {
            header.content_length = Some(u64::try_from(self.length).unwrap_or(0));
        }
        if self.dictionary {
            header.dictionary_id = Some(0x0A0B_0C0D);
        }
        header
    }

    fn content(&self) -> Vec<u8> {
        let mut data = vec![0_u8; self.length];
        match self.content {
            Content::Shaped(shape) => shape.fill(0, &mut data),
            Content::Periodic => fill_periodic(&mut data),
            Content::PeriodicTail => {
                fill_periodic(&mut data);
                fill_tail(&mut data);
            }
            Content::Interrupted => fill_interrupted(&mut data, self.block_bytes),
        }
        data
    }

    fn file(&self) -> String {
        format!("{}.eqz", self.name)
    }

    /// The stream this vector describes.
    ///
    /// The chunking is the whole buffer in both directions. A stream is identical at every
    /// chunk size, which the permutation segments prove separately, so this segment fixes the
    /// chunking and varies only the architecture.
    fn encode(&self) -> Result<Vec<u8>> {
        let stream = self.encode_content()?;
        match self.outcome {
            Outcome::Content => Ok(stream),
            Outcome::Refused(Corruption::RepeatUnset) => unset_repeat(&stream),
            Outcome::Refused(other) => Err(Error::child(format!(
                "{} asks for a refusal, {other:?}, that the catalog cannot build",
                self.name
            ))),
        }
    }

    /// The frame the encoder writes for this vector's content.
    fn encode_content(&self) -> Result<Vec<u8>> {
        let data = self.content();
        let mut encoder = Encoder::with_layout(self.header(), self.region_bytes, self.block_bytes)?;
        let mut stream = Vec::new();
        let mut room = vec![0_u8; 65_536];
        let mut fed = 0_usize;
        while fed < data.len() {
            let rest = data.get(fed..).ok_or(FormatError::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut room)?;
            append(&mut stream, &room, progress.produced)?;
            fed = fed.saturating_add(progress.consumed);
        }
        loop {
            let progress = encoder.finish(&mut room)?;
            append(&mut stream, &room, progress.produced)?;
            if progress.state == StreamState::Finished {
                break;
            }
        }
        Ok(stream)
    }

    /// Decodes `stream` and reports whether it holds this vector's content.
    fn decodes(&self, stream: &[u8]) -> Result<()> {
        let decoded = decode(stream)?;
        if decoded == self.content() {
            Ok(())
        } else {
            Err(Error::child(format!(
                "{} decoded to {} bytes, not the {} the catalog describes",
                self.name,
                decoded.len(),
                self.length
            )))
        }
    }

    /// Holds `stream` to everything the catalog states about this vector.
    ///
    /// The content, the hash of it, the refusal when the vector is one, and every structural
    /// claim the vector carries. A lane runs this over its own streams and over the other
    /// lane's, so a stream that is byte identical and does not behave the same is still a
    /// finding.
    fn check(&self, stream: &[u8]) -> Result<()> {
        match self.outcome {
            Outcome::Content => {
                self.decodes(stream)?;
                let found = digest(&decode(stream)?);
                let stated = self.plaintext_hash();
                if found != stated {
                    return Err(Error::child(format!(
                        "{} decoded to {found}, not the {stated} the catalog states",
                        self.name
                    )));
                }
            }
            Outcome::Refused(corruption) => match decode(stream) {
                Ok(bytes) => {
                    return Err(Error::child(format!(
                        "{} decoded to {} bytes and states it is refused",
                        self.name,
                        bytes.len()
                    )));
                }
                Err(Error::Codec(FormatError::CorruptData(found))) if found == corruption => {}
                Err(other) => {
                    return Err(Error::child(format!(
                        "{} was refused with {other}, not with {corruption:?}",
                        self.name
                    )));
                }
            },
        }
        self.holds_claims(stream)
    }

    /// The hash of the content this vector decodes to.
    fn plaintext_hash(&self) -> String {
        digest(&self.content())
    }

    /// Reads every structural claim this vector carries against the frame it wrote.
    fn holds_claims(&self, stream: &[u8]) -> Result<()> {
        if self.claims.is_empty() {
            return Ok(());
        }
        let frame = walk(stream)?;
        let content = self.content();
        for claim in self.claims {
            if !frame.holds(*claim, &content) {
                return Err(Error::child(format!(
                    "{} does not carry {}",
                    self.name,
                    claim.name()
                )));
            }
        }
        Ok(())
    }

    fn manifest_line(&self, stream_bytes: usize) -> String {
        let mut line = format!(
            "{} content={} logical={} region={} block={} class={} stream={}",
            self.name,
            self.content.name(),
            self.length,
            self.region_bytes,
            self.block_bytes,
            history_label(self.class),
            stream_bytes,
        );
        match self.outcome {
            Outcome::Content => {
                let _ = write!(line, " plaintext={}", self.plaintext_hash());
            }
            Outcome::Refused(corruption) => {
                let _ = write!(line, " refuses={corruption:?}");
            }
        }
        for claim in self.claims {
            let _ = write!(line, " carries={}", claim.name());
        }
        line.push('\n');
        line
    }
}

impl Content {
    const fn name(self) -> &'static str {
        match self {
            Self::Shaped(shape) => shape.name(),
            Self::Periodic => "periodic",
            Self::PeriodicTail => "periodic-tail",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Writes the pattern, repeated, over the whole buffer.
fn fill_periodic(out: &mut [u8]) {
    for (at, slot) in out.iter_mut().enumerate() {
        let position = u64::try_from(at).unwrap_or(0);
        *slot = pattern_byte(position.checked_rem(PERIOD).unwrap_or(0));
    }
}

/// Replaces the last bytes of the buffer with content that occurs nowhere before them.
///
/// The block that holds them therefore ends in a literal run no match follows, which is the
/// one place the terminal symbol is carried.
fn fill_tail(out: &mut [u8]) {
    let start = out.len().saturating_sub(TAIL_BYTES);
    let Some(tail) = out.get_mut(start..) else {
        return;
    };
    for (at, slot) in tail.iter_mut().enumerate() {
        let position = u64::try_from(at).unwrap_or(0);
        *slot = pattern_byte(
            position
                .wrapping_add(PERIOD)
                .wrapping_mul(7)
                .wrapping_add(11),
        );
    }
}

/// Writes the pattern, with the second block of the region replaced.
///
/// The replacement matches nothing, so the encoder emits it as a RAW block. Its last period is
/// the pattern again, so the block after it opens on a match exactly one period back, which is
/// the distance the block before the RAW one left in the offset slot. Every coded offset of
/// that third block is therefore the repeat code.
fn fill_interrupted(out: &mut [u8], block_bytes: u32) {
    let block = u64::from(block_bytes);
    for (at, slot) in out.iter_mut().enumerate() {
        let position = u64::try_from(at).unwrap_or(0);
        let inside = position.checked_rem(block).unwrap_or(0);
        let second = position.checked_div(block) == Some(1);
        *slot = if second && inside < block.saturating_sub(PERIOD) {
            Shape::Mixed.at(position)
        } else {
            pattern_byte(position.checked_rem(PERIOD).unwrap_or(0))
        };
    }
}

const fn history_label(class: ResourceClass) -> &'static str {
    match class {
        ResourceClass::Minimal => "minimal",
        ResourceClass::Small => "small",
        ResourceClass::Medium => "medium",
        ResourceClass::Large => "large",
        ResourceClass::Huge => "huge",
    }
}

fn append(sink: &mut Vec<u8>, room: &[u8], produced: usize) -> Result<()> {
    let written = room.get(..produced).ok_or(FormatError::InvalidParameter)?;
    sink.extend_from_slice(written);
    Ok(())
}

/// The hex SHA-256 of a byte sequence.
fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut out = String::new();
    for byte in hasher.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The policy every lane reads a vector under.
const fn permissive() -> DecoderPolicy {
    DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes())
}

/// Decodes a whole stream through the streaming decoder.
fn decode(stream: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = Decoder::new(permissive());
    let mut room = vec![0_u8; 4_096];
    let mut out = Vec::new();
    let mut at = 0_usize;
    while at < stream.len() {
        let rest = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
        let progress = decoder.decode(rest, &mut room)?;
        append(&mut out, &room, progress.produced)?;
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            break;
        }
        if progress.consumed == 0 && progress.produced == 0 {
            break;
        }
    }
    loop {
        let progress = decoder.decode(&[], &mut room)?;
        append(&mut out, &room, progress.produced)?;
        if progress.produced == 0 {
            break;
        }
    }
    decoder.finish()?;
    Ok(out)
}

/// What one block of a frame declares, read without expanding a payload.
struct BlockFacts {
    kind: BlockType,
    decoded: u32,
    /// The content offset of this block's first decoded byte.
    at: u64,
    prologue: Option<BlockPrologue>,
}

/// One frame's structure: its regions, and the blocks each holds.
struct Frame {
    regions: Vec<Vec<BlockFacts>>,
}

/// Reads a frame's structure without expanding a payload.
///
/// Every field it reads is one a reader needs to reach the next structure, so a walk that
/// reaches the terminator is itself evidence that the type of every block is knowable without
/// decoding one.
fn walk(stream: &[u8]) -> Result<Frame> {
    let (header, mut at) = FrameHeader::decode(stream)?;
    let mut regions: Vec<Vec<BlockFacts>> = Vec::new();
    let mut produced = 0_u64;
    loop {
        let rest = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
        let (record, used) = Record::decode(rest, header.integrity)?;
        at = at.saturating_add(used);
        let region = match record {
            Record::Terminator => break,
            Record::Region(region) => region,
        };
        let mut blocks: Vec<BlockFacts> = Vec::new();
        let mut left = region.physical_size;
        while left > 0 {
            let rest = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
            let (block, used) = BlockHeader::decode(rest)?;
            at = at.saturating_add(used);
            left = left
                .checked_sub(u64::try_from(used).unwrap_or(u64::MAX))
                .ok_or(FormatError::InvalidParameter)?;
            let mut prologue = None;
            let stored = match block.kind {
                BlockType::Raw => u64::from(block.decoded_len()),
                BlockType::Rle => 1,
                BlockType::Compressed => {
                    let body = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
                    let (parsed, used) = BlockPrologue::decode(
                        body,
                        block.decoded_len(),
                        blocks.is_empty(),
                        &permissive(),
                    )?;
                    let bytes = u64::try_from(used)
                        .ok()
                        .and_then(|prologue_bytes| prologue_bytes.checked_add(parsed.body_bytes()?))
                        .ok_or(FormatError::InvalidParameter)?;
                    prologue = Some(parsed);
                    bytes
                }
            };
            blocks.push(BlockFacts {
                kind: block.kind,
                decoded: block.decoded_len(),
                at: produced,
                prologue,
            });
            produced = produced.saturating_add(u64::from(block.decoded_len()));
            at = at.saturating_add(
                usize::try_from(stored).map_err(|_| FormatError::InvalidParameter)?,
            );
            left = left
                .checked_sub(stored)
                .ok_or(FormatError::InvalidParameter)?;
        }
        regions.push(blocks);
    }
    if at != stream.len() {
        return Err(Error::child(String::from(
            "the walk did not reach the frame's end",
        )));
    }
    Ok(Frame { regions })
}

impl Frame {
    /// Whether this frame carries a claim.
    fn holds(&self, claim: Claim, content: &[u8]) -> bool {
        match claim {
            Claim::CodedNotRun => {
                self.blocks()
                    .any(|block| block.kind == BlockType::Compressed)
                    && !self.blocks().any(|block| block.kind == BlockType::Rle)
            }
            Claim::TableAtRegionStart => {
                self.regions
                    .iter()
                    .filter(|blocks| {
                        blocks
                            .first()
                            .is_some_and(|block| block.kind == BlockType::Compressed)
                    })
                    .count()
                    >= 2
            }
            Claim::TableInForce => self.blocks().any(BlockFacts::names_a_table_in_force),
            Claim::RawBetweenCompressed => self.regions.iter().any(|blocks| {
                blocks.windows(3).any(|window| {
                    window
                        .first()
                        .is_some_and(|b| b.kind == BlockType::Compressed)
                        && window.get(1).is_some_and(|b| b.kind == BlockType::Raw)
                        && window
                            .get(2)
                            .is_some_and(|b| b.offsets_are_all_repeats(content))
                })
            }),
            Claim::Terminal => self.blocks().any(BlockFacts::ends_in_the_terminal_symbol),
        }
    }

    fn blocks(&self) -> impl Iterator<Item = &BlockFacts> {
        self.regions.iter().flatten()
    }
}

impl BlockFacts {
    /// Whether this block names the table in force for a stream it carries.
    fn names_a_table_in_force(&self) -> bool {
        let Some(prologue) = self.prologue.as_ref() else {
            return false;
        };
        (0..BLOCK_STREAMS).any(|index| {
            prologue.stream(index).is_some_and(|stream| {
                stream.count > 0 && stream.repeats && stream.description_bits == 0
            })
        })
    }

    /// Whether this block ends in the terminal symbol.
    ///
    /// The terminal marks a literal run no match follows, so it is the one sequence that
    /// carries a length and no distance. A block that carries more lengths than distances
    /// carries it, and a block that carries as many of each does not.
    fn ends_in_the_terminal_symbol(&self) -> bool {
        let Some(prologue) = self.prologue.as_ref() else {
            return false;
        };
        let lengths = prologue.stream(MATCH_LENGTH_STREAM).map(|s| s.count);
        let distances = prologue.stream(MATCH_DISTANCE_STREAM).map(|s| s.count);
        match (lengths, distances) {
            (Some(lengths), Some(distances)) => lengths > distances,
            _ => false,
        }
    }

    /// Whether every coded offset this block carries is the repeat code.
    ///
    /// Read from the structure and from the content, and not by decoding the stream. The
    /// decomposition spends no suffix bit on a coded offset at or below `SUFFIXLESS_DISTANCE`
    /// and on the repeat code, and a suffix bit on every other. So a block that carries
    /// offsets and spends no suffix bit on them carries only those two kinds, and content with
    /// no match at a distance that short leaves the repeat code alone.
    fn offsets_are_all_repeats(&self, content: &[u8]) -> bool {
        if self.kind != BlockType::Compressed {
            return false;
        }
        let Some(prologue) = self.prologue.as_ref() else {
            return false;
        };
        let Some(offsets) = prologue.stream(MATCH_DISTANCE_STREAM) else {
            return false;
        };
        offsets.count > 0 && offsets.suffix_bits == 0 && no_short_match(content, self)
    }
}

/// Whether no match inside a block can sit at a distance the decomposition gives no suffix.
///
/// A match reaches back over the bytes its region produced, so the whole content before the
/// block's last byte is what has to hold no repetition that short.
fn no_short_match(content: &[u8], block: &BlockFacts) -> bool {
    let end = usize::try_from(block.at.saturating_add(u64::from(block.decoded)))
        .unwrap_or(usize::MAX)
        .min(content.len());
    for distance in 1..=SUFFIXLESS_DISTANCE {
        let mut at = distance;
        while at.saturating_add(MIN_MATCH) <= end {
            let here = content.get(at..at.saturating_add(MIN_MATCH));
            let back = at
                .checked_sub(distance)
                .and_then(|from| content.get(from..from.saturating_add(MIN_MATCH)));
            if here.is_some() && here == back {
                return false;
            }
            at = at.saturating_add(1);
        }
    }
    true
}

/// Builds the refusal vector: a region whose first coded offset names a slot nothing set.
///
/// The encoder cannot write one. A region boundary leaves the offset slot unset, so the first
/// match of a region is always coded as its own distance, and the repeat code only ever names
/// a distance an earlier match of the same region established.
///
/// So the bytes are the encoder's, with one section replaced. The block is the first of its
/// region, which is why it names no table in force and carries a description for every stream
/// it uses. Its match-distance description and payload are rebuilt over an alphabet whose only
/// symbol is the repeat code, and the extents and the declared table memory are restated for
/// what the replacement occupies. Everything else is the block the encoder wrote.
fn unset_repeat(stream: &[u8]) -> Result<Vec<u8>> {
    let (header, mut at) = FrameHeader::decode(stream)?;
    let rest = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
    let (record, used) = Record::decode(rest, header.integrity)?;
    let Record::Region(region) = record else {
        return Err(Error::child(String::from(
            "the refusal vector needs a frame that opens with a region",
        )));
    };
    at = at.saturating_add(used);

    let rest = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
    let (block, used) = BlockHeader::decode(rest)?;
    if block.kind != BlockType::Compressed || !block.last {
        return Err(Error::child(String::from(
            "the refusal vector needs a region of one COMPRESSED block",
        )));
    }
    at = at.saturating_add(used);
    let payload = stream.get(at..).ok_or(FormatError::InvalidParameter)?;

    let decoded_size = block.decoded_len();
    let (prologue, prologue_bytes) =
        BlockPrologue::decode(payload, decoded_size, true, &permissive())?;
    let body_bytes = usize::try_from(prologue.body_bytes().ok_or(FormatError::InvalidParameter)?)
        .map_err(|_| FormatError::InvalidParameter)?;
    let body = payload
        .get(prologue_bytes..prologue_bytes.saturating_add(body_bytes))
        .ok_or(FormatError::InvalidParameter)?;

    let sections = sections(&prologue, body)?;
    let offsets = prologue
        .stream(MATCH_DISTANCE_STREAM)
        .ok_or(FormatError::InvalidParameter)?;
    if offsets.count == 0 || offsets.repeats {
        return Err(Error::child(String::from(
            "the refusal vector needs a block whose own description codes its offsets",
        )));
    }

    let (description, coded, table_bytes) = repeat_only_stream(offsets.count)?;
    let replaced = replace_offsets(&prologue, &sections, &description, &coded, table_bytes)?;

    let mut out = Vec::new();
    out.extend_from_slice(
        stream
            .get(..header.encoded_len())
            .ok_or(FormatError::InvalidParameter)?,
    );
    let physical = u64::try_from(BLOCK_HEADER_BYTES.saturating_add(replaced.len()))
        .map_err(|_| FormatError::InvalidParameter)?;
    let region = RegionHeader::new(region.logical_size, physical, header.integrity)?;
    out.extend_from_slice(Record::Region(region).encode().as_bytes());
    out.extend_from_slice(
        BlockHeader::compressed(true, decoded_size)?
            .encode()
            .as_bytes(),
    );
    out.extend_from_slice(&replaced);
    out.extend_from_slice(Record::Terminator.encode().as_bytes());
    Ok(out)
}

/// One block's twelve sections, each as the bytes it occupies and the bits it declares.
struct Sections {
    groups: [[(Vec<u8>, u64); BLOCK_STREAMS]; 3],
}

/// Splits a block's stored body into its twelve sections.
fn sections(prologue: &BlockPrologue, body: &[u8]) -> Result<Sections> {
    let mut groups: [[(Vec<u8>, u64); BLOCK_STREAMS]; 3] = Default::default();
    let mut at = 0_usize;
    let extents = [
        prologue.description_bits,
        prologue.payload_bits,
        prologue.suffix_bits,
    ];
    for (group, declared) in groups.iter_mut().zip(extents.iter()) {
        for (section, &bits) in group.iter_mut().zip(declared.iter()) {
            let len =
                usize::try_from(bits.div_ceil(8)).map_err(|_| FormatError::InvalidParameter)?;
            let end = at.saturating_add(len);
            let bytes = body
                .get(at..end)
                .ok_or(FormatError::InvalidParameter)?
                .to_vec();
            *section = (bytes, bits);
            at = end;
        }
    }
    Ok(Sections { groups })
}

/// The description, the coded payload, and the table bytes of a match-distance stream whose
/// every symbol is the repeat code.
fn repeat_only_stream(count: u64) -> Result<(BitBuf, Vec<u8>, u64)> {
    let alphabet = Alphabet::MatchDistance;
    let repeat = Alphabet::MatchDistance
        .split(REPEAT_CODE)
        .map(|split| split.symbol)?;
    let symbol = usize::from(repeat);
    let span = usize::try_from(alphabet.size()).map_err(|_| FormatError::InvalidParameter)?;
    let mut counts = vec![0_u64; span];
    let slot = counts
        .get_mut(symbol)
        .ok_or(FormatError::InvalidParameter)?;
    *slot = count;

    let table = rans::Table::normalize(&counts, alphabet.size())?;
    let mut writer = BitWriter::new();
    table.describe(&mut writer);
    let description = writer.finish();
    let symbols = vec![
        u16::try_from(symbol).map_err(|_| FormatError::InvalidParameter)?;
        usize::try_from(count).map_err(|_| FormatError::InvalidParameter)?
    ];
    let coded = table.encoder()?.write(&symbols, 4)?;
    Ok((description, coded, rans::table_bytes_for(table.log())))
}

/// Rebuilds a block's stored payload with its match-distance sections replaced.
fn replace_offsets(
    prologue: &BlockPrologue,
    sections: &Sections,
    description: &BitBuf,
    coded: &[u8],
    table_bytes: u64,
) -> Result<Vec<u8>> {
    let old = sections
        .groups
        .first()
        .and_then(|group| group.get(MATCH_DISTANCE_STREAM))
        .ok_or(FormatError::InvalidParameter)?;
    let old_bytes = declared_table_bytes(&old.0, old.1)?;

    let mut rebuilt = BlockPrologue {
        table_bytes: prologue
            .table_bytes
            .checked_sub(old_bytes)
            .and_then(|kept| kept.checked_add(table_bytes))
            .ok_or(FormatError::InvalidParameter)?,
        mode: prologue.mode,
        counts: prologue.counts,
        description_bits: prologue.description_bits,
        payload_bits: prologue.payload_bits,
        suffix_bits: prologue.suffix_bits,
    };
    let description_bits = description.bits();
    let payload_bits = bits_of(coded.len())?;
    set(&mut rebuilt.description_bits, description_bits)?;
    set(&mut rebuilt.payload_bits, payload_bits)?;
    set(&mut rebuilt.suffix_bits, 0)?;

    let mut out = rebuilt.encode(true).as_bytes().to_vec();
    let replacements = [description.bytes(), coded, &[][..]];
    for (group, replacement) in sections.groups.iter().zip(replacements.iter()) {
        for (index, section) in group.iter().enumerate() {
            if index == MATCH_DISTANCE_STREAM {
                out.extend_from_slice(replacement);
            } else {
                out.extend_from_slice(&section.0);
            }
        }
    }
    Ok(out)
}

/// The table bytes a match-distance description declares.
fn declared_table_bytes(bytes: &[u8], bits: u64) -> Result<u64> {
    let mut reader = BitReader::new(bytes, bits);
    let declared = rans::Declared::parse(&mut reader, Alphabet::MatchDistance.size())?;
    Ok(declared.validate()?.table_bytes())
}

fn bits_of(bytes: usize) -> Result<u64> {
    u64::try_from(bytes)
        .ok()
        .and_then(|len| len.checked_mul(8))
        .ok_or(FormatError::InvalidParameter)
        .map_err(Error::from)
}

fn set(group: &mut [u64; BLOCK_STREAMS], bits: u64) -> Result<()> {
    let slot = group
        .get_mut(MATCH_DISTANCE_STREAM)
        .ok_or(FormatError::InvalidParameter)?;
    *slot = bits;
    Ok(())
}

/// Writes every vector of the catalog into `dir`, with the manifest that lists them.
///
/// # Errors
///
/// Fails when the directory cannot be written, or when the codec refuses a catalog entry.
pub fn write(dir: &Path) -> Result<String> {
    fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))?;
    let mut manifest = format!("# Entroq format vectors, written on {}\n", host::platform());
    let mut total = 0_usize;
    for vector in catalog() {
        let stream = vector.encode()?;
        let path = dir.join(vector.file());
        fs::write(&path, &stream).map_err(|e| Error::at("write", &path, e))?;
        manifest.push_str(&vector.manifest_line(stream.len()));
        total = total.saturating_add(stream.len());
    }
    let path = dir.join(MANIFEST);
    fs::write(&path, &manifest).map_err(|e| Error::at("write", &path, e))?;

    let mut report = String::new();
    let _ = writeln!(
        report,
        "wrote {} vectors, {total} stream bytes, into {}",
        catalog().len(),
        dir.display()
    );
    Ok(report)
}

/// What a comparison of two lanes found.
pub struct Comparison {
    pub identical: bool,
    pub findings: Vec<String>,
    pub vectors: usize,
    pub bytes: u64,
}

/// Compares the vectors two lanes wrote, and reads each lane's output against the catalog.
///
/// # Errors
///
/// Fails when a vector cannot be read. A vector that differs, or that does not behave as the
/// catalog states, is a finding rather than an error.
pub fn cross(mine: &Path, theirs: &Path) -> Result<Comparison> {
    let mut findings = Vec::new();
    let mut bytes = 0_u64;
    let vectors = catalog();

    for side in [mine, theirs] {
        let path = side.join(MANIFEST);
        if !path.is_file() {
            return Err(Error::child(format!(
                "{} holds no {MANIFEST}, so that lane wrote no vectors",
                side.display()
            )));
        }
    }

    for vector in &vectors {
        let ours = read(mine, &vector.file())?;
        let yours = read(theirs, &vector.file())?;
        bytes = bytes.saturating_add(u64::try_from(ours.len()).unwrap_or(0));

        if ours == yours {
            if let Err(failure) = vector.check(&yours) {
                findings.push(format!("{}: {failure}", vector.name));
            }
            if let Err(failure) = vector.check(&ours) {
                findings.push(format!("{}, own lane: {failure}", vector.name));
            }
        } else {
            findings.push(difference(&ours, &yours).map_or_else(
                || {
                    format!(
                        "{}: the two lanes wrote {} and {} bytes",
                        vector.name,
                        ours.len(),
                        yours.len()
                    )
                },
                |at| {
                    format!(
                        "{}: the two lanes differ at byte {at}, {} against {} bytes long",
                        vector.name,
                        ours.len(),
                        yours.len()
                    )
                },
            ));
        }
    }

    Ok(Comparison {
        identical: findings.is_empty(),
        findings,
        vectors: vectors.len(),
        bytes,
    })
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = dir.join(name);
    fs::read(&path).map_err(|e| Error::at("read", &path, e))
}

/// The first byte position at which two streams differ, when both reach it.
fn difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right.iter())
        .position(|(one, other)| one != other)
}

/// The comparison as a report.
#[must_use]
pub fn report(comparison: &Comparison, mine: &Path, theirs: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# The cross-architecture proof\n");
    let _ = writeln!(out, "Reading lane: {}", host::platform());
    let _ = writeln!(out, "Own vectors: {}", mine.display());
    let _ = writeln!(out, "Other vectors: {}", theirs.display());
    let _ = writeln!(
        out,
        "Vectors: {}, {} stream bytes\n",
        comparison.vectors, comparison.bytes
    );
    if comparison.identical {
        let _ = writeln!(
            out,
            "Every vector is byte identical across the two lanes. Each lane's output decodes \
             to the content the catalog states the hash of, carries every structure the \
             catalog claims for it, and the refusal vector is refused on both."
        );
    } else {
        let _ = writeln!(out, "The two lanes do not agree:");
        for finding in &comparison.findings {
            let _ = writeln!(out, "* {finding}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        Claim, Comparison, Content, MANIFEST, Outcome, catalog, cross, difference, report, write,
    };
    use crate::content::Shape;

    /// The content every vector decodes to, by hash.
    ///
    /// Pinned so that a change to the catalog is a change to this table. The first sixteen were
    /// written against a version that stored every content byte, and their content is what has
    /// to be unchanged now that the encoder compresses: the stream bytes are an encoder
    /// generation's and the content is the format's.
    const PLAINTEXT: [(&str, &str); 21] = [
        (
            "v00-empty",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "v01-one-byte",
            "6922e93e3827642ce4b883c756b31abf80036649d3614bf5fcb3adda43b8ea32",
        ),
        (
            "v02-short-mixed",
            "88ef1143ab40897fcd2a271cbb5ff12db7068ecda597ec5bfcd1104da91aeb66",
        ),
        (
            "v03-block-edge",
            "47b193717b9007d62d72c0814896507856147ca38df3fde65b65ae09315dffba",
        ),
        (
            "v04-multi-block",
            "2ef3176b4b27e0a697479b04dc5db0160239da861429286384b09472b07f27ce",
        ),
        (
            "v05-region-edge",
            "de2f256064a0af797747c2b97505dc0b9f3df0de4f489eac731c23ae9ca9cc31",
        ),
        (
            "v06-multi-region",
            "b9b8561490d31103a2783ddcbf67ffcb6aa02b1aa71a9800aad615aeb20c8c55",
        ),
        (
            "v07-past-region",
            "1a30b1a419ce94bfd8e70863f7a794f3c434384915dfbd6764bbe26b19a4c85b",
        ),
        (
            "v08-empty-wide",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "v09-one-byte-wide",
            "e3b98a4da31a127d4bde6e43033f66ba274cab0eb7eb1c70ec41402bf6273dd8",
        ),
        (
            "v10-short-wide",
            "80bd5cb5a9ca35dcdea1d59b5f1778f4114f6215af38004a02a99a1d37383648",
        ),
        (
            "v11-block-wide",
            "f600eca824e84a43f0691b267bd620e462c50da165c5b80e17aecb7a924f1fa8",
        ),
        (
            "v12-multi-block-wide",
            "44b3b5a59744c5ac10df87826cb5b37b0fa839b2f3cb30a22078534a583aafca",
        ),
        (
            "v13-region-wide",
            "cf33bb79ba55df0e6d1cee951c62b47f8a1be97b8c8d439b4b956393c97f7e69",
        ),
        (
            "v14-multi-region-wide",
            "2615fa1f6ebaad59a622251db029186eaab763bd693393bf33f8c9b6a5d86150",
        ),
        (
            "v15-past-region-wide",
            "2cb74edba754a81d121c9db6833704a8e7d417e5b13d1a19f4a52f007d644264",
        ),
        (
            "v16-coded-block",
            "cdaf8ac5a1dcca8651e3907c7ccb6e9ce13d499c61cd8ee7f50a467db963061a",
        ),
        (
            "v17-table-at-region-start",
            "b696a7fbc71ec54b4da82083785b01a8d41c880314bf13089a4c205e374c06f9",
        ),
        (
            "v18-table-in-force",
            "b696a7fbc71ec54b4da82083785b01a8d41c880314bf13089a4c205e374c06f9",
        ),
        (
            "v19-raw-between-compressed",
            "387748a7491d03fd4e2a09ea7ac1023205bce8f45301290a8194a6218c8d3de4",
        ),
        (
            "v20-terminal-symbol",
            "e5adef72bc509defdf1aec9b24967a947dd6f54d985ee071d06bc3499794ae7e",
        ),
    ];

    #[test]
    fn every_vector_name_is_unique_so_a_lane_compares_like_with_like() {
        let mut names: Vec<&str> = catalog().iter().map(|vector| vector.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn the_catalog_covers_every_length_and_both_layouts() {
        let vectors = catalog();
        let mut lengths: Vec<usize> = vectors
            .iter()
            .filter(|vector| matches!(vector.content, Content::Shaped(_)))
            .map(|vector| vector.length)
            .collect();
        lengths.sort_unstable();
        lengths.dedup();
        assert_eq!(lengths.len(), super::LENGTHS.len());
        assert!(vectors.iter().any(|vector| vector.region_bytes == 4_096));
        assert!(
            vectors
                .iter()
                .any(|vector| vector.region_bytes == codec::stream::DEFAULT_REGION_BYTES)
        );
    }

    #[test]
    fn every_vector_behaves_as_the_catalog_states() {
        for vector in catalog() {
            let stream = vector.encode().unwrap_or_default();
            assert!(!stream.is_empty(), "{} did not encode", vector.name);
            assert!(
                vector.check(&stream).is_ok(),
                "{} did not hold what the catalog states",
                vector.name
            );
        }
    }

    /// The content each vector decodes to is the content the catalog recorded.
    ///
    /// This is what "the vectors keep decoding unchanged" means once the encoder compresses.
    /// The stream bytes moved, because a new encoder generation chose a different
    /// representation for the same content, and the content did not.
    #[test]
    fn every_vector_decodes_to_the_plaintext_hash_the_catalog_records() {
        let vectors = catalog();
        assert_eq!(
            vectors
                .iter()
                .filter(|vector| vector.outcome == Outcome::Content)
                .count(),
            PLAINTEXT.len()
        );
        for (vector, (name, hash)) in vectors
            .iter()
            .filter(|vector| vector.outcome == Outcome::Content)
            .zip(PLAINTEXT.iter())
        {
            assert_eq!(vector.name, *name, "the catalog changed order");
            assert_eq!(&vector.plaintext_hash(), hash, "{name} changed content");
        }
    }

    /// Every edge the COMPRESSED block introduced is claimed by a vector, and held by it.
    #[test]
    fn every_edge_the_compressed_block_introduced_is_carried_by_a_vector() {
        let vectors = catalog();
        for claim in [
            Claim::CodedNotRun,
            Claim::TableAtRegionStart,
            Claim::TableInForce,
            Claim::RawBetweenCompressed,
            Claim::Terminal,
        ] {
            let carrier = vectors.iter().find(|vector| vector.claims.contains(&claim));
            assert!(carrier.is_some(), "no vector claims {}", claim.name());
            let Some(vector) = carrier else { continue };
            let stream = vector.encode().unwrap_or_default();
            assert!(
                vector.holds_claims(&stream).is_ok(),
                "{} does not carry {}",
                vector.name,
                claim.name()
            );
        }
    }

    /// The refusal vector is refused on the path a caller drives, and produces no content.
    #[test]
    fn the_refusal_vector_is_refused_rather_than_decoded() {
        let vectors = catalog();
        let refusal = vectors
            .iter()
            .find(|vector| matches!(vector.outcome, Outcome::Refused(_)));
        assert!(refusal.is_some(), "the catalog carries no refusal vector");
        let Some(vector) = refusal else { return };
        let stream = vector.encode().unwrap_or_default();
        assert!(!stream.is_empty());
        assert!(super::decode(&stream).is_err(), "{} decoded", vector.name);
        assert!(vector.check(&stream).is_ok());
    }

    #[test]
    fn the_manifest_states_what_every_vector_decodes_to_or_why_it_does_not() {
        for vector in catalog() {
            let line = vector.manifest_line(0);
            match vector.outcome {
                Outcome::Content => assert!(
                    line.contains(&format!("plaintext={}", vector.plaintext_hash())),
                    "{line}"
                ),
                Outcome::Refused(_) => assert!(line.contains("refuses="), "{line}"),
            }
            for claim in vector.claims {
                assert!(
                    line.contains(&format!("carries={}", claim.name())),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn two_lanes_that_wrote_the_same_bytes_agree() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-agree");
        let _ = std::fs::remove_dir_all(&root);
        let mine = root.join("mine");
        let theirs = root.join("theirs");
        assert!(write(&mine).is_ok());
        assert!(write(&theirs).is_ok());

        let comparison = cross(&mine, &theirs);
        assert!(comparison.is_ok(), "the comparison could not run");
        let comparison = comparison.unwrap_or(Comparison {
            identical: false,
            findings: Vec::new(),
            vectors: 0,
            bytes: 0,
        });
        assert!(comparison.identical, "{:?}", comparison.findings);
        assert!(report(&comparison, &mine, &theirs).contains("byte identical"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lane_that_wrote_one_byte_differently_is_a_finding() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-differ");
        let _ = std::fs::remove_dir_all(&root);
        let mine = root.join("mine");
        let theirs = root.join("theirs");
        assert!(write(&mine).is_ok());
        assert!(write(&theirs).is_ok());

        let victim = catalog()
            .into_iter()
            .find(|vector| vector.length > 0)
            .map(|vector| vector.file());
        let victim = victim.unwrap_or_default();
        let path = theirs.join(&victim);
        let mut bytes = std::fs::read(&path).unwrap_or_default();
        if let Some(slot) = bytes.last_mut() {
            *slot = slot.wrapping_add(1);
        }
        assert!(std::fs::write(&path, &bytes).is_ok());

        let comparison = cross(&mine, &theirs);
        assert!(comparison.is_ok(), "the comparison could not run");
        let comparison = comparison.unwrap_or(Comparison {
            identical: true,
            findings: Vec::new(),
            vectors: 0,
            bytes: 0,
        });
        assert!(!comparison.identical);
        assert!(
            comparison
                .findings
                .iter()
                .any(|finding| finding.contains("differ at byte"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_lane_is_a_failure_rather_than_an_agreement() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-missing");
        let _ = std::fs::remove_dir_all(&root);
        let mine = root.join("mine");
        assert!(write(&mine).is_ok());
        assert!(cross(&mine, &root.join("absent")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lane_writes_a_manifest_that_names_every_vector() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-manifest");
        let _ = std::fs::remove_dir_all(&root);
        assert!(write(&root).is_ok());
        let manifest = std::fs::read_to_string(root.join(MANIFEST)).unwrap_or_default();
        for vector in catalog() {
            assert!(
                manifest.contains(vector.name),
                "{} is not listed",
                vector.name
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_first_difference_is_the_position_a_reader_looks_at() {
        assert_eq!(difference(b"abc", b"abd"), Some(2));
        assert_eq!(difference(b"abc", b"abc"), None);
        assert_eq!(difference(b"abc", b"ab"), None);
    }

    #[test]
    fn the_catalog_carries_more_than_one_content_shape() {
        let vectors = catalog();
        for content in [
            Content::Shaped(Shape::Zeros),
            Content::Shaped(Shape::Mixed),
            Content::Periodic,
            Content::PeriodicTail,
            Content::Interrupted,
        ] {
            assert!(
                vectors.iter().any(|vector| vector.content == content),
                "{} is carried by no vector",
                content.name()
            );
        }
    }

    /// The pattern carries no repetition short enough to code an offset with no suffix bit,
    /// which is what the repeat claim reads the content for.
    #[test]
    fn the_pattern_holds_no_repetition_the_repeat_claim_would_mistake() {
        let mut data = vec![0_u8; 4_096];
        super::fill_periodic(&mut data);
        let block = super::BlockFacts {
            kind: codec::format::BlockType::Compressed,
            decoded: 4_096,
            at: 0,
            prologue: None,
        };
        assert!(super::no_short_match(&data, &block));
    }
}

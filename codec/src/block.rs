//! Owns the COMPRESSED block: what its payload carries, the state a decoder carries across the
//! blocks of one region, and the order the two are read in.
//!
//! This module does not own the prologue's fields, which the format module parses and
//! validates, the coders the streams pass through, the sequence representation their symbols
//! are drawn from, or the streaming machine that drives blocks.
//!
//! One coder per symbol class, in the order a block carries the streams:
//!
//! ```text
//! literal byte      canonical Huffman, single-level table
//! literal run       rANS, 1 state
//! match length      rANS, 2 states
//! match distance    rANS, 4 states
//! ```
//!
//! # The two histories
//!
//! A region carries two dependencies between its blocks: the table in force for each symbol
//! class, and the offset cache. Both are unchanged across a RAW block and across an RLE block,
//! because a decoder never sees the sequences of one and cannot update a history from it. Both
//! are discarded at a region boundary, so every dependency this representation creates stays
//! inside one region and the frame flag that declares region independence still covers all of
//! them.
//!
//! # The bound
//!
//! The bound is taken over the state the mechanism holds, which is the four tables in force,
//! and not over the four tables one block declares. The two are equal exactly when every
//! stream of the block carries symbols; a class whose stream is empty holds a table the block
//! does not declare and the decoder still holds it. So the decoder holds the figure it will
//! hold against policy, and the encoder gives each class a quarter of the ceiling, which makes
//! the state it holds bounded by the same figure whichever streams a block carries.

use crate::entropy::bits::{BitBuf, BitReader, BitWriter};
use crate::entropy::{huffman, rans};
use crate::format::{
    BLOCK_STREAMS, BlockPrologue, Corruption, DecoderPolicy, Error, LITERAL_BYTE_STREAM,
    LITERAL_RUN_STREAM, MATCH_DISTANCE_STREAM, MATCH_LENGTH_STREAM, mode_mask,
};
use crate::sequence::cache::OffsetCache;
use crate::sequence::streams::{Streams, SymbolStream};
use crate::sequence::{Alphabet, Sequences};

/// The classes a block may spend its table ceiling over.
///
/// The encoder gives each class this share, so the four tables in force sum to at most the
/// ceiling whatever the streams of one block carry.
const TABLE_SHARES: u64 = BLOCK_STREAMS as u64;

/// One COMPRESSED block, as a decoder meets it inside its region.
#[derive(Clone, Copy, Debug)]
pub struct Block<'a> {
    /// The stored bytes of this block's payload that have arrived, from its first byte.
    pub arrived: &'a [u8],
    /// The stored bytes the region still declares, this block's payload included.
    pub declared: u64,
    /// Whether this is the first block of its region, where the mode field is absent.
    pub first_in_region: bool,
    /// The bytes the region produced before this block, at most one window of them.
    pub history: &'a [u8],
}

/// The coder one symbol class is coded under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Coder {
    /// Canonical Huffman, decoded through a single-level table.
    Huffman,
    /// rANS, interleaved at the states the class affords.
    Rans {
        /// The interleaved states.
        states: usize,
    },
}

/// The coder and the alphabet of one stream.
fn class_of(stream: usize) -> Result<(Coder, Alphabet), Error> {
    let coder = match stream {
        LITERAL_BYTE_STREAM => Coder::Huffman,
        LITERAL_RUN_STREAM => Coder::Rans { states: 1 },
        MATCH_LENGTH_STREAM => Coder::Rans { states: 2 },
        MATCH_DISTANCE_STREAM => Coder::Rans { states: 4 },
        _ => return Err(Error::InvalidParameter),
    };
    let alphabet = Alphabet::ALL
        .get(stream)
        .copied()
        .ok_or(Error::InvalidParameter)?;
    Ok((coder, alphabet))
}

/// Where one section sits inside a block's stored body.
#[derive(Clone, Copy, Debug, Default)]
struct Span {
    at: usize,
    len: usize,
    bits: u64,
}

impl Span {
    /// The bytes of the body this section occupies.
    fn of<'a>(&self, body: &'a [u8]) -> Result<&'a [u8], Error> {
        let end = self
            .at
            .checked_add(self.len)
            .ok_or(Error::CorruptData(Corruption::BlockExtent))?;
        body.get(self.at..end)
            .ok_or(Error::CorruptData(Corruption::BlockExtent))
    }
}

/// Locates one group of four sections, each starting on a byte.
fn locate(bits: &[u64; BLOCK_STREAMS], at: &mut usize) -> Result<[Span; BLOCK_STREAMS], Error> {
    let mut spans = [Span::default(); BLOCK_STREAMS];
    for (span, &declared) in spans.iter_mut().zip(bits.iter()) {
        let len = usize::try_from(declared.div_ceil(8))
            .map_err(|_| Error::CorruptData(Corruption::BlockExtent))?;
        *span = Span {
            at: *at,
            len,
            bits: declared,
        };
        *at = at
            .checked_add(len)
            .ok_or(Error::CorruptData(Corruption::BlockExtent))?;
    }
    Ok(spans)
}

/// A table a decoder decodes one class under, and the states its coder is interleaved at.
#[derive(Clone, Debug)]
enum InForce {
    Huffman(huffman::DecodeTable),
    Rans(rans::DecodeTable, usize),
}

impl InForce {
    /// The bytes this table occupies.
    fn table_bytes(&self) -> u64 {
        match *self {
            Self::Huffman(ref table) => table.allocated_bytes(),
            Self::Rans(ref table, _) => table.allocated_bytes(),
        }
    }

    /// Decodes one stream, and reports the payload bits it consumed.
    fn decode(&self, section: &[u8], bits: u64, out: &mut [u16]) -> Result<u64, Error> {
        match *self {
            Self::Huffman(ref table) => {
                let mut reader = BitReader::new(section, bits);
                table.decode(&mut reader, out)?;
                Ok(reader.consumed())
            }
            Self::Rans(ref table, states) => {
                let consumed = table.decode(section, out, states)?;
                u64::try_from(consumed)
                    .ok()
                    .and_then(|bytes| bytes.checked_mul(8))
                    .ok_or(Error::CorruptData(Corruption::BlockPayload))
            }
        }
    }
}

/// A description a decoder has admitted and not yet built a table from.
#[derive(Clone, Debug)]
enum Admitted {
    Huffman(huffman::Admitted),
    Rans(rans::Admitted, usize),
}

impl Admitted {
    /// The bytes the table will occupy, computed from the description alone.
    const fn table_bytes(&self) -> u64 {
        match *self {
            Self::Huffman(ref admitted) => admitted.table_bytes(),
            Self::Rans(ref admitted, _) => admitted.table_bytes(),
        }
    }

    /// Allocates the decode table.
    fn build(&self) -> Result<InForce, Error> {
        match *self {
            Self::Huffman(ref admitted) => Ok(InForce::Huffman(admitted.build()?)),
            Self::Rans(ref admitted, states) => Ok(InForce::Rans(admitted.build()?, states)),
        }
    }
}

/// The state a decoder carries across the blocks of one region.
///
/// The table in force for each symbol class, and the offset cache. A region starts holding
/// neither.
#[derive(Debug)]
pub struct Decoder {
    tables: [Option<InForce>; BLOCK_STREAMS],
    cache: OffsetCache,
    tables_built: u64,
}

impl Decoder {
    /// The state a region starts in: no table in force, and an unset offset slot.
    #[must_use]
    pub const fn at_region_start() -> Self {
        Self {
            tables: [None, None, None, None],
            cache: OffsetCache::reset(),
            tables_built: 0,
        }
    }

    /// Discards both histories, which is what a region boundary does to them.
    ///
    /// The tables this decoder has built are a count of its own work and not a history, so a
    /// region boundary leaves the count where it is.
    pub fn reset(&mut self) {
        self.tables = [None, None, None, None];
        self.cache = OffsetCache::reset();
    }

    /// The tables this decoder has built, over every block it has read.
    ///
    /// A count and not a time. It is what makes the position of a refusal measurable: a block
    /// refused before the commitment point leaves it where it was.
    #[must_use]
    pub const fn tables_built(&self) -> u64 {
        self.tables_built
    }

    /// The bytes the tables in force occupy together.
    ///
    /// This is the state the mechanism holds, which is the figure the bound is taken over.
    #[must_use]
    pub fn table_bytes(&self) -> u64 {
        let mut total = 0u64;
        for slot in &self.tables {
            if let Some(table) = slot.as_ref() {
                total = total.saturating_add(table.table_bytes());
            }
        }
        total
    }

    /// The distance the offset slot holds, if a match of this region has set it.
    #[must_use]
    pub const fn offset_slot(&self) -> Option<u32> {
        self.cache.slot()
    }

    /// Reads one COMPRESSED block, and reports the stored bytes it spent.
    ///
    /// `out` is sized by the caller from the decoded size the block header declared, and that
    /// is the one source for the figure. Every declared quantity is validated before anything
    /// is sized from it, and the tables are built only once the block's whole requirement has
    /// been checked against what this decoder will hold.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedFeature` for a symbol model this build does not implement,
    /// `LimitExceeded` when the tables the block needs are above policy, `CorruptData` when a
    /// declared quantity contradicts another or the block's own content, and `TruncatedInput`
    /// when the stored bytes the block declares have not arrived.
    pub fn read(
        &mut self,
        block: &Block<'_>,
        policy: &DecoderPolicy,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let decoded_size =
            u32::try_from(out.len()).map_err(|_| Error::CorruptData(Corruption::BlockSize))?;
        let (prologue, used) =
            BlockPrologue::decode(block.arrived, decoded_size, block.first_in_region, policy)?;
        let body = Self::locate_body(block, &prologue, used)?;

        let mut at = 0usize;
        let descriptions = locate(&prologue.description_bits, &mut at)?;
        let payloads = locate(&prologue.payload_bits, &mut at)?;
        let suffixes = locate(&prologue.suffix_bits, &mut at)?;

        let fresh = Self::admit(&prologue, &descriptions, body)?;
        self.commit(&prologue, &fresh, policy)?;

        let mut decoded: [SymbolStream; BLOCK_STREAMS] = [
            SymbolStream::default(),
            SymbolStream::default(),
            SymbolStream::default(),
            SymbolStream::default(),
        ];
        for (index, slot) in decoded.iter_mut().enumerate() {
            let stream = prologue.stream(index).ok_or(Error::InvalidParameter)?;
            let count = usize::try_from(stream.count)
                .map_err(|_| Error::CorruptData(Corruption::BlockCount))?;
            let mut symbols = vec![0u16; count];
            if count > 0 {
                let payload = payloads.get(index).ok_or(Error::InvalidParameter)?;
                let table = self
                    .tables
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or(Error::CorruptData(Corruption::TableUnset))?;
                let spent = table.decode(payload.of(body)?, payload.bits, &mut symbols)?;
                if spent != payload.bits {
                    return Err(Error::CorruptData(Corruption::BlockPayload));
                }
            }
            let suffix = suffixes.get(index).ok_or(Error::InvalidParameter)?;
            *slot = SymbolStream::new(
                symbols,
                BitBuf::new(suffix.of(body)?.to_vec(), suffix.bits)?,
            );
        }

        let [literal_byte, literal_run, match_length, match_distance] = decoded;
        let streams = Streams::new(literal_byte, literal_run, match_length, match_distance);
        let mut cache = self.cache;
        let sequences = streams.sequences(&mut cache)?;
        expand(&sequences, block.history, out)?;
        self.cache = cache;

        used.checked_add(body.len())
            .ok_or(Error::CorruptData(Corruption::BlockExtent))
    }

    /// The block's stored body, located from the twelve extents alone.
    ///
    /// A body the region cannot hold is corruption, and a body the input does not yet hold is
    /// truncation. The twelve extents are what separate the two.
    fn locate_body<'a>(
        block: &Block<'a>,
        prologue: &BlockPrologue,
        used: usize,
    ) -> Result<&'a [u8], Error> {
        let body_bytes = prologue
            .body_bytes()
            .ok_or(Error::CorruptData(Corruption::BlockExtent))?;
        let stored = u64::try_from(used)
            .ok()
            .and_then(|prologue_bytes| prologue_bytes.checked_add(body_bytes))
            .ok_or(Error::CorruptData(Corruption::BlockExtent))?;
        if stored > block.declared {
            return Err(Error::CorruptData(Corruption::BlockExtent));
        }
        let end =
            usize::try_from(stored).map_err(|_| Error::CorruptData(Corruption::BlockExtent))?;
        block
            .arrived
            .get(used..end)
            .ok_or(Error::TruncatedInput { needed: end })
    }

    /// Parses and validates the descriptions of the streams that carry one.
    ///
    /// Nothing here allocates a table. A malformed description is refused before one exists.
    fn admit(
        prologue: &BlockPrologue,
        descriptions: &[Span; BLOCK_STREAMS],
        body: &[u8],
    ) -> Result<[Option<Admitted>; BLOCK_STREAMS], Error> {
        let mut fresh: [Option<Admitted>; BLOCK_STREAMS] = [None, None, None, None];
        for (index, slot) in fresh.iter_mut().enumerate() {
            let stream = prologue.stream(index).ok_or(Error::InvalidParameter)?;
            if stream.count == 0 || stream.repeats {
                continue;
            }
            let (coder, alphabet) = class_of(index)?;
            let span = descriptions.get(index).ok_or(Error::InvalidParameter)?;
            let mut reader = BitReader::new(span.of(body)?, span.bits);
            let admitted = match coder {
                Coder::Huffman => Admitted::Huffman(
                    huffman::Declared::parse(&mut reader, alphabet.size())?.validate()?,
                ),
                Coder::Rans { states } => Admitted::Rans(
                    rans::Declared::parse(&mut reader, alphabet.size())?.validate()?,
                    states,
                ),
            };
            if reader.consumed() != span.bits {
                return Err(Error::CorruptData(Corruption::BlockDescription));
            }
            *slot = Some(admitted);
        }
        Ok(fresh)
    }

    /// Checks the block's requirement and the state this decoder will hold, then builds.
    ///
    /// Both figures come from the descriptions and from the tables already in force, so the
    /// refusal precedes every allocation the block would size. A superseded table is freed
    /// before its replacement is built, which is what makes the validated figure the peak and
    /// not a figure the decoder passes through on its way to it.
    fn commit(
        &mut self,
        prologue: &BlockPrologue,
        fresh: &[Option<Admitted>; BLOCK_STREAMS],
        policy: &DecoderPolicy,
    ) -> Result<(), Error> {
        let mut required = 0u64;
        let mut held = 0u64;
        for index in 0..BLOCK_STREAMS {
            let stream = prologue.stream(index).ok_or(Error::InvalidParameter)?;
            let standing = self
                .tables
                .get(index)
                .and_then(Option::as_ref)
                .map_or(0, InForce::table_bytes);
            let after = fresh
                .get(index)
                .and_then(Option::as_ref)
                .map_or(standing, Admitted::table_bytes);
            held = held.saturating_add(after);
            if stream.count == 0 {
                continue;
            }
            if stream.repeats && standing == 0 {
                return Err(Error::CorruptData(Corruption::TableUnset));
            }
            required = required.saturating_add(after);
        }
        if required != prologue.table_bytes {
            return Err(Error::CorruptData(Corruption::TableMemory));
        }
        let _admitted = policy.admit_table_bytes(held)?;

        for (index, slot) in fresh.iter().enumerate() {
            let Some(admitted) = slot.as_ref() else {
                continue;
            };
            let table = self.tables.get_mut(index).ok_or(Error::InvalidParameter)?;
            *table = None;
            *table = Some(admitted.build()?);
            self.tables_built = self.tables_built.saturating_add(1);
        }
        Ok(())
    }
}

/// The state an encoder carries across the blocks of one region.
#[derive(Debug)]
pub struct Encoder {
    tables: [Option<Built>; BLOCK_STREAMS],
    cache: OffsetCache,
}

/// A table an encoder codes one class under.
#[derive(Clone, Debug)]
enum Built {
    Huffman(huffman::Code),
    Rans(rans::Table, usize),
}

impl Built {
    /// The bytes a decoder allocates for this table.
    const fn table_bytes(&self) -> u64 {
        match *self {
            Self::Huffman(ref code) => code.table_bytes(),
            Self::Rans(ref table, _) => table.table_bytes(),
        }
    }

    /// The description a decoder rebuilds this table from.
    fn describe(&self) -> BitBuf {
        let mut writer = BitWriter::new();
        match *self {
            Self::Huffman(ref code) => code.describe(&mut writer),
            Self::Rans(ref table, _) => table.describe(&mut writer),
        }
        writer.finish()
    }

    /// Codes one stream under this table.
    fn write(&self, symbols: &[u16]) -> Result<BitBuf, Error> {
        match *self {
            Self::Huffman(ref code) => {
                let mut writer = BitWriter::new();
                code.encoder()?.write(symbols, &mut writer)?;
                Ok(writer.finish())
            }
            Self::Rans(ref table, states) => {
                let bytes = table.encoder()?.write(symbols, states)?;
                let bits = u64::try_from(bytes.len())
                    .ok()
                    .and_then(|len| len.checked_mul(8))
                    .ok_or(Error::InvalidParameter)?;
                BitBuf::new(bytes, bits)
            }
        }
    }
}

impl Encoder {
    /// The state a region starts in: no table in force, and an unset offset slot.
    #[must_use]
    pub const fn at_region_start() -> Self {
        Self {
            tables: [None, None, None, None],
            cache: OffsetCache::reset(),
        }
    }

    /// Discards both histories, which is what a region boundary does to them.
    pub fn reset(&mut self) {
        *self = Self::at_region_start();
    }

    /// The bytes the tables in force occupy together.
    #[must_use]
    pub fn table_bytes(&self) -> u64 {
        let mut total = 0u64;
        for slot in &self.tables {
            if let Some(table) = slot.as_ref() {
                total = total.saturating_add(table.table_bytes());
            }
        }
        total
    }

    /// Assembles the payload of one COMPRESSED block.
    ///
    /// `repeat` names, per stream, the streams that code under the table already in force.
    /// Which streams those are is the encoder's choice and not the format's; what the format
    /// decides is that a block may carry no description for such a stream.
    ///
    /// Each class may spend a quarter of the policy ceiling on its table, so the four tables
    /// this encoder holds stay inside the ceiling whichever streams a block carries.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a stream is asked to repeat a table that cannot code it
    /// or that no block has built, and when the first block of a region is asked to repeat
    /// anything. Returns `LimitExceeded` when a table the block needs does not fit its share of
    /// the ceiling, which is the block the caller emits under another type.
    pub fn assemble(
        &mut self,
        sequences: &Sequences,
        first_in_region: bool,
        repeat: [bool; BLOCK_STREAMS],
        policy: &DecoderPolicy,
    ) -> Result<Vec<u8>, Error> {
        let mut cache = self.cache;
        let streams = Streams::of(sequences, &mut cache)?;
        let share = policy
            .max_table_bytes()
            .checked_div(TABLE_SHARES)
            .ok_or(Error::InvalidParameter)?;
        let assembled = self.code(&streams, first_in_region, repeat, share)?;

        let mut held = 0u64;
        for (index, built) in assembled.fresh.iter().enumerate() {
            let after = built.as_ref().map_or_else(
                || {
                    self.tables
                        .get(index)
                        .and_then(Option::as_ref)
                        .map_or(0, Built::table_bytes)
                },
                Built::table_bytes,
            );
            held = held.saturating_add(after);
        }
        let _held = policy.admit_table_bytes(held)?;

        let mut out = Vec::new();
        out.extend_from_slice(assembled.prologue.encode(first_in_region).as_bytes());
        for group in [
            &assembled.descriptions,
            &assembled.payloads,
            &assembled.suffixes,
        ] {
            for section in group {
                out.extend_from_slice(section.bytes());
            }
        }

        for (index, built) in assembled.fresh.into_iter().enumerate() {
            if let Some(built) = built {
                let slot = self.tables.get_mut(index).ok_or(Error::InvalidParameter)?;
                *slot = Some(built);
            }
        }
        self.cache = cache;
        Ok(out)
    }

    /// Codes the four streams and declares what each one spent.
    fn code(
        &self,
        streams: &Streams,
        first_in_region: bool,
        repeat: [bool; BLOCK_STREAMS],
        share: u64,
    ) -> Result<Assembled, Error> {
        let mut assembled = Assembled::default();
        let mut prologue = BlockPrologue {
            table_bytes: 0,
            mode: 0,
            counts: [0; BLOCK_STREAMS],
            description_bits: [0; BLOCK_STREAMS],
            payload_bits: [0; BLOCK_STREAMS],
            suffix_bits: [0; BLOCK_STREAMS],
        };

        for index in 0..BLOCK_STREAMS {
            let (coder, alphabet) = class_of(index)?;
            let stream = streams.stream(alphabet);
            let symbols = stream.symbols();
            let repeats = repeat.get(index).copied().unwrap_or(false);
            prologue.counts = spend(
                prologue.counts,
                index,
                u64::try_from(symbols.len()).unwrap_or(u64::MAX),
            )?;
            prologue.suffix_bits = spend(prologue.suffix_bits, index, stream.suffix().bits())?;
            assembled.suffixes.push(stream.suffix().clone());

            if symbols.is_empty() {
                if repeats {
                    return Err(Error::InvalidParameter);
                }
                assembled.descriptions.push(BitWriter::new().finish());
                assembled.payloads.push(BitWriter::new().finish());
                continue;
            }

            let (built, description) = if repeats {
                if first_in_region {
                    return Err(Error::InvalidParameter);
                }
                prologue.mode |= mode_mask(index)?;
                let held = self
                    .tables
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or(Error::InvalidParameter)?
                    .clone();
                (held, BitWriter::new().finish())
            } else {
                let built = build(coder, alphabet, symbols, share)?;
                let description = built.describe();
                (built, description)
            };
            prologue.description_bits =
                spend(prologue.description_bits, index, description.bits())?;
            assembled.descriptions.push(description);

            let payload = built.write(symbols)?;
            prologue.payload_bits = spend(prologue.payload_bits, index, payload.bits())?;
            assembled.payloads.push(payload);
            prologue.table_bytes = prologue.table_bytes.saturating_add(built.table_bytes());
            if !repeats {
                let slot = assembled
                    .fresh
                    .get_mut(index)
                    .ok_or(Error::InvalidParameter)?;
                *slot = Some(built);
            }
        }
        assembled.prologue = prologue;
        Ok(assembled)
    }
}

/// What one block's four streams coded to, and what its prologue declares about them.
#[derive(Debug, Default)]
struct Assembled {
    prologue: BlockPrologue,
    descriptions: Vec<BitBuf>,
    payloads: Vec<BitBuf>,
    suffixes: Vec<BitBuf>,
    fresh: [Option<Built>; BLOCK_STREAMS],
}

/// Builds the table one stream codes under, inside the share its class may spend.
fn build(coder: Coder, alphabet: Alphabet, symbols: &[u16], share: u64) -> Result<Built, Error> {
    let frequencies = frequencies(symbols, alphabet)?;
    match coder {
        Coder::Huffman => Ok(Built::Huffman(huffman::Code::build_within(
            &frequencies,
            alphabet.size(),
            share,
        )?)),
        Coder::Rans { states } => {
            let table = rans::Table::normalize(&frequencies, alphabet.size())?;
            if table.table_bytes() > share {
                return Err(Error::LimitExceeded {
                    declared: table.table_bytes(),
                    allowed: share,
                });
            }
            Ok(Built::Rans(table, states))
        }
    }
}

/// Places one declared quantity in its stream's slot.
fn spend(
    mut group: [u64; BLOCK_STREAMS],
    index: usize,
    value: u64,
) -> Result<[u64; BLOCK_STREAMS], Error> {
    let slot = group.get_mut(index).ok_or(Error::InvalidParameter)?;
    *slot = value;
    Ok(group)
}

/// The occurrences of every symbol of one alphabet.
fn frequencies(symbols: &[u16], alphabet: Alphabet) -> Result<Vec<u64>, Error> {
    let span = usize::try_from(alphabet.size()).map_err(|_| Error::InvalidParameter)?;
    let mut counts = vec![0u64; span];
    for &symbol in symbols {
        let slot = counts
            .get_mut(usize::from(symbol))
            .ok_or(Error::InvalidParameter)?;
        *slot = slot.saturating_add(1);
    }
    Ok(counts)
}

/// Writes the content one block's sequences name.
///
/// A match copies from the bytes the region has already produced: `history` is what it
/// produced before this block, and `out` is what this block has produced so far. Every step is
/// checked against the block's remaining decoded bytes and against the bytes the region holds,
/// so nothing here reaches past either.
fn expand(sequences: &Sequences, history: &[u8], out: &mut [u8]) -> Result<(), Error> {
    let literals = sequences.literals();
    let mut taken = 0usize;
    let mut written = 0usize;
    for step in sequences.steps() {
        let run =
            usize::try_from(step.run).map_err(|_| Error::CorruptData(Corruption::BlockContent))?;
        let literal_end = taken
            .checked_add(run)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        let produced_end = written
            .checked_add(run)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        let source = literals
            .get(taken..literal_end)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        let target = out
            .get_mut(written..produced_end)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        target.copy_from_slice(source);
        taken = literal_end;
        written = produced_end;

        let Some(matched) = step.matched else {
            continue;
        };
        let length = usize::try_from(matched.length)
            .map_err(|_| Error::CorruptData(Corruption::BlockContent))?;
        let distance = usize::try_from(matched.distance)
            .map_err(|_| Error::CorruptData(Corruption::MatchReach))?;
        let produced = history
            .len()
            .checked_add(written)
            .ok_or(Error::CorruptData(Corruption::MatchReach))?;
        if distance == 0 {
            return Err(Error::CorruptData(Corruption::RepeatDistance));
        }
        if distance > produced {
            return Err(Error::CorruptData(Corruption::MatchReach));
        }
        let end = written
            .checked_add(length)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        if end > out.len() {
            return Err(Error::CorruptData(Corruption::BlockContent));
        }
        let mut from = produced.saturating_sub(distance);
        while written < end {
            let byte = from
                .checked_sub(history.len())
                .map_or_else(|| history.get(from).copied(), |at| out.get(at).copied())
                .ok_or(Error::CorruptData(Corruption::MatchReach))?;
            let slot = out
                .get_mut(written)
                .ok_or(Error::CorruptData(Corruption::BlockContent))?;
            *slot = byte;
            written = written.saturating_add(1);
            from = from.saturating_add(1);
        }
    }
    if written != out.len() {
        return Err(Error::CorruptData(Corruption::BlockContent));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Block, Decoder, Encoder};
    use crate::format::{
        BLOCK_MODEL_V1, BLOCK_STREAMS, BlockPrologue, Corruption, DecoderPolicy, Error, Feature,
        LITERAL_BYTE_STREAM, LITERAL_RUN_STREAM, MATCH_DISTANCE_STREAM, MATCH_LENGTH_STREAM,
    };
    use crate::sequence::{Alphabet, MAX_MATCH_LENGTH, MIN_MATCH, Match, Sequences, Step, WINDOW};

    /// No stream repeats a table in force.
    const FRESH: [bool; BLOCK_STREAMS] = [false; BLOCK_STREAMS];

    /// A deterministic stream of values, so a failure reproduces from its seed alone.
    struct Source {
        state: u64,
    }

    impl Source {
        const fn new(seed: u64) -> Self {
            Self { state: seed | 1 }
        }

        fn byte(&mut self) -> u8 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u8::try_from((self.state >> 33) & 0xFF).unwrap_or(0)
        }
    }

    /// One block's content and the sequences that name it, built together.
    ///
    /// The content is produced by this builder and never by the module under test, so the round
    /// trip is checked against bytes a second implementation placed.
    struct Plan {
        content: Vec<u8>,
        sequences: Sequences,
    }

    struct Builder {
        history: Vec<u8>,
        produced: usize,
        literals: Vec<u8>,
        steps: Vec<Step>,
        source: Source,
        alphabet: u16,
    }

    impl Builder {
        fn new(history: &[u8], seed: u64, alphabet: u16) -> Self {
            Self {
                history: history.to_vec(),
                produced: 0,
                literals: Vec::new(),
                steps: Vec::new(),
                source: Source::new(seed),
                alphabet,
            }
        }

        fn literal(&mut self) -> u8 {
            if self.alphabet <= 1 {
                return b'a';
            }
            self.source
                .byte()
                .checked_rem(u8::try_from(self.alphabet).unwrap_or(u8::MAX))
                .unwrap_or(0)
        }

        fn step(&mut self, run: u32, matched: Option<Match>) -> Option<()> {
            for _ in 0..run {
                let byte = self.literal();
                self.literals.push(byte);
                self.history.push(byte);
                self.produced = self.produced.checked_add(1)?;
            }
            if let Some(matched) = matched {
                let distance = usize::try_from(matched.distance).ok()?;
                let start = self.history.len().checked_sub(distance)?;
                for lane in 0..usize::try_from(matched.length).ok()? {
                    let at = start.checked_add(lane)?;
                    let byte = self.history.get(at).copied()?;
                    self.history.push(byte);
                    self.produced = self.produced.checked_add(1)?;
                }
            }
            self.steps.push(Step { run, matched });
            Some(())
        }

        fn finish(self, history_bytes: usize) -> Option<Plan> {
            let content = self.history.get(history_bytes..)?.to_vec();
            let sequences = Sequences::new(self.literals, self.steps).ok()?;
            Some(Plan { content, sequences })
        }
    }

    /// The shape of one block: how long its literal runs are, and what its matches look like.
    #[derive(Clone, Copy, Debug)]
    struct Shape {
        run: u32,
        length: u32,
        distance: u32,
        alphabet: u16,
    }

    /// Builds one block of exactly `size` decoded bytes in the declared shape.
    ///
    /// The first run is widened to prime the window when the history is short, so a shape that
    /// asks for a distance the block has not yet produced still produces that distance.
    fn plan(shape: Shape, size: usize, history: &[u8], seed: u64) -> Option<Plan> {
        let mut builder = Builder::new(history, seed, shape.alphabet);
        let mut first = true;
        let distance = usize::try_from(shape.distance).ok()?;
        while builder.produced < size {
            let left = size.checked_sub(builder.produced)?;
            let reach = history.len().checked_add(builder.produced)?;
            let mut run = usize::try_from(shape.run).ok()?.min(left);
            if first && reach < distance {
                run = run.max(distance.checked_sub(reach)?).min(left);
            }
            first = false;
            let after = left.checked_sub(run)?;
            let reach = reach.checked_add(run)?;
            let room = usize::try_from(shape.length).ok()?.min(after);
            if shape.distance == 0
                || shape.distance > WINDOW
                || room < MIN_MATCH as usize
                || reach < distance
            {
                builder.step(u32::try_from(left).ok()?, None)?;
                break;
            }
            builder.step(
                u32::try_from(run).ok()?,
                Some(Match {
                    length: u32::try_from(room).ok()?,
                    distance: shape.distance,
                }),
            )?;
        }
        builder.finish(history.len())
    }

    /// The shapes every round trip is checked over.
    fn shapes() -> Vec<Shape> {
        vec![
            Shape {
                run: u32::MAX,
                length: 0,
                distance: 0,
                alphabet: 256,
            },
            Shape {
                run: 0,
                length: MIN_MATCH,
                distance: 1,
                alphabet: 256,
            },
            Shape {
                run: 3,
                length: MAX_MATCH_LENGTH,
                distance: 4,
                alphabet: 256,
            },
            Shape {
                run: 17,
                length: 31,
                distance: 1_024,
                alphabet: 256,
            },
            Shape {
                run: 1,
                length: 64,
                distance: 3,
                alphabet: 2,
            },
            Shape {
                run: 40,
                length: MIN_MATCH,
                distance: 4_093,
                alphabet: 64,
            },
        ]
    }

    /// Assembles one block and reads it back, and reports its stored payload.
    fn round_trip(
        plan: &Plan,
        history: &[u8],
        first_in_region: bool,
        repeat: [bool; BLOCK_STREAMS],
        encoder: &mut Encoder,
        decoder: &mut Decoder,
    ) -> Result<Vec<u8>, Error> {
        let policy = DecoderPolicy::CONSERVATIVE;
        let payload = encoder.assemble(&plan.sequences, first_in_region, repeat, &policy)?;
        let mut out = vec![0_u8; plan.content.len()];
        let block = Block {
            arrived: &payload,
            declared: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            first_in_region,
            history,
        };
        let spent = decoder.read(&block, &policy, &mut out)?;
        assert_eq!(
            spent,
            payload.len(),
            "the block spent other than its payload"
        );
        assert_eq!(out, plan.content, "the block did not decode to its content");
        Ok(payload)
    }

    /// One block, assembled and read at a region start.
    fn one_block(plan: &Plan) -> Result<Vec<u8>, Error> {
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        round_trip(plan, &[], true, FRESH, &mut encoder, &mut decoder)
    }

    /// Reads a prologue back and re-encodes it with one field changed.
    ///
    /// The two flags are separate because a block assembled at a region start carries no mode
    /// field, and giving it one is how a block that names a table in force there is built at
    /// all.
    fn edited(
        payload: &[u8],
        parsed_first: bool,
        written_first: bool,
        edit: impl FnOnce(&mut BlockPrologue),
    ) -> Result<Vec<u8>, Error> {
        let policy = DecoderPolicy::CONSERVATIVE;
        let (mut prologue, used) = BlockPrologue::decode(payload, u32::MAX, parsed_first, &policy)?;
        edit(&mut prologue);
        let mut out = Vec::new();
        out.extend_from_slice(prologue.encode(written_first).as_bytes());
        out.extend_from_slice(payload.get(used..).ok_or(Error::InvalidParameter)?);
        Ok(out)
    }

    /// Reads one payload at a declared stored length and a declared decoded size.
    fn read_with(
        payload: &[u8],
        declared: u64,
        size: usize,
        first_in_region: bool,
        history: &[u8],
        decoder: &mut Decoder,
    ) -> Result<usize, Error> {
        let mut out = vec![0_u8; size];
        let block = Block {
            arrived: payload,
            declared,
            first_in_region,
            history,
        };
        decoder.read(&block, &DecoderPolicy::CONSERVATIVE, &mut out)
    }

    /// Reads one payload the way a decoder that has just met it would, and reports the tables
    /// it had built when it answered.
    fn refuse(payload: &[u8], size: usize) -> (Error, u64) {
        let mut decoder = Decoder::at_region_start();
        let declared = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let error = read_with(payload, declared, size, true, &[], &mut decoder)
            .err()
            .unwrap_or(Error::InvalidParameter);
        (error, decoder.tables_built())
    }

    #[test]
    fn a_compressed_block_round_trips_at_every_shape_and_size() -> Result<(), Error> {
        let sizes = [1_usize, 2, 4_096, 16_384, 65_536];
        let mut checked = 0usize;
        for size in sizes {
            for (index, shape) in shapes().into_iter().enumerate() {
                let seed = u64::try_from(index).unwrap_or(0).saturating_add(1);
                let plan = plan(shape, size, &[], seed).ok_or(Error::InvalidParameter)?;
                assert_eq!(plan.content.len(), size, "the plan did not fill the block");
                let payload = one_block(&plan)?;
                assert!(
                    !payload.is_empty(),
                    "a block of {size} bytes in shape {index} stored nothing"
                );
                checked = checked.saturating_add(1);
            }
        }
        assert_eq!(
            checked,
            sizes.len().saturating_mul(shapes().len()),
            "a shape was skipped rather than round-tripped"
        );
        Ok(())
    }

    /// The carry: a RAW block between two COMPRESSED ones leaves both histories untouched, and
    /// the repeat code after it is what proves the offset slot crossed.
    #[test]
    fn a_region_carries_both_histories_across_a_block_that_is_not_compressed() -> Result<(), Error>
    {
        let shape = Shape {
            run: 6,
            length: 12,
            distance: 40,
            alphabet: 32,
        };
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();

        let first = plan(shape, 2_048, &[], 11).ok_or(Error::InvalidParameter)?;
        let _first = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;
        let tables = decoder.table_bytes();
        let slot = decoder.offset_slot();
        assert!(tables > 0 && slot.is_some());

        // The RAW block. Neither side sees a sequence of it, so neither history moves.
        let mut region = first.content.clone();
        let stored: Vec<u8> = (0..64_u32)
            .map(|at| u8::try_from(at & 0xFF).unwrap_or(0))
            .collect();
        region.extend_from_slice(&stored);
        assert_eq!(
            decoder.table_bytes(),
            tables,
            "a RAW block moved the tables"
        );
        assert_eq!(decoder.offset_slot(), slot, "a RAW block moved the slot");

        // The third block opens with a match at the distance the slot already holds, so its
        // first coded offset is the repeat code and it resolves only if the slot crossed.
        let distance = slot.ok_or(Error::InvalidParameter)?;
        let mut builder = Builder::new(&region, 12, 32);
        builder
            .step(
                0,
                Some(Match {
                    length: 9,
                    distance,
                }),
            )
            .ok_or(Error::InvalidParameter)?;
        builder
            .step(
                5,
                Some(Match {
                    length: 7,
                    distance,
                }),
            )
            .ok_or(Error::InvalidParameter)?;
        builder.step(3, None).ok_or(Error::InvalidParameter)?;
        let third = builder
            .finish(region.len())
            .ok_or(Error::InvalidParameter)?;
        let payload = round_trip(&third, &region, false, FRESH, &mut encoder, &mut decoder)?;

        let (prologue, _used) = BlockPrologue::decode(
            &payload,
            u32::try_from(third.content.len()).unwrap_or(u32::MAX),
            false,
            &DecoderPolicy::CONSERVATIVE,
        )?;
        let distances = prologue
            .stream(MATCH_DISTANCE_STREAM)
            .ok_or(Error::InvalidParameter)?;
        assert_eq!(distances.count, 2);
        assert_eq!(
            distances.suffix_bits, 0,
            "the repeat code carries no raw suffix, so neither distance spent one"
        );
        Ok(())
    }

    /// A region boundary discards both histories, read from the decoder's own state.
    #[test]
    fn a_region_boundary_discards_both_histories() -> Result<(), Error> {
        let shape = Shape {
            run: 6,
            length: 12,
            distance: 40,
            alphabet: 32,
        };
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let first = plan(shape, 2_048, &[], 21).ok_or(Error::InvalidParameter)?;
        let _first = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;

        let distance = decoder.offset_slot().ok_or(Error::InvalidParameter)?;
        let mut builder = Builder::new(&first.content, 22, 32);
        builder
            .step(
                0,
                Some(Match {
                    length: 9,
                    distance,
                }),
            )
            .ok_or(Error::InvalidParameter)?;
        builder.step(4, None).ok_or(Error::InvalidParameter)?;
        let next = builder
            .finish(first.content.len())
            .ok_or(Error::InvalidParameter)?;
        let payload =
            encoder.assemble(&next.sequences, false, FRESH, &DecoderPolicy::CONSERVATIVE)?;

        decoder.reset();
        assert_eq!(decoder.table_bytes(), 0, "a region boundary kept a table");
        assert_eq!(decoder.offset_slot(), None, "a region boundary kept a slot");

        let declared = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let refused = read_with(
            &payload,
            declared,
            next.content.len(),
            false,
            &first.content,
            &mut decoder,
        );
        assert_eq!(
            refused,
            Err(Error::CorruptData(Corruption::RepeatUnset)),
            "the first coded offset of the next region named a slot nothing had set"
        );
        assert!(
            decoder.tables_built() > 0,
            "the refusal is answered in the sequence loop, after the block's own tables exist"
        );
        Ok(())
    }

    /// The mode field is absent on the first block of a region, so nothing there names a table.
    #[test]
    fn the_first_block_of_a_region_cannot_express_a_mode() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 1_024, &[], 31).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let (prologue, used) = BlockPrologue::decode(
            &payload,
            u32::try_from(plan.content.len()).unwrap_or(u32::MAX),
            true,
            &DecoderPolicy::CONSERVATIVE,
        )?;
        assert_eq!(prologue.mode, 0);
        assert_eq!(
            prologue.encode(false).len(),
            used.saturating_add(1),
            "the mode field is the one byte the first block of a region does not carry"
        );

        let named = edited(&payload, true, false, |prologue| {
            prologue.mode = 0b0000_0010;
        })?;
        let (error, built) = refuse(&named, plan.content.len());
        assert!(
            matches!(error, Error::CorruptData(_)),
            "a block naming a table in force at a region start is corrupt, and was {error}"
        );
        assert_eq!(
            error,
            Error::CorruptData(Corruption::SequenceCount),
            "the byte a block spends on a mode field is read as a symbol count there, and the              four counts no longer describe one block"
        );
        assert_eq!(built, 0, "it was refused before a table existed");
        Ok(())
    }

    #[test]
    fn a_model_this_build_does_not_implement_is_an_unsupported_feature() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 41).ok_or(Error::InvalidParameter)?;
        let mut payload = one_block(&plan)?;
        assert_eq!(payload.first().copied(), Some(BLOCK_MODEL_V1));
        let slot = payload.first_mut().ok_or(Error::InvalidParameter)?;
        *slot = 7;
        let (error, built) = refuse(&payload, plan.content.len());
        assert_eq!(
            error,
            Error::UnsupportedFeature(Feature::BlockModel { model: 7 })
        );
        assert_eq!(built, 0);
        Ok(())
    }

    #[test]
    fn a_declared_peak_above_the_ceiling_is_limit_exceeded() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 51).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let raised = edited(&payload, true, true, |prologue| {
            prologue.table_bytes = 65_537;
        })?;
        let (error, built) = refuse(&raised, plan.content.len());
        assert_eq!(
            error,
            Error::LimitExceeded {
                declared: 65_537,
                allowed: 65_536
            }
        );
        assert_eq!(built, 0, "it was refused before a count was read");
        Ok(())
    }

    #[test]
    fn a_declared_peak_that_disagrees_with_the_descriptions_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 61).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let moved = edited(&payload, true, true, |prologue| {
            prologue.table_bytes = prologue.table_bytes.saturating_sub(2);
        })?;
        let (error, built) = refuse(&moved, plan.content.len());
        assert_eq!(error, Error::CorruptData(Corruption::TableMemory));
        assert_eq!(built, 0, "no table was built for a block that disagreed");
        Ok(())
    }

    #[test]
    fn a_symbol_count_above_what_the_decoded_size_admits_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 71).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let raised = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.counts.get_mut(LITERAL_BYTE_STREAM) {
                *slot = 513;
            }
        })?;
        let (error, built) = refuse(&raised, plan.content.len());
        assert_eq!(error, Error::CorruptData(Corruption::BlockCount));
        assert_eq!(built, 0, "it was refused before a description was parsed");
        Ok(())
    }

    #[test]
    fn a_literal_run_count_that_differs_from_the_match_length_count_is_corrupt() -> Result<(), Error>
    {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 81).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let moved = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.counts.get_mut(LITERAL_RUN_STREAM) {
                *slot = slot.saturating_add(1);
            }
        })?;
        let (error, built) = refuse(&moved, plan.content.len());
        assert_eq!(error, Error::CorruptData(Corruption::SequenceCount));
        assert_eq!(built, 0);

        let above = edited(&payload, true, true, |prologue| {
            let length = prologue
                .counts
                .get(MATCH_LENGTH_STREAM)
                .copied()
                .unwrap_or(0);
            if let Some(slot) = prologue.counts.get_mut(MATCH_DISTANCE_STREAM) {
                *slot = length.saturating_add(1);
            }
        })?;
        assert_eq!(
            refuse(&above, plan.content.len()).0,
            Error::CorruptData(Corruption::SequenceCount),
            "a match-distance count above the match-length count is corrupt"
        );
        Ok(())
    }

    #[test]
    fn a_description_extent_contradicting_the_table_a_stream_named_is_corrupt() -> Result<(), Error>
    {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 91).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;

        let silent = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.description_bits.get_mut(LITERAL_BYTE_STREAM) {
                *slot = 0;
            }
        })?;
        let (error, built) = refuse(&silent, plan.content.len());
        assert_eq!(error, Error::CorruptData(Corruption::BlockDescription));
        assert_eq!(built, 0);

        let repeating = edited(&payload, true, false, |prologue| {
            prologue.mode = 0b0000_0001;
        })?;
        let mut decoder = Decoder::at_region_start();
        let declared = u64::try_from(repeating.len()).unwrap_or(u64::MAX);
        assert_eq!(
            read_with(
                &repeating,
                declared,
                plan.content.len(),
                false,
                &[],
                &mut decoder
            ),
            Err(Error::CorruptData(Corruption::BlockDescription)),
            "a stream that names a table in force and declares a description carries bytes \
             nothing names"
        );
        Ok(())
    }

    #[test]
    fn a_stream_declares_no_section_its_class_does_not_carry() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 101).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let sectioned = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.suffix_bits.get_mut(LITERAL_BYTE_STREAM) {
                *slot = 8;
            }
        })?;
        let (error, built) = refuse(&sectioned, plan.content.len());
        assert_eq!(
            error,
            Error::CorruptData(Corruption::BlockSection),
            "the literal-byte alphabet carries no raw suffix"
        );
        assert_eq!(built, 0);
        Ok(())
    }

    #[test]
    fn extents_that_overspend_the_body_are_corrupt_and_a_short_body_is_truncation()
    -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 111).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let declared = u64::try_from(payload.len()).unwrap_or(u64::MAX);

        let overspent = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.suffix_bits.get_mut(MATCH_DISTANCE_STREAM) {
                *slot = slot.saturating_add(8);
            }
        })?;
        let mut decoder = Decoder::at_region_start();
        assert_eq!(
            read_with(
                &overspent,
                declared,
                plan.content.len(),
                true,
                &[],
                &mut decoder
            ),
            Err(Error::CorruptData(Corruption::BlockExtent)),
            "twelve extents that spend more than the region left for the block are corrupt"
        );
        assert_eq!(decoder.tables_built(), 0);

        let mut decoder = Decoder::at_region_start();
        let short = payload
            .get(..payload.len().saturating_sub(1))
            .unwrap_or(&[]);
        assert_eq!(
            read_with(short, declared, plan.content.len(), true, &[], &mut decoder),
            Err(Error::TruncatedInput {
                needed: payload.len()
            }),
            "a section the arrived bytes do not reach is a request for more input"
        );
        assert_eq!(decoder.tables_built(), 0);
        Ok(())
    }

    #[test]
    fn a_set_reject_reserved_bit_of_the_mode_field_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 121).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let reserved = edited(&payload, true, false, |prologue| {
            prologue.mode = 0b0001_0000;
        })?;
        let mut decoder = Decoder::at_region_start();
        let declared = u64::try_from(reserved.len()).unwrap_or(u64::MAX);
        assert_eq!(
            read_with(
                &reserved,
                declared,
                plan.content.len(),
                false,
                &[],
                &mut decoder
            ),
            Err(Error::CorruptData(Corruption::ModeReserved))
        );
        assert_eq!(decoder.tables_built(), 0);
        Ok(())
    }

    #[test]
    fn a_stream_that_names_a_table_nothing_built_is_corrupt() -> Result<(), Error> {
        let literals = Shape {
            run: u32::MAX,
            length: 0,
            distance: 0,
            alphabet: 32,
        };
        let first = plan(literals, 256, &[], 131).ok_or(Error::InvalidParameter)?;
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let _first = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;
        assert_eq!(
            first.sequences.steps().len(),
            1,
            "a block of one literal run carries no match, so no distance table exists"
        );

        let shape = Shape {
            run: 4,
            length: 8,
            distance: 16,
            alphabet: 32,
        };
        let next = plan(shape, 256, &first.content, 132).ok_or(Error::InvalidParameter)?;
        let payload =
            encoder.assemble(&next.sequences, false, FRESH, &DecoderPolicy::CONSERVATIVE)?;
        let named = edited(&payload, false, false, |prologue| {
            prologue.mode = 0b0000_1000;
            if let Some(slot) = prologue.description_bits.get_mut(MATCH_DISTANCE_STREAM) {
                *slot = 0;
            }
        })?;
        let declared = u64::try_from(named.len()).unwrap_or(u64::MAX);
        let refused = read_with(
            &named,
            declared,
            next.content.len(),
            false,
            &first.content,
            &mut decoder,
        );
        assert_eq!(refused, Err(Error::CorruptData(Corruption::TableUnset)));

        // A stream that declares no symbols names no table either, which is the other half of
        // the same rule and is answered before a description is parsed.
        let quiet = plan(literals, 256, &first.content, 133).ok_or(Error::InvalidParameter)?;
        let payload =
            encoder.assemble(&quiet.sequences, false, FRESH, &DecoderPolicy::CONSERVATIVE)?;
        let named = edited(&payload, false, false, |prologue| {
            prologue.mode = 0b0000_1000;
        })?;
        let declared = u64::try_from(named.len()).unwrap_or(u64::MAX);
        let mut fresh = Decoder::at_region_start();
        assert_eq!(
            read_with(
                &named,
                declared,
                quiet.content.len(),
                false,
                &first.content,
                &mut fresh
            ),
            Err(Error::CorruptData(Corruption::ModeStream))
        );
        assert_eq!(fresh.tables_built(), 0);
        Ok(())
    }

    #[test]
    fn a_sequence_past_the_blocks_remaining_decoded_bytes_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 4,
            length: 64,
            distance: 16,
            alphabet: 48,
        };
        let matched = plan(shape, 4_096, &[], 141).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&matched)?;
        let declared = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let mut decoder = Decoder::at_region_start();
        assert_eq!(
            read_with(&payload, declared, 4_095, true, &[], &mut decoder),
            Err(Error::CorruptData(Corruption::BlockContent)),
            "a match that runs past the block's remaining decoded bytes is corrupt"
        );

        // A block that ends on a match, so the refusal lands on the match and not on the run
        // before it.
        let mut builder = Builder::new(&[], 142, 48);
        for _ in 0..8 {
            builder
                .step(
                    16,
                    Some(Match {
                        length: 32,
                        distance: 12,
                    }),
                )
                .ok_or(Error::InvalidParameter)?;
        }
        let ends_on_match = builder.finish(0).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&ends_on_match)?;
        let declared = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let mut decoder = Decoder::at_region_start();
        let short = ends_on_match.content.len().saturating_sub(1);
        assert_eq!(
            read_with(&payload, declared, short, true, &[], &mut decoder),
            Err(Error::CorruptData(Corruption::BlockContent)),
            "a literal run or a match past them is corrupt"
        );
        Ok(())
    }

    #[test]
    fn a_match_past_the_bytes_the_region_produced_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 4,
            length: 16,
            distance: 2_000,
            alphabet: 48,
        };
        let history = vec![b'z'; 4_096];
        let plan = plan(shape, 1_024, &history, 151).ok_or(Error::InvalidParameter)?;
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let payload = round_trip(&plan, &history, true, FRESH, &mut encoder, &mut decoder)?;

        let mut decoder = Decoder::at_region_start();
        let declared = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        assert_eq!(
            read_with(
                &payload,
                declared,
                plan.content.len(),
                true,
                &[],
                &mut decoder
            ),
            Err(Error::CorruptData(Corruption::MatchReach)),
            "the same block against a region that has produced nothing reaches past it"
        );
        Ok(())
    }

    /// A distance of zero has no expression: coded value 1 names the offset slot and every
    /// value above it is a distance shifted by one, so the alphabet maps no symbol to zero.
    #[test]
    fn a_distance_of_zero_cannot_be_expressed() {
        assert_eq!(Alphabet::MatchDistance.coded_value(0), None);
        assert_eq!(Alphabet::MatchDistance.raw_value(1), None);
        assert_eq!(Alphabet::MatchDistance.raw_value(2), Some(1));
    }

    /// A block whose distance stream declares one symbol more than its matches spend.
    ///
    /// The stream carries one distinct symbol, whose rANS payload is the flushed states and
    /// nothing else, so the declared payload extent still agrees and the block reaches the
    /// exit condition that owns the count.
    #[test]
    fn a_block_that_ends_with_a_symbol_count_unspent_is_corrupt() -> Result<(), Error> {
        let mut builder = Builder::new(&[], 161, 48);
        builder
            .step(
                100,
                Some(Match {
                    length: 8,
                    distance: 64,
                }),
            )
            .ok_or(Error::InvalidParameter)?;
        builder.step(50, None).ok_or(Error::InvalidParameter)?;
        let plan = builder.finish(0).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;

        let raised = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.counts.get_mut(MATCH_DISTANCE_STREAM) {
                *slot = 2;
            }
        })?;
        let (error, _built) = refuse(&raised, plan.content.len());
        assert_eq!(error, Error::CorruptData(Corruption::SequenceCount));
        Ok(())
    }

    /// A declared quantity wider than a 64-bit value is corrupt, and the loop that read it
    /// stops rather than continuing.
    #[test]
    fn a_declared_quantity_wider_than_the_format_permits_is_corrupt() {
        let mut payload = vec![BLOCK_MODEL_V1];
        payload.extend(core::iter::repeat_n(0x80_u8, 12));
        payload.push(0);
        let (error, built) = refuse(&payload, 512);
        assert_eq!(error, Error::CorruptData(Corruption::Varint));
        assert_eq!(built, 0);
    }

    /// A payload extent is held against what the coder consumed, and not assumed from it.
    ///
    /// A stream of one distinct literal codes no bits at all, so a block that declares payload
    /// bits for it declares bits its coder never spends.
    #[test]
    fn a_payload_extent_the_coder_does_not_consume_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 1,
        };
        let plan = plan(shape, 512, &[], 201).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let (prologue, _used) = BlockPrologue::decode(
            &payload,
            u32::try_from(plan.content.len()).unwrap_or(u32::MAX),
            true,
            &DecoderPolicy::CONSERVATIVE,
        )?;
        assert_eq!(
            prologue
                .stream(LITERAL_BYTE_STREAM)
                .ok_or(Error::InvalidParameter)?
                .payload_bits,
            0,
            "a code of one symbol writes no bits"
        );

        let mut claimed = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.payload_bits.get_mut(LITERAL_BYTE_STREAM) {
                *slot = 8;
            }
        })?;
        claimed.push(0);
        let mut decoder = Decoder::at_region_start();
        let declared = u64::try_from(claimed.len()).unwrap_or(u64::MAX);
        assert_eq!(
            read_with(
                &claimed,
                declared,
                plan.content.len(),
                true,
                &[],
                &mut decoder
            ),
            Err(Error::CorruptData(Corruption::BlockPayload))
        );
        Ok(())
    }

    /// A payload extent that disagrees with what the coder consumed is corrupt.
    #[test]
    fn a_payload_extent_the_coder_does_not_spend_is_corrupt() -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let plan = plan(shape, 512, &[], 171).ok_or(Error::InvalidParameter)?;
        let payload = one_block(&plan)?;
        let moved = edited(&payload, true, true, |prologue| {
            if let Some(slot) = prologue.payload_bits.get_mut(LITERAL_BYTE_STREAM) {
                *slot = slot.saturating_sub(1);
            }
        })?;
        let (error, _built) = refuse(&moved, plan.content.len());
        assert!(
            matches!(
                error,
                Error::CorruptData(Corruption::BlockPayload | Corruption::CodedStream)
            ),
            "a payload extent the coder does not spend is corrupt, and was {error}"
        );
        Ok(())
    }

    /// The bound is taken over the state the mechanism holds.
    ///
    /// A block declares the tables it decodes under. A class whose stream carries no symbols
    /// holds a table the block does not declare, so the two figures are equal exactly when
    /// every stream carries symbols, and the decoder holds the figure it will hold against
    /// policy either way.
    #[test]
    fn the_bound_is_taken_over_the_state_the_mechanism_holds() -> Result<(), Error> {
        let policy = DecoderPolicy::CONSERVATIVE;
        let shape = Shape {
            run: 8,
            length: 16,
            distance: 24,
            alphabet: 48,
        };
        let full = plan(shape, 16_384, &[], 181).ok_or(Error::InvalidParameter)?;
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let payload = round_trip(&full, &[], true, FRESH, &mut encoder, &mut decoder)?;
        let (prologue, _used) = BlockPrologue::decode(
            &payload,
            u32::try_from(full.content.len()).unwrap_or(u32::MAX),
            true,
            &policy,
        )?;
        for index in 0..BLOCK_STREAMS {
            let stream = prologue.stream(index).ok_or(Error::InvalidParameter)?;
            assert!(
                stream.count > 0,
                "every stream of this block carries symbols"
            );
        }
        assert_eq!(
            decoder.table_bytes(),
            prologue.table_bytes,
            "the state the mechanism holds is the state this block declared"
        );
        assert!(decoder.table_bytes() <= policy.max_table_bytes());
        assert_eq!(decoder.table_bytes(), encoder.table_bytes());

        // A block with no matches leaves the distance table in force and declares nothing for
        // it, which is the case the two figures differ in.
        let literals = Shape {
            run: u32::MAX,
            length: 0,
            distance: 0,
            alphabet: 48,
        };
        let next = plan(literals, 4_096, &full.content, 182).ok_or(Error::InvalidParameter)?;
        let payload = round_trip(
            &next,
            &full.content,
            false,
            FRESH,
            &mut encoder,
            &mut decoder,
        )?;
        let (prologue, _used) = BlockPrologue::decode(
            &payload,
            u32::try_from(next.content.len()).unwrap_or(u32::MAX),
            false,
            &policy,
        )?;
        assert_eq!(
            prologue
                .stream(MATCH_DISTANCE_STREAM)
                .ok_or(Error::InvalidParameter)?
                .count,
            0
        );
        assert!(
            decoder.table_bytes() > prologue.table_bytes,
            "the decoder holds a table this block did not declare"
        );
        assert!(
            decoder.table_bytes() <= policy.max_table_bytes(),
            "and the figure it holds is the one policy admitted"
        );
        Ok(())
    }

    /// A stream decodes under the table in force, which is what the mode field carries.
    #[test]
    fn a_stream_decodes_under_the_table_already_in_force() -> Result<(), Error> {
        let matched = Match {
            length: 16,
            distance: 24,
        };
        let mut builder = Builder::new(&[], 191, 48);
        builder
            .step(24, Some(matched))
            .ok_or(Error::InvalidParameter)?;
        for _ in 0..99 {
            builder
                .step(8, Some(matched))
                .ok_or(Error::InvalidParameter)?;
        }
        let first = builder.finish(0).ok_or(Error::InvalidParameter)?;
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let fresh = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;

        // Every symbol of the three repeating streams is one the table in force already codes.
        // Which streams may repeat is the encoder's question, and the rule that answers it is
        // not this phase's.
        let mut builder = Builder::new(&first.content, 192, 48);
        for _ in 0..100 {
            builder
                .step(8, Some(matched))
                .ok_or(Error::InvalidParameter)?;
        }
        let next = builder
            .finish(first.content.len())
            .ok_or(Error::InvalidParameter)?;
        let repeat = [false, true, true, true];
        let repeated = round_trip(
            &next,
            &first.content,
            false,
            repeat,
            &mut encoder,
            &mut decoder,
        )?;
        assert!(
            repeated.len() < fresh.len(),
            "a block that transmits three fewer descriptions is smaller"
        );
        let (prologue, _used) = BlockPrologue::decode(
            &repeated,
            u32::try_from(next.content.len()).unwrap_or(u32::MAX),
            false,
            &DecoderPolicy::CONSERVATIVE,
        )?;
        assert_eq!(prologue.mode, 0b0000_1110);
        for index in [
            LITERAL_RUN_STREAM,
            MATCH_LENGTH_STREAM,
            MATCH_DISTANCE_STREAM,
        ] {
            assert_eq!(
                prologue
                    .stream(index)
                    .ok_or(Error::InvalidParameter)?
                    .description_bits,
                0
            );
        }
        Ok(())
    }
}

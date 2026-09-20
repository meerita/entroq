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
//! # The expansion bound
//!
//! A COMPRESSED block whose stored payload is not below the size it decodes to is refused as
//! corrupt, before the payload is read. The version 1 bound on what a frame may add to its
//! content then holds for every stream a decoder accepts, and not only for every stream a
//! conforming encoder wrote. It costs a conforming encoder nothing: a rule that assembles
//! every type and emits the cheapest never produces such a block.
//!
//! # The table bound
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
use crate::sequence::{Alphabet, Match, Sequences};

/// The classes a block may spend its table ceiling over.
///
/// The encoder gives each class this share, so the four tables in force sum to at most the
/// ceiling whatever the streams of one block carry.
const TABLE_SHARES: u64 = BLOCK_STREAMS as u64;

/// The width of one match-copy chunk.
///
/// Provisional at 32, pending the width measurement the selection phase runs; the kernel shape
/// does not change with the width. A match whose distance is at least this width reads only
/// bytes the region has already produced, so no chunk read reaches a byte the match is still
/// to write.
const COPY_WIDTH: usize = 32;

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
    peak_table_bytes: u64,
    poison: Option<Error>,
}

impl Decoder {
    /// The state a region starts in: no table in force, and an unset offset slot.
    #[must_use]
    pub const fn at_region_start() -> Self {
        Self {
            tables: [None, None, None, None],
            cache: OffsetCache::reset(),
            tables_built: 0,
            peak_table_bytes: 0,
            poison: None,
        }
    }

    /// Discards both histories, which is what a region boundary does to them.
    ///
    /// The tables this decoder has built and the widest set it has held are counts of its own
    /// work and not histories, so a region boundary leaves both where they are. A decoder that
    /// has failed stays failed: a region boundary is a position in a stream this decoder has
    /// already refused to read.
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

    /// The widest set of tables this decoder has ever held, over every block it has read.
    ///
    /// The figure covers the transition from one block's tables to the next one's and not only
    /// the state each block settles at, so it is the peak the memory bound is taken over and
    /// not a figure the decoder passes above on its way to it.
    #[must_use]
    pub const fn peak_table_bytes(&self) -> u64 {
        self.peak_table_bytes
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
    ///
    /// The slot is internal state with no caller, and this reads it so that the tests which
    /// prove it crosses a block and is discarded at a region boundary can see it at all.
    #[cfg(test)]
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
    /// A decoder that has refused a block is poisoned and answers every later call with the
    /// same error. The read order commits the tables before it decodes the payloads and moves
    /// the offset cache only after, so a failure between the two leaves a state no stream
    /// establishes, and reading on from it would decode against tables and a cache that no
    /// block put together.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedFeature` for a symbol model this build does not implement,
    /// `LimitExceeded` when the tables the block needs are above policy, `CorruptData` when a
    /// declared quantity contradicts another or the block's own content or when the block
    /// stores at least the bytes it decodes to, and `TruncatedInput` when the stored bytes the
    /// block declares have not arrived.
    pub fn read(
        &mut self,
        block: &Block<'_>,
        policy: &DecoderPolicy,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if let Some(error) = self.poison {
            return Err(error);
        }
        match self.expand_block(block, policy, out) {
            Ok(spent) => Ok(spent),
            Err(error) => {
                self.poison = Some(error);
                Err(error)
            }
        }
    }

    /// Reads one block, leaving this decoder in whatever state the failure reached.
    ///
    /// A block is read in an order that commits before it decodes: the tables are built once
    /// the block's whole requirement is admitted, and the offset cache moves only once the
    /// block has produced every byte it declared. So a failure between the two leaves the
    /// tables of this block beside the cache of the block before it, which is a state no
    /// stream establishes. `read` is what makes that state unreachable, by refusing to read
    /// again from it.
    fn expand_block(
        &mut self,
        block: &Block<'_>,
        policy: &DecoderPolicy,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let decoded_size =
            u32::try_from(out.len()).map_err(|_| Error::CorruptData(Corruption::BlockSize))?;
        let (prologue, used) =
            BlockPrologue::decode(block.arrived, decoded_size, block.first_in_region, policy)?;
        let body = Self::locate_body(block, &prologue, used, decoded_size)?;

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
        decoded_size: u32,
    ) -> Result<&'a [u8], Error> {
        let body_bytes = prologue
            .body_bytes()
            .ok_or(Error::CorruptData(Corruption::BlockExtent))?;
        let stored = u64::try_from(used)
            .ok()
            .and_then(|prologue_bytes| prologue_bytes.checked_add(body_bytes))
            .ok_or(Error::CorruptData(Corruption::BlockExtent))?;
        if stored >= u64::from(decoded_size) {
            return Err(Error::CorruptData(Corruption::BlockExpansion));
        }
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
    /// refusal precedes every allocation the block would size.
    ///
    /// The build is two passes, and the order is the bound. Every table this block replaces is
    /// freed before any replacement is built, so the state grows from the tables the block
    /// keeps to the figure policy admitted and passes above neither. Freeing and building one
    /// stream at a time would hold a new table beside the old tables of the streams not
    /// reached yet, and that sum is above the admitted figure whenever a block replaces a small
    /// table with a large one.
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
            // A table in force and the bytes it occupies are different facts, and only the
            // first says whether a stream may name it. A Huffman code over one distinct symbol
            // writes no bits and needs no table, so it is in force at zero bytes, and reading
            // the figure for the fact would refuse the block that names it.
            let standing = self.tables.get(index).and_then(Option::as_ref);
            let after = fresh.get(index).and_then(Option::as_ref).map_or_else(
                || standing.map_or(0, InForce::table_bytes),
                Admitted::table_bytes,
            );
            held = held.saturating_add(after);
            if stream.count == 0 {
                continue;
            }
            if stream.repeats && standing.is_none() {
                return Err(Error::CorruptData(Corruption::TableUnset));
            }
            required = required.saturating_add(after);
        }
        if required != prologue.table_bytes {
            return Err(Error::CorruptData(Corruption::TableMemory));
        }
        let _admitted = policy.admit_table_bytes(held)?;

        for (index, slot) in fresh.iter().enumerate() {
            if slot.is_none() {
                continue;
            }
            let table = self.tables.get_mut(index).ok_or(Error::InvalidParameter)?;
            *table = None;
        }
        self.observe_peak();
        for (index, slot) in fresh.iter().enumerate() {
            let Some(admitted) = slot.as_ref() else {
                continue;
            };
            let table = self.tables.get_mut(index).ok_or(Error::InvalidParameter)?;
            *table = Some(admitted.build()?);
            self.tables_built = self.tables_built.saturating_add(1);
            self.observe_peak();
        }
        Ok(())
    }

    /// Records the tables in force against the widest set this decoder has ever held.
    ///
    /// Called after the tables a block replaces are freed and after each replacement is built,
    /// so the figure covers the transition and not only the state a block settles at. It is
    /// four additions and a comparison per table built, which is once per description and not
    /// once per symbol.
    fn observe_peak(&mut self) {
        self.peak_table_bytes = self.peak_table_bytes.max(self.table_bytes());
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
    /// Whether this table can code every symbol of `symbols`.
    ///
    /// A table built for another block carries the symbols that block held. One that does not
    /// carry a symbol of this block cannot code it, so a stream may not name it in force.
    fn carries(&self, symbols: &[u16]) -> bool {
        symbols.iter().all(|&symbol| match *self {
            Self::Huffman(ref code) => code.carries(symbol),
            Self::Rans(ref table, _) => table.carries(symbol),
        })
    }

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

    /// The bytes a decoder allocates for the tables this encoder holds in force.
    ///
    /// The encoder itself never allocates them; the figure is what the blocks it writes ask a
    /// decoder for. It reads internal state with no caller, and exists so that the tests which
    /// hold the encoder to the same ceiling as the decoder can see it.
    #[cfg(test)]
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

    /// Discards both histories, which is what a region boundary does to them.
    pub fn reset(&mut self) {
        *self = Self::at_region_start();
    }

    /// Assembles the payload of one COMPRESSED block into `out`, leaving this encoder unmoved.
    ///
    /// `out` is cleared and filled with the block's stored payload. Nothing here changes the
    /// two histories: the caller decides whether this block is the one it emits, and a block
    /// it discards must leave no trace, because a decoder never sees a candidate. `adopt`
    /// takes the returned assembly when the caller emits it.
    ///
    /// `terms.repeat` states how a stream may name the table already in force for its class.
    /// Which streams do is the encoder's choice and not the format's; what the format decides
    /// is that a block may carry no description for such a stream.
    ///
    /// `ceiling` is the stored length this block must stay below, which is the decoded size a
    /// decoder refuses a COMPRESSED block at. An assembly that does not is answered with
    /// `None` and nothing is written, so `out` never holds more than `ceiling` bytes.
    ///
    /// Each class may spend a quarter of the policy ceiling on its table, so the four tables
    /// this encoder holds stay inside the ceiling whichever streams a block carries.
    ///
    /// Coding one block allocates in proportion to that block and frees it before this call
    /// returns. Nothing allocated here is held between calls.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a stream is named to repeat a table that cannot code it
    /// or that no block has built, and when the first block of a region is named to repeat
    /// anything. Returns `LimitExceeded` when a table the block needs does not fit its share of
    /// the ceiling, which is the block the caller emits under another type.
    pub fn assemble_into(
        &self,
        sequences: &Sequences,
        terms: Terms,
        policy: &DecoderPolicy,
        out: &mut Vec<u8>,
    ) -> Result<Option<Assembly>, Error> {
        let Terms {
            first_in_region,
            ceiling,
            ..
        } = terms;
        let mut cache = self.cache;
        let streams = Streams::of(sequences, &mut cache)?;
        let share = policy
            .max_table_bytes()
            .checked_div(TABLE_SHARES)
            .ok_or(Error::InvalidParameter)?;
        let assembled = self.code(&streams, first_in_region, terms, share)?;

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

        let prologue = assembled.prologue.encode(first_in_region);
        let body = assembled
            .prologue
            .body_bytes()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(Error::InvalidParameter)?;
        let stored = prologue
            .len()
            .checked_add(body)
            .ok_or(Error::InvalidParameter)?;
        if stored >= ceiling {
            return Ok(None);
        }

        out.clear();
        out.extend_from_slice(prologue.as_bytes());
        for group in [
            &assembled.descriptions,
            &assembled.payloads,
            &assembled.suffixes,
        ] {
            for section in group {
                out.extend_from_slice(section.bytes());
            }
        }

        Ok(Some(Assembly {
            fresh: assembled.fresh,
            cache,
            repeated: assembled.repeated,
        }))
    }

    /// Takes the two histories of an assembly this encoder produced and the caller emitted.
    ///
    /// A block that was assembled and not emitted is never adopted, because a decoder that
    /// never saw it rebuilds neither history from it.
    pub fn adopt(&mut self, assembly: Assembly) {
        for (index, built) in assembly.fresh.into_iter().enumerate() {
            if let (Some(built), Some(slot)) = (built, self.tables.get_mut(index)) {
                *slot = Some(built);
            }
        }
        self.cache = assembly.cache;
    }

    /// Codes the four streams and declares what each one spent.
    fn code(
        &self,
        streams: &Streams,
        first_in_region: bool,
        terms: Terms,
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
            let named = terms.names(index);
            prologue.counts = spend(
                prologue.counts,
                index,
                u64::try_from(symbols.len()).unwrap_or(u64::MAX),
            )?;
            prologue.suffix_bits = spend(prologue.suffix_bits, index, stream.suffix().bits())?;
            assembled.suffixes.push(stream.suffix().clone());

            if symbols.is_empty() {
                if named {
                    return Err(Error::InvalidParameter);
                }
                assembled.descriptions.push(BitWriter::new().finish());
                assembled.payloads.push(BitWriter::new().finish());
                continue;
            }

            let in_force = if first_in_region {
                None
            } else {
                self.tables.get(index).and_then(Option::as_ref)
            };
            let written = if named {
                let held = in_force.ok_or(Error::InvalidParameter)?;
                Coded {
                    description: BitWriter::new().finish(),
                    payload: held.write(symbols)?,
                    built: held.clone(),
                    repeats: true,
                }
            } else if terms.repeat.is_some() {
                let (fresh, counts) = Self::fresh(coder, alphabet, symbols, share)?;
                drop(counts);
                fresh
            } else {
                let (fresh, counts) = Self::fresh(coder, alphabet, symbols, share)?;
                let chosen = Self::cheaper(fresh, &counts, in_force, symbols)?;
                drop(counts);
                chosen
            };

            if written.repeats {
                prologue.mode |= mode_mask(index)?;
                assembled.repeated = spend_flag(assembled.repeated, index, true)?;
            } else {
                let slot = assembled
                    .fresh
                    .get_mut(index)
                    .ok_or(Error::InvalidParameter)?;
                *slot = Some(written.built.clone());
            }
            prologue.description_bits =
                spend(prologue.description_bits, index, written.description.bits())?;
            prologue.payload_bits = spend(prologue.payload_bits, index, written.payload.bits())?;
            prologue.table_bytes = prologue
                .table_bytes
                .saturating_add(written.built.table_bytes());
            assembled.descriptions.push(written.description);
            assembled.payloads.push(written.payload);
        }
        assembled.prologue = prologue;
        Ok(assembled)
    }

    /// One stream coded under a table built for it, with the description that rebuilds it.
    ///
    /// Returns the histogram the table was built over alongside the coded stream, so the reuse
    /// decision that follows reads the same counts without a second pass.
    fn fresh(
        coder: Coder,
        alphabet: Alphabet,
        symbols: &[u16],
        share: u64,
    ) -> Result<(Coded, Vec<u64>), Error> {
        let (built, counts) = build(coder, alphabet, symbols, share)?;
        let description = built.describe();
        let payload = built.write(symbols)?;
        Ok((
            Coded {
                description,
                payload,
                built,
                repeats: false,
            },
            counts,
        ))
    }

    /// The cheaper of coding a stream fresh and coding it under the table in force.
    ///
    /// The trigger is self-financing and it is per stream: a stream names the table in force
    /// only when the bits it then spends are fewer than the description and the payload a
    /// fresh table would cost together. The mode field is not on either side of the
    /// comparison, because a block that is not the first of its region carries it whether or
    /// not a bit of it is set.
    ///
    /// A literal-byte stream costs its held table analytically from the counts the fresh table
    /// was built over, which is exactly what a trial encode would report, so a fresh win skips
    /// the held encode and its buffer. Every other class runs the exact production trial.
    fn cheaper(
        fresh: Coded,
        counts: &[u64],
        in_force: Option<&Built>,
        symbols: &[u16],
    ) -> Result<Coded, Error> {
        let Some(held) = in_force else {
            return Ok(fresh);
        };
        if !held.carries(symbols) {
            return Ok(fresh);
        }
        let spent = fresh
            .description
            .bits()
            .saturating_add(fresh.payload.bits());
        let payload = match *held {
            Built::Huffman(ref code) => {
                if code.encoded_bits_for_counts(counts)? >= spent {
                    return Ok(fresh);
                }
                held.write(symbols)?
            }
            Built::Rans(..) => {
                let payload = held.write(symbols)?;
                if payload.bits() >= spent {
                    return Ok(fresh);
                }
                payload
            }
        };
        Ok(Coded {
            description: BitWriter::new().finish(),
            payload,
            built: held.clone(),
            repeats: true,
        })
    }
}

/// The terms one block is assembled under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Terms {
    /// Whether this is the first block of its region, where the mode field is absent.
    pub first_in_region: bool,
    /// The streams that name the table already in force for their class, whatever it costs
    /// them, or `None` for the trigger that names one only when it spends fewer bits.
    ///
    /// No shipped path names a stream. The field exists so that the gate which holds the
    /// trigger against every mask a block admits can assemble those masks.
    pub repeat: Option<[bool; BLOCK_STREAMS]>,
    /// The stored length this block must stay below, which is the decoded size a decoder
    /// refuses a COMPRESSED block at.
    pub ceiling: usize,
}

impl Terms {
    /// Whether the caller named one stream outright.
    fn names(&self, stream: usize) -> bool {
        self.repeat
            .is_some_and(|mask| mask.get(stream).copied().unwrap_or(false))
    }
}

/// The two histories one assembled block leaves behind, and what its streams did.
///
/// An assembly is produced without changing the encoder that made it, so a caller that
/// assembles several candidates and emits one passes that one to `adopt` and drops the rest.
#[derive(Debug)]
pub struct Assembly {
    fresh: [Option<Built>; BLOCK_STREAMS],
    cache: OffsetCache,
    repeated: [bool; BLOCK_STREAMS],
}

impl Assembly {
    /// The streams of this block that named the table already in force for their class.
    #[must_use]
    pub const fn repeated(&self) -> [bool; BLOCK_STREAMS] {
        self.repeated
    }
}

/// One stream's coded form, and the table it was coded under.
#[derive(Debug)]
struct Coded {
    description: BitBuf,
    payload: BitBuf,
    built: Built,
    repeats: bool,
}

/// What one block's four streams coded to, and what its prologue declares about them.
#[derive(Debug, Default)]
struct Assembled {
    prologue: BlockPrologue,
    descriptions: Vec<BitBuf>,
    payloads: Vec<BitBuf>,
    suffixes: Vec<BitBuf>,
    fresh: [Option<Built>; BLOCK_STREAMS],
    repeated: [bool; BLOCK_STREAMS],
}

/// Builds the table one stream codes under, inside the share its class may spend.
///
/// Returns the histogram the table was built over alongside the table. The caller holds the one
/// pass per stream through the reuse decision, then drops it with the block.
fn build(
    coder: Coder,
    alphabet: Alphabet,
    symbols: &[u16],
    share: u64,
) -> Result<(Built, Vec<u64>), Error> {
    let frequencies = frequencies(symbols, alphabet)?;
    match coder {
        Coder::Huffman => Ok((
            Built::Huffman(huffman::Code::build_within(
                &frequencies,
                alphabet.size(),
                share,
            )?),
            frequencies,
        )),
        Coder::Rans { states } => {
            let table = rans::Table::normalize(&frequencies, alphabet.size())?;
            if table.table_bytes() > share {
                return Err(Error::LimitExceeded {
                    declared: table.table_bytes(),
                    allowed: share,
                });
            }
            Ok((Built::Rans(table, states), frequencies))
        }
    }
}

/// Places one flag in its stream's slot.
fn spend_flag(
    mut group: [bool; BLOCK_STREAMS],
    index: usize,
    value: bool,
) -> Result<[bool; BLOCK_STREAMS], Error> {
    let slot = group.get_mut(index).ok_or(Error::InvalidParameter)?;
    *slot = value;
    Ok(group)
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
pub fn expand(sequences: &Sequences, history: &[u8], out: &mut [u8]) -> Result<(), Error> {
    #[cfg(test)]
    capture::record(sequences, history, out.len());
    #[cfg(feature = "width-selection")]
    match crate::width_selection::selected() {
        16 => return expand_width::<16>(sequences, history, out),
        64 => return expand_width::<64>(sequences, history, out),
        0 => return expand_rule(sequences, history, out),
        _ => {}
    }
    expand_width::<COPY_WIDTH>(sequences, history, out)
}

/// Temporary: the length-directed rule arm of the width-selection campaign.
///
/// A match of at least 64 bytes at a distance of at least 64 copies in 64-byte chunks; a
/// shorter one copies in 16-byte chunks; a match with a distance below 16 takes the scalar
/// path. Removed when the width is selected.
#[cfg(feature = "width-selection")]
fn expand_rule(sequences: &Sequences, history: &[u8], out: &mut [u8]) -> Result<(), Error> {
    let literals = sequences.literals();
    let logical = out.len();
    let h = history.len();
    let mut taken = 0usize;
    let mut written = 0usize;
    for step in sequences.steps() {
        let run =
            usize::try_from(step.run).map_err(|_| Error::CorruptData(Corruption::BlockContent))?;
        taken = copy_literals(literals, out, taken, written, run)?;
        written = written
            .checked_add(run)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        let Some(matched) = step.matched else {
            continue;
        };
        let (from, end, distance) = admit_match(h, written, logical, matched)?;
        let length = end
            .checked_sub(written)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        let done = if length >= 64 && distance >= 64 {
            converge::<64>(history, out, written, from, end, h)?
        } else if length >= 16 && distance >= 16 {
            converge::<16>(history, out, written, from, end, h)?
        } else {
            written
        };
        if done < end {
            let rest = from
                .checked_add(done.saturating_sub(written))
                .ok_or(Error::CorruptData(Corruption::MatchReach))?;
            copy_match_scalar(history, out, done, end, rest)?;
        }
        written = end;
    }
    if written != logical {
        return Err(Error::CorruptData(Corruption::BlockContent));
    }
    Ok(())
}

/// Temporary: the bulk half of one rule arm, answering the destination it reached.
#[cfg(feature = "width-selection")]
fn converge<const K: usize>(
    history: &[u8],
    out: &mut [u8],
    written: usize,
    from: usize,
    end: usize,
    h: usize,
) -> Result<usize, Error> {
    let length = end
        .checked_sub(written)
        .ok_or(Error::CorruptData(Corruption::BlockContent))?;
    if from >= h {
        copy_out_bulk::<K>(out, written, from.saturating_sub(h), end)
    } else if from
        .checked_add(length)
        .is_some_and(|source_end| source_end <= h)
    {
        copy_history_bulk::<K>(history, out, written, from, end)
    } else {
        Ok(written)
    }
}

/// The scalar expansion body: the oracle every optimized match-copy path answers to.
///
/// It validates and copies exactly as the format contract requires, one byte at a time. The
/// scalar match copy it calls is the same one the production path falls back to, so the two
/// cannot drift on the copy itself. It is compiled only for the tests that compare against it;
/// production expands through the width-chunked body.
#[cfg(test)]
fn expand_scalar(sequences: &Sequences, history: &[u8], out: &mut [u8]) -> Result<(), Error> {
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
        let from = produced.saturating_sub(distance);
        copy_match_scalar(history, out, written, end, from)?;
        written = end;
    }
    if written != out.len() {
        return Err(Error::CorruptData(Corruption::BlockContent));
    }
    Ok(())
}

/// Copies one match's bytes from the bytes its region has already produced.
///
/// The source may sit in `history`, in `out`, or cross the boundary between them. An
/// overlapping source propagates one byte at a time, which is what a match whose length
/// exceeds its distance requires. The caller has validated the length against the logical
/// block end and the distance against the produced bytes, so no read or write reaches past
/// either.
fn copy_match_scalar(
    history: &[u8],
    out: &mut [u8],
    mut written: usize,
    end: usize,
    mut from: usize,
) -> Result<(), Error> {
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
    Ok(())
}

/// Validates one match exactly as the scalar oracle does, before its copy.
///
/// The scalar path makes the same checks per byte; with the same per-match values every
/// per-byte check is implied, and the emit order is unchanged, so the accept set and the
/// refusal class are the oracle's. Answers the absolute source position, the absolute end, and
/// the distance.
#[inline]
fn admit_match(
    history_len: usize,
    written: usize,
    logical: usize,
    matched: Match,
) -> Result<(usize, usize, usize), Error> {
    let length = usize::try_from(matched.length)
        .map_err(|_| Error::CorruptData(Corruption::BlockContent))?;
    let distance = usize::try_from(matched.distance)
        .map_err(|_| Error::CorruptData(Corruption::MatchReach))?;
    let produced = history_len
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
    if end > logical {
        return Err(Error::CorruptData(Corruption::BlockContent));
    }
    Ok((produced.saturating_sub(distance), end, distance))
}

/// Copies one literal run, with the oracle's checks and error classes.
#[inline]
fn copy_literals(
    literals: &[u8],
    out: &mut [u8],
    taken: usize,
    written: usize,
    run: usize,
) -> Result<usize, Error> {
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
    Ok(literal_end)
}

/// Copies one `K`-byte chunk whose source is inside the current block output.
///
/// The `distance >= K` contract puts `src + K` at or before `dst`, so the chunk reads only
/// bytes the match has already produced and the two slices do not alias.
#[inline]
fn copy_out_chunk<const K: usize>(out: &mut [u8], dst: usize, src: usize) -> Result<(), Error> {
    let (head, tail) = out.split_at_mut(dst);
    let src_end = src
        .checked_add(K)
        .ok_or(Error::CorruptData(Corruption::MatchReach))?;
    let source = head
        .get(src..src_end)
        .ok_or(Error::CorruptData(Corruption::MatchReach))?;
    let target = tail
        .get_mut(..K)
        .ok_or(Error::CorruptData(Corruption::BlockContent))?;
    target.copy_from_slice(source);
    Ok(())
}

/// Bulk-copies whole chunks of a match whose source is the current block output.
///
/// Every chunk read ends at or before the write cursor because the distance is at least `K`.
/// Only chunks whose write ends at or before the match end are copied; the caller scalars the
/// remainder, so no write reaches past the match end. Answers the destination it reached.
fn copy_out_bulk<const K: usize>(
    out: &mut [u8],
    written: usize,
    src: usize,
    end: usize,
) -> Result<usize, Error> {
    let mut dst = written;
    let mut at = src;
    while dst.checked_add(K).is_some_and(|chunk_end| chunk_end <= end) {
        copy_out_chunk::<K>(out, dst, at)?;
        dst = dst
            .checked_add(K)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        at = at
            .checked_add(K)
            .ok_or(Error::CorruptData(Corruption::MatchReach))?;
    }
    Ok(dst)
}

/// Bulk-copies whole chunks of a match whose source lies in `history`.
///
/// A chunk read that crosses the history/output boundary is split into a history half and an
/// output half. The output half reads bytes at or before the write cursor because the distance
/// is at least `K`, so no read touches a byte the match is still to write. Answers the
/// destination it reached.
fn copy_history_bulk<const K: usize>(
    history: &[u8],
    out: &mut [u8],
    written: usize,
    from: usize,
    end: usize,
) -> Result<usize, Error> {
    let h = history.len();
    let mut dst = written;
    let mut at = from;
    while dst.checked_add(K).is_some_and(|chunk_end| chunk_end <= end) {
        let at_end = at
            .checked_add(K)
            .ok_or(Error::CorruptData(Corruption::MatchReach))?;
        let dst_end = dst
            .checked_add(K)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        if at_end <= h {
            let source = history
                .get(at..at_end)
                .ok_or(Error::CorruptData(Corruption::MatchReach))?;
            let target = out
                .get_mut(dst..dst_end)
                .ok_or(Error::CorruptData(Corruption::BlockContent))?;
            target.copy_from_slice(source);
        } else {
            let head = h.saturating_sub(at);
            let dst_head = dst
                .checked_add(head)
                .ok_or(Error::CorruptData(Corruption::BlockContent))?;
            let source = history
                .get(at..h)
                .ok_or(Error::CorruptData(Corruption::MatchReach))?;
            let target = out
                .get_mut(dst..dst_head)
                .ok_or(Error::CorruptData(Corruption::BlockContent))?;
            target.copy_from_slice(source);
            let rest = K.saturating_sub(head);
            let rest_end = dst_head
                .checked_add(rest)
                .ok_or(Error::CorruptData(Corruption::BlockContent))?;
            if rest_end > out.len() {
                return Err(Error::CorruptData(Corruption::BlockContent));
            }
            out.copy_within(0..rest, dst_head);
        }
        dst = dst_end;
        at = at_end;
    }
    Ok(dst)
}

/// Expands one block through a width-chunked match copy with a bounded exact scalar tail.
///
/// A match whose validated distance is at least `K` is copied `K` bytes at a time while a
/// whole chunk fits inside the match end; the shorter final tail, every match with a smaller
/// distance, every true overlap, and every source that crosses the history boundary without
/// being wholly in one of the two is copied by the scalar oracle. Every chunk read ends at or
/// before the write cursor and every chunk write ends at or before the match end, which is the
/// logical block output end at most. The two paths agree on every accepted byte and every
/// refusal.
fn expand_width<const K: usize>(
    sequences: &Sequences,
    history: &[u8],
    out: &mut [u8],
) -> Result<(), Error> {
    let literals = sequences.literals();
    let logical = out.len();
    let h = history.len();
    let mut taken = 0usize;
    let mut written = 0usize;
    for step in sequences.steps() {
        let run =
            usize::try_from(step.run).map_err(|_| Error::CorruptData(Corruption::BlockContent))?;
        taken = copy_literals(literals, out, taken, written, run)?;
        written = written
            .checked_add(run)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        let Some(matched) = step.matched else {
            continue;
        };
        let (from, end, distance) = admit_match(h, written, logical, matched)?;
        let length = end
            .checked_sub(written)
            .ok_or(Error::CorruptData(Corruption::BlockContent))?;
        if distance >= K {
            let done = if from >= h {
                copy_out_bulk::<K>(out, written, from.saturating_sub(h), end)?
            } else if from
                .checked_add(length)
                .is_some_and(|source_end| source_end <= h)
            {
                copy_history_bulk::<K>(history, out, written, from, end)?
            } else {
                written
            };
            if done < end {
                let rest = from
                    .checked_add(done.saturating_sub(written))
                    .ok_or(Error::CorruptData(Corruption::MatchReach))?;
                copy_match_scalar(history, out, done, end, rest)?;
            }
        } else {
            copy_match_scalar(history, out, written, end, from)?;
        }
        written = end;
    }
    if written != logical {
        return Err(Error::CorruptData(Corruption::BlockContent));
    }
    Ok(())
}

/// Records every block expansion a test drives, so the differential can replay the exact
/// validated sequences and history production saw.
#[cfg(test)]
mod capture {
    use super::Sequences;
    use std::cell::{Cell, RefCell};

    /// One captured expansion: the sequences, the history, and the logical output length.
    pub type Captured = (Sequences, Vec<u8>, usize);

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
        static HELD: RefCell<Vec<Captured>> = const { RefCell::new(Vec::new()) };
    }

    pub fn record(sequences: &Sequences, history: &[u8], out_len: usize) {
        if !ARMED.with(Cell::get) {
            return;
        }
        HELD.with(|held| {
            held.borrow_mut()
                .push((sequences.clone(), history.to_vec(), out_len));
        });
    }

    /// Arms capture on this thread and discards anything captured before.
    pub fn arm() {
        HELD.with(|held| held.borrow_mut().clear());
        ARMED.with(|armed| armed.set(true));
    }

    /// Disarms capture on this thread.
    pub fn disarm() {
        ARMED.with(|armed| armed.set(false));
    }

    /// Takes everything captured since the last arm.
    pub fn take() -> Vec<Captured> {
        HELD.with(|held| std::mem::take(&mut *held.borrow_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Block, Built, Coded, Coder, Decoder, Encoder, Streams, TABLE_SHARES, Terms, capture,
        expand, expand_scalar, huffman,
    };
    use crate::entropy::bits::{BitBuf, BitReader, BitWriter};
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
        fill(&mut builder, shape, size, history.len())?;
        builder.finish(history.len())
    }

    /// Steps a builder in `shape` until the block it is building holds `size` bytes.
    ///
    /// A block a test builds by hand is a few dozen bytes, and the expansion bound refuses a
    /// COMPRESSED block that small whatever it holds. So a test that needs a readable block
    /// tops its own steps up to a length a table can pay for.
    fn fill(builder: &mut Builder, shape: Shape, size: usize, history_bytes: usize) -> Option<()> {
        let mut first = builder.produced == 0;
        let distance = usize::try_from(shape.distance).ok()?;
        while builder.produced < size {
            let left = size.checked_sub(builder.produced)?;
            let reach = history_bytes.checked_add(builder.produced)?;
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
        Some(())
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

    /// Assembles one block's payload and adopts it, which is what emitting it would do.
    fn assemble(
        encoder: &mut Encoder,
        sequences: &Sequences,
        first_in_region: bool,
        repeat: [bool; BLOCK_STREAMS],
    ) -> Result<Vec<u8>, Error> {
        let mut payload = Vec::new();
        let assembly = encoder
            .assemble_into(
                sequences,
                Terms {
                    first_in_region,
                    repeat: Some(repeat),
                    ceiling: usize::MAX,
                },
                &DecoderPolicy::CONSERVATIVE,
                &mut payload,
            )?
            .ok_or(Error::InvalidParameter)?;
        encoder.adopt(assembly);
        Ok(payload)
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
        let mut payload = Vec::new();
        let assembly = encoder
            .assemble_into(
                &plan.sequences,
                Terms {
                    first_in_region,
                    repeat: Some(repeat),
                    ceiling: usize::MAX,
                },
                &policy,
                &mut payload,
            )?
            .ok_or(Error::InvalidParameter)?;
        encoder.adopt(assembly);
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
        assert!(
            decoder.peak_table_bytes() <= policy.max_table_bytes(),
            "the decoder held {} table bytes against a ceiling of {}",
            decoder.peak_table_bytes(),
            policy.max_table_bytes()
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
        let mut carried = 0usize;
        for size in sizes {
            for (index, shape) in shapes().into_iter().enumerate() {
                let seed = u64::try_from(index).unwrap_or(0).saturating_add(1);
                let plan = plan(shape, size, &[], seed).ok_or(Error::InvalidParameter)?;
                assert_eq!(plan.content.len(), size, "the plan did not fill the block");
                let mut encoder = Encoder::at_region_start();
                let mut decoder = Decoder::at_region_start();
                let mut payload = Vec::new();
                let assembly = encoder
                    .assemble_into(
                        &plan.sequences,
                        Terms {
                            first_in_region: true,
                            repeat: Some(FRESH),
                            ceiling: usize::MAX,
                        },
                        &DecoderPolicy::CONSERVATIVE,
                        &mut payload,
                    )?
                    .ok_or(Error::InvalidParameter)?;
                encoder.adopt(assembly);
                assert!(
                    !payload.is_empty(),
                    "a block of {size} bytes in shape {index} stored nothing"
                );
                checked = checked.saturating_add(1);

                let compresses = payload.len() < size;
                let read = read_with(
                    &payload,
                    u64::try_from(payload.len()).unwrap_or(u64::MAX),
                    size,
                    true,
                    &[],
                    &mut decoder,
                );
                if compresses {
                    assert_eq!(
                        read,
                        Ok(payload.len()),
                        "shape {index} at {size} bytes did not spend its payload"
                    );
                    carried = carried.saturating_add(1);
                } else {
                    assert_eq!(
                        read,
                        Err(Error::CorruptData(Corruption::BlockExpansion)),
                        "shape {index} at {size} bytes stored {} bytes and was not refused",
                        payload.len()
                    );
                }

                // A block of one or two bytes is narrower than any prologue, and the shape
                // that holds no structure is above its own size until the literal code has a
                // whole block of symbols to amortize its description over.
                let expands = size < 4_096 || (index == 0 && size < 65_536);
                assert_eq!(
                    compresses,
                    !expands,
                    "shape {index} at {size} bytes stored {} bytes",
                    payload.len()
                );
            }
        }
        assert_eq!(
            checked,
            sizes.len().saturating_mul(shapes().len()),
            "a shape was skipped rather than round-tripped"
        );
        assert_eq!(
            carried, 16,
            "every block below its own decoded size carried, and no other did"
        );
        Ok(())
    }

    /// A literal code whose decode table is wider than a quarter of the version 1 ceiling.
    ///
    /// The encoder never builds one, because it gives each class a quarter share. A decoder
    /// admits one, because what it holds a block to is the sum over the four classes. So this
    /// is a table only a stream a caller did not write can ask for.
    fn wide_literal_code() -> Result<huffman::Code, Error> {
        let mut counts = vec![0_u64; 256];
        let (mut a, mut b) = (1_u64, 1_u64);
        for symbol in 0..15_usize {
            let slot = counts.get_mut(symbol).ok_or(Error::InvalidParameter)?;
            *slot = a;
            let next = a.saturating_add(b);
            a = b;
            b = next;
        }
        huffman::Code::build(&counts, 256, 15)
    }

    /// Rebuilds a block's stored body with one description replaced, restating the prologue.
    fn with_description(
        payload: &[u8],
        first_in_region: bool,
        index: usize,
        description: &BitBuf,
        table_bytes: u64,
    ) -> Result<Vec<u8>, Error> {
        let policy = DecoderPolicy::CONSERVATIVE;
        let (mut prologue, used) =
            BlockPrologue::decode(payload, u32::MAX, first_in_region, &policy)?;
        let body = payload.get(used..).ok_or(Error::InvalidParameter)?;
        let mut at = 0_usize;
        let descriptions = super::locate(&prologue.description_bits, &mut at)?;
        let payloads = super::locate(&prologue.payload_bits, &mut at)?;
        let suffixes = super::locate(&prologue.suffix_bits, &mut at)?;

        let slot = prologue
            .description_bits
            .get_mut(index)
            .ok_or(Error::InvalidParameter)?;
        *slot = description.bits();
        prologue.table_bytes = table_bytes;

        let mut out = Vec::new();
        out.extend_from_slice(prologue.encode(first_in_region).as_bytes());
        for (lane, span) in descriptions.iter().enumerate() {
            if lane == index {
                out.extend_from_slice(description.bytes());
            } else {
                out.extend_from_slice(span.of(body)?);
            }
        }
        for span in &payloads {
            out.extend_from_slice(span.of(body)?);
        }
        for span in &suffixes {
            out.extend_from_slice(span.of(body)?);
        }
        Ok(out)
    }

    /// The table bound is taken over the transition and not only over the state a block
    /// settles at.
    ///
    /// The first block leaves three tables at the FAST preference and no literal table. The
    /// second replaces all four, and its literal table alone is half the ceiling. Both states
    /// are inside the ceiling, and the peak covers the move from one to the other.
    #[test]
    fn the_table_bound_covers_the_transition_between_two_blocks() -> Result<(), Error> {
        let policy = DecoderPolicy::CONSERVATIVE;
        let history: Vec<u8> = (0..4_096_u32)
            .map(|at| u8::try_from(at & 0xFF).unwrap_or(0))
            .collect();
        let matches = Shape {
            run: 0,
            length: MIN_MATCH,
            distance: 64,
            alphabet: 256,
        };
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let first = plan(matches, 65_536, &history, 71).ok_or(Error::InvalidParameter)?;
        let _first = round_trip(&first, &history, true, FRESH, &mut encoder, &mut decoder)?;
        assert_eq!(
            decoder.table_bytes(),
            12_288,
            "the first block was meant to leave three tables at the FAST preference and no literal one"
        );

        let mut region = history.clone();
        region.extend_from_slice(&first.content);
        let mixed = Shape {
            run: 4,
            length: 8,
            distance: 64,
            alphabet: 64,
        };
        let next = plan(mixed, 2_048, &region, 72).ok_or(Error::InvalidParameter)?;
        let mut payload = Vec::new();
        let assembly = encoder
            .assemble_into(
                &next.sequences,
                Terms {
                    first_in_region: false,
                    repeat: Some(FRESH),
                    ceiling: usize::MAX,
                },
                &policy,
                &mut payload,
            )?
            .ok_or(Error::InvalidParameter)?;
        encoder.adopt(assembly);

        let (prologue, used) = BlockPrologue::decode(
            &payload,
            u32::try_from(next.content.len()).map_err(|_| Error::InvalidParameter)?,
            false,
            &policy,
        )?;
        let body = payload.get(used..).ok_or(Error::InvalidParameter)?;
        let mut at = 0_usize;
        let descriptions = super::locate(&prologue.description_bits, &mut at)?;
        let span = descriptions
            .get(LITERAL_BYTE_STREAM)
            .ok_or(Error::InvalidParameter)?;
        let mut reader = BitReader::new(span.of(body)?, span.bits);
        let narrow = huffman::Declared::parse(&mut reader, 256)?
            .validate()?
            .table_bytes();

        let wide = wide_literal_code()?;
        assert_eq!(wide.table_bytes(), 32_768);
        let mut writer = BitWriter::new();
        wide.describe(&mut writer);
        let declared = prologue
            .table_bytes
            .saturating_sub(narrow)
            .saturating_add(wide.table_bytes());
        assert!(
            declared <= policy.max_table_bytes(),
            "the edited block was meant to declare a figure policy admits, and declares {declared}"
        );

        let edited = with_description(
            &payload,
            false,
            LITERAL_BYTE_STREAM,
            &writer.finish(),
            declared,
        )?;
        let mut out = vec![0_u8; next.content.len()];
        let block = Block {
            arrived: &edited,
            declared: u64::try_from(edited.len()).unwrap_or(u64::MAX),
            first_in_region: false,
            history: &region,
        };
        let _read = decoder.read(&block, &policy, &mut out);

        assert!(
            decoder.peak_table_bytes() >= decoder.table_bytes(),
            "the peak sits below a state the decoder held"
        );
        assert!(
            decoder.peak_table_bytes() <= policy.max_table_bytes(),
            "the decoder held {} table bytes against a ceiling of {}",
            decoder.peak_table_bytes(),
            policy.max_table_bytes()
        );
        Ok(())
    }

    /// Capped rANS tables hold a structured block's peak to three preferences.
    ///
    /// The fixture carries three rANS streams and no literal table, so the peak is the
    /// rANS sum alone: three tables of 4096 bytes, down from 49 152 bytes uncapped.
    #[test]
    fn capped_tables_hold_a_structured_block_peak_to_three_preferences() -> Result<(), Error> {
        let history: Vec<u8> = (0..4_096_u32)
            .map(|at| u8::try_from(at & 0xFF).unwrap_or(0))
            .collect();
        let matches = Shape {
            run: 0,
            length: MIN_MATCH,
            distance: 64,
            alphabet: 256,
        };
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let first = plan(matches, 65_536, &history, 71).ok_or(Error::InvalidParameter)?;
        let _first = round_trip(&first, &history, true, FRESH, &mut encoder, &mut decoder)?;
        assert!(
            decoder.peak_table_bytes()
                <= 3 * crate::entropy::rans::table_bytes_for(
                    crate::entropy::rans::FAST_TABLE_LOG_MAX
                ),
            "a structured block peaked at {} rANS table bytes",
            decoder.peak_table_bytes()
        );
        Ok(())
    }

    /// The expansion bound: a block that stores at least what it decodes to is refused, and
    /// the refusal precedes every allocation the block would size.
    #[test]
    fn a_compressed_block_that_stores_its_decoded_size_is_refused_before_its_payload()
    -> Result<(), Error> {
        let shape = Shape {
            run: 8,
            length: 0,
            distance: 0,
            alphabet: 256,
        };
        let plan = plan(shape, 64, &[], 91).ok_or(Error::InvalidParameter)?;
        let mut encoder = Encoder::at_region_start();
        let mut payload = Vec::new();
        let assembly = encoder
            .assemble_into(
                &plan.sequences,
                Terms {
                    first_in_region: true,
                    repeat: Some(FRESH),
                    ceiling: usize::MAX,
                },
                &DecoderPolicy::CONSERVATIVE,
                &mut payload,
            )?
            .ok_or(Error::InvalidParameter)?;
        encoder.adopt(assembly);
        assert!(
            payload.len() >= plan.content.len(),
            "the block was meant to be one no encoder would emit"
        );

        let mut refused = Vec::new();
        assert!(
            encoder
                .assemble_into(
                    &plan.sequences,
                    Terms {
                        first_in_region: true,
                        repeat: Some(FRESH),
                        ceiling: plan.content.len(),
                    },
                    &DecoderPolicy::CONSERVATIVE,
                    &mut refused,
                )?
                .is_none(),
            "an assembly at the size a decoder refuses was written out"
        );
        assert!(refused.is_empty(), "a refused assembly wrote bytes");

        let (error, built) = refuse(&payload, plan.content.len());
        assert_eq!(error, Error::CorruptData(Corruption::BlockExpansion));
        assert_eq!(
            built, 0,
            "the bound is read from the twelve extents, before a table exists"
        );

        // One byte of decoded size more than the block stores is the first size it is not
        // refused at, which is what makes the refusal a boundary and not a constant.
        let mut wider = plan.content.clone();
        wider.resize(payload.len().saturating_add(1), 0);
        let mut decoder = Decoder::at_region_start();
        let read = read_with(
            &payload,
            u64::try_from(payload.len()).unwrap_or(u64::MAX),
            wider.len(),
            true,
            &[],
            &mut decoder,
        );
        assert_ne!(read, Err(Error::CorruptData(Corruption::BlockExpansion)));
        Ok(())
    }

    /// The reuse trigger: a stream names the table in force only when that spends fewer bits.
    #[test]
    fn a_stream_repeats_a_table_only_when_repeating_is_cheaper() -> Result<(), Error> {
        let shape = Shape {
            run: 9,
            length: 14,
            distance: 48,
            alphabet: 40,
        };
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let first = plan(shape, 4_096, &[], 51).ok_or(Error::InvalidParameter)?;
        let _first = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;

        let next = plan(shape, 4_096, &first.content, 52).ok_or(Error::InvalidParameter)?;
        let policy = DecoderPolicy::CONSERVATIVE;
        let mut fresh = Vec::new();
        let _fresh = encoder.assemble_into(
            &next.sequences,
            Terms {
                first_in_region: false,
                repeat: Some(FRESH),
                ceiling: usize::MAX,
            },
            &policy,
            &mut fresh,
        )?;
        let mut chosen = Vec::new();
        let assembly = encoder
            .assemble_into(
                &next.sequences,
                Terms {
                    first_in_region: false,
                    repeat: None,
                    ceiling: usize::MAX,
                },
                &policy,
                &mut chosen,
            )?
            .ok_or(Error::InvalidParameter)?;
        let repeated = assembly.repeated();
        encoder.adopt(assembly);

        assert!(
            repeated.iter().any(|&stream| stream),
            "no stream of a block that follows a like block named the table in force"
        );
        assert!(
            chosen.len() <= fresh.len(),
            "the trigger chose {} stored bytes where a description in every stream cost {}",
            chosen.len(),
            fresh.len()
        );

        let size = u32::try_from(next.content.len()).map_err(|_| Error::InvalidParameter)?;
        let (prologue, _used) = BlockPrologue::decode(&chosen, size, false, &policy)?;
        for index in 0..BLOCK_STREAMS {
            let stream = prologue.stream(index).ok_or(Error::InvalidParameter)?;
            assert_eq!(
                stream.repeats,
                repeated.get(index).copied().unwrap_or(false),
                "stream {index} declared a mode bit the assembly did not report"
            );
            if stream.repeats {
                assert_eq!(
                    stream.description_bits, 0,
                    "stream {index} repeated and described"
                );
            }
        }

        let mut out = vec![0_u8; next.content.len()];
        let block = Block {
            arrived: &chosen,
            declared: u64::try_from(chosen.len()).unwrap_or(u64::MAX),
            first_in_region: false,
            history: &first.content,
        };
        let spent = decoder.read(&block, &policy, &mut out)?;
        assert_eq!(spent, chosen.len());
        assert_eq!(
            out, next.content,
            "a repeating block did not decode to its content"
        );
        Ok(())
    }

    /// The decision the exact trial encode reaches, over the inputs the shipped rule reads.
    fn repeats_by_trial(fresh: &Coded, held: &Built, symbols: &[u16]) -> Result<bool, Error> {
        if !held.carries(symbols) {
            return Ok(false);
        }
        let spent = fresh
            .description
            .bits()
            .saturating_add(fresh.payload.bits());
        Ok(held.write(symbols)?.bits() < spent)
    }

    /// The literal-byte opportunity one block presents to the table in force.
    struct Opportunity {
        fresh: Coded,
        counts: Vec<u64>,
        symbols: Vec<u16>,
    }

    /// The fresh table a block's literal bytes build, the counts it was built over, and the
    /// symbols a held table would have to code.
    ///
    /// A block whose literal-byte stream is empty presents no opportunity, because the encoder
    /// codes no section for it and reaches no decision about it.
    fn literal_byte_opportunity(
        encoder: &Encoder,
        sequences: &Sequences,
        share: u64,
    ) -> Result<Option<Opportunity>, Error> {
        let mut cache = encoder.cache;
        let streams = Streams::of(sequences, &mut cache)?;
        let symbols = streams.stream(Alphabet::LiteralByte).symbols().to_vec();
        if symbols.is_empty() {
            return Ok(None);
        }
        let (fresh, counts) =
            Encoder::fresh(Coder::Huffman, Alphabet::LiteralByte, &symbols, share)?;
        Ok(Some(Opportunity {
            fresh,
            counts,
            symbols,
        }))
    }

    /// A coded stream that declares exactly `payload` bits and no description, for holding the
    /// trigger against a held cost that lands on its total exactly.
    fn fresh_costing(built: Built, payload: u64) -> Coded {
        let mut writer = BitWriter::new();
        let mut left = payload;
        while left > 0 {
            let width = u32::try_from(left.min(32)).unwrap_or(32);
            writer.push(0, width);
            left = left.saturating_sub(u64::from(width));
        }
        Coded {
            description: BitWriter::new().finish(),
            payload: writer.finish(),
            built,
            repeats: false,
        }
    }

    /// Every literal-byte reuse opportunity a region presents decides the way the trial encode
    /// decided it, and the analytic cost is the bit count that trial would have reported.
    #[test]
    fn the_analytic_literal_byte_cost_decides_as_the_trial_encode_did() -> Result<(), Error> {
        let policy = DecoderPolicy::CONSERVATIVE;
        let share = policy
            .max_table_bytes()
            .checked_div(TABLE_SHARES)
            .ok_or(Error::InvalidParameter)?;
        let mut repeat_wins = 0usize;
        let mut fresh_wins = 0usize;
        let all = shapes();
        for (lane, pair) in all
            .iter()
            .flat_map(|first| all.iter().map(move |second| (*first, *second)))
            .enumerate()
        {
            let mut encoder = Encoder::at_region_start();
            let mut history: Vec<u8> = Vec::new();
            let mut out = Vec::new();
            for block in 0..4usize {
                let seed = u64::try_from(lane.saturating_mul(16).saturating_add(block))
                    .unwrap_or(0)
                    .saturating_add(101);
                let taken = if block % 2 == 0 { pair.0 } else { pair.1 };
                let next = plan(taken, 4_096, &history, seed).ok_or(Error::InvalidParameter)?;
                let first_in_region = block == 0;
                let presented = literal_byte_opportunity(&encoder, &next.sequences, share)?;
                let mut expected = false;
                if let Some(Opportunity {
                    fresh,
                    counts,
                    symbols,
                }) = presented
                {
                    let held = if first_in_region {
                        None
                    } else {
                        encoder
                            .tables
                            .get(LITERAL_BYTE_STREAM)
                            .and_then(Option::as_ref)
                    };
                    if let Some(table) = held
                        && table.carries(&symbols)
                    {
                        let Built::Huffman(ref code) = *table else {
                            return Err(Error::InvalidParameter);
                        };
                        assert_eq!(
                            code.encoded_bits_for_counts(&counts)?,
                            table.write(&symbols)?.bits(),
                            "pair {lane} block {block} costed the held table differently \
                             from the trial encode"
                        );
                        expected = repeats_by_trial(&fresh, table, &symbols)?;
                        if expected {
                            repeat_wins = repeat_wins.saturating_add(1);
                        } else {
                            fresh_wins = fresh_wins.saturating_add(1);
                        }
                    }
                    let chosen = Encoder::cheaper(fresh, &counts, held, &symbols)?;
                    assert_eq!(
                        chosen.repeats, expected,
                        "pair {lane} block {block} decided against the trial encode"
                    );
                }

                let assembly = encoder
                    .assemble_into(
                        &next.sequences,
                        Terms {
                            first_in_region,
                            repeat: None,
                            ceiling: usize::MAX,
                        },
                        &policy,
                        &mut out,
                    )?
                    .ok_or(Error::InvalidParameter)?;
                assert_eq!(
                    assembly
                        .repeated()
                        .get(LITERAL_BYTE_STREAM)
                        .copied()
                        .unwrap_or(false),
                    expected,
                    "pair {lane} block {block} emitted a mode bit the decision did not name"
                );
                encoder.adopt(assembly);
                history.extend_from_slice(&next.content);
            }
        }
        assert!(
            repeat_wins > 0 && fresh_wins > 0,
            "the opportunities exercised {repeat_wins} repeat wins and {fresh_wins} fresh wins, \
             so one side of the comparison went unread"
        );
        Ok(())
    }

    /// A held cost that lands exactly on what the fresh table spends takes the fresh table, and
    /// one bit either side of it decides the other way.
    #[test]
    fn the_analytic_trigger_holds_the_tie_to_the_fresh_table() -> Result<(), Error> {
        let mut counts = vec![0u64; 256];
        for (symbol, count) in [(0usize, 4u64), (1, 2), (2, 1)] {
            let slot = counts.get_mut(symbol).ok_or(Error::InvalidParameter)?;
            *slot = count;
        }
        let symbols = [0u16, 0, 0, 0, 1, 1, 2];
        let code = huffman::Code::build_within(&counts, 256, u64::MAX)?;
        let held = Built::Huffman(code);
        let Built::Huffman(ref code) = held else {
            return Err(Error::InvalidParameter);
        };
        let exact = code.encoded_bits_for_counts(&counts)?;
        assert_eq!(
            exact,
            held.write(&symbols)?.bits(),
            "the analytic cost differs from the trial encode"
        );

        for (spent, repeats) in [
            (exact.saturating_sub(1), false),
            (exact, false),
            (exact.saturating_add(1), true),
        ] {
            let fresh = fresh_costing(held.clone(), spent);
            let chosen = Encoder::cheaper(fresh, &counts, Some(&held), &symbols)?;
            assert_eq!(
                chosen.repeats, repeats,
                "a held cost of {exact} against a fresh cost of {spent} decided the wrong way"
            );
        }
        Ok(())
    }

    /// A table in force that cannot code a symbol of the next block is not named by it.
    #[test]
    fn a_stream_does_not_repeat_a_table_that_cannot_code_it() -> Result<(), Error> {
        let narrow = Shape {
            run: 6,
            length: 10,
            distance: 32,
            alphabet: 2,
        };
        let wide = Shape {
            run: 6,
            length: 10,
            distance: 32,
            alphabet: 200,
        };
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();
        let first = plan(narrow, 4_096, &[], 61).ok_or(Error::InvalidParameter)?;
        let _first = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;

        let next = plan(wide, 4_096, &first.content, 62).ok_or(Error::InvalidParameter)?;
        let policy = DecoderPolicy::CONSERVATIVE;
        let mut payload = Vec::new();
        let assembly = encoder
            .assemble_into(
                &next.sequences,
                Terms {
                    first_in_region: false,
                    repeat: None,
                    ceiling: usize::MAX,
                },
                &policy,
                &mut payload,
            )?
            .ok_or(Error::InvalidParameter)?;
        let repeated = assembly.repeated();
        encoder.adopt(assembly);
        assert!(
            !repeated.get(LITERAL_BYTE_STREAM).copied().unwrap_or(true),
            "a literal code built over two symbols was named for a block of two hundred"
        );

        let mut out = vec![0_u8; next.content.len()];
        let block = Block {
            arrived: &payload,
            declared: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            first_in_region: false,
            history: &first.content,
        };
        let _spent = decoder.read(&block, &policy, &mut out)?;
        assert_eq!(out, next.content);
        Ok(())
    }

    /// A table in force at zero bytes is still in force.
    ///
    /// A Huffman code over one distinct symbol writes no bits and needs no decode table, so it
    /// is in force and occupies nothing. The decoder read the bytes for the fact and refused
    /// the next block that named it, which is a valid stream refused as corrupt. The encoder
    /// writes such a pair whenever a block's literals are one repeated byte, so the defect was
    /// reachable from real content.
    #[test]
    fn a_literal_table_of_one_symbol_is_in_force_at_no_bytes() -> Result<(), Error> {
        let shape = Shape {
            run: 6,
            length: 12,
            distance: 40,
            alphabet: 1,
        };
        let policy = DecoderPolicy::CONSERVATIVE;
        let mut encoder = Encoder::at_region_start();
        let mut decoder = Decoder::at_region_start();

        let first = plan(shape, 2_048, &[], 71).ok_or(Error::InvalidParameter)?;
        let opening = round_trip(&first, &[], true, FRESH, &mut encoder, &mut decoder)?;

        // The literal code of the first block, read back from the block's own description.
        let (declared, used) = BlockPrologue::decode(
            &opening,
            u32::try_from(first.content.len()).map_err(|_| Error::InvalidParameter)?,
            true,
            &policy,
        )?;
        let bits = declared
            .description_bits
            .get(LITERAL_BYTE_STREAM)
            .copied()
            .ok_or(Error::InvalidParameter)?;
        let body = opening.get(used..).ok_or(Error::InvalidParameter)?;
        let section = body
            .get(..usize::try_from(bits.div_ceil(8)).map_err(|_| Error::InvalidParameter)?)
            .ok_or(Error::InvalidParameter)?;
        let code = huffman::Declared::parse(
            &mut BitReader::new(section, bits),
            Alphabet::LiteralByte.size(),
        )?
        .validate()?;
        assert_eq!(
            code.table_bytes(),
            0,
            "a code over one distinct symbol asked for a table"
        );

        // The second block names it, so it carries no description for its literals at all.
        let mut repeat = FRESH;
        let slot = repeat
            .get_mut(LITERAL_BYTE_STREAM)
            .ok_or(Error::InvalidParameter)?;
        *slot = true;
        let next = plan(shape, 2_048, &first.content, 72).ok_or(Error::InvalidParameter)?;
        let payload = round_trip(
            &next,
            &first.content,
            false,
            repeat,
            &mut encoder,
            &mut decoder,
        )?;
        let (named, _used) = BlockPrologue::decode(
            &payload,
            u32::try_from(next.content.len()).map_err(|_| Error::InvalidParameter)?,
            false,
            &policy,
        )?;
        let literals = named
            .stream(LITERAL_BYTE_STREAM)
            .ok_or(Error::InvalidParameter)?;
        assert!(literals.count > 0 && literals.repeats && literals.description_bits == 0);
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
        // The tail is one literal run rather than more matches, so the block is long enough
        // for a table to pay for itself and still carries exactly the two coded offsets.
        let tail = 2_048_usize
            .checked_sub(builder.produced)
            .ok_or(Error::InvalidParameter)?;
        builder
            .step(
                u32::try_from(tail).map_err(|_| Error::InvalidParameter)?,
                None,
            )
            .ok_or(Error::InvalidParameter)?;
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
        fill(&mut builder, shape, 2_048, first.content.len()).ok_or(Error::InvalidParameter)?;
        let next = builder
            .finish(first.content.len())
            .ok_or(Error::InvalidParameter)?;
        let payload = assemble(&mut encoder, &next.sequences, false, FRESH)?;

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
        let payload = assemble(&mut encoder, &next.sequences, false, FRESH)?;
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
        let payload = assemble(&mut encoder, &quiet.sequences, false, FRESH)?;
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
        let shape = Shape {
            run: 12,
            length: 8,
            distance: 64,
            alphabet: 48,
        };
        let mut builder = Builder::new(&[], 161, shape.alphabet);
        builder
            .step(
                100,
                Some(Match {
                    length: 8,
                    distance: 64,
                }),
            )
            .ok_or(Error::InvalidParameter)?;
        fill(&mut builder, shape, 2_048, 0).ok_or(Error::InvalidParameter)?;
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

    /// Runs both expansions over one case.
    ///
    /// Reports whether they agree on the logical output bytes and on the refusal class. A case
    /// both paths accept must decode to the same bytes; a case both paths refuse must refuse
    /// for the same reason.
    fn agree(sequences: &Sequences, history: &[u8], out_len: usize) -> bool {
        let mut oracle = vec![0_u8; out_len];
        let mut production = vec![0_u8; out_len];
        let expected = expand_scalar(sequences, history, &mut oracle);
        let got = expand(sequences, history, &mut production);
        match (expected, got) {
            (Ok(()), Ok(())) => oracle == production,
            (Err(expected), Err(got)) => format!("{expected:?}") == format!("{got:?}"),
            _ => false,
        }
    }

    /// Assembles and reads one plan, keeping every expansion production performed.
    ///
    /// A block the reader refuses before its expansion captures nothing, which is the same
    /// case in which there is no expansion to compare.
    fn capture_plan(
        plan: &Plan,
        history: &[u8],
        first_in_region: bool,
    ) -> Result<Vec<capture::Captured>, Error> {
        let mut encoder = Encoder::at_region_start();
        let mut payload = Vec::new();
        let assembly = encoder
            .assemble_into(
                &plan.sequences,
                Terms {
                    first_in_region,
                    repeat: Some(FRESH),
                    ceiling: usize::MAX,
                },
                &DecoderPolicy::CONSERVATIVE,
                &mut payload,
            )?
            .ok_or(Error::InvalidParameter)?;
        encoder.adopt(assembly);
        capture::arm();
        let mut decoder = Decoder::at_region_start();
        let mut out = vec![0_u8; plan.content.len()];
        let block = Block {
            arrived: &payload,
            declared: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            first_in_region,
            history,
        };
        let _read = decoder.read(&block, &DecoderPolicy::CONSERVATIVE, &mut out);
        capture::disarm();
        Ok(capture::take())
    }

    /// Every expansion production performs answers the scalar oracle on bytes and refusal.
    #[test]
    fn every_production_expansion_agrees_with_the_scalar_oracle() -> Result<(), Error> {
        let sizes = [1_usize, 2, 64, 4_096, 16_384, 65_536];
        let mut cases = 0usize;
        for size in sizes {
            for (index, shape) in shapes().into_iter().enumerate() {
                let seed = u64::try_from(index).unwrap_or(0).saturating_add(1);
                let plan = plan(shape, size, &[], seed).ok_or(Error::InvalidParameter)?;
                for (sequences, history, out_len) in capture_plan(&plan, &[], true)? {
                    assert!(
                        agree(&sequences, &history, out_len),
                        "shape {index} at {size} bytes diverged"
                    );
                    cases = cases.saturating_add(1);
                }
            }
        }
        assert!(cases > 0, "no expansion was captured");
        Ok(())
    }

    /// An expansion whose source crosses the history boundary answers the scalar oracle.
    #[test]
    fn a_history_crossing_expansion_agrees_with_the_scalar_oracle() -> Result<(), Error> {
        let history: Vec<u8> = (0..4_096_u32)
            .map(|at| u8::try_from(at & 0xFF).unwrap_or(0))
            .collect();
        let shape = Shape {
            run: 16,
            length: MAX_MATCH_LENGTH,
            distance: 64,
            alphabet: 256,
        };
        let plan = plan(shape, 16_384, &history, 211).ok_or(Error::InvalidParameter)?;
        let mut cases = 0usize;
        for (sequences, held, out_len) in capture_plan(&plan, &history, false)? {
            assert!(
                agree(&sequences, &held, out_len),
                "a crossing case diverged"
            );
            cases = cases.saturating_add(1);
        }
        assert!(cases > 0, "no history-crossing expansion was captured");
        Ok(())
    }

    /// Rewrites one step of a block into a case no valid stream produces.
    ///
    /// The values are drawn from outside every domain the sequence representation enforces, so
    /// the caller uses the unchecked constructor; the two paths must still agree on both the
    /// bytes and the refusal class.
    fn mutate(
        sequences: &Sequences,
        at: usize,
        kind: usize,
        h: usize,
        out_len: usize,
    ) -> Option<Sequences> {
        let mut changed = sequences.steps().to_vec();
        let step = *changed.get(at)?;
        let mut written = 0usize;
        for prior in changed.get(..at)? {
            written = written.saturating_add(usize::try_from(prior.run).ok()?);
            if let Some(matched) = prior.matched {
                written = written.saturating_add(usize::try_from(matched.length).ok()?);
            }
        }
        let produced = h.checked_add(written)?;
        let original = step.matched.unwrap_or(Match {
            length: MIN_MATCH,
            distance: 1,
        });
        let matched = match kind {
            0 => Match {
                distance: 0,
                ..original
            },
            1 => Match {
                distance: u32::try_from(produced.checked_add(1)?).ok()?,
                ..original
            },
            2 => Match {
                length: u32::try_from(out_len.checked_sub(written)?.checked_add(1)?).ok()?,
                ..original
            },
            3 => original,
            4 => Match {
                distance: u32::try_from(written.checked_add(1)?).ok()?,
                ..original
            },
            _ => Match {
                distance: original.distance.max(1),
                length: original.distance.max(1).saturating_add(8),
            },
        };
        let new_step = if kind == 3 {
            Step {
                run: step.run.checked_add(1)?,
                matched: step.matched,
            }
        } else {
            Step {
                run: step.run,
                matched: Some(matched),
            }
        };
        *changed.get_mut(at)? = new_step;
        Some(Sequences::new_unchecked(
            sequences.literals().to_vec(),
            changed,
        ))
    }

    /// Every mutated case answers the scalar oracle on bytes and refusal class.
    ///
    /// The mutations carry a zero distance, a distance past the produced bytes, a length past
    /// the block's remaining output, a run that overruns the literal bytes, a source that
    /// crosses the history boundary, and a true self-overlap.
    #[test]
    fn every_mutated_expansion_agrees_with_the_scalar_oracle() -> Result<(), Error> {
        let history: Vec<u8> = (0..4_096_u32)
            .map(|at| u8::try_from(at & 0xFF).unwrap_or(0))
            .collect();
        let mut cases = 0usize;
        for (index, shape) in shapes().into_iter().enumerate() {
            let seed = u64::try_from(index).unwrap_or(0).saturating_add(101);
            let plan = plan(shape, 16_384, &history, seed).ok_or(Error::InvalidParameter)?;
            for (sequences, held, out_len) in capture_plan(&plan, &history, false)? {
                let h = held.len();
                for at in 0..sequences.steps().len() {
                    for kind in 0..6 {
                        let Some(mutated) = mutate(&sequences, at, kind, h, out_len) else {
                            continue;
                        };
                        assert!(
                            agree(&mutated, &held, out_len),
                            "shape {index} step {at} kind {kind} diverged"
                        );
                        cases = cases.saturating_add(1);
                    }
                }
            }
        }
        assert!(cases > 0, "no mutation was checked");
        Ok(())
    }

    /// One block of `run` literals, one match, and an optional literal tail.
    fn synthetic(
        run: usize,
        length: usize,
        distance: usize,
        tail: usize,
    ) -> Option<(Sequences, usize)> {
        let mut literals = Vec::new();
        for index in 0..run {
            literals.push(u8::try_from(index & 0xFF).ok()?);
        }
        for index in 0..tail {
            literals.push(u8::try_from((index ^ 0x5A) & 0xFF).ok()?);
        }
        let mut steps = vec![Step {
            run: u32::try_from(run).ok()?,
            matched: Some(Match {
                length: u32::try_from(length).ok()?,
                distance: u32::try_from(distance).ok()?,
            }),
        }];
        if tail > 0 {
            steps.push(Step {
                run: u32::try_from(tail).ok()?,
                matched: None,
            });
        }
        let out_len = run.checked_add(length)?.checked_add(tail)?;
        Some((Sequences::new_unchecked(literals, steps), out_len))
    }

    /// The kernel agrees with the oracle at every chunk boundary.
    ///
    /// Lengths cover shorter than a chunk, exactly a chunk, one past a chunk, an exact multiple,
    /// a remainder of one, and a remainder of `K - 1`. Distances cover below the width, exactly
    /// the width, one past it, and a wide distance. A history of zero, one, several, and more
    /// than a chunk covers a source entirely in the block output, entirely in history, and
    /// crossing the boundary. A tail after the match puts the match end inside the block rather
    /// than exactly at it, so the final partial chunk is exercised.
    #[test]
    fn the_width_chunked_kernel_agrees_at_every_chunk_boundary() {
        let k = super::COPY_WIDTH;
        let distances = [
            1,
            k.saturating_sub(1),
            k,
            k.saturating_add(1),
            k.saturating_mul(2),
        ];
        let lengths = [
            1,
            k.saturating_sub(1),
            k,
            k.saturating_add(1),
            k.saturating_mul(2),
            k.saturating_mul(2).saturating_add(1),
            k.saturating_mul(3).saturating_sub(1),
            k.saturating_mul(3),
        ];
        let mut cases = 0usize;
        for history_len in [0usize, 1, 8, 64, k.saturating_add(3)] {
            let history: Vec<u8> = (0..history_len)
                .map(|at| u8::try_from(at & 0xFF).unwrap_or(0))
                .collect();
            for distance in distances {
                for length in lengths {
                    for run in [0usize, 1, 8, k] {
                        for tail in [0usize, 1] {
                            let Some((sequences, out_len)) = synthetic(run, length, distance, tail)
                            else {
                                continue;
                            };
                            assert!(
                                agree(&sequences, &history, out_len),
                                "distance {distance} length {length} run {run} tail {tail} \
                                 history {history_len} diverged"
                            );
                            cases = cases.saturating_add(1);
                        }
                    }
                }
            }
        }
        assert!(cases > 0, "no boundary case was checked");
    }
}

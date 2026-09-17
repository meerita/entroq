//! Owns the encoder engine: the path from input bytes to a compliant stream.
//!
//! This module does not own the format contract, match finding, parse strategy, or entropy
//! coding. It composes them, and it owns the choice of which block type carries a block.
//!
//! The layout this module states is encoder policy. It decides how staged input becomes
//! regions and blocks, and a decoder never learns it: every size it implies is declared in a
//! header the decoder reads. A later encoder generation replaces it without changing what a
//! decoder accepts.
//!
//! # The selection rule
//!
//! For each block the encoder assembles every type the block admits and emits the one that
//! stores the fewest bytes. A tie goes to the type a decoder pays least for, which is RAW,
//! then RLE, then COMPRESSED. The rule is encoder freedom; the type it chose is declared in
//! the block header, so a reader learns it without decoding a payload.
//!
//! A rule that assembles costs assembly work and nothing in bytes. A rule that predicts
//! instead emits blocks that expand, and a block that expands is one a decoder refuses.

use crate::block;
use crate::format::{
    BLOCK_HEADER_BYTES, BlockHeader, BlockType, DecoderPolicy, Error, IntegrityMode,
    MAX_BLOCK_BYTES, RegionHeader,
};
use crate::parser::{MAX_PARSE_BYTES, Parser};

/// The input bytes one region holds before the encoder closes it.
///
/// The value is provisional. It is not a format constant: the region header declares the
/// sizes a decoder needs, so a measurement replaces this default without changing what a
/// decoder accepts.
pub const DEFAULT_REGION_BYTES: usize = 1_048_576;

/// The input bytes one block holds.
///
/// Provisional on the same terms as the region size, and never above the bytes one parse may
/// take.
pub const DEFAULT_BLOCK_BYTES: u32 = 65_536;

/// How the encoder divides staged input into regions and blocks.
///
/// The layout decides where a block ends. Which type carries it is the selection rule's, and
/// the two are separate: a layout that cuts the same boundaries emits different types on
/// different content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Layout {
    region_bytes: usize,
    block_bytes: u32,
}

impl Layout {
    /// A layout that stages `region_bytes` of input and cuts it into `block_bytes` blocks.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when either size is zero, or when the block size is above
    /// what a block header can declare.
    pub const fn new(region_bytes: usize, block_bytes: u32) -> Result<Self, Error> {
        if region_bytes == 0 || block_bytes == 0 || block_bytes > MAX_BLOCK_BYTES {
            return Err(Error::InvalidParameter);
        }
        Ok(Self {
            region_bytes,
            block_bytes,
        })
    }

    /// The input bytes one region holds.
    #[must_use]
    pub const fn region_bytes(self) -> usize {
        self.region_bytes
    }

    /// The input bytes one block holds.
    #[must_use]
    pub const fn block_bytes(self) -> u32 {
        self.block_bytes
    }

    /// The header of a region of `logical` staged bytes whose blocks occupy `physical`.
    ///
    /// The physical size is what the blocks actually stored, so it is known only once they are
    /// assembled. That is why a region is assembled before its header is written: a reader
    /// reaches the next record from this field without an index, and a field written before
    /// the blocks exist could only describe blocks that store their own decoded size.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the sizes cannot describe a region this layout can
    /// fill.
    pub fn region(
        self,
        logical: usize,
        physical: u64,
        integrity: IntegrityMode,
    ) -> Result<RegionHeader, Error> {
        if logical > self.region_bytes {
            return Err(Error::InvalidParameter);
        }
        RegionHeader::new(
            u64::try_from(logical).map_err(|_| Error::InvalidParameter)?,
            physical,
            integrity,
        )
    }

    /// The bytes the blocks of a region of `logical` staged bytes occupy at most.
    ///
    /// Every block type stores at most its own decoded size, so a region never stores more
    /// than its content and one header per block. It is the capacity a caller reserves for the
    /// blocks of one region, and the figure the version 1 expansion bound is built from.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the total does not fit in 64 bits.
    pub fn physical_bytes(self, logical: usize) -> Result<u64, Error> {
        let block_bytes = usize::try_from(self.block_bytes).map_err(|_| Error::InvalidParameter)?;
        let blocks = logical.div_ceil(block_bytes);
        let overhead = blocks
            .checked_mul(BLOCK_HEADER_BYTES)
            .ok_or(Error::InvalidParameter)?;
        u64::try_from(
            logical
                .checked_add(overhead)
                .ok_or(Error::InvalidParameter)?,
        )
        .map_err(|_| Error::InvalidParameter)
    }

    /// Where the block that starts at `at` in a staged region of `len` bytes ends, and whether
    /// it is the last block of that region.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the block is empty or above what a header declares.
    pub fn block(self, at: usize, len: usize) -> Result<(usize, bool), Error> {
        let block_bytes = usize::try_from(self.block_bytes).map_err(|_| Error::InvalidParameter)?;
        let end = at
            .checked_add(block_bytes)
            .ok_or(Error::InvalidParameter)?
            .min(len);
        let size = end.checked_sub(at).ok_or(Error::InvalidParameter)?;
        if size == 0 || size > MAX_BLOCK_BYTES as usize {
            return Err(Error::InvalidParameter);
        }
        Ok((end, end == len))
    }
}

/// What an encoder recorded about the blocks it emitted.
///
/// Every field is a count over the life of the encoder that reports it, and every one is
/// derived from a decision the encoder had already taken. Reading them changes no byte the
/// encoder writes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Statistics {
    /// The input bytes the blocks covered.
    pub input_bytes: u64,
    /// The stored bytes the blocks occupied, their headers included.
    pub output_bytes: u64,
    /// The blocks emitted.
    pub blocks: u64,
    /// The blocks emitted as RAW.
    pub raw_blocks: u64,
    /// The blocks emitted as RLE.
    pub rle_blocks: u64,
    /// The blocks emitted as COMPRESSED.
    pub compressed_blocks: u64,
    /// The blocks whose COMPRESSED assembly asked for more table memory than policy allows.
    ///
    /// Such a block is emitted under another type. The count is what makes that visible, so a
    /// caller reading a frame of RAW blocks can tell a ceiling from an incompressible input.
    pub table_limited_blocks: u64,
    /// The streams that named the table already in force for their class.
    pub streams_repeated: u64,
}

/// Turns one block of input into the cheapest block type it can assemble.
///
/// The emitter owns the two things a block needs and a layout does not: the parse the
/// sequences come from, and the tables and offset cache a region carries across its blocks.
/// Both are discarded at a region boundary, which `reset` performs.
pub struct Emitter {
    parser: Parser,
    tables: block::Encoder,
    payload: Vec<u8>,
    policy: DecoderPolicy,
    statistics: Statistics,
}

impl Emitter {
    /// An emitter at a region start, assembling blocks of at most `block_bytes` for a decoder
    /// holding `policy`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the block size is above the bytes one parse may take.
    pub fn new(policy: DecoderPolicy, block_bytes: u32) -> Result<Self, Error> {
        let block_bytes = usize::try_from(block_bytes).map_err(|_| Error::InvalidParameter)?;
        if block_bytes == 0 || block_bytes > MAX_PARSE_BYTES {
            return Err(Error::InvalidParameter);
        }
        Ok(Self {
            parser: Parser::new(),
            tables: block::Encoder::at_region_start(),
            // A COMPRESSED block this emitter writes stores fewer bytes than it decodes to,
            // and an assembly that does not is never written here, so the block length is the
            // whole of what this buffer ever holds.
            payload: Vec::with_capacity(block_bytes),
            policy,
            statistics: Statistics::default(),
        })
    }

    /// What this emitter recorded about the blocks it emitted.
    #[must_use]
    pub const fn statistics(&self) -> Statistics {
        self.statistics
    }

    /// The bytes this emitter holds between calls, for blocks of `block_bytes`.
    ///
    /// Assembling one block allocates in proportion to that block and frees it before the call
    /// returns, so it is not in this figure and nothing here grows with the input.
    #[must_use]
    pub const fn state_bytes(block_bytes: usize) -> usize {
        Parser::declared_bytes(block_bytes).saturating_add(block_bytes)
    }

    /// Discards everything a region boundary discards: the parse history, the tables in force,
    /// and the offset cache.
    pub fn reset(&mut self) {
        self.parser.reset();
        self.tables.reset();
    }

    /// Emits one block of `input` into `out`, header first, and reports the type it chose.
    ///
    /// Every type the block admits is assembled and the one storing the fewest bytes is
    /// emitted. A tie goes to RAW, then RLE, then COMPRESSED, which is the order a decoder
    /// pays for them in.
    ///
    /// A block whose COMPRESSED assembly asks for more table memory than policy allows is
    /// emitted under another type and counted, which is what keeps the fallback visible rather
    /// than silent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the block is empty or longer than one parse may take,
    /// and propagates what the parse or the assembly reports.
    pub fn emit(
        &mut self,
        input: &[u8],
        last: bool,
        first_in_region: bool,
        out: &mut Vec<u8>,
    ) -> Result<BlockType, Error> {
        let size = u32::try_from(input.len()).map_err(|_| Error::InvalidParameter)?;
        let value = input.first().copied().ok_or(Error::InvalidParameter)?;
        if input.len() > MAX_PARSE_BYTES {
            return Err(Error::InvalidParameter);
        }

        // RAW stores the decoded size and every block admits it, so it is the figure the other
        // candidates have to beat.
        let mut kind = BlockType::Raw;
        let mut stored = input.len();
        if stored > 1 && input.iter().all(|&byte| byte == value) {
            kind = BlockType::Rle;
            stored = 1;
        }

        let Self {
            parser,
            tables,
            payload,
            policy,
            statistics,
        } = self;
        let sequences = parser.parse(input)?;
        let assembled = tables.assemble_into(
            sequences,
            block::Terms {
                first_in_region,
                reuse: block::Reuse::SelfFinancing,
                ceiling: stored,
            },
            policy,
            payload,
        );
        let assembly = match assembled {
            Ok(assembly) => assembly,
            Err(Error::LimitExceeded { .. }) => {
                statistics.table_limited_blocks = statistics.table_limited_blocks.saturating_add(1);
                None
            }
            Err(error) => return Err(error),
        };
        if assembly.is_some() {
            kind = BlockType::Compressed;
            stored = payload.len();
        }

        let header = match kind {
            BlockType::Raw => BlockHeader::raw(last, size)?,
            BlockType::Rle => BlockHeader::rle(last, size)?,
            BlockType::Compressed => BlockHeader::compressed(last, size)?,
        };
        out.extend_from_slice(header.encode().as_bytes());
        match kind {
            BlockType::Raw => out.extend_from_slice(input),
            BlockType::Rle => out.push(value),
            BlockType::Compressed => out.extend_from_slice(payload),
        }

        if let Some(assembly) = assembly {
            let repeated = assembly.repeated().iter().filter(|&&named| named).count();
            tables.adopt(assembly);
            statistics.streams_repeated = statistics
                .streams_repeated
                .saturating_add(u64::try_from(repeated).unwrap_or(0));
        }
        statistics.blocks = statistics.blocks.saturating_add(1);
        statistics.input_bytes = statistics
            .input_bytes
            .saturating_add(u64::try_from(input.len()).unwrap_or(u64::MAX));
        statistics.output_bytes = statistics.output_bytes.saturating_add(
            u64::try_from(stored.saturating_add(BLOCK_HEADER_BYTES)).unwrap_or(u64::MAX),
        );
        let counted = match kind {
            BlockType::Raw => &mut statistics.raw_blocks,
            BlockType::Rle => &mut statistics.rle_blocks,
            BlockType::Compressed => &mut statistics.compressed_blocks,
        };
        *counted = counted.saturating_add(1);
        Ok(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Emitter, Layout};
    use crate::block;
    use crate::format::{
        BLOCK_HEADER_BYTES, BLOCK_STREAMS, BlockType, DecoderPolicy, Error, IntegrityMode,
        MAX_BLOCK_BYTES,
    };
    use crate::parser::Parser;

    fn header_cost(blocks: u64) -> u64 {
        blocks.saturating_mul(BLOCK_HEADER_BYTES as u64)
    }

    /// The input classes every selection figure is taken over.
    #[derive(Clone, Copy, Debug)]
    enum Class {
        Zeros,
        OneByte,
        Incompressible,
        Repetitive,
        Text,
        Sparse,
    }

    impl Class {
        const ALL: [Self; 6] = [
            Self::Zeros,
            Self::OneByte,
            Self::Incompressible,
            Self::Repetitive,
            Self::Text,
            Self::Sparse,
        ];

        const fn name(self) -> &'static str {
            match self {
                Self::Zeros => "zeros",
                Self::OneByte => "one-byte",
                Self::Incompressible => "incompressible",
                Self::Repetitive => "repetitive",
                Self::Text => "text",
                Self::Sparse => "sparse",
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
                        let word = position.wrapping_mul(0x9E37_79B9).wrapping_add(11);
                        let pick = u8::try_from(word.wrapping_shr(28) & 0x0F).unwrap_or(0);
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
                };
            }
            out
        }
    }

    /// A value that depends on its position and on nothing else.
    ///
    /// The mix avalanches rather than rotating, because a value taken from one multiply of
    /// the position carries the arithmetic structure of the position into the byte and a
    /// match finder reads that structure as content.
    fn mix(position: u64) -> u8 {
        let mut state = position.wrapping_add(0x9E37_79B9_7F4A_7C15);
        state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        u8::try_from((state ^ (state >> 31)).wrapping_shr(24) & 0xFF).unwrap_or(0)
    }

    /// Every mask of the four streams, which is what hindsight ranges over.
    fn masks() -> Vec<[bool; BLOCK_STREAMS]> {
        (0..16_u8)
            .map(|bits| {
                let mut mask = [false; BLOCK_STREAMS];
                for (index, slot) in mask.iter_mut().enumerate() {
                    *slot = bits & (1 << index) != 0;
                }
                mask
            })
            .collect()
    }

    #[test]
    fn a_layout_refuses_a_size_the_format_cannot_represent() {
        assert_eq!(Layout::new(0, 16), Err(Error::InvalidParameter));
        assert_eq!(Layout::new(16, 0), Err(Error::InvalidParameter));
        assert_eq!(
            Layout::new(16, MAX_BLOCK_BYTES.saturating_add(1)),
            Err(Error::InvalidParameter)
        );
    }

    #[test]
    fn the_default_sizes_describe_a_layout_the_format_can_carry() -> Result<(), Error> {
        let layout = Layout::new(DEFAULT_REGION_BYTES, DEFAULT_BLOCK_BYTES)?;
        assert_eq!(layout.region_bytes(), DEFAULT_REGION_BYTES);
        assert_eq!(layout.block_bytes(), DEFAULT_BLOCK_BYTES);
        let (end, last) = layout.block(0, DEFAULT_REGION_BYTES)?;
        assert_eq!(end, 65_536);
        assert!(!last);
        Ok(())
    }

    #[test]
    fn the_physical_size_counts_every_block_header_the_layout_writes() -> Result<(), Error> {
        let layout = Layout::new(1_024, 256)?;
        for (logical, blocks) in [(1_usize, 1_u64), (256, 1), (257, 2), (512, 2), (1_024, 4)] {
            let expected = u64::try_from(logical)
                .map_err(|_| Error::InvalidParameter)?
                .saturating_add(header_cost(blocks));
            assert_eq!(
                layout.physical_bytes(logical)?,
                expected,
                "logical {logical}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_blocks_of_a_region_cover_it_once_and_the_last_one_says_so() -> Result<(), Error> {
        let layout = Layout::new(1_024, 300)?;
        let len = 1_000_usize;
        let mut at = 0_usize;
        let mut blocks = 0_u64;
        let mut covered = 0_usize;
        while at < len {
            let (end, last) = layout.block(at, len)?;
            let span = end.checked_sub(at).ok_or(Error::InvalidParameter)?;
            assert!(span > 0 && span <= 300);
            assert_eq!(last, end == len);
            covered = covered.saturating_add(span);
            blocks = blocks.saturating_add(1);
            at = end;
        }
        assert_eq!(covered, len);
        assert_eq!(blocks, 4);
        assert_eq!(
            layout.physical_bytes(len)?,
            u64::try_from(len)
                .map_err(|_| Error::InvalidParameter)?
                .saturating_add(header_cost(blocks))
        );
        Ok(())
    }

    #[test]
    fn a_region_header_declares_what_its_blocks_stored() -> Result<(), Error> {
        let layout = Layout::new(1_024, 256)?;
        let region = layout.region(700, 712, IntegrityMode::PerRegion)?;
        assert_eq!(region.logical_size, 700);
        assert_eq!(region.physical_size, 712);
        assert_eq!(
            layout.region(2_048, 64, IntegrityMode::Absent),
            Err(Error::InvalidParameter),
            "a region above the layout's own staging is not one it can fill"
        );
        Ok(())
    }

    /// Each input class gets the type its content earns, and the type is what the emitter
    /// recorded.
    #[test]
    fn each_input_class_is_emitted_under_the_type_its_content_earns() -> Result<(), Error> {
        let block_bytes = 16_384_usize;
        let expected = [
            (Class::Zeros, BlockType::Rle),
            (Class::OneByte, BlockType::Rle),
            (Class::Incompressible, BlockType::Raw),
            (Class::Repetitive, BlockType::Compressed),
            (Class::Text, BlockType::Compressed),
            (Class::Sparse, BlockType::Compressed),
        ];
        for (class, want) in expected {
            let mut emitter = Emitter::new(DecoderPolicy::CONSERVATIVE, 16_384)?;
            let content = class.content(block_bytes);
            let mut out = Vec::new();
            let kind = emitter.emit(&content, true, true, &mut out)?;
            assert_eq!(kind, want, "class {}", class.name());
            let stats = emitter.statistics();
            assert_eq!(stats.blocks, 1);
            assert_eq!(
                stats.compressed_blocks.saturating_add(stats.raw_blocks) + stats.rle_blocks,
                1
            );
            let stored = out.len().saturating_sub(BLOCK_HEADER_BYTES);
            match kind {
                BlockType::Raw => assert_eq!(stored, block_bytes),
                BlockType::Rle => assert_eq!(stored, 1),
                BlockType::Compressed => assert!(stored < block_bytes),
            }
        }
        Ok(())
    }

    /// A block whose tables do not fit policy is emitted under another type, and the emitter
    /// records that it was.
    #[test]
    fn a_block_whose_tables_are_above_policy_is_emitted_as_raw_and_recorded() -> Result<(), Error> {
        let content = Class::Text.content(16_384);
        let policy = DecoderPolicy::CONSERVATIVE;
        let mut wide = Emitter::new(policy, 16_384)?;
        let mut room = Vec::new();
        assert_eq!(
            wide.emit(&content, true, true, &mut room)?,
            BlockType::Compressed
        );
        assert_eq!(wide.statistics().table_limited_blocks, 0);

        // The same content under a policy whose quarter share cannot hold one table.
        let narrow = policy.with_max_table_bytes(64);
        let mut limited = Emitter::new(narrow, 16_384)?;
        let mut out = Vec::new();
        assert_eq!(
            limited.emit(&content, true, true, &mut out)?,
            BlockType::Raw
        );
        let stats = limited.statistics();
        assert_eq!(stats.table_limited_blocks, 1);
        assert_eq!(stats.raw_blocks, 1);
        assert_eq!(stats.compressed_blocks, 0);
        Ok(())
    }

    /// The reuse trigger against hindsight.
    ///
    /// The type choice is exhaustive by construction, so the only freedom that can be worse
    /// than hindsight is which streams named the table in force. This walks every block of
    /// every class, assembles it under all sixteen masks, and names the blocks where the
    /// trigger spent more than the cheapest of them.
    #[test]
    fn the_reuse_trigger_is_never_dearer_than_the_cheapest_mask_in_hindsight() -> Result<(), Error>
    {
        let block_bytes = 8_192_usize;
        let policy = DecoderPolicy::CONSERVATIVE;
        let mut worse: Vec<String> = Vec::new();
        let mut assembled = 0_usize;
        for class in Class::ALL {
            let content = class.content(block_bytes.saturating_mul(6));
            let mut parser = Parser::new();
            let mut tables = block::Encoder::at_region_start();
            let mut at = 0_usize;
            let mut index = 0_usize;
            while at < content.len() {
                let end = at.saturating_add(block_bytes).min(content.len());
                let input = content.get(at..end).ok_or(Error::InvalidParameter)?;
                let sequences = parser.parse(input)?;
                let first_in_region = at == 0;

                let mut chosen = Vec::new();
                let taken = tables.assemble_into(
                    sequences,
                    block::Terms {
                        first_in_region,
                        reuse: block::Reuse::SelfFinancing,
                        ceiling: usize::MAX,
                    },
                    &policy,
                    &mut chosen,
                )?;

                let mut best = chosen.len();
                let mut scratch = Vec::new();
                for mask in masks() {
                    if first_in_region && mask.iter().any(|&named| named) {
                        continue;
                    }
                    let named = tables.assemble_into(
                        sequences,
                        block::Terms {
                            first_in_region,
                            reuse: block::Reuse::Named(mask),
                            ceiling: usize::MAX,
                        },
                        &policy,
                        &mut scratch,
                    );
                    if named.is_ok() {
                        best = best.min(scratch.len());
                    }
                }
                if chosen.len() > best {
                    worse.push(format!(
                        "{} block {index}: the trigger stored {} where a mask stored {best}",
                        class.name(),
                        chosen.len()
                    ));
                }
                assembled = assembled.saturating_add(1);
                if let Some(taken) = taken {
                    tables.adopt(taken);
                }
                at = end;
                index = index.saturating_add(1);
            }
        }
        assert_eq!(assembled, 36, "a block was skipped rather than assembled");
        assert!(
            worse.is_empty(),
            "the trigger chose worse than hindsight on {} of {assembled} blocks: {worse:?}",
            worse.len()
        );
        Ok(())
    }
}

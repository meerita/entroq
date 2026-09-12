//! Owns the encoder engine: the path from input bytes to a compliant stream.
//!
//! This module does not own the format contract, match finding, parse strategy, or entropy
//! coding. It composes them.
//!
//! The layout this module states is encoder policy. It decides how staged input becomes
//! regions and blocks, and a decoder never learns it: every size it implies is declared in a
//! header the decoder reads. A later encoder generation replaces it without changing what a
//! decoder accepts.

use crate::format::{
    BLOCK_HEADER_BYTES, BlockHeader, Error, IntegrityMode, MAX_BLOCK_BYTES, RegionHeader,
};

/// The input bytes one region holds before the encoder closes it.
///
/// The value is provisional. It is not a format constant: the region header declares the
/// sizes a decoder needs, so a measurement replaces this default without changing what a
/// decoder accepts.
pub const DEFAULT_REGION_BYTES: usize = 1_048_576;

/// The input bytes one block holds.
///
/// Provisional on the same terms as the region size.
pub const DEFAULT_BLOCK_BYTES: u32 = 65_536;

/// How the encoder divides staged input into regions and blocks.
///
/// Every block this layout describes is RAW. The format decodes more than that, and which
/// representation an encoder chooses for a block is an encoder decision, not a format one.
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

    /// The header of a region that `logical` staged bytes fill.
    ///
    /// The physical size counts the block headers this layout will write, so the header is
    /// complete before the first block is emitted and a reader reaches the next record
    /// without an index.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the sizes cannot describe a region this layout can
    /// fill.
    pub fn region(self, logical: usize, integrity: IntegrityMode) -> Result<RegionHeader, Error> {
        let physical = self.physical_bytes(logical)?;
        RegionHeader::new(
            u64::try_from(logical).map_err(|_| Error::InvalidParameter)?,
            physical,
            integrity,
        )
    }

    /// The bytes the blocks of a region of `logical` staged bytes occupy.
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

    /// The header of the block that starts at `at` in a staged region of `len` bytes, and
    /// the offset the block ends at.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the block is empty or above what a header declares.
    pub fn block(self, at: usize, len: usize) -> Result<(BlockHeader, usize), Error> {
        let block_bytes = usize::try_from(self.block_bytes).map_err(|_| Error::InvalidParameter)?;
        let end = at
            .checked_add(block_bytes)
            .ok_or(Error::InvalidParameter)?
            .min(len);
        let size = u32::try_from(end.checked_sub(at).ok_or(Error::InvalidParameter)?)
            .map_err(|_| Error::InvalidParameter)?;
        Ok((BlockHeader::raw(end == len, size)?, end))
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Layout};
    use crate::format::{BLOCK_HEADER_BYTES, Error, IntegrityMode, MAX_BLOCK_BYTES};

    fn header_cost(blocks: u64) -> u64 {
        blocks.saturating_mul(BLOCK_HEADER_BYTES as u64)
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
        let (block, end) = layout.block(0, DEFAULT_REGION_BYTES)?;
        assert_eq!(block.decoded_len(), DEFAULT_BLOCK_BYTES);
        assert!(!block.last);
        assert_eq!(end, 65_536);
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
            let (block, end) = layout.block(at, len)?;
            let span = end.checked_sub(at).ok_or(Error::InvalidParameter)?;
            assert_eq!(
                block.decoded_len(),
                u32::try_from(span).map_err(|_| Error::InvalidParameter)?
            );
            assert_eq!(block.last, end == len);
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
    fn a_region_header_declares_what_the_layout_will_write() -> Result<(), Error> {
        let layout = Layout::new(1_024, 256)?;
        let region = layout.region(700, IntegrityMode::PerRegion)?;
        assert_eq!(region.logical_size, 700);
        assert_eq!(region.physical_size, 712);
        assert_eq!(region.integrity, Some(0));
        Ok(())
    }
}

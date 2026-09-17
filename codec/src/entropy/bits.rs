//! Owns the bit stream the entropy coders read and write: the writer, the reader, and the
//! bit order both work in.
//!
//! This module does not own what a bit means. A coder owns its own symbols and its own
//! description.
//!
//! Bit order is most significant first, inside a value and inside the stream. Every bit moves
//! by explicit shift and byte position, and no native integer reaches the stream through its
//! in-memory representation, so the same bits are written and read on every architecture.
//!
//! ```text
//! peek   reads zeros past the end, because a table-driven decoder peeks the widest code
//!        before it knows how wide the code is
//! take   refuses past the end, because a parse that read zeros would rebuild a plausible
//!        table out of the padding
//! ```
//!
//! The two are separate calls rather than one call with a flag, so a parse cannot reach the
//! padding by accident. A payload decode uses `peek` and is checked against `overrun` when it
//! ends, which is exact: the stream carries its bit count and the decoder carries its
//! position.

use super::{low_byte, narrow, shift_left, shift_left_wide, shift_right, shift_right_wide};
use crate::format::{Corruption, Error};

/// The widest value one push may carry.
///
/// The accumulator holds fewer than eight bits between calls, so 57 is what fits without a
/// second flush inside the push. Every width the coders write is far below it: a Huffman code
/// is at most 15 bits and a description field at most 13.
pub const MAX_WIDTH: u32 = 57;

/// The low `width` bits, as a mask.
///
/// A width of zero is a real case: a symbol whose raw suffix is empty writes no bits at all.
#[must_use]
pub const fn mask(width: u32) -> u64 {
    if width == 0 {
        0
    } else if width >= 64 {
        u64::MAX
    } else {
        shift_left(1, width).wrapping_sub(1)
    }
}

/// A stream of bits and the exact count of them.
///
/// The count is carried and not derived from the byte length. The final byte is padded, and a
/// reader that took the byte length for the bit count would read the padding as content.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BitBuf {
    bytes: Vec<u8>,
    bits: u64,
}

impl BitBuf {
    /// The bytes the stream occupies, final padding included.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The bits the stream carries, padding excluded.
    #[must_use]
    pub const fn bits(&self) -> u64 {
        self.bits
    }

    /// Whether the stream carries no bits.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bits == 0
    }
}

/// Writes bits most significant first, through a 64-bit accumulator.
#[derive(Debug, Default)]
pub struct BitWriter {
    acc: u64,
    held: u32,
    out: Vec<u8>,
    bits: u64,
}

impl BitWriter {
    /// A writer holding no bits.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            acc: 0,
            held: 0,
            out: Vec::new(),
            bits: 0,
        }
    }

    /// The bits written so far.
    #[must_use]
    pub const fn bits(&self) -> u64 {
        self.bits
    }

    /// Appends the low `width` bits of `value`, most significant first.
    ///
    /// A width above `MAX_WIDTH` is written in pieces rather than refused, so the writer has no
    /// precondition a caller can violate and no width is rejected in one build and accepted in
    /// another. Every width the coders write is far below the limit.
    pub fn push(&mut self, value: u64, width: u32) {
        let mut left = width;
        while left > MAX_WIDTH {
            left = left.saturating_sub(MAX_WIDTH);
            self.put(shift_right(value, left), MAX_WIDTH);
        }
        self.put(value, left);
    }

    /// Appends the low `width` bits of `value`, for a `width` the accumulator holds whole.
    fn put(&mut self, value: u64, width: u32) {
        self.acc = shift_left(self.acc, width) | (value & mask(width));
        self.held = self.held.saturating_add(width);
        self.bits = self.bits.saturating_add(u64::from(width));
        while self.held >= 8 {
            self.held = self.held.saturating_sub(8);
            self.out.push(low_byte(shift_right(self.acc, self.held)));
        }
    }

    /// Closes the stream, padding the final byte with zeros.
    #[must_use]
    pub fn finish(mut self) -> BitBuf {
        if self.held > 0 {
            let pad = 8u32.saturating_sub(self.held);
            self.out.push(low_byte(shift_left(self.acc, pad)));
        }
        BitBuf {
            bytes: self.out,
            bits: self.bits,
        }
    }
}

/// Reads bits most significant first, from the first bit written.
#[derive(Clone, Copy, Debug)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    bits: u64,
    at: u64,
}

impl<'a> BitReader<'a> {
    /// A reader over `bytes`, which carries `bits` bits of content.
    ///
    /// The bit count is the caller's declaration. A count above what the bytes hold makes
    /// every read past the bytes return zeros, which `overrun` does not catch, so a caller
    /// that reads a declared count from a stream checks it against the bytes first.
    #[must_use]
    pub const fn new(bytes: &'a [u8], bits: u64) -> Self {
        Self { bytes, bits, at: 0 }
    }

    /// A reader over every bit of `buf`.
    #[must_use]
    pub fn over(buf: &'a BitBuf) -> Self {
        Self::new(buf.bytes(), buf.bits())
    }

    /// The bits the stream declared.
    #[must_use]
    pub const fn bits(&self) -> u64 {
        self.bits
    }

    /// The bits consumed so far, which may pass the declared count.
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.at
    }

    /// Whether the reader has passed the end of what the stream declared.
    #[must_use]
    pub const fn overrun(&self) -> bool {
        self.at > self.bits
    }

    /// The next `width` bits without consuming them, reading zeros past the end.
    #[must_use]
    pub fn peek(&self, width: u32) -> u64 {
        bits_at(self.bytes, self.at, width)
    }

    /// Advances by `width` bits, which may pass the end.
    pub const fn skip(&mut self, width: u32) {
        self.at = self.at.saturating_add(width as u64);
    }

    /// The next `width` bits, refusing to pass the end.
    ///
    /// # Errors
    ///
    /// Returns `TruncatedInput` when the stream declares fewer bits than the field needs.
    pub fn take(&mut self, width: u32) -> Result<u64, Error> {
        let end = self.at.saturating_add(u64::from(width));
        if end > self.bits {
            return Err(Error::TruncatedInput {
                needed: usize::try_from(end.div_ceil(8)).unwrap_or(usize::MAX),
            });
        }
        let value = bits_at(self.bytes, self.at, width);
        self.at = end;
        Ok(value)
    }

    /// Reports the overrun a payload decode ended in.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the decode consumed more bits than the stream declared,
    /// which means it decoded padding as content.
    pub const fn finish(&self) -> Result<(), Error> {
        if self.overrun() {
            return Err(Error::CorruptData(Corruption::CodedStream));
        }
        Ok(())
    }
}

/// The bits at `[start, start + width)` of a most-significant-first packing.
///
/// Out of range reads as zero.
fn bits_at(bytes: &[u8], start: u64, width: u32) -> u64 {
    if width == 0 {
        return 0;
    }
    let Ok(first) = usize::try_from(shift_right(start, 3)) else {
        return 0;
    };
    let offset = bit_offset(start);
    let spanned = offset.saturating_add(width).div_ceil(8);
    let mut acc: u128 = 0;
    let mut lane = 0u32;
    while lane < spanned {
        let byte = usize::try_from(lane)
            .ok()
            .and_then(|lane| first.checked_add(lane))
            .and_then(|at| bytes.get(at))
            .copied()
            .unwrap_or(0);
        acc = shift_left_wide(acc, 8) | u128::from(byte);
        lane = lane.saturating_add(1);
    }
    let loaded = spanned.saturating_mul(8);
    let shift = loaded.saturating_sub(offset).saturating_sub(width);
    narrow(shift_right_wide(acc, shift) & u128::from(mask(width)))
}

// The offset is masked below eight, so the narrowing is exact.
#[allow(clippy::cast_possible_truncation)]
const fn bit_offset(at: u64) -> u32 {
    (at & 7) as u32
}

#[cfg(test)]
mod tests {
    use super::{BitReader, BitWriter, MAX_WIDTH, mask};
    use crate::format::{Corruption, Error};

    /// The widths a round trip is checked at: the boundaries, the byte crossings, the widest
    /// push the accumulator holds whole, and the widths above it that the writer splits.
    const WIDTHS: [u32; 14] = [0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 32, MAX_WIDTH, 58, 64];

    fn value_for(width: u32, seed: u64) -> u64 {
        seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) & mask(width)
    }

    #[test]
    fn a_mask_covers_its_width_and_nothing_above_it() {
        assert_eq!(mask(0), 0);
        assert_eq!(mask(1), 1);
        assert_eq!(mask(8), 0xFF);
        assert_eq!(mask(63), u64::MAX >> 1);
        assert_eq!(mask(64), u64::MAX);
        assert_eq!(mask(200), u64::MAX);
    }

    #[test]
    fn every_width_round_trips_at_every_bit_offset() {
        for lead in 0..8u32 {
            for &width in &WIDTHS {
                let mut writer = BitWriter::new();
                writer.push(mask(lead), lead);
                let mut expected = Vec::new();
                for step in 0..16u64 {
                    let value = value_for(width, step);
                    writer.push(value, width);
                    expected.push(value);
                }
                let buf = writer.finish();
                assert_eq!(buf.bits(), u64::from(lead) + u64::from(width) * 16);

                let mut reader = BitReader::over(&buf);
                assert_eq!(reader.take(lead), Ok(mask(lead)));
                for value in expected {
                    assert_eq!(reader.take(width), Ok(value));
                }
                assert_eq!(reader.consumed(), buf.bits());
                assert!(!reader.overrun());
            }
        }
    }

    #[test]
    fn a_peek_does_not_consume_and_a_skip_does() {
        let mut writer = BitWriter::new();
        writer.push(0b1011_0010, 8);
        let buf = writer.finish();

        let mut reader = BitReader::over(&buf);
        assert_eq!(reader.peek(4), 0b1011);
        assert_eq!(reader.peek(4), 0b1011);
        assert_eq!(reader.consumed(), 0);
        reader.skip(4);
        assert_eq!(reader.peek(4), 0b0010);
        assert_eq!(reader.consumed(), 4);
    }

    #[test]
    fn a_peek_past_the_end_reads_zeros_and_a_take_refuses() {
        let mut writer = BitWriter::new();
        writer.push(0b111, 3);
        let buf = writer.finish();

        let mut reader = BitReader::over(&buf);
        assert_eq!(reader.peek(8), 0b1110_0000);
        assert_eq!(
            reader.take(4),
            Err(Error::TruncatedInput { needed: 1 }),
            "a field wider than the stream is truncation, not zeros"
        );
        assert_eq!(reader.take(3), Ok(0b111));
        assert_eq!(reader.take(0), Ok(0));
    }

    #[test]
    fn an_overrun_is_reported_when_the_decode_ends() {
        let mut writer = BitWriter::new();
        writer.push(0b1010, 4);
        let buf = writer.finish();

        let mut reader = BitReader::over(&buf);
        reader.skip(4);
        assert_eq!(reader.finish(), Ok(()));
        reader.skip(1);
        assert!(reader.overrun());
        assert_eq!(
            reader.finish(),
            Err(Error::CorruptData(Corruption::CodedStream))
        );
    }

    #[test]
    fn an_empty_stream_carries_no_bits_and_no_bytes() {
        let buf = BitWriter::new().finish();
        assert!(buf.is_empty());
        assert_eq!(buf.bits(), 0);
        assert_eq!(buf.bytes(), &[] as &[u8]);
        let mut reader = BitReader::over(&buf);
        assert_eq!(reader.peek(16), 0);
        assert!(reader.take(1).is_err());
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn a_width_above_what_the_accumulator_holds_is_written_in_pieces() {
        // The writer has no precondition, so a width past the accumulator is split rather than
        // refused in one build and accepted in another.
        let mut writer = BitWriter::new();
        writer.push(u64::MAX, 64);
        writer.push(0x0123_4567_89AB_CDEF, 100);
        let buf = writer.finish();
        assert_eq!(buf.bits(), 164);

        let mut reader = BitReader::over(&buf);
        assert_eq!(reader.take(64), Ok(u64::MAX));
        assert_eq!(reader.take(36), Ok(0), "a width past 64 leads with zeros");
        assert_eq!(reader.take(64), Ok(0x0123_4567_89AB_CDEF));
        assert!(!reader.overrun());
    }

    #[test]
    fn the_final_byte_is_padded_with_zeros() {
        let mut writer = BitWriter::new();
        writer.push(0b1, 1);
        let buf = writer.finish();
        assert_eq!(buf.bits(), 1);
        assert_eq!(buf.bytes(), &[0b1000_0000]);
    }
}

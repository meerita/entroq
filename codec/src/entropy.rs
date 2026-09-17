//! Owns entropy coding: the coders, the table descriptions they transmit, the validation a
//! decoder applies to a description before it allocates, and the bit IO they run on.
//!
//! This module does not own sequence representation, the parse that produced it, or the block
//! that carries the descriptions.
//!
//! One coder per symbol class, and one rANS implementation with the state count as a parameter
//! rather than one coder per state count:
//!
//! ```text
//! literal bytes     canonical Huffman, length limited, single-level table
//! literal runs      rANS, 1 state
//! match lengths     rANS, 2 states
//! match distances   rANS, 4 states
//! ```
//!
//! Interleaving costs one flushed state per state per block and buys the decoder independent
//! dependency chains, so the state count a symbol class affords rises with the share of that
//! class the raw suffix carries. Literals carry no suffix, so they carry no interleaving.
//!
//! Every coder follows the same order, and the order is the contract:
//!
//! ```text
//! parse       bits -> what a decoder has read and does not yet trust
//! validate    what it read -> what it may allocate, or a typed rejection
//! build       an admitted description -> the decode table
//! ```
//!
//! A decode table is reachable only from an admitted description, and an admitted description
//! is reachable only from validation, so no allocation can precede the validation that sizes
//! it. The requirement is pure arithmetic over the admitted description, so a decoder states
//! what it would allocate before it allocates anything.

pub mod bits;
pub mod huffman;
pub mod rans;

use crate::format::Error;

/// The peak table memory one block may declare, over every table it decodes under.
///
/// A version-1 constant. The declared limits alone would admit more: one Huffman table at the
/// 15-bit length limit is 65 536 bytes by itself and three rANS tables at the widest table log
/// are 49 152 bytes together, so the structural maximum at the shipped shapes is 114 688. The
/// ceiling therefore binds the encoder as well as the stream, and an encoder that cannot meet
/// it reports failure to its caller rather than emitting a block a decoder refuses.
pub const MAX_BLOCK_TABLE_BYTES: u64 = 65_536;

/// The widest alphabet a coder's table holds.
///
/// Both shipped table shapes carry the symbol in eight bits: the Huffman entry packs a symbol
/// and a code width into sixteen, and the rANS entry packs a symbol, a frequency and a
/// cumulative start into thirty-two.
pub const MAX_ALPHABET_SIZE: u32 = 256;

/// Admits a block's declared peak table memory, or refuses it.
///
/// The figure is a sum over the descriptions a block carries, computed from the descriptions
/// alone, so a decoder calls this before it builds any of the tables.
///
/// # Errors
///
/// Returns `LimitExceeded` naming the declared requirement and the ceiling when the block asks
/// for more than one block may declare.
pub const fn admit_table_bytes(declared: u64) -> Result<u64, Error> {
    if declared > MAX_BLOCK_TABLE_BYTES {
        return Err(Error::LimitExceeded {
            declared,
            allowed: MAX_BLOCK_TABLE_BYTES,
        });
    }
    Ok(declared)
}

/// The smallest power of two at or above `value`, as an exponent.
pub(crate) const fn ceil_log2(value: u64) -> u32 {
    if value <= 1 {
        0
    } else {
        64u32.saturating_sub(value.saturating_sub(1).leading_zeros())
    }
}

/// The bits a value at most `bound` needs.
pub(crate) const fn width_for(bound: u64) -> u32 {
    if bound == 0 {
        1
    } else {
        64u32.saturating_sub(bound.leading_zeros())
    }
}

// A shift at or above the width of the value clears it, which is the arithmetic every call
// site here already checks for. Wrapping would return the value unshifted instead.
#[allow(clippy::arithmetic_side_effects)]
pub(crate) const fn shift_left(value: u64, by: u32) -> u64 {
    if by >= 64 { 0 } else { value << by }
}

#[allow(clippy::arithmetic_side_effects)]
pub(crate) const fn shift_right(value: u64, by: u32) -> u64 {
    if by >= 64 { 0 } else { value >> by }
}

#[allow(clippy::arithmetic_side_effects)]
pub(crate) const fn shift_left_wide(value: u128, by: u32) -> u128 {
    if by >= 128 { 0 } else { value << by }
}

#[allow(clippy::arithmetic_side_effects)]
pub(crate) const fn shift_right_wide(value: u128, by: u32) -> u128 {
    if by >= 128 { 0 } else { value >> by }
}

// The mask keeps the value below 256, so the narrowing is exact.
#[allow(clippy::cast_possible_truncation)]
pub(crate) const fn low_byte(value: u64) -> u8 {
    (value & 0xFF) as u8
}

// The caller masks the value to at most 64 bits, so the narrowing is exact.
#[allow(clippy::cast_possible_truncation)]
pub(crate) const fn narrow(value: u128) -> u64 {
    value as u64
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_ALPHABET_SIZE, MAX_BLOCK_TABLE_BYTES, admit_table_bytes, bits::BitReader,
        bits::BitWriter, ceil_log2, huffman, rans, shift_right, width_for,
    };
    use crate::format::{Corruption, Error};

    /// The alphabets the coders are proved over.
    ///
    /// One symbol, two, the widest the tables hold, and the sizes between that cross a field
    /// width or a power of two.
    const ALPHABETS: [u32; 9] = [1, 2, 3, 5, 17, 32, 60, 255, MAX_ALPHABET_SIZE];

    /// The state counts the rANS coder ships at.
    const STATE_COUNTS: [usize; 3] = [1, 2, 4];

    /// A deterministic stream over `alphabet_size` symbols, skewed so the coders meet a real
    /// distribution rather than a flat one.
    fn stream(alphabet_size: u32, len: usize, seed: u64) -> Vec<u16> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            // Two draws folded together concentrate the mass on the low symbols, which is the
            // shape a bucketed alphabet produces.
            let span = u64::from(alphabet_size);
            let first = shift_right(state, 33).checked_rem(span).unwrap_or(0);
            let second = shift_right(state, 11).checked_rem(span).unwrap_or(0);
            let symbol = first.min(second);
            out.push(u16::try_from(symbol).unwrap_or(0));
        }
        out
    }

    fn counts_of(symbols: &[u16], alphabet_size: u32) -> Vec<u64> {
        let span = usize::try_from(alphabet_size).unwrap_or(0);
        let mut counts = vec![0u64; span];
        for &symbol in symbols {
            if let Some(slot) = counts.get_mut(usize::from(symbol)) {
                *slot = slot.saturating_add(1);
            }
        }
        counts
    }

    /// Describes, parses, validates, builds, codes and decodes one Huffman stream.
    fn huffman_round_trip(symbols: &[u16], alphabet_size: u32) -> Result<u64, Error> {
        let counts = counts_of(symbols, alphabet_size);
        let code = huffman::Code::build(&counts, alphabet_size, huffman::LENGTH_LIMIT)?;

        let mut writer = BitWriter::new();
        code.describe(&mut writer);
        let description = writer.finish();

        let mut writer = BitWriter::new();
        code.encoder()?.write(symbols, &mut writer)?;
        let payload = writer.finish();

        let mut reader = BitReader::over(&description);
        let declared = huffman::Declared::parse(&mut reader, alphabet_size)?;
        assert_eq!(
            reader.consumed(),
            description.bits(),
            "the parse read every bit the description carried"
        );
        let admitted = declared.validate()?;
        let table = admitted.build()?;
        assert_eq!(
            admitted.table_bytes(),
            table.allocated_bytes(),
            "the declared requirement is the bytes the table occupies"
        );

        let mut decoded = vec![0u16; symbols.len()];
        let mut reader = BitReader::over(&payload);
        table.decode(&mut reader, &mut decoded)?;
        assert_eq!(decoded, symbols, "the Huffman coder round trips");
        Ok(admitted.table_bytes())
    }

    /// Describes, parses, validates, builds, codes and decodes one rANS stream.
    fn rans_round_trip(symbols: &[u16], alphabet_size: u32, states: usize) -> Result<u64, Error> {
        let counts = counts_of(symbols, alphabet_size);
        let table = rans::Table::normalize(&counts, alphabet_size)?;

        let mut writer = BitWriter::new();
        table.describe(&mut writer);
        let description = writer.finish();
        let payload = table.encoder()?.write(symbols, states)?;

        let mut reader = BitReader::over(&description);
        let declared = rans::Declared::parse(&mut reader, alphabet_size)?;
        assert_eq!(
            reader.consumed(),
            description.bits(),
            "the parse read every bit the description carried"
        );
        let admitted = declared.validate()?;
        let built = admitted.build()?;
        assert_eq!(
            admitted.table_bytes(),
            built.allocated_bytes(),
            "the declared requirement is the bytes the table occupies"
        );

        let mut decoded = vec![0u16; symbols.len()];
        let read = built.decode(&payload, &mut decoded, states)?;
        assert_eq!(decoded, symbols, "the rANS coder round trips");
        assert_eq!(read, payload.len(), "the decode consumed the whole payload");
        Ok(admitted.table_bytes())
    }

    #[test]
    fn every_coder_round_trips_every_alphabet_size() -> Result<(), Error> {
        for &alphabet_size in &ALPHABETS {
            for (seed, &len) in [1usize, 2, 3, 40, 517, 4_099].iter().enumerate() {
                let symbols = stream(alphabet_size, len, seed as u64 + 1);
                let _ = huffman_round_trip(&symbols, alphabet_size)?;
                for &states in &STATE_COUNTS {
                    let _ = rans_round_trip(&symbols, alphabet_size, states)?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn a_stream_of_one_distinct_symbol_round_trips_and_costs_no_payload() -> Result<(), Error> {
        for &alphabet_size in &ALPHABETS {
            for symbol in [0u16, 1, 7, 254] {
                if u32::from(symbol) >= alphabet_size {
                    continue;
                }
                let symbols = vec![symbol; 300];
                assert_eq!(
                    huffman_round_trip(&symbols, alphabet_size)?,
                    0,
                    "a single-symbol code builds no table"
                );
                for &states in &STATE_COUNTS {
                    let _ = rans_round_trip(&symbols, alphabet_size, states)?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn an_empty_stream_decodes_to_nothing_and_builds_no_table() -> Result<(), Error> {
        let counts = vec![0u64; 16];
        assert_eq!(
            huffman::Code::build(&counts, 16, huffman::LENGTH_LIMIT),
            Err(Error::InvalidParameter),
            "an empty stream has no code"
        );
        assert_eq!(
            rans::Table::normalize(&counts, 16),
            Err(Error::InvalidParameter),
            "an empty stream has no table"
        );

        // A block whose stream carries no symbol decodes no symbol, under whatever table the
        // block declared for the streams that do.
        let symbols = stream(16, 500, 9);
        let counts = counts_of(&symbols, 16);
        let code = huffman::Code::build(&counts, 16, huffman::LENGTH_LIMIT)?;
        let mut writer = BitWriter::new();
        code.describe(&mut writer);
        let description = writer.finish();
        let table = huffman::Declared::parse(&mut BitReader::over(&description), 16)?
            .validate()?
            .build()?;
        let nothing = BitWriter::new().finish();
        let mut reader = BitReader::over(&nothing);
        table.decode(&mut reader, &mut [])?;
        assert_eq!(reader.consumed(), 0);

        let rans_table = rans::Table::normalize(&counts, 16)?;
        let mut writer = BitWriter::new();
        rans_table.describe(&mut writer);
        let description = writer.finish();
        let built = rans::Declared::parse(&mut BitReader::over(&description), 16)?
            .validate()?
            .build()?;
        for &states in &STATE_COUNTS {
            let payload = rans_table.encoder()?.write(&[], states)?;
            assert_eq!(built.decode(&payload, &mut [], states)?, payload.len());
        }
        Ok(())
    }

    #[test]
    fn a_rebuilt_table_is_the_table_that_wrote_the_description() -> Result<(), Error> {
        for &alphabet_size in &ALPHABETS {
            let symbols = stream(alphabet_size, 2_000, 11);
            let counts = counts_of(&symbols, alphabet_size);

            let code = huffman::Code::build(&counts, alphabet_size, huffman::LENGTH_LIMIT)?;
            let mut writer = BitWriter::new();
            code.describe(&mut writer);
            let description = writer.finish();
            let rebuilt =
                huffman::Declared::parse(&mut BitReader::over(&description), alphabet_size)?
                    .validate()?;
            assert_eq!(rebuilt.code(), &code, "the rebuilt code is the code");

            let table = rans::Table::normalize(&counts, alphabet_size)?;
            let mut writer = BitWriter::new();
            table.describe(&mut writer);
            let description = writer.finish();
            let rebuilt = rans::Declared::parse(&mut BitReader::over(&description), alphabet_size)?
                .validate()?;
            assert_eq!(rebuilt.table(), &table, "the rebuilt table is the table");
        }
        Ok(())
    }

    #[test]
    fn a_truncated_description_is_truncation_and_never_a_table_of_zeros() -> Result<(), Error> {
        for &alphabet_size in &[17u32, 60, 255] {
            let symbols = stream(alphabet_size, 3_000, 13);
            let counts = counts_of(&symbols, alphabet_size);

            let code = huffman::Code::build(&counts, alphabet_size, huffman::LENGTH_LIMIT)?;
            let mut writer = BitWriter::new();
            code.describe(&mut writer);
            let description = writer.finish();
            for cut in 0..description.bits() {
                let mut reader = BitReader::new(description.bytes(), cut);
                let parsed = huffman::Declared::parse(&mut reader, alphabet_size);
                assert!(
                    matches!(
                        parsed,
                        Err(Error::TruncatedInput { .. }
                            | Error::CorruptData(Corruption::DescriptionToken))
                    ),
                    "a description cut at {cut} bits was not refused"
                );
            }

            let table = rans::Table::normalize(&counts, alphabet_size)?;
            let mut writer = BitWriter::new();
            table.describe(&mut writer);
            let description = writer.finish();
            for cut in 0..description.bits() {
                let mut reader = BitReader::new(description.bytes(), cut);
                match rans::Declared::parse(&mut reader, alphabet_size) {
                    // A short read can still describe a whole table when the cut falls after
                    // the last frequency the total needed. It never describes a different one,
                    // and it never rebuilds from padding.
                    Ok(declared) => assert_eq!(declared.validate()?.table(), &table),
                    Err(error) => assert!(
                        matches!(error, Error::TruncatedInput { .. } | Error::CorruptData(_)),
                        "a description cut at {cut} bits was refused as {error:?}"
                    ),
                }
            }
        }
        Ok(())
    }

    /// A decode table is built only from an admitted description, and a description is admitted
    /// only by validation, so a description validation refuses never reaches an allocation.
    /// What this walks is the other half: every description validation does admit declares,
    /// before it allocates, exactly the bytes it then occupies.
    #[test]
    fn no_allocation_precedes_the_validation_that_sizes_it() -> Result<(), Error> {
        for &alphabet_size in &[5u32, 17, 60] {
            let symbols = stream(alphabet_size, 1_500, 23);
            let counts = counts_of(&symbols, alphabet_size);

            let mut writer = BitWriter::new();
            huffman::Code::build(&counts, alphabet_size, huffman::LENGTH_LIMIT)?
                .describe(&mut writer);
            let code_bits = writer.finish();

            let mut writer = BitWriter::new();
            rans::Table::normalize(&counts, alphabet_size)?.describe(&mut writer);
            let table_bits = writer.finish();

            for (bits, family) in [(&code_bits, 0u8), (&table_bits, 1u8)] {
                for flip in 0..bits.bits() {
                    let mut bytes = bits.bytes().to_vec();
                    let at = usize::try_from(shift_right(flip, 3)).unwrap_or(0);
                    if let Some(byte) = bytes.get_mut(at) {
                        *byte ^= 0x80u8 >> (flip & 7);
                    }
                    let mut reader = BitReader::new(&bytes, bits.bits());
                    let requirement = if family == 0 {
                        match huffman::Declared::parse(&mut reader, alphabet_size) {
                            Ok(declared) => match declared.validate() {
                                Ok(admitted) => Some((
                                    admitted.table_bytes(),
                                    admitted.build()?.allocated_bytes(),
                                )),
                                Err(_) => None,
                            },
                            Err(_) => None,
                        }
                    } else {
                        match rans::Declared::parse(&mut reader, alphabet_size) {
                            Ok(declared) => match declared.validate() {
                                Ok(admitted) => Some((
                                    admitted.table_bytes(),
                                    admitted.build()?.allocated_bytes(),
                                )),
                                Err(_) => None,
                            },
                            Err(_) => None,
                        }
                    };
                    if let Some((declared, allocated)) = requirement {
                        assert_eq!(
                            declared, allocated,
                            "a description admitted after a flip at bit {flip} allocated what \
                             it did not declare"
                        );
                        assert!(admit_table_bytes(declared).is_ok());
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn four_descriptions_above_the_ceiling_are_refused_with_the_bytes_they_needed() {
        assert_eq!(admit_table_bytes(MAX_BLOCK_TABLE_BYTES), Ok(65_536));
        assert_eq!(
            admit_table_bytes(114_688),
            Err(Error::LimitExceeded {
                declared: 114_688,
                allowed: MAX_BLOCK_TABLE_BYTES,
            }),
            "the structural maximum at the shipped shapes is above the ceiling"
        );

        // The structural maximum: one Huffman table at the length limit and three rANS tables
        // at the widest table log.
        let widest = huffman::table_bytes_for(huffman::LENGTH_LIMIT)
            + 3 * rans::table_bytes_for(rans::TABLE_LOG_MAX);
        assert_eq!(widest, 114_688);
        assert!(admit_table_bytes(widest).is_err());
    }

    #[test]
    fn the_encoder_re_limits_a_huffman_code_that_would_not_fit() -> Result<(), Error> {
        // Every symbol of the widest alphabet occurs, so the narrowest complete code over them
        // is eight bits and its table is 512 bytes. The figure is a property of the counts,
        // which is what makes the refusal below exact rather than probable.
        let span = usize::try_from(MAX_ALPHABET_SIZE).unwrap_or(0);
        let mut counts = vec![0u64; span];
        let mut symbols: Vec<u16> = Vec::new();
        for (symbol, slot) in counts.iter_mut().enumerate() {
            let count = u64::try_from(span.saturating_sub(symbol)).unwrap_or(1);
            *slot = count;
            for _ in 0..count {
                symbols.push(u16::try_from(symbol).unwrap_or(0));
            }
        }

        let mut previous = u64::MAX;
        for budget in [65_536u64, 32_768, 16_384, 4_096, 1_024, 512] {
            let code = huffman::Code::build_within(&counts, MAX_ALPHABET_SIZE, budget)?;
            assert!(
                code.table_bytes() <= budget,
                "a re-limited code fits the budget it was built for"
            );
            assert!(code.table_bytes() <= previous);
            previous = code.table_bytes();

            // The re-limited code is still a complete prefix code over the same alphabet, and
            // it still codes every symbol the stream carries.
            let mut writer = BitWriter::new();
            code.describe(&mut writer);
            let description = writer.finish();
            let admitted =
                huffman::Declared::parse(&mut BitReader::over(&description), MAX_ALPHABET_SIZE)?
                    .validate()?;
            assert_eq!(admitted.code(), &code);

            let mut writer = BitWriter::new();
            code.encoder()?.write(&symbols, &mut writer)?;
            let payload = writer.finish();
            let mut decoded = vec![0u16; symbols.len()];
            admitted
                .build()?
                .decode(&mut BitReader::over(&payload), &mut decoded)?;
            assert_eq!(decoded, symbols);
        }
        assert_eq!(previous, huffman::table_bytes_for(8));

        // A budget below what the narrowest code over these symbols needs is refused, and the
        // error names the bytes that would have been required.
        assert_eq!(
            huffman::Code::build_within(&counts, MAX_ALPHABET_SIZE, 8),
            Err(Error::LimitExceeded {
                declared: huffman::table_bytes_for(8),
                allowed: 8,
            })
        );
        Ok(())
    }

    #[test]
    fn the_declared_requirement_is_arithmetic_over_the_description() {
        assert_eq!(huffman::table_bytes_for(1), 4);
        assert_eq!(huffman::table_bytes_for(11), 4_096);
        assert_eq!(huffman::table_bytes_for(huffman::LENGTH_LIMIT), 65_536);
        assert_eq!(rans::table_bytes_for(rans::TABLE_LOG_MIN), 128);
        assert_eq!(rans::table_bytes_for(rans::TABLE_LOG_MAX), 16_384);
    }

    #[test]
    fn the_shared_arithmetic_holds_at_its_boundaries() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(256), 8);
        assert_eq!(ceil_log2(257), 9);
        assert_eq!(width_for(0), 1);
        assert_eq!(width_for(1), 1);
        assert_eq!(width_for(2), 2);
        assert_eq!(width_for(4_096), 13);
    }
}

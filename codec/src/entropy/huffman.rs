//! Owns canonical, length-limited Huffman: the code, the description that transmits it, the
//! validation a decoder applies before it allocates, and the single-level table it decodes
//! through.
//!
//! This module does not own which symbol class takes Huffman, and it does not own the block
//! that carries the description.
//!
//! ```text
//! build       counts -> the optimal code of at most `limit` bits per symbol
//! describe    a code -> the bits the stream carries
//! parse       bits -> what a decoder has read and does not yet trust
//! validate    what it read -> what it may allocate, or a typed rejection
//! build       an admitted description -> the decode table
//! ```
//!
//! The order is the contract. A decode table is reachable only from an `Admitted`, and an
//! `Admitted` is reachable only from `validate`, so no allocation can precede the validation
//! that sizes it.
//!
//! The lengths are optimal rather than repaired. A limit can be met by building an
//! unrestricted code and shortening what exceeds the limit, which is cheaper and is not
//! optimal. Package merge builds the optimal length-limited code directly, so the Kraft sum is
//! exactly one and the limit holds by construction.
//!
//! A stream of one distinct symbol carries a code of no bits at all, and the description says
//! so with a flag. The symbol carries no information, and the alternative is a one-bit code
//! whose Kraft sum is a half.

use super::bits::{BitReader, BitWriter};
use super::{ceil_log2, shift_left, shift_right, shift_right_wide};
use crate::format::{Corruption, Error};

/// The longest code the format admits.
///
/// It bounds a decoder's table before the decoder has seen a table. A symbol that would want a
/// longer code occurs with probability below `2^-15`, so the limit costs almost nothing.
pub const LENGTH_LIMIT: u32 = 15;

/// Bits the literal token of a description spends on a code length.
///
/// Four, because the limit is 15 and no length the construction can produce needs a fifth bit.
/// A length above the limit is therefore not expressible, and validation rejects one anyway,
/// because a description can also reach validation from a structure this module did not write.
const LITERAL_BITS: u32 = 4;

/// The token widths of the description's two run forms.
const REPEAT_COUNT_BITS: u32 = 7;
const ZERO_COUNT_BITS: u32 = 8;

/// The shortest run either form encodes. A shorter run costs less as literals.
const MIN_RUN: usize = 2;

/// Bytes the decode table occupies for a code whose longest code is `max_length`.
///
/// One entry per slot of the widest code, each holding a symbol and the width to skip.
#[must_use]
pub const fn table_bytes_for(max_length: u32) -> u64 {
    shift_left(2, max_length)
}

/// A built code: one length per symbol of the declared alphabet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Code {
    alphabet_size: u32,
    /// Zero for a symbol that does not occur. A single-symbol code declares every length zero
    /// and names its symbol, which is what separates the two.
    lengths: Vec<u32>,
    single: Option<u16>,
    max_length: u32,
}

impl Code {
    /// The optimal code of at most `limit` bits per symbol, over the counts a stream produced.
    ///
    /// `counts` is indexed by symbol and spans the declared alphabet. Scratch memory is bounded
    /// by the alphabet and the limit, never by the stream: at most `2 * alphabet_size` weights
    /// and one kind flag per entry, over `limit` levels.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the counts span no alphabet, when no symbol occurs, or
    /// when the limit is too small to hold the symbols that do.
    pub fn build(counts: &[u64], alphabet_size: u32, limit: u32) -> Result<Self, Error> {
        let span = usize::try_from(alphabet_size).map_err(|_| Error::InvalidParameter)?;
        if counts.len() != span
            || span == 0
            || alphabet_size > super::MAX_ALPHABET_SIZE
            || limit > LENGTH_LIMIT
        {
            return Err(Error::InvalidParameter);
        }
        let mut present: Vec<(u64, u16)> = Vec::new();
        for (symbol, &count) in counts.iter().enumerate() {
            if count > 0 {
                let symbol = u16::try_from(symbol).map_err(|_| Error::InvalidParameter)?;
                present.push((count, symbol));
            }
        }
        present.sort_unstable();

        let mut lengths = vec![0u32; span];
        match present.len() {
            0 => return Err(Error::InvalidParameter),
            1 => {
                let (_, symbol) = *present.first().ok_or(Error::InvalidParameter)?;
                return Ok(Self {
                    alphabet_size,
                    lengths,
                    single: Some(symbol),
                    max_length: 0,
                });
            }
            _ => package_merge(&present, limit, &mut lengths)?,
        }

        let max_length = lengths.iter().copied().max().unwrap_or(0);
        if max_length == 0 || max_length > limit {
            return Err(Error::InvalidParameter);
        }
        Ok(Self {
            alphabet_size,
            lengths,
            single: None,
            max_length,
        })
    }

    /// The optimal code whose decode table fits `budget_bytes`.
    ///
    /// The declared limit alone admits a table of 65 536 bytes, which is the whole ceiling a
    /// block may declare over its four tables. An encoder that has already spent part of that
    /// ceiling therefore re-limits the code to what is left. A re-limited code is still the
    /// optimal complete prefix code at its own limit, not a repaired one.
    ///
    /// # Errors
    ///
    /// Returns `LimitExceeded` when no admissible limit holds the symbols that occur inside the
    /// budget, naming the bytes the narrowest code would have required.
    pub fn build_within(
        counts: &[u64],
        alphabet_size: u32,
        budget_bytes: u64,
    ) -> Result<Self, Error> {
        let distinct = counts.iter().filter(|&&count| count > 0).count();
        if distinct <= 1 {
            return Self::build(counts, alphabet_size, LENGTH_LIMIT);
        }
        let narrowest = ceil_log2(u64::try_from(distinct).unwrap_or(u64::MAX)).max(1);
        if narrowest > LENGTH_LIMIT {
            return Err(Error::InvalidParameter);
        }
        let mut limit = LENGTH_LIMIT;
        while table_bytes_for(limit) > budget_bytes {
            if limit <= narrowest {
                return Err(Error::LimitExceeded {
                    declared: table_bytes_for(narrowest),
                    allowed: budget_bytes,
                });
            }
            limit = limit.saturating_sub(1);
        }
        Self::build(counts, alphabet_size, limit)
    }

    /// The alphabet this code spans.
    #[must_use]
    pub const fn alphabet_size(&self) -> u32 {
        self.alphabet_size
    }

    /// The longest code, which is zero for a single-symbol code.
    #[must_use]
    pub const fn max_length(&self) -> u32 {
        self.max_length
    }

    /// The bytes a decoder allocates for this code.
    #[must_use]
    pub const fn table_bytes(&self) -> u64 {
        if self.single.is_some() {
            0
        } else {
            table_bytes_for(self.max_length)
        }
    }

    /// Writes the description a decoder rebuilds this code from.
    ///
    /// The form is a token stream over the lengths: a literal length, a repeat of the previous
    /// length, or a run of zeros. At each position the longest admissible run is taken, a zero
    /// run in preference to a repeat, so the description is a function of the lengths alone.
    pub fn describe(&self, out: &mut BitWriter) {
        let symbol_bits = symbol_field_bits(self.alphabet_size);
        if let Some(symbol) = self.single {
            out.push(1, 1);
            out.push(u64::from(symbol), symbol_bits);
            return;
        }
        out.push(0, 1);
        let mut at = 0usize;
        while at < self.lengths.len() {
            let value = self.lengths.get(at).copied().unwrap_or(0);
            let run = run_length(&self.lengths, at, value);
            if value == 0 && run >= MIN_RUN {
                let take = run.min(run_cap(ZERO_COUNT_BITS));
                out.push(0b11, 2);
                out.push(run_field(take), ZERO_COUNT_BITS);
                at = at.saturating_add(take);
                continue;
            }
            out.push(0, 1);
            out.push(u64::from(value), LITERAL_BITS);
            at = at.saturating_add(1);
            let mut left = run.saturating_sub(1);
            while left >= MIN_RUN {
                let take = left.min(run_cap(REPEAT_COUNT_BITS));
                out.push(0b10, 2);
                out.push(run_field(take), REPEAT_COUNT_BITS);
                at = at.saturating_add(take);
                left = left.saturating_sub(take);
            }
            for _ in 0..left {
                out.push(0, 1);
                out.push(u64::from(value), LITERAL_BITS);
                at = at.saturating_add(1);
            }
        }
    }

    /// The encoder table: the canonical code and its width, per symbol.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the lengths do not assign a canonical code.
    pub fn encoder(&self) -> Result<Encoder, Error> {
        let span = usize::try_from(self.alphabet_size).map_err(|_| Error::InvalidParameter)?;
        let mut table = vec![CodeWord { value: 0, width: 0 }; span];
        if self.single.is_some() {
            return Ok(Encoder { table });
        }
        for assigned in canonical(&self.lengths)? {
            let slot = table
                .get_mut(usize::from(assigned.symbol))
                .ok_or(Error::InvalidParameter)?;
            *slot = CodeWord {
                value: assigned.value,
                width: assigned.length,
            };
        }
        Ok(Encoder { table })
    }
}

/// One symbol's canonical code.
#[derive(Clone, Copy, Debug)]
struct Assigned {
    symbol: u16,
    value: u16,
    length: u32,
}

/// The encoder's per-symbol code word.
#[derive(Clone, Copy, Debug)]
struct CodeWord {
    value: u16,
    width: u32,
}

/// Writes symbols under a built code.
#[derive(Debug)]
pub struct Encoder {
    table: Vec<CodeWord>,
}

impl Encoder {
    /// Writes every symbol of `symbols`.
    ///
    /// A single-symbol code writes no bits, which is what its zero-length code means.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a symbol is outside the alphabet the code was built
    /// over, or occurs under a code that gave it no length.
    pub fn write(&self, symbols: &[u16], out: &mut BitWriter) -> Result<(), Error> {
        for &symbol in symbols {
            let word = self
                .table
                .get(usize::from(symbol))
                .copied()
                .ok_or(Error::InvalidParameter)?;
            if word.width == 0 {
                continue;
            }
            out.push(u64::from(word.value), word.width);
        }
        Ok(())
    }
}

/// A description a decoder has read and does not yet trust.
///
/// Nothing here is proportional to the bytes the stream carried: the lengths span the declared
/// alphabet, which is a property of the symbol model and not of the input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declared {
    alphabet_size: u32,
    lengths: Vec<u32>,
    single: Option<u16>,
}

impl Declared {
    /// Reads a description, refusing the moment it would pass the end of what it was given.
    ///
    /// # Errors
    ///
    /// Returns `TruncatedInput` when the description ends inside a field, and `CorruptData`
    /// when a token repeats a length nothing declared or a run passes the end of the alphabet.
    pub fn parse(reader: &mut BitReader, alphabet_size: u32) -> Result<Self, Error> {
        let span = usize::try_from(alphabet_size).map_err(|_| Error::InvalidParameter)?;
        if span == 0 || alphabet_size > super::MAX_ALPHABET_SIZE {
            return Err(Error::InvalidParameter);
        }
        let symbol_bits = symbol_field_bits(alphabet_size);
        if reader.take(1)? == 1 {
            let symbol = u16::try_from(reader.take(symbol_bits)?)
                .map_err(|_| Error::CorruptData(Corruption::SymbolAlphabet))?;
            return Ok(Self {
                alphabet_size,
                lengths: vec![0; span],
                single: Some(symbol),
            });
        }
        let mut lengths = vec![0u32; span];
        let mut at = 0usize;
        let mut previous: Option<u32> = None;
        while at < span {
            if reader.take(1)? == 0 {
                let value = narrow_field(reader.take(LITERAL_BITS)?);
                set_run(&mut lengths, at, 1, value)?;
                previous = Some(value);
                at = at.saturating_add(1);
                continue;
            }
            if reader.take(1)? == 1 {
                let count = run_count(reader.take(ZERO_COUNT_BITS)?);
                set_run(&mut lengths, at, count, 0)?;
                previous = Some(0);
                at = at.saturating_add(count);
                continue;
            }
            let count = run_count(reader.take(REPEAT_COUNT_BITS)?);
            let value = previous.ok_or(Error::CorruptData(Corruption::DescriptionToken))?;
            set_run(&mut lengths, at, count, value)?;
            at = at.saturating_add(count);
        }
        Ok(Self {
            alphabet_size,
            lengths,
            single: None,
        })
    }

    /// Applies every rule a decoder holds a description to before it allocates for it.
    ///
    /// Each rule is a property of the description alone, so an invalid table is refused without
    /// decoding a symbol under it.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when a single-symbol code names a symbol outside its alphabet,
    /// when the length count contradicts the alphabet, when a length is above the limit, or
    /// when the lengths are not a complete prefix code.
    pub fn validate(self) -> Result<Admitted, Error> {
        let span = usize::try_from(self.alphabet_size).map_err(|_| Error::InvalidParameter)?;
        if let Some(symbol) = self.single {
            if u32::from(symbol) >= self.alphabet_size {
                return Err(Error::CorruptData(Corruption::SymbolAlphabet));
            }
            return Ok(Admitted {
                code: Code {
                    alphabet_size: self.alphabet_size,
                    lengths: self.lengths,
                    single: Some(symbol),
                    max_length: 0,
                },
            });
        }
        if self.lengths.len() != span {
            return Err(Error::CorruptData(Corruption::LengthCount));
        }

        let mut kraft: u128 = 0;
        let mut max_length = 0u32;
        let mut distinct = 0u32;
        for &length in &self.lengths {
            if length > LENGTH_LIMIT {
                return Err(Error::CorruptData(Corruption::CodeLength));
            }
            if length > 0 {
                distinct = distinct.saturating_add(1);
                max_length = max_length.max(length);
                kraft = kraft.saturating_add(shift_right_wide(KRAFT_UNIT, length));
            }
        }
        if distinct < 2 {
            return Err(Error::CorruptData(Corruption::LengthCount));
        }
        if kraft != KRAFT_UNIT {
            return Err(Error::CorruptData(Corruption::KraftSum));
        }
        Ok(Admitted {
            code: Code {
                alphabet_size: self.alphabet_size,
                lengths: self.lengths,
                single: None,
                max_length,
            },
        })
    }
}

/// A description validation has admitted, and the requirement it declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Admitted {
    code: Code,
}

impl Admitted {
    /// The bytes the decode table will occupy, computed from the description alone.
    #[must_use]
    pub const fn table_bytes(&self) -> u64 {
        self.code.table_bytes()
    }

    /// The code this description named.
    #[must_use]
    pub const fn code(&self) -> &Code {
        &self.code
    }

    /// Allocates the decode table.
    ///
    /// This is the first allocation the description sizes, and it is reachable only from an
    /// admitted description.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the admitted lengths do not assign a canonical code.
    pub fn build(&self) -> Result<DecodeTable, Error> {
        if let Some(symbol) = self.code.single {
            return Ok(DecodeTable {
                entries: Vec::new(),
                max_length: 0,
                single: Some(symbol),
            });
        }
        let width = self.code.max_length;
        let size = usize::try_from(shift_left(1, width)).map_err(|_| Error::InvalidParameter)?;
        let mut entries = vec![0u16; size];
        for assigned in canonical(&self.code.lengths)? {
            let span = shift_left(1, width.saturating_sub(assigned.length));
            let base = shift_left(
                u64::from(assigned.value),
                width.saturating_sub(assigned.length),
            );
            let base = usize::try_from(base).map_err(|_| Error::InvalidParameter)?;
            let span = usize::try_from(span).map_err(|_| Error::InvalidParameter)?;
            let end = base.checked_add(span).ok_or(Error::InvalidParameter)?;
            let slots = entries
                .get_mut(base..end)
                .ok_or(Error::CorruptData(Corruption::KraftSum))?;
            slots.fill(entry(assigned.symbol, assigned.length));
        }
        Ok(DecodeTable {
            entries,
            max_length: width,
            single: None,
        })
    }
}

/// The single-level decode table: one entry per slot of the widest code.
///
/// An entry holds the symbol and the width to skip, so one lookup and one stream read decode
/// one symbol.
#[derive(Clone, Debug)]
pub struct DecodeTable {
    entries: Vec<u16>,
    max_length: u32,
    single: Option<u16>,
}

impl DecodeTable {
    /// The bytes this table occupies.
    #[must_use]
    pub fn allocated_bytes(&self) -> u64 {
        u64::try_from(self.entries.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(2)
    }

    /// Decodes one symbol per slot of `out`.
    ///
    /// The reader peeks the widest code, which reads zeros past the end, so the decode is
    /// checked against the bit count the stream declared when it ends.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the bits name no code of any declared length, or when the
    /// decode consumed more bits than the stream carried.
    pub fn decode(&self, reader: &mut BitReader, out: &mut [u16]) -> Result<(), Error> {
        if let Some(symbol) = self.single {
            out.fill(symbol);
            return Ok(());
        }
        for slot in out.iter_mut() {
            let at = usize::try_from(reader.peek(self.max_length))
                .map_err(|_| Error::CorruptData(Corruption::CodedStream))?;
            let found = self
                .entries
                .get(at)
                .copied()
                .ok_or(Error::CorruptData(Corruption::CodedStream))?;
            let width = entry_width(found);
            if width == 0 {
                return Err(Error::CorruptData(Corruption::CodedStream));
            }
            *slot = entry_symbol(found);
            reader.skip(width);
        }
        reader.finish()
    }
}

/// The canonical assignment: symbols ordered by length and then by symbol, codes assigned in
/// increasing order with a shift at every length change.
fn canonical(lengths: &[u32]) -> Result<Vec<Assigned>, Error> {
    let mut order: Vec<(u32, u16)> = Vec::new();
    for (symbol, &length) in lengths.iter().enumerate() {
        if length > 0 {
            let symbol = u16::try_from(symbol).map_err(|_| Error::InvalidParameter)?;
            order.push((length, symbol));
        }
    }
    order.sort_unstable();

    let mut out = Vec::with_capacity(order.len());
    let mut code: u64 = 0;
    let mut previous = order.first().map_or(0, |&(length, _)| length);
    for &(length, symbol) in &order {
        if length >= previous {
            code = shift_left(code, length.saturating_sub(previous));
        } else {
            code = shift_right(code, previous.saturating_sub(length));
        }
        previous = length;
        if code >= shift_left(1, length) {
            return Err(Error::CorruptData(Corruption::KraftSum));
        }
        let value = u16::try_from(code).map_err(|_| Error::CorruptData(Corruption::KraftSum))?;
        out.push(Assigned {
            symbol,
            value,
            length,
        });
        code = code.checked_add(1).ok_or(Error::InvalidParameter)?;
    }
    Ok(out)
}

/// The optimal length-limited code lengths, by package merge.
///
/// Package merge selects the `2n - 2` cheapest coins from a set that holds, for every symbol and
/// every denomination down to `2^-limit`, one coin of that denomination and that symbol's
/// weight. The number of times a symbol appears in the selection is its code length.
///
/// The levels are kept as weights and one kind flag per entry rather than as a tree, so the
/// scratch is `limit` levels of at most `2n` entries and never a tree of packages.
fn package_merge(present: &[(u64, u16)], limit: u32, lengths: &mut [u32]) -> Result<(), Error> {
    let count = present.len();
    let depth = ceil_log2(u64::try_from(count).unwrap_or(u64::MAX));
    if count < 2 || limit == 0 || limit < depth {
        return Err(Error::InvalidParameter);
    }
    let capacity = count.saturating_mul(2);

    // Level zero is the leaves. Level k is the leaves merged with the packages of level k - 1.
    // A tie goes to the leaf: every tie-break selects a set of the same total weight, so the
    // code cost is identical and declaring one keeps the result a function of the counts.
    let mut kinds: Vec<Vec<bool>> = Vec::with_capacity(usize::try_from(limit).unwrap_or(0));
    let mut weights: Vec<u64> = present.iter().map(|&(weight, _)| weight).collect();
    kinds.push(vec![true; count]);

    let mut level = 1u32;
    while level < limit {
        let mut merged_weights: Vec<u64> = Vec::with_capacity(capacity);
        let mut merged_kinds: Vec<bool> = Vec::with_capacity(capacity);
        let mut leaf = 0usize;
        let mut pair = 0usize;
        loop {
            let package = match (weights.get(pair), weights.get(pair.saturating_add(1))) {
                (Some(&low), Some(&high)) => {
                    Some(low.checked_add(high).ok_or(Error::InvalidParameter)?)
                }
                _ => None,
            };
            let next = present.get(leaf).map(|&(weight, _)| weight);
            match (next, package) {
                (Some(weight), Some(cost)) if weight > cost => {
                    merged_weights.push(cost);
                    merged_kinds.push(false);
                    pair = pair.saturating_add(2);
                }
                (Some(weight), _) => {
                    merged_weights.push(weight);
                    merged_kinds.push(true);
                    leaf = leaf.saturating_add(1);
                }
                (None, Some(cost)) => {
                    merged_weights.push(cost);
                    merged_kinds.push(false);
                    pair = pair.saturating_add(2);
                }
                (None, None) => break,
            }
        }
        weights = merged_weights;
        kinds.push(merged_kinds);
        level = level.saturating_add(1);
    }

    // A package at one level is two entries of the level below, and both the packages and the
    // leaves keep their order through the merge, so the selection at each level is a prefix.
    let mut selected = capacity.saturating_sub(2);
    for level in kinds.iter().rev() {
        if selected > level.len() {
            return Err(Error::InvalidParameter);
        }
        let used = level
            .iter()
            .take(selected)
            .filter(|&&is_leaf| is_leaf)
            .count();
        for &(_, symbol) in present.iter().take(used) {
            let slot = lengths
                .get_mut(usize::from(symbol))
                .ok_or(Error::InvalidParameter)?;
            *slot = slot.saturating_add(1);
        }
        selected = selected.saturating_sub(used).saturating_mul(2);
    }

    for &(_, symbol) in present {
        let length = lengths
            .get(usize::from(symbol))
            .copied()
            .ok_or(Error::InvalidParameter)?;
        if length == 0 || length > limit {
            return Err(Error::InvalidParameter);
        }
    }
    Ok(())
}

/// The run of `value` that starts at `at`.
fn run_length(lengths: &[u32], at: usize, value: u32) -> usize {
    let mut run = 0usize;
    while lengths.get(at.saturating_add(run)).copied() == Some(value) {
        run = run.saturating_add(1);
    }
    run
}

/// The longest run a count field of `width` bits can carry.
fn run_cap(width: u32) -> usize {
    usize::try_from(super::bits::mask(width))
        .unwrap_or(usize::MAX)
        .saturating_add(MIN_RUN)
}

/// The field value a run of `take` occupies.
fn run_field(take: usize) -> u64 {
    u64::try_from(take.saturating_sub(MIN_RUN)).unwrap_or(0)
}

/// The run a count field declares.
fn run_count(field: u64) -> usize {
    usize::try_from(field)
        .unwrap_or(usize::MAX)
        .saturating_add(MIN_RUN)
}

/// Writes `value` into `count` slots from `at`, refusing a run that passes the alphabet.
fn set_run(lengths: &mut [u32], at: usize, count: usize, value: u32) -> Result<(), Error> {
    let end = at
        .checked_add(count)
        .ok_or(Error::CorruptData(Corruption::DescriptionToken))?;
    lengths
        .get_mut(at..end)
        .ok_or(Error::CorruptData(Corruption::DescriptionToken))?
        .fill(value);
    Ok(())
}

/// The Kraft sum of a complete prefix code, in units of `2^-64`.
///
/// Below it the code has holes a hostile stream reaches; above it two symbols share a prefix.
const KRAFT_UNIT: u128 = 1u128 << 64;

/// Bits the single-symbol flag spends on its symbol.
const fn symbol_field_bits(alphabet_size: u32) -> u32 {
    if alphabet_size <= 1 {
        1
    } else {
        32u32.saturating_sub(alphabet_size.saturating_sub(1).leading_zeros())
    }
}

// The literal field is four bits wide, so the narrowing is exact.
#[allow(clippy::cast_possible_truncation)]
const fn narrow_field(value: u64) -> u32 {
    (value & 0xF) as u32
}

// The symbol is below 256 and the width is below 16, so the entry keeps both exactly.
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
const fn entry(symbol: u16, width: u32) -> u16 {
    (symbol << 8) | ((width as u16) & 0xFF)
}

// The entry packs the symbol above the width, so neither shift loses a bit.
#[allow(clippy::arithmetic_side_effects)]
const fn entry_symbol(value: u16) -> u16 {
    value >> 8
}

fn entry_width(value: u16) -> u32 {
    u32::from(value & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::{Code, Declared, LENGTH_LIMIT, LITERAL_BITS, canonical, table_bytes_for};
    use crate::entropy::bits::{BitReader, BitWriter};
    use crate::entropy::shift_right_wide;
    use crate::format::{Corruption, Error};

    fn declared(alphabet_size: u32, lengths: &[u32], single: Option<u16>) -> Declared {
        Declared {
            alphabet_size,
            lengths: lengths.to_vec(),
            single,
        }
    }

    /// The Kraft sum of a set of lengths, in units of `2^-64`.
    fn kraft(lengths: &[u32]) -> u128 {
        lengths
            .iter()
            .filter(|&&length| length > 0)
            .map(|&length| shift_right_wide(super::KRAFT_UNIT, length))
            .sum()
    }

    #[test]
    fn a_built_code_is_complete_and_holds_its_limit() -> Result<(), Error> {
        for limit in 4..=LENGTH_LIMIT {
            for skew in 1..=6u32 {
                let counts: Vec<u64> = (0..16u32)
                    .map(|symbol| u64::from(1u32 << (skew * (16 - symbol) % 13)))
                    .collect();
                let code = Code::build(&counts, 16, limit)?;
                assert!(
                    code.max_length() <= limit,
                    "the limit holds by construction"
                );
                assert_eq!(kraft(&code.lengths), super::KRAFT_UNIT);
                assert_eq!(code.table_bytes(), table_bytes_for(code.max_length()));
                let _ = canonical(&code.lengths)?;
            }
        }
        Ok(())
    }

    #[test]
    fn a_code_length_above_the_limit_is_refused_before_a_table_is_built() {
        // The literal token is four bits wide and the limit is fifteen, so this description is
        // not expressible on the wire. The rule holds at the validation boundary, which every
        // description crosses whatever structure it arrived from.
        let mut lengths = vec![1u32; 2];
        lengths.push(LENGTH_LIMIT + 1);
        assert_eq!(
            declared(3, &lengths, None).validate(),
            Err(Error::CorruptData(Corruption::CodeLength))
        );
    }

    #[test]
    fn a_length_count_that_contradicts_its_alphabet_is_refused() {
        assert_eq!(
            declared(8, &[1, 1], None).validate(),
            Err(Error::CorruptData(Corruption::LengthCount)),
            "fewer lengths than the alphabet declares"
        );
        assert_eq!(
            declared(2, &[1, 1, 2, 2], None).validate(),
            Err(Error::CorruptData(Corruption::LengthCount)),
            "more lengths than the alphabet declares"
        );
        assert_eq!(
            declared(4, &[3, 0, 0, 0], None).validate(),
            Err(Error::CorruptData(Corruption::LengthCount)),
            "a multi-symbol code that declares one symbol"
        );
    }

    #[test]
    fn lengths_that_are_not_a_complete_prefix_code_are_refused() {
        assert_eq!(
            declared(4, &[1, 2, 0, 0], None).validate(),
            Err(Error::CorruptData(Corruption::KraftSum)),
            "an incomplete code leaves holes a hostile stream reaches"
        );
        assert_eq!(
            declared(4, &[1, 1, 1, 0], None).validate(),
            Err(Error::CorruptData(Corruption::KraftSum)),
            "an over-subscribed code gives two symbols one prefix"
        );
    }

    #[test]
    fn a_single_symbol_code_outside_its_alphabet_is_refused() -> Result<(), Error> {
        // Wire-reachable: the symbol field is as wide as the alphabet needs, so it carries
        // values the alphabet does not hold.
        let mut writer = BitWriter::new();
        writer.push(1, 1);
        writer.push(63, 6);
        let bits = writer.finish();
        let parsed = Declared::parse(&mut BitReader::over(&bits), 60)?;
        assert_eq!(
            parsed.validate(),
            Err(Error::CorruptData(Corruption::SymbolAlphabet))
        );
        Ok(())
    }

    #[test]
    fn a_description_token_that_no_position_admits_is_refused() {
        // A repeat with nothing before it to repeat.
        let mut writer = BitWriter::new();
        writer.push(0, 1);
        writer.push(0b10, 2);
        writer.push(0, 7);
        let bits = writer.finish();
        assert_eq!(
            Declared::parse(&mut BitReader::over(&bits), 8),
            Err(Error::CorruptData(Corruption::DescriptionToken))
        );

        // A zero run that passes the end of the alphabet.
        let mut writer = BitWriter::new();
        writer.push(0, 1);
        writer.push(0b11, 2);
        writer.push(255, 8);
        let bits = writer.finish();
        assert_eq!(
            Declared::parse(&mut BitReader::over(&bits), 8),
            Err(Error::CorruptData(Corruption::DescriptionToken))
        );

        // A repeat that passes the end of the alphabet.
        let mut writer = BitWriter::new();
        writer.push(0, 1);
        writer.push(0, 1);
        writer.push(1, LITERAL_BITS);
        writer.push(0b10, 2);
        writer.push(127, 7);
        let bits = writer.finish();
        assert_eq!(
            Declared::parse(&mut BitReader::over(&bits), 8),
            Err(Error::CorruptData(Corruption::DescriptionToken))
        );
    }
}

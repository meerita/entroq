//! Owns the sequence representation: the four alphabets a block's symbols are drawn from, the
//! decomposition that splits a coded value into a symbol and a raw suffix, and the sequences
//! the two describe.
//!
//! This module does not own entropy coding, the block that carries the streams, the parse that
//! produced the sequences, or the window history a match copies from.
//!
//! One alphabet per symbol class, and one decomposition rule over the three that carry a
//! magnitude:
//!
//! ```text
//! alphabet          coded value      domain            suffix   coder
//! literal byte      the byte         0 to 255          none     Huffman
//! literal run       run + 1          0 to 65 536       yes      rANS, 1 state
//! match length      length - 3       4 to 256          yes      rANS, 2 states
//! match distance    distance + 1     1 to 65 536       yes      rANS, 4 states
//! ```
//!
//! A run of zero is a real and common symbol, so the run stream codes `run + 1` and never a
//! zero. A length is coded against the shortest match the format can express, because no
//! shorter match exists and codes spent on lengths nothing can emit would price the alphabet
//! wrongly. A distance is shifted by one so that coded value 1 is free to name the offset slot.
//!
//! # The decomposition
//!
//! A bucket code over the octaves of the coded value. Symbol `i` owns the interval
//! `[lo_i, lo_i + 2^w_i)`, the symbol names the interval, and a raw suffix of `w_i` bits names
//! the position inside it, so `value = lo_i + suffix` and the pair is a bijection onto the
//! coded value by construction.
//!
//! ```text
//! an octave at or below the mantissa width gives each of its values its own symbol, and a
//!   suffix of no bits
//! an octave above it is cut into 2^m symbols, each owning 2^(b-m) consecutive values
//! ```
//!
//! The boundary set is a format constant, and it is arithmetic rather than a table. The block
//! length it was chosen at is 65 536 input bytes.
//!
//! # The two symbols that name no magnitude
//!
//! The match-length alphabet carries one symbol beyond its buckets, as its last symbol. It
//! marks a block that ends in a literal run with no match, and it is carried there and nowhere
//! else.
//!
//! The match-distance alphabet carries one symbol below its buckets, as its first. It names the
//! distance the offset slot holds, which is why every distance is coded one above itself.

pub mod cache;
pub mod streams;

use crate::entropy::{bits::mask, shift_left, shift_right};
use crate::format::{Corruption, Error};

/// The history a match may reach back into, in bytes.
///
/// The window of resource class 0, which is the one class the encoder declares.
pub const WINDOW: u32 = 65_536;

/// The shortest match the representation can express.
pub const MIN_MATCH: u32 = 4;

/// The longest match the representation can express.
pub const MAX_MATCH_LENGTH: u32 = 256;

/// The longest literal run the representation can express.
pub const MAX_LITERAL_RUN: u32 = 65_536;

/// The mantissa bits the decomposition promotes out of the raw suffix and into the symbol.
pub const MANTISSA_BITS: u32 = 2;

/// The symbols one octave above the mantissa width is cut into.
const OCTAVE_SYMBOLS: u32 = 1 << MANTISSA_BITS;

/// The symbols the octaves at or below the mantissa width hold together.
///
/// Those octaves hold `1 + 2 + ... + 2^m` values between them and give each value its own
/// symbol, so the first octave the decomposition actually cuts starts above this many.
const DEGENERATE_SYMBOLS: u32 = (1 << (MANTISSA_BITS + 1)) - 1;

/// A coded value as the decomposition writes it: one symbol and its raw suffix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Split {
    /// The symbol whose interval holds the coded value.
    pub symbol: u16,
    /// The bits the suffix occupies, which the symbol alone determines.
    pub width: u32,
    /// The position of the coded value inside the symbol's interval.
    pub suffix: u64,
}

/// The interval one symbol owns, in the form the decoder reads it.
///
/// Both fields are the decomposition's own and neither is a second definition of it: `width`
/// is what `Alphabet::suffix_width` computes for the symbol, and `low` is the coded value
/// `Alphabet::reconstruct` returns for it at a suffix of zero. A decoder reads the pair from a
/// table instead of computing it per symbol, and the tests hold every entry to those two
/// methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Bucket {
    /// The bits the symbol's raw suffix occupies.
    pub(crate) width: u32,
    /// The coded value the symbol names when its suffix is zero.
    pub(crate) low: u32,
}

/// One of the four symbol classes a block codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Alphabet {
    /// The bytes of the literal runs.
    LiteralByte,
    /// The length of each literal run.
    LiteralRun,
    /// The length of each match, and the symbol that ends the block.
    MatchLength,
    /// The distance of each match, and the symbol that names the offset slot.
    MatchDistance,
}

impl Alphabet {
    /// The four classes in the order a block carries their streams.
    pub const ALL: [Self; 4] = [
        Self::LiteralByte,
        Self::LiteralRun,
        Self::MatchLength,
        Self::MatchDistance,
    ];

    /// The symbols the alphabet holds, every reserved symbol included.
    #[must_use]
    pub const fn size(self) -> u32 {
        if matches!(self, Self::LiteralByte) {
            return 256;
        }
        let lead = self.lead_symbols();
        let buckets = symbols_for(self.coded_max().saturating_sub(lead as u64));
        let terminal = if self.carries_terminal() { 1 } else { 0 };
        lead.saturating_add(buckets).saturating_add(terminal)
    }

    /// The widest span any one stream's histogram allocates over.
    ///
    /// The per-block histograms size one vector per alphabet, so the transient
    /// counts scale with the sum of the four spans and no single one passes this.
    #[must_use]
    pub(crate) const fn max_size() -> u32 {
        let run = Self::LiteralRun.size();
        let length = Self::MatchLength.size();
        let distance = Self::MatchDistance.size();
        let widest = if run > length { run } else { length };
        let widest = if widest > distance { widest } else { distance };
        if Self::LiteralByte.size() > widest {
            Self::LiteralByte.size()
        } else {
            widest
        }
    }

    /// The spans of the four streams added together.
    ///
    /// Four histograms, one per alphabet, each holding its span in 64-bit counts.
    #[must_use]
    pub(crate) const fn total_size() -> u32 {
        Self::LiteralByte
            .size()
            .saturating_add(Self::LiteralRun.size())
            .saturating_add(Self::MatchLength.size())
            .saturating_add(Self::MatchDistance.size())
    }

    /// The symbol that ends a block after a literal run no match follows.
    ///
    /// The last symbol of the match-length alphabet, and of no other.
    #[must_use]
    pub const fn terminal(self) -> Option<u16> {
        if self.carries_terminal() {
            narrow_symbol(self.size().saturating_sub(1))
        } else {
            None
        }
    }

    /// Whether the alphabet reserves its last symbol for the end of a block.
    #[must_use]
    pub const fn carries_terminal(self) -> bool {
        matches!(self, Self::MatchLength)
    }

    /// The symbols the alphabet reserves below its buckets.
    ///
    /// One on the match-distance alphabet, for the offset slot. None anywhere else.
    #[must_use]
    pub const fn lead_symbols(self) -> u32 {
        match self {
            Self::MatchDistance => 1,
            _ => 0,
        }
    }

    /// The smallest raw value the domain admits.
    #[must_use]
    pub fn min_raw(self) -> u64 {
        match self {
            Self::LiteralByte | Self::LiteralRun => 0,
            Self::MatchLength => u64::from(MIN_MATCH),
            Self::MatchDistance => 1,
        }
    }

    /// The largest raw value the domain admits.
    #[must_use]
    pub fn max_raw(self) -> u64 {
        match self {
            Self::LiteralByte => 255,
            Self::LiteralRun => u64::from(MAX_LITERAL_RUN),
            Self::MatchLength => u64::from(MAX_MATCH_LENGTH),
            Self::MatchDistance => u64::from(WINDOW),
        }
    }

    /// The smallest coded value the domain reaches.
    #[must_use]
    pub const fn coded_min(self) -> u64 {
        match self {
            Self::LiteralByte => 0,
            _ => 1,
        }
    }

    /// The largest coded value the domain reaches.
    #[must_use]
    pub const fn coded_max(self) -> u64 {
        match self {
            Self::LiteralByte => 255,
            Self::LiteralRun => (MAX_LITERAL_RUN as u64).saturating_add(1),
            Self::MatchLength => {
                (MAX_MATCH_LENGTH as u64).saturating_sub((MIN_MATCH as u64).saturating_sub(1))
            }
            Self::MatchDistance => (WINDOW as u64).saturating_add(1),
        }
    }

    /// The coded value a raw value takes, or `None` when the domain does not hold it.
    #[must_use]
    pub fn coded_value(self, raw: u64) -> Option<u64> {
        if raw < self.min_raw() || raw > self.max_raw() {
            return None;
        }
        Some(match self {
            Self::LiteralByte => raw,
            Self::LiteralRun | Self::MatchDistance => raw.saturating_add(1),
            Self::MatchLength => raw.saturating_sub(u64::from(MIN_MATCH).saturating_sub(1)),
        })
    }

    /// The raw value a coded value names, or `None` when the coded value names no raw value.
    ///
    /// A coded value the alphabet reserves below its buckets names no raw value at all. On the
    /// match-distance alphabet that is the repeat code, whose distance the offset slot holds
    /// and this alphabet does not.
    #[must_use]
    pub fn raw_value(self, coded: u64) -> Option<u64> {
        let lead = u64::from(self.lead_symbols());
        if coded < self.coded_min() || coded > self.coded_max() || (lead > 0 && coded <= lead) {
            return None;
        }
        Some(match self {
            Self::LiteralByte => coded,
            Self::LiteralRun | Self::MatchDistance => coded.saturating_sub(1),
            Self::MatchLength => coded.saturating_add(u64::from(MIN_MATCH).saturating_sub(1)),
        })
    }

    /// The symbol and raw suffix that name a coded value.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the coded value is outside the domain the alphabet
    /// declares, which is a value no parse this format admits can produce.
    pub fn split(self, coded: u64) -> Result<Split, Error> {
        if coded < self.coded_min() || coded > self.coded_max() {
            return Err(Error::InvalidParameter);
        }
        let flat = matches!(self, Self::LiteralByte) || coded <= u64::from(self.lead_symbols());
        if flat {
            let index = if matches!(self, Self::LiteralByte) {
                coded
            } else {
                coded.saturating_sub(1)
            };
            let symbol = narrow_symbol_wide(index).ok_or(Error::InvalidParameter)?;
            return Ok(Split {
                symbol,
                width: 0,
                suffix: 0,
            });
        }
        let lead = self.lead_symbols();
        let (index, width, suffix) = bucket_split(coded.saturating_sub(u64::from(lead)));
        let symbol = narrow_symbol(index.saturating_add(lead)).ok_or(Error::InvalidParameter)?;
        Ok(Split {
            symbol,
            width,
            suffix,
        })
    }

    /// The bits the raw suffix of a symbol occupies.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the symbol is at or above the alphabet's size, or when it is
    /// the terminal, which names no coded value and carries no suffix.
    pub fn suffix_width(self, symbol: u16) -> Result<u32, Error> {
        let index = u32::from(symbol);
        if index >= self.size() {
            return Err(Error::CorruptData(Corruption::SequenceSymbol));
        }
        if self.terminal() == Some(symbol) {
            return Err(Error::CorruptData(Corruption::TerminalSymbol));
        }
        if matches!(self, Self::LiteralByte) || index < self.lead_symbols() {
            return Ok(0);
        }
        Ok(bucket_width(index.saturating_sub(self.lead_symbols())))
    }

    /// The coded value a symbol and its raw suffix name.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the symbol is not one the alphabet codes a value with, or
    /// when the suffix takes the value past the domain the alphabet declares. The last symbol
    /// of a bucketed alphabet owns an interval the domain can end inside, so a suffix above
    /// that symbol's own share of its interval is refused here and at no later step.
    pub fn reconstruct(self, symbol: u16, suffix: u64) -> Result<u64, Error> {
        let width = self.suffix_width(symbol)?;
        if suffix > mask(width) {
            return Err(Error::CorruptData(Corruption::SequenceSuffix));
        }
        let index = u32::from(symbol);
        let lead = self.lead_symbols();
        let coded = if matches!(self, Self::LiteralByte) {
            u64::from(index)
        } else if index < lead {
            u64::from(index).saturating_add(1)
        } else {
            bucket_low(index.saturating_sub(lead))
                .saturating_add(suffix)
                .saturating_add(u64::from(lead))
        };
        if coded < self.coded_min() || coded > self.coded_max() {
            return Err(Error::CorruptData(Corruption::SequenceSuffix));
        }
        Ok(coded)
    }

    /// The symbols the alphabet names a coded value with.
    ///
    /// Its size, less the terminal it reserves, which names none.
    pub(crate) const fn value_symbols(self) -> usize {
        let terminal = if self.carries_terminal() { 1 } else { 0 };
        self.size().saturating_sub(terminal) as usize
    }

    /// The interval one symbol owns.
    ///
    /// Total over every symbol of every alphabet, and the same arithmetic `suffix_width` and
    /// `reconstruct` run: the lead symbols the alphabet reserves name one coded value each and
    /// carry no suffix, and every other symbol names the bucket its index gives it.
    const fn bucket(self, index: u32) -> Bucket {
        if matches!(self, Self::LiteralByte) {
            return Bucket {
                width: 0,
                low: index,
            };
        }
        let lead = self.lead_symbols();
        if index < lead {
            return Bucket {
                width: 0,
                low: index.saturating_add(1),
            };
        }
        let bucket = index.saturating_sub(lead);
        Bucket {
            width: bucket_width(bucket),
            low: narrow_index(bucket_low(bucket).saturating_add(lead as u64)),
        }
    }

    /// The interval of every symbol the alphabet names a value with, in symbol order.
    const fn buckets<const N: usize>(self) -> [Bucket; N] {
        let mut table = [Bucket { width: 0, low: 0 }; N];
        let mut index = 0u32;
        // The lint set refuses an indexed write and a const context has no iterator, so the
        // table is filled one slot at a time through the slice it is.
        let mut rest: &mut [Bucket] = table.as_mut_slice();
        while let [slot, tail @ ..] = rest {
            *slot = self.bucket(index);
            index = index.saturating_add(1);
            rest = tail;
        }
        table
    }
}

/// The interval of every literal-run symbol, in symbol order.
pub(crate) const LITERAL_RUN_BUCKETS: [Bucket; Alphabet::LiteralRun.value_symbols()] =
    Alphabet::LiteralRun.buckets();

/// The interval of every match-length symbol that names a length.
///
/// The terminal is the alphabet's last symbol and names none, so the table stops below it.
pub(crate) const MATCH_LENGTH_BUCKETS: [Bucket; Alphabet::MatchLength.value_symbols()] =
    Alphabet::MatchLength.buckets();

/// The interval of every match-distance symbol, the repeat code's included.
pub(crate) const MATCH_DISTANCE_BUCKETS: [Bucket; Alphabet::MatchDistance.value_symbols()] =
    Alphabet::MatchDistance.buckets();

/// One match: how far back it reaches and how many bytes it copies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Match {
    /// The bytes the match copies.
    pub length: u32,
    /// The bytes back from the current position the copy starts at.
    pub distance: u32,
}

/// One sequence: a literal run, and the match that follows it or the end of the block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Step {
    /// The literal bytes the step emits before its match.
    pub run: u32,
    /// The match that follows the run, or the end of the block.
    pub matched: Option<Match>,
}

/// The sequences of one block: its literal bytes, and the steps that place them.
///
/// The steps partition the literal bytes: the runs sum to exactly the bytes the vector holds.
/// A step with no match is the last step and nothing else, because a block ends either on a
/// match or on the literal run the terminal symbol closes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Sequences {
    literals: Vec<u8>,
    steps: Vec<Step>,
}

impl Sequences {
    /// The sequences a parse emitted, checked against the domains the alphabets declare.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a run, a length, or a distance is outside its domain,
    /// when the runs do not sum to the literal bytes the vector holds, or when a step other
    /// than the last carries no match.
    pub fn new(literals: Vec<u8>, steps: Vec<Step>) -> Result<Self, Error> {
        let mut covered = 0u64;
        for (index, step) in steps.iter().enumerate() {
            if Alphabet::LiteralRun
                .coded_value(u64::from(step.run))
                .is_none()
            {
                return Err(Error::InvalidParameter);
            }
            covered = covered.saturating_add(u64::from(step.run));
            match step.matched {
                Some(matched) => {
                    if Alphabet::MatchLength
                        .coded_value(u64::from(matched.length))
                        .is_none()
                        || Alphabet::MatchDistance
                            .coded_value(u64::from(matched.distance))
                            .is_none()
                    {
                        return Err(Error::InvalidParameter);
                    }
                }
                None => {
                    if index.saturating_add(1) != steps.len() {
                        return Err(Error::InvalidParameter);
                    }
                }
            }
        }
        if covered != u64::try_from(literals.len()).unwrap_or(u64::MAX) {
            return Err(Error::InvalidParameter);
        }
        Ok(Self { literals, steps })
    }

    /// The sequences a differential test hands both expansions, unchecked.
    ///
    /// The mutation matrix carries values no valid stream produces, so the domains `new`
    /// enforces are deliberately bypassed. Test-only: production sequences always come
    /// through `new`, `push`, or the stream decoder.
    #[cfg(test)]
    pub(crate) const fn new_unchecked(literals: Vec<u8>, steps: Vec<Step>) -> Self {
        Self { literals, steps }
    }

    /// Empty sequences whose storage is sized once and never grows.
    ///
    /// A producer that emits one block after another builds its storage here and reuses it,
    /// so the steady state costs no allocation per block. The two figures are the block's
    /// worst cases: every byte a literal, and one step per shortest match.
    #[must_use]
    pub fn with_capacity(literal_bytes: usize, steps: usize) -> Self {
        Self {
            literals: Vec::with_capacity(literal_bytes),
            steps: Vec::with_capacity(steps),
        }
    }

    /// Empties the sequences and keeps the storage they hold.
    pub fn clear(&mut self) {
        self.literals.clear();
        self.steps.clear();
    }

    /// Appends one step: the literal bytes its run places, and the match that follows them.
    ///
    /// Each step is checked as it arrives, so the invariants `new` checks over a finished
    /// vector hold over a growing one too: the runs sum to the literal bytes by construction,
    /// every value is inside its domain, and a step that carries no match is the last step.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the run, the length or the distance is outside its
    /// domain, or when a step follows one that carried no match.
    pub fn push(&mut self, literals: &[u8], matched: Option<Match>) -> Result<(), Error> {
        if self.steps.last().is_some_and(|last| last.matched.is_none()) {
            return Err(Error::InvalidParameter);
        }
        let run = u32::try_from(literals.len()).map_err(|_| Error::InvalidParameter)?;
        if Alphabet::LiteralRun.coded_value(u64::from(run)).is_none() {
            return Err(Error::InvalidParameter);
        }
        if let Some(matched) = matched
            && (Alphabet::MatchLength
                .coded_value(u64::from(matched.length))
                .is_none()
                || Alphabet::MatchDistance
                    .coded_value(u64::from(matched.distance))
                    .is_none())
        {
            return Err(Error::InvalidParameter);
        }
        self.literals.extend_from_slice(literals);
        self.steps.push(Step { run, matched });
        Ok(())
    }

    /// The literal bytes the steps place, in the order the block emits them.
    #[must_use]
    pub fn literals(&self) -> &[u8] {
        &self.literals
    }

    /// The steps, in the order the parser emitted them.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The bytes the sequences decode to.
    #[must_use]
    pub fn decoded_len(&self) -> u64 {
        let mut total = 0u64;
        for step in &self.steps {
            total = total.saturating_add(u64::from(step.run));
            if let Some(matched) = step.matched {
                total = total.saturating_add(u64::from(matched.length));
            }
        }
        total
    }
}

/// The symbols a bucketed domain reaching `coded_max` needs.
const fn symbols_for(coded_max: u64) -> u32 {
    if coded_max == 0 {
        return 0;
    }
    let (index, _, _) = bucket_split(coded_max);
    index.saturating_add(1)
}

/// The bucket index, the suffix width, and the suffix of a value at or above one.
///
/// Total, so that no caller carries a precondition: a value of zero takes the first bucket, and
/// the domain checks at the boundaries above are what keep it unreachable.
const fn bucket_split(value: u64) -> (u32, u32, u64) {
    if value <= 1 {
        return (0, 0, 0);
    }
    let octave = 63u32.saturating_sub(value.leading_zeros());
    if octave <= MANTISSA_BITS {
        return (narrow_index(value.saturating_sub(1)), 0, 0);
    }
    let width = octave.saturating_sub(MANTISSA_BITS);
    let inside = value.saturating_sub(shift_left(1, octave));
    let index = DEGENERATE_SYMBOLS
        .saturating_add(OCTAVE_SYMBOLS.saturating_mul(width.saturating_sub(1)))
        .saturating_add(narrow_index(shift_right(inside, width)));
    (index, width, inside & mask(width))
}

/// The suffix width of a bucket index.
const fn bucket_width(index: u32) -> u32 {
    if index < DEGENERATE_SYMBOLS {
        return 0;
    }
    match index
        .saturating_sub(DEGENERATE_SYMBOLS)
        .checked_div(OCTAVE_SYMBOLS)
    {
        Some(octaves) => octaves.saturating_add(1),
        None => 1,
    }
}

/// The smallest value a bucket index owns.
const fn bucket_low(index: u32) -> u64 {
    if index < DEGENERATE_SYMBOLS {
        return (index as u64).saturating_add(1);
    }
    let width = bucket_width(index);
    let inside = match index
        .saturating_sub(DEGENERATE_SYMBOLS)
        .checked_rem(OCTAVE_SYMBOLS)
    {
        Some(inside) => inside,
        None => 0,
    };
    shift_left(1, width.saturating_add(MANTISSA_BITS))
        .saturating_add(shift_left(inside as u64, width))
}

// The widest alphabet holds 256 symbols, so every symbol either fits sixteen bits or is not a
// symbol of any alphabet this format defines.
#[allow(clippy::cast_possible_truncation)]
const fn narrow_symbol(value: u32) -> Option<u16> {
    if value > u16::MAX as u32 {
        None
    } else {
        Some(value as u16)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn narrow_symbol_wide(value: u64) -> Option<u16> {
    if value > u64::from(u16::MAX) {
        None
    } else {
        Some(value as u16)
    }
}

// The caller masks the value to a position inside one octave, which no alphabet's size exceeds.
#[allow(clippy::cast_possible_truncation)]
const fn narrow_index(value: u64) -> u32 {
    (value & 0xFFFF_FFFF) as u32
}

#[cfg(test)]
mod tests {
    use super::{
        Alphabet, Bucket, LITERAL_RUN_BUCKETS, MATCH_DISTANCE_BUCKETS, MATCH_LENGTH_BUCKETS,
        MAX_LITERAL_RUN, MAX_MATCH_LENGTH, MIN_MATCH, Match, Sequences, Step, WINDOW, bucket_low,
        bucket_width, narrow_symbol,
    };
    use crate::entropy::{MAX_ALPHABET_SIZE, bits::mask};
    use crate::format::{Corruption, Error};

    /// The pair the decoder reads from a table is the pair the general methods compute, for
    /// every symbol of every alphabet.
    ///
    /// This is what keeps the table a form of the decomposition rather than a second
    /// definition of it. The terminal names no value and is checked to be refused as one.
    #[test]
    fn every_bucket_is_what_the_general_methods_compute() -> Result<(), Error> {
        for alphabet in Alphabet::ALL {
            for index in 0..alphabet.size() {
                let symbol = narrow_symbol(index).ok_or(Error::InvalidParameter)?;
                if alphabet.terminal() == Some(symbol) {
                    assert_eq!(
                        alphabet.suffix_width(symbol),
                        Err(Error::CorruptData(Corruption::TerminalSymbol)),
                        "{alphabet:?} {symbol}"
                    );
                    continue;
                }
                let bucket = alphabet.bucket(index);
                assert_eq!(
                    bucket.width,
                    alphabet.suffix_width(symbol)?,
                    "{alphabet:?} {symbol} width"
                );
                assert_eq!(
                    u64::from(bucket.low),
                    alphabet.reconstruct(symbol, 0)?,
                    "{alphabet:?} {symbol} low"
                );
                assert!(
                    u64::from(bucket.low) >= alphabet.coded_min(),
                    "{alphabet:?} {symbol} below the domain"
                );
            }
        }
        Ok(())
    }

    /// Each decode table holds one entry per symbol that names a value, in symbol order, and
    /// its length is derived from the alphabet rather than written down.
    #[test]
    fn every_decode_table_covers_the_symbols_that_name_a_value() {
        let tables: [(Alphabet, &[Bucket]); 3] = [
            (Alphabet::LiteralRun, &LITERAL_RUN_BUCKETS),
            (Alphabet::MatchLength, &MATCH_LENGTH_BUCKETS),
            (Alphabet::MatchDistance, &MATCH_DISTANCE_BUCKETS),
        ];
        for (alphabet, table) in tables {
            let reserved = u32::from(alphabet.carries_terminal());
            assert_eq!(
                u32::try_from(table.len()).unwrap_or(u32::MAX),
                alphabet.size().saturating_sub(reserved),
                "{alphabet:?} table length"
            );
            for (index, bucket) in table.iter().enumerate() {
                let at = u32::try_from(index).unwrap_or(u32::MAX);
                assert_eq!(*bucket, alphabet.bucket(at), "{alphabet:?} {index}");
            }

            // The terminal is the alphabet's last symbol, so the table stops exactly below it
            // and a terminal symbol finds no entry.
            if let Some(terminal) = alphabet.terminal() {
                assert_eq!(usize::from(terminal), table.len(), "{alphabet:?} terminal");
            }
        }
    }

    /// Reused storage holds every invariant a finished vector holds.
    #[test]
    fn storage_built_once_refuses_what_the_finished_vector_refuses() -> Result<(), Error> {
        let mut sequences = Sequences::with_capacity(64, 8);
        sequences.push(
            b"abcd",
            Some(Match {
                length: MIN_MATCH,
                distance: 1,
            }),
        )?;
        assert_eq!(sequences.decoded_len(), 8);
        assert_eq!(sequences.literals(), b"abcd");

        // A step with no match is the last step, and nothing may follow it.
        sequences.push(b"ef", None)?;
        assert_eq!(sequences.push(b"gh", None), Err(Error::InvalidParameter));
        assert_eq!(sequences.decoded_len(), 10);

        // Every value is checked against the domain its alphabet declares.
        let mut fresh = Sequences::with_capacity(64, 8);
        for outside in [
            Match {
                length: MIN_MATCH.saturating_sub(1),
                distance: 1,
            },
            Match {
                length: MAX_MATCH_LENGTH.saturating_add(1),
                distance: 1,
            },
            Match {
                length: MIN_MATCH,
                distance: 0,
            },
            Match {
                length: MIN_MATCH,
                distance: WINDOW.saturating_add(1),
            },
        ] {
            assert_eq!(
                fresh.push(b"", Some(outside)),
                Err(Error::InvalidParameter),
                "{outside:?}"
            );
        }
        let long = vec![0u8; MAX_LITERAL_RUN as usize + 1];
        assert_eq!(fresh.push(&long, None), Err(Error::InvalidParameter));

        // What a refused push left behind is nothing at all.
        assert!(fresh.steps().is_empty());
        assert!(fresh.literals().is_empty());
        Ok(())
    }

    /// Clearing keeps the storage, which is what makes a producer allocation-free per block.
    #[test]
    fn clearing_empties_the_sequences_and_keeps_their_storage() -> Result<(), Error> {
        let mut sequences = Sequences::with_capacity(4_096, 64);
        let literals = sequences.literals().as_ptr();
        for _ in 0..4 {
            sequences.clear();
            assert_eq!(sequences.decoded_len(), 0);
            assert!(sequences.steps().is_empty());
            sequences.push(
                b"ab",
                Some(Match {
                    length: MIN_MATCH,
                    distance: 2,
                }),
            )?;
            assert_eq!(sequences.decoded_len(), 6);
        }
        assert!(
            std::ptr::eq(sequences.literals().as_ptr(), literals),
            "clearing reallocated the literal storage"
        );

        // A refilled vector is the vector the validating constructor would have accepted.
        let rebuilt = Sequences::new(sequences.literals().to_vec(), sequences.steps().to_vec())?;
        assert_eq!(rebuilt, sequences);
        Ok(())
    }

    /// The sizes the decomposition gives the four alphabets, and the terminal's index.
    ///
    /// The figures a block header and the format specification both state, so they are asserted
    /// here rather than derived at every reader.
    #[test]
    fn the_four_alphabets_hold_the_symbols_the_decomposition_gives_them() {
        assert_eq!(Alphabet::LiteralByte.size(), 256);
        assert_eq!(Alphabet::LiteralRun.size(), 60);
        assert_eq!(Alphabet::MatchLength.size(), 28);
        assert_eq!(Alphabet::MatchDistance.size(), 61);

        assert_eq!(Alphabet::MatchLength.terminal(), Some(27));
        for alphabet in Alphabet::ALL {
            assert!(alphabet.size() <= MAX_ALPHABET_SIZE);
            if !alphabet.carries_terminal() {
                assert_eq!(alphabet.terminal(), None);
            }
        }

        // One symbol below the buckets on the distance alphabet, for the offset slot, and none
        // anywhere else.
        assert_eq!(Alphabet::MatchDistance.lead_symbols(), 1);
        assert_eq!(Alphabet::LiteralRun.lead_symbols(), 0);
        assert_eq!(Alphabet::MatchLength.lead_symbols(), 0);
        assert_eq!(Alphabet::LiteralByte.lead_symbols(), 0);
    }

    /// The bijection, walked value for value over every alphabet's whole declared domain.
    ///
    /// The alphabets are small enough to walk whole, and walking them is what keeps the
    /// decomposition a rule rather than a table nobody checked.
    #[test]
    fn every_coded_value_maps_to_one_symbol_and_one_suffix_and_back() -> Result<(), Error> {
        for alphabet in Alphabet::ALL {
            let mut previous_low = None;
            for coded in alphabet.coded_min()..=alphabet.coded_max() {
                let split = alphabet.split(coded)?;
                assert!(
                    u32::from(split.symbol) < alphabet.size(),
                    "{alphabet:?} split {coded} onto a symbol it does not hold"
                );
                assert_eq!(
                    alphabet.suffix_width(split.symbol)?,
                    split.width,
                    "{alphabet:?} symbol {} declares one width and split wrote another",
                    split.symbol
                );
                assert!(split.suffix <= mask(split.width));
                assert_eq!(
                    alphabet.reconstruct(split.symbol, split.suffix)?,
                    coded,
                    "{alphabet:?} did not reconstruct coded value {coded}"
                );

                // The intervals are consecutive: a symbol's low is above the previous symbol's,
                // and the suffix is the position inside it.
                let low = coded.saturating_sub(split.suffix);
                if let Some(seen) = previous_low
                    && seen != low
                {
                    assert!(
                        low > seen,
                        "{alphabet:?} symbol intervals are not increasing"
                    );
                }
                previous_low = Some(low);

                if alphabet.lead_symbols() > 0 && coded <= u64::from(alphabet.lead_symbols()) {
                    // A reserved coded value names a symbol and no raw value. The offset slot
                    // holds what it names, and this alphabet does not.
                    assert_eq!(alphabet.raw_value(coded), None);
                } else {
                    let raw = alphabet
                        .raw_value(coded)
                        .ok_or(Error::CorruptData(Corruption::SequenceSuffix))?;
                    assert_eq!(alphabet.coded_value(raw), Some(coded));
                    assert!(raw >= alphabet.min_raw() && raw <= alphabet.max_raw());
                }
            }
        }
        Ok(())
    }

    /// Every symbol of every alphabet owns an interval, and the intervals partition the domain.
    #[test]
    fn the_symbols_partition_the_domain_they_cover() -> Result<(), Error> {
        for alphabet in Alphabet::ALL {
            let mut expected = alphabet.coded_min();
            for symbol in 0..alphabet.size() {
                let symbol = u16::try_from(symbol).map_err(|_| Error::InvalidParameter)?;
                if alphabet.terminal() == Some(symbol) {
                    assert_eq!(
                        alphabet.suffix_width(symbol),
                        Err(Error::CorruptData(Corruption::TerminalSymbol))
                    );
                    continue;
                }
                let width = alphabet.suffix_width(symbol)?;
                assert_eq!(alphabet.reconstruct(symbol, 0)?, expected);
                expected = expected
                    .saturating_add(mask(width))
                    .saturating_add(1)
                    .min(alphabet.coded_max().saturating_add(1));
            }
            assert_eq!(
                expected,
                alphabet.coded_max().saturating_add(1),
                "{alphabet:?} leaves values its symbols do not cover"
            );
        }
        Ok(())
    }

    #[test]
    fn a_symbol_at_or_above_its_alphabet_is_refused() {
        for alphabet in Alphabet::ALL {
            for symbol in [
                alphabet.size(),
                alphabet.size().saturating_add(1),
                300,
                65_535,
            ] {
                let Ok(symbol) = u16::try_from(symbol) else {
                    continue;
                };
                assert_eq!(
                    alphabet.suffix_width(symbol),
                    Err(Error::CorruptData(Corruption::SequenceSymbol)),
                    "{alphabet:?} admitted symbol {symbol}"
                );
                assert_eq!(
                    alphabet.reconstruct(symbol, 0),
                    Err(Error::CorruptData(Corruption::SequenceSymbol))
                );
            }
        }
    }

    /// The last symbol of a bucketed alphabet owns an interval the domain ends inside, so its
    /// declared share of that interval is narrower than its width. A suffix above the share is
    /// refused here, which is the only place that can see it.
    #[test]
    fn a_suffix_the_symbol_did_not_declare_is_refused() -> Result<(), Error> {
        for alphabet in [
            Alphabet::LiteralRun,
            Alphabet::MatchLength,
            Alphabet::MatchDistance,
        ] {
            let last = alphabet.split(alphabet.coded_max())?;
            assert!(
                last.width > 0,
                "{alphabet:?} ends on a symbol with no suffix, so this clause proves nothing"
            );
            assert!(
                last.suffix < mask(last.width),
                "{alphabet:?} ends exactly on its last symbol's interval"
            );
            for suffix in [
                last.suffix.saturating_add(1),
                mask(last.width),
                mask(last.width).saturating_add(1),
                u64::MAX,
            ] {
                assert_eq!(
                    alphabet.reconstruct(last.symbol, suffix),
                    Err(Error::CorruptData(Corruption::SequenceSuffix)),
                    "{alphabet:?} admitted suffix {suffix} on its last symbol"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn a_value_outside_the_declared_domain_is_not_representable() {
        for alphabet in Alphabet::ALL {
            assert_eq!(
                alphabet.coded_value(alphabet.max_raw().saturating_add(1)),
                None
            );
            assert_eq!(
                alphabet.split(alphabet.coded_max().saturating_add(1)),
                Err(Error::InvalidParameter)
            );
            if alphabet.coded_min() > 0 {
                assert_eq!(alphabet.split(0), Err(Error::InvalidParameter));
            }
        }
        assert_eq!(
            Alphabet::MatchLength.coded_value(u64::from(MIN_MATCH) - 1),
            None
        );
        assert_eq!(Alphabet::MatchDistance.coded_value(0), None);
    }

    #[test]
    fn the_bucket_arithmetic_holds_at_its_boundaries() {
        // The degenerate octaves: one symbol per value, no suffix.
        for index in 0..7u32 {
            assert_eq!(bucket_width(index), 0);
            assert_eq!(bucket_low(index), u64::from(index) + 1);
        }
        // The first cut octave, four symbols of two values each.
        assert_eq!((bucket_width(7), bucket_low(7)), (1, 8));
        assert_eq!((bucket_width(10), bucket_low(10)), (1, 14));
        assert_eq!((bucket_width(11), bucket_low(11)), (2, 16));
        // The octave the window sits at.
        assert_eq!((bucket_width(59), bucket_low(59)), (14, 65_536));
    }

    #[test]
    fn sequences_refuse_what_the_representation_cannot_express() {
        let run = Step {
            run: 1,
            matched: None,
        };
        assert!(Sequences::new(vec![0], vec![run]).is_ok());
        assert_eq!(
            Sequences::new(vec![0, 0], vec![run]),
            Err(Error::InvalidParameter),
            "the runs must sum to the literal bytes"
        );
        assert_eq!(
            Sequences::new(vec![0], vec![run, run]),
            Err(Error::InvalidParameter),
            "a step with no match is the last step"
        );
        for matched in [
            super::Match {
                length: MIN_MATCH - 1,
                distance: 1,
            },
            super::Match {
                length: MAX_MATCH_LENGTH + 1,
                distance: 1,
            },
            super::Match {
                length: MIN_MATCH,
                distance: 0,
            },
            super::Match {
                length: MIN_MATCH,
                distance: WINDOW + 1,
            },
        ] {
            assert_eq!(
                Sequences::new(
                    Vec::new(),
                    vec![Step {
                        run: 0,
                        matched: Some(matched),
                    }]
                ),
                Err(Error::InvalidParameter),
                "{matched:?} is outside the domain the representation declares"
            );
        }
        assert_eq!(
            Sequences::new(
                Vec::new(),
                vec![Step {
                    run: MAX_LITERAL_RUN + 1,
                    matched: None,
                }]
            ),
            Err(Error::InvalidParameter)
        );
    }
}

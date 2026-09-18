//! Owns the four symbol streams of one block: what a sequence vector becomes, and what becomes
//! a sequence vector again.
//!
//! This module does not own the block that carries the streams, the entropy coding their
//! symbols pass through, or the declared extents a block header states for them.
//!
//! Four streams, one per symbol class, in the order the alphabet list gives them. Each stream
//! carries its coded symbols and its raw suffixes separately, so a suffix is never interleaved
//! with a symbol and a decoder reads each through its own position.
//!
//! ```text
//! literal byte      one symbol per literal, no suffix
//! literal run       one symbol per sequence
//! match length      one symbol per sequence, the terminal included
//! match distance    one symbol per sequence that carries a match
//! ```
//!
//! The literal-run count and the match-length count are equal, because every sequence carries
//! both. The match-distance count does not exceed them, because the terminal symbol ends a run
//! that no match follows.

use super::cache::{OffsetCache, REPEAT_CODE};
use super::{
    Alphabet, LITERAL_RUN_BUCKETS, MATCH_DISTANCE_BUCKETS, MATCH_LENGTH_BUCKETS, Match, Sequences,
    Step,
};
use crate::entropy::bits::{BitBuf, BitReader, BitWriter};
use crate::entropy::low_byte;
use crate::format::{Corruption, Error};

/// The match-length symbol that ends a block, which the loop tests every symbol against.
const TERMINAL_LENGTH: Option<u16> = Alphabet::MatchLength.terminal();

/// The largest coded value each bucketed alphabet's domain reaches.
const LITERAL_RUN_CODED_MAX: u64 = Alphabet::LiteralRun.coded_max();
const MATCH_LENGTH_CODED_MAX: u64 = Alphabet::MatchLength.coded_max();
const MATCH_DISTANCE_CODED_MAX: u64 = Alphabet::MatchDistance.coded_max();

/// One symbol class of one block: its coded symbols, and the raw suffixes they declare.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SymbolStream {
    symbols: Vec<u16>,
    suffix: BitBuf,
}

impl SymbolStream {
    /// A stream from symbols a decoder has decoded and the suffix bits its block carried.
    #[must_use]
    pub const fn new(symbols: Vec<u16>, suffix: BitBuf) -> Self {
        Self { symbols, suffix }
    }

    /// The coded symbols, in block order.
    #[must_use]
    pub fn symbols(&self) -> &[u16] {
        &self.symbols
    }

    /// The raw suffixes, in the order the symbols declare them.
    #[must_use]
    pub const fn suffix(&self) -> &BitBuf {
        &self.suffix
    }
}

/// The four streams of one block.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Streams {
    literal_byte: SymbolStream,
    literal_run: SymbolStream,
    match_length: SymbolStream,
    match_distance: SymbolStream,
}

impl Streams {
    /// The streams a decoder has read, in the order a block carries them.
    #[must_use]
    pub const fn new(
        literal_byte: SymbolStream,
        literal_run: SymbolStream,
        match_length: SymbolStream,
        match_distance: SymbolStream,
    ) -> Self {
        Self {
            literal_byte,
            literal_run,
            match_length,
            match_distance,
        }
    }

    /// The stream of one symbol class.
    #[must_use]
    pub const fn stream(&self, alphabet: Alphabet) -> &SymbolStream {
        match alphabet {
            Alphabet::LiteralByte => &self.literal_byte,
            Alphabet::LiteralRun => &self.literal_run,
            Alphabet::MatchLength => &self.match_length,
            Alphabet::MatchDistance => &self.match_distance,
        }
    }

    /// The four streams one sequence vector codes to.
    ///
    /// The cache is the caller's, because the slot carries across the blocks of a region and
    /// is discarded at a region boundary. A match whose distance the slot already names codes
    /// the repeat code instead, and the slot is updated on every match either way.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a value the sequences carry is outside the domain its
    /// alphabet declares, which `Sequences` refuses at construction.
    pub fn of(sequences: &Sequences, cache: &mut OffsetCache) -> Result<Self, Error> {
        let mut literal_symbols = Vec::with_capacity(sequences.literals().len());
        for &byte in sequences.literals() {
            literal_symbols.push(u16::from(byte));
        }

        let steps = sequences.steps();
        let mut runs = Emitter::with_capacity(steps.len());
        let mut lengths = Emitter::with_capacity(steps.len());
        let mut distances = Emitter::with_capacity(steps.len());

        for step in steps {
            runs.emit(Alphabet::LiteralRun, u64::from(step.run))?;
            if let Some(matched) = step.matched {
                lengths.emit(Alphabet::MatchLength, u64::from(matched.length))?;
                if cache.names(matched.distance) {
                    distances.emit_coded(Alphabet::MatchDistance, REPEAT_CODE)?;
                } else {
                    distances.emit(Alphabet::MatchDistance, u64::from(matched.distance))?;
                }
                cache.use_distance(matched.distance);
            } else {
                let terminal = Alphabet::MatchLength
                    .terminal()
                    .ok_or(Error::InvalidParameter)?;
                lengths.emit_symbol(terminal);
            }
        }

        Ok(Self {
            literal_byte: SymbolStream::new(literal_symbols, BitWriter::new().finish()),
            literal_run: runs.finish(),
            match_length: lengths.finish(),
            match_distance: distances.finish(),
        })
    }

    /// The sequence vector the four streams code.
    ///
    /// Every symbol and every suffix here is chosen by whoever wrote the block, so each is
    /// checked against the alphabet that owns it before anything relies on it. The cache is
    /// the caller's, for the reason `of` states, and it is left holding what the last match of
    /// these streams used.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the stream lengths do not describe one block, when a symbol
    /// is not one its alphabet holds, when a suffix takes a value past its domain, when the
    /// terminal symbol occurs anywhere but last, or when a repeat code names a slot that names
    /// no distance. Returns `TruncatedInput` when a suffix stream is shorter than the widths
    /// its symbols declare.
    pub fn sequences(&self, cache: &mut OffsetCache) -> Result<Sequences, Error> {
        let count = self.literal_run.symbols.len();
        if self.match_length.symbols.len() != count || self.match_distance.symbols.len() > count {
            return Err(Error::CorruptData(Corruption::SequenceCount));
        }

        let mut literals = Vec::with_capacity(self.literal_byte.symbols.len());
        for &symbol in &self.literal_byte.symbols {
            literals.push(low_byte(Alphabet::LiteralByte.reconstruct(symbol, 0)?));
        }

        // Measured on project source, logs and short repeated tokens: reading each symbol's
        // width and low coded value from a table rather than computing them per symbol takes
        // this loop from about 29 to about 12 nanoseconds per sequence.
        let mut run_bits = BitReader::over(&self.literal_run.suffix);
        let mut length_bits = BitReader::over(&self.match_length.suffix);
        let mut distance_bits = BitReader::over(&self.match_distance.suffix);
        let mut steps = Vec::with_capacity(count);
        let mut taken = 0usize;

        // The count check above holds both vectors to the sequence count, so the pair walks
        // every sequence and reaches past neither.
        for (index, (&run_symbol, &length_symbol)) in self
            .literal_run
            .symbols
            .iter()
            .zip(&self.match_length.symbols)
            .enumerate()
        {
            let run = narrow(
                Alphabet::LiteralRun
                    .raw_value(read_literal_run(run_symbol, &mut run_bits)?)
                    .ok_or(Error::CorruptData(Corruption::SequenceSuffix))?,
            )?;
            let matched = if TERMINAL_LENGTH == Some(length_symbol) {
                if index.saturating_add(1) != count {
                    return Err(Error::CorruptData(Corruption::TerminalSymbol));
                }
                None
            } else {
                let length = narrow(
                    Alphabet::MatchLength
                        .raw_value(read_match_length(length_symbol, &mut length_bits)?)
                        .ok_or(Error::CorruptData(Corruption::SequenceSuffix))?,
                )?;
                let symbol = symbol_at(&self.match_distance.symbols, taken)?;
                taken = taken.saturating_add(1);
                let coded = read_match_distance(symbol, &mut distance_bits)?;
                let distance = if coded == REPEAT_CODE {
                    cache.resolve()?
                } else {
                    narrow(
                        Alphabet::MatchDistance
                            .raw_value(coded)
                            .ok_or(Error::CorruptData(Corruption::SequenceSuffix))?,
                    )?
                };
                cache.use_distance(distance);
                Some(Match { length, distance })
            };
            steps.push(Step { run, matched });
        }

        if taken != self.match_distance.symbols.len() {
            return Err(Error::CorruptData(Corruption::SequenceCount));
        }
        for reader in [&run_bits, &length_bits, &distance_bits] {
            if reader.consumed() != reader.bits() {
                return Err(Error::CorruptData(Corruption::SequenceExtent));
            }
        }

        Sequences::new(literals, steps).map_err(|_| Error::CorruptData(Corruption::SequenceCount))
    }
}

/// One stream under construction: its symbols, and the suffix bits they declare.
struct Emitter {
    symbols: Vec<u16>,
    suffix: BitWriter,
}

impl Emitter {
    fn with_capacity(steps: usize) -> Self {
        Self {
            symbols: Vec::with_capacity(steps),
            suffix: BitWriter::new(),
        }
    }

    fn emit(&mut self, alphabet: Alphabet, raw: u64) -> Result<(), Error> {
        let coded = alphabet.coded_value(raw).ok_or(Error::InvalidParameter)?;
        self.emit_coded(alphabet, coded)
    }

    fn emit_coded(&mut self, alphabet: Alphabet, coded: u64) -> Result<(), Error> {
        let split = alphabet.split(coded)?;
        self.symbols.push(split.symbol);
        self.suffix.push(split.suffix, split.width);
        Ok(())
    }

    fn emit_symbol(&mut self, symbol: u16) {
        self.symbols.push(symbol);
    }

    fn finish(self) -> SymbolStream {
        SymbolStream::new(self.symbols, self.suffix.finish())
    }
}

/// The symbol at a position, or the count the streams disagree about.
fn symbol_at(symbols: &[u16], at: usize) -> Result<u16, Error> {
    symbols
        .get(at)
        .copied()
        .ok_or(Error::CorruptData(Corruption::SequenceCount))
}

/// The coded value a literal-run symbol and the suffix bits it declares name.
///
/// The three refusals are the ones the general methods make, in the order they made them: a
/// symbol the alphabet names no value with finds no entry, a suffix stream shorter than the
/// entry's width is truncation, and a value past the domain is corruption. The fourth refusal
/// of `Alphabet::reconstruct`, a suffix above the width's mask, is unreachable on this path
/// because `BitReader::take` cannot return more than the width it was given holds.
#[inline]
fn read_literal_run(symbol: u16, bits: &mut BitReader<'_>) -> Result<u64, Error> {
    let bucket = LITERAL_RUN_BUCKETS
        .get(usize::from(symbol))
        .ok_or(Error::CorruptData(Corruption::SequenceSymbol))?;
    let suffix = bits.take(bucket.width)?;
    let coded = u64::from(bucket.low).saturating_add(suffix);
    if coded > LITERAL_RUN_CODED_MAX {
        return Err(Error::CorruptData(Corruption::SequenceSuffix));
    }
    Ok(coded)
}

/// The coded value a match-length symbol and the suffix bits it declares name.
///
/// The terminal names no length and the table stops below it, so a terminal symbol reaching
/// here would be refused as a symbol the alphabet holds no value for. The loop tests for the
/// terminal first, so none does.
#[inline]
fn read_match_length(symbol: u16, bits: &mut BitReader<'_>) -> Result<u64, Error> {
    let bucket = MATCH_LENGTH_BUCKETS
        .get(usize::from(symbol))
        .ok_or(Error::CorruptData(Corruption::SequenceSymbol))?;
    let suffix = bits.take(bucket.width)?;
    let coded = u64::from(bucket.low).saturating_add(suffix);
    if coded > MATCH_LENGTH_CODED_MAX {
        return Err(Error::CorruptData(Corruption::SequenceSuffix));
    }
    Ok(coded)
}

/// The coded value a match-distance symbol and the suffix bits it declares name.
///
/// Coded, and not the distance, because the repeat code is a coded value the alphabet names no
/// distance for and the caller resolves it against the offset slot.
#[inline]
fn read_match_distance(symbol: u16, bits: &mut BitReader<'_>) -> Result<u64, Error> {
    let bucket = MATCH_DISTANCE_BUCKETS
        .get(usize::from(symbol))
        .ok_or(Error::CorruptData(Corruption::SequenceSymbol))?;
    let suffix = bits.take(bucket.width)?;
    let coded = u64::from(bucket.low).saturating_add(suffix);
    if coded > MATCH_DISTANCE_CODED_MAX {
        return Err(Error::CorruptData(Corruption::SequenceSuffix));
    }
    Ok(coded)
}

/// A value the alphabets bound below the window, as the sequence types carry it.
fn narrow(value: u64) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::CorruptData(Corruption::SequenceSuffix))
}

#[cfg(test)]
mod tests {
    use super::{Streams, SymbolStream, read_literal_run, read_match_distance, read_match_length};
    use crate::entropy::bits::{BitBuf, BitReader, BitWriter, mask};
    use crate::format::{Corruption, Error};
    use crate::sequence::cache::OffsetCache;
    use crate::sequence::{
        Alphabet, MAX_LITERAL_RUN, MAX_MATCH_LENGTH, MIN_MATCH, Match, Sequences, Step, WINDOW,
    };

    /// One specialized reader, as the agreement test drives it.
    type Reader = fn(u16, &mut BitReader<'_>) -> Result<u64, Error>;

    /// The three bucketed alphabets and the reader the decode loop reads each through.
    const READERS: [(Alphabet, Reader); 3] = [
        (Alphabet::LiteralRun, read_literal_run),
        (Alphabet::MatchLength, read_match_length),
        (Alphabet::MatchDistance, read_match_distance),
    ];

    /// A deterministic stream of values, so a failure reproduces from its seed alone.
    struct Source {
        state: u64,
    }

    impl Source {
        const fn new(seed: u64) -> Self {
            Self { state: seed | 1 }
        }

        fn next(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.state >> 11
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next().checked_rem(bound.max(1)).unwrap_or(0)
        }
    }

    fn literals_for(steps: &[Step], seed: u64) -> Vec<u8> {
        let mut source = Source::new(seed);
        let mut out = Vec::new();
        for step in steps {
            for _ in 0..step.run {
                out.push(u8::try_from(source.below(256)).unwrap_or(0));
            }
        }
        out
    }

    fn round_trip(steps: Vec<Step>, seed: u64) -> Result<Streams, Error> {
        let sequences = Sequences::new(literals_for(&steps, seed), steps)?;
        let streams = Streams::of(&sequences, &mut OffsetCache::reset())?;
        assert_eq!(
            streams.sequences(&mut OffsetCache::reset())?,
            sequences,
            "seed {seed}: the four streams did not decode to the sequences they coded"
        );
        assert_eq!(
            streams.stream(Alphabet::LiteralRun).symbols().len(),
            streams.stream(Alphabet::MatchLength).symbols().len(),
            "the literal-run count and the match-length count are equal"
        );
        assert!(
            streams.stream(Alphabet::MatchDistance).symbols().len()
                <= streams.stream(Alphabet::LiteralRun).symbols().len(),
            "the match-distance count does not exceed the sequence count"
        );
        assert!(
            streams.stream(Alphabet::LiteralByte).suffix().is_empty(),
            "the literal byte alphabet carries no suffix"
        );
        Ok(streams)
    }

    const fn matched(length: u32, distance: u32) -> Match {
        Match { length, distance }
    }

    /// Every shape the exit gate names, each round-tripped through the four streams.
    #[test]
    fn every_shape_of_sequence_vector_round_trips() -> Result<(), Error> {
        // Literals only: one sequence, no match, the terminal ends the block.
        let _ = round_trip(
            vec![Step {
                run: 300,
                matched: None,
            }],
            1,
        )?;

        // Matches only: no literal byte is emitted at all.
        let _ = round_trip(
            (0..40)
                .map(|index| Step {
                    run: 0,
                    matched: Some(matched(MIN_MATCH + index % 7, 1 + index % 11)),
                })
                .collect(),
            2,
        )?;

        // Alternating runs and matches.
        let _ = round_trip(
            (0..60)
                .map(|index| Step {
                    run: index % 5,
                    matched: Some(matched(MIN_MATCH + index % 13, 1 + index * 7 % 4_096)),
                })
                .collect(),
            3,
        )?;

        // A run of the longest length the representation can express.
        let _ = round_trip(
            vec![
                Step {
                    run: MAX_LITERAL_RUN,
                    matched: Some(matched(MIN_MATCH, 1)),
                },
                Step {
                    run: 0,
                    matched: None,
                },
            ],
            4,
        )?;

        // The match boundaries: the shortest, the longest, a distance of one, and a distance of
        // the whole window.
        let _ = round_trip(
            vec![
                Step {
                    run: 1,
                    matched: Some(matched(MIN_MATCH, 1)),
                },
                Step {
                    run: 1,
                    matched: Some(matched(MAX_MATCH_LENGTH, WINDOW)),
                },
                Step {
                    run: 1,
                    matched: Some(matched(MAX_MATCH_LENGTH, 1)),
                },
                Step {
                    run: 1,
                    matched: Some(matched(MIN_MATCH, WINDOW)),
                },
            ],
            5,
        )?;

        // A block that ends on a match, so it carries no terminal at all.
        let streams = round_trip(
            vec![Step {
                run: 3,
                matched: Some(matched(MIN_MATCH, 2)),
            }],
            6,
        )?;
        assert_eq!(
            streams.stream(Alphabet::MatchLength).symbols(),
            &[Alphabet::MatchLength.split(1)?.symbol]
        );

        // A block with no sequences at all.
        let _ = round_trip(Vec::new(), 7)?;

        // Every length and every distance the domains admit, walked at the extremes and across
        // the octave boundaries the decomposition cuts at.
        let mut steps = Vec::new();
        for length in [MIN_MATCH, 5, 7, 8, 15, 16, 131, 132, 255, MAX_MATCH_LENGTH] {
            for distance in [1u32, 2, 3, 7, 8, 15, 16, 4_096, 65_535, WINDOW] {
                steps.push(Step {
                    run: distance % 3,
                    matched: Some(matched(length, distance)),
                });
            }
        }
        let _ = round_trip(steps, 8)?;
        Ok(())
    }

    /// The generated case: 10 000 matches whose distances repeat at a declared rate, and both
    /// sides resolving the same distances.
    ///
    /// One slot updated on every match means the repeat code fires exactly when a distance
    /// equals the one before it, so the count is computed from the distances alone rather than
    /// from a second copy of the cache.
    #[test]
    fn the_cache_resolves_the_same_distances_on_both_sides() -> Result<(), Error> {
        const MATCHES: usize = 10_000;
        const REPEAT_IN: u64 = 3;

        let mut source = Source::new(97);
        let mut distances: Vec<u32> = Vec::with_capacity(MATCHES);
        for index in 0..MATCHES {
            let repeat = index > 0 && source.below(REPEAT_IN) == 0;
            let distance = if repeat {
                distances.last().copied().unwrap_or(1)
            } else {
                u32::try_from(source.below(u64::from(WINDOW)).saturating_add(1)).unwrap_or(1)
            };
            distances.push(distance);
        }

        let steps: Vec<Step> = distances
            .iter()
            .enumerate()
            .map(|(index, &distance)| Step {
                run: u32::try_from(index % 4).unwrap_or(0),
                matched: Some(matched(
                    MIN_MATCH + u32::try_from(index % 17).unwrap_or(0),
                    distance,
                )),
            })
            .collect();
        let streams = round_trip(steps, 98)?;

        let expected = distances
            .windows(2)
            .filter(|pair| pair.first() == pair.last())
            .count();
        let repeat_symbol = Alphabet::MatchDistance.split(super::REPEAT_CODE)?.symbol;
        let coded = streams
            .stream(Alphabet::MatchDistance)
            .symbols()
            .iter()
            .filter(|&&symbol| symbol == repeat_symbol)
            .count();
        assert_eq!(
            coded, expected,
            "the repeat code fires exactly when a distance equals the one before it"
        );
        assert!(
            expected > MATCHES / 10,
            "the declared repeat rate produced {expected} repeats, too few to prove anything"
        );
        Ok(())
    }

    /// The first match of a region has no slot to name, so a stream that names one is refused
    /// before a distance exists.
    #[test]
    fn a_repeat_code_before_any_match_is_refused() -> Result<(), Error> {
        let repeat = Alphabet::MatchDistance.split(super::REPEAT_CODE)?.symbol;
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![Alphabet::LiteralRun.split(1)?.symbol], empty()),
            SymbolStream::new(vec![Alphabet::MatchLength.split(1)?.symbol], empty()),
            SymbolStream::new(vec![repeat], empty()),
        );
        assert_eq!(
            streams.sequences(&mut OffsetCache::reset()),
            Err(Error::CorruptData(Corruption::RepeatUnset))
        );
        Ok(())
    }

    #[test]
    fn the_terminal_symbol_anywhere_but_last_is_refused() -> Result<(), Error> {
        let terminal = Alphabet::MatchLength
            .terminal()
            .ok_or(Error::InvalidParameter)?;
        let run = Alphabet::LiteralRun.split(1)?.symbol;
        let length = Alphabet::MatchLength.split(1)?.symbol;
        let distance = Alphabet::MatchDistance.split(2)?.symbol;

        // The terminal in the first of two sequences.
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![run, run], empty()),
            SymbolStream::new(vec![terminal, length], empty()),
            SymbolStream::new(vec![distance], empty()),
        );
        assert_eq!(
            streams.sequences(&mut OffsetCache::reset()),
            Err(Error::CorruptData(Corruption::TerminalSymbol))
        );

        // The same symbol as the last match-length symbol is the block's ending, not corruption.
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![run, run], empty()),
            SymbolStream::new(vec![length, terminal], empty()),
            SymbolStream::new(vec![distance], empty()),
        );
        assert!(streams.sequences(&mut OffsetCache::reset()).is_ok());

        // The terminal carries no suffix and names no length, wherever it is read from.
        assert_eq!(
            Alphabet::MatchLength.suffix_width(terminal),
            Err(Error::CorruptData(Corruption::TerminalSymbol))
        );
        Ok(())
    }

    #[test]
    fn stream_counts_that_do_not_describe_one_block_are_refused() -> Result<(), Error> {
        let run = Alphabet::LiteralRun.split(1)?.symbol;
        let length = Alphabet::MatchLength.split(1)?.symbol;
        let distance = Alphabet::MatchDistance.split(2)?.symbol;

        for (runs, lengths, distances) in [
            (vec![run, run], vec![length], vec![distance, distance]),
            (vec![run], vec![length, length], vec![distance]),
            (vec![run], vec![length], vec![distance, distance]),
        ] {
            let streams = Streams::new(
                SymbolStream::default(),
                SymbolStream::new(runs, empty()),
                SymbolStream::new(lengths, empty()),
                SymbolStream::new(distances, empty()),
            );
            assert_eq!(
                streams.sequences(&mut OffsetCache::reset()),
                Err(Error::CorruptData(Corruption::SequenceCount))
            );
        }

        // A literal stream the runs do not account for.
        let streams = Streams::new(
            SymbolStream::new(vec![0, 0], empty()),
            SymbolStream::new(vec![run], empty()),
            SymbolStream::new(vec![length], empty()),
            SymbolStream::new(vec![distance], empty()),
        );
        assert_eq!(
            streams.sequences(&mut OffsetCache::reset()),
            Err(Error::CorruptData(Corruption::SequenceCount))
        );
        Ok(())
    }

    #[test]
    fn a_symbol_no_alphabet_holds_is_refused() -> Result<(), Error> {
        let run = Alphabet::LiteralRun.split(1)?.symbol;
        let length = Alphabet::MatchLength.split(1)?.symbol;
        let distance = Alphabet::MatchDistance.split(2)?.symbol;

        let past = |alphabet: Alphabet| u16::try_from(alphabet.size()).unwrap_or(u16::MAX);
        for (literals, runs, lengths, distances) in [
            (
                vec![past(Alphabet::LiteralByte)],
                vec![run],
                vec![length],
                vec![distance],
            ),
            (
                Vec::new(),
                vec![past(Alphabet::LiteralRun)],
                vec![length],
                vec![distance],
            ),
            (
                Vec::new(),
                vec![run],
                vec![past(Alphabet::MatchLength)],
                vec![distance],
            ),
            (
                Vec::new(),
                vec![run],
                vec![length],
                vec![past(Alphabet::MatchDistance)],
            ),
        ] {
            let streams = Streams::new(
                SymbolStream::new(literals, empty()),
                SymbolStream::new(runs, empty()),
                SymbolStream::new(lengths, empty()),
                SymbolStream::new(distances, empty()),
            );
            assert_eq!(
                streams.sequences(&mut OffsetCache::reset()),
                Err(Error::CorruptData(Corruption::SequenceSymbol))
            );
        }
        Ok(())
    }

    /// A suffix stream that carries neither what the symbols declare nor what they admit.
    #[test]
    fn a_suffix_stream_the_symbols_do_not_account_for_is_refused() -> Result<(), Error> {
        // The last symbol of the run alphabet owns an interval the domain ends inside, so a
        // suffix above its declared share names a run the representation cannot express.
        let last = Alphabet::LiteralRun.split(Alphabet::LiteralRun.coded_max())?;
        let mut writer = BitWriter::new();
        writer.push(mask(last.width), last.width);
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![last.symbol], writer.finish()),
            SymbolStream::new(
                vec![
                    Alphabet::MatchLength
                        .terminal()
                        .ok_or(Error::InvalidParameter)?,
                ],
                empty(),
            ),
            SymbolStream::default(),
        );
        assert_eq!(
            streams.sequences(&mut OffsetCache::reset()),
            Err(Error::CorruptData(Corruption::SequenceSuffix))
        );

        // A suffix stream shorter than the widths its symbols declare is truncation, and never
        // a suffix rebuilt out of padding.
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![last.symbol], empty()),
            SymbolStream::new(
                vec![
                    Alphabet::MatchLength
                        .terminal()
                        .ok_or(Error::InvalidParameter)?,
                ],
                empty(),
            ),
            SymbolStream::default(),
        );
        assert!(matches!(
            streams.sequences(&mut OffsetCache::reset()),
            Err(Error::TruncatedInput { .. })
        ));

        // A suffix stream longer than the widths its symbols declare carries bits no symbol
        // accounts for.
        let mut writer = BitWriter::new();
        writer.push(0, 9);
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![Alphabet::LiteralRun.split(1)?.symbol], writer.finish()),
            SymbolStream::new(
                vec![
                    Alphabet::MatchLength
                        .terminal()
                        .ok_or(Error::InvalidParameter)?,
                ],
                empty(),
            ),
            SymbolStream::default(),
        );
        assert_eq!(
            streams.sequences(&mut OffsetCache::reset()),
            Err(Error::CorruptData(Corruption::SequenceExtent))
        );
        Ok(())
    }

    /// A decoder holds decoded symbols and the suffix bytes its block declared an extent for,
    /// and nothing else. Rebuilding the streams from exactly those two proves the decode side
    /// needs nothing the encoder kept.
    #[test]
    fn a_decoder_assembles_the_streams_from_symbols_and_declared_extents() -> Result<(), Error> {
        let steps: Vec<Step> = (0..200)
            .map(|index| Step {
                run: index % 6,
                matched: Some(matched(MIN_MATCH + index % 9, 1 + index * 13 % 9_973)),
            })
            .collect();
        let sequences = Sequences::new(literals_for(&steps, 41), steps)?;
        let streams = Streams::of(&sequences, &mut OffsetCache::reset())?;

        let rebuilt = |alphabet: Alphabet| -> Result<SymbolStream, Error> {
            let stream = streams.stream(alphabet);
            Ok(SymbolStream::new(
                stream.symbols().to_vec(),
                BitBuf::new(stream.suffix().bytes().to_vec(), stream.suffix().bits())?,
            ))
        };
        let read = Streams::new(
            rebuilt(Alphabet::LiteralByte)?,
            rebuilt(Alphabet::LiteralRun)?,
            rebuilt(Alphabet::MatchLength)?,
            rebuilt(Alphabet::MatchDistance)?,
        );
        assert_eq!(read, streams);
        assert_eq!(read.sequences(&mut OffsetCache::reset())?, sequences);
        Ok(())
    }

    /// A suffix stream carrying one symbol's raw suffix at a declared width.
    fn suffix_of(value: u64, width: u32) -> BitBuf {
        let mut writer = BitWriter::new();
        writer.push(value, width);
        writer.finish()
    }

    /// Every specialized reader returns what the general methods return, for every symbol its
    /// alphabet names a value with and at the suffix values that bound each symbol's interval.
    ///
    /// This is what holds the decode tables to the decomposition: a reader that drifted from
    /// `reconstruct` on any symbol, or consumed a width other than the one the symbol
    /// declares, fails here.
    #[test]
    fn every_specialized_reader_agrees_with_the_general_methods() -> Result<(), Error> {
        for (alphabet, reader) in READERS {
            for index in 0..alphabet.size() {
                let symbol = u16::try_from(index).map_err(|_| Error::InvalidParameter)?;
                if alphabet.terminal() == Some(symbol) {
                    continue;
                }
                let width = alphabet.suffix_width(symbol)?;
                let top = mask(width);
                for suffix in [0, top.min(1), top.saturating_sub(1), top] {
                    let buf = suffix_of(suffix, width);
                    let mut bits = BitReader::over(&buf);
                    let read = reader(symbol, &mut bits);
                    assert_eq!(
                        read,
                        alphabet.reconstruct(symbol, suffix),
                        "{alphabet:?} symbol {symbol} suffix {suffix}"
                    );
                    assert_eq!(
                        bits.consumed(),
                        u64::from(width),
                        "{alphabet:?} symbol {symbol} consumed a width the symbol did not declare"
                    );
                    if let Ok(coded) = read {
                        assert_eq!(
                            alphabet.raw_value(coded),
                            alphabet
                                .reconstruct(symbol, suffix)
                                .ok()
                                .and_then(|coded| alphabet.raw_value(coded)),
                            "{alphabet:?} symbol {symbol} suffix {suffix} raw value"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// A symbol its alphabet names no value with is refused as a symbol, before any suffix is
    /// read and whichever alphabet it arrives on.
    #[test]
    fn every_specialized_reader_refuses_a_symbol_its_alphabet_does_not_name_a_value_with()
    -> Result<(), Error> {
        for (alphabet, reader) in READERS {
            let reserved = u32::from(alphabet.carries_terminal());
            let first = alphabet.size().saturating_sub(reserved);
            for index in [first, alphabet.size(), u32::from(u16::MAX)] {
                let symbol = u16::try_from(index).map_err(|_| Error::InvalidParameter)?;
                let buf = suffix_of(0, 14);
                let mut bits = BitReader::over(&buf);
                assert_eq!(
                    reader(symbol, &mut bits),
                    Err(Error::CorruptData(Corruption::SequenceSymbol)),
                    "{alphabet:?} symbol {symbol}"
                );
                assert_eq!(bits.consumed(), 0, "{alphabet:?} symbol {symbol}");
            }
        }
        Ok(())
    }

    /// The three refusals the decode loop makes, in the classes the general methods named
    /// them: a symbol no value belongs to, a suffix stream shorter than the width its symbol
    /// declares, and a value the suffix takes past the domain.
    #[test]
    fn every_reader_refusal_keeps_the_class_the_general_methods_name() -> Result<(), Error> {
        for (alphabet, reader) in READERS {
            let reserved = u32::from(alphabet.carries_terminal());
            let last = u16::try_from(alphabet.size().saturating_sub(reserved).saturating_sub(1))
                .map_err(|_| Error::InvalidParameter)?;
            let width = alphabet.suffix_width(last)?;
            assert!(width > 0, "{alphabet:?} last symbol carries no suffix");

            // One bit short of what the symbol declares is truncation and not corruption.
            let short = suffix_of(0, width.saturating_sub(1));
            let mut bits = BitReader::over(&short);
            assert!(
                matches!(reader(last, &mut bits), Err(Error::TruncatedInput { .. })),
                "{alphabet:?} short suffix"
            );

            // The last symbol owns an interval the domain ends inside, so its widest suffix
            // names a value the alphabet does not reach.
            let past = suffix_of(mask(width), width);
            let mut bits = BitReader::over(&past);
            assert_eq!(
                reader(last, &mut bits),
                Err(Error::CorruptData(Corruption::SequenceSuffix)),
                "{alphabet:?} suffix past the domain"
            );
        }
        Ok(())
    }

    /// The terminal ends a block and marks nothing else, at every position a block can hold.
    ///
    /// The specialized length reader has no entry for the terminal, so the loop's own test for
    /// it is what keeps the error class `TerminalSymbol` rather than `SequenceSymbol`.
    #[test]
    fn a_terminal_symbol_is_the_block_ending_only_as_the_last_symbol() -> Result<(), Error> {
        let run = Alphabet::LiteralRun.split(1)?.symbol;
        let length = Alphabet::MatchLength.split(1)?.symbol;
        let distance = Alphabet::MatchDistance.split(2)?.symbol;
        let terminal = Alphabet::MatchLength
            .terminal()
            .ok_or(Error::InvalidParameter)?;

        for count in 1..=4usize {
            for at in 0..count {
                let lengths: Vec<u16> = (0..count)
                    .map(|index| if index == at { terminal } else { length })
                    .collect();
                let streams = Streams::new(
                    SymbolStream::default(),
                    SymbolStream::new(vec![run; count], empty()),
                    SymbolStream::new(lengths, empty()),
                    SymbolStream::new(vec![distance; count.saturating_sub(1)], empty()),
                );
                let decoded = streams.sequences(&mut OffsetCache::reset());
                if at.saturating_add(1) == count {
                    assert!(
                        decoded.is_ok(),
                        "count {count} terminal at {at}: {decoded:?}"
                    );
                } else {
                    assert_eq!(
                        decoded,
                        Err(Error::CorruptData(Corruption::TerminalSymbol)),
                        "count {count} terminal at {at}"
                    );
                }
            }
        }
        Ok(())
    }

    /// Generated step vectors: every shape the generator produces codes to four streams and
    /// decodes back to itself.
    #[test]
    fn every_generated_step_vector_round_trips() {
        for seed in 1..=96u64 {
            let mut source = Source::new(seed.wrapping_mul(6_364_136_223_846_793_005));
            let count = usize::try_from(source.below(120).saturating_add(1)).unwrap_or(1);
            let mut steps = Vec::with_capacity(count);
            for index in 0..count {
                let spread = match source.below(8) {
                    0 => 1_024,
                    1 => 64,
                    _ => 4,
                };
                let run = u32::try_from(source.below(spread)).unwrap_or(0);
                let last = index.saturating_add(1) == count;
                let matched = if last && source.below(2) == 0 {
                    None
                } else {
                    let span =
                        u64::from(MAX_MATCH_LENGTH.saturating_sub(MIN_MATCH)).saturating_add(1);
                    Some(matched(
                        MIN_MATCH.saturating_add(u32::try_from(source.below(span)).unwrap_or(0)),
                        u32::try_from(source.below(u64::from(WINDOW)))
                            .unwrap_or(0)
                            .saturating_add(1),
                    ))
                };
                steps.push(Step { run, matched });
            }
            let coded = round_trip(steps, seed);
            assert!(coded.is_ok(), "seed {seed}: {coded:?}");
        }
    }

    fn empty() -> BitBuf {
        BitWriter::new().finish()
    }
}

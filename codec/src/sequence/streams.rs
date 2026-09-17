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
use super::{Alphabet, Match, Sequences, Step};
use crate::entropy::bits::{BitBuf, BitReader, BitWriter};
use crate::entropy::low_byte;
use crate::format::{Corruption, Error};

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
    /// The offset cache starts unset, which is the state a region boundary restores, and a
    /// match whose distance the slot already names codes the repeat code instead.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a value the sequences carry is outside the domain its
    /// alphabet declares, which `Sequences` refuses at construction.
    pub fn of(sequences: &Sequences) -> Result<Self, Error> {
        let mut literal_symbols = Vec::with_capacity(sequences.literals().len());
        for &byte in sequences.literals() {
            literal_symbols.push(u16::from(byte));
        }

        let steps = sequences.steps();
        let mut runs = Emitter::with_capacity(steps.len());
        let mut lengths = Emitter::with_capacity(steps.len());
        let mut distances = Emitter::with_capacity(steps.len());
        let mut cache = OffsetCache::reset();

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
    /// checked against the alphabet that owns it before anything relies on it.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the stream lengths do not describe one block, when a symbol
    /// is not one its alphabet holds, when a suffix takes a value past its domain, when the
    /// terminal symbol occurs anywhere but last, or when a repeat code names a slot that names
    /// no distance. Returns `TruncatedInput` when a suffix stream is shorter than the widths
    /// its symbols declare.
    pub fn sequences(&self) -> Result<Sequences, Error> {
        let count = self.literal_run.symbols.len();
        if self.match_length.symbols.len() != count || self.match_distance.symbols.len() > count {
            return Err(Error::CorruptData(Corruption::SequenceCount));
        }

        let mut literals = Vec::with_capacity(self.literal_byte.symbols.len());
        for &symbol in &self.literal_byte.symbols {
            literals.push(low_byte(Alphabet::LiteralByte.reconstruct(symbol, 0)?));
        }

        let mut run_bits = BitReader::over(&self.literal_run.suffix);
        let mut length_bits = BitReader::over(&self.match_length.suffix);
        let mut distance_bits = BitReader::over(&self.match_distance.suffix);
        let mut cache = OffsetCache::reset();
        let mut steps = Vec::with_capacity(count);
        let mut taken = 0usize;

        for index in 0..count {
            let run = narrow(read(
                Alphabet::LiteralRun,
                symbol_at(&self.literal_run.symbols, index)?,
                &mut run_bits,
            )?)?;
            let coded_length = symbol_at(&self.match_length.symbols, index)?;
            let matched = if Alphabet::MatchLength.terminal() == Some(coded_length) {
                if index.saturating_add(1) != count {
                    return Err(Error::CorruptData(Corruption::TerminalSymbol));
                }
                None
            } else {
                let length = narrow(read(Alphabet::MatchLength, coded_length, &mut length_bits)?)?;
                let symbol = symbol_at(&self.match_distance.symbols, taken)?;
                taken = taken.saturating_add(1);
                let coded = read_coded(Alphabet::MatchDistance, symbol, &mut distance_bits)?;
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

/// The raw value a symbol and the suffix bits it declares name.
fn read(alphabet: Alphabet, symbol: u16, bits: &mut BitReader<'_>) -> Result<u64, Error> {
    let coded = read_coded(alphabet, symbol, bits)?;
    alphabet
        .raw_value(coded)
        .ok_or(Error::CorruptData(Corruption::SequenceSuffix))
}

/// The coded value a symbol and the suffix bits it declares name.
fn read_coded(alphabet: Alphabet, symbol: u16, bits: &mut BitReader<'_>) -> Result<u64, Error> {
    let width = alphabet.suffix_width(symbol)?;
    let suffix = bits.take(width)?;
    alphabet.reconstruct(symbol, suffix)
}

/// A value the alphabets bound below the window, as the sequence types carry it.
fn narrow(value: u64) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::CorruptData(Corruption::SequenceSuffix))
}

#[cfg(test)]
mod tests {
    use super::{Streams, SymbolStream};
    use crate::entropy::bits::{BitBuf, BitWriter, mask};
    use crate::format::{Corruption, Error};
    use crate::sequence::{
        Alphabet, MAX_LITERAL_RUN, MAX_MATCH_LENGTH, MIN_MATCH, Match, Sequences, Step, WINDOW,
    };

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
        let streams = Streams::of(&sequences)?;
        assert_eq!(
            streams.sequences()?,
            sequences,
            "the four streams did not decode to the sequences they coded"
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
            streams.sequences(),
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
            streams.sequences(),
            Err(Error::CorruptData(Corruption::TerminalSymbol))
        );

        // The same symbol as the last match-length symbol is the block's ending, not corruption.
        let streams = Streams::new(
            SymbolStream::default(),
            SymbolStream::new(vec![run, run], empty()),
            SymbolStream::new(vec![length, terminal], empty()),
            SymbolStream::new(vec![distance], empty()),
        );
        assert!(streams.sequences().is_ok());

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
                streams.sequences(),
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
            streams.sequences(),
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
                streams.sequences(),
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
            streams.sequences(),
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
            streams.sequences(),
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
            streams.sequences(),
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
        let streams = Streams::of(&sequences)?;

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
        assert_eq!(read.sequences()?, sequences);
        Ok(())
    }

    fn empty() -> BitBuf {
        BitWriter::new().finish()
    }
}

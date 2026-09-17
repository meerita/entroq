//! Owns parse strategy: which of the representations a match finder's proposals admit the
//! encoder emits.
//!
//! This module does not own match search or entropy coding. It decides what to emit.
//!
//! One strategy: greedy. The parser searches at the position it stands on, takes the match the
//! finder reports when it reaches the minimum, and never revisits the decision. One candidate
//! source, one commit per position, and no cost model: the choice between two candidates is
//! the finder's tie-break and the choice between a match and a literal is the minimum match
//! length. At search depth 8 this pair spends the least encoder time of the five measured pairs
//! whose commit horizon is bounded under every cost model they were priced against. What it
//! gives up against an exact optimal parse is 19.86 per cent of the representation cost.
//!
//! # What the parser holds
//!
//! The parser owns the window a match reaches into. The buffer is three windows wide, which is
//! what lets the slide that makes room move by exactly one window and still leave a whole
//! window of history behind it, whatever block length the caller feeds.
//!
//! ```text
//! head table and link array    327 680 bytes    the finder's
//! window buffer                196 608 bytes    three windows
//! literal bytes                the block        the block's worst case, every byte a literal
//! steps                        the block / 4    the block's worst case, one step per match
//! ```
//!
//! Every one of them is allocated once, when the parser is built. The sequence storage is
//! emptied and refilled per block rather than allocated per block, so the steady state costs
//! no allocation at all and the figure above is the same at the first block and at the
//! millionth.
//!
//! The figure is what the parser allocates and holds. It does not count the input fragment,
//! which the caller owns and passes in, and it is not a resident-set figure: nothing here
//! measures resident pages.
//!
//! # The horizon
//!
//! A decision at a position reads at most one maximum match length ahead of it, because that
//! is the longest match the representation expresses, so the lookahead the strategy requires is
//! 256 bytes and does not move with the input. The literal bytes behind the position are
//! already decided and are output rather than lookahead.
//!
//! # Determinism
//!
//! The same input, cut into the same blocks, produces the same sequences. The finder's
//! tie-break reads a reported length and distance and nothing else, the walk order is the
//! chain's and not a hash table's iteration order, and no branch here reads an address.

use crate::format::Error;
use crate::matchfinder::BoundedHashChain;
use crate::sequence::{MIN_MATCH, Sequences, Step, WINDOW};
use crate::simd::Kernel;

/// The input bytes one call may parse.
///
/// One window, which is also the longest literal run the representation expresses, so a block
/// of this length that holds no match at all is still one sequence.
pub const MAX_PARSE_BYTES: usize = WINDOW as usize;

/// The bytes the parser's buffer holds.
///
/// Three windows. Two would hold a window and a block, but a slide moves by one window exactly
/// so that every stored position keeps its link slot, and from a buffer of two windows that
/// leaves only `filled - window` bytes of history behind. At three windows a slide is reached
/// only above two windows filled, so a whole window always survives it.
const CAPACITY: usize = MAX_PARSE_BYTES.saturating_mul(3);

/// The bytes the strategy reads ahead of the position it decides at.
pub const LOOKAHEAD_BYTES: usize = crate::sequence::MAX_MATCH_LENGTH as usize;

/// A greedy parse over a bounded hash chain.
pub struct Parser {
    finder: BoundedHashChain,
    window: Box<[u8]>,
    /// The bytes of `window` that hold input.
    filled: usize,
    /// The first position of `window` the finder has not been given.
    inserted: usize,
    /// The sequences of the block last parsed. Emptied and refilled, never reallocated.
    sequences: Sequences,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// A parser with no history, on the kernel this build selected.
    #[must_use]
    pub fn new() -> Self {
        Self::of(BoundedHashChain::new())
    }

    /// A parser with no history, on a named kernel.
    #[must_use]
    pub fn with_kernel(kernel: Kernel) -> Self {
        Self::of(BoundedHashChain::with_kernel(kernel))
    }

    /// The kernel the finder compares candidates with.
    #[must_use]
    pub const fn kernel(&self) -> Kernel {
        self.finder.kernel()
    }

    /// The bytes the parser holds between calls, whatever the input is.
    #[must_use]
    pub const fn state_bytes() -> usize {
        BoundedHashChain::state_bytes().saturating_add(CAPACITY)
    }

    /// The bytes the parser allocates and holds, for a block of `block_bytes`.
    ///
    /// This is parser-owned allocated memory: every allocation the parser makes, held from the
    /// moment it is built until it is dropped. It does not count the input fragment, which the
    /// caller owns and passes in, and it is not a resident-set figure.
    ///
    /// Every term is allocated once and none of them grows, so this is a bound the run meets
    /// rather than an estimate it approaches. A block that holds no match fills the literal
    /// storage exactly; a block of nothing but minimum-length matches fills the step storage
    /// exactly; no block fills both.
    #[must_use]
    pub const fn declared_bytes(block_bytes: usize) -> usize {
        Self::state_bytes()
            .saturating_add(block_bytes)
            .saturating_add(step_capacity(block_bytes).saturating_mul(size_of::<Step>()))
    }

    /// The bytes a match in the next call may reach back into.
    ///
    /// At most one window, because that is the farthest a distance can name. Every byte
    /// returned survives the slide the next call may perform, so this is what the next block
    /// can reach and not what the buffer happens to hold.
    #[must_use]
    pub fn history(&self) -> &[u8] {
        let reach = self.filled.min(MAX_PARSE_BYTES);
        self.window
            .get(self.filled.saturating_sub(reach)..self.filled)
            .unwrap_or_default()
    }

    /// Discards the history.
    ///
    /// A region boundary is what calls this. Nothing a later region emits may name a position
    /// an earlier one produced, and the offset cache a block carries is discarded on the same
    /// boundary.
    pub fn reset(&mut self) {
        self.finder.reset();
        self.filled = 0;
        self.inserted = 0;
    }

    /// The sequences of one block of input.
    ///
    /// The bytes are appended to the history first, so a match may reach back into the blocks
    /// before this one, as far as the window. The sequences decode to exactly `input`.
    ///
    /// The result borrows the storage the parser reuses, so the next call overwrites it. A
    /// caller that needs two blocks at once clones the first.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when `input` is longer than one call may parse, or when the
    /// sequences the parse produced fall outside the domains the alphabets declare.
    pub fn parse(&mut self, input: &[u8]) -> Result<&Sequences, Error> {
        if input.len() > MAX_PARSE_BYTES {
            return Err(Error::InvalidParameter);
        }
        self.make_room(input.len());
        let start = self.filled;
        let end = start.saturating_add(input.len());
        let Some(room) = self.window.get_mut(start..end) else {
            return Err(Error::InvalidParameter);
        };
        room.copy_from_slice(input);
        self.filled = end;

        let Self {
            finder,
            window,
            inserted,
            sequences,
            ..
        } = self;
        let Some(data) = window.get(..end) else {
            return Err(Error::InvalidParameter);
        };

        sequences.clear();
        let mut run_start = start;
        let mut at = start;
        while at < end {
            // Searching at a position requires every position before it to be in the chain,
            // which the positions a taken match covered are not yet.
            while *inserted < at {
                finder.insert(data, *inserted);
                *inserted = inserted.saturating_add(1);
            }
            let found = finder.step(data, at);
            *inserted = at.saturating_add(1);
            let Some(matched) = found.matched else {
                at = at.saturating_add(1);
                continue;
            };
            let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
            sequences.push(run, Some(matched))?;
            at = at.saturating_add(matched.length as usize);
            run_start = at;
        }
        if run_start < end {
            let run = data.get(run_start..end).ok_or(Error::InvalidParameter)?;
            sequences.push(run, None)?;
        }
        // The last positions of the block were offered to a chain that could not hash them,
        // because the bytes its hash reads were not there yet. The next call holds them, so
        // they are offered again rather than left out of the chain for good.
        *inserted = (*inserted).min(BoundedHashChain::insertable_end(end));
        Ok(sequences)
    }

    fn of(finder: BoundedHashChain) -> Self {
        Self {
            finder,
            window: vec![0u8; CAPACITY].into_boxed_slice(),
            filled: 0,
            inserted: 0,
            sequences: Sequences::with_capacity(MAX_PARSE_BYTES, step_capacity(MAX_PARSE_BYTES)),
        }
    }

    /// Makes room for `len` more bytes, sliding by exactly one window when the buffer is full.
    ///
    /// The shift is one window, so every stored position keeps its link slot and the finder
    /// moves with one pass over its own entries. A slide keeps at least one window less `len`
    /// bytes of history, and keeps the whole window when the caller feeds full blocks.
    fn make_room(&mut self, len: usize) {
        if self.filled.saturating_add(len) <= CAPACITY {
            return;
        }
        let window = WINDOW as usize;
        self.window.copy_within(window..self.filled, 0);
        self.filled = self.filled.saturating_sub(window);
        self.inserted = self.inserted.saturating_sub(window);
        self.finder.slide();
    }
}

/// The steps a block of `block_bytes` can hold.
///
/// Every step but the last carries a match, and a step that carries one covers its run and at
/// least the minimum match length, so a block holds at most one step per minimum match plus
/// the step a trailing literal run closes.
// The divisor is the minimum match length, a non-zero format constant, so the division is
// total.
#[allow(clippy::arithmetic_side_effects)]
const fn step_capacity(block_bytes: usize) -> usize {
    (block_bytes / MIN_MATCH as usize).saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::{CAPACITY, LOOKAHEAD_BYTES, MAX_PARSE_BYTES, Parser};
    use crate::block::expand;
    use crate::format::Error;
    use crate::matchfinder::BoundedHashChain;
    use crate::sequence::{MAX_MATCH_LENGTH, MIN_MATCH, Sequences, WINDOW};
    use crate::simd::ALL;

    const SEED: u64 = 0x5EED_0000_0000_0006;

    struct Source(u64);

    impl Source {
        fn byte(&mut self) -> u8 {
            self.0 = self
                .0
                .wrapping_mul(0x5851_F42D_4C95_7F2D)
                .wrapping_add(0x1405_7B7E_F767_814F);
            crate::entropy::low_byte(self.0 >> 24)
        }
    }

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut source = Source(seed);
        (0..len).map(|_| source.byte()).collect()
    }

    /// The shapes the round trip covers.
    #[derive(Clone, Copy, Debug)]
    enum Shape {
        Zeros,
        OneByte,
        Random,
        Repetitive,
        NearWindow,
        Incompressible,
    }

    const SHAPES: [Shape; 6] = [
        Shape::Zeros,
        Shape::OneByte,
        Shape::Random,
        Shape::Repetitive,
        Shape::NearWindow,
        Shape::Incompressible,
    ];

    impl Shape {
        const fn name(self) -> &'static str {
            match self {
                Self::Zeros => "zeros",
                Self::OneByte => "one-byte",
                Self::Random => "random",
                Self::Repetitive => "repetitive",
                Self::NearWindow => "near-window",
                Self::Incompressible => "incompressible",
            }
        }

        fn content(self, len: usize) -> Vec<u8> {
            match self {
                Self::Zeros => vec![0u8; len],
                Self::OneByte => vec![0x5Au8; len],
                Self::Random => noise(len, SEED),
                Self::Repetitive => {
                    let phrase = b"the same thirty-seven bytes, again!!!";
                    (0..len)
                        .map(|at| {
                            phrase
                                .get(at.checked_rem(phrase.len()).unwrap_or(0))
                                .copied()
                                .unwrap_or(b'?')
                        })
                        .collect()
                }
                // A block of content, one window of filler, and the same block again, so the
                // last part's matches sit at the window edge.
                Self::NearWindow => {
                    let window = WINDOW as usize;
                    let head = noise(window / 2, SEED ^ 0x31);
                    let filler = noise(window / 2, SEED ^ 0x32);
                    let mut data = Vec::with_capacity(len);
                    while data.len() < len {
                        data.extend_from_slice(&head);
                        data.extend_from_slice(&filler);
                        data.extend_from_slice(&head);
                    }
                    data.truncate(len);
                    data
                }
                // High entropy with no byte-level structure a finder can exploit, and a
                // different generator from `Random`, so the two are not one input twice.
                Self::Incompressible => (0..len)
                    .map(|at| {
                        let mixed = (at as u64)
                            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                            .rotate_left(29)
                            .wrapping_mul(0xBF58_476D_1CE4_E5B9);
                        crate::entropy::low_byte(mixed >> 33)
                    })
                    .collect(),
            }
        }
    }

    /// Parses `data` in `block`-byte calls and reconstructs it the way a decoder does.
    fn round_trip(parser: &mut Parser, data: &[u8], block: usize) -> Result<Vec<u8>, Error> {
        let mut out = vec![0u8; data.len()];
        let mut done = 0usize;
        while done < data.len() {
            let end = done.saturating_add(block).min(data.len());
            let chunk = data.get(done..end).unwrap_or_default();
            let sequences = parser.parse(chunk)?;
            assert_eq!(
                sequences.decoded_len(),
                chunk.len() as u64,
                "a block's sequences decode to the block"
            );
            let sequences = sequences.clone();
            let (history, room) = out.split_at_mut(done);
            let target = room.get_mut(..chunk.len()).ok_or(Error::InvalidParameter)?;
            expand(&sequences, history, target)?;
            done = end;
        }
        Ok(out)
    }

    /// Every match a sequence vector carries, checked against the domains and against the
    /// bytes the region has produced when the match is read.
    fn check_domains(sequences: &Sequences, produced_before: u64, name: &str) {
        let mut produced = produced_before;
        for step in sequences.steps() {
            produced = produced.saturating_add(u64::from(step.run));
            let Some(matched) = step.matched else {
                continue;
            };
            assert!(matched.length >= MIN_MATCH, "{name}");
            assert!(matched.length <= MAX_MATCH_LENGTH, "{name}");
            assert!(matched.distance >= 1, "{name}");
            assert!(matched.distance <= WINDOW, "{name}");
            assert!(u64::from(matched.distance) <= produced, "{name}");
            produced = produced.saturating_add(u64::from(matched.length));
        }
    }

    #[test]
    fn a_parse_reconstructs_every_shape_at_every_block_size() -> Result<(), Error> {
        let sizes = [0usize, 1, 3, 4, 255, 256, 4_096, 65_536, 65_537, 100_000];
        let blocks = [MAX_PARSE_BYTES, 4_096, 1_019];
        let mut checked = 0usize;
        for shape in SHAPES {
            for size in sizes {
                let data = shape.content(size);
                for block in blocks {
                    let mut parser = Parser::new();
                    let out = round_trip(&mut parser, &data, block)?;
                    assert_eq!(out, data, "{} at {size} in {block}", shape.name());
                    checked = checked.saturating_add(1);
                }
            }
        }
        assert_eq!(
            checked,
            SHAPES
                .len()
                .saturating_mul(sizes.len())
                .saturating_mul(blocks.len()),
            "a shape, a size or a block size was skipped"
        );
        Ok(())
    }

    #[test]
    fn every_match_a_parse_emits_is_inside_its_domain() -> Result<(), Error> {
        for shape in SHAPES {
            let data = shape.content(160_000);
            let mut parser = Parser::new();
            let mut produced = 0u64;
            for chunk in data.chunks(MAX_PARSE_BYTES) {
                let sequences = parser.parse(chunk)?;
                check_domains(sequences, produced, shape.name());
                produced = produced.saturating_add(sequences.decoded_len());
            }
        }
        Ok(())
    }

    #[test]
    fn an_empty_block_is_one_empty_sequence_vector() -> Result<(), Error> {
        let mut parser = Parser::new();
        let sequences = parser.parse(&[])?;
        assert_eq!(sequences.decoded_len(), 0);
        assert!(sequences.steps().is_empty());
        assert!(sequences.literals().is_empty());
        Ok(())
    }

    #[test]
    fn a_block_longer_than_one_call_may_parse_is_refused() {
        let mut parser = Parser::new();
        let data = vec![0u8; MAX_PARSE_BYTES.saturating_add(1)];
        assert_eq!(parser.parse(&data), Err(Error::InvalidParameter));
    }

    #[test]
    fn the_same_input_produces_the_same_sequences() -> Result<(), Error> {
        for shape in SHAPES {
            let data = shape.content(80_000);
            let first = parse_all(&data)?;
            let second = parse_all(&data)?;
            assert_eq!(first, second, "{}", shape.name());
        }
        Ok(())
    }

    #[test]
    fn a_reset_discards_the_history() -> Result<(), Error> {
        let data = Shape::Repetitive.content(120_000);
        let fresh = parse_all(&data)?;
        let mut parser = Parser::new();
        for chunk in Shape::Random.content(120_000).chunks(MAX_PARSE_BYTES) {
            let _ = parser.parse(chunk)?;
        }
        parser.reset();
        assert!(parser.history().is_empty());
        let mut after = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            after.push(parser.parse(chunk)?.clone());
        }
        assert_eq!(fresh, after, "a reset parser is a fresh parser");
        Ok(())
    }

    #[test]
    fn every_kernel_produces_the_same_sequences() -> Result<(), Error> {
        for shape in SHAPES {
            let data = shape.content(80_000);
            let mut expected: Option<Vec<Sequences>> = None;
            for &kernel in ALL {
                let mut parser = Parser::with_kernel(kernel);
                assert_eq!(parser.kernel(), kernel);
                let mut produced = Vec::new();
                for chunk in data.chunks(MAX_PARSE_BYTES) {
                    produced.push(parser.parse(chunk)?.clone());
                }
                match expected {
                    None => expected = Some(produced),
                    Some(ref first) => {
                        assert_eq!(&produced, first, "{} on {}", kernel.name(), shape.name());
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn the_declared_bound_states_what_the_parse_holds() {
        assert_eq!(CAPACITY, 196_608);
        assert_eq!(Parser::state_bytes(), 524_288);
        assert_eq!(
            Parser::state_bytes(),
            BoundedHashChain::state_bytes().saturating_add(CAPACITY)
        );
        assert_eq!(
            Parser::declared_bytes(0),
            Parser::state_bytes().saturating_add(16)
        );
        assert_eq!(Parser::declared_bytes(MAX_PARSE_BYTES), 851_984);
        assert_eq!(LOOKAHEAD_BYTES, MAX_MATCH_LENGTH as usize);
    }

    #[test]
    fn a_block_boundary_leaves_no_position_out_of_the_chain() -> Result<(), Error> {
        let mut parser = Parser::new();
        for size in [4_096usize, 1, 3, 65_536, 700] {
            let _ = parser.parse(&noise(size, SEED ^ 0x51))?;
            assert!(
                parser.inserted <= BoundedHashChain::insertable_end(parser.filled),
                "a position the chain could not hash was recorded as inserted"
            );
        }
        Ok(())
    }

    #[test]
    fn the_history_is_one_window_of_bytes_the_next_block_can_reach() -> Result<(), Error> {
        let window = WINDOW as usize;
        // Block lengths that divide the buffer evenly and ones that do not, because a slide
        // reached from a part-filled buffer is the case that loses history and a slide reached
        // from a full one is not.
        for block in [window, 40_000, 1_019] {
            let mut parser = Parser::new();
            let mut fed = 0usize;
            let mut slides = 0usize;
            // Fed until the slide has been reached twice, and no further: the input the check
            // needs is what it takes to slide, not a fixed volume.
            while slides < 2 && fed < CAPACITY.saturating_mul(4) {
                let promised = parser.history().to_vec();
                let before = parser.filled;
                let _ = parser.parse(&noise(block, SEED ^ 0x41))?;
                fed = fed.saturating_add(block);
                if parser.filled < before.saturating_add(block) {
                    slides = slides.saturating_add(1);
                }

                let history = parser.history();
                assert!(
                    history.len() <= window,
                    "a history of {} bytes at block {block}",
                    history.len()
                );
                assert_eq!(
                    history.len(),
                    fed.min(window),
                    "the history is short of the window at block {block}"
                );

                // Every byte the history named before this call is still in the buffer
                // directly before the block just parsed, which is where a match at that
                // block's first position reaches for it. A slide that dropped a promised byte
                // shortens or moves this run.
                let start = parser.filled.saturating_sub(block);
                let kept = parser
                    .window
                    .get(start.saturating_sub(promised.len())..start);
                assert_eq!(
                    kept,
                    Some(promised.as_slice()),
                    "a slide dropped history the next block was told it could reach, at block \
                     {block}"
                );
            }
            assert_eq!(slides, 2, "block {block} did not reach the slide twice");
        }
        Ok(())
    }

    fn parse_all(data: &[u8]) -> Result<Vec<Sequences>, Error> {
        let mut parser = Parser::new();
        let mut out = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            out.push(parser.parse(chunk)?.clone());
        }
        Ok(out)
    }
}

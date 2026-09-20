//! Owns parse strategy: which of the representations a match finder's proposals admit the
//! encoder emits.
//!
//! This module does not own match search or entropy coding. It decides what to emit.
//!
//! One strategy: greedy over a single-entry table with an adaptive skip. The parser searches
//! at the position it stands on, takes the match the table reports when it reaches the
//! minimum, and never revisits the decision. Consecutive searched misses advance the search
//! by `1 + (streak >> 6)`, so barren input costs few searches; a taken match and each block
//! start reset the streak. Skipped positions are never searched nor inserted, and the choice
//! between a match and a literal is the minimum match length. The reference chain at search
//! depth 8 stays available behind a constructor, and the FAST trade it lost is measured
//! elsewhere: shorter mean match length for an order of magnitude less search work.
//!
//! # What the parser holds
//!
//! The parser owns the window a match reaches into. The buffer is three windows wide, which is
//! what lets the slide that makes room move by exactly one window and still leave a whole
//! window of history behind it, whatever block length the caller feeds.
//!
//! ```text
//! single table                 65 536 bytes     the finder's
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
//! The same input, cut into the same blocks, produces the same sequences. The tag gate reads
//! the bytes the positions name and nothing else, the skip schedule reads the miss count and
//! nothing else, and no branch here reads an address.

use crate::format::Error;
use crate::matchfinder::{BoundedHashChain, CHAIN_DEPTH_BALANCED, SingleHash};
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

/// Which search structure a parse runs on.
///
/// FAST is the shipped path. The chain is the reference the FAST trade was measured against,
/// kept for comparison and never as a byte oracle for FAST output. BALANCED is the
/// production chain32 path behind the length-lazy parse.
enum Matcher {
    Fast(SingleHash),
    Chain(BoundedHashChain),
    Balanced(BoundedHashChain),
}

/// The shift the skip schedule ramps on: 64 searches per step.
const SKIP_SHIFT: u32 = 6;

/// The positions a miss streak skips over.
///
/// One for the first 64 consecutive searched misses, two for the next 64, and so on. The
/// schedule reads the streak and nothing else, which is what keeps the parse deterministic.
fn skip_jump(streak: u32) -> usize {
    1_usize.saturating_add(usize::try_from(streak >> SKIP_SHIFT).unwrap_or(usize::MAX))
}

/// A greedy parse over a single-entry table with an adaptive skip.
pub struct Parser {
    matcher: Matcher,
    window: Box<[u8]>,
    /// The bytes of `window` that hold input.
    filled: usize,
    /// The first position of `window` the chain has not been given. The FAST path inserts
    /// inline and leaves this alone.
    inserted: usize,
    /// Whether the BALANCED parse carries the FAST skip schedule. Always true in production;
    /// the test-only no-skip constructor clears it for the preservation comparison.
    balanced_skip: bool,
    /// The sequences of the block last parsed. Emptied and refilled, never reallocated.
    sequences: Sequences,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// A FAST parser with no history, on the kernel this build selected.
    #[must_use]
    pub fn new() -> Self {
        Self::of(Matcher::Fast(SingleHash::new()))
    }

    /// A FAST parser with no history, on a named kernel.
    #[must_use]
    pub fn with_kernel(kernel: Kernel) -> Self {
        Self::of(Matcher::Fast(SingleHash::with_kernel(kernel)))
    }

    /// A chain-driven parser with no history, on the kernel this build selected.
    ///
    /// The reference the FAST trade was measured against. It round-trips every input the
    /// FAST path does, and its bytes are not the FAST bytes: the skip changes the history a
    /// later search reads, by design.
    #[must_use]
    pub fn chain() -> Self {
        Self::of(Matcher::Chain(BoundedHashChain::new()))
    }

    /// A chain-driven parser with no history, on a named kernel.
    #[must_use]
    pub fn chain_with_kernel(kernel: Kernel) -> Self {
        Self::of(Matcher::Chain(BoundedHashChain::with_kernel(kernel)))
    }

    /// A BALANCED parser with no history, on the kernel this build selected.
    ///
    /// The production BALANCED matcher: the bounded hash chain at depth 32 behind the
    /// length-lazy depth-1 parse. The greedy chain reference stays behind `chain`.
    #[must_use]
    pub fn balanced() -> Self {
        Self::of(Matcher::Balanced(BoundedHashChain::with_depth(
            CHAIN_DEPTH_BALANCED,
        )))
    }

    /// A BALANCED parser with no history, on a named kernel.
    #[must_use]
    pub fn balanced_with_kernel(kernel: Kernel) -> Self {
        Self::of(Matcher::Balanced(BoundedHashChain::with_depth_and_kernel(
            CHAIN_DEPTH_BALANCED,
            kernel,
        )))
    }

    /// A BALANCED parser without the skip schedule, on the kernel this build selected.
    ///
    /// Test-only: the preservation comparison measures the production skip path against
    /// this no-skip path on the same inputs.
    #[cfg(test)]
    #[must_use]
    pub fn balanced_without_skip() -> Self {
        let mut parser = Self::of(Matcher::Balanced(BoundedHashChain::with_depth(
            CHAIN_DEPTH_BALANCED,
        )));
        parser.balanced_skip = false;
        parser
    }

    /// The kernel the matcher compares candidates with.
    #[must_use]
    pub const fn kernel(&self) -> Kernel {
        match &self.matcher {
            Matcher::Fast(matcher) => matcher.kernel(),
            Matcher::Chain(matcher) | Matcher::Balanced(matcher) => matcher.kernel(),
        }
    }

    /// The bytes the parser holds between calls, whatever the input is.
    #[must_use]
    pub const fn state_bytes() -> usize {
        SingleHash::state_bytes().saturating_add(CAPACITY)
    }

    /// The bytes a chain-driven parser holds between calls, whatever the input is.
    #[must_use]
    pub const fn chain_state_bytes() -> usize {
        BoundedHashChain::state_bytes().saturating_add(CAPACITY)
    }

    /// The bytes a BALANCED parser holds between calls, whatever the input is.
    ///
    /// The depth moves the walk bound only, so the figure equals the chain figure.
    #[must_use]
    pub const fn balanced_state_bytes() -> usize {
        Self::chain_state_bytes()
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

    /// The bytes a BALANCED parser allocates and holds, for a block of `block_bytes`.
    ///
    /// Same accounting as the FAST declaration, over the chain state the BALANCED
    /// matcher holds.
    #[must_use]
    pub const fn balanced_declared_bytes(block_bytes: usize) -> usize {
        Self::balanced_state_bytes()
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
        match &mut self.matcher {
            Matcher::Fast(matcher) => matcher.reset(),
            Matcher::Chain(matcher) | Matcher::Balanced(matcher) => matcher.reset(),
        }
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
            matcher,
            window,
            inserted,
            balanced_skip,
            sequences,
            ..
        } = self;
        let Some(data) = window.get(..end) else {
            return Err(Error::InvalidParameter);
        };

        match matcher {
            Matcher::Fast(matcher) => parse_fast(matcher, data, start, end, sequences)?,
            Matcher::Chain(matcher) => {
                parse_chain(matcher, data, start, end, inserted, sequences)?;
            }
            Matcher::Balanced(matcher) => {
                parse_balanced(
                    matcher,
                    data,
                    start,
                    end,
                    inserted,
                    *balanced_skip,
                    sequences,
                )?;
            }
        }
        Ok(sequences)
    }

    /// Makes room for `len` more bytes, sliding by exactly one window when the buffer is full.
    ///
    /// The shift is one window, so every stored position keeps its link slot and the finder
    /// moves with one pass over its own entries. A slide keeps at least one window less `len`
    /// bytes of history, and keeps the whole window when the caller feeds full blocks.
    fn of(matcher: Matcher) -> Self {
        Self {
            matcher,
            window: vec![0u8; CAPACITY].into_boxed_slice(),
            filled: 0,
            inserted: 0,
            balanced_skip: true,
            sequences: Sequences::with_capacity(MAX_PARSE_BYTES, step_capacity(MAX_PARSE_BYTES)),
        }
    }

    /// Makes room for `len` more bytes, sliding by exactly one window when the buffer is full.
    ///
    /// The shift is one window, so every stored position keeps its slot and the matcher
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
        match &mut self.matcher {
            Matcher::Fast(matcher) => matcher.slide(),
            Matcher::Chain(matcher) | Matcher::Balanced(matcher) => matcher.slide(),
        }
    }
}

/// The sequences of the FAST parse of the bytes the caller staged at `start..end`.
///
/// The skip schedule resets at each block start: a streak never crosses a block boundary,
/// which is what keeps two blocks of one input parsing the same in any order the stream
/// cuts them.
fn parse_fast(
    table: &mut SingleHash,
    data: &[u8],
    start: usize,
    end: usize,
    sequences: &mut Sequences,
) -> Result<(), Error> {
    sequences.clear();
    let mut run_start = start;
    let mut at = start;
    let mut streak = 0_u32;
    while at < end {
        let Some(matched) = table.search(data, at) else {
            streak = streak.saturating_add(1);
            at = at.saturating_add(skip_jump(streak));
            continue;
        };
        streak = 0;
        let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
        sequences.push(run, Some(matched))?;
        let from = at.saturating_add(1);
        at = at
            .saturating_add(usize::try_from(matched.length).unwrap_or(usize::MAX))
            .min(end);
        run_start = at;
        // The searched position entered the table in the search; the covered ones enter
        // here, as the chain's catch-up would have placed them. Skipped positions stay out.
        let mut covered = from;
        while covered < at {
            table.insert(data, covered);
            covered = covered.saturating_add(1);
        }
    }
    if run_start < end {
        let run = data.get(run_start..end).ok_or(Error::InvalidParameter)?;
        sequences.push(run, None)?;
    }
    Ok(())
}

/// The sequences of the BALANCED length-lazy depth-1 parse of the bytes at `start..end`.
///
/// At `p` with current best `cur`, the parse inserts `p`, reads the best candidate at
/// `p+1`, and delays one literal iff the next candidate is strictly longer. No score,
/// no threshold, no distance cost. After one delay the resulting candidate commits;
/// no second consecutive lazy step exists.
///
/// The insertion discipline matches the greedy chain catch-up: every position before a
/// search is inserted exactly once, covered positions of a taken match are inserted,
/// and tail positions the hash cannot read follow the `insertable_end` re-offer rule.
///
/// When `skip` holds, the parse carries the FAST schedule verbatim: consecutive searched
/// misses advance the search by `skip_jump(streak)`, a taken match and each block start
/// reset the streak, and skipped positions are never searched nor inserted. The schedule
/// reads the streak and nothing else, so the parse stays deterministic. When `skip` is
/// cleared the parse searches every position; only the test-only no-skip constructor
/// clears it, for the preservation comparison.
// The seven arguments are the block window every parse function takes, plus the skip
// switch this parse carries for its preservation comparison: the same shape as
// `parse_chain`, not a wider contract.
#[allow(clippy::too_many_arguments)]
fn parse_balanced(
    chain: &mut BoundedHashChain,
    data: &[u8],
    start: usize,
    end: usize,
    inserted: &mut usize,
    skip: bool,
    sequences: &mut Sequences,
) -> Result<(), Error> {
    sequences.clear();
    let mut run_start = start;
    let mut at = start;
    let mut streak = 0_u32;
    let mut delayed = false;
    let mut pending: Option<Option<crate::sequence::Match>> = None;
    while at < end {
        while *inserted < at {
            chain.insert(data, *inserted);
            *inserted = inserted.saturating_add(1);
        }
        let current = pending
            .take()
            .unwrap_or_else(|| chain.peek(data, at).matched);
        let Some(current) = current.filter(|found| found.length >= MIN_MATCH) else {
            chain.insert(data, at);
            *inserted = at.saturating_add(1);
            if skip {
                streak = streak.saturating_add(1);
                let jump = skip_jump(streak);
                at = at.saturating_add(jump).min(end);
                // Skipped positions stay out of the chain: the miss was searched and
                // inserted, the jump-over positions are neither.
                *inserted = at;
            } else {
                at = at.saturating_add(1);
            }
            delayed = false;
            continue;
        };
        if delayed {
            let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
            sequences.push(run, Some(current))?;
            chain.insert(data, at);
            *inserted = at.saturating_add(1);
            let take_end = at
                .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
                .min(end);
            while *inserted < take_end {
                chain.insert(data, *inserted);
                *inserted = inserted.saturating_add(1);
            }
            at = take_end;
            run_start = at;
            streak = 0;
            delayed = false;
            continue;
        }
        chain.insert(data, at);
        *inserted = at.saturating_add(1);
        let next = if at.saturating_add(1) < end {
            chain.peek(data, at.saturating_add(1)).matched
        } else {
            None
        };
        let Some(next) = next.filter(|found| found.length >= MIN_MATCH) else {
            let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
            sequences.push(run, Some(current))?;
            let take_end = at
                .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
                .min(end);
            while *inserted < take_end {
                chain.insert(data, *inserted);
                *inserted = inserted.saturating_add(1);
            }
            at = take_end;
            run_start = at;
            streak = 0;
            delayed = false;
            continue;
        };
        if next.length > current.length {
            at = at.saturating_add(1);
            pending = Some(Some(next));
            delayed = true;
            continue;
        }
        let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
        sequences.push(run, Some(current))?;
        let take_end = at
            .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
            .min(end);
        while *inserted < take_end {
            chain.insert(data, *inserted);
            *inserted = inserted.saturating_add(1);
        }
        at = take_end;
        run_start = at;
        streak = 0;
        delayed = false;
    }
    if run_start < end {
        let run = data.get(run_start..end).ok_or(Error::InvalidParameter)?;
        sequences.push(run, None)?;
    }
    *inserted = (*inserted).min(BoundedHashChain::insertable_end(end));
    Ok(())
}

/// The sequences of the reference chain parse of the bytes at `start..end`.
fn parse_chain(
    chain: &mut BoundedHashChain,
    data: &[u8],
    start: usize,
    end: usize,
    inserted: &mut usize,
    sequences: &mut Sequences,
) -> Result<(), Error> {
    sequences.clear();
    let mut run_start = start;
    let mut at = start;
    while at < end {
        // Searching at a position requires every position before it to be in the chain,
        // which the positions a taken match covered are not yet.
        while *inserted < at {
            chain.insert(data, *inserted);
            *inserted = inserted.saturating_add(1);
        }
        let found = chain.step(data, at);
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
    Ok(())
}

/// The steps a block of `block_bytes` can hold.
///
/// Every step but the last carries a match, and a step that carries one covers its run and at
/// least the minimum match length, so a block holds at most one step per minimum match plus
/// the step a trailing literal run closes.
// The divisor is the minimum match length, a non-zero format constant, so the division is
// total. The per-block transient sizes one vector per step, so the emitter reads this with it.
#[allow(clippy::arithmetic_side_effects)]
pub(crate) const fn step_capacity(block_bytes: usize) -> usize {
    (block_bytes / MIN_MATCH as usize).saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::{CAPACITY, LOOKAHEAD_BYTES, MAX_PARSE_BYTES, Parser, skip_jump};
    use crate::block::expand;
    use crate::format::Error;
    use crate::matchfinder::{BoundedHashChain, CHAIN_DEPTH_BALANCED, SingleHash};
    use crate::sequence::{MAX_MATCH_LENGTH, MIN_MATCH, Match, Sequences, WINDOW};
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
        // The FAST matcher moved the parser from the chain's five bytes per window byte to
        // one: the single table holds 65 536 bytes where the head table and link array held
        // 327 680.
        assert_eq!(CAPACITY, 196_608);
        assert_eq!(Parser::state_bytes(), 262_144);
        assert_eq!(
            Parser::state_bytes(),
            SingleHash::state_bytes().saturating_add(CAPACITY)
        );
        assert_eq!(
            Parser::declared_bytes(0),
            Parser::state_bytes().saturating_add(16)
        );
        assert_eq!(Parser::declared_bytes(MAX_PARSE_BYTES), 589_840);
        assert_eq!(Parser::chain_state_bytes(), 524_288);
        assert_eq!(Parser::balanced_state_bytes(), 524_288);
        assert_eq!(Parser::balanced_declared_bytes(MAX_PARSE_BYTES), 851_984);
        assert_eq!(LOOKAHEAD_BYTES, MAX_MATCH_LENGTH as usize);
    }

    #[test]
    fn the_skip_schedule_ramps_one_step_per_sixty_four_misses() {
        for streak in [0_u32, 1, 63] {
            assert_eq!(skip_jump(streak), 1, "streak {streak}");
        }
        for streak in [64_u32, 65, 127] {
            assert_eq!(skip_jump(streak), 2, "streak {streak}");
        }
        assert_eq!(skip_jump(128), 3);
        // The largest jump the investigation measured on incompressible input.
        assert_eq!(skip_jump(2_816), 45);
        assert_eq!(skip_jump(u32::MAX), 67_108_864);
    }

    #[test]
    fn a_block_boundary_leaves_no_position_out_of_the_chain() -> Result<(), Error> {
        // The counter lives on the chain path only; the FAST path inserts inline and keeps
        // no bookkeeping for a boundary to rewind.
        let mut parser = Parser::chain();
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

    #[test]
    fn the_chain_path_round_trips_and_is_deterministic() -> Result<(), Error> {
        // The reference stays available and tested, but its bytes are not FAST bytes: the
        // skip changes the history a later search reads, so this asserts the reference
        // against itself and against the decoded content, never against the FAST path.
        for shape in SHAPES {
            let data = shape.content(100_000);
            let mut parser = Parser::chain();
            let out = round_trip(&mut parser, &data, MAX_PARSE_BYTES)?;
            assert_eq!(out, data, "chain round trip on {}", shape.name());
            let first = parse_all_chain(&data)?;
            let second = parse_all_chain(&data)?;
            assert_eq!(first, second, "chain determinism on {}", shape.name());
        }
        Ok(())
    }

    fn parse_all_chain(data: &[u8]) -> Result<Vec<Sequences>, Error> {
        let mut parser = Parser::chain();
        let mut out = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            out.push(parser.parse(chunk)?.clone());
        }
        Ok(out)
    }

    #[test]
    fn an_exact_repeat_at_the_window_edge_stays_valid() -> Result<(), Error> {
        let window = WINDOW as usize;
        let head = noise(32_768, SEED ^ 0x61);
        let mut first = head.clone();
        first.extend_from_slice(&noise(window.saturating_sub(head.len()), SEED ^ 0x62));
        assert_eq!(first.len(), window);
        let mut parser = Parser::new();
        let _ = parser.parse(&first)?.clone();
        // The second block repeats the first exactly at the farthest distance the window
        // admits. The FAST table prefers near candidates, so the match structure fragments;
        // what is pinned here is validity, not coverage.
        let second = parser.parse(&first)?.clone();
        for step in second.steps() {
            if let Some(found) = step.matched {
                assert!(found.distance >= 1, "a match reported itself");
                assert!(found.distance <= WINDOW, "a match reached past the window");
            }
        }
        assert_eq!(
            second.decoded_len(),
            u64::try_from(window).unwrap_or(0),
            "the far repeat decoded short"
        );
        let mut out = vec![0_u8; window];
        expand(&second, &first, &mut out)?;
        assert_eq!(out, first, "the far repeat did not expand to its content");
        Ok(())
    }

    #[test]
    fn tiny_and_rle_blocks_parse_to_their_content() -> Result<(), Error> {
        for len in [1_usize, 2, 3, 4, 7, 63, 64] {
            for byte in [0_u8, 0xA5] {
                let content = vec![byte; len];
                let mut parser = Parser::new();
                let sequences = parser.parse(&content)?.clone();
                assert_eq!(
                    sequences.decoded_len(),
                    u64::try_from(len).unwrap_or(0),
                    "decoded length at {len}"
                );
                let mut out = vec![0_u8; len];
                expand(&sequences, &[], &mut out)?;
                assert_eq!(out, content, "tiny RLE content at {len}");
            }
            let mixed: Vec<u8> = (0_usize..len)
                .map(|at| {
                    u8::try_from(at.saturating_mul(31).checked_rem(251).unwrap_or(0)).unwrap_or(0)
                })
                .collect();
            let mut parser = Parser::new();
            let sequences = parser.parse(&mixed)?.clone();
            let mut out = vec![0_u8; len];
            expand(&sequences, &[], &mut out)?;
            assert_eq!(out, mixed, "tiny mixed content at {len}");
        }
        Ok(())
    }

    #[test]
    fn fast_parse_round_trips_encoder_workload_classes() -> Result<(), Error> {
        // The shapes the FAST ratio trade was measured over, in the parser's own terms:
        // structured text at several entropies, sparse content, and short tokens.
        let phrase = b"the quick brown fox jumps over the lazy dog. ";
        let text: Vec<u8> = (0_usize..65_536_usize)
            .map(|at| {
                phrase
                    .get(at.checked_rem(phrase.len()).unwrap_or(0))
                    .copied()
                    .unwrap_or(b' ')
            })
            .collect();
        let json: Vec<u8> = (0_usize..65_536_usize)
            .map(|at| {
                b"{\"key\": 0123456789, \"v\": true} "
                    .get(at.checked_rem(32).unwrap_or(0))
                    .copied()
                    .unwrap_or(b' ')
            })
            .collect();
        let sparse: Vec<u8> = (0_usize..65_536_usize)
            .map(|at| {
                if at.checked_rem(64) == Some(0) {
                    let mixed = (u64::try_from(at).unwrap_or(0))
                        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        .rotate_left(29)
                        .wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    crate::entropy::low_byte(mixed >> 33)
                } else {
                    0
                }
            })
            .collect();
        for (name, data) in [("text", text), ("json", json), ("sparse", sparse)] {
            let mut parser = Parser::new();
            let out = round_trip(&mut parser, &data, MAX_PARSE_BYTES)?;
            assert_eq!(out, data, "{name}");
        }
        Ok(())
    }

    #[test]
    fn the_balanced_parser_round_trips_and_is_deterministic() -> Result<(), Error> {
        for shape in SHAPES {
            let data = shape.content(100_000);
            let mut parser = Parser::balanced();
            let out = round_trip(&mut parser, &data, MAX_PARSE_BYTES)?;
            assert_eq!(out, data, "balanced round trip on {}", shape.name());
            let first = parse_all_balanced(&data)?;
            let second = parse_all_balanced(&data)?;
            assert_eq!(first, second, "balanced determinism on {}", shape.name());
        }
        Ok(())
    }

    fn parse_all_balanced(data: &[u8]) -> Result<Vec<Sequences>, Error> {
        let mut parser = Parser::balanced();
        let mut out = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            out.push(parser.parse(chunk)?.clone());
        }
        Ok(out)
    }

    #[test]
    fn the_balanced_parser_keeps_block_continuity() -> Result<(), Error> {
        let data = Shape::Repetitive.content(100_000);
        let mut whole = Parser::balanced();
        let mut split = Parser::balanced();
        let mut first_out = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            first_out.push(whole.parse(chunk)?.clone());
        }
        let mut second_out = Vec::new();
        for chunk in data.chunks(1_019) {
            let _ = split.parse(chunk)?.clone();
        }
        split.reset();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            second_out.push(split.parse(chunk)?.clone());
        }
        assert_eq!(
            first_out, second_out,
            "a reset balanced parser is a fresh one"
        );
        let _ = second_out;
        Ok(())
    }

    #[test]
    fn every_kernel_produces_the_same_balanced_sequences() -> Result<(), Error> {
        for shape in SHAPES {
            let data = shape.content(40_000);
            let mut expected: Option<Vec<Sequences>> = None;
            for &kernel in ALL {
                let mut parser = Parser::balanced_with_kernel(kernel);
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
    fn a_balanced_block_boundary_leaves_no_position_out_of_the_chain() -> Result<(), Error> {
        let mut parser = Parser::balanced();
        for size in [4_096usize, 1, 3, 65_536, 700] {
            let _ = parser.parse(&noise(size, SEED ^ 0x51))?;
            assert!(
                parser.inserted <= BoundedHashChain::insertable_end(parser.filled),
                "a position the chain could not hash was recorded as inserted"
            );
        }
        Ok(())
    }

    /// One lazy delay the oracle recorded.
    struct Delay {
        at: usize,
        cur_len: u32,
        cur_dist: u32,
        nxt_len: u32,
        nxt_dist: u32,
    }

    /// An independent length-lazy depth-1 parse over `data`, recording every delay.
    ///
    /// Uses the same chain API and the same strict-longer rule as the production
    /// BALANCED parse, but written separately so the production parse is checked
    /// against it rather than against itself.
    #[allow(clippy::arithmetic_side_effects)]
    fn lazy_oracle(data: &[u8]) -> Result<(Sequences, Vec<Delay>), Error> {
        let mut chain = BoundedHashChain::with_depth(CHAIN_DEPTH_BALANCED);
        let mut sequences = Sequences::with_capacity(data.len(), data.len() / 4 + 1);
        let mut delays: Vec<Delay> = Vec::new();
        let end = data.len();
        let mut run_start = 0usize;
        let mut at = 0usize;
        let mut inserted = 0usize;
        let mut delayed = false;
        let mut pending: Option<Option<Match>> = None;
        while at < end {
            while inserted < at {
                chain.insert(data, inserted);
                inserted = inserted.saturating_add(1);
            }
            let current = pending
                .take()
                .unwrap_or_else(|| chain.peek(data, at).matched);
            let Some(current) = current.filter(|found| found.length >= MIN_MATCH) else {
                chain.insert(data, at);
                inserted = at.saturating_add(1);
                at = at.saturating_add(1);
                delayed = false;
                continue;
            };
            if delayed {
                let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
                sequences.push(run, Some(current))?;
                chain.insert(data, at);
                inserted = at.saturating_add(1);
                let take_end = at
                    .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
                    .min(end);
                while inserted < take_end {
                    chain.insert(data, inserted);
                    inserted = inserted.saturating_add(1);
                }
                at = take_end;
                run_start = at;
                delayed = false;
                continue;
            }
            chain.insert(data, at);
            inserted = at.saturating_add(1);
            let next = if at.saturating_add(1) < end {
                chain.peek(data, at.saturating_add(1)).matched
            } else {
                None
            };
            let Some(next) = next.filter(|found| found.length >= MIN_MATCH) else {
                let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
                sequences.push(run, Some(current))?;
                let take_end = at
                    .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
                    .min(end);
                while inserted < take_end {
                    chain.insert(data, inserted);
                    inserted = inserted.saturating_add(1);
                }
                at = take_end;
                run_start = at;
                delayed = false;
                continue;
            };
            if next.length > current.length {
                assert!(!delayed, "a second consecutive delay at {at}");
                delays.push(Delay {
                    at,
                    cur_len: current.length,
                    cur_dist: current.distance,
                    nxt_len: next.length,
                    nxt_dist: next.distance,
                });
                at = at.saturating_add(1);
                pending = Some(Some(next));
                delayed = true;
                continue;
            }
            let run = data.get(run_start..at).ok_or(Error::InvalidParameter)?;
            sequences.push(run, Some(current))?;
            let take_end = at
                .saturating_add(usize::try_from(current.length).unwrap_or(usize::MAX))
                .min(end);
            while inserted < take_end {
                chain.insert(data, inserted);
                inserted = inserted.saturating_add(1);
            }
            at = take_end;
            run_start = at;
            delayed = false;
        }
        if run_start < end {
            let run = data.get(run_start..end).ok_or(Error::InvalidParameter)?;
            sequences.push(run, None)?;
        }
        Ok((sequences, delays))
    }

    /// Where the corpus cache sits, named the same way the repository tooling names it.
    fn corpus_cache() -> std::path::PathBuf {
        std::env::var("CORPUS")
            .map_or_else(
                |_| {
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("..")
                        .join("..")
                        .join("corpus")
                },
                std::path::PathBuf::from,
            )
            .join("cache")
    }

    /// The M27 frozen 64 KiB prefixes with the lenlazy1 replacement counts the
    /// investigation sealed: per-position delay decisions, not just final bytes.
    const ORACLE_PREFIXES: [(&str, usize, u64); 6] = [
        ("project-source-medium.rs", 65_536, 16),
        ("project-logs-medium.log", 65_536, 1_094),
        ("project-json-medium.json", 65_536, 796),
        ("project-database-rows-medium.tsv", 65_536, 1_260),
        ("project-long-repetitions-medium.bin", 65_536, 0),
        ("project-serialized-binary-medium.bin", 65_536, 465),
    ];

    #[test]
    fn the_lazy_decisions_match_the_m27_log() -> Result<(), Error> {
        let cache = corpus_cache();
        if !cache.is_dir() {
            println!(
                "lazy-oracle: {} does not hold the corpus, so nothing was measured",
                cache.display()
            );
            return Ok(());
        }
        for (name, len, expected) in ORACLE_PREFIXES {
            let path = cache.join(name);
            let Ok(whole) = std::fs::read(&path) else {
                println!(
                    "lazy-oracle: {} is not in the corpus, so nothing was measured",
                    path.display()
                );
                return Ok(());
            };
            let data = whole
                .get(..len.min(whole.len()))
                .ok_or(Error::InvalidParameter)?;
            assert_eq!(data.len(), len, "{name} holds {} bytes", whole.len());
            let (oracle_sequences, delays) = lazy_oracle(data)?;
            assert_eq!(
                u64::try_from(delays.len()).unwrap_or(u64::MAX),
                expected,
                "{name} replacement count moved"
            );
            for delay in &delays {
                assert!(
                    delay.nxt_len > delay.cur_len,
                    "{name} delayed at {} without a strictly longer next",
                    delay.at
                );
                assert!(
                    delay.cur_dist >= 1
                        && delay.cur_dist <= WINDOW
                        && delay.nxt_dist >= 1
                        && delay.nxt_dist <= WINDOW,
                    "{name} delay at {} names a distance outside the window",
                    delay.at
                );
            }
            let mut previous: Option<usize> = None;
            for delay in &delays {
                if let Some(prev) = previous {
                    assert!(
                        delay.at > prev.saturating_add(1),
                        "{name} delayed twice in a row at {prev} and {}",
                        delay.at
                    );
                }
                previous = Some(delay.at);
            }
            let mut parser = Parser::balanced_without_skip();
            let parsed = parser.parse(data)?.clone();
            assert_eq!(
                parsed, oracle_sequences,
                "{name} production parse differs from the oracle"
            );
            let mut out = vec![0u8; data.len()];
            expand(&parsed, &[], &mut out)?;
            assert_eq!(
                out, data,
                "{name} lazy output did not expand to its content"
            );
        }
        Ok(())
    }

    #[test]
    fn the_lazy_oracle_never_delays_twice_on_adversarial_shapes() -> Result<(), Error> {
        let mut shapes: Vec<(&str, Vec<u8>)> = vec![
            ("zeros", vec![0u8; 16_384]),
            ("one-byte", vec![0x5Au8; 16_384]),
            (
                "periodic",
                (0..16_384usize)
                    .map(|at| crate::entropy::low_byte(u64::try_from(at % 7).unwrap_or(0)))
                    .collect(),
            ),
            ("noise", noise(16_384, SEED)),
        ];
        shapes.push(("repetitive", Shape::Repetitive.content(16_384)));
        for (name, data) in shapes {
            let (sequences, delays) = lazy_oracle(&data)?;
            for delay in &delays {
                assert!(
                    delay.nxt_len > delay.cur_len,
                    "{name} delayed at {} on equal lengths",
                    delay.at
                );
            }
            let mut parser = Parser::balanced_without_skip();
            let parsed = parser.parse(&data)?.clone();
            assert_eq!(
                parsed, sequences,
                "{name} production parse differs from the oracle"
            );
            let mut out = vec![0u8; data.len()];
            expand(&parsed, &[], &mut out)?;
            assert_eq!(out, data, "{name} did not expand to its content");
        }
        Ok(())
    }

    #[test]
    fn equal_lengths_never_delay() -> Result<(), Error> {
        let mut equal = vec![b'.'; 64];
        for start in [0usize, 8, 16] {
            for (offset, byte) in b"WXYZAB".iter().enumerate() {
                if let Some(slot) = equal.get_mut(start.saturating_add(offset)) {
                    *slot = *byte;
                }
            }
        }
        let (_, delays) = lazy_oracle(&equal)?;
        for delay in &delays {
            assert!(
                delay.nxt_len > delay.cur_len,
                "equal lengths delayed at {}",
                delay.at
            );
        }
        let mut parser = Parser::balanced_without_skip();
        let parsed = parser.parse(&equal)?.clone();
        let (oracle_sequences, _) = lazy_oracle(&equal)?;
        assert_eq!(parsed, oracle_sequences, "equal-length tie diverged");
        Ok(())
    }

    /// Parses `data` in one call per block through the production skip path.
    fn parse_all_balanced_skip(data: &[u8]) -> Result<Vec<Sequences>, Error> {
        let mut parser = Parser::balanced();
        let mut out = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            out.push(parser.parse(chunk)?.clone());
        }
        Ok(out)
    }

    /// Parses `data` in one call per block through the test-only no-skip path.
    fn parse_all_balanced_no_skip(data: &[u8]) -> Result<Vec<Sequences>, Error> {
        let mut parser = Parser::balanced_without_skip();
        let mut out = Vec::new();
        for chunk in data.chunks(MAX_PARSE_BYTES) {
            out.push(parser.parse(chunk)?.clone());
        }
        Ok(out)
    }

    /// The first position where the skip and no-skip parses part ways.
    ///
    /// Answers `None` when every block agrees. Otherwise names the block, the step,
    /// the input position, and the expected vs produced run and match, so a structured
    /// divergence classifies before anything changes.
    fn skip_divergence(expected: &[Sequences], produced: &[Sequences]) -> Option<String> {
        if expected.len() != produced.len() {
            return Some(format!(
                "block count differs: no-skip {} vs skip {}",
                expected.len(),
                produced.len()
            ));
        }
        for (block, (want, got)) in expected.iter().zip(produced.iter()).enumerate() {
            if want.steps().len() != got.steps().len() {
                return Some(format!(
                    "block {block}: step count differs: no-skip {} vs skip {}",
                    want.steps().len(),
                    got.steps().len()
                ));
            }
            let mut position = block.saturating_mul(MAX_PARSE_BYTES);
            for (step, (want, got)) in want.steps().iter().zip(got.steps().iter()).enumerate() {
                if want == got {
                    position =
                        position.saturating_add(usize::try_from(want.run).unwrap_or(usize::MAX));
                    if let Some(matched) = want.matched {
                        position = position
                            .saturating_add(usize::try_from(matched.length).unwrap_or(usize::MAX));
                    }
                    continue;
                }
                return Some(format!(
                    "block {block} step {step} at {position}: no-skip run {} len {} dist {} \
                     vs skip run {} len {} dist {}",
                    want.run,
                    want.matched.map_or(0, |matched| matched.length),
                    want.matched.map_or(0, |matched| matched.distance),
                    got.run,
                    got.matched.map_or(0, |matched| matched.length),
                    got.matched.map_or(0, |matched| matched.distance),
                ));
            }
            if want.literals() != got.literals() {
                return Some(format!(
                    "block {block}: literal bytes differ ({} vs {} bytes)",
                    want.literals().len(),
                    got.literals().len()
                ));
            }
        }
        None
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn balanced_fast_skip_sequences_hold_the_accepted_drift() -> Result<(), Error> {
        // The FAST schedule carried verbatim moves the structured operating point by
        // the accepted, bounded amount: identical sequence decisions where the skip
        // stays inert, and exactly the documented first divergence where coverage
        // loss steps over a match start. A new divergent fixture, or a moved first
        // divergence, is a gate failure, not a silent absorption.
        //
        // The byte cost is pinned by the BALANCED byte oracle; what is pinned here
        // is where the decisions drift, including database-rows, whose block bytes
        // keep byte identity while the sequences part ways.
        // The accepted first divergences, no-skip vs skip, per fixture. `None`
        // means the skip is inert there and the sequences must agree exactly.
        const EXPECTED: [(&str, usize, Option<&str>); 8] = [
            ("project-source-medium.rs", 65_536, None),
            (
                "project-logs-medium.log",
                65_536,
                Some(
                    "block 0 step 0 at 0: no-skip run 122 len 6 dist 122 vs skip run 123 len 5 dist 122",
                ),
            ),
            ("project-json-medium.json", 65_536, None),
            (
                "project-database-rows-medium.tsv",
                65_536,
                Some(
                    "block 0 step 0 at 0: no-skip run 90 len 7 dist 70 vs skip run 91 len 6 dist 70",
                ),
            ),
            (
                "project-long-repetitions-medium.bin",
                65_536,
                Some(
                    "block 0 step 0 at 0: no-skip run 4096 len 256 dist 4096 vs skip run 4102 len 256 dist 4096",
                ),
            ),
            (
                "project-serialized-binary-medium.bin",
                65_536,
                Some("block 0: step count differs: no-skip 1943 vs skip 1935"),
            ),
            ("project-json-medium.json", 262_144, None),
            (
                "project-database-rows-medium.tsv",
                262_144,
                Some(
                    "block 0 step 0 at 0: no-skip run 90 len 7 dist 70 vs skip run 91 len 6 dist 70",
                ),
            ),
        ];
        let cache = corpus_cache();
        if !cache.is_dir() {
            println!(
                "skip-preservation: {} does not hold the corpus, so nothing was measured",
                cache.display()
            );
            return Ok(());
        }
        for (name, len, expected) in EXPECTED {
            let path = cache.join(name);
            let Ok(whole) = std::fs::read(&path) else {
                println!(
                    "skip-preservation: {} is not in the corpus, so nothing was measured",
                    path.display()
                );
                return Ok(());
            };
            let data = whole
                .get(..len.min(whole.len()))
                .ok_or(Error::InvalidParameter)?;
            assert_eq!(data.len(), len, "{name} holds {} bytes", whole.len());
            let unskipped = parse_all_balanced_no_skip(data)?;
            let skipped = parse_all_balanced_skip(data)?;
            let divergence = skip_divergence(&unskipped, &skipped);
            match expected {
                None => assert!(
                    divergence.is_none(),
                    "{name} ({len} bytes): a new skip divergence appeared: {}",
                    divergence.unwrap_or_default()
                ),
                Some(want) => assert_eq!(
                    divergence.as_deref(),
                    Some(want),
                    "{name} ({len} bytes): the accepted drift moved or healed"
                ),
            }
            if expected.is_none() {
                println!(
                    "skip-preservation: {name} {len} identical in {} blocks",
                    skipped.len()
                );
            }
        }
        // Determinism of the pinned path itself: the same input cut into the
        // same blocks produces the same skip sequences on every run.
        for (name, data) in [
            ("zeros", vec![0u8; 65_536]),
            ("one-byte", vec![0x5Au8; 65_536]),
            (
                "periodic",
                (0..65_536usize)
                    .map(|at| crate::entropy::low_byte(u64::try_from(at % 7).unwrap_or(0)))
                    .collect::<Vec<u8>>(),
            ),
            ("noise", noise(65_536, SEED)),
            ("repetitive", Shape::Repetitive.content(65_536)),
            ("incompressible", Shape::Incompressible.content(65_536)),
        ] {
            let first = parse_all_balanced_skip(&data)?;
            let second = parse_all_balanced_skip(&data)?;
            assert_eq!(first, second, "{name}: the skip parse is not deterministic");
            let mut out = vec![0u8; data.len()];
            let mut done = 0_usize;
            for sequences in &first {
                let len = usize::try_from(sequences.decoded_len()).unwrap_or(usize::MAX);
                let (history, room) = out.split_at_mut(done);
                let target = room.get_mut(..len).ok_or(Error::InvalidParameter)?;
                expand(sequences, history, target)?;
                done = done.saturating_add(len);
            }
            assert_eq!(
                out, data,
                "{name}: the skip parse did not expand to its content"
            );
        }
        Ok(())
    }

    /// Samples per arm. The repository trusts a timing whose spread against the median stays
    /// inside 0.15; the spread is printed here and the release run of this test is what the
    /// RAW gate records, because a debug build's absolute figures are not a throughput.
    const SAMPLES: usize = 7;

    /// The spread of one timing series: slowest minus fastest over the median.
    #[allow(clippy::arithmetic_side_effects, clippy::cast_precision_loss)]
    fn spread(fastest: u128, slowest: u128, median: u128) -> f64 {
        slowest.saturating_sub(fastest) as f64 / median.max(1) as f64
    }

    /// One throughput figure against another, for the printed record.
    #[allow(clippy::cast_precision_loss)]
    fn ratio(baseline: u64, measured: u64) -> f64 {
        baseline as f64 / measured.max(1) as f64
    }

    /// One arm's timing: its median in picoseconds per byte and its spread.
    type Timing = (u64, f64);

    /// The median and the spread of one timing series, printed under its label.
    #[allow(clippy::arithmetic_side_effects)]
    fn summarize(series: &mut [u128], label: &str) -> Timing {
        series.sort_unstable();
        let median = series.get(series.len() / 2).copied().unwrap_or(u128::MAX);
        let fastest = series.first().copied().unwrap_or(0);
        let slowest = series.last().copied().unwrap_or(0);
        let spread = spread(fastest, slowest, median);
        println!(
            "raw-gate parse {label}: median {median} ps/B, min {fastest}, max {slowest}, \
             spread {spread:.3}",
        );
        (u64::try_from(median).unwrap_or(u64::MAX), spread)
    }

    /// Both arms' parse cost over one input, as integer picoseconds per input byte.
    ///
    /// Every sample is one complete parse of the input, cut into whole blocks exactly as the
    /// encoder cuts it, on a fresh parser so setup is included as the caller pays it. The
    /// arms alternate inside the sample loop, so host drift lands on both rather than on one.
    ///
    /// Returns `(FAST_SKIP, NO_SKIP)` as one `Timing` each.
    #[allow(clippy::arithmetic_side_effects)]
    fn timed_pair(data: &[u8]) -> Result<(Timing, Timing), Error> {
        let mut skip_series = Vec::with_capacity(SAMPLES);
        let mut no_skip_series = Vec::with_capacity(SAMPLES);
        let bytes = u128::try_from(data.len()).map_err(|_| Error::InvalidParameter)?;
        // One untimed pass per arm first: the pages, the branch predictors, and
        // the allocator caches warm once, then the timed samples read steady
        // state rather than a first-run fault.
        for skip in [true, false] {
            let mut parser = if skip {
                Parser::balanced()
            } else {
                Parser::balanced_without_skip()
            };
            for chunk in data.chunks(MAX_PARSE_BYTES) {
                let _ = parser.parse(chunk)?;
            }
        }
        for sample in 0..SAMPLES {
            let order = if sample % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            };
            for skip in order {
                let mut parser = if skip {
                    Parser::balanced()
                } else {
                    Parser::balanced_without_skip()
                };
                let started = std::time::Instant::now();
                for chunk in data.chunks(MAX_PARSE_BYTES) {
                    let _ = parser.parse(chunk)?;
                }
                // Picoseconds per byte, so a release build's sub-nanosecond
                // figure keeps its resolution instead of flooring to zero.
                let per_byte = started.elapsed().as_nanos().saturating_mul(1_000) / bytes;
                if skip {
                    skip_series.push(per_byte);
                } else {
                    no_skip_series.push(per_byte);
                }
            }
        }
        let skip = summarize(&mut skip_series, "FAST_SKIP");
        let no_skip = summarize(&mut no_skip_series, "NO_SKIP");
        Ok((skip, no_skip))
    }

    /// The RAW CPU gate of the accepted skip: the production `FAST_SKIP` parse against the
    /// test-only no-skip arm on incompressible input.
    ///
    /// The gate names high-entropy and already-compressed inputs at 64 KiB and 1 MiB. Both
    /// arms run the production chain32 + length-lazy1 parse on the same bytes; the only
    /// difference is the skip schedule. The recorded no-skip chain32 baseline this must
    /// erase was 17.5 to 29 ns/B end to end (M27 Stage J); the gate requires a factor of at
    /// least ten, with the environment and the spread recorded.
    #[test]
    fn balanced_fast_skip_recovers_the_raw_parse_cost() -> Result<(), Error> {
        const RAW: [(&str, usize); 4] = [
            ("project-high-entropy-medium.bin", 65_536),
            ("project-already-compressed-medium.bin", 65_536),
            ("project-high-entropy-medium.bin", 1_048_576),
            ("project-already-compressed-medium.bin", 1_048_576),
        ];
        let cache = corpus_cache();
        if !cache.is_dir() {
            println!(
                "raw-gate: {} does not hold the corpus, so nothing was measured",
                cache.display()
            );
            return Ok(());
        }
        for (name, len) in RAW {
            let path = cache.join(name);
            let Ok(whole) = std::fs::read(&path) else {
                println!(
                    "raw-gate: {} is not in the corpus, so nothing was measured",
                    path.display()
                );
                return Ok(());
            };
            let data = whole
                .get(..len.min(whole.len()))
                .ok_or(Error::InvalidParameter)?;
            assert_eq!(data.len(), len, "{name} holds {} bytes", whole.len());
            let ((skip_ps, skip_spread), (no_skip_ps, no_skip_spread)) = timed_pair(data)?;
            println!(
                "raw-gate {name} {len}: FAST_SKIP {skip_ps} ps/B (spread {skip_spread:.3}) vs \
                 NO_SKIP {no_skip_ps} ps/B (spread {no_skip_spread:.3}), ratio {:.1}x",
                ratio(no_skip_ps, skip_ps),
            );
            assert!(
                skip_ps.saturating_mul(10) <= no_skip_ps,
                "{name} {len}: FAST_SKIP is {skip_ps} ps/B against the no-skip {no_skip_ps}, \
                 short of the tenfold the RAW gate requires"
            );
        }
        Ok(())
    }
}

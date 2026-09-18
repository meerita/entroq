//! Owns match finding: the search structures that propose matches and the window invariants
//! they maintain.
//!
//! This module does not own which proposed match the encoder takes. The parser owns that.
//!
//! One structure: a bounded hash chain at search depth 8. A head table maps the hash of four
//! bytes to the most recent position that produced it, and a link array, one entry per window
//! byte, joins each position to the previous position with the same hash. A search walks that
//! chain from the head, bounded by the depth, by the window, and by the link array's own
//! aliasing.
//!
//! ```text
//! head table    16 384 entries of 4 bytes      one entry per four window bytes
//! link array    65 536 entries of 4 bytes      one entry per window byte
//! total                                        5 bytes of state per window byte
//! ```
//!
//! The family, the load factor and the depth are a measured operating point. The link array is
//! one link per window byte and does not move, so the structure cannot cost less than four
//! bytes per window byte whatever its head table does, and the head table only chooses how much
//! sits on that floor.
//!
//! # Positions
//!
//! A position is an index into the buffer the caller passes, and every stored position fits
//! `u32`. The caller keeps positions below twice the window by sliding, and `slide` is what
//! moves the structure with it.
//!
//! The link array is indexed by the position modulo the window, so two positions one window
//! apart share a slot. The older of the two is exactly at the window edge from the newer, and
//! the walk reads the slot before an insert overwrites it, so the aliasing costs a candidate
//! that is about to leave the window and never a correct one. A stale link is caught where it
//! is read: a link that does not go backwards names a slot a newer position has taken.

use crate::sequence::{MAX_MATCH_LENGTH, MIN_MATCH, Match, WINDOW};
use crate::simd::{self, Kernel};

/// The candidate positions one search may compare.
///
/// Eight, resolved against a parse rather than against a match count. Depth 1 costs 3.06
/// nanoseconds per input byte less and 27.3 per cent more representation. Depths 32 and 128
/// buy nothing a bounded-horizon parse wants, because at their cost a parser that looks one
/// position further ahead on a shallower chain produces a cheaper parse.
pub const SEARCH_DEPTH: u32 = 8;

/// The head-table entries, one per four window bytes.
pub const HEAD_ENTRIES: usize = 16_384;

/// The link-array entries, one per window byte.
pub const LINK_ENTRIES: usize = WINDOW as usize;

/// The bytes the structure holds, whatever the input is.
pub const STATE_BYTES: usize = (HEAD_ENTRIES + LINK_ENTRIES) * 4;

/// The bytes the hash reads at a position.
const HASH_BYTES: usize = 4;

/// The head-table index is the top `HEAD_BITS` bits of the product.
const HEAD_BITS: u32 = HEAD_ENTRIES.trailing_zeros();

/// The multiplier the hash mixes with.
const MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;

/// The entry value that names no position.
const EMPTY: u32 = u32::MAX;

/// What one search reported, and what it spent reporting it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Found {
    /// The best match at the position, or `None` when no candidate reached the minimum.
    pub matched: Option<Match>,
    /// The candidate positions this search compared.
    ///
    /// The depth bound is what this counts against, and the walk already holds it: the value
    /// is the loop's own bound variable, so reading it costs nothing the search was not
    /// already paying.
    pub candidates: u32,
}

/// A bounded hash chain over a window of 65 536 bytes.
///
/// The structure holds positions, never bytes. The caller owns the buffer and passes it to
/// every call, so one buffer can serve the finder and the parse that drives it.
pub struct BoundedHashChain {
    head: Box<[u32]>,
    link: Box<[u32]>,
    kernel: Kernel,
}

impl Default for BoundedHashChain {
    fn default() -> Self {
        Self::new()
    }
}

impl BoundedHashChain {
    /// An empty chain, on the kernel this build selected.
    #[must_use]
    pub fn new() -> Self {
        Self::with_kernel(simd::SELECTED)
    }

    /// An empty chain, on a named kernel.
    ///
    /// The kernel changes speed and never the match a search reports. A caller names one to
    /// compare two of them or to reproduce a result on the scalar path.
    #[must_use]
    pub fn with_kernel(kernel: Kernel) -> Self {
        Self {
            head: vec![EMPTY; HEAD_ENTRIES].into_boxed_slice(),
            link: vec![EMPTY; LINK_ENTRIES].into_boxed_slice(),
            kernel,
        }
    }

    /// The kernel this chain compares candidates with.
    #[must_use]
    pub const fn kernel(&self) -> Kernel {
        self.kernel
    }

    /// Discards every position the structure holds.
    ///
    /// A region boundary is what calls this: nothing a later region emits may name a position
    /// an earlier one produced.
    pub fn reset(&mut self) {
        self.head.fill(EMPTY);
        self.link.fill(EMPTY);
    }

    /// Moves every stored position back by one window, discarding what falls out.
    ///
    /// The shift is the window exactly, so a position keeps its link slot and the array needs
    /// no permutation. A caller that slides its buffer by any other amount would have to
    /// rebuild the structure instead.
    pub fn slide(&mut self) {
        let window = WINDOW;
        for entry in self.head.iter_mut().chain(self.link.iter_mut()) {
            *entry = if *entry == EMPTY || *entry < window {
                EMPTY
            } else {
                entry.saturating_sub(window)
            };
        }
    }

    /// Searches at `at`, then inserts it.
    ///
    /// A match is reported only when it reaches the minimum match length, and its length is
    /// capped by the maximum the representation expresses and by the bytes `data` holds after
    /// `at`. Its distance is at least one and at most the window.
    ///
    /// The tie-break reads the reported length and distance and nothing else: longest wins,
    /// and a tie goes to the smaller distance. No candidate's position in the walk, and no
    /// address, takes part in it.
    pub fn step(&mut self, data: &[u8], at: usize) -> Found {
        let mut found = Found::default();
        let Some(slot) = self.slot(data, at) else {
            return found;
        };
        let cap = (MAX_MATCH_LENGTH as usize).min(data.len().saturating_sub(at));
        let oldest = at.saturating_sub(WINDOW as usize);
        let mut candidate = self.head.get(slot).copied().unwrap_or(EMPTY);
        let mut best_length = 0u32;
        let mut best_distance = 0u32;
        while candidate != EMPTY && found.candidates < SEARCH_DEPTH {
            let position = candidate as usize;
            // A link that is not behind the position, or is older than the window, ends the
            // walk. Nothing reachable past it is newer.
            if position >= at || position < oldest {
                break;
            }
            found.candidates = found.candidates.saturating_add(1);
            let length = length_of(simd::common_prefix(self.kernel, data, position, at, cap));
            if length >= MIN_MATCH {
                let distance = distance_of(at.saturating_sub(position));
                if length > best_length || (length == best_length && distance < best_distance) {
                    best_length = length;
                    best_distance = distance;
                }
            }
            let next = self
                .link
                .get(position % LINK_ENTRIES)
                .copied()
                .unwrap_or(EMPTY);
            // A link that does not go backwards is stale: a newer position has taken the slot.
            // Following it would walk forward, and at equal values it would not terminate.
            if next == EMPTY || next as usize >= position {
                break;
            }
            candidate = next;
        }
        if best_length >= MIN_MATCH {
            found.matched = Some(Match {
                length: best_length,
                distance: best_distance,
            });
        }
        self.link_in(slot, at);
        found
    }

    /// Inserts `at` without searching.
    ///
    /// This is what a parser does at the positions a taken match covers. The chain stays
    /// correct whatever the search did, because a link only has to name the previous occupant
    /// of its bucket.
    pub fn insert(&mut self, data: &[u8], at: usize) {
        if let Some(slot) = self.slot(data, at) {
            self.link_in(slot, at);
        }
    }

    /// The bytes this structure holds. It does not move with the input.
    #[must_use]
    pub const fn state_bytes() -> usize {
        STATE_BYTES
    }

    /// The first position of a buffer of `len` bytes the structure cannot take.
    ///
    /// The hash reads four bytes, so the last three positions of a buffer are offered and
    /// dropped. A caller that will later hold more bytes offers them again then, rather than
    /// recording them as inserted and leaving a hole in the chain at every boundary.
    #[must_use]
    pub const fn insertable_end(len: usize) -> usize {
        len.saturating_sub(HASH_BYTES.saturating_sub(1))
    }

    /// The head-table slot of the bytes at `at`, or `None` when `data` does not hold the
    /// bytes the hash reads or the position does not fit an entry.
    fn slot(&self, data: &[u8], at: usize) -> Option<usize> {
        let end = at.checked_add(HASH_BYTES)?;
        let bytes: [u8; HASH_BYTES] = data.get(at..end)?.try_into().ok()?;
        if u32::try_from(at).is_err() {
            return None;
        }
        let product = u64::from(u32::from_le_bytes(bytes)).wrapping_mul(MULTIPLIER);
        let index = narrow_index(product >> (u64::BITS.saturating_sub(HEAD_BITS)));
        (index < self.head.len()).then_some(index)
    }

    /// Links `at` in front of whatever the slot held, then takes the slot.
    fn link_in(&mut self, slot: usize, at: usize) {
        let Ok(position) = u32::try_from(at) else {
            return;
        };
        let previous = self.head.get(slot).copied().unwrap_or(EMPTY);
        if let Some(entry) = self.link.get_mut(at % LINK_ENTRIES) {
            *entry = previous;
        }
        if let Some(entry) = self.head.get_mut(slot) {
            *entry = position;
        }
    }
}

/// A compared prefix as a match length.
///
/// The comparison is capped at the maximum match length, which is far below `u32::MAX`, so the
/// narrowing cannot lose a value a caller can produce.
fn length_of(prefix: usize) -> u32 {
    u32::try_from(prefix).unwrap_or(MAX_MATCH_LENGTH)
}

/// A position difference as a match distance.
///
/// The walk rejects a candidate older than the window before it compares one, so the
/// difference is at most the window and the narrowing is exact.
fn distance_of(difference: usize) -> u32 {
    u32::try_from(difference).unwrap_or(WINDOW)
}

// The product was shifted right by 64 - HEAD_BITS, so at most HEAD_BITS remain and the value
// is below the head-table entry count.
#[allow(clippy::cast_possible_truncation)]
const fn narrow_index(shifted: u64) -> usize {
    shifted as usize
}

#[cfg(test)]
mod tests {
    use super::{BoundedHashChain, EMPTY, HEAD_ENTRIES, LINK_ENTRIES, SEARCH_DEPTH, STATE_BYTES};
    use crate::sequence::{MAX_MATCH_LENGTH, MIN_MATCH, Match, WINDOW};
    use crate::simd::{ALL, Kernel};

    const SEED: u64 = 0x5EED_0000_0000_0005;

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

    /// The inputs that drive an unbounded chain to its cap: every position shares one bucket,
    /// or a small set of them.
    fn adversarial(len: usize) -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("zeros", vec![0u8; len]),
            ("one-byte", vec![0x5Au8; len]),
            (
                "periodic",
                (0..len)
                    .map(|at| crate::entropy::low_byte(u64::try_from(at % 7).unwrap_or(0)))
                    .collect(),
            ),
        ]
    }

    /// The bytes a match names, read the way a decoder reconstructs them.
    fn copied(data: &[u8], at: usize, matched: Match) -> Vec<u8> {
        let mut out = Vec::with_capacity(matched.length as usize);
        let start = at.saturating_sub(matched.distance as usize);
        for step in 0..matched.length as usize {
            let from = start.saturating_add(step);
            let byte = data
                .get(from)
                .copied()
                .or_else(|| out.get(from.saturating_sub(at)).copied())
                .unwrap_or(0);
            out.push(byte);
        }
        out
    }

    #[test]
    fn a_search_compares_at_most_the_search_depth() {
        let mut peak = 0u32;
        for (name, data) in adversarial(16_384) {
            for &kernel in ALL {
                let mut finder = BoundedHashChain::with_kernel(kernel);
                for at in 0..data.len() {
                    let found = finder.step(&data, at);
                    assert!(
                        found.candidates <= SEARCH_DEPTH,
                        "{name} on {} compared {} candidates at {at}",
                        kernel.name(),
                        found.candidates
                    );
                    peak = peak.max(found.candidates);
                }
            }
        }
        assert_eq!(peak, SEARCH_DEPTH, "no shape reached the bound");
    }

    #[test]
    fn a_reported_match_is_inside_every_domain_and_names_the_bytes_it_claims() {
        let mut shapes = adversarial(8_192);
        shapes.push(("noise", noise(8_192, SEED)));
        shapes.push((
            "runs",
            (0..8_192usize)
                .map(|at| crate::entropy::low_byte(u64::try_from(at / 37 % 11).unwrap_or(0)))
                .collect(),
        ));
        for (name, data) in shapes {
            let mut finder = BoundedHashChain::new();
            for at in 0..data.len() {
                let Some(matched) = finder.step(&data, at).matched else {
                    continue;
                };
                assert!(matched.length >= MIN_MATCH, "{name} at {at}");
                assert!(matched.length <= MAX_MATCH_LENGTH, "{name} at {at}");
                assert!(matched.distance >= 1, "{name} at {at}");
                assert!(matched.distance <= WINDOW, "{name} at {at}");
                assert!(matched.distance as usize <= at, "{name} at {at}");
                let end = at.saturating_add(matched.length as usize);
                assert!(end <= data.len(), "{name} at {at}");
                assert_eq!(
                    copied(&data, at, matched),
                    data.get(at..end).unwrap_or_default(),
                    "{name} at {at}"
                );
            }
        }
    }

    #[test]
    fn the_tie_break_takes_the_longest_and_then_the_nearest() {
        // Three occurrences of one four-byte token, at 0, 8 and 16. The bytes after each
        // occurrence decide which candidate the search at 16 keeps.
        let mut equal = vec![b'.'; 64];
        for start in [0usize, 8, 16] {
            for (offset, byte) in b"WXYZAB".iter().enumerate() {
                if let Some(slot) = equal.get_mut(start.saturating_add(offset)) {
                    *slot = *byte;
                }
            }
        }
        let mut finder = BoundedHashChain::new();
        for at in 0..16usize {
            let _ = finder.step(&equal, at);
        }
        let found = finder.step(&equal, 16);
        assert_eq!(
            found.matched,
            Some(Match {
                length: 8,
                distance: 8
            }),
            "an equal-length tie goes to the nearer candidate"
        );

        let mut longer = equal.clone();
        // The farther candidate keeps agreeing for one byte more than the nearer one.
        if let Some(slot) = longer.get_mut(14) {
            *slot = b'!';
        }
        if let Some(slot) = longer.get_mut(6) {
            *slot = b'C';
        }
        if let Some(slot) = longer.get_mut(22) {
            *slot = b'C';
        }
        let mut finder = BoundedHashChain::new();
        for at in 0..16usize {
            let _ = finder.step(&longer, at);
        }
        let found = finder.step(&longer, 16);
        assert_eq!(
            found.matched,
            Some(Match {
                length: 8,
                distance: 16
            }),
            "a longer match beats a nearer one"
        );
    }

    #[test]
    fn the_window_bounds_a_candidate() {
        let window = WINDOW as usize;
        for extra in [0usize, 1] {
            let later = window.saturating_add(extra);
            let mut data = noise(later.saturating_add(64), SEED ^ extra as u64);
            for offset in 0..16usize {
                let byte = data.get(offset).copied().unwrap_or(0);
                if let Some(slot) = data.get_mut(later.saturating_add(offset)) {
                    *slot = byte;
                }
            }
            let mut finder = BoundedHashChain::new();
            finder.insert(&data, 0);
            let found = finder.step(&data, later);
            if extra == 0 {
                assert_eq!(
                    found.matched.map(|matched| matched.distance),
                    Some(WINDOW),
                    "a candidate exactly one window back is reachable"
                );
            } else {
                assert_eq!(
                    found.matched, None,
                    "a candidate past the window is not reachable"
                );
                assert_eq!(found.candidates, 0, "and it is not even compared");
            }
        }
    }

    #[test]
    fn a_slide_moves_every_position_by_one_window() {
        let window = WINDOW as usize;
        let mut data = noise(window.saturating_mul(2), SEED ^ 0x11);
        for offset in 0..16usize {
            let byte = data
                .get(window.saturating_add(offset))
                .copied()
                .unwrap_or(0);
            if let Some(slot) = data.get_mut(window.saturating_add(100).saturating_add(offset)) {
                *slot = byte;
            }
        }
        let mut finder = BoundedHashChain::new();
        finder.insert(&data, window);
        finder.slide();
        let slid = data.get(window..).unwrap_or_default();
        let found = finder.step(slid, 100);
        assert_eq!(
            found.matched.map(|matched| matched.distance),
            Some(100),
            "the position the slide moved is found at its new index"
        );
    }

    #[test]
    fn a_reset_discards_every_position() {
        let data = vec![3u8; 1_024];
        let mut finder = BoundedHashChain::new();
        for at in 0..64usize {
            let _ = finder.step(&data, at);
        }
        assert!(finder.step(&data, 64).matched.is_some());
        finder.reset();
        assert!(finder.head.iter().all(|entry| *entry == EMPTY));
        assert!(finder.link.iter().all(|entry| *entry == EMPTY));
        assert_eq!(finder.step(&data, 64).matched, None);
    }

    #[test]
    fn the_declared_state_does_not_move_with_the_input() {
        assert_eq!(STATE_BYTES, 327_680);
        assert_eq!(HEAD_ENTRIES.saturating_mul(4), 65_536);
        assert_eq!(LINK_ENTRIES.saturating_mul(4), 262_144);
        let finder = BoundedHashChain::with_kernel(Kernel::Scalar);
        assert_eq!(finder.kernel(), Kernel::Scalar);
        assert_eq!(
            finder
                .head
                .len()
                .saturating_add(finder.link.len())
                .saturating_mul(4),
            BoundedHashChain::state_bytes()
        );
    }

    #[test]
    fn every_kernel_reports_the_same_matches() {
        let mut shapes = adversarial(4_096);
        shapes.push(("noise", noise(4_096, SEED ^ 0x22)));
        for (name, data) in shapes {
            let mut finders: Vec<BoundedHashChain> = ALL
                .iter()
                .map(|&k| BoundedHashChain::with_kernel(k))
                .collect();
            for at in 0..data.len() {
                let mut expected = None;
                for (index, finder) in finders.iter_mut().enumerate() {
                    let found = finder.step(&data, at);
                    if index == 0 {
                        expected = Some(found);
                    } else {
                        assert_eq!(Some(found), expected, "{name} at {at}");
                    }
                }
            }
        }
    }
}

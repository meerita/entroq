//! Owns the offset cache: the one slot a decoder carries across the sequences of a region, and
//! the rule that decides what the repeat code names.
//!
//! This module does not own the match-distance alphabet, the stream the code is coded on, or
//! when a region boundary resets the slot.
//!
//! ```text
//! coded value 1        names the distance the slot holds
//! coded value above 1  names a distance, one below the coded value
//! ```
//!
//! The slot updates on every match, whether or not that match named it, so the distance stream
//! is a function of every match before it inside a region. One slot, move to front, four bytes
//! of decoder state, no declared initial history, and no shifted view: a literal run of zero
//! does not change what a code means.

use crate::format::{Corruption, Error};

/// The coded value of the match-distance alphabet that names the slot.
pub const REPEAT_CODE: u64 = 1;

/// The bytes of decoder state the cache occupies.
pub const STATE_BYTES: u64 = 4;

/// The distance the last match used, or nothing.
///
/// The cache starts unset at a region boundary and no initial history is declared, so a code
/// that names it before any match has set it names nothing and is refused.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OffsetCache {
    slot: Option<u32>,
}

impl OffsetCache {
    /// The state a region boundary restores: one slot, unset.
    #[must_use]
    pub const fn reset() -> Self {
        Self { slot: None }
    }

    /// The distance the slot holds, if it holds one.
    #[must_use]
    pub const fn slot(self) -> Option<u32> {
        self.slot
    }

    /// Whether the slot already names this distance, which is what lets a match code the
    /// repeat code instead of its distance.
    #[must_use]
    pub fn names(self, distance: u32) -> bool {
        self.slot == Some(distance)
    }

    /// The distance the repeat code names.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the slot names nothing, and when it names a distance below
    /// one. The two are distinct: a slot nothing has set names no distance at all, and a slot
    /// holding a distance no window admits is a slot a decoder must not copy from. A decoder
    /// that collapsed them would report the wrong rule as broken.
    pub const fn resolve(self) -> Result<u32, Error> {
        match self.slot {
            None => Err(Error::CorruptData(Corruption::RepeatUnset)),
            Some(0) => Err(Error::CorruptData(Corruption::RepeatDistance)),
            Some(distance) => Ok(distance),
        }
    }

    /// Records the distance a match used, whichever way the match named it.
    ///
    /// Total, so that the update carries no precondition a caller can violate. `resolve` is the
    /// boundary that owns what a slot may name, and it refuses there rather than here.
    pub const fn use_distance(&mut self, distance: u32) {
        self.slot = Some(distance);
    }
}

#[cfg(test)]
mod tests {
    use super::OffsetCache;
    use crate::format::{Corruption, Error};

    #[test]
    fn a_code_that_names_an_unset_slot_is_refused_before_a_distance_exists() {
        let cache = OffsetCache::reset();
        assert_eq!(cache.slot(), None);
        assert_eq!(
            cache.resolve(),
            Err(Error::CorruptData(Corruption::RepeatUnset)),
            "the slot a region boundary leaves names nothing"
        );
        assert!(!cache.names(1));
    }

    /// The update is total and the resolution is the boundary that owns what a slot may name,
    /// so a slot holding a distance no window admits is refused where a decoder would read it.
    #[test]
    fn a_code_that_resolves_to_a_distance_below_one_is_refused() {
        let mut cache = OffsetCache::reset();
        cache.use_distance(0);
        assert_eq!(
            cache.resolve(),
            Err(Error::CorruptData(Corruption::RepeatDistance))
        );
        cache.use_distance(1);
        assert_eq!(cache.resolve(), Ok(1));
    }

    #[test]
    fn the_slot_holds_the_last_distance_a_match_used() {
        let mut cache = OffsetCache::reset();
        for distance in [7u32, 7, 12, 65_536, 1] {
            cache.use_distance(distance);
            assert_eq!(cache.slot(), Some(distance));
            assert_eq!(cache.resolve(), Ok(distance));
            assert!(cache.names(distance));
        }
    }
}

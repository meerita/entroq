//! Owns the content a proof runs on.
//!
//! Every byte is a function of its own position, so content is produced in place at any chunk
//! size and checked in place without holding any of it. That is what lets a proof stream a
//! gibibyte without allocating one, and what makes a shape identical on every architecture:
//! the generator is integer arithmetic on a position, not a random source.
//!
//! This module does not own what a proof measures. It owns only what the proof reads.

/// A content class.
///
/// Each one reaches a different part of the skeleton: a constant run is what an RLE block
/// exists for, mixed content is what a RAW block costs its expansion bound on, and a run
/// length that divides no block size evenly starts a repeat at a different offset in every
/// block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    Zeros,
    OneByte,
    Mixed,
    Runs,
    TextLike,
}

pub const SHAPES: [Shape; 5] = [
    Shape::Zeros,
    Shape::OneByte,
    Shape::Mixed,
    Shape::Runs,
    Shape::TextLike,
];

impl Shape {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Zeros => "zeros",
            Self::OneByte => "one-byte",
            Self::Mixed => "mixed",
            Self::Runs => "runs",
            Self::TextLike => "text-like",
        }
    }

    /// The byte this shape holds at a position.
    #[must_use]
    pub fn at(self, position: u64) -> u8 {
        match self {
            Self::Zeros => 0,
            Self::OneByte => 0xA5,
            Self::Mixed => mix(position),
            Self::Runs => mix(position.checked_div(97).unwrap_or(0)),
            Self::TextLike => {
                const ALPHABET: &[u8; 16] = b"the quick brown ";
                let index = usize::try_from(position & 0x0F).unwrap_or(0);
                ALPHABET.get(index).copied().unwrap_or(b' ')
            }
        }
    }

    /// Writes the bytes this shape holds from `at` onward.
    pub fn fill(self, at: u64, out: &mut [u8]) {
        let mut position = at;
        for slot in out.iter_mut() {
            *slot = self.at(position);
            position = position.saturating_add(1);
        }
    }
}

/// The mixing step every non-constant shape is built from.
///
/// It is a multiply by an odd 64-bit constant and a shift of the high byte down. Both are
/// defined on every architecture and neither reads memory, so two hosts of different byte
/// order produce the same sequence.
fn mix(position: u64) -> u8 {
    let mixed = position.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    u8::try_from(mixed.wrapping_shr(56)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{SHAPES, Shape};

    #[test]
    fn a_shape_holds_the_same_byte_at_a_position_however_it_is_filled() {
        for shape in SHAPES {
            let mut whole = [0_u8; 256];
            shape.fill(0, &mut whole);
            for (offset, expected) in whole.iter().enumerate() {
                let mut one = [0_u8; 1];
                shape.fill(u64::try_from(offset).unwrap_or(0), &mut one);
                assert_eq!(one.first(), Some(expected), "{} at {offset}", shape.name());
            }
        }
    }

    #[test]
    fn every_shape_states_a_distinct_name() {
        let mut names: Vec<&str> = SHAPES.iter().map(|shape| shape.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn a_constant_shape_is_constant_and_a_mixed_one_is_not() {
        let mut zeros = [0xFF_u8; 64];
        Shape::Zeros.fill(0, &mut zeros);
        assert!(zeros.iter().all(|byte| *byte == 0));

        let mut mixed = [0_u8; 64];
        Shape::Mixed.fill(0, &mut mixed);
        assert!(mixed.windows(2).any(|pair| pair.first() != pair.last()));
    }
}

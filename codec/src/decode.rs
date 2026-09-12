//! Owns the decoder engine: the path from a validated stream back to the original bytes.
//!
//! This module does not own field validation, which the format module performs once at its
//! boundary, or the streaming state machine that drives it.
//!
//! A payload is expanded in steps. Each step reads only the input it was given and writes
//! only the output it was given, so a block larger than either buffer costs more steps and
//! never more memory. Nothing here allocates.

use crate::format::{BlockHeader, BlockType, Error};

/// What one expansion step moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Moved {
    /// The stored bytes the step read.
    pub consumed: usize,
    /// The decoded bytes the step wrote.
    pub produced: usize,
}

/// The payload of one block, expanded across as many steps as the caller's buffers require.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Payload {
    /// The stored bytes are the content.
    Raw {
        /// The stored bytes still to copy.
        remaining: u32,
    },
    /// One stored byte, repeated to the size the header declared.
    Rle {
        /// The repeated byte, once it has arrived.
        value: Option<u8>,
        /// The decoded bytes still to write.
        remaining: u32,
    },
}

impl Payload {
    /// The payload a block header describes, before any of it has arrived.
    #[must_use]
    pub const fn new(header: BlockHeader) -> Self {
        match header.kind {
            BlockType::Raw => Self::Raw {
                remaining: header.size,
            },
            BlockType::Rle => Self::Rle {
                value: None,
                remaining: header.size,
            },
        }
    }

    /// Whether every decoded byte of this payload has been written.
    #[must_use]
    pub const fn is_done(self) -> bool {
        match self {
            Self::Raw { remaining } | Self::Rle { remaining, .. } => remaining == 0,
        }
    }

    /// The stored bytes this payload still needs from the stream.
    #[must_use]
    pub fn stored_remaining(self) -> usize {
        match self {
            Self::Raw { remaining } => usize::try_from(remaining).unwrap_or(usize::MAX),
            Self::Rle { value, .. } => usize::from(value.is_none()),
        }
    }

    /// Moves as much of this payload as the two buffers allow.
    ///
    /// A step that moves nothing means one of the buffers is empty, not that the payload is
    /// finished. `is_done` answers that.
    ///
    /// # Errors
    ///
    /// Returns `OutputTooSmall` or `TruncatedInput` when a buffer contradicts the length the
    /// step computed from it, which the caller's buffers cannot cause.
    pub fn step(&mut self, input: &[u8], out: &mut [u8]) -> Result<Moved, Error> {
        match *self {
            Self::Raw { remaining } => {
                let take = usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .min(input.len())
                    .min(out.len());
                let source = input
                    .get(..take)
                    .ok_or(Error::TruncatedInput { needed: take })?;
                let target = out
                    .get_mut(..take)
                    .ok_or(Error::OutputTooSmall { needed: take })?;
                target.copy_from_slice(source);
                *self = Self::Raw {
                    remaining: remaining.saturating_sub(narrow(take)),
                };
                Ok(Moved {
                    consumed: take,
                    produced: take,
                })
            }
            Self::Rle {
                value: None,
                remaining,
            } => {
                let Some(byte) = input.first().copied() else {
                    return Ok(Moved {
                        consumed: 0,
                        produced: 0,
                    });
                };
                *self = Self::Rle {
                    value: Some(byte),
                    remaining,
                };
                Ok(Moved {
                    consumed: 1,
                    produced: 0,
                })
            }
            Self::Rle {
                value: Some(byte),
                remaining,
            } => {
                let take = usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .min(out.len());
                let target = out
                    .get_mut(..take)
                    .ok_or(Error::OutputTooSmall { needed: take })?;
                target.fill(byte);
                *self = Self::Rle {
                    value: Some(byte),
                    remaining: remaining.saturating_sub(narrow(take)),
                };
                Ok(Moved {
                    consumed: 0,
                    produced: take,
                })
            }
        }
    }
}

fn narrow(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Moved, Payload};
    use crate::format::{BlockHeader, Error};

    #[test]
    fn a_raw_payload_copies_what_both_buffers_allow() -> Result<(), Error> {
        let block = BlockHeader::raw(true, 8)?;
        let mut payload = Payload::new(block);
        assert_eq!(payload.stored_remaining(), 8);

        let mut out = [0_u8; 8];
        let first = out.get_mut(..3).ok_or(Error::InvalidParameter)?;
        assert_eq!(
            payload.step(&[1, 2, 3, 4, 5], first)?,
            Moved {
                consumed: 3,
                produced: 3
            }
        );
        assert!(!payload.is_done());
        assert_eq!(payload.stored_remaining(), 5);

        let rest = out.get_mut(3..).ok_or(Error::InvalidParameter)?;
        assert_eq!(
            payload.step(&[4, 5, 6, 7, 8], rest)?,
            Moved {
                consumed: 5,
                produced: 5
            }
        );
        assert!(payload.is_done());
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8]);
        Ok(())
    }

    #[test]
    fn a_raw_payload_moves_nothing_when_a_buffer_is_empty() -> Result<(), Error> {
        let block = BlockHeader::raw(true, 4)?;
        let mut payload = Payload::new(block);
        let mut out = [0_u8; 4];
        assert_eq!(
            payload.step(&[], &mut out)?,
            Moved {
                consumed: 0,
                produced: 0
            }
        );
        assert_eq!(
            payload.step(&[1, 2, 3, 4], &mut [])?,
            Moved {
                consumed: 0,
                produced: 0
            }
        );
        assert!(!payload.is_done());
        Ok(())
    }

    #[test]
    fn an_rle_payload_takes_one_stored_byte_and_then_writes_only_output() -> Result<(), Error> {
        let block = BlockHeader::rle(true, 5)?;
        let mut payload = Payload::new(block);
        assert_eq!(payload.stored_remaining(), 1);

        let mut out = [0_u8; 5];
        assert_eq!(
            payload.step(&[0xAB], &mut out)?,
            Moved {
                consumed: 1,
                produced: 0
            }
        );
        assert_eq!(payload.stored_remaining(), 0);

        let first = out.get_mut(..2).ok_or(Error::InvalidParameter)?;
        assert_eq!(
            payload.step(&[], first)?,
            Moved {
                consumed: 0,
                produced: 2
            }
        );
        assert!(!payload.is_done());

        let rest = out.get_mut(2..).ok_or(Error::InvalidParameter)?;
        assert_eq!(
            payload.step(&[], rest)?,
            Moved {
                consumed: 0,
                produced: 3
            }
        );
        assert!(payload.is_done());
        assert_eq!(out, [0xAB; 5]);
        Ok(())
    }

    #[test]
    fn a_payload_never_writes_past_the_output_it_was_given() -> Result<(), Error> {
        let block = BlockHeader::rle(true, 1_000)?;
        let mut payload = Payload::new(block);
        let mut out = [0_u8; 16];
        let moved = payload.step(&[0x11], &mut out)?;
        assert_eq!(moved.consumed, 1);
        assert_eq!(moved.produced, 0);
        let mut written = 0_usize;
        while !payload.is_done() {
            let moved = payload.step(&[], &mut out)?;
            assert!(moved.produced <= out.len());
            written = written.saturating_add(moved.produced);
        }
        assert_eq!(written, 1_000);
        Ok(())
    }
}

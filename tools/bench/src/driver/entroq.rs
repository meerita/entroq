//! Owns the Entroq boundary: how the harness drives this repository's codec.
//!
//! The codec is driven in-process, through the public API of its own crate, which is the same
//! boundary a competitor is driven through. Both sides of a comparison therefore cross one
//! call and one process, and neither is charged for a boundary the other does not pay.
//!
//! There is no unsafe code here and no linked archive. The codec is a crate of this
//! workspace, built at the revision the run records.
//!
//! Three metrics this module reports that a competitor commonly cannot: the bytes each
//! machine holds, which both machines state themselves; the allocations one compression
//! makes, which the harness allocator serves because every Entroq allocation passes through
//! it; and the streaming latency, which the encoder's own incremental interface produces.
//!
//! This module does not own what is measured, how it is timed, or where it is written.

// Every method takes a receiver, and a mutable one where another codec needs it, because the
// dispatching enum calls them uniformly. Every method that can fail for another codec keeps
// the same shape here too. A session that held no state would still take both, so the shape
// is the interface and not an oversight.
#![allow(
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut
)]

use std::time::Instant;

use codec::format::{DecoderPolicy, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass};
use codec::stream::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Decoder, Encoder, StreamState};

use super::{Chunk, Method, Notes};
use crate::error::{Error, Result};

/// The resource class this revision declares.
///
/// Every selection behind this codec was measured at a window of 65 536 bytes, and the class
/// that declares that window is the smallest one. The other four stay declared and unmeasured.
const CLASS: ResourceClass = ResourceClass::Small;

/// One Entroq session, at the one operating point this revision exposes.
pub struct Session {
    header: FrameHeader,
    policy: DecoderPolicy,
}

impl Session {
    /// Opens the codec at one of the points the subject names.
    ///
    /// # Errors
    ///
    /// Never fails. The shape matches the competitors', whose libraries can refuse a context.
    pub const fn open(_point: &str) -> Result<Self> {
        // The subject declares one operating point, because this revision admits no mode.
        // `Session::open` has already checked the name against it.
        Ok(Self {
            header: FrameHeader::new(
                CLASS,
                RegionIndependence::Independent,
                IntegrityMode::Absent,
            ),
            policy: DecoderPolicy::CONSERVATIVE,
        })
    }

    /// The version the codec crate declares for itself.
    pub fn linked_version(&self) -> Option<String> {
        Some(String::from(codec::VERSION))
    }

    pub const fn notes(&self) -> Notes {
        Notes {
            format: "the Entroq format, version 1, as one frame: resource class small, \
                     regions independent, a window of 65 536 bytes, a region of 1 048 576 \
                     input bytes, and a block of 65 536 input bytes. The encoder assembles \
                     every block type each block admits and emits the one that stores the \
                     fewest bytes.",
            integrity: "None. The frame declares integrity absent. Version 1 defines the \
                        position and the width of the integrity field and computes nothing \
                        into it, so this measurement carries no checksum and is credited for \
                        none, which is the setting every competitor here was measured under \
                        too.",
            state_bytes: Method::By(
                "Encoder::steady_state_bytes and Decoder::steady_state_bytes, which each \
                 machine reports for what it holds between calls",
            ),
            allocations: Method::By(
                "the harness allocator, which every allocation this codec makes passes \
                 through, because the codec is a crate of this workspace rather than a \
                 linked library with its own",
            ),
            streaming: Method::By(
                "Encoder::encode and Encoder::finish, fed one chunk at a time, which is the \
                 codec's base execution model rather than a wrapper over a one-shot path",
            ),
            threads: Method::Absent(
                "This revision is single threaded and publishes no thread parameter. \
                 Parallelism is enabled by the format and is not implemented yet.",
            ),
        }
    }

    /// The output capacity a compression of `input` bytes needs.
    ///
    /// It is the version 1 expansion bound: every block type stores at most the bytes it
    /// decodes to, so a frame never exceeds its content plus one header per structure.
    pub fn compress_bound(&self, input: usize) -> usize {
        let logical = u64::try_from(input).unwrap_or(u64::MAX);
        let region = u64::try_from(DEFAULT_REGION_BYTES).unwrap_or(u64::MAX);
        self.header
            .raw_frame_bytes(logical, region, DEFAULT_BLOCK_BYTES)
            .ok()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .unwrap_or(usize::MAX)
    }

    /// Compresses `src` into `dst` and reports the compressed length.
    ///
    /// # Errors
    ///
    /// Fails when the codec refuses the request, or when `dst` is smaller than the bound.
    pub fn compress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut encoder = Encoder::new(self.header).map_err(failed)?;
        let mut at = 0_usize;
        let mut fed = 0_usize;
        while fed < src.len() {
            let rest = src.get(fed..).ok_or_else(|| short("the input"))?;
            let room = dst.get_mut(at..).ok_or_else(|| short("the output"))?;
            let progress = encoder.encode(rest, room).map_err(failed)?;
            if progress.consumed == 0 && progress.produced == 0 {
                return Err(short("the output"));
            }
            fed = fed.saturating_add(progress.consumed);
            at = at.saturating_add(progress.produced);
        }
        loop {
            let room = dst.get_mut(at..).ok_or_else(|| short("the output"))?;
            let progress = encoder.finish(room).map_err(failed)?;
            at = at.saturating_add(progress.produced);
            if progress.state == StreamState::Finished {
                return Ok(at);
            }
            if progress.produced == 0 {
                return Err(short("the output"));
            }
        }
    }

    /// Decompresses `src` into `dst` and reports the decompressed length.
    ///
    /// # Errors
    ///
    /// Fails when the stream is refused, or when `dst` is smaller than the content.
    pub fn decompress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut decoder = Decoder::new(self.policy);
        let mut at = 0_usize;
        let mut produced = 0_usize;
        loop {
            let rest = src.get(at..).unwrap_or_default();
            let room = dst.get_mut(produced..).ok_or_else(|| short("the output"))?;
            let progress = decoder.decode(rest, room).map_err(failed)?;
            at = at.saturating_add(progress.consumed);
            produced = produced.saturating_add(progress.produced);
            if progress.state == StreamState::Finished {
                break;
            }
            if progress.consumed == 0 && progress.produced == 0 {
                break;
            }
        }
        decoder.finish().map_err(failed)?;
        Ok(produced)
    }

    /// The bytes the encoder holds between calls, as the machine reports them.
    pub fn encoder_state_bytes(&self) -> Option<u64> {
        Encoder::new(self.header)
            .ok()
            .map(|encoder| u64::try_from(encoder.steady_state_bytes()).unwrap_or(u64::MAX))
    }

    /// The bytes the decoder holds between calls, as the machine reports them.
    pub fn decoder_state_bytes(&self) -> Option<u64> {
        Some(u64::try_from(Decoder::new(self.policy).steady_state_bytes()).unwrap_or(u64::MAX))
    }

    /// Compresses `src` one chunk at a time and reports what each chunk cost and produced.
    ///
    /// The encoder closes a region when the region is full and otherwise only when the caller
    /// asks, so feeding one chunk at a time produces the same stream as feeding the whole
    /// input. A chunk that produces nothing is a chunk the encoder staged, and its cost is
    /// still the caller's.
    ///
    /// # Errors
    ///
    /// Fails when the codec refuses the request, or when `dst` is smaller than the bound.
    pub fn stream(&mut self, src: &[u8], chunk: usize, dst: &mut [u8]) -> Result<Vec<Chunk>> {
        let mut encoder = Encoder::new(self.header).map_err(failed)?;
        let mut chunks = Vec::new();
        let mut at = 0_usize;
        let mut fed = 0_usize;
        while fed < src.len() {
            let end = fed.saturating_add(chunk.max(1)).min(src.len());
            let piece = src.get(fed..end).ok_or_else(|| short("the input"))?;
            let started = Instant::now();
            let mut taken = 0_usize;
            let mut written = 0_usize;
            while taken < piece.len() {
                let rest = piece.get(taken..).ok_or_else(|| short("the input"))?;
                let room = dst.get_mut(at..).ok_or_else(|| short("the output"))?;
                let progress = encoder.encode(rest, room).map_err(failed)?;
                if progress.consumed == 0 && progress.produced == 0 {
                    return Err(short("the output"));
                }
                taken = taken.saturating_add(progress.consumed);
                written = written.saturating_add(progress.produced);
                at = at.saturating_add(progress.produced);
            }
            chunks.push(Chunk {
                input_bytes: piece.len(),
                output_bytes: written,
                elapsed_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            });
            fed = end;
        }
        let started = Instant::now();
        let mut written = 0_usize;
        loop {
            let room = dst.get_mut(at..).ok_or_else(|| short("the output"))?;
            let progress = encoder.finish(room).map_err(failed)?;
            written = written.saturating_add(progress.produced);
            at = at.saturating_add(progress.produced);
            if progress.state == StreamState::Finished {
                break;
            }
            if progress.produced == 0 {
                return Err(short("the output"));
            }
        }
        chunks.push(Chunk {
            input_bytes: 0,
            output_bytes: written,
            elapsed_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        });
        Ok(chunks)
    }
}

fn failed(error: codec::format::Error) -> Error {
    Error::measure("the Entroq codec", error.to_string())
}

fn short(what: &str) -> Error {
    Error::measure(
        format!("the Entroq measurement over {what}"),
        "ran out of room, which the compression bound is sized to prevent",
    )
}

#[cfg(test)]
mod tests {
    use super::Session;

    fn content(len: usize) -> Vec<u8> {
        const ALPHABET: &[u8; 16] = b"the quick brown ";
        (0..len)
            .map(|at| ALPHABET.get(at & 0x0F).copied().unwrap_or(b' '))
            .collect()
    }

    #[test]
    fn the_codec_round_trips_through_the_boundary_the_harness_drives() {
        let Ok(mut session) = Session::open("default") else {
            unreachable!("the codec opens at the one point it declares")
        };
        for length in [0_usize, 1, 1_024, 200_000] {
            let data = content(length);
            let mut coded = vec![0_u8; session.compress_bound(data.len()).max(64)];
            let Ok(stored) = session.compress(&data, &mut coded) else {
                unreachable!("the codec compresses {length} bytes")
            };
            let mut restored = vec![0_u8; data.len().max(1)];
            let Ok(produced) =
                session.decompress(coded.get(..stored).unwrap_or_default(), &mut restored)
            else {
                unreachable!("the codec decodes what it wrote at {length} bytes")
            };
            assert_eq!(produced, data.len(), "at {length} bytes");
            assert_eq!(restored.get(..produced), Some(data.as_slice()));
        }
    }

    #[test]
    fn a_compression_stays_inside_the_bound_the_session_states() {
        let Ok(mut session) = Session::open("default") else {
            unreachable!("the codec opens at the one point it declares")
        };
        let data = content(300_000);
        let bound = session.compress_bound(data.len());
        let mut coded = vec![0_u8; bound];
        let Ok(stored) = session.compress(&data, &mut coded) else {
            unreachable!("the codec compresses the input")
        };
        assert!(stored <= bound, "{stored} bytes against a bound of {bound}");
        assert!(stored < data.len(), "repetitive content did not compress");
    }

    #[test]
    fn streaming_produces_the_same_bytes_as_one_call() {
        let Ok(mut session) = Session::open("default") else {
            unreachable!("the codec opens at the one point it declares")
        };
        let data = content(200_000);
        let bound = session.compress_bound(data.len());
        let mut whole = vec![0_u8; bound];
        let Ok(stored) = session.compress(&data, &mut whole) else {
            unreachable!("the codec compresses the input")
        };
        let mut streamed = vec![0_u8; bound];
        let Ok(chunks) = session.stream(&data, 64 * 1024, &mut streamed) else {
            unreachable!("the codec streams the input")
        };
        let produced: usize = chunks.iter().map(|chunk| chunk.output_bytes).sum();
        assert_eq!(produced, stored);
        assert_eq!(whole.get(..stored), streamed.get(..produced));
        assert!(chunks.len() > 1, "one chunk is not a streamed measurement");
    }

    #[test]
    fn both_machines_report_the_bytes_they_hold() {
        let Ok(session) = Session::open("default") else {
            unreachable!("the codec opens at the one point it declares")
        };
        assert!(session.encoder_state_bytes().unwrap_or(0) > 0);
        assert!(session.decoder_state_bytes().unwrap_or(0) > 0);
    }

    #[test]
    fn every_metric_the_notes_claim_is_one_this_boundary_reaches() {
        let Ok(session) = Session::open("default") else {
            unreachable!("the codec opens at the one point it declares")
        };
        let notes = session.notes();
        assert!(notes.state_bytes.is_available());
        assert!(notes.allocations.is_available());
        assert!(notes.streaming.is_available());
        assert!(!notes.threads.is_available());
        assert!(notes.integrity.contains("None"), "{}", notes.integrity);
    }
}

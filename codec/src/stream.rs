//! Owns the streaming state machine: incremental input, incremental output, explicit finish,
//! and the buffer budget the machine holds.
//!
//! This module does not own encode or decode logic. It drives them within a declared bound.
//!
//! Both directions work on caller-supplied buffers and report what they moved, so neither
//! holds a whole input or a whole output. The encoder holds one region of staged input,
//! because a region header declares the bytes its blocks occupy before the first of them is
//! written. The decoder holds a header scratch sized for the widest header the format
//! defines, and nothing else: a payload travels from the caller's input to the caller's
//! output without being stored. Neither side allocates once it is running.
//!
//! Chunk size is the caller's choice and never reaches the bytes. The encoder closes a region
//! when the region is full, and otherwise only when the caller asks, so one input produces
//! one stream at every chunk size.

use crate::decode::Payload;
use crate::encode::Layout;
use crate::format::{
    BLOCK_HEADER_BYTES, BlockHeader, Corruption, DecoderPolicy, Error, FRAME_HEADER_FIXED_BYTES,
    FRAME_HEADER_MAX_BYTES, FrameHeader, RECORD_TAG_BYTES, Record,
};

pub use crate::encode::{DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES};

/// The bytes a streaming machine keeps for one header.
///
/// The widest header the format defines is a frame header carrying every optional field, so a
/// full scratch always holds a complete header of any kind.
const SCRATCH_BYTES: usize = FRAME_HEADER_MAX_BYTES;

/// Why a streaming call returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamState {
    /// The call took every byte it was given and has room for more.
    NeedsInput,
    /// The output buffer filled. The next call resumes where this one stopped.
    NeedsOutput,
    /// The stream ended.
    Finished,
}

/// What one streaming call moved, and why it returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    /// The input bytes the call took.
    pub consumed: usize,
    /// The output bytes the call wrote.
    pub produced: usize,
    /// Why the call returned.
    pub state: StreamState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Emitting {
    Start,
    Open,
    Blocks,
    Terminator,
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Closing {
    Open,
    Flush,
    Finish,
}

/// Turns input into a frame, at whatever chunk size the caller has.
///
/// The encoder stages one region of input, closes it when it is full, and writes it out as
/// RAW blocks. `flush` closes the current region early and keeps the stream open. `finish`
/// closes it and writes the terminator.
///
/// The memory the encoder holds is one region plus one header scratch, and `memory_bytes`
/// states it. Nothing is allocated after construction.
pub struct Encoder {
    header: FrameHeader,
    layout: Layout,
    staged: Vec<u8>,
    scratch: [u8; SCRATCH_BYTES],
    scratch_len: usize,
    scratch_at: usize,
    data_at: usize,
    data_end: usize,
    next_block_at: usize,
    accepted: u64,
    emitting: Emitting,
    closing: Closing,
    poison: Option<Error>,
}

impl Encoder {
    /// An encoder for `header`, using the default region and block sizes.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the header declares an index, which this encoder does
    /// not write.
    pub fn new(header: FrameHeader) -> Result<Self, Error> {
        Self::with_layout(header, DEFAULT_REGION_BYTES, DEFAULT_BLOCK_BYTES)
    }

    /// An encoder that stages `region_bytes` of input and cuts it into `block_bytes` blocks.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a size is zero, when the block size is above what a
    /// block header declares, or when the header declares an index this encoder does not
    /// write.
    pub fn with_layout(
        header: FrameHeader,
        region_bytes: usize,
        block_bytes: u32,
    ) -> Result<Self, Error> {
        if header.index_location.is_some() {
            return Err(Error::InvalidParameter);
        }
        let layout = Layout::new(region_bytes, block_bytes)?;
        Ok(Self {
            header,
            layout,
            staged: Vec::with_capacity(region_bytes),
            scratch: [0; SCRATCH_BYTES],
            scratch_len: 0,
            scratch_at: 0,
            data_at: 0,
            data_end: 0,
            next_block_at: 0,
            accepted: 0,
            emitting: Emitting::Start,
            closing: Closing::Open,
            poison: None,
        })
    }

    /// The bytes this encoder holds, beyond the buffers the caller passes it.
    ///
    /// The value is fixed at construction and does not move with the input.
    #[must_use]
    pub const fn memory_bytes(&self) -> usize {
        self.staged.capacity().saturating_add(SCRATCH_BYTES)
    }

    /// Takes input and writes whatever the stream is ready to emit.
    ///
    /// The call takes as much input as the staging region has room for and writes as much as
    /// `out` holds. It returns `NeedsInput` once it has taken every byte it was given and has
    /// nothing waiting for the output, and `NeedsOutput` otherwise.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the stream is already closing or closed. A failed
    /// encoder stays failed and returns the same error to every later call.
    pub fn encode(&mut self, input: &[u8], out: &mut [u8]) -> Result<Progress, Error> {
        if let Some(error) = self.poison {
            return Err(error);
        }
        if self.closing != Closing::Open || self.emitting == Emitting::Done {
            return Err(self.poisoned(Error::InvalidParameter));
        }

        let mut consumed = 0_usize;
        let mut produced = 0_usize;
        loop {
            let room = self.layout.region_bytes().saturating_sub(self.staged.len());
            let rest = input.get(consumed..).unwrap_or_default();
            if room > 0 && !rest.is_empty() {
                let take = room.min(rest.len());
                let chunk = rest.get(..take).unwrap_or_default();
                self.staged.extend_from_slice(chunk);
                consumed = consumed.saturating_add(take);
                self.accepted = self
                    .accepted
                    .saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
                continue;
            }
            let target = out.get_mut(produced..).unwrap_or_default();
            if target.is_empty() {
                break;
            }
            let moved = self.drain(target)?;
            produced = produced.saturating_add(moved);
            // Writing out the last block of a region frees the staging room, which is
            // progress the caller sees as neither input nor output.
            if moved == 0 && self.layout.region_bytes().saturating_sub(self.staged.len()) == room {
                break;
            }
        }

        let state = if consumed < input.len() || self.blocked() {
            StreamState::NeedsOutput
        } else {
            StreamState::NeedsInput
        };
        Ok(Progress {
            consumed,
            produced,
            state,
        })
    }

    /// Ends the current region and keeps the stream open.
    ///
    /// Everything staged becomes a region, so the bytes written so far decode to every byte
    /// the encoder has taken. A flush with nothing staged writes no region.
    ///
    /// The call returns `NeedsOutput` while bytes remain to write, and `NeedsInput` once the
    /// flush has finished. Call it again with more room until it returns `NeedsInput`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the stream is already finished.
    pub fn flush(&mut self, out: &mut [u8]) -> Result<Progress, Error> {
        if let Some(error) = self.poison {
            return Err(error);
        }
        if self.closing == Closing::Finish || self.emitting == Emitting::Done {
            return Err(self.poisoned(Error::InvalidParameter));
        }
        self.closing = Closing::Flush;
        let produced = self.drain(out)?;
        let state = if self.closing == Closing::Open && !self.blocked() {
            StreamState::NeedsInput
        } else {
            StreamState::NeedsOutput
        };
        Ok(Progress {
            consumed: 0,
            produced,
            state,
        })
    }

    /// Ends the stream.
    ///
    /// Everything staged becomes a region and the terminator follows it. The call returns
    /// `NeedsOutput` while bytes remain to write and `Finished` once the frame is complete.
    /// Call it again with more room until it returns `Finished`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the header declared a content length and the encoder
    /// took a different number of bytes.
    pub fn finish(&mut self, out: &mut [u8]) -> Result<Progress, Error> {
        if let Some(error) = self.poison {
            return Err(error);
        }
        if self.closing != Closing::Finish {
            if let Some(declared) = self.header.content_length
                && declared != self.accepted
            {
                return Err(self.poisoned(Error::InvalidParameter));
            }
            self.closing = Closing::Finish;
        }
        let produced = self.drain(out)?;
        let state = if self.emitting == Emitting::Done && !self.blocked() {
            StreamState::Finished
        } else {
            StreamState::NeedsOutput
        };
        Ok(Progress {
            consumed: 0,
            produced,
            state,
        })
    }

    const fn blocked(&self) -> bool {
        if self.scratch_at < self.scratch_len || self.data_at < self.data_end {
            return true;
        }
        self.staged.len() >= self.layout.region_bytes()
    }

    const fn poisoned(&mut self, error: Error) -> Error {
        self.poison = Some(error);
        error
    }

    fn drain(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        match self.pump(out) {
            Ok(produced) => Ok(produced),
            Err(error) => Err(self.poisoned(error)),
        }
    }

    fn pump(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        let mut produced = 0_usize;
        loop {
            let room = out.get_mut(produced..).unwrap_or_default();
            if room.is_empty() {
                return Ok(produced);
            }
            if self.scratch_at < self.scratch_len {
                let take = self
                    .scratch_len
                    .saturating_sub(self.scratch_at)
                    .min(room.len());
                let end = self.scratch_at.saturating_add(take);
                let source = self
                    .scratch
                    .get(self.scratch_at..end)
                    .ok_or(Error::InvalidParameter)?;
                let target = room.get_mut(..take).ok_or(Error::InvalidParameter)?;
                target.copy_from_slice(source);
                self.scratch_at = end;
                produced = produced.saturating_add(take);
                continue;
            }
            if self.data_at < self.data_end {
                let take = self.data_end.saturating_sub(self.data_at).min(room.len());
                let end = self.data_at.saturating_add(take);
                let source = self
                    .staged
                    .get(self.data_at..end)
                    .ok_or(Error::InvalidParameter)?;
                let target = room.get_mut(..take).ok_or(Error::InvalidParameter)?;
                target.copy_from_slice(source);
                self.data_at = end;
                produced = produced.saturating_add(take);
                continue;
            }
            if !self.advance()? {
                return Ok(produced);
            }
        }
    }

    fn advance(&mut self) -> Result<bool, Error> {
        match self.emitting {
            Emitting::Start => {
                let encoded = self.header.encode();
                self.queue(encoded.as_bytes());
                self.emitting = Emitting::Open;
                Ok(true)
            }
            Emitting::Open => {
                let full = self.staged.len() >= self.layout.region_bytes();
                if !self.staged.is_empty() && (full || self.closing != Closing::Open) {
                    let region = self
                        .layout
                        .region(self.staged.len(), self.header.integrity)?;
                    let encoded = Record::Region(region).encode();
                    self.queue(encoded.as_bytes());
                    self.next_block_at = 0;
                    self.emitting = Emitting::Blocks;
                    return Ok(true);
                }
                if self.staged.is_empty() && self.closing == Closing::Finish {
                    let encoded = Record::Terminator.encode();
                    self.queue(encoded.as_bytes());
                    self.emitting = Emitting::Terminator;
                    return Ok(true);
                }
                if self.staged.is_empty() && self.closing == Closing::Flush {
                    self.closing = Closing::Open;
                }
                Ok(false)
            }
            Emitting::Blocks => {
                if self.next_block_at >= self.staged.len() {
                    self.staged.clear();
                    self.next_block_at = 0;
                    self.emitting = Emitting::Open;
                    return Ok(true);
                }
                let (block, end) = self.layout.block(self.next_block_at, self.staged.len())?;
                let encoded = block.encode();
                self.queue(encoded.as_bytes());
                self.data_at = self.next_block_at;
                self.data_end = end;
                self.next_block_at = end;
                Ok(true)
            }
            Emitting::Terminator => {
                self.emitting = Emitting::Done;
                Ok(false)
            }
            Emitting::Done => Ok(false),
        }
    }

    fn queue(&mut self, bytes: &[u8]) {
        debug_assert!(
            bytes.len() <= SCRATCH_BYTES,
            "the scratch is sized for the widest header the format defines"
        );
        self.scratch_len = 0;
        self.scratch_at = 0;
        for byte in bytes {
            if let Some(slot) = self.scratch.get_mut(self.scratch_len) {
                *slot = *byte;
                self.scratch_len = self.scratch_len.saturating_add(1);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reading {
    Frame,
    Record,
    Block,
    Payload { last: bool, payload: Payload },
    Done,
}

/// Turns a frame back into its content, at whatever chunk size the caller has.
///
/// The decoder validates the frame header against its policy before it reads anything else,
/// validates every later structure at the boundary that owns it, and writes decoded bytes
/// only into the buffer the caller supplies.
///
/// A decoder that has failed is poisoned: every later call returns the same error.
pub struct Decoder {
    policy: DecoderPolicy,
    header: Option<FrameHeader>,
    scratch: [u8; SCRATCH_BYTES],
    scratch_len: usize,
    need: usize,
    reading: Reading,
    region_logical: u64,
    region_physical: u64,
    produced: u64,
    poison: Option<Error>,
}

impl Decoder {
    /// A decoder that admits a frame only when `policy` allows what the frame declares.
    #[must_use]
    pub const fn new(policy: DecoderPolicy) -> Self {
        Self {
            policy,
            header: None,
            scratch: [0; SCRATCH_BYTES],
            scratch_len: 0,
            need: FRAME_HEADER_FIXED_BYTES,
            reading: Reading::Frame,
            region_logical: 0,
            region_physical: 0,
            produced: 0,
            poison: None,
        }
    }

    /// The bytes this decoder holds, beyond the buffers the caller passes it.
    ///
    /// The value is a constant of the format, not of the stream.
    #[must_use]
    pub const fn memory_bytes(&self) -> usize {
        SCRATCH_BYTES
    }

    /// What the frame declared, once its header has arrived and policy has admitted it.
    #[must_use]
    pub const fn header(&self) -> Option<FrameHeader> {
        self.header
    }

    /// Takes compressed input and writes decoded output.
    ///
    /// The call returns `Finished` once the terminator has been read, `NeedsOutput` when the
    /// output buffer filled, and `NeedsInput` when it used everything it was given.
    ///
    /// # Errors
    ///
    /// Returns the error that names the structure the stream violated, and `LimitExceeded`
    /// when the frame declares more than policy allows. A stream that stops early is not an
    /// error here: `finish` reports it.
    pub fn decode(&mut self, input: &[u8], out: &mut [u8]) -> Result<Progress, Error> {
        if let Some(error) = self.poison {
            return Err(error);
        }
        let mut consumed = 0_usize;
        let mut produced = 0_usize;
        loop {
            match self.reading {
                Reading::Done => {
                    return Ok(Progress {
                        consumed,
                        produced,
                        state: StreamState::Finished,
                    });
                }
                Reading::Frame => {
                    let rest = input.get(consumed..).unwrap_or_default();
                    consumed = consumed.saturating_add(self.fill(rest));
                    if self.scratch_len < self.need {
                        return Ok(Progress {
                            consumed,
                            produced,
                            state: StreamState::NeedsInput,
                        });
                    }
                    self.read_frame()?;
                }
                Reading::Record => {
                    let rest = input.get(consumed..).unwrap_or_default();
                    consumed = consumed.saturating_add(self.fill(rest));
                    if self.scratch_len < self.need {
                        return Ok(Progress {
                            consumed,
                            produced,
                            state: StreamState::NeedsInput,
                        });
                    }
                    self.read_record()?;
                }
                Reading::Block => {
                    let rest = input.get(consumed..).unwrap_or_default();
                    consumed = consumed.saturating_add(self.fill(rest));
                    if self.scratch_len < self.need {
                        return Ok(Progress {
                            consumed,
                            produced,
                            state: StreamState::NeedsInput,
                        });
                    }
                    self.read_block()?;
                }
                Reading::Payload { last, mut payload } => {
                    let source = input.get(consumed..).unwrap_or_default();
                    let target = out.get_mut(produced..).unwrap_or_default();
                    let moved = match payload.step(source, target) {
                        Ok(moved) => moved,
                        Err(error) => return Err(self.poisoned(error)),
                    };
                    consumed = consumed.saturating_add(moved.consumed);
                    produced = produced.saturating_add(moved.produced);
                    self.spend(moved.consumed, moved.produced)?;
                    if payload.is_done() {
                        self.end_block(last)?;
                        continue;
                    }
                    self.reading = Reading::Payload { last, payload };
                    if moved.consumed == 0 && moved.produced == 0 {
                        let state = if source.is_empty() {
                            StreamState::NeedsInput
                        } else {
                            StreamState::NeedsOutput
                        };
                        return Ok(Progress {
                            consumed,
                            produced,
                            state,
                        });
                    }
                }
            }
        }
    }

    /// Reports whether the stream reached its terminator.
    ///
    /// Call it once the compressed input is exhausted and `decode` has stopped producing.
    ///
    /// # Errors
    ///
    /// Returns `TruncatedInput` with the bytes the pending structure still needs when the
    /// stream stopped before its terminator, and the poisoned error when the decoder failed.
    pub fn finish(&self) -> Result<(), Error> {
        if let Some(error) = self.poison {
            return Err(error);
        }
        match self.reading {
            Reading::Done => Ok(()),
            Reading::Payload { payload, .. } => Err(Error::TruncatedInput {
                needed: payload.stored_remaining().max(1),
            }),
            Reading::Frame | Reading::Record | Reading::Block => Err(Error::TruncatedInput {
                needed: self.need.saturating_sub(self.scratch_len).max(1),
            }),
        }
    }

    fn read_frame(&mut self) -> Result<(), Error> {
        let held = self.scratch.get(..self.scratch_len).unwrap_or_default();
        match FrameHeader::decode(held) {
            Ok((header, used)) => {
                if let Err(error) = self.policy.admit(&header) {
                    return Err(self.poisoned(error));
                }
                self.header = Some(header);
                self.consume_scratch(used);
                self.need = RECORD_TAG_BYTES;
                self.reading = Reading::Record;
                Ok(())
            }
            Err(Error::TruncatedInput { needed }) => self.widen(needed),
            Err(error) => Err(self.poisoned(error)),
        }
    }

    fn read_record(&mut self) -> Result<(), Error> {
        let Some(frame) = self.header else {
            return Err(self.poisoned(Error::InvalidFormat));
        };
        let held = self.scratch.get(..self.scratch_len).unwrap_or_default();
        match Record::decode(held, frame.integrity) {
            Ok((Record::Terminator, used)) => {
                self.consume_scratch(used);
                if let Some(declared) = frame.content_length
                    && declared != self.produced
                {
                    return Err(self.poisoned(Error::CorruptData(Corruption::ContentLength)));
                }
                self.reading = Reading::Done;
                Ok(())
            }
            Ok((Record::Region(region), used)) => {
                self.consume_scratch(used);
                self.region_logical = region.logical_size;
                self.region_physical = region.physical_size;
                self.need = BLOCK_HEADER_BYTES;
                self.reading = Reading::Block;
                Ok(())
            }
            Err(Error::TruncatedInput { needed }) => self.widen(needed),
            Err(error) => Err(self.poisoned(error)),
        }
    }

    fn read_block(&mut self) -> Result<(), Error> {
        let held = self.scratch.get(..self.scratch_len).unwrap_or_default();
        match BlockHeader::decode(held) {
            Ok((block, used)) => {
                self.consume_scratch(used);
                let cost = u64::try_from(used).unwrap_or(u64::MAX);
                let Some(physical) = self.region_physical.checked_sub(cost) else {
                    return Err(self.poisoned(Error::CorruptData(Corruption::RegionMismatch)));
                };
                self.region_physical = physical;
                let payload = match Payload::new(block) {
                    Ok(payload) => payload,
                    Err(error) => return Err(self.poisoned(error)),
                };
                let stored = u64::try_from(payload.stored_remaining()).unwrap_or(u64::MAX);
                if stored > self.region_physical
                    || u64::from(block.decoded_len()) > self.region_logical
                {
                    return Err(self.poisoned(Error::CorruptData(Corruption::RegionMismatch)));
                }
                self.reading = Reading::Payload {
                    last: block.last,
                    payload,
                };
                Ok(())
            }
            Err(Error::TruncatedInput { needed }) => self.widen(needed),
            Err(error) => Err(self.poisoned(error)),
        }
    }

    const fn end_block(&mut self, last: bool) -> Result<(), Error> {
        if last {
            if self.region_logical != 0 || self.region_physical != 0 {
                return Err(self.poisoned(Error::CorruptData(Corruption::RegionMismatch)));
            }
            self.need = RECORD_TAG_BYTES;
            self.reading = Reading::Record;
            return Ok(());
        }
        if self.region_physical == 0 || self.region_logical == 0 {
            return Err(self.poisoned(Error::CorruptData(Corruption::RegionMismatch)));
        }
        self.need = BLOCK_HEADER_BYTES;
        self.reading = Reading::Block;
        Ok(())
    }

    fn spend(&mut self, stored: usize, decoded: usize) -> Result<(), Error> {
        let stored = u64::try_from(stored).unwrap_or(u64::MAX);
        let decoded = u64::try_from(decoded).unwrap_or(u64::MAX);
        let (Some(physical), Some(logical)) = (
            self.region_physical.checked_sub(stored),
            self.region_logical.checked_sub(decoded),
        ) else {
            return Err(self.poisoned(Error::CorruptData(Corruption::RegionMismatch)));
        };
        self.region_physical = physical;
        self.region_logical = logical;
        self.produced = self.produced.saturating_add(decoded);
        Ok(())
    }

    /// Raises the bytes the pending header needs.
    ///
    /// A structure that does not ask for more than the scratch already holds would make the
    /// loop repeat itself, and one that asks for more than the widest header the format
    /// defines cannot be read at all. Both are refused rather than retried.
    const fn widen(&mut self, needed: usize) -> Result<(), Error> {
        if needed <= self.scratch_len || needed > SCRATCH_BYTES {
            return Err(self.poisoned(Error::CorruptData(Corruption::HeaderWidth)));
        }
        self.need = needed;
        Ok(())
    }

    fn fill(&mut self, input: &[u8]) -> usize {
        let want = self.need.min(SCRATCH_BYTES);
        let take = want.saturating_sub(self.scratch_len).min(input.len());
        let end = self.scratch_len.saturating_add(take);
        let (Some(target), Some(source)) = (
            self.scratch.get_mut(self.scratch_len..end),
            input.get(..take),
        ) else {
            return 0;
        };
        target.copy_from_slice(source);
        self.scratch_len = end;
        take
    }

    fn consume_scratch(&mut self, used: usize) {
        if used >= self.scratch_len {
            self.scratch_len = 0;
            return;
        }
        self.scratch.copy_within(used..self.scratch_len, 0);
        self.scratch_len = self.scratch_len.saturating_sub(used);
    }

    const fn poisoned(&mut self, error: Error) -> Error {
        self.poison = Some(error);
        error
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_BLOCK_BYTES, DEFAULT_REGION_BYTES, Decoder, Encoder, Progress, StreamState,
    };
    use crate::format::{
        BlockHeader, Corruption, DecoderPolicy, Error, Feature, FrameHeader, IntegrityMode, Record,
        RegionHeader, RegionIndependence, ResourceClass,
    };

    /// The layout the malformed-structure fixtures are built on.
    ///
    /// The sizes are small so that one fixture holds several regions and several blocks, and
    /// so that every structural offset below is short enough to state.
    const FIXTURE_REGION_BYTES: usize = 64;
    const FIXTURE_BLOCK_BYTES: u32 = 16;

    const CLASSES: [ResourceClass; 5] = [
        ResourceClass::Minimal,
        ResourceClass::Small,
        ResourceClass::Medium,
        ResourceClass::Large,
        ResourceClass::Huge,
    ];
    const INDEPENDENCE: [RegionIndependence; 2] = [
        RegionIndependence::Dependent,
        RegionIndependence::Independent,
    ];
    const INTEGRITY: [IntegrityMode; 2] = [IntegrityMode::Absent, IntegrityMode::PerRegion];

    fn permissive() -> DecoderPolicy {
        DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes())
    }

    fn plain_header() -> FrameHeader {
        FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::Absent,
        )
    }

    /// The content at a byte position, which does not depend on how the bytes are chunked.
    fn fill_from(at: u64, out: &mut [u8]) {
        let mut position = at;
        for slot in out.iter_mut() {
            let mixed = position.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            *slot = u8::try_from(mixed.wrapping_shr(56)).unwrap_or(0);
            position = position.saturating_add(1);
        }
    }

    fn sample(len: usize) -> Vec<u8> {
        let mut data = vec![0_u8; len];
        fill_from(0, &mut data);
        data
    }

    fn encode_all(
        header: FrameHeader,
        region_bytes: usize,
        block_bytes: u32,
        data: &[u8],
        in_chunk: usize,
        out_chunk: usize,
    ) -> Result<Vec<u8>, Error> {
        let mut encoder = Encoder::with_layout(header, region_bytes, block_bytes)?;
        let mut stream = Vec::new();
        let mut room = vec![0_u8; out_chunk];
        let mut at = 0_usize;
        while at < data.len() {
            let end = at.saturating_add(in_chunk).min(data.len());
            let chunk = data.get(at..end).ok_or(Error::InvalidParameter)?;
            let mut fed = 0_usize;
            while fed < chunk.len() {
                let rest = chunk.get(fed..).ok_or(Error::InvalidParameter)?;
                let progress = encoder.encode(rest, &mut room)?;
                keep(&mut stream, &room, progress)?;
                assert!(
                    progress.consumed > 0 || progress.produced > 0,
                    "the encoder stalled"
                );
                fed = fed.saturating_add(progress.consumed);
            }
            at = end;
        }
        loop {
            let progress = encoder.finish(&mut room)?;
            keep(&mut stream, &room, progress)?;
            if progress.state == StreamState::Finished {
                break;
            }
            assert!(progress.produced > 0, "the encoder stalled while finishing");
        }
        Ok(stream)
    }

    fn decode_all(
        policy: DecoderPolicy,
        stream: &[u8],
        in_chunk: usize,
        out_chunk: usize,
    ) -> Result<Vec<u8>, Error> {
        let mut decoder = Decoder::new(policy);
        let mut out = Vec::new();
        let mut room = vec![0_u8; out_chunk];
        let mut at = 0_usize;
        while at < stream.len() {
            let end = at.saturating_add(in_chunk).min(stream.len());
            let chunk = stream.get(at..end).ok_or(Error::InvalidParameter)?;
            let progress = decoder.decode(chunk, &mut room)?;
            keep(&mut out, &room, progress)?;
            assert!(
                progress.consumed > 0 || progress.produced > 0,
                "the decoder stalled"
            );
            at = at.saturating_add(progress.consumed);
            if progress.state == StreamState::Finished {
                break;
            }
        }
        loop {
            let progress = decoder.decode(&[], &mut room)?;
            keep(&mut out, &room, progress)?;
            if progress.produced == 0 {
                break;
            }
        }
        decoder.finish()?;
        Ok(out)
    }

    fn keep(sink: &mut Vec<u8>, room: &[u8], progress: Progress) -> Result<(), Error> {
        let written = room
            .get(..progress.produced)
            .ok_or(Error::InvalidParameter)?;
        sink.extend_from_slice(written);
        Ok(())
    }

    fn fixture(data: &[u8]) -> Result<Vec<u8>, Error> {
        encode_all(
            plain_header(),
            FIXTURE_REGION_BYTES,
            FIXTURE_BLOCK_BYTES,
            data,
            data.len().max(1),
            4_096,
        )
    }

    fn refuses(stream: &[u8], expected: Error) -> Result<(), Error> {
        let mut decoder = Decoder::new(permissive());
        let mut room = [0_u8; 256];
        let mut at = 0_usize;
        loop {
            let rest = stream.get(at..).unwrap_or_default();
            match decoder.decode(rest, &mut room) {
                Ok(progress) => {
                    at = at.saturating_add(progress.consumed);
                    assert_ne!(
                        progress.state,
                        StreamState::Finished,
                        "the stream decoded to a successful end"
                    );
                    assert!(
                        progress.consumed > 0 || progress.produced > 0,
                        "the decoder stalled instead of refusing"
                    );
                }
                Err(error) => {
                    assert_eq!(error, expected);
                    assert_eq!(
                        decoder.decode(&[], &mut room),
                        Err(expected),
                        "a failed decoder answered a later call"
                    );
                    return Ok(());
                }
            }
        }
    }

    fn with_byte(stream: &[u8], at: usize, value: u8) -> Result<Vec<u8>, Error> {
        let mut bytes = stream.to_vec();
        let slot = bytes.get_mut(at).ok_or(Error::InvalidParameter)?;
        *slot = value;
        Ok(bytes)
    }

    #[test]
    fn an_input_encodes_to_the_same_bytes_at_every_chunk_size() -> Result<(), Error> {
        let data = sample(300_000);
        let whole = encode_all(
            plain_header(),
            DEFAULT_REGION_BYTES,
            DEFAULT_BLOCK_BYTES,
            &data,
            data.len(),
            data.len(),
        )?;
        for in_chunk in [1_usize, 7, 4_096, 65_536, data.len()] {
            for out_chunk in [1_usize, 7, 4_096, 65_536] {
                let stream = encode_all(
                    plain_header(),
                    DEFAULT_REGION_BYTES,
                    DEFAULT_BLOCK_BYTES,
                    &data,
                    in_chunk,
                    out_chunk,
                )?;
                assert_eq!(stream, whole, "input {in_chunk}, output {out_chunk}");
            }
        }
        Ok(())
    }

    #[test]
    fn a_stream_decodes_to_the_same_bytes_at_every_chunk_size() -> Result<(), Error> {
        let data = sample(300_000);
        let stream = encode_all(
            plain_header(),
            DEFAULT_REGION_BYTES,
            DEFAULT_BLOCK_BYTES,
            &data,
            4_096,
            4_096,
        )?;
        for in_chunk in [1_usize, 7, 4_096, 65_536, stream.len()] {
            for out_chunk in [1_usize, 7, 4_096, 65_536] {
                let decoded = decode_all(permissive(), &stream, in_chunk, out_chunk)?;
                assert_eq!(decoded, data, "input {in_chunk}, output {out_chunk}");
            }
        }
        Ok(())
    }

    #[test]
    fn a_round_trip_holds_at_every_size_that_stresses_a_boundary() -> Result<(), Error> {
        let sizes = [
            0_usize, 1, 2, 15, 16, 17, 63, 64, 65, 127, 128, 129, 191, 192, 255, 256, 1_000,
        ];
        for integrity in INTEGRITY {
            let header = FrameHeader::new(
                ResourceClass::Small,
                RegionIndependence::Independent,
                integrity,
            );
            for size in sizes {
                let data = sample(size);
                for in_chunk in [1_usize, 7, size.max(1)] {
                    let stream = encode_all(
                        header,
                        FIXTURE_REGION_BYTES,
                        FIXTURE_BLOCK_BYTES,
                        &data,
                        in_chunk,
                        3,
                    )?;
                    let decoded = decode_all(permissive(), &stream, 5, 2)?;
                    assert_eq!(decoded, data, "size {size}, chunk {in_chunk}");
                }
            }
        }
        Ok(())
    }

    #[test]
    fn the_stream_carries_every_legal_frame_header_combination() -> Result<(), Error> {
        let data = sample(500);
        for class in CLASSES {
            for independence in INDEPENDENCE {
                for integrity in INTEGRITY {
                    for content in [None, Some(500_u64)] {
                        for dictionary in [None, Some(0x0A0B_0C0D_u32)] {
                            let mut header = FrameHeader::new(class, independence, integrity);
                            header.content_length = content;
                            header.dictionary_id = dictionary;
                            let stream = encode_all(
                                header,
                                FIXTURE_REGION_BYTES,
                                FIXTURE_BLOCK_BYTES,
                                &data,
                                64,
                                64,
                            )?;
                            assert_eq!(decode_all(permissive(), &stream, 64, 64)?, data);

                            let mut inspector = Decoder::new(permissive());
                            let mut room = [0_u8; 64];
                            let _ = inspector.decode(&stream, &mut room)?;
                            assert_eq!(inspector.header(), Some(header));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn flush_produces_a_decodable_prefix_and_the_stream_continues() -> Result<(), Error> {
        let first = sample(100);
        let second = sample(150);
        let mut encoder =
            Encoder::with_layout(plain_header(), FIXTURE_REGION_BYTES, FIXTURE_BLOCK_BYTES)?;
        let mut room = [0_u8; 7];
        let mut stream = Vec::new();

        let mut fed = 0_usize;
        while fed < first.len() {
            let rest = first.get(fed..).ok_or(Error::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut room)?;
            keep(&mut stream, &room, progress)?;
            fed = fed.saturating_add(progress.consumed);
        }
        loop {
            let progress = encoder.flush(&mut room)?;
            keep(&mut stream, &room, progress)?;
            if progress.state == StreamState::NeedsInput {
                break;
            }
        }

        let prefix = stream.clone();
        let mut decoder = Decoder::new(permissive());
        let mut out = Vec::new();
        let mut plain = [0_u8; 16];
        let mut at = 0_usize;
        while at < prefix.len() {
            let rest = prefix.get(at..).ok_or(Error::InvalidParameter)?;
            let progress = decoder.decode(rest, &mut plain)?;
            keep(&mut out, &plain, progress)?;
            at = at.saturating_add(progress.consumed);
            if progress.consumed == 0 && progress.produced == 0 {
                break;
            }
        }
        assert_eq!(
            out, first,
            "the flushed prefix did not decode to what was fed"
        );
        assert!(
            matches!(decoder.finish(), Err(Error::TruncatedInput { .. })),
            "the prefix reported a successful end"
        );

        let mut fed = 0_usize;
        while fed < second.len() {
            let rest = second.get(fed..).ok_or(Error::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut room)?;
            keep(&mut stream, &room, progress)?;
            fed = fed.saturating_add(progress.consumed);
        }
        loop {
            let progress = encoder.finish(&mut room)?;
            keep(&mut stream, &room, progress)?;
            if progress.state == StreamState::Finished {
                break;
            }
        }

        let mut whole = first.clone();
        whole.extend_from_slice(&second);
        assert_eq!(decode_all(permissive(), &stream, 5, 5)?, whole);
        assert!(
            stream.starts_with(&prefix),
            "the flush rewrote earlier bytes"
        );
        Ok(())
    }

    #[test]
    fn a_flush_with_nothing_staged_writes_no_region() -> Result<(), Error> {
        let mut encoder = Encoder::new(plain_header())?;
        let mut room = [0_u8; 64];
        let header_bytes = plain_header().encoded_len();

        let first = encoder.flush(&mut room)?;
        assert_eq!(first.produced, header_bytes);
        assert_eq!(first.state, StreamState::NeedsInput);

        let second = encoder.flush(&mut room)?;
        assert_eq!(second.produced, 0);
        assert_eq!(second.state, StreamState::NeedsInput);

        let third = encoder.finish(&mut room)?;
        assert_eq!(third.produced, 1);
        assert_eq!(third.state, StreamState::Finished);
        Ok(())
    }

    #[test]
    fn an_empty_input_finishes_as_a_frame_with_no_region() -> Result<(), Error> {
        let stream = encode_all(
            plain_header(),
            FIXTURE_REGION_BYTES,
            FIXTURE_BLOCK_BYTES,
            &[],
            1,
            64,
        )?;
        assert_eq!(stream.len(), plain_header().encoded_len().saturating_add(1));
        assert!(decode_all(permissive(), &stream, 1, 1)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_truncated_stream_never_reaches_a_successful_end() -> Result<(), Error> {
        let data = sample(200);
        let stream = fixture(&data)?;
        assert_eq!(decode_all(permissive(), &stream, 7, 7)?, data);

        for length in 0..stream.len() {
            let cut = stream.get(..length).ok_or(Error::InvalidParameter)?;
            let mut decoder = Decoder::new(permissive());
            let mut room = [0_u8; 32];
            let mut at = 0_usize;
            let mut finished = false;
            loop {
                let rest = cut.get(at..).unwrap_or_default();
                let progress = decoder.decode(rest, &mut room)?;
                at = at.saturating_add(progress.consumed);
                if progress.state == StreamState::Finished {
                    finished = true;
                    break;
                }
                if progress.consumed == 0 && progress.produced == 0 {
                    break;
                }
            }
            assert!(
                !finished,
                "a stream cut at {length} decoded as a whole frame"
            );
            let ended = decoder.finish();
            assert!(
                matches!(ended, Err(Error::TruncatedInput { needed }) if needed > 0),
                "a stream cut at {length} ended as {ended:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn every_malformed_structure_yields_the_error_that_names_it() -> Result<(), Error> {
        let data = sample(100);
        let stream = fixture(&data)?;
        assert_eq!(decode_all(permissive(), &stream, 9, 9)?, data);

        refuses(&with_byte(&stream, 0, 0x00)?, Error::InvalidFormat)?;
        refuses(
            &with_byte(&stream, 4, 2)?,
            Error::UnsupportedVersion { major: 2 },
        )?;
        refuses(
            &with_byte(&stream, 5, 0b0001_0001)?,
            Error::CorruptData(Corruption::FrameFlag),
        )?;
        refuses(
            &with_byte(&stream, 8, 1)?,
            Error::UnsupportedFeature(Feature::ReservedFrameFlag),
        )?;
        refuses(
            &with_byte(&stream, 12, 1)?,
            Error::CorruptData(Corruption::FrameReserved),
        )?;
        refuses(
            &with_byte(&stream, 6, 9)?,
            Error::CorruptData(Corruption::ResourceClass),
        )?;
        refuses(
            &with_byte(&stream, 7, 9)?,
            Error::CorruptData(Corruption::IntegrityMode),
        )?;
        refuses(
            &with_byte(&stream, 16, 0x7F)?,
            Error::CorruptData(Corruption::RecordTag),
        )?;
        refuses(
            &with_byte(&stream, 17, 0)?,
            Error::CorruptData(Corruption::RegionLogicalSize),
        )?;
        refuses(
            &with_byte(&stream, 25, 4)?,
            Error::CorruptData(Corruption::RegionPhysicalSize),
        )?;
        refuses(
            &with_byte(&stream, 17, 65)?,
            Error::CorruptData(Corruption::RegionMismatch),
        )?;
        refuses(
            &with_byte(&stream, 33, 0x84)?,
            Error::UnsupportedFeature(Feature::CompressedBlock),
        )?;
        refuses(
            &with_byte(&stream, 33, 0x86)?,
            Error::CorruptData(Corruption::BlockType),
        )?;

        let mut zeroed = stream;
        for at in 33..37 {
            let slot = zeroed.get_mut(at).ok_or(Error::InvalidParameter)?;
            *slot = 0;
        }
        refuses(&zeroed, Error::CorruptData(Corruption::BlockSize))?;
        Ok(())
    }

    #[test]
    fn a_declared_content_length_the_frame_does_not_reach_is_corrupt_data() -> Result<(), Error> {
        let data = sample(100);
        let mut header = plain_header();
        header.content_length = Some(100);
        let stream = encode_all(
            header,
            FIXTURE_REGION_BYTES,
            FIXTURE_BLOCK_BYTES,
            &data,
            100,
            4_096,
        )?;
        assert_eq!(decode_all(permissive(), &stream, 9, 9)?, data);

        refuses(
            &with_byte(&stream, 16, 99)?,
            Error::CorruptData(Corruption::ContentLength),
        )
    }

    #[test]
    fn an_encoder_that_declares_a_content_length_refuses_a_different_total() -> Result<(), Error> {
        let mut header = plain_header();
        header.content_length = Some(10);
        let mut encoder = Encoder::with_layout(header, FIXTURE_REGION_BYTES, FIXTURE_BLOCK_BYTES)?;
        let mut room = [0_u8; 256];
        let progress = encoder.encode(&[0_u8; 4], &mut room)?;
        assert_eq!(progress.consumed, 4);
        assert_eq!(encoder.finish(&mut room), Err(Error::InvalidParameter));
        assert_eq!(
            encoder.encode(&[0_u8; 1], &mut room),
            Err(Error::InvalidParameter),
            "a failed encoder answered a later call"
        );
        Ok(())
    }

    #[test]
    fn the_encoder_refuses_to_declare_an_index_it_does_not_write() {
        let mut header = plain_header();
        header.index_location = Some(64);
        assert!(matches!(Encoder::new(header), Err(Error::InvalidParameter)));
    }

    #[test]
    fn a_frame_above_policy_is_refused_before_the_decoder_reads_a_block() -> Result<(), Error> {
        let data = sample(200);
        let header = FrameHeader::new(
            ResourceClass::Huge,
            RegionIndependence::Independent,
            IntegrityMode::Absent,
        );
        let stream = encode_all(
            header,
            FIXTURE_REGION_BYTES,
            FIXTURE_BLOCK_BYTES,
            &data,
            200,
            4_096,
        )?;

        let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
        let mut room = [0_u8; 4_096];
        assert_eq!(
            decoder.decode(&stream, &mut room),
            Err(Error::LimitExceeded {
                declared: ResourceClass::Huge.history_bytes(),
                allowed: ResourceClass::Small.history_bytes(),
            })
        );
        assert_eq!(decoder.header(), None, "a refused frame was recorded");
        Ok(())
    }

    #[test]
    fn a_partial_read_stops_cleanly_and_reports_what_it_produced() -> Result<(), Error> {
        let data = sample(500);
        let stream = fixture(&data)?;
        let mut decoder = Decoder::new(permissive());
        let mut room = [0_u8; 64];
        let mut out = Vec::new();
        let mut at = 0_usize;

        while out.len() < 120 {
            let rest = stream.get(at..).ok_or(Error::InvalidParameter)?;
            let progress = decoder.decode(rest, &mut room)?;
            keep(&mut out, &room, progress)?;
            at = at.saturating_add(progress.consumed);
        }
        assert!(out.len() >= 120);
        assert_eq!(
            out.as_slice(),
            data.get(..out.len()).ok_or(Error::InvalidParameter)?
        );
        assert!(
            matches!(decoder.finish(), Err(Error::TruncatedInput { .. })),
            "a partial read reported a complete stream"
        );

        loop {
            let rest = stream.get(at..).unwrap_or_default();
            let progress = decoder.decode(rest, &mut room)?;
            keep(&mut out, &room, progress)?;
            at = at.saturating_add(progress.consumed);
            if progress.state == StreamState::Finished {
                break;
            }
        }
        decoder.finish()?;
        assert_eq!(out, data, "the resumed read did not finish the stream");
        Ok(())
    }

    #[test]
    fn an_rle_region_decodes_through_a_one_byte_output_buffer() -> Result<(), Error> {
        let header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::PerRegion,
        );
        let block = BlockHeader::rle(true, 4_096)?;
        let mut body = block.encode().as_bytes().to_vec();
        body.push(0x7E);
        let physical = u64::try_from(body.len()).map_err(|_| Error::InvalidParameter)?;
        let region = RegionHeader::new(4_096, physical, header.integrity)?;

        let mut stream = header.encode().as_bytes().to_vec();
        stream.extend_from_slice(Record::Region(region).encode().as_bytes());
        stream.extend_from_slice(&body);
        stream.extend_from_slice(Record::Terminator.encode().as_bytes());

        let decoded = decode_all(permissive(), &stream, 1, 1)?;
        assert_eq!(decoded.len(), 4_096);
        assert!(decoded.iter().all(|byte| *byte == 0x7E));
        assert_eq!(decode_all(permissive(), &stream, 3, 7)?, decoded);
        Ok(())
    }

    #[test]
    fn a_region_that_declares_more_blocks_than_it_holds_is_corrupt_data() -> Result<(), Error> {
        let header = plain_header();
        let block = BlockHeader::raw(true, 4)?;
        let mut body = block.encode().as_bytes().to_vec();
        body.extend_from_slice(&[1, 2, 3, 4]);

        let mut stream = header.encode().as_bytes().to_vec();
        let region = RegionHeader::new(8, 16, header.integrity)?;
        stream.extend_from_slice(Record::Region(region).encode().as_bytes());
        stream.extend_from_slice(&body);
        stream.extend_from_slice(Record::Terminator.encode().as_bytes());

        refuses(&stream, Error::CorruptData(Corruption::RegionMismatch))
    }

    #[test]
    fn the_declared_memory_bound_holds_across_a_decade_of_input_sizes() -> Result<(), Error> {
        let header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::PerRegion,
        );
        let mut bounds = Vec::new();
        for megabytes in [1_usize, 8, 64] {
            let total = megabytes.saturating_mul(1_048_576);
            bounds.push(streamed(header, total)?);
        }
        let first = bounds.first().copied().ok_or(Error::InvalidParameter)?;
        for bound in &bounds {
            assert_eq!(*bound, first, "the bound moved with the input size");
        }
        assert_eq!(
            first,
            (
                DEFAULT_REGION_BYTES.saturating_add(super::SCRATCH_BYTES),
                super::SCRATCH_BYTES
            )
        );
        Ok(())
    }

    /// Streams `total` generated bytes through both machines without holding any of them.
    ///
    /// Nothing here is proportional to `total`: the generated content is produced and checked
    /// in place, and every buffer is fixed before the run.
    fn streamed(header: FrameHeader, total: usize) -> Result<(usize, usize), Error> {
        const CHUNK: usize = 64 * 1024;

        let mut encoder = Encoder::new(header)?;
        let mut decoder = Decoder::new(permissive());
        let bound = (encoder.memory_bytes(), decoder.memory_bytes());

        let mut input = vec![0_u8; CHUNK];
        let mut coded = vec![0_u8; CHUNK];
        let mut plain = vec![0_u8; CHUNK];
        let mut want = vec![0_u8; CHUNK];
        let mut fed = 0_usize;
        let mut checked = 0_u64;

        while fed < total {
            let span = CHUNK.min(total.saturating_sub(fed));
            let chunk = input.get_mut(..span).ok_or(Error::InvalidParameter)?;
            fill_from(
                u64::try_from(fed).map_err(|_| Error::InvalidParameter)?,
                chunk,
            );
            let mut sent = 0_usize;
            while sent < span {
                let rest = chunk.get(sent..).ok_or(Error::InvalidParameter)?;
                let progress = encoder.encode(rest, &mut coded)?;
                sent = sent.saturating_add(progress.consumed);
                verify(
                    &mut decoder,
                    &coded,
                    progress,
                    &mut plain,
                    &mut want,
                    &mut checked,
                )?;
            }
            fed = fed.saturating_add(span);
            assert_eq!(encoder.memory_bytes(), bound.0, "the encoder grew");
        }
        loop {
            let progress = encoder.finish(&mut coded)?;
            verify(
                &mut decoder,
                &coded,
                progress,
                &mut plain,
                &mut want,
                &mut checked,
            )?;
            if progress.state == StreamState::Finished {
                break;
            }
        }
        verify(
            &mut decoder,
            &coded,
            Progress {
                consumed: 0,
                produced: 0,
                state: StreamState::NeedsInput,
            },
            &mut plain,
            &mut want,
            &mut checked,
        )?;

        decoder.finish()?;
        assert_eq!(
            checked,
            u64::try_from(total).map_err(|_| Error::InvalidParameter)?
        );
        assert_eq!(encoder.memory_bytes(), bound.0, "the encoder grew");
        assert_eq!(decoder.memory_bytes(), bound.1, "the decoder grew");
        Ok(bound)
    }

    /// Pushes what the encoder produced through the decoder and checks it against the source.
    fn verify(
        decoder: &mut Decoder,
        coded: &[u8],
        progress: Progress,
        plain: &mut [u8],
        want: &mut [u8],
        checked: &mut u64,
    ) -> Result<(), Error> {
        let mut rest = coded
            .get(..progress.produced)
            .ok_or(Error::InvalidParameter)?;
        loop {
            let moved = decoder.decode(rest, plain)?;
            rest = rest.get(moved.consumed..).ok_or(Error::InvalidParameter)?;
            if moved.produced > 0 {
                let got = plain.get(..moved.produced).ok_or(Error::InvalidParameter)?;
                let expected = want
                    .get_mut(..moved.produced)
                    .ok_or(Error::InvalidParameter)?;
                fill_from(*checked, expected);
                assert_eq!(got, expected, "the stream did not round trip");
                *checked = checked.saturating_add(
                    u64::try_from(moved.produced).map_err(|_| Error::InvalidParameter)?,
                );
            }
            if rest.is_empty() && moved.produced == 0 {
                return Ok(());
            }
        }
    }
}

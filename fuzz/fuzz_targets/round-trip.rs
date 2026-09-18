//! Owns the driver that holds the codec to its first property over arbitrary bytes.
//!
//! ```text
//! decode(encode(x)) == x
//! ```
//!
//! Every other driver reads a parser or a decoder with bytes no encoder wrote. This one reads
//! the pair, with bytes no corpus holds, and it is the only driver whose input is content
//! rather than a stream.
//!
//! ```text
//! byte 0      the region length the encoder cuts at, as an exponent
//! byte 1      the block length it cuts at, as an exponent
//! byte 2      the chunk the content arrives in, and the room the encoder writes into
//! byte 3 on   the content
//! ```
//!
//! The layout is part of the input because a block boundary and a region boundary are where
//! the representation's state is carried and discarded, and content alone would leave both at
//! one setting. The chunking is part of it for the same reason: a stream is identical at every
//! chunk size, and a driver that fixes the chunking never reads the path that splits a
//! structure across two calls.
//!
//! What the driver holds the pair to:
//!
//! ```text
//! no content causes a panic in either direction
//! the decoded bytes are the content, byte for byte
//! the encoder states the type of every block it emitted, and they sum to the blocks
//! ```

#![no_main]

use codec::format::{
    DecoderPolicy, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass,
};
use codec::stream::{Decoder, Encoder, StreamState};
use libfuzzer_sys::fuzz_target;

/// The narrowest and the widest block the driver asks the encoder to cut at.
const BLOCK_EXPONENT: (u32, u32) = (6, 14);
/// The region length, as a multiple of the block length the same input chose.
const REGION_BLOCKS: (u32, u32) = (1, 5);

fuzz_target!(|data: &[u8]| {
    let Some((&region_byte, rest)) = data.split_first() else {
        return;
    };
    let Some((&block_byte, rest)) = rest.split_first() else {
        return;
    };
    let Some((&chunk_byte, content)) = rest.split_first() else {
        return;
    };

    let exponent = BLOCK_EXPONENT.0
        + u32::from(block_byte) % (BLOCK_EXPONENT.1 - BLOCK_EXPONENT.0 + 1);
    let block_bytes = 1_u32 << exponent;
    let blocks = REGION_BLOCKS.0 + u32::from(region_byte) % (REGION_BLOCKS.1 - REGION_BLOCKS.0 + 1);
    let region_bytes = usize::try_from(block_bytes).unwrap_or(1).saturating_mul(
        usize::try_from(blocks).unwrap_or(1),
    );
    let chunk = usize::from(chunk_byte).saturating_add(1);

    let header = FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    );
    let Ok(mut encoder) = Encoder::with_layout(header, region_bytes, block_bytes) else {
        return;
    };

    let mut stream = Vec::new();
    let mut room = vec![0_u8; chunk];
    let mut fed = 0_usize;
    while fed < content.len() {
        let end = fed.saturating_add(chunk).min(content.len());
        let input = content.get(fed..end).unwrap_or_default();
        let Ok(step) = encoder.encode(input, &mut room) else {
            panic!("the encoder refused content it was given");
        };
        stream.extend_from_slice(room.get(..step.produced).unwrap_or_default());
        fed = fed.saturating_add(step.consumed);
        if step.consumed == 0 && step.produced == 0 {
            panic!("the encoder made no progress");
        }
    }
    loop {
        let Ok(step) = encoder.finish(&mut room) else {
            panic!("the encoder refused to finish");
        };
        stream.extend_from_slice(room.get(..step.produced).unwrap_or_default());
        if step.state == StreamState::Finished {
            break;
        }
        if step.produced == 0 {
            panic!("the encoder stalled while finishing");
        }
    }

    let counted = encoder.statistics();
    assert_eq!(
        counted.blocks,
        counted
            .raw_blocks
            .saturating_add(counted.rle_blocks)
            .saturating_add(counted.compressed_blocks),
        "the encoder emitted blocks it did not state the type of"
    );

    let policy = DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes());
    let mut decoder = Decoder::new(policy);
    let mut decoded = Vec::new();
    let mut at = 0_usize;
    loop {
        let end = at.saturating_add(chunk).min(stream.len());
        let input = stream.get(at..end).unwrap_or_default();
        let Ok(step) = decoder.decode(input, &mut room) else {
            panic!("the decoder refused a stream this encoder wrote");
        };
        decoded.extend_from_slice(room.get(..step.produced).unwrap_or_default());
        at = at.saturating_add(step.consumed);
        if step.state == StreamState::Finished {
            break;
        }
        if step.consumed == 0 && step.produced == 0 {
            panic!("the decoder made no progress");
        }
    }
    if decoder.finish().is_err() {
        panic!("the decoder did not accept the end of a stream this encoder wrote");
    }
    assert!(decoded == content, "the round trip is not exact");
});

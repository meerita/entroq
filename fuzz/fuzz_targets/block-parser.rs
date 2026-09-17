//! Owns the driver that feeds hostile bytes to the COMPRESSED block.
//!
//! The block payload is the widest untrusted structure the format defines. It declares a
//! symbol model, a peak table memory, a mode field, four symbol counts and twelve section
//! extents, and every one of them sizes work a decoder would otherwise do on the stream's
//! word. The block module is not public, so this driver reaches it the way a caller does: it
//! wraps the input in the smallest frame that carries one COMPRESSED block and drives the
//! streaming decoder over it.
//!
//! ```text
//! byte 0, 1   the decoded size the block header declares, plus one
//! byte 2 on   the block's stored payload, from the model byte onward
//! ```
//!
//! The envelope is built rather than fuzzed, so almost every byte of an input lands on the
//! prologue and the stored body instead of on a magic number the parser rejects in four bytes.
//! The frame parser and the streaming decoder have drivers of their own.
//!
//! What the driver holds the decoder to:
//!
//! ```text
//! no payload causes a panic
//! a call never produces more than the output buffer it was given
//! no payload makes the decoder hold more table memory than policy admits
//! a decoder that produced the block's declared size and then refused is a contradiction
//! ```
//!
//! The third is the rule the whole block layout exists to serve. The tables are the one
//! allocation a block's declarations size, and the declared peak is checked before any of them
//! is built, so a payload that reaches a wider set than policy admits is a defect in the check
//! and not in the stream.

#![no_main]

use codec::format::{
    BLOCK_HEADER_BYTES, BlockHeader, DecoderPolicy, FrameHeader, IntegrityMode, Record,
    RegionHeader, RegionIndependence, ResourceClass,
};
use codec::stream::{Decoder, StreamState};
use libfuzzer_sys::fuzz_target;

/// The decoded bytes one input may produce before the driver stops driving.
const OUTPUT_CEILING: usize = 1 << 18;

fuzz_target!(|data: &[u8]| {
    let Some((&high, rest)) = data.split_first() else {
        return;
    };
    let Some((&low, payload)) = rest.split_first() else {
        return;
    };
    if payload.is_empty() {
        return;
    }

    let decoded_size = (u32::from(high) << 8 | u32::from(low)).saturating_add(1);
    let Ok(block) = BlockHeader::compressed(true, decoded_size) else {
        return;
    };
    let physical = u64::try_from(BLOCK_HEADER_BYTES.saturating_add(payload.len())).unwrap_or(0);
    let header = FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    );
    let Ok(region) = RegionHeader::new(u64::from(decoded_size), physical, IntegrityMode::Absent)
    else {
        return;
    };

    let mut stream = Vec::new();
    stream.extend_from_slice(header.encode().as_bytes());
    stream.extend_from_slice(Record::Region(region).encode().as_bytes());
    stream.extend_from_slice(block.encode().as_bytes());
    stream.extend_from_slice(payload);
    stream.extend_from_slice(Record::Terminator.encode().as_bytes());

    let policy = DecoderPolicy::CONSERVATIVE;
    let admitted = policy.max_table_bytes();
    let mut decoder = Decoder::new(policy);
    let mut out = vec![0_u8; 4_096];
    let mut at = 0_usize;
    let mut produced = 0_usize;
    let mut refused = false;

    loop {
        let input = stream.get(at..).unwrap_or_default();
        let Ok(step) = decoder.decode(input, &mut out) else {
            refused = true;
            break;
        };

        assert!(step.consumed <= input.len(), "consumed beyond the input");
        assert!(step.produced <= out.len(), "produced beyond the output");

        at = at.saturating_add(step.consumed);
        produced = produced.saturating_add(step.produced);

        if step.state == StreamState::Finished || produced >= OUTPUT_CEILING {
            break;
        }
        // Neither buffer moved, so the next call with the same buffers cannot move either.
        if step.consumed == 0 && step.produced == 0 {
            break;
        }
    }

    let held = decoder.peak_table_bytes();
    assert!(
        held <= admitted,
        "the decoder held {held} table bytes against the {admitted} policy admits"
    );
    if refused {
        assert!(
            produced < usize::try_from(decoded_size).unwrap_or(usize::MAX),
            "the block produced every byte it declared and was then refused"
        );
    }
    let _ = std::hint::black_box(decoder.finish());
});

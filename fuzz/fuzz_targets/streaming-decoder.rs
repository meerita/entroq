//! Owns the driver that drives the streaming decoder with hostile bytes.
//!
//! The streaming decoder is the widest untrusted surface the codec has: it reads the frame
//! header, every record, every block header, and every payload, and it does so across chunk
//! boundaries the input chooses. This driver makes the chunking part of the input, so one
//! corpus entry covers both the bytes and the arrival pattern that splits them.
//!
//! ```text
//! byte 0      the input chunk size the decoder is fed, plus one
//! byte 1      the output buffer size the decoder writes into, plus one
//! byte 2 on   the stream
//! ```
//!
//! Both sizes are one to 256 bytes, which is below every header the format defines, so a
//! structure is split across calls far more often than it arrives whole.
//!
//! What the driver holds the decoder to:
//!
//! ```text
//! no input causes a panic
//! a call never consumes more than the input it was given
//! a call never produces more than the output buffer it was given
//! the bytes the decoder holds do not grow with the stream
//! ```
//!
//! The ceiling below is the driver's bound, not the decoder's. The decoder writes only into
//! the buffer it is handed, so a caller decides how long to keep driving it; a small stream
//! that declares a long run of bytes is expansion the caller stops, not allocation the
//! decoder performs. Without a ceiling one input could ask this driver to walk hundreds of
//! gigabytes through a 1-byte buffer, which reads as a hang and finds nothing.

#![no_main]

use codec::format::DecoderPolicy;
use codec::stream::{Decoder, StreamState};
use libfuzzer_sys::fuzz_target;

/// The decoded bytes one input may produce before the driver stops driving.
const OUTPUT_CEILING: usize = 1 << 18;

fuzz_target!(|data: &[u8]| {
    let Some((&chunk_byte, rest)) = data.split_first() else {
        return;
    };
    let Some((&output_byte, stream)) = rest.split_first() else {
        return;
    };

    let chunk = usize::from(chunk_byte).saturating_add(1);
    let mut out = vec![0_u8; usize::from(output_byte).saturating_add(1)];

    let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
    let held = decoder.steady_state_bytes();
    let mut at = 0_usize;
    let mut produced = 0_usize;

    loop {
        let end = at.saturating_add(chunk).min(stream.len());
        let input = stream.get(at..end).unwrap_or_default();
        let Ok(step) = decoder.decode(input, &mut out) else {
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
        // Every other outcome advanced the input or filled the output, so the loop ends.
        if step.consumed == 0 && step.produced == 0 {
            break;
        }
    }

    let _ = std::hint::black_box(decoder.finish());
    assert_eq!(
        decoder.steady_state_bytes(),
        held,
        "the decoder grew with the stream"
    );
});

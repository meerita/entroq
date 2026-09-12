//! Owns the driver that feeds hostile bytes to the frame header parser.
//!
//! The frame header is the first structure a decoder reads and the only one that decides
//! whether the stream is Entroq at all, so it is the boundary that sees the most bytes no
//! encoder wrote. Every input here is a candidate frame header, and nothing later in a
//! stream is read.
//!
//! A parse that fails is the expected outcome for almost every input and is not a finding.
//! A parse that succeeds is held to the contract the format states:
//!
//! ```text
//! the parser reports the bytes it consumed, and never more than it was given
//! the consumed count is the width the parsed header declares
//! serializing the parsed header reproduces those bytes exactly
//! parsing the serialized bytes yields the same header again
//! ```
//!
//! The last two are what make the header a fixed point: a header that decodes from bytes it
//! does not encode back to is a header two implementations can disagree about.
//!
//! The parsed header is then offered to the conservative decoder policy, so the path that
//! refuses a frame before anything is allocated for it is covered by the same input.

#![no_main]

use codec::format::{DecoderPolicy, FrameHeader};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok((header, used)) = FrameHeader::decode(data) else {
        return;
    };

    assert!(used <= data.len(), "consumed {used} of {}", data.len());
    assert_eq!(used, header.encoded_len(), "consumed width");

    let encoded = header.encode();
    assert_eq!(encoded.len(), used, "serialized width");
    assert_eq!(
        encoded.as_bytes(),
        data.get(..used).unwrap_or_default(),
        "serializing the parsed header changed the bytes"
    );

    match FrameHeader::decode(encoded.as_bytes()) {
        Ok((again, width)) => {
            assert_eq!(again, header, "the header is not a fixed point");
            assert_eq!(width, used, "the width is not a fixed point");
        }
        Err(error) => panic!("the serialized header does not parse: {error}"),
    }

    let _ = std::hint::black_box(DecoderPolicy::CONSERVATIVE.admit(&header));
});

//! Owns the driver that feeds hostile bytes to the two entropy table parsers.
//!
//! A table description is the one structure in a block whose fields size an allocation. The
//! decoder's rule is that the size is computed from the description alone, checked against the
//! ceiling, and only then allocated, so this driver reads a description the way the block does
//! and holds the rule at each step.
//!
//! ```text
//! byte 0      the coder and the alphabet: the low bit picks Huffman or rANS, the next two
//!             pick which of the four symbol classes the description is read against
//! byte 1 on   the description
//! ```
//!
//! A parse that fails is the expected outcome for almost every input and is not a finding. A
//! parse that succeeds is held to the contract the format states:
//!
//! ```text
//! a validated description declares its table bytes before a table exists
//! no admitted description declares more than one block may declare
//! the table the description builds occupies exactly the bytes it declared
//! a description is never parsed past the bits it was given
//! ```
//!
//! The third is the one that matters most. The declared figure is what a decoder refuses a
//! block by, and a figure that is not the bytes the table then takes would make every refusal
//! a guess.

#![no_main]

use codec::entropy::bits::BitReader;
use codec::entropy::{MAX_BLOCK_TABLE_BYTES, huffman, rans};
use codec::sequence::Alphabet;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, description)) = data.split_first() else {
        return;
    };
    let alphabet = match selector >> 1 & 0b11 {
        0 => Alphabet::LiteralByte,
        1 => Alphabet::LiteralRun,
        2 => Alphabet::MatchLength,
        _ => Alphabet::MatchDistance,
    };
    let bits = u64::try_from(description.len()).unwrap_or(0).saturating_mul(8);
    let mut reader = BitReader::new(description, bits);

    let (declared, built) = if selector & 1 == 0 {
        let Ok(parsed) = huffman::Declared::parse(&mut reader, alphabet.size()) else {
            return;
        };
        let Ok(admitted) = parsed.validate() else {
            return;
        };
        let declared = admitted.table_bytes();
        assert!(
            declared <= MAX_BLOCK_TABLE_BYTES,
            "one Huffman table declared {declared} bytes"
        );
        let Ok(table) = admitted.build() else {
            return;
        };
        (declared, table.allocated_bytes())
    } else {
        let Ok(parsed) = rans::Declared::parse(&mut reader, alphabet.size()) else {
            return;
        };
        let Ok(admitted) = parsed.validate() else {
            return;
        };
        let declared = admitted.table_bytes();
        assert!(
            declared <= MAX_BLOCK_TABLE_BYTES,
            "one rANS table declared {declared} bytes"
        );
        let Ok(table) = admitted.build() else {
            return;
        };
        (declared, table.allocated_bytes())
    };

    assert!(
        !reader.overrun(),
        "the description was read past the bits it declared"
    );
    assert_eq!(
        built, declared,
        "the table occupies bytes the description did not declare"
    );
});

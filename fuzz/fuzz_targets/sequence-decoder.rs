//! Owns the driver that feeds hostile symbols to the sequence decoder.
//!
//! The four symbol streams arrive from an entropy decoder, so every symbol and every suffix
//! bit in them is chosen by whoever wrote the block. The sequence decoder is the boundary that
//! turns them into literal runs and matches, and it is the boundary that owns which symbol an
//! alphabet holds, which suffix a symbol declares, where the terminal symbol may occur, and
//! what the repeat code names.
//!
//! ```text
//! byte 0      how many of the input's symbols go to each of the three sequence streams
//! byte 1      whether the offset slot starts set, and to what
//! byte 2 on   the symbols, two bytes each, and then the suffix bits of the three streams
//! ```
//!
//! What the driver holds the decoder to:
//!
//! ```text
//! no symbol stream causes a panic
//! a decoded sequence vector re-codes to itself under the same starting slot
//! ```
//!
//! The second is the property that makes the representation a representation. It is asserted
//! in the encode-first direction, which is the canonical one: a stream a decoder accepts may
//! code a distance the slot already names without using the repeat code, and re-coding it
//! would use the repeat code, so the decode-first direction is not a fixed point and the
//! format does not claim it is.

#![no_main]

use codec::entropy::bits::BitBuf;
use codec::sequence::cache::OffsetCache;
use codec::sequence::streams::{Streams, SymbolStream};
use libfuzzer_sys::fuzz_target;

/// The symbols one input may name, which bounds the work one input asks for.
const SYMBOL_CEILING: usize = 4_096;

fuzz_target!(|data: &[u8]| {
    let Some((&shape, rest)) = data.split_first() else {
        return;
    };
    let Some((&slot, body)) = rest.split_first() else {
        return;
    };

    let sequences = usize::from(shape & 0x3F).min(SYMBOL_CEILING);
    let literals = usize::from(shape >> 6).saturating_mul(sequences);
    let symbols: Vec<u16> = body
        .chunks_exact(2)
        .map(|pair| {
            let high = pair.first().copied().unwrap_or(0);
            let low = pair.last().copied().unwrap_or(0);
            u16::from(high) << 8 | u16::from(low)
        })
        .collect();

    let taken = |at: usize, count: usize| -> Vec<u16> {
        symbols
            .get(at..at.saturating_add(count))
            .unwrap_or_default()
            .to_vec()
    };
    let suffix = body.len() / 4;
    let bits = |count: usize| -> BitBuf {
        let bytes = body.get(..count).unwrap_or_default().to_vec();
        let declared = u64::try_from(bytes.len()).unwrap_or(0).saturating_mul(8);
        BitBuf::new(bytes, declared).unwrap_or_default()
    };

    let mut at = 0_usize;
    let literal_byte = SymbolStream::new(taken(at, literals), BitBuf::default());
    at = at.saturating_add(literals);
    let literal_run = SymbolStream::new(taken(at, sequences), bits(suffix));
    at = at.saturating_add(sequences);
    let match_length = SymbolStream::new(taken(at, sequences), bits(suffix));
    at = at.saturating_add(sequences);
    let match_distance = SymbolStream::new(taken(at, sequences), bits(suffix));

    let streams = Streams::new(literal_byte, literal_run, match_length, match_distance);
    let start = if slot == 0 {
        OffsetCache::reset()
    } else {
        let mut cache = OffsetCache::reset();
        cache.use_distance(u32::from(slot));
        cache
    };

    let mut cache = start;
    let Ok(decoded) = streams.sequences(&mut cache) else {
        return;
    };

    let mut again = start;
    let Ok(recoded) = Streams::of(&decoded, &mut again) else {
        panic!("a decoded sequence vector did not re-code");
    };
    let mut third = start;
    match recoded.sequences(&mut third) {
        Ok(round) => assert!(
            round == decoded,
            "the sequence vector is not a fixed point of its own coding"
        ),
        Err(error) => panic!("a re-coded sequence vector did not decode: {error}"),
    }
    assert_eq!(again, third, "the two directions left different slots");
});

//! The frozen frame identity target for the eight registered representative entries.
//!
//! Every entry is written as one version 1 frame in both Entroq modes at one rung, and the
//! frame length and FNV-1a digest are pinned. The digest covers the frame length and every
//! frame byte, so a byte that moves fails even when the length does not.
//!
//! This is the identity a decoder-only change must preserve. It reads the entries the M9
//! decoder work was selected over, in both modes, so the closing validation has a committed
//! target to compare against.
//!
//! The corpus lives outside the repository and is materialized by the corpus tooling. A host
//! that does not hold it reports so and measures nothing, which is the one thing this file
//! does conditionally.

use std::env;
use std::path::PathBuf;

use codec::format::{
    DecoderPolicy, Error, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass,
};
use codec::stream::{Decoder, Encoder, StreamState};

/// The input bytes this target reads from each entry.
///
/// It is the prefix the M9 decoder investigations measured over, so the frame identity and the
/// selection evidence are read at the same workload.
const RUNG_BYTES: usize = 262_144;

/// The block length both modes cut the rung into. It is the block length the selection rule
/// was measured at.
const BLOCK_BYTES: u32 = 65_536;

/// One entry's frame identity in both modes.
struct Pin {
    /// The entry's cached file name.
    file: &'static str,
    /// The input bytes read from it.
    bytes: usize,
    fast_bytes: usize,
    fast_digest: u64,
    balanced_bytes: usize,
    balanced_digest: u64,
}

/// The eight registered representative entries, in registry order.
const PINS: [Pin; 8] = [
    Pin {
        file: "project-source-small.rs",
        bytes: 32_768,
        fast_bytes: 3_057,
        fast_digest: 0xD0B0_0D0F_020C_DA56,
        balanced_bytes: 1_811,
        balanced_digest: 0xC259_992B_1C28_E0C8,
    },
    Pin {
        file: "project-source-medium.rs",
        bytes: RUNG_BYTES,
        fast_bytes: 21_700,
        fast_digest: 0x3626_F9B2_6826_09EC,
        balanced_bytes: 9_982,
        balanced_digest: 0x30FD_0F7A_FE1F_8B73,
    },
    Pin {
        file: "project-json-medium.json",
        bytes: RUNG_BYTES,
        fast_bytes: 53_905,
        fast_digest: 0x314A_18AE_591A_82D0,
        balanced_bytes: 40_202,
        balanced_digest: 0x2D1A_F586_0D9F_DB9B,
    },
    Pin {
        file: "project-logs-large.log",
        bytes: RUNG_BYTES,
        fast_bytes: 67_155,
        fast_digest: 0x4765_ED07_E5CC_9290,
        balanced_bytes: 54_412,
        balanced_digest: 0xD423_05E4_6479_2694,
    },
    Pin {
        file: "gutenberg-shakespeare",
        bytes: RUNG_BYTES,
        fast_bytes: 112_320,
        fast_digest: 0x0A7A_267A_E9C8_3CD6,
        balanced_bytes: 98_806,
        balanced_digest: 0xC527_2D58_D85C_43E3,
    },
    Pin {
        file: "project-database-rows-medium.tsv",
        bytes: RUNG_BYTES,
        fast_bytes: 108_548,
        fast_digest: 0xE0EE_873F_9C1E_19CE,
        balanced_bytes: 100_169,
        balanced_digest: 0xD66E_8BD0_8E74_DD03,
    },
    Pin {
        file: "project-serialized-binary-medium.bin",
        bytes: RUNG_BYTES,
        fast_bytes: 194_724,
        fast_digest: 0xCD67_E9CC_A940_C448,
        balanced_bytes: 191_854,
        balanced_digest: 0x2390_7AB3_1CA7_41DF,
    },
    Pin {
        file: "project-long-repetitions-medium.bin",
        bytes: RUNG_BYTES,
        fast_bytes: 5_491,
        fast_digest: 0x029E_7F7F_EA06_5C34,
        balanced_bytes: 5_226,
        balanced_digest: 0xA3ED_6F0A_A735_4A4F,
    },
];

/// The FNV-1a digest of a frame: its length and every byte, in order.
fn digest(stream: &[u8]) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    let mix = |hash: &mut u64, value: u64| {
        for shift in 0..8u32 {
            *hash ^= (value >> shift.saturating_mul(8)) & 0xFF;
            *hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    mix(&mut hash, u64::try_from(stream.len()).unwrap_or(u64::MAX));
    for &byte in stream {
        mix(&mut hash, u64::from(byte));
    }
    hash
}

/// Where the corpus cache sits, which the corpus tooling names the same way.
fn cache() -> PathBuf {
    env::var("CORPUS")
        .map_or_else(
            |_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join("corpus")
            },
            PathBuf::from,
        )
        .join("cache")
}

/// Encodes one entry through the FAST operating point at one region and one block.
fn frame_fast(data: &[u8]) -> Result<Vec<u8>, Error> {
    let header = FrameHeader::new(
        ResourceClass::Minimal,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    );
    drive(Encoder::with_layout(header, RUNG_BYTES, BLOCK_BYTES)?, data)
}

/// Encodes one entry through the BALANCED operating point at one region and one block.
fn frame_balanced(data: &[u8]) -> Result<Vec<u8>, Error> {
    let header = FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    );
    drive(
        Encoder::balanced_with_layout(header, RUNG_BYTES, BLOCK_BYTES)?,
        data,
    )
}

/// Feeds one encoder and collects its frame.
fn drive(mut encoder: Encoder, data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut stream = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut fed = 0_usize;
    while fed < data.len() {
        let rest = data.get(fed..).ok_or(Error::InvalidParameter)?;
        let progress = encoder.encode(rest, &mut room)?;
        stream.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        fed = fed.saturating_add(progress.consumed);
        assert!(
            progress.consumed > 0 || progress.produced > 0,
            "the encoder stalled"
        );
    }
    loop {
        let progress = encoder.finish(&mut room)?;
        stream.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        if progress.state == StreamState::Finished {
            break;
        }
    }
    Ok(stream)
}

/// Decodes a whole frame and checks it against its content.
fn expand(stream: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(DecoderPolicy::CONSERVATIVE);
    let mut out = Vec::new();
    let mut room = vec![0_u8; 8_192];
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, &mut room)?;
        out.extend_from_slice(
            room.get(..progress.produced)
                .ok_or(Error::InvalidParameter)?,
        );
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            break;
        }
        assert!(
            progress.consumed > 0 || progress.produced > 0,
            "the decoder stalled"
        );
    }
    decoder.finish()?;
    Ok(out)
}

#[test]
fn every_registered_entry_keeps_its_fast_and_balanced_frame_identity() -> Result<(), Error> {
    let cache = cache();
    if !cache.is_dir() {
        println!(
            "frame-identity: {} does not hold the corpus, so nothing was measured",
            cache.display()
        );
        return Ok(());
    }

    let mut measured = Vec::new();
    for pin in &PINS {
        let path = cache.join(pin.file);
        let Ok(whole) = std::fs::read(&path) else {
            println!(
                "frame-identity: {} is not in the corpus, so nothing was measured",
                path.display()
            );
            return Ok(());
        };
        let data = whole
            .get(..pin.bytes.min(whole.len()))
            .ok_or(Error::InvalidParameter)?;
        assert_eq!(
            data.len(),
            pin.bytes,
            "{} holds {} bytes and the target reads {}",
            pin.file,
            whole.len(),
            pin.bytes
        );

        let fast = frame_fast(data)?;
        let balanced = frame_balanced(data)?;
        assert_eq!(expand(&fast)?, data, "{} FAST did not round trip", pin.file);
        assert_eq!(
            expand(&balanced)?,
            data,
            "{} BALANCED did not round trip",
            pin.file
        );
        println!(
            "frame-identity: {} {} -> fast {} {:016X}, balanced {} {:016X}",
            pin.file,
            pin.bytes,
            fast.len(),
            digest(&fast),
            balanced.len(),
            digest(&balanced),
        );
        measured.push((fast, balanced));
    }

    for (pin, (fast, balanced)) in PINS.iter().zip(measured.iter()) {
        assert_eq!(
            fast.len(),
            pin.fast_bytes,
            "{} moved from its recorded FAST frame bytes",
            pin.file
        );
        assert_eq!(
            digest(fast),
            pin.fast_digest,
            "{} moved from its recorded FAST frame digest at equal length",
            pin.file
        );
        assert_eq!(
            balanced.len(),
            pin.balanced_bytes,
            "{} moved from its recorded BALANCED frame bytes",
            pin.file
        );
        assert_eq!(
            digest(balanced),
            pin.balanced_digest,
            "{} moved from its recorded BALANCED frame digest at equal length",
            pin.file
        );
    }
    Ok(())
}

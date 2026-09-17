//! Whether two processes parse one input into the same sequences.
//!
//! Determinism inside one process proves that no run depends on the one before it. It does not
//! prove that nothing depends on an address, because two runs in one process see the same
//! address space layout. A second process does not, so the parse is run again in one and the
//! two fingerprints are compared.
//!
//! This file owns no codec behavior. It drives the public API and nothing else.

use std::process::Command;

use codec::parser::{MAX_PARSE_BYTES, Parser};

/// Set on the child, which computes the fingerprint, prints it, and returns.
const CHILD: &str = "ENTROQ_PARSE_FINGERPRINT_CHILD";

/// The test the child is asked to run, and the line it prints.
const TEST: &str = "the_same_input_produces_the_same_sequences_in_another_process";
const MARKER: &str = "entroq-parse-fingerprint ";

/// The input length the fingerprint is taken over.
///
/// Long enough to cross several blocks and to fill the window, short enough that a second
/// process is cheap.
const LENGTH: usize = 400_000;

/// The bytes the fingerprint is taken over: a mix of content a finder exploits and content it
/// cannot, so the parse emits both matches and long literal runs.
fn input() -> Vec<u8> {
    (0..LENGTH)
        .map(|at| {
            let value = u64::try_from(at).unwrap_or(0);
            if at.checked_rem(1_024).unwrap_or(0) < 512 {
                let phrase = b"a repeated phrase of twenty-nine!";
                phrase
                    .get(at.checked_rem(phrase.len()).unwrap_or(0))
                    .copied()
                    .unwrap_or(b'?')
            } else {
                let mixed = value
                    .wrapping_mul(0xBF58_476D_1CE4_E5B9)
                    .rotate_left(31)
                    .wrapping_mul(0x94D0_49BB_1331_11EB);
                u8::try_from((mixed >> 33) & 0xFF).unwrap_or(0)
            }
        })
        .collect()
}

/// A fingerprint over every literal byte and every step of every block.
///
/// FNV-1a, because the value only has to differ when the parse differs and has to be the same
/// arithmetic in both processes.
fn fingerprint() -> Result<u64, String> {
    let data = input();
    let mut parser = Parser::new();
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut mix = |value: u64| {
        for shift in 0..8u32 {
            hash ^= (value >> (shift.saturating_mul(8))) & 0xFF;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    for chunk in data.chunks(MAX_PARSE_BYTES) {
        let sequences = parser.parse(chunk).map_err(|error| format!("{error:?}"))?;
        mix(u64::try_from(sequences.literals().len()).unwrap_or(0));
        for &byte in sequences.literals() {
            mix(u64::from(byte));
        }
        mix(u64::try_from(sequences.steps().len()).unwrap_or(0));
        for step in sequences.steps() {
            mix(u64::from(step.run));
            match step.matched {
                Some(matched) => {
                    mix(u64::from(matched.length));
                    mix(u64::from(matched.distance));
                }
                None => mix(u64::MAX),
            }
        }
    }
    Ok(hash)
}

#[test]
fn the_same_input_produces_the_same_sequences_in_another_process() -> Result<(), String> {
    let here = fingerprint()?;
    if std::env::var_os(CHILD).is_some() {
        println!("{MARKER}{here:016x}");
        return Ok(());
    }

    assert_eq!(here, fingerprint()?, "two runs in one process disagree");

    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let output = Command::new(exe)
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, "1")
        .output()
        .map_err(|error| error.to_string())?;
    assert!(
        output.status.success(),
        "the second process did not finish: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    let reported = printed
        .lines()
        .find_map(|line| line.strip_prefix(MARKER))
        .ok_or_else(|| format!("the second process printed no fingerprint:\n{printed}"))?;
    assert_eq!(
        reported,
        format!("{here:016x}"),
        "two processes parsed one input differently"
    );
    Ok(())
}

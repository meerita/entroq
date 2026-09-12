//! Owns the driver that proves the fuzz mechanism runs.
//!
//! This driver tests no Entroq behavior and covers no Entroq code. It exists so that the
//! runner, the segment budget, and the persistent corpus can be exercised on their own,
//! separately from any driver that reads a parser or a decoder. A runner defect and a codec
//! defect then cannot be mistaken for each other.
//!
//! A passing segment here is not codec coverage. It says that one bounded segment ran, that
//! it stopped inside its budget, and that its corpus survived the invocation.
//!
//! The body walks the input through a small branch set, so libFuzzer has edges to discover
//! and the corpus grows. Every operation is total, so no input reaches a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut state: u32 = 0;
    for byte in data {
        state = match byte & 0b0000_0111 {
            0 => state.wrapping_add(u32::from(*byte)),
            1 => state.rotate_left(3),
            2 => state ^ 0x5555_5555,
            3 => state.wrapping_mul(2_654_435_761),
            4 => state >> 1,
            5 => state.reverse_bits(),
            6 => state.swap_bytes(),
            _ => state.count_ones(),
        };
    }
    let _ = std::hint::black_box(state);
});

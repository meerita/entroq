//! Owns the deterministic shapes the project corpus is generated from.
//!
//! Every byte a generated entry holds comes from its recorded seed through the arithmetic
//! defined here. A generator reads no clock, no environment variable, no filesystem, and no
//! container whose iteration order is unspecified, and it names no platform word size in the
//! sequence it produces. The same seed therefore produces the same bytes on every host and
//! every architecture.
//!
//! Each shape is one production-shaped data class. Two shapes exist when a compressor sees
//! them differently, not when a human would name them differently.
//!
//! This module does not own which entries exist, how large they are, or which seed each one
//! carries.

use std::io::{self, Write};

/// How much a generator appends before the driver takes what it needs.
///
/// A shape appends whole records, so the last record of an entry is cut wherever the byte
/// target falls. The cut is part of the recorded output, not an accident of buffering.
const BLOCK: usize = 16 * 1024;

/// The repeated unit of the long-repetition shape.
const PATTERN_BYTES: usize = 4 * 1024;

/// How many distinct tokens the short-token shape draws from.
const TOKEN_COUNT: usize = 48;

/// One production-shaped data class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Json,
    Logs,
    SourceCode,
    DatabaseRows,
    SerializedBinary,
    MixedBinaryText,
    Zeros,
    LongRepetitions,
    ShortRepeatedTokens,
    HighEntropy,
    AlreadyCompressed,
    SparseStructures,
    LargeBlob,
}

impl Shape {
    /// The name the registry, a segment, and a result use for this class.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Logs => "logs",
            Self::SourceCode => "source-code",
            Self::DatabaseRows => "database-rows",
            Self::SerializedBinary => "serialized-binary",
            Self::MixedBinaryText => "mixed-binary-and-text",
            Self::Zeros => "zeros",
            Self::LongRepetitions => "long-repetitions",
            Self::ShortRepeatedTokens => "short-repeated-tokens",
            Self::HighEntropy => "high-entropy",
            Self::AlreadyCompressed => "already-compressed",
            Self::SparseStructures => "sparse-structures",
            Self::LargeBlob => "large-blobs",
        }
    }

    /// The file extension a materialized entry of this shape carries.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Logs => "log",
            Self::SourceCode => "rs",
            Self::DatabaseRows => "tsv",
            Self::MixedBinaryText => "mixed",
            Self::SerializedBinary
            | Self::Zeros
            | Self::LongRepetitions
            | Self::ShortRepeatedTokens
            | Self::HighEntropy
            | Self::AlreadyCompressed
            | Self::SparseStructures
            | Self::LargeBlob => "bin",
        }
    }
}

/// Every shape, so a test can cover the set without repeating it.
#[cfg(test)]
pub const SHAPES: &[Shape] = &[
    Shape::Json,
    Shape::Logs,
    Shape::SourceCode,
    Shape::DatabaseRows,
    Shape::SerializedBinary,
    Shape::MixedBinaryText,
    Shape::Zeros,
    Shape::LongRepetitions,
    Shape::ShortRepeatedTokens,
    Shape::HighEntropy,
    Shape::AlreadyCompressed,
    Shape::SparseStructures,
    Shape::LargeBlob,
];

/// Writes exactly `bytes` bytes of `shape`, derived from `seed`.
///
/// The output depends on the shape, the seed, and the byte count, and on nothing else. Two
/// calls with the same three arguments write the same bytes on any host.
///
/// # Errors
///
/// Fails when the sink rejects a write.
pub fn write(shape: Shape, seed: u64, bytes: u64, sink: &mut impl Write) -> io::Result<()> {
    let mut generator = Generator::new(shape, seed);
    let mut buffer = Vec::with_capacity(BLOCK);
    let mut written: u64 = 0;
    while written < bytes {
        buffer.clear();
        generator.fill(&mut buffer);
        let remaining = bytes.saturating_sub(written);
        let take = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let Some(chunk) = buffer.get(..take) else {
            break;
        };
        sink.write_all(chunk)?;
        written = written.saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
    }
    Ok(())
}

/// The `SplitMix64` sequence, which is the only source of variation a generated entry has.
///
/// It is stated here rather than taken from a dependency so the bytes of the project corpus
/// cannot change when a dependency changes its algorithm.
struct Rng {
    state: u64,
}

impl Rng {
    const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
    const MIX_A: u64 = 0xBF58_476D_1CE4_E5B9;
    const MIX_B: u64 = 0x94D0_49BB_1331_11EB;

    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(Self::GAMMA);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(Self::MIX_A);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(Self::MIX_B);
        mixed ^ (mixed >> 31)
    }

    /// A value below `bound`, or zero when `bound` is zero.
    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64().checked_rem(bound).unwrap_or(0)
    }

    fn index(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).unwrap_or(u64::MAX);
        usize::try_from(self.below(bound)).unwrap_or(0)
    }

    fn byte(&mut self) -> u8 {
        u8::try_from(self.next_u64() & 0xFF).unwrap_or(0)
    }

    /// A value in `low..=high`, clamped when the range is inverted.
    fn between(&mut self, low: u64, high: u64) -> u64 {
        let span = high.saturating_sub(low).saturating_add(1);
        low.saturating_add(self.below(span))
    }
}

/// One shape in progress, holding whatever state that shape repeats across blocks.
struct Generator {
    shape: Shape,
    rng: Rng,
    /// The repeated unit of the long-repetition shape.
    pattern: Vec<u8>,
    /// The vocabulary of the short-token shape.
    tokens: Vec<String>,
    /// The running value of the large-blob walk.
    walk: u8,
    /// How many records the shape has emitted, for the fields that count.
    counter: u64,
}

impl Generator {
    fn new(shape: Shape, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let pattern = if matches!(shape, Shape::LongRepetitions) {
            let mut bytes = Vec::with_capacity(PATTERN_BYTES);
            while bytes.len() < PATTERN_BYTES {
                bytes.extend_from_slice(&rng.next_u64().to_le_bytes());
            }
            bytes.truncate(PATTERN_BYTES);
            bytes
        } else {
            Vec::new()
        };
        let tokens = if matches!(shape, Shape::ShortRepeatedTokens) {
            (0..TOKEN_COUNT).map(|_| token(&mut rng)).collect()
        } else {
            Vec::new()
        };
        Self {
            shape,
            rng,
            pattern,
            tokens,
            walk: 0x80,
            counter: 0,
        }
    }

    /// Appends at least one block of this shape.
    fn fill(&mut self, out: &mut Vec<u8>) {
        match self.shape {
            Shape::Json => self.json(out),
            Shape::Logs => self.logs(out),
            Shape::SourceCode => self.source(out),
            Shape::DatabaseRows => self.rows(out),
            Shape::SerializedBinary => self.serialized(out),
            Shape::MixedBinaryText => self.mixed(out),
            Shape::Zeros => out.resize(BLOCK, 0),
            Shape::LongRepetitions => self.repetitions(out),
            Shape::ShortRepeatedTokens => self.short_tokens(out),
            Shape::HighEntropy => self.entropy(out),
            Shape::AlreadyCompressed => self.compressed(out),
            Shape::SparseStructures => self.sparse(out),
            Shape::LargeBlob => self.blob(out),
        }
    }

    fn json(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            self.counter = self.counter.wrapping_add(1);
            let record = format!(
                "{{\"id\":{},\"user\":\"user_{:05}\",\"event\":\"{}\",\"ok\":{},\
                 \"score\":0.{:04},\"region\":\"{}\",\"tags\":[\"{}\",\"{}\"],\
                 \"ts\":\"2026-{:02}-{:02}T{:02}:{:02}:{:02}Z\"}}\n",
                self.counter,
                self.rng.below(100_000),
                pick(&mut self.rng, EVENTS),
                if self.rng.below(4) == 0 {
                    "false"
                } else {
                    "true"
                },
                self.rng.below(10_000),
                pick(&mut self.rng, REGIONS),
                pick(&mut self.rng, TAGS),
                pick(&mut self.rng, TAGS),
                self.rng.between(1, 12),
                self.rng.between(1, 28),
                self.rng.below(24),
                self.rng.below(60),
                self.rng.below(60),
            );
            out.extend_from_slice(record.as_bytes());
        }
    }

    fn logs(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            self.counter = self.counter.wrapping_add(1);
            let record = format!(
                "2026-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z {:<5} service={} \
                 request={:08x} status={} latency_ms={} bytes={} msg=\"{}\"\n",
                self.rng.between(1, 12),
                self.rng.between(1, 28),
                self.rng.below(24),
                self.rng.below(60),
                self.rng.below(60),
                self.rng.below(1000),
                pick(&mut self.rng, LEVELS),
                pick(&mut self.rng, SERVICES),
                self.rng.below(u64::from(u32::MAX)),
                pick(&mut self.rng, STATUSES),
                self.rng.between(1, 4000),
                self.rng.between(64, 65536),
                pick(&mut self.rng, MESSAGES),
            );
            out.extend_from_slice(record.as_bytes());
        }
    }

    fn source(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            self.counter = self.counter.wrapping_add(1);
            let name = format!(
                "{}_{}",
                pick(&mut self.rng, VERBS),
                pick(&mut self.rng, NOUNS)
            );
            let field = pick(&mut self.rng, NOUNS);
            let record = format!(
                "/// {} the {} that the header declares.\n\
                 fn {}(header: &Header, limit: usize) -> Result<{}> {{\n    \
                 let {} = header.{};\n    \
                 if {} > limit {{\n        \
                 return Err(Error::LimitExceeded);\n    \
                 }}\n    \
                 Ok({})\n\
                 }}\n\n",
                pick(&mut self.rng, VERBS),
                field,
                name,
                pick(&mut self.rng, TYPES),
                field,
                field,
                field,
                field,
            );
            out.extend_from_slice(record.as_bytes());
        }
    }

    fn rows(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            self.counter = self.counter.wrapping_add(1);
            let record = format!(
                "{}\t{:08x}-{:04x}-{:04x}\t2026-{:02}-{:02} {:02}:{:02}:{:02}\t\
                 {}.{:02}\t{}\t{}\n",
                self.counter,
                self.rng.below(u64::from(u32::MAX)),
                self.rng.below(0x1_0000),
                self.rng.below(0x1_0000),
                self.rng.between(1, 12),
                self.rng.between(1, 28),
                self.rng.below(24),
                self.rng.below(60),
                self.rng.below(60),
                self.rng.below(100_000),
                self.rng.below(100),
                pick(&mut self.rng, STATES),
                pick(&mut self.rng, REGIONS),
            );
            out.extend_from_slice(record.as_bytes());
        }
    }

    fn serialized(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            self.counter = self.counter.wrapping_add(1);
            let payload = usize::try_from(self.rng.between(8, 96)).unwrap_or(8);
            let total = u32::try_from(payload.saturating_add(20)).unwrap_or(u32::MAX);
            // Every field is little endian, so the record does not depend on the host.
            out.extend_from_slice(&total.to_le_bytes());
            out.extend_from_slice(&u16::try_from(self.rng.below(8)).unwrap_or(0).to_le_bytes());
            out.extend_from_slice(&self.counter.to_le_bytes());
            out.extend_from_slice(
                &u32::try_from(self.rng.between(1_760_000_000, 1_790_000_000))
                    .unwrap_or(0)
                    .to_le_bytes(),
            );
            out.extend_from_slice(&u16::try_from(payload).unwrap_or(u16::MAX).to_le_bytes());
            for _ in 0..payload {
                out.push(self.rng.byte() & 0x3F);
            }
        }
    }

    fn mixed(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            let sentences = self.rng.between(2, 6);
            for _ in 0..sentences {
                out.extend_from_slice(pick(&mut self.rng, MESSAGES).as_bytes());
                out.push(b' ');
            }
            out.push(b'\n');
            let blob = self.rng.between(64, 512);
            for _ in 0..blob {
                out.push(self.rng.byte());
            }
            out.push(b'\n');
        }
    }

    fn repetitions(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            out.extend_from_slice(&self.pattern);
            self.counter = self.counter.wrapping_add(1);
            // Every eighth copy carries one mutated byte, so the shape is repetitive
            // without being a single block the whole entry reduces to.
            if self.counter.trailing_zeros() >= 3 {
                let at = out
                    .len()
                    .saturating_sub(self.rng.index(PATTERN_BYTES).max(1));
                if let Some(cell) = out.get_mut(at) {
                    *cell = self.rng.byte();
                }
            }
        }
    }

    fn short_tokens(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            let index = self.rng.index(self.tokens.len().max(1));
            if let Some(token) = self.tokens.get(index) {
                out.extend_from_slice(token.as_bytes());
            }
            out.push(if self.rng.below(8) == 0 { b'\n' } else { b' ' });
        }
    }

    fn entropy(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            out.extend_from_slice(&self.rng.next_u64().to_le_bytes());
        }
    }

    fn compressed(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            let payload = self.rng.between(256, 8192);
            // A container header a compressor can model, ahead of a payload it cannot.
            out.extend_from_slice(b"ENTQ");
            out.extend_from_slice(&u32::try_from(payload).unwrap_or(0).to_le_bytes());
            for _ in 0..payload {
                out.push(self.rng.byte());
            }
        }
    }

    fn sparse(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            let hole = usize::try_from(self.rng.between(256, 2048)).unwrap_or(256);
            out.resize(out.len().saturating_add(hole), 0);
            self.counter = self.counter.wrapping_add(1);
            out.extend_from_slice(&self.counter.to_le_bytes());
            for _ in 0..8 {
                out.push(self.rng.byte());
            }
        }
    }

    fn blob(&mut self, out: &mut Vec<u8>) {
        while out.len() < BLOCK {
            // A correlated walk, which is what a sampled signal or an image plane looks
            // like to a compressor: neighbouring bytes are close, distant ones are not.
            let step = self.rng.byte() & 0x7;
            self.walk = if self.rng.below(2) == 0 {
                self.walk.wrapping_add(step)
            } else {
                self.walk.wrapping_sub(step)
            };
            out.push(self.walk);
        }
    }
}

/// One vocabulary entry, chosen by the sequence and never by a container's iteration order.
fn pick(rng: &mut Rng, table: &'static [&'static str]) -> &'static str {
    let index = rng.index(table.len().max(1));
    table.get(index).copied().unwrap_or("")
}

/// One token of the short-token vocabulary.
fn token(rng: &mut Rng) -> String {
    let length = usize::try_from(rng.between(3, 9)).unwrap_or(3);
    let mut text = String::with_capacity(length);
    for _ in 0..length {
        let letter = u8::try_from(u64::from(b'a').saturating_add(rng.below(26))).unwrap_or(b'a');
        text.push(char::from(letter));
    }
    text
}

const EVENTS: &[&str] = &[
    "checkout",
    "login",
    "logout",
    "search",
    "view",
    "add_to_cart",
    "refund",
    "signup",
];
const REGIONS: &[&str] = &[
    "eu-west-1",
    "eu-central-1",
    "us-east-1",
    "us-west-2",
    "ap-south-1",
    "sa-east-1",
];
const TAGS: &[&str] = &[
    "beta", "cart", "mobile", "desktop", "promo", "retry", "cached", "cold",
];
const LEVELS: &[&str] = &["INFO", "WARN", "ERROR", "DEBUG", "TRACE"];
const SERVICES: &[&str] = &[
    "gateway",
    "auth",
    "catalog",
    "billing",
    "search",
    "scheduler",
    "ingest",
];
const STATUSES: &[&str] = &[
    "200", "201", "204", "301", "400", "404", "429", "500", "503",
];
const MESSAGES: &[&str] = &[
    "request completed",
    "connection reset by peer",
    "cache miss, falling back to origin",
    "retrying after backoff",
    "token refreshed",
    "queue depth above threshold",
    "shard rebalanced",
    "checkpoint written",
];
const STATES: &[&str] = &[
    "settled",
    "pending",
    "failed",
    "refunded",
    "disputed",
    "cancelled",
];
const VERBS: &[&str] = &[
    "resolve", "decode", "validate", "reserve", "commit", "flush", "seek", "bound",
];
const NOUNS: &[&str] = &[
    "window", "block", "frame", "region", "index", "literal", "sequence", "checksum",
];
const TYPES: &[&str] = &["usize", "u32", "u64", "Bounds", "Window", "Region"];

#[cfg(test)]
mod tests {
    use super::{SHAPES, Shape, write};

    fn bytes(shape: Shape, seed: u64, count: u64) -> Vec<u8> {
        let mut out = Vec::new();
        assert!(write(shape, seed, count, &mut out).is_ok());
        out
    }

    #[test]
    fn every_shape_writes_exactly_the_byte_count_it_was_asked_for() {
        for shape in SHAPES {
            for count in [0_u64, 1, 7, 1024, 20_000] {
                assert_eq!(
                    bytes(*shape, 7, count).len() as u64,
                    count,
                    "{} at {count} bytes",
                    shape.name()
                );
            }
        }
    }

    #[test]
    fn every_shape_repeats_byte_for_byte_from_one_seed() {
        for shape in SHAPES {
            assert_eq!(
                bytes(*shape, 42, 40_000),
                bytes(*shape, 42, 40_000),
                "{} is not deterministic",
                shape.name()
            );
        }
    }

    #[test]
    fn a_prefix_of_an_entry_is_the_entry_of_that_length() {
        for shape in SHAPES {
            let long = bytes(*shape, 11, 30_000);
            let short = bytes(*shape, 11, 9_000);
            assert_eq!(long.get(..9_000).map(<[u8]>::to_vec), Some(short));
        }
    }

    #[test]
    fn a_different_seed_produces_different_bytes() {
        for shape in SHAPES {
            // Zeros carries no seeded variation: every byte of it is defined by the shape.
            if matches!(shape, Shape::Zeros) {
                continue;
            }
            assert_ne!(
                bytes(*shape, 1, 8_192),
                bytes(*shape, 2, 8_192),
                "{} ignores its seed",
                shape.name()
            );
        }
    }

    #[test]
    fn every_shape_name_and_extension_is_stated() {
        for shape in SHAPES {
            assert!(!shape.name().is_empty());
            assert!(!shape.extension().is_empty());
        }
    }

    #[test]
    fn every_shape_appears_once_in_the_set() {
        let mut names: Vec<&str> = SHAPES.iter().map(|shape| shape.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn the_shapes_a_compressor_should_reduce_are_smaller_than_the_ones_it_cannot() {
        // A shape that claims to be repetitive and one that claims to be incompressible
        // must differ in the only property that distinguishes them: distinct byte runs.
        let repetitive = bytes(Shape::LongRepetitions, 3, 64 * 1024);
        let entropy = bytes(Shape::HighEntropy, 3, 64 * 1024);
        assert!(distinct_runs(&repetitive) < distinct_runs(&entropy));
    }

    fn distinct_runs(data: &[u8]) -> usize {
        let mut seen = std::collections::BTreeSet::new();
        for window in data.windows(8) {
            let _ = seen.insert(window.to_vec());
        }
        seen.len()
    }
}

//! Owns the competitor boundary: how the harness drives a linked competitor library, and
//! what each project publishes for the metrics a result carries.
//!
//! Every competitor is measured in-process, through the library API its own project
//! publishes, linked from the laboratory. A command line invocation costs milliseconds on
//! the development host against tens of microseconds for the codec work, so a subprocess
//! measurement reports process creation; two of the five competitors publish no command line
//! tool at all; and neither a state size nor an allocation count is reachable from outside
//! the process.
//!
//! A competitor gets the configuration its own project recommends and nothing else. Where
//! the project publishes an allocator hook, it is handed the harness allocator, so one
//! counter covers both sides of the boundary. Where it publishes none, the result says the
//! metric is unavailable and why, and never reports a zero that a reader would take for a
//! measurement.
//!
//! The unsafe surface of this crate is here and in the allocator. It exists to cross a C
//! boundary, not to make anything faster, and no Entroq codec path reaches it.
//!
//! This module does not own what is measured, how it is timed, or where it is written.

mod brotli;
mod lz4;
mod snappy;
mod zlib;
mod zstd;

use crate::catalog::Codec;
use crate::error::{Error, Result};

/// Where one metric's number comes from, or why there is none.
pub enum Method {
    /// The project publishes an interface that reports it. The text names the call.
    By(&'static str),
    /// The project publishes no such interface. The text says what is missing.
    Absent(&'static str),
}

impl Method {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::By(_))
    }

    pub const fn text(&self) -> &'static str {
        match self {
            Self::By(text) | Self::Absent(text) => text,
        }
    }
}

/// What one competitor publishes, stated once so every result carries the same facts.
pub struct Notes {
    /// The exact format the measured bytes are in.
    ///
    /// A project that defines more than one format, or that is commonly measured inside a
    /// container it does not define, states which one a number came from.
    pub format: &'static str,
    /// How the bytes the codec owns are obtained.
    pub state_bytes: Method,
    /// How the allocations one operation makes are obtained.
    pub allocations: Method,
    /// How the streaming interface is driven.
    pub streaming: Method,
    /// How a thread count is set.
    pub threads: Method,
}

/// One chunk of a streamed compression.
pub struct Chunk {
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub elapsed_ns: u64,
}

/// One competitor, configured at one operating point, ready to measure.
pub enum Session {
    Lz4(lz4::Session),
    Zstd(zstd::Session),
    Brotli(brotli::Session),
    Snappy(snappy::Session),
    Zlib(zlib::Session),
}

impl Session {
    /// Opens one competitor at one of the operating points its catalog entry names.
    ///
    /// # Errors
    ///
    /// Fails when the codec is not one the laboratory holds, the operating point is not one
    /// the catalog names for it, or the library refuses to produce a context.
    pub fn open(codec: &Codec, point: &str) -> Result<Self> {
        if !codec.operating_points.contains(&point) {
            return Err(Error::measure(
                format!("the operating point `{point}`"),
                format!(
                    "is not one of the points the catalog pins for {}: {}",
                    codec.display,
                    codec.operating_points.join(", ")
                ),
            ));
        }
        match codec.name {
            "lz4" => lz4::Session::open(point).map(Self::Lz4),
            "zstd" => zstd::Session::open(point).map(Self::Zstd),
            "brotli" => brotli::Session::open(point).map(Self::Brotli),
            "snappy" => snappy::Session::open(point).map(Self::Snappy),
            "zlib" => zlib::Session::open(point).map(Self::Zlib),
            other => Err(Error::measure(
                format!("the competitor `{other}`"),
                "is pinned in the catalog and is not linked into this harness",
            )),
        }
    }

    /// What the linked library reports as its own version, for the competitors that publish
    /// one.
    ///
    /// A version that disagrees with the pinned one means the linker resolved a copy the
    /// laboratory did not build.
    pub fn linked_version(&self) -> Option<String> {
        match self {
            Self::Lz4(session) => session.linked_version(),
            Self::Zstd(session) => session.linked_version(),
            Self::Brotli(session) => session.linked_version(),
            Self::Snappy(session) => session.linked_version(),
            Self::Zlib(session) => session.linked_version(),
        }
    }

    pub const fn notes(&self) -> Notes {
        match self {
            Self::Lz4(session) => session.notes(),
            Self::Zstd(session) => session.notes(),
            Self::Brotli(session) => session.notes(),
            Self::Snappy(session) => session.notes(),
            Self::Zlib(session) => session.notes(),
        }
    }

    /// The output capacity a compression of `input` bytes needs.
    pub fn compress_bound(&self, input: usize) -> usize {
        match self {
            Self::Lz4(session) => session.compress_bound(input),
            Self::Zstd(session) => session.compress_bound(input),
            Self::Brotli(session) => session.compress_bound(input),
            Self::Snappy(session) => session.compress_bound(input),
            Self::Zlib(session) => session.compress_bound(input),
        }
    }

    /// Compresses `src` into `dst` and reports the compressed length.
    ///
    /// # Errors
    ///
    /// Fails when the library reports an error, or the input exceeds what its interface
    /// accepts.
    pub fn compress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        match self {
            Self::Lz4(session) => session.compress(src, dst),
            Self::Zstd(session) => session.compress(src, dst),
            Self::Brotli(session) => session.compress(src, dst),
            Self::Snappy(session) => session.compress(src, dst),
            Self::Zlib(session) => session.compress(src, dst),
        }
    }

    /// Decompresses `src` into `dst` and reports the decompressed length.
    ///
    /// # Errors
    ///
    /// Fails when the library reports an error.
    pub fn decompress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        match self {
            Self::Lz4(session) => session.decompress(src, dst),
            Self::Zstd(session) => session.decompress(src, dst),
            Self::Brotli(session) => session.decompress(src, dst),
            Self::Snappy(session) => session.decompress(src, dst),
            Self::Zlib(session) => session.decompress(src, dst),
        }
    }

    /// The bytes the encoder holds, as the project's own interface reports them.
    pub fn encoder_state_bytes(&self) -> Option<u64> {
        match self {
            Self::Lz4(session) => session.encoder_state_bytes(),
            Self::Zstd(session) => session.encoder_state_bytes(),
            Self::Brotli(_) | Self::Snappy(_) | Self::Zlib(_) => None,
        }
    }

    /// The bytes the decoder holds, as the project's own interface reports them.
    pub fn decoder_state_bytes(&self) -> Option<u64> {
        match self {
            Self::Zstd(session) => session.decoder_state_bytes(),
            Self::Lz4(_) | Self::Brotli(_) | Self::Snappy(_) | Self::Zlib(_) => None,
        }
    }

    /// Compresses `src` through the project's streaming interface, one chunk at a time, and
    /// reports what each chunk cost and produced.
    ///
    /// # Errors
    ///
    /// Fails when the library reports an error, or the competitor publishes no streaming
    /// interface. `notes().streaming` states which competitors those are before a caller
    /// asks.
    pub fn stream(&mut self, src: &[u8], chunk: usize, dst: &mut [u8]) -> Result<Vec<Chunk>> {
        match self {
            Self::Lz4(session) => session.stream(src, chunk, dst),
            Self::Zstd(session) => session.stream(src, chunk, dst),
            Self::Brotli(session) => session.stream(src, chunk, dst),
            Self::Zlib(session) => session.stream(src, chunk, dst),
            Self::Snappy(session) => Err(Error::measure(
                "the Snappy streaming metric",
                session.notes().streaming.text(),
            )),
        }
    }

    /// Compresses `src` with `workers` worker threads, for a competitor whose library
    /// publishes a thread parameter.
    ///
    /// Returns `None` when it publishes none. `notes().threads` states why.
    ///
    /// # Errors
    ///
    /// Fails when the library refuses the thread count or reports an error.
    pub fn threaded(&self, workers: u32, src: &[u8], dst: &mut [u8]) -> Option<Result<usize>> {
        match self {
            Self::Zstd(session) => Some(session.threaded(workers, src, dst)),
            Self::Lz4(_) | Self::Brotli(_) | Self::Snappy(_) | Self::Zlib(_) => None,
        }
    }
}

/// The suffix of an operating point name, as an integer.
///
/// `level-12` is 12 and `q5` is 5. Every catalog point carries its parameter in its name, so
/// the catalog stays the one place a point is declared.
fn parameter(point: &str, prefix: &str) -> Option<i32> {
    point.strip_prefix(prefix)?.parse().ok()
}

/// The length a C interface that counts in `int` accepts.
fn as_int(len: usize, what: &str) -> Result<i32> {
    i32::try_from(len).map_err(|_| {
        Error::measure(
            format!("the {what}"),
            format!("is {len} bytes, and this library's interface counts in a 32-bit int"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{Method, Session, as_int, parameter};
    use crate::catalog::find;

    #[test]
    fn a_point_carries_its_parameter_in_its_name() {
        assert_eq!(parameter("level-12", "level-"), Some(12));
        assert_eq!(parameter("q5", "q"), Some(5));
        assert_eq!(parameter("hc-9", "hc-"), Some(9));
    }

    #[test]
    fn a_name_that_is_not_that_point_carries_nothing() {
        assert_eq!(parameter("level-12", "q"), None);
        assert_eq!(parameter("default", "level-"), None);
        assert_eq!(parameter("level-x", "level-"), None);
    }

    #[test]
    fn a_length_beyond_a_32_bit_interface_is_reported_not_truncated() {
        assert_eq!(as_int(1024, "input").ok(), Some(1024));
        assert!(as_int(usize::MAX, "input").is_err());
    }

    #[test]
    fn a_method_states_whether_a_metric_is_reachable() {
        assert!(Method::By("ZSTD_sizeof_CCtx").is_available());
        assert!(!Method::Absent("the project publishes no state size").is_available());
    }

    #[test]
    fn a_point_the_catalog_does_not_pin_is_refused() {
        let Some(codec) = find("zstd") else { return };
        let failure = Session::open(codec, "level-99");
        assert!(failure.is_err());
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("level-22"), "{message}");
    }

    #[test]
    fn every_pinned_competitor_opens_at_every_point_the_catalog_names() {
        for codec in crate::catalog::CODECS {
            for point in codec.operating_points {
                assert!(
                    Session::open(codec, point).is_ok(),
                    "{} does not open at {point}",
                    codec.name
                );
            }
        }
    }

    #[test]
    fn every_linked_library_reports_the_version_the_catalog_pins() {
        for codec in crate::catalog::CODECS {
            let Some(point) = codec.operating_points.first() else {
                continue;
            };
            let Ok(session) = Session::open(codec, point) else {
                continue;
            };
            let Some(reported) = session.linked_version() else {
                continue;
            };
            assert!(
                codec.version.contains(&reported),
                "{} links a library that reports {reported}, and the catalog pins {}",
                codec.name,
                codec.version
            );
        }
    }
}

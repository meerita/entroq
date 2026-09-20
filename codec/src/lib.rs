//! Owns the Entroq codec.
//!
//! The crate exports the format contract, the entropy coding the format's tables are declared
//! and validated through, the sequence representation their symbols are drawn from, the
//! match finder and the parser that produce the sequences, the kernel dispatch the finder runs
//! on, and the streaming pair that compresses through every block type the format defines and
//! reads every one back. Every other module is private until the mechanism it owns is designed
//! and measured.
//!
//! The compressed block is not among them. It carries three mechanisms whose state crosses the
//! blocks of a region, and a caller that drove it directly would own invariants the streaming
//! pair already holds. The streaming pair is the only thing that drives it, and the statistics
//! a caller reads about it are on the streaming pair.

/// The version of this codec, which is the encoder version a measurement records.
///
/// The format version a stream declares is the frame header's and is not this. Two encoder
/// versions write different bytes for the same content and both stay decodable by every
/// decoder of the format version they declare.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Temporary measurement seam for the width-selection campaign.
///
/// It lets the benchmark harness select the match-copy width at run time, so the three
/// candidate widths are measured as three arms at one revision. The selected width is compiled
/// in and this module is removed when the selection is made; it is not part of the API.
#[cfg(feature = "width-selection")]
pub mod width_selection {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static WIDTH: AtomicUsize = AtomicUsize::new(32);

    /// Selects the match-copy width the next expansions use.
    pub fn select(width: usize) {
        WIDTH.store(width, Ordering::Relaxed);
    }

    /// The match-copy width currently selected.
    #[must_use]
    pub fn selected() -> usize {
        WIDTH.load(Ordering::Relaxed)
    }
}

mod block;
mod checksum;
mod decode;
mod encode;
pub mod entropy;
pub mod format;
mod index;
pub mod matchfinder;
pub mod parser;
pub mod sequence;
pub mod simd;
pub mod stream;

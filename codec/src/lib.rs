//! Owns the Entroq codec.
//!
//! The crate exports the format contract, the entropy coding the format's tables are declared
//! and validated through, the sequence representation their symbols are drawn from, and the
//! streaming pair that drives them. Every other module is private until the mechanism it owns
//! is designed and measured.

mod checksum;
mod decode;
mod encode;
pub mod entropy;
pub mod format;
mod index;
mod matchfinder;
mod parser;
pub mod sequence;
mod simd;
pub mod stream;

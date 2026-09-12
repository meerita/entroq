//! Owns the Entroq codec.
//!
//! The crate exports the format contract and the streaming pair that drives it. Every other
//! module is private until the mechanism it owns is designed and measured.

mod checksum;
mod decode;
mod encode;
mod entropy;
pub mod format;
mod index;
mod matchfinder;
mod parser;
mod simd;
pub mod stream;

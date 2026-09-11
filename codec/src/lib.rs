//! Owns the Entroq codec.
//!
//! The crate exports no codec API. Every module is private until the mechanism it owns is
//! designed and measured.

mod checksum;
mod decode;
mod encode;
mod entropy;
mod format;
mod index;
mod matchfinder;
mod parser;
mod simd;
mod stream;
